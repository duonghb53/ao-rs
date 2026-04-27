use super::*;

impl LifecycleManager {
    /// Probe one session and apply any resulting transitions.
    pub(super) async fn poll_one(&self, mut session: Session) -> Result<()> {
        let id = session.id.clone();
        // ---- 1. Runtime liveness ----
        let alive = match &session.runtime_handle {
            Some(handle) => match self.runtime.is_alive(handle).await {
                Ok(a) => a,
                Err(e) => {
                    // Runtime probe itself errored — treat as unknown,
                    // emit TickError, and don't transition.
                    self.emit(OrchestratorEvent::TickError {
                        id: id.clone(),
                        message: format!("is_alive: {e}"),
                    });
                    return Ok(());
                }
            },
            // No handle means we never got far enough to spawn a runtime,
            // or it was intentionally cleared. Consider the session dead.
            None => {
                self.terminate(&mut session, TerminationReason::NoHandle)
                    .await?;
                return Ok(());
            }
        };

        if !alive {
            self.terminate(&mut session, TerminationReason::RuntimeGone)
                .await?;
            return Ok(());
        }

        // ---- 2. Agent activity detection ----
        let activity = match self.agent.detect_activity(&session).await {
            Ok(a) => a,
            Err(e) => {
                self.emit(OrchestratorEvent::TickError {
                    id: id.clone(),
                    message: format!("detect_activity: {e}"),
                });
                return Ok(());
            }
        };

        // Agent says the process exited — treat the same as runtime gone,
        // but attribute the reason to the agent so observers can distinguish.
        if activity.is_terminal() {
            self.terminate(&mut session, TerminationReason::AgentExited)
                .await?;
            return Ok(());
        }

        // ao-ts parity: `waiting_input` is a first-class lifecycle status.
        // This must win early so a session doesn't stay `Working` while the
        // agent is blocked on a prompt.
        if activity == ActivityState::WaitingInput && session.status != SessionStatus::NeedsInput {
            self.transition(&mut session, SessionStatus::NeedsInput)
                .await?;
        }

        // ---- 3. Persist any activity transition ----
        if session.activity != Some(activity) {
            let prev = session.activity;
            session.activity = Some(activity);
            self.sessions.save(&session).await?;
            self.emit(OrchestratorEvent::ActivityChanged {
                id: id.clone(),
                prev,
                next: activity,
            });
        }

        // Phase H: maintain the idle-since timestamp used by
        // `check_stuck`. Unconditional every tick — the helper itself
        // decides whether to insert, preserve, or remove.
        self.update_idle_since(&session.id, activity);

        // Snapshot the status BEFORE any transitioning step runs, so
        // step 6 (`check_stuck`) can yield the tick if an earlier step
        // already mutated `session.status`. Matches the TS reference's
        // `determineStatus` contract of one transition per call — see
        // Design Decision 8 in docs/ai/design/feature-agent-stuck-detection.md
        // and the "one transition per tick" entry in memory.
        let pre_transition_status = session.status;

        // ---- 4. Status transitions driven by activity ----
        // Slice 1 Phase C handles the happy-path Spawning → Working flip.
        // Phase F layers SCM-driven transitions on top (see step 5).
        // Phase H extends this with the symmetric `Stuck → Working`
        // recovery: a session that went idle long enough to park in
        // `Stuck` should exit the moment the agent starts producing
        // activity again. `transition` auto-clears the `agent-stuck`
        // tracker via `status_to_reaction_key(Stuck) = Some("agent-stuck")`
        // so there's no bespoke cleanup needed here.
        if matches!(
            session.status,
            SessionStatus::Spawning | SessionStatus::Stuck | SessionStatus::NeedsInput
        ) && matches!(activity, ActivityState::Active | ActivityState::Ready)
        {
            self.transition(&mut session, SessionStatus::Working)
                .await?;
        }

        // ---- 5. PR-driven status transitions (Phase F) ----
        // Only runs when a `Scm` plugin is wired in (via `with_scm`). A
        // failing probe inside `poll_scm` emits `TickError` on the shared
        // channel and returns `Ok(())` so one bad `gh` shell-out doesn't
        // kill the whole tick.
        if self.scm.is_some() {
            self.poll_scm(&mut session).await?;
        }

        // ---- 5b. Worktree cleanup on Merged ----
        // When a workspace plugin is wired in and the session just landed in
        // `Merged`, remove its worktree. The session YAML stays on disk for
        // history; only the working-directory folder is deleted.
        if session.status == SessionStatus::Merged {
            // Kill the runtime (tmux window) — best-effort.
            if let Some(ref handle) = session.runtime_handle {
                match self.runtime.destroy(handle).await {
                    Ok(()) => tracing::info!(session = %session.id, "→ killed runtime on merge"),
                    Err(e) => {
                        tracing::warn!(session = %session.id, error = %e, "runtime destroy on merge failed")
                    }
                }
            }

            if let Some(ref workspace) = self.workspace {
                if let Some(ref ws_path) = session.workspace_path {
                    match workspace.destroy(ws_path).await {
                        Ok(()) => {
                            tracing::info!(
                                session = %session.id,
                                path = %ws_path.display(),
                                "→ removed worktree"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                session = %session.id,
                                path = %ws_path.display(),
                                error = %e,
                                "worktree cleanup failed"
                            );
                        }
                    }
                }
            }

            // Auto-terminate on merge (issue #220): transition to Killed and
            // emit Terminated{PrMerged} so observers see a clean shutdown event.
            if self.lifecycle_config.auto_terminate_on_merge {
                self.terminate(&mut session, TerminationReason::PrMerged)
                    .await?;
                return Ok(());
            }
        }

        // ---- 6. Agent-stuck detection (Phase H) ----
        // Gated on the pre-transition snapshot: if step 4 or 5 already
        // mutated `session.status` this tick, we yield and let the next
        // tick decide whether stuck still applies. Also gated on a
        // reaction engine being configured — without one, there's no
        // `agent-stuck` config to read and no way to emit the tracker
        // event, so the early-return in `check_stuck` would fire anyway
        // but checking here keeps the happy path one branch shorter.
        if self.reaction_engine.is_some() && session.status == pre_transition_status {
            self.check_stuck(&mut session).await?;
        }

        Ok(())
    }

