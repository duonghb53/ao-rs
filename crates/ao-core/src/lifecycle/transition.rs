use super::*;

impl LifecycleManager {
    /// Flip a session to a terminal state, persist, and emit both the
    /// `StatusChanged` (to the chosen terminal `SessionStatus`) and the
    /// `Terminated` event.
    ///
    /// Also clears any reaction trackers the engine was holding for this
    /// session. Without this, a long-running `ao-rs watch` would slowly
    /// leak tracker entries for every terminated session.
    pub(super) async fn terminate(
        &self,
        session: &mut Session,
        reason: TerminationReason,
    ) -> Result<()> {
        // ao-ts parity: runtime/agent exit is `killed`. Rust keeps `terminated`
        // for "exited but not explicitly killed" elsewhere, but lifecycle
        // liveness probes map to `killed` just like ao-ts.
        let terminal_status = match reason {
            TerminationReason::RuntimeGone
            | TerminationReason::AgentExited
            | TerminationReason::NoHandle => SessionStatus::Killed,
        };
        if session.status != terminal_status {
            self.transition(session, terminal_status).await?;
        }
        if let Some(engine) = self.reaction_engine.as_ref() {
            engine.clear_all_for_session(&session.id);
        }
        // Purge per-session bookkeeping so a long-running watch loop
        // doesn't accumulate entries for every session it has ever seen.
        self.idle_since
            .lock()
            .unwrap_or_else(|e| {
                tracing::error!("lifecycle idle_since mutex poisoned; recovering inner state: {e}");
                e.into_inner()
            })
            .remove(&session.id);
        self.last_review_backlog_check
            .lock()
            .unwrap_or_else(|e| {
                tracing::error!(
                    "last_review_backlog_check mutex poisoned; recovering inner state: {e}"
                );
                e.into_inner()
            })
            .remove(&session.id);
        self.emit(OrchestratorEvent::Terminated {
            id: session.id.clone(),
            reason,
        });
        Ok(())
    }

    /// Transition status, persist, emit `StatusChanged`, and (if a
    /// reaction engine is attached) dispatch any reaction associated
    /// with the new status.
    ///
    /// Ordering matters: normally we save + emit `StatusChanged` *before*
    /// calling the engine, so subscribers see the transition event in the
    /// right order and so a panicking engine doesn't lose the state change.
    ///
    /// **Phase G parking hook.** When the reaction is `auto-merge` and
    /// the engine reports a non-escalated failure, `transition` persists
    /// the session as `MergeFailed` (instead of `Mergeable`) so the next
    /// SCM tick's `derive_scm_status` can decide whether to retry
    /// (still-ready observation re-promotes to `Mergeable`) or abandon
    /// (flake / closed PR drops off the PR track). Escalated outcomes are
    /// left in `Mergeable` so the retry loop stops and the human
    /// notification stands — see the doc on `should_park_in_merge_failed`.
    pub async fn transition(&self, session: &mut Session, to: SessionStatus) -> Result<()> {
        if session.status == to {
            return Ok(());
        }
        let from = session.status;
        session.status = to;

        // Poll cost on every status change (not every tick). Only
        // overwrite when the agent returns Some — a None keeps the
        // existing cost intact so we never lose data.
        //
        // `cost_estimate` may do blocking file I/O (JSONL parsing),
        // and `record_cost` writes to disk — both are wrapped in
        // spawn_blocking to avoid starving the Tokio executor.
        match self.agent.cost_estimate(session).await {
            Ok(Some(cost)) => {
                // Best-effort ledger write — don't fail the transition.
                let sid = session.id.0.clone();
                let pid = session.project_id.clone();
                let br = session.branch.clone();
                let c = cost.clone();
                let ca = session.created_at;
                let ledger_result = tokio::task::spawn_blocking(move || {
                    crate::cost_ledger::record_cost(&sid, &pid, &br, &c, ca)
                })
                .await;
                match ledger_result {
                    Ok(Err(e)) => {
                        tracing::warn!(session = %session.id, "cost ledger write failed: {e}");
                    }
                    Err(e) => {
                        tracing::warn!(session = %session.id, "cost ledger task panicked: {e}");
                    }
                    Ok(Ok(())) => {}
                }
                session.cost = Some(cost);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(session = %session.id, "cost_estimate failed: {e}");
            }
        }

        // Phase 1 invariant: **one status transition per session per tick**.
        //
        // The Phase G auto-merge retry loop needs to "park" a just-entered
        // `Mergeable` session in `MergeFailed` when the auto-merge action
        // fails without escalating. Historically this produced two
        // transitions/events in one tick (`… → Mergeable` then
        // `Mergeable → MergeFailed`). To preserve the invariant while
        // keeping reaction dispatch semantics, we decide the *final*
        // persisted status before emitting `StatusChanged`.
        let mut persisted_to = to;
        if let Some(engine) = self.reaction_engine.as_ref() {
            if let Some(next_key) = status_to_reaction_key(to) {
                match engine.dispatch(session, next_key).await {
                    Ok(Some(outcome))
                        if should_park_in_merge_failed(engine, session, to, next_key, &outcome) =>
                    {
                        persisted_to = SessionStatus::MergeFailed;
                        session.status = persisted_to;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            session = %session.id,
                            reaction = next_key,
                            error = %e,
                            "reaction dispatch failed; lifecycle loop continues"
                        );
                    }
                }
            }
        }

