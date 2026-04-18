use super::*;

impl LifecycleManager {
    /// Decide whether the session should transition to `Stuck` *right now*,
    /// based on the idle-since bookkeeping and the configured `agent-stuck`
    /// threshold.
    ///
    /// This is the shared predicate used by:
    /// - `check_stuck` (step 6), which runs when no other transition happened
    ///   this tick.
    /// - `poll_scm` (step 5), which may override a would-be `PrOpen` transition
    ///   to `Stuck` so the tick still performs **one** status transition while
    ///   matching the TS reference behavior (where stuck detection can win over
    ///   the fallback `pr_open` state).
    pub(super) fn should_mark_stuck(&self, session: &Session) -> bool {
        if !is_stuck_eligible(session.status) {
            return false;
        }

        let idle_started = {
            let map = self.idle_since.lock().unwrap_or_else(|e| {
                tracing::error!("lifecycle idle_since mutex poisoned; recovering inner state: {e}");
                e.into_inner()
            });
            map.get(&session.id).copied()
        };
        let Some(idle_started) = idle_started else {
            return false;
        };

        let Some(engine) = self.reaction_engine.as_ref() else {
            return false;
        };
        let Some(cfg) = engine.resolve_reaction_config(session, "agent-stuck") else {
            return false;
        };
        let Some(raw) = cfg.threshold.as_deref() else {
            return false;
        };
        let Some(threshold) = parse_duration(raw) else {
            engine.warn_once_parse_failure("agent-stuck", "threshold", raw);
            return false;
        };

        idle_started.elapsed() > threshold
    }

    /// Phase H agent-stuck detection. Called from `poll_one` as the
    /// final transitioning step, gated on the pre-transition snapshot
    /// and the presence of a reaction engine.
    ///
    /// Early-returns (without erroring) in every "nothing to do" case:
    ///
    /// 1. Status is not stuck-eligible (terminal, already `Stuck`,
    ///    `NeedsInput`, etc.). Keeps us from double-firing and from
    ///    stuck-classifying states where idleness is expected.
    /// 2. No `idle_since` entry exists — the session is actively doing
    ///    something, so the stuck clock isn't running.
    /// 3. No `agent-stuck` reaction is configured. Missing config is a
    ///    deliberate "disabled" signal, not an error.
    /// 4. The configured `threshold` is absent or unparseable.
    ///    Malformed strings log a one-shot `tracing::warn!` via
    ///    `ReactionEngine::warn_once_parse_failure`, but do not panic.
    /// 5. The idle elapsed time has not yet *strictly* exceeded the
    ///    threshold (`elapsed <= threshold` early-returns; the flip
    ///    fires only on `elapsed > threshold`). Exactly-equal holds
    ///    the clock for one more tick, matching the strict `>` that
    ///    `ReactionEngine::dispatch` uses on `first_triggered_at`.
    ///
    /// Only when all five guards pass do we call `transition` into
    /// `SessionStatus::Stuck`, which in turn dispatches the
    /// `agent-stuck` reaction. Subsequent ticks see `Stuck` (not
    /// stuck-eligible) and stay stable until activity flips back.
    pub(super) async fn check_stuck(&self, session: &mut Session) -> Result<()> {
        if !self.should_mark_stuck(session) {
            return Ok(());
        }

        // All guards passed: park the session in `Stuck`. The
        // `transition` helper handles `agent-stuck` dispatch via the
        // existing `status_to_reaction_key` path and emits
        // `StatusChanged` on the event bus, so there is nothing
        // extra to do here.
        self.transition(session, SessionStatus::Stuck).await
    }