    /// Crash-recovery sweep run once at startup.
    ///
    /// Scans all sessions for those already in `Merged` status that still
    /// have a `workspace_path` on disk. This handles the race where the
    /// process was killed after persisting `Merged` but before the per-tick
    /// `destroy()` call completed — terminal sessions are skipped by
    /// `poll_one`, so without this sweep the worktree would live forever.
    pub(super) async fn sweep_merged_worktrees(&self) {
        let Some(ref workspace) = self.workspace else {
            return;
        };

        let sessions = match self.sessions.list().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("startup worktree sweep: failed to list sessions: {e}");
                return;
            }
        };

        for session in sessions {
            if session.status != SessionStatus::Merged {
                continue;
            }
            let Some(ref ws_path) = session.workspace_path else {
                continue;
            };
            if !ws_path.exists() {
                continue;
            }
            match workspace.destroy(ws_path).await {
                Ok(()) => {
                    tracing::info!(
                        session = %session.id,
                        path = %ws_path.display(),
                        "→ removed worktree (startup sweep)"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        session = %session.id,
                        path = %ws_path.display(),
                        error = %e,
                        "startup worktree sweep: cleanup failed"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::tests::{fake_session, recv_timeout, setup, MockAgent, MockRuntime};
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn first_tick_emits_spawned_and_transitions_to_working() {
        let (lifecycle, sessions, _rt, _agent, base) = setup("spawned", ActivityState::Ready).await;
        let mut rx = lifecycle.subscribe();
        sessions.save(&fake_session("s1", "demo")).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::Spawned { .. })),
            "expected Spawned event, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ActivityChanged {
                    next: ActivityState::Ready,
                    ..
                }
            )),
            "expected ActivityChanged → Ready, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Spawning,
                    to: SessionStatus::Working,
                    ..
                }
            )),
            "expected StatusChanged Spawning → Working, got {events:?}"
        );

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].status, SessionStatus::Working);
        assert_eq!(persisted[0].activity, Some(ActivityState::Ready));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn dead_runtime_terminates_session() {
        let (lifecycle, sessions, rt, _agent, base) = setup("dead", ActivityState::Ready).await;
        let mut rx = lifecycle.subscribe();
        sessions.save(&fake_session("s1", "demo")).await.unwrap();

        rt.alive.store(false, Ordering::SeqCst);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::Terminated {
                    reason: TerminationReason::RuntimeGone,
                    ..
                }
            )),
            "expected Terminated(RuntimeGone), got {events:?}"
        );

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Killed);
        assert!(persisted[0].is_terminal());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn exited_activity_terminates_with_agent_reason() {
        let (lifecycle, sessions, _rt, agent, base) = setup("exited", ActivityState::Ready).await;
        let mut rx = lifecycle.subscribe();
        sessions.save(&fake_session("s1", "demo")).await.unwrap();
        agent.set(ActivityState::Exited);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::Terminated {
                    reason: TerminationReason::AgentExited,
                    ..
                }
            )),
            "expected Terminated(AgentExited), got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn terminal_sessions_are_skipped_on_subsequent_ticks() {
        let (lifecycle, sessions, _rt, _agent, base) = setup("skip", ActivityState::Ready).await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Done;
        sessions.save(&s).await.unwrap();

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert_eq!(events.len(), 1, "got {events:?}");
        assert!(matches!(&events[0], OrchestratorEvent::Spawned { .. }));

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Done);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn spawned_is_emitted_only_once_per_session() {
        let (lifecycle, sessions, _rt, _agent, base) = setup("once", ActivityState::Ready).await;
        sessions.save(&fake_session("s1", "demo")).await.unwrap();
        let mut rx = lifecycle.subscribe();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut spawned_count = 0;
        while let Some(e) = recv_timeout(&mut rx).await {
            if matches!(e, OrchestratorEvent::Spawned { .. }) {
                spawned_count += 1;
            }
        }
        assert_eq!(spawned_count, 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn session_restored_emitted_for_preexisting_sessions_on_first_tick() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("restored");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let mut old = fake_session("old", "demo");
        old.created_at = 1;
        old.status = SessionStatus::Working;
        sessions.save(&old).await.unwrap();

        let lifecycle = Arc::new(
            LifecycleManager::new(
                sessions.clone(),
                Arc::new(MockRuntime::new(true)) as Arc<dyn Runtime>,
                Arc::new(MockAgent::new(ActivityState::Ready)) as Arc<dyn Agent>,
            )
            .with_poll_interval(Duration::from_millis(20)),
        );

        let mut rx = lifecycle.subscribe();
        let handle = lifecycle.spawn();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut saw_restored = None;
        let mut saw_spawned = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                Ok(Ok(OrchestratorEvent::SessionRestored {
                    id,
                    project_id,
                    status,
                })) => {
                    saw_restored = Some((id, project_id, status));
                    break;
                }
                Ok(Ok(OrchestratorEvent::Spawned { .. })) => {
                    saw_spawned = true;
                }
                _ => {}
            }
        }

        handle.stop().await;
        assert!(
            !saw_spawned,
            "pre-existing session must not surface as Spawned"
        );
        let (id, project_id, status) = saw_restored.expect("SessionRestored was never emitted");
        assert_eq!(id.0, "old");
        assert_eq!(project_id, "demo");
        assert_eq!(status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn spawned_emitted_for_sessions_created_after_loop_startup() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("post-startup-spawn");
        let sessions = Arc::new(SessionManager::new(base.clone()));

        let lifecycle = Arc::new(
            LifecycleManager::new(
                sessions.clone(),
                Arc::new(MockRuntime::new(true)) as Arc<dyn Runtime>,
                Arc::new(MockAgent::new(ActivityState::Ready)) as Arc<dyn Agent>,
            )
            .with_poll_interval(Duration::from_millis(20)),
        );

        let mut rx = lifecycle.subscribe();
        let handle = lifecycle.spawn();

        tokio::time::sleep(Duration::from_millis(5)).await;
        sessions.save(&fake_session("fresh", "demo")).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut saw_spawned = false;
        let mut saw_restored = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                Ok(Ok(OrchestratorEvent::Spawned { id, .. })) if id.0 == "fresh" => {
                    saw_spawned = true;
                    break;
                }
                Ok(Ok(OrchestratorEvent::SessionRestored { id, .. })) if id.0 == "fresh" => {
                    saw_restored = true;
                }
                _ => {}
            }
        }

        handle.stop().await;
        assert!(!saw_restored, "fresh session must not surface as restored");
        assert!(saw_spawned, "fresh session never surfaced as Spawned");

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Auto-terminate on merge (issue #220) ---------- //

    #[tokio::test]
    async fn merged_pr_terminates_session_with_pr_merged_reason() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("auto-terminate-merged");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(crate::lifecycle::tests::MockScm::new());

        let lifecycle = Arc::new(
            LifecycleManager::new(sessions.clone(), runtime, agent)
                .with_scm(scm.clone() as Arc<dyn crate::traits::Scm>),
        );
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(crate::lifecycle::tests::fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Merged);
        scm.set_ci(CiStatus::Passing);
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
                OrchestratorEvent::Terminated {
                    reason: TerminationReason::PrMerged,
                    ..
                }
            )),
            "expected Terminated(PrMerged), got {events:?}"
        );

        let persisted = sessions.find_by_prefix("s1").await.unwrap();
        assert_eq!(persisted.status, SessionStatus::Killed);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn auto_terminate_on_merge_opt_out_leaves_session_in_merged() {
        use crate::config::LifecycleConfig;
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("auto-terminate-optout");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(crate::lifecycle::tests::MockScm::new());

        let lifecycle = Arc::new(
            LifecycleManager::new(sessions.clone(), runtime, agent)
                .with_scm(scm.clone() as Arc<dyn crate::traits::Scm>)
                .with_lifecycle_config(LifecycleConfig {
                    auto_terminate_on_merge: false,
                }),
        );
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(crate::lifecycle::tests::fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Merged);
        scm.set_ci(CiStatus::Passing);
        scm.set_review(ReviewDecision::None);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::Terminated {
                    reason: TerminationReason::PrMerged,
                    ..
                }
            )),
            "Terminated(PrMerged) must not fire when auto_terminate_on_merge=false, got {events:?}"
        );

        let persisted = sessions.find_by_prefix("s1").await.unwrap();
        assert_eq!(
            persisted.status,
            SessionStatus::Merged,
            "session must stay Merged when opt-out"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn closed_without_merge_does_not_emit_pr_merged_termination() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("auto-terminate-closed");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(crate::lifecycle::tests::MockScm::new());

        let lifecycle = Arc::new(
            LifecycleManager::new(sessions.clone(), runtime, agent)
                .with_scm(scm.clone() as Arc<dyn crate::traits::Scm>),
        );
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(crate::lifecycle::tests::fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Closed);
        scm.set_ci(CiStatus::Passing);
        scm.set_review(ReviewDecision::None);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::Terminated {
                    reason: TerminationReason::PrMerged,
                    ..
                }
            )),
            "Terminated(PrMerged) must not fire for closed-not-merged PR, got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn background_loop_starts_and_stops_cleanly() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("loop");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        sessions.save(&fake_session("s1", "demo")).await.unwrap();

        let lifecycle = Arc::new(
            LifecycleManager::new(
                sessions.clone(),
                Arc::new(MockRuntime::new(true)) as Arc<dyn Runtime>,
                Arc::new(MockAgent::new(ActivityState::Ready)) as Arc<dyn Agent>,
            )
            .with_poll_interval(Duration::from_millis(20)),
        );

        let mut rx = lifecycle.subscribe();
        let handle = lifecycle.spawn();

        let mut saw_status_change = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                Ok(Ok(OrchestratorEvent::StatusChanged { .. })) => {
                    saw_status_change = true;
                    break;
                }
                Ok(Ok(_)) => {}
                _ => {}
            }
        }

        handle.stop().await;
        assert!(
            saw_status_change,
            "background loop never emitted StatusChanged"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