        // If the "parking" rewrite lands us back in the original status,
        // this tick should not persist or emit a no-op transition. (The
        // reaction attempt has already been recorded by the engine.)
        if persisted_to == from {
            session.status = from;
            return Ok(());
        }

        self.sessions.save(session).await?;
        self.emit(OrchestratorEvent::StatusChanged {
            id: session.id.clone(),
            from,
            to: persisted_to,
        });

        // Issue #169: notify the parent orchestrator (if any) so it can
        // react to worker state changes without manual human prodding.
        // Only fires for transitions the orchestrator actually needs to
        // see; best-effort, never fails the transition.
        if is_orchestrator_notifiable(persisted_to) {
            self.notify_orchestrator(session, persisted_to).await;
        }

        if let Some(engine) = self.reaction_engine.as_ref() {
            // Leaving a reaction-triggering status? Clear its tracker so
            // the next entry (e.g. new CI failure after a fix) gets a
            // fresh retry budget. Parking-loop transitions
            // (`Mergeable ↔ MergeFailed`) are the exception — see
            // `clear_tracker_on_transition` for the rationale.
            clear_tracker_on_transition(engine, &session.id, from, persisted_to);
        }

        Ok(())
    }

    /// Best-effort delivery of a worker state-change notification to the
    /// parent orchestrator via `Runtime::send_message`. Silent when the
    /// worker has no `spawned_by`, the parent yaml is gone, or the
    /// parent has no live runtime handle — any of those mean "no one to
    /// tell", not "error".
    pub(super) async fn notify_orchestrator(&self, worker: &Session, to: SessionStatus) {
        let Some(orch_id) = worker.spawned_by.as_ref() else {
            return;
        };
        let parent = match self.sessions.find_by_prefix(&orch_id.0).await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(
                    session = %worker.id,
                    parent = %orch_id.0,
                    "orchestrator session lookup failed: {e}"
                );
                return;
            }
        };
        let Some(handle) = parent.runtime_handle.as_deref() else {
            return;
        };
        let msg = format_orchestrator_notification(worker, to);
        if let Err(e) = self.runtime.send_message(handle, &msg).await {
            tracing::warn!(
                session = %worker.id,
                parent = %parent.id,
                "failed to deliver orchestrator notification: {e}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::tests::{
        build_engine_with_ci_failed, fake_pr, fake_session, recv_timeout, MockAgent, MockRuntime,
        MockScm,
    };
    use crate::reactions::ReactionAction;
    use std::collections::HashSet;

    // ---------- Reaction engine integration (Phase D) ---------- //

    #[tokio::test]
    async fn transition_into_ci_failed_dispatches_reaction_on_shared_channel() {
        // ci-failed is dispatched via check_ci_failed (poll_scm step 6), not
        // through the generic status_to_reaction_key path in transition. This
        // test exercises the full SCM-driven tick so the wiring is end-to-end.
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("reaction-transition");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine = build_engine_with_ci_failed(&lifecycle, "fix CI please");
        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(7, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::None);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::CiFailed,
                    ..
                }
            )),
            "expected StatusChanged to CiFailed, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ReactionTriggered {
                    action: ReactionAction::SendToAgent,
                    ..
                }
            )),
            "expected ReactionTriggered(SendToAgent) from engine, got {events:?}"
        );

        assert_eq!(engine.attempts(&s.id, "ci-failed"), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn leaving_reaction_status_clears_tracker() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("reaction-clear");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine = build_engine_with_ci_failed(&lifecycle, "fix");
        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(8, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::None);
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine.attempts(&s.id, "ci-failed"), 1);

        let s_updated = sessions.find_by_prefix("s1").await.unwrap();
        assert_eq!(s_updated.status, SessionStatus::CiFailed);

        let mut s2 = s_updated;
        lifecycle
            .transition(&mut s2, SessionStatus::PrOpen)
            .await
            .unwrap();
        assert_eq!(
            engine.attempts(&s2.id, "ci-failed"),
            0,
            "tracker should be cleared on exit from CiFailed"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn unrelated_transition_does_not_touch_reaction_engine() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("no-react");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);
        let engine = build_engine_with_ci_failed(&lifecycle, "never fires");
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine.clone()));

        let mut rx = lifecycle.subscribe();
        sessions.save(&fake_session("s1", "demo")).await.unwrap();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::StatusChanged {
                to: SessionStatus::Working,
                ..
            }
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::ReactionTriggered { .. })),
            "unexpected ReactionTriggered on Working transition: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Orchestrator notification (issue #169) ---------- //

    #[tokio::test]
    async fn transition_notifies_parent_orchestrator_via_runtime() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("orchestrator-notify");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let lifecycle = Arc::new(LifecycleManager::new(
            sessions.clone(),
            runtime.clone() as Arc<dyn Runtime>,
            agent,
        ));

        let mut parent = fake_session("orch1", "demo");
        parent.runtime_handle = Some("orch-handle".into());
        sessions.save(&parent).await.unwrap();

        let mut worker = fake_session("work1", "demo");
        worker.status = SessionStatus::Working;
        worker.spawned_by = Some(parent.id.clone());
        sessions.save(&worker).await.unwrap();

        lifecycle
            .transition(&mut worker, SessionStatus::PrOpen)
            .await
            .unwrap();

        let sends = runtime.sends();
        assert_eq!(
            sends.len(),
            1,
            "expected one notification to parent, got {sends:?}"
        );
        assert_eq!(sends[0].0, "orch-handle");
        assert!(
            sends[0].1.contains("pr_open"),
            "message should mention new status, got {:?}",
            sends[0].1
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn transition_without_spawned_by_sends_no_message() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("orchestrator-notify-none");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let lifecycle = Arc::new(LifecycleManager::new(
            sessions.clone(),
            runtime.clone() as Arc<dyn Runtime>,
            agent,
        ));

        let mut worker = fake_session("lone1", "demo");
        worker.status = SessionStatus::Working;
        assert!(worker.spawned_by.is_none());
        sessions.save(&worker).await.unwrap();

        lifecycle
            .transition(&mut worker, SessionStatus::PrOpen)
            .await
            .unwrap();

        assert!(
            runtime.sends().is_empty(),
            "workers without spawned_by must not trigger a send"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn transition_into_non_notifiable_status_sends_no_message() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("orchestrator-notify-filter");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let lifecycle = Arc::new(LifecycleManager::new(
            sessions.clone(),
            runtime.clone() as Arc<dyn Runtime>,
            agent,
        ));

        let mut parent = fake_session("orch2", "demo");
        parent.runtime_handle = Some("orch-handle".into());
        sessions.save(&parent).await.unwrap();

        let mut worker = fake_session("work2", "demo");
        worker.status = SessionStatus::Spawning;
        worker.spawned_by = Some(parent.id.clone());
        sessions.save(&worker).await.unwrap();

        lifecycle
            .transition(&mut worker, SessionStatus::Working)
            .await
            .unwrap();

        assert!(
            runtime.sends().is_empty(),
            "transition to Working should not notify orchestrator"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Issue #195 H3: all-complete dispatch ---------- //

    #[tokio::test]
    async fn all_complete_fires_once_when_last_session_terminates() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::ReactionConfig;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("all-complete");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let cfg = ReactionConfig::new(ReactionAction::Notify);
        let mut map = std::collections::HashMap::new();
        map.insert("all-complete".into(), cfg);
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine.clone()));

        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Done;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ReactionTriggered {
                    reaction_key,
                    ..
                } if reaction_key == "all-complete"
            )),
            "expected all-complete ReactionTriggered, got {events:?}"
        );

        lifecycle.tick(&mut seen).await.unwrap();
        let mut events2 = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events2.push(e);
        }
        assert!(
            !events2.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ReactionTriggered {
                    reaction_key,
                    ..
                } if reaction_key == "all-complete"
            )),
            "all-complete must NOT re-fire on second tick: {events2:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn all_complete_resets_on_new_session() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::ReactionConfig;
        use crate::session_manager::SessionManager;
        use std::sync::atomic::Ordering;
        let base = unique_temp_dir("all-complete-reset");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let mut map = std::collections::HashMap::new();
        map.insert(
            "all-complete".into(),
            ReactionConfig::new(ReactionAction::Notify),
        );
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine.clone()));
        let mut rx = lifecycle.subscribe();

        let mut s1 = fake_session("s1", "demo");
        s1.status = SessionStatus::Done;
        sessions.save(&s1).await.unwrap();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        while recv_timeout(&mut rx).await.is_some() {}

        let s2 = fake_session("s2", "demo");
        sessions.save(&s2).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();
        while recv_timeout(&mut rx).await.is_some() {}
        assert!(
            !lifecycle.all_complete_fired.load(Ordering::Relaxed),
            "flag must be reset when a non-terminal session appears"
        );

        let mut s2_done = sessions.find_by_prefix("s2").await.unwrap();
        s2_done.status = SessionStatus::Done;
        sessions.save(&s2_done).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();
        let mut events3 = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events3.push(e);
        }
        assert!(
            events3.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ReactionTriggered {
                    reaction_key,
                    ..
                } if reaction_key == "all-complete"
            )),
            "all-complete must re-fire after a new drain: {events3:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