    /// Port of `maybeDispatchMergeConflicts` from
    /// `packages/core/src/lifecycle-manager.ts:1085-1188`.
    ///
    /// The merge-conflicts reaction is orthogonal to the status ladder:
    /// a PR can be `CONFLICTING` while the session sits at any of the
    /// six PR-track statuses (`pr_open` through `mergeable`). Collapsing
    /// it into a new `SessionStatus` variant would hide CI failures and
    /// review state simultaneously. Instead, we fire the reaction on top
    /// of whatever status the ladder settled on, and remember we fired
    /// it on the `Session` itself (`last_merge_conflict_dispatched`) so
    /// subsequent ticks observing the same conflict don't re-send.
    ///
    /// Three branches:
    ///
    /// 1. **Clear** — status is `Merged` or `Killed`: the PR is closed
    ///    out, conflict tracking is moot. Drops the reaction-engine
    ///    tracker and resets the flag. Matches TS lines 1106-1112.
    /// 2. **Dispatch** — conflicts present (`!readiness.no_conflicts`)
    ///    and flag not yet set: call `ReactionEngine::dispatch` once,
    ///    mark the flag, persist. Matches TS lines 1149-1180.
    /// 3. **Resolve** — no conflicts but the flag is set: clear tracker
    ///    and reset the flag so the next conflict episode fires again.
    ///    Matches TS lines 1181-1187.
    ///
    /// Guards (all early-return with `Ok(())`, matching TS's short-circuits):
    ///
    /// - No `reaction_engine` configured (caller gates this, but the
    ///   helper double-checks for safety).
    /// - No `observation` — either no PR in hand or SCM probes failed.
    ///   Matches TS `if (!project || !session.pr) return;` at line 1098.
    /// - Status not on the PR track for the dispatch/resolve branches;
    ///   the clear branch runs unconditionally on `Merged`/`Killed`.
    ///
    /// Action-routing (`send-to-agent` vs. `notify` vs. `auto-merge`) is
    /// delegated to `ReactionEngine::dispatch`, which the four existing
    /// status-driven reactions already use. No new per-action code here.
    pub(super) async fn check_merge_conflicts(
        &self,
        session: &mut Session,
        observation: Option<&ScmObservation>,
    ) -> Result<()> {
        let Some(engine) = self.reaction_engine.as_ref() else {
            return Ok(());
        };

        // Clear branch: the session has reached any terminal status.
        // Runs before the observation gate because terminal transitions
        // should reset state even on the tick that observed no PR (e.g.
        // the PR just got merged and gh no longer returns it). Widened
        // beyond TS's `merged | killed` to cover every terminal variant
        // because (a) `poll_scm` step 4 could transition into `Done` /
        // `Terminated` / `Errored` on the same tick, leaving stale flag
        // state on the persisted session, and (b) `is_terminal()` is the
        // canonical "no future ticks" predicate elsewhere in this file.
        if session.status.is_terminal() {
            engine.clear_tracker(&session.id, "merge-conflicts");
            if session.last_merge_conflict_dispatched.is_some() {
                session.last_merge_conflict_dispatched = None;
                self.sessions.save(session).await?;
            }
            return Ok(());
        }

        // Dispatch/resolve branches need a fresh observation (hence the
        // `readiness.no_conflicts` signal). No observation = no data =
        // skip this tick.
        let Some(observation) = observation else {
            return Ok(());
        };

        // Gate: only PR-track statuses. Sessions in `Working`, `Stuck`,
        // `NeedsInput`, etc. don't have a PR-level conflict concept even
        // if a `pr` was detected — matches the TS allowlist at 1116-1122.
        let eligible = matches!(
            session.status,
            SessionStatus::PrOpen
                | SessionStatus::CiFailed
                | SessionStatus::ReviewPending
                | SessionStatus::ChangesRequested
                | SessionStatus::Approved
                | SessionStatus::Mergeable
        );
        if !eligible {
            return Ok(());
        }

        let has_conflicts = !observation.readiness.no_conflicts;
        let already_dispatched = session.last_merge_conflict_dispatched == Some(true);

        if has_conflicts {
            if already_dispatched {
                return Ok(());
            }
            // `dispatch` handles `send-to-agent` / `notify` action routing
            // internally. It returns `Ok(Some(_))` when a reaction was
            // actually fired, `Ok(None)` when the key has no configured
            // reaction, and `Err(_)` on plugin failure. We only set the
            // suppression flag when the reaction *did* fire — matches TS
            // `lifecycle-manager.ts:1174`, which writes
            // `lastMergeConflictDispatched` only inside the `try` block
            // that performs the actual send. An `Err` propagates — flag
            // stays `None`, next tick retries; mirrors TS's `try/catch`.
            if engine.dispatch(session, "merge-conflicts").await?.is_some() {
                session.last_merge_conflict_dispatched = Some(true);
                self.sessions.save(session).await?;
            }
        } else if already_dispatched {
            // Conflicts resolved: re-arm so a *future* conflict fires
            // a fresh dispatch. Clearing the tracker also resets the
            // retry/escalation counter in `ReactionEngine` — the next
            // episode starts with a full budget.
            engine.clear_tracker(&session.id, "merge-conflicts");
            session.last_merge_conflict_dispatched = None;
            self.sessions.save(session).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::tests::{
        drain_events, fake_session, rewind_idle_since, setup, setup_stuck, MockAgent, MockRuntime,
        MockScm,
    };
    use crate::reactions::ReactionAction;
    use std::collections::HashSet;

    // ---------- Phase H: is_stuck_eligible classification ---------- //

    const ALL_SESSION_STATUSES: &[SessionStatus] = &[
        SessionStatus::Spawning,
        SessionStatus::Working,
        SessionStatus::NeedsInput,
        SessionStatus::Idle,
        SessionStatus::Stuck,
        SessionStatus::PrOpen,
        SessionStatus::CiFailed,
        SessionStatus::ReviewPending,
        SessionStatus::ChangesRequested,
        SessionStatus::Approved,
        SessionStatus::Mergeable,
        SessionStatus::MergeFailed,
        SessionStatus::Cleanup,
        SessionStatus::Merged,
        SessionStatus::Killed,
        SessionStatus::Terminated,
        SessionStatus::Done,
        SessionStatus::Errored,
    ];

    #[test]
    fn all_session_statuses_list_is_exhaustive_for_stuck_check() {
        for status in ALL_SESSION_STATUSES {
            match status {
                SessionStatus::Spawning
                | SessionStatus::Working
                | SessionStatus::NeedsInput
                | SessionStatus::Idle
                | SessionStatus::Stuck
                | SessionStatus::PrOpen
                | SessionStatus::CiFailed
                | SessionStatus::ReviewPending
                | SessionStatus::ChangesRequested
                | SessionStatus::Approved
                | SessionStatus::Mergeable
                | SessionStatus::MergeFailed
                | SessionStatus::Cleanup
                | SessionStatus::Merged
                | SessionStatus::Killed
                | SessionStatus::Terminated
                | SessionStatus::Done
                | SessionStatus::Errored => {}
            }
        }
    }

    #[test]
    fn is_stuck_eligible_classifies_every_variant() {
        for &status in ALL_SESSION_STATUSES {
            let expected = matches!(
                status,
                SessionStatus::Working
                    | SessionStatus::PrOpen
                    | SessionStatus::CiFailed
                    | SessionStatus::ReviewPending
                    | SessionStatus::ChangesRequested
                    | SessionStatus::Approved
                    | SessionStatus::Mergeable
            );
            assert_eq!(
                is_stuck_eligible(status),
                expected,
                "is_stuck_eligible({status:?}) classification mismatch"
            );
        }
    }

    #[test]
    fn is_stuck_eligible_excludes_merge_failed() {
        assert!(!is_stuck_eligible(SessionStatus::MergeFailed));
    }

    #[test]
    fn is_stuck_eligible_excludes_needs_input() {
        assert!(!is_stuck_eligible(SessionStatus::NeedsInput));
    }

    // ---------- idle_since bookkeeping (Phase H) ---------- //

    #[tokio::test]
    async fn update_idle_since_inserts_preserves_and_clears() {
        let (lifecycle, _sessions, _rt, _agent, _base) =
            setup("idle_since_helper", ActivityState::Idle).await;
        let id = SessionId("sess-idle".into());

        let read_entry = |lm: &LifecycleManager| -> Option<Instant> {
            lm.idle_since
                .lock()
                .unwrap_or_else(|e| {
                    tracing::error!("idle_since mutex poisoned; recovering inner state: {e}");
                    e.into_inner()
                })
                .get(&id)
                .copied()
        };

        assert!(read_entry(&lifecycle).is_none());

        lifecycle.update_idle_since(&id, ActivityState::Idle);
        let t1 = read_entry(&lifecycle).expect("first idle should insert");

        lifecycle.update_idle_since(&id, ActivityState::Idle);
        let t2 = read_entry(&lifecycle).expect("second idle should keep entry");
        assert_eq!(t1, t2, "idle → idle must not reset the timestamp");

        lifecycle.update_idle_since(&id, ActivityState::Blocked);
        let t3 = read_entry(&lifecycle).expect("blocked should keep entry");
        assert_eq!(t1, t3, "idle → blocked must not reset the timestamp");

        lifecycle.update_idle_since(&id, ActivityState::Active);
        assert!(
            read_entry(&lifecycle).is_none(),
            "active activity must clear idle_since"
        );

        lifecycle.update_idle_since(&id, ActivityState::Idle);
        assert!(
            read_entry(&lifecycle).is_some(),
            "idle after clear must re-insert"
        );

        lifecycle.update_idle_since(&id, ActivityState::WaitingInput);
        assert!(
            read_entry(&lifecycle).is_none(),
            "waiting_input must clear idle_since"
        );

        lifecycle.update_idle_since(&id, ActivityState::Idle);
        lifecycle.update_idle_since(&id, ActivityState::Ready);
        assert!(
            read_entry(&lifecycle).is_none(),
            "ready must clear idle_since"
        );
    }

    #[tokio::test]
    async fn update_idle_since_tracks_sessions_independently() {
        let (lifecycle, _sessions, _rt, _agent, _base) =
            setup("idle_since_multi", ActivityState::Idle).await;
        let a = SessionId("sess-a".into());
        let b = SessionId("sess-b".into());

        lifecycle.update_idle_since(&a, ActivityState::Idle);
        lifecycle.update_idle_since(&b, ActivityState::Idle);

        lifecycle.update_idle_since(&a, ActivityState::Active);

        let map = lifecycle.idle_since.lock().unwrap_or_else(|e| {
            tracing::error!("idle_since mutex poisoned; recovering inner state: {e}");
            e.into_inner()
        });
        assert!(!map.contains_key(&a), "sess-a should have been cleared");
        assert!(map.contains_key(&b), "sess-b should still be idle");
    }

    // ---------- Phase H: agent-stuck detection integration ---------- //

    async fn setup_stuck_no_config(
        label: &str,
    ) -> (
        Arc<LifecycleManager>,
        Arc<crate::session_manager::SessionManager>,
        Arc<MockAgent>,
        std::path::PathBuf,
    ) {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::{ReactionAction, ReactionConfig};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir(label);
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent = Arc::new(MockAgent::new(ActivityState::Idle));

        let lifecycle =
            LifecycleManager::new(sessions.clone(), runtime, agent.clone() as Arc<dyn Agent>);

        let mut cfg = ReactionConfig::new(ReactionAction::Notify);
        cfg.message = Some("other".into());
        let mut map = std::collections::HashMap::new();
        map.insert("ci-failed".into(), cfg);
        let engine_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine));
        (lifecycle, sessions, agent, base)
    }

    #[tokio::test]
    async fn stuck_detection_fires_on_working_after_threshold() {
        let (lifecycle, sessions, _agent, base) =
            setup_stuck("stuck_from_working", Some("1s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        let early = drain_events(&mut rx).await;
        assert!(
            !early.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "stuck transition must not fire before threshold elapses: {early:?}"
        );

        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let later = drain_events(&mut rx).await;
        assert!(
            later.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "expected Working → Stuck transition after threshold; got {later:?}"
        );
        assert!(
            later
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::ReactionTriggered { .. })),
            "expected ReactionTriggered for agent-stuck; got {later:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_detection_fires_on_pr_open_after_threshold() {
        let (lifecycle, sessions, _agent, base) =
            setup_stuck("stuck_from_pr_open", Some("1s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s2", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::PrOpen,
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "expected PrOpen → Stuck after threshold; got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_recovers_to_working_on_active_activity() {
        let (lifecycle, sessions, agent, base) = setup_stuck("stuck_recovery", Some("1s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s3", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let reloaded = sessions.list().await.unwrap();
        assert_eq!(reloaded[0].status, SessionStatus::Stuck);

        let _ = drain_events(&mut rx).await;

        agent.set(ActivityState::Active);
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Stuck,
                    to: SessionStatus::Working,
                    ..
                }
            )),
            "expected Stuck → Working recovery; got {events:?}"
        );

        let map = lifecycle.idle_since.lock().unwrap_or_else(|e| {
            tracing::error!("idle_since mutex poisoned; recovering inner state: {e}");
            e.into_inner()
        });
        assert!(
            !map.contains_key(&s.id),
            "idle_since should be cleared after recovery"
        );
        drop(map);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_not_triggered_without_agent_stuck_config() {
        let (lifecycle, sessions, _agent, base) = setup_stuck_no_config("stuck_no_config").await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s4", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "no agent-stuck config means no stuck transition; got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_not_triggered_before_threshold_elapses() {
        let (lifecycle, sessions, _agent, base) =
            setup_stuck("stuck_before_threshold", Some("10s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s5", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "stuck transition must not fire before 10s threshold; got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_failed_does_not_become_stuck() {
        let (lifecycle, sessions, _agent, base) =
            setup_stuck("merge_failed_no_stuck", Some("1s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s6", "demo");
        s.status = SessionStatus::MergeFailed;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "MergeFailed must not be stuck-eligible; got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn one_transition_per_tick_prefers_scm_transition_over_stuck() {
        use crate::lifecycle::tests::{fake_pr, unique_temp_dir};
        use crate::reactions::ReactionConfig;
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("one_transition_per_tick_scm_over_stuck");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Idle));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);

        let mut stuck_cfg = ReactionConfig::new(ReactionAction::Notify);
        stuck_cfg.threshold = Some("1s".into());
        let ci_cfg = ReactionConfig::new(ReactionAction::Notify);
        let mut map = std::collections::HashMap::new();
        map.insert("agent-stuck".into(), stuck_cfg);
        map.insert("ci-failed".into(), ci_cfg);
        let engine_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime,
            lifecycle.events_sender(),
        ));

        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine)
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::Pending);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::CiFailed,
                    ..
                }
            )),
            "expected Working → CiFailed transition, got {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "must NOT transition to Stuck on same tick as SCM transition: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- merge-conflicts reaction (issue #192) ---------- //

    fn script_conflicting_pr(scm: &MockScm, pr_number: u32) {
        use crate::lifecycle::tests::fake_pr;
        use crate::scm::{CiStatus, MergeReadiness, PrState, ReviewDecision};
        scm.set_pr(Some(fake_pr(pr_number, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::None);
        scm.set_readiness(MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: false,
            no_conflicts: false,
            blockers: vec!["conflicts".into()],
        });
    }

    fn clear_conflicts(scm: &MockScm) {
        use crate::scm::MergeReadiness;
        scm.set_readiness(MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: false,
            no_conflicts: true,
            blockers: vec!["pending".into()],
        });
    }

    #[tokio::test]
    async fn merge_conflicts_dispatches_once_on_conflicting_pr() {
        use crate::lifecycle::tests::setup_with_merge_conflicts_engine;
        let (lifecycle, sessions, scm, runtime, engine, base) =
            setup_with_merge_conflicts_engine("mc-once").await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        script_conflicting_pr(&scm, 42);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        assert_eq!(
            engine.attempts(&s.id, "merge-conflicts"),
            1,
            "expected exactly one merge-conflicts dispatch"
        );
        let sends = runtime.sends();
        assert!(
            sends.iter().any(|(_, msg)| msg == "please rebase"),
            "expected rebase message to be sent, got {sends:?}"
        );
        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].last_merge_conflict_dispatched,
            Some(true),
            "flag should be set after successful dispatch"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_conflicts_suppresses_redispatch_on_subsequent_tick() {
        use crate::lifecycle::tests::setup_with_merge_conflicts_engine;
        let (lifecycle, sessions, scm, _runtime, engine, base) =
            setup_with_merge_conflicts_engine("mc-suppress").await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        script_conflicting_pr(&scm, 42);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        assert_eq!(
            engine.attempts(&s.id, "merge-conflicts"),
            1,
            "second tick with same conflict must NOT re-dispatch"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_conflicts_rearms_after_resolution() {
        use crate::lifecycle::tests::setup_with_merge_conflicts_engine;
        let (lifecycle, sessions, scm, _runtime, engine, base) =
            setup_with_merge_conflicts_engine("mc-rearm").await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        script_conflicting_pr(&scm, 42);
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine.attempts(&s.id, "merge-conflicts"), 1);
        assert_eq!(
            sessions.list().await.unwrap()[0].last_merge_conflict_dispatched,
            Some(true)
        );

        clear_conflicts(&scm);
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            engine.attempts(&s.id, "merge-conflicts"),
            0,
            "resolve branch must clear the tracker"
        );
        assert_eq!(
            sessions.list().await.unwrap()[0].last_merge_conflict_dispatched,
            None,
            "flag must reset on resolution"
        );

        script_conflicting_pr(&scm, 42);
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            engine.attempts(&s.id, "merge-conflicts"),
            1,
            "re-armed tracker must fire on the next conflict"
        );
        assert_eq!(
            sessions.list().await.unwrap()[0].last_merge_conflict_dispatched,
            Some(true)
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_conflicts_unconfigured_no_op() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("mc-unconfigured");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle =
            LifecycleManager::new(sessions.clone(), runtime.clone() as Arc<dyn Runtime>, agent);
        let engine = Arc::new(ReactionEngine::new(
            std::collections::HashMap::new(),
            runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();
        script_conflicting_pr(&scm, 42);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        assert!(
            runtime.sends().is_empty(),
            "unconfigured reaction must not send anything"
        );
        assert_eq!(engine.attempts(&s.id, "merge-conflicts"), 0);
        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].last_merge_conflict_dispatched, None,
            "no-config dispatch must leave the suppression flag untouched"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_conflicts_clears_on_merged_status() {
        use crate::lifecycle::tests::setup_with_merge_conflicts_engine;
        use crate::scm::{CiStatus, MergeReadiness, PrState, ReviewDecision};
        let (lifecycle, sessions, scm, _runtime, engine, base) =
            setup_with_merge_conflicts_engine("mc-clear-merged").await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        s.last_merge_conflict_dispatched = Some(true);
        sessions.save(&s).await.unwrap();

        let prime = s.clone();
        lifecycle
            .reaction_engine
            .as_ref()
            .unwrap()
            .dispatch(&prime, "merge-conflicts")
            .await
            .unwrap();
        assert_eq!(engine.attempts(&s.id, "merge-conflicts"), 1);

        use crate::lifecycle::tests::fake_pr;
        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Merged);
        scm.set_ci(CiStatus::Passing);
        scm.set_review(ReviewDecision::Approved);
        scm.set_readiness(MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        });

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Merged);
        assert_eq!(
            persisted[0].last_merge_conflict_dispatched, None,
            "clear branch must reset the flag on Merged"
        );
        assert_eq!(
            engine.attempts(&s.id, "merge-conflicts"),
            0,
            "clear branch must drop the tracker on Merged"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_conflicts_ignored_on_non_pr_track_status() {
        use crate::lifecycle::tests::setup_with_merge_conflicts_engine;
        let (lifecycle, sessions, _scm, runtime, engine, base) =
            setup_with_merge_conflicts_engine("mc-non-pr-track").await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        assert_eq!(engine.attempts(&s.id, "merge-conflicts"), 0);
        assert!(runtime.sends().is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }
}
