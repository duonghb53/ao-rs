//! Slice 2 Phase D — Reaction dispatch.
//!
//! The reaction engine sits between `LifecycleManager` and the side-effect
//! plugins (`Runtime::send_message`, `Scm::merge`). When the lifecycle loop
//! observes a status transition into a "trigger state" like `CiFailed`, it
//! asks the engine to fire the corresponding reaction. The engine looks the
//! reaction up in `AoConfig::reactions`, tracks attempts, and runs the
//! configured action — or escalates to `Notify` when retries are exhausted.
//!
//! Mirrors `executeReaction` / `ReactionTracker` from
//! `packages/core/src/lifecycle-manager.ts` (lines ~570-710 in the reference).
//!
//! ## Why the engine is separate from LifecycleManager
//!
//! TS bundled everything into one big `createLifecycleManager` closure. In
//! Rust we split them so:
//!
//! 1. Tests can exercise the engine without a polling loop — unit tests
//!    build a `ReactionEngine` directly, call `dispatch`, and assert events.
//! 2. Future CLI commands (`ao-rs react fire ci-failed <id>`) can reuse
//!    the engine directly without going through lifecycle ticks.
//! 3. The lifecycle loop stays a thin state machine; all "business logic"
//!    about what an action *means* lives here.
//!
//! ## Tracker semantics
//!
//! The tracker is keyed on `(SessionId, reaction_key)`. One tracker per
//! (session, reaction) pair, regardless of how many times the same status
//! transition fires. A tracker is:
//!
//! - **Incremented** on every dispatch attempt (including the one that
//!   ultimately escalates).
//! - **Cleared** by `LifecycleManager` via `clear_tracker` when a session
//!   *leaves* the triggering status — so a new CI failure after a fix
//!   doesn't inherit the old failure's retry budget. This matches the TS
//!   `clearReactionTracker` calls on transition reset.
//!
//! ## What Phase D does NOT do
//!
//! - Duration-based escalation (`escalate-after: 10m`) is recognised but
//!   not honoured: the engine logs-once and only escalates on attempt
//!   counts. Adding a wall-clock parser belongs next to the duration use
//!   in Phase E's `agent-stuck` reaction.
//! - `auto-merge` does not actually merge. It emits `ReactionTriggered`
//!   and logs "would merge"; Phase E wires the `Scm::merge` call and adds
//!   `ao-rs merge <id>` as the imperative alternative.
//! - Notifier plugins. `Notify` just emits `ReactionTriggered` on the
//!   broadcast channel — CLI subscribers turn that into `println!`. A
//!   proper notifier trait (Slack, desktop, …) is post-Slice-2.

use crate::{
    error::Result,
    events::OrchestratorEvent,
    reactions::{EscalateAfter, ReactionAction, ReactionConfig, ReactionOutcome},
    traits::Runtime,
    types::{Session, SessionId, SessionStatus},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

/// Per-(session, reaction) attempt bookkeeping. Mirrors TS `ReactionTracker`.
#[derive(Debug, Clone, Copy)]
struct TrackerState {
    /// How many times this reaction has been dispatched for this session.
    /// Incremented *before* the action runs, so a dispatch that errored
    /// still counts.
    attempts: u32,
}

/// Map a `SessionStatus` to the reaction key that should fire on entry.
///
/// Returns `None` for statuses that don't map to a reaction today. The
/// three Phase D reactions are `ci-failed`, `changes-requested`,
/// `approved-and-green`; everything else returns `None` so the engine
/// is a no-op on unrelated transitions.
///
/// Public so `LifecycleManager` can peek at the mapping without having
/// to duplicate it — and so Phase E tests can assert additional mappings
/// (e.g. `Stuck` → `"agent-stuck"`) by extending this one spot.
pub const fn status_to_reaction_key(status: SessionStatus) -> Option<&'static str> {
    match status {
        SessionStatus::CiFailed => Some("ci-failed"),
        SessionStatus::ChangesRequested => Some("changes-requested"),
        SessionStatus::Mergeable => Some("approved-and-green"),
        // TODO(PhaseE): add Stuck → "agent-stuck" and Errored → "agent-errored".
        // agent-stuck needs auxiliary state (time entered Idle) that the
        // engine doesn't track today — the pure status-to-key mapping will
        // work, but the engine side needs a `status_entered_at` tracker.
        _ => None,
    }
}

/// The reaction dispatcher. Holds config, attempt trackers, and the
/// Runtime handle needed to actually talk to the agent process.
///
/// `Arc<ReactionEngine>` is what gets wired into `LifecycleManager`.
pub struct ReactionEngine {
    /// Reaction-key → config. Sourced from `AoConfig::reactions` at build
    /// time. Hot-reload is deferred — a config change today needs a
    /// lifecycle restart, which matches how the TS reference behaves.
    config: HashMap<String, ReactionConfig>,
    /// Runtime used for `SendToAgent`. Required because every reaction
    /// configuration today could choose `send-to-agent` as its action.
    runtime: Arc<dyn Runtime>,
    /// Shared broadcast channel — cloned from `LifecycleManager::events_tx`.
    /// The engine emits `ReactionTriggered` / `ReactionEscalated` here so
    /// subscribers see them alongside lifecycle events.
    events_tx: broadcast::Sender<OrchestratorEvent>,
    /// Per-(session, reaction) attempt state. `Mutex` (not async) because
    /// the critical sections are tiny map mutations — no awaiting.
    trackers: Mutex<HashMap<(SessionId, String), TrackerState>>,
}

impl ReactionEngine {
    /// Build an engine from a loaded config. The caller owns the runtime
    /// and the broadcast channel — typically `LifecycleManager` hands its
    /// own `events_tx` in via `clone()` so engine events share the channel.
    pub fn new(
        config: HashMap<String, ReactionConfig>,
        runtime: Arc<dyn Runtime>,
        events_tx: broadcast::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            config,
            runtime,
            events_tx,
            trackers: Mutex::new(HashMap::new()),
        }
    }

    /// Fire the reaction configured for `reaction_key` against `session`,
    /// if any. Returns `None` when there's no matching config — the
    /// caller (usually `LifecycleManager::transition`) treats that as
    /// "silently do nothing" rather than an error.
    ///
    /// `session` is borrowed (not cloned) because dispatch only needs
    /// the ID and runtime handle; nothing is persisted back.
    pub async fn dispatch(
        &self,
        session: &Session,
        reaction_key: &str,
    ) -> Result<Option<ReactionOutcome>> {
        let Some(cfg) = self.config.get(reaction_key).cloned() else {
            tracing::debug!(
                reaction = reaction_key,
                session = %session.id,
                "no reaction configured; skipping"
            );
            return Ok(None);
        };

        // `auto: false` means "the key exists so don't fall through to a
        // default, but don't actually do anything automatically". For
        // non-notify actions we skip entirely. For `Notify` we DO run it
        // (a disabled reaction still wants to surface to a human) but we
        // bypass the retry/escalation machinery — `auto: false` notify
        // has no budget, it just fires once per transition. Otherwise a
        // user who configured `auto: false` + `retries: 0` would see
        // spurious escalations on the first attempt.
        if !cfg.auto {
            if cfg.action == ReactionAction::Notify {
                let outcome = self.dispatch_notify(session, reaction_key, &cfg);
                return Ok(Some(outcome));
            }
            tracing::debug!(
                reaction = reaction_key,
                session = %session.id,
                "reaction auto: false; skipping non-notify action"
            );
            return Ok(None);
        }

        // Bump attempts under the lock and decide escalation inside the
        // same critical section so two concurrent dispatches can't both
        // escape the retry budget.
        let (attempts, should_escalate) = {
            let mut trackers = self
                .trackers
                .lock()
                .expect("reaction tracker mutex poisoned");
            let entry = trackers
                .entry((session.id.clone(), reaction_key.to_string()))
                .or_insert(TrackerState { attempts: 0 });
            entry.attempts += 1;
            let attempts = entry.attempts;

            // TS semantics: `retries` is the MAX number of attempts the
            // engine will make before escalating. Unset = infinite.
            let max_attempts = cfg.retries;
            let mut escalate = max_attempts.is_some_and(|n| attempts > n);

            // `escalate-after: N` (Attempts form) is an independent gate
            // with the same `>` comparison. Duration form is a no-op in
            // Phase D — see module comment.
            if let Some(EscalateAfter::Attempts(n)) = cfg.escalate_after {
                if attempts > n {
                    escalate = true;
                }
            } else if matches!(cfg.escalate_after, Some(EscalateAfter::Duration(_))) {
                // Duration-based escalation is Phase E — see module doc.
                // Logged at `trace!` to avoid spamming a watcher running
                // with `RUST_LOG=debug` once per poll tick.
                tracing::trace!(
                    reaction = reaction_key,
                    "duration-based escalate-after not implemented; ignoring"
                );
            }

            (attempts, escalate)
        };

        if should_escalate {
            self.emit(OrchestratorEvent::ReactionEscalated {
                id: session.id.clone(),
                reaction_key: reaction_key.to_string(),
                attempts,
            });
            // Escalation ALWAYS reports as an executed `Notify`, regardless
            // of the originally configured action. This matches the TS
            // `action: "escalated"` semantic but uses our existing enum.
            self.emit(OrchestratorEvent::ReactionTriggered {
                id: session.id.clone(),
                reaction_key: reaction_key.to_string(),
                action: ReactionAction::Notify,
            });
            return Ok(Some(ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: true,
                action: ReactionAction::Notify,
                message: cfg.message.clone(),
                escalated: true,
            }));
        }

        let outcome = match cfg.action {
            ReactionAction::SendToAgent => {
                self.dispatch_send_to_agent(session, reaction_key, &cfg)
                    .await
            }
            ReactionAction::Notify => self.dispatch_notify(session, reaction_key, &cfg),
            ReactionAction::AutoMerge => self.dispatch_auto_merge(session, reaction_key, &cfg),
        };
        Ok(Some(outcome))
    }

    /// Forget any tracker state for `(session, reaction_key)`. Called by
    /// `LifecycleManager` on the tick that *leaves* a triggering status,
    /// so the next time the session re-enters it, retries start from zero.
    /// A lingering tracker would mean a session that failed CI, was fixed,
    /// and failed again would start already half-way through the retry
    /// budget — not what anyone wants.
    pub fn clear_tracker(&self, session_id: &SessionId, reaction_key: &str) {
        let mut trackers = self
            .trackers
            .lock()
            .expect("reaction tracker mutex poisoned");
        trackers.remove(&(session_id.clone(), reaction_key.to_string()));
    }

    /// Drop every tracker entry for `session_id`. Called by
    /// `LifecycleManager::terminate` — without this, the map would grow
    /// monotonically over a long-running `ao-rs watch` as terminated
    /// sessions leave orphan entries behind. Cheap: one full-map walk
    /// per termination, and the N is small (reaction-key count).
    pub fn clear_all_for_session(&self, session_id: &SessionId) {
        let mut trackers = self
            .trackers
            .lock()
            .expect("reaction tracker mutex poisoned");
        trackers.retain(|(sid, _), _| sid != session_id);
    }

    /// Current attempt count for `(session, reaction_key)`. Returns 0 if
    /// no tracker exists yet. Exposed for tests and for future CLI
    /// debugging (`ao-rs react status <id>`).
    pub fn attempts(&self, session_id: &SessionId, reaction_key: &str) -> u32 {
        self.trackers
            .lock()
            .expect("reaction tracker mutex poisoned")
            .get(&(session_id.clone(), reaction_key.to_string()))
            .map(|t| t.attempts)
            .unwrap_or(0)
    }

    // ---------- action implementations ---------- //

    async fn dispatch_send_to_agent(
        &self,
        session: &Session,
        reaction_key: &str,
        cfg: &ReactionConfig,
    ) -> ReactionOutcome {
        // `SendToAgent` requires a message body. A missing message is
        // recorded as a failure rather than falling through to a generic
        // boilerplate — Phase D keeps the config honest and surfaces bad
        // configs rather than silently sending noise to the agent.
        let Some(message) = cfg.message.clone() else {
            tracing::warn!(
                reaction = reaction_key,
                session = %session.id,
                "send-to-agent configured without a message; skipping"
            );
            return ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: false,
                action: ReactionAction::SendToAgent,
                message: None,
                escalated: false,
            };
        };

        // `send-to-agent` needs a live runtime handle. A session that's
        // still Spawning may not have one yet — count it as a soft failure
        // (no event emitted) so the next tick can retry.
        let Some(handle) = session.runtime_handle.as_deref() else {
            tracing::warn!(
                reaction = reaction_key,
                session = %session.id,
                "send-to-agent but session has no runtime_handle yet"
            );
            return ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: false,
                action: ReactionAction::SendToAgent,
                message: Some(message),
                escalated: false,
            };
        };

        match self.runtime.send_message(handle, &message).await {
            Ok(()) => {
                self.emit(OrchestratorEvent::ReactionTriggered {
                    id: session.id.clone(),
                    reaction_key: reaction_key.to_string(),
                    action: ReactionAction::SendToAgent,
                });
                ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: true,
                    action: ReactionAction::SendToAgent,
                    message: Some(message),
                    escalated: false,
                }
            }
            Err(e) => {
                // Don't emit a triggered event on send failure — subscribers
                // would misread it as "message delivered". The tracker has
                // already been incremented, so the next dispatch (from the
                // next tick) will count against the same retry budget.
                tracing::warn!(
                    reaction = reaction_key,
                    session = %session.id,
                    error = %e,
                    "runtime.send_message failed; retry next tick"
                );
                ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: false,
                    action: ReactionAction::SendToAgent,
                    message: Some(message),
                    escalated: false,
                }
            }
        }
    }

    fn dispatch_notify(
        &self,
        session: &Session,
        reaction_key: &str,
        cfg: &ReactionConfig,
    ) -> ReactionOutcome {
        self.emit(OrchestratorEvent::ReactionTriggered {
            id: session.id.clone(),
            reaction_key: reaction_key.to_string(),
            action: ReactionAction::Notify,
        });
        ReactionOutcome {
            reaction_type: reaction_key.to_string(),
            success: true,
            action: ReactionAction::Notify,
            message: cfg.message.clone(),
            escalated: false,
        }
    }

    fn dispatch_auto_merge(
        &self,
        session: &Session,
        reaction_key: &str,
        _cfg: &ReactionConfig,
    ) -> ReactionOutcome {
        // Phase D just records the intent — Phase E will call
        // `Scm::merge` from here once LifecycleManager carries an
        // `Arc<dyn Scm>` and knows the session's PR.
        tracing::info!(
            reaction = reaction_key,
            session = %session.id,
            "auto-merge requested; Phase D emits intent only (Phase E: wire Scm::merge)"
        );
        self.emit(OrchestratorEvent::ReactionTriggered {
            id: session.id.clone(),
            reaction_key: reaction_key.to_string(),
            action: ReactionAction::AutoMerge,
        });
        ReactionOutcome {
            reaction_type: reaction_key.to_string(),
            success: true,
            action: ReactionAction::AutoMerge,
            message: None,
            escalated: false,
        }
    }

    /// Broadcast an event. A send error means zero subscribers — the
    /// same "not worth surfacing" case as `LifecycleManager::emit`.
    fn emit(&self, event: OrchestratorEvent) {
        let _ = self.events_tx.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        reactions::{EscalateAfter, ReactionAction, ReactionConfig},
        traits::Runtime,
        types::{now_ms, ActivityState, Session, SessionId, SessionStatus},
    };
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex as StdMutex;

    // ---------- Helpers ---------- //

    /// Mock runtime that records every send_message for assertions.
    struct RecordingRuntime {
        sends: StdMutex<Vec<(String, String)>>,
        fail_send: std::sync::atomic::AtomicBool,
    }

    impl RecordingRuntime {
        fn new() -> Self {
            Self {
                sends: StdMutex::new(Vec::new()),
                fail_send: std::sync::atomic::AtomicBool::new(false),
            }
        }
        fn sends(&self) -> Vec<(String, String)> {
            self.sends.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Runtime for RecordingRuntime {
        async fn create(
            &self,
            _session_id: &str,
            _cwd: &Path,
            _launch_command: &str,
            _env: &[(String, String)],
        ) -> Result<String> {
            Ok("mock-handle".into())
        }
        async fn send_message(&self, handle: &str, msg: &str) -> Result<()> {
            if self.fail_send.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("mock send failed".into()));
            }
            self.sends
                .lock()
                .unwrap()
                .push((handle.to_string(), msg.to_string()));
            Ok(())
        }
        async fn is_alive(&self, _handle: &str) -> Result<bool> {
            Ok(true)
        }
        async fn destroy(&self, _handle: &str) -> Result<()> {
            Ok(())
        }
    }

    fn fake_session(id: &str) -> Session {
        Session {
            id: SessionId(id.into()),
            project_id: "demo".into(),
            status: SessionStatus::CiFailed,
            branch: format!("ao-{id}"),
            task: "t".into(),
            workspace_path: Some(PathBuf::from("/tmp/ws")),
            runtime_handle: Some(format!("handle-{id}")),
            activity: Some(ActivityState::Ready),
            created_at: now_ms(),
        }
    }

    fn build(
        cfg_map: HashMap<String, ReactionConfig>,
    ) -> (
        Arc<ReactionEngine>,
        Arc<RecordingRuntime>,
        broadcast::Receiver<OrchestratorEvent>,
    ) {
        let runtime = Arc::new(RecordingRuntime::new());
        let (tx, rx) = broadcast::channel(32);
        let engine = Arc::new(ReactionEngine::new(
            cfg_map,
            runtime.clone() as Arc<dyn Runtime>,
            tx,
        ));
        (engine, runtime, rx)
    }

    fn drain(rx: &mut broadcast::Receiver<OrchestratorEvent>) -> Vec<OrchestratorEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    // ---------- Tests ---------- //

    #[test]
    fn status_map_covers_phase_d_reactions() {
        assert_eq!(
            status_to_reaction_key(SessionStatus::CiFailed),
            Some("ci-failed")
        );
        assert_eq!(
            status_to_reaction_key(SessionStatus::ChangesRequested),
            Some("changes-requested")
        );
        assert_eq!(
            status_to_reaction_key(SessionStatus::Mergeable),
            Some("approved-and-green")
        );
        assert_eq!(status_to_reaction_key(SessionStatus::Working), None);
        assert_eq!(status_to_reaction_key(SessionStatus::Approved), None);
    }

    #[tokio::test]
    async fn dispatch_unconfigured_key_is_noop() {
        let (engine, runtime, mut rx) = build(HashMap::new());
        let session = fake_session("s1");
        let result = engine.dispatch(&session, "ci-failed").await.unwrap();
        assert!(result.is_none());
        assert!(runtime.sends().is_empty());
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn dispatch_send_to_agent_calls_runtime_and_emits_event() {
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("CI broke — please fix.".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, mut rx) = build(map);
        let session = fake_session("s1");

        let result = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();

        assert!(result.success);
        assert!(!result.escalated);
        assert_eq!(result.action, ReactionAction::SendToAgent);
        assert_eq!(runtime.sends().len(), 1);
        assert_eq!(runtime.sends()[0].0, "handle-s1");
        assert_eq!(runtime.sends()[0].1, "CI broke — please fix.");

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            OrchestratorEvent::ReactionTriggered {
                reaction_key,
                action,
                ..
            } => {
                assert_eq!(reaction_key, "ci-failed");
                assert_eq!(*action, ReactionAction::SendToAgent);
            }
            other => panic!("unexpected event {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_send_to_agent_without_message_fails_softly() {
        let config = ReactionConfig::new(ReactionAction::SendToAgent); // no message
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, mut rx) = build(map);
        let session = fake_session("s1");
        let result = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();

        assert!(!result.success);
        assert!(runtime.sends().is_empty());
        // No event emitted on soft failure — subscribers shouldn't see a
        // "triggered" event for a dispatch that never left the engine.
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn dispatch_send_to_agent_propagates_runtime_send_failure_as_soft_failure() {
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix it".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, mut rx) = build(map);
        runtime.fail_send.store(true, Ordering::SeqCst);
        let session = fake_session("s1");

        let result = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(!result.success);
        // Attempt still counted — so the next tick's retry competes for
        // the same retry budget. Guards against a pathological case where
        // the engine would forget that it tried.
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 1);
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn dispatch_notify_emits_event_and_succeeds() {
        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("approved & green".into());
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let (engine, runtime, mut rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();

        assert!(result.success);
        assert_eq!(result.action, ReactionAction::Notify);
        assert!(runtime.sends().is_empty());

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::Notify,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn dispatch_auto_merge_emits_intent_event() {
        let config = ReactionConfig::new(ReactionAction::AutoMerge);
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let (engine, _runtime, mut rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();
        assert!(result.success);
        assert_eq!(result.action, ReactionAction::AutoMerge);

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::AutoMerge,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn dispatch_auto_false_skips_active_actions_but_allows_notify() {
        // `auto: false` on SendToAgent → no-op.
        let mut sta = ReactionConfig::new(ReactionAction::SendToAgent);
        sta.auto = false;
        sta.message = Some("noop".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), sta);

        // `auto: false` on Notify → still runs (notify is always a human
        // call, the disable flag doesn't gate it).
        let mut notify = ReactionConfig::new(ReactionAction::Notify);
        notify.auto = false;
        map.insert("approved-and-green".into(), notify);

        let (engine, runtime, mut rx) = build(map);

        // Active action should be skipped entirely — no outcome, no event.
        let s1 = fake_session("s1");
        assert!(engine.dispatch(&s1, "ci-failed").await.unwrap().is_none());
        assert!(runtime.sends().is_empty());
        assert!(drain(&mut rx).is_empty());

        // Notify must still fire.
        let mut s2 = fake_session("s2");
        s2.status = SessionStatus::Mergeable;
        let result = engine
            .dispatch(&s2, "approved-and-green")
            .await
            .unwrap()
            .unwrap();
        assert!(result.success);
        assert_eq!(result.action, ReactionAction::Notify);
    }

    #[tokio::test]
    async fn retries_exhausted_escalates_to_notify_and_emits_both_events() {
        // retries: 2 means the 3rd dispatch attempt is the one that escalates.
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.retries = Some(2);
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, mut rx) = build(map);
        let session = fake_session("s1");

        // Attempts 1 and 2: normal SendToAgent.
        let r1 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r1.success);
        assert!(!r1.escalated);
        let r2 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r2.success);
        assert!(!r2.escalated);
        assert_eq!(runtime.sends().len(), 2);

        // Attempt 3: escalate.
        let r3 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r3.escalated);
        assert_eq!(r3.action, ReactionAction::Notify);
        // Runtime NOT called on escalation — we only notify.
        assert_eq!(runtime.sends().len(), 2);

        // Events across all three dispatches:
        // triggered(send), triggered(send), escalated + triggered(notify).
        let events = drain(&mut rx);
        assert_eq!(events.len(), 4, "got {events:?}");
        let escalated_count = events
            .iter()
            .filter(|e| matches!(e, OrchestratorEvent::ReactionEscalated { .. }))
            .count();
        assert_eq!(escalated_count, 1);
        // Final event must be the escalated-notify triggered.
        assert!(matches!(
            events.last().unwrap(),
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::Notify,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn escalate_after_attempts_escalates_independently_of_retries() {
        // No `retries` → infinite, but `escalate-after: 1` forces
        // escalation after the first attempt.
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.escalate_after = Some(EscalateAfter::Attempts(1));
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, _rx) = build(map);
        let session = fake_session("s1");

        // Attempt 1: normal send.
        let r1 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(!r1.escalated);
        assert_eq!(runtime.sends().len(), 1);

        // Attempt 2: escalated (attempts=2 > 1).
        let r2 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r2.escalated);
        assert_eq!(runtime.sends().len(), 1);
    }

    #[tokio::test]
    async fn escalate_after_duration_is_ignored_in_phase_d() {
        // Duration form is a no-op in Phase D. This test locks in that
        // contract so Phase E has a clear "before" baseline.
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.escalate_after = Some(EscalateAfter::Duration("10m".into()));
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, _rx) = build(map);
        let session = fake_session("s1");

        // Five attempts all still run the configured action — no escalation
        // because Duration escalate-after is not honoured yet.
        for _ in 0..5 {
            let r = engine
                .dispatch(&session, "ci-failed")
                .await
                .unwrap()
                .unwrap();
            assert!(!r.escalated);
        }
        assert_eq!(runtime.sends().len(), 5);
    }

    #[tokio::test]
    async fn clear_tracker_after_escalation_restores_real_action() {
        // Contract: once a session escalates and then *leaves* the
        // triggering status (lifecycle calls clear_tracker), re-entering
        // the same status must run the configured action again rather
        // than immediately re-escalating. This is the whole point of
        // clearing trackers on exit — without it, a session that
        // recovered and re-failed would see nothing but escalations.
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.retries = Some(1);
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, _rx) = build(map);
        let session = fake_session("s1");

        // 1st attempt: SendToAgent runs.
        let r1 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(!r1.escalated);
        // 2nd attempt: escalates (attempts=2 > retries=1).
        let r2 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r2.escalated);
        assert_eq!(runtime.sends().len(), 1);

        // Lifecycle clears the tracker on exit from CiFailed.
        engine.clear_tracker(&session.id, "ci-failed");

        // Re-entry: action runs again from a clean slate.
        let r3 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r3.success);
        assert!(!r3.escalated);
        assert_eq!(r3.action, ReactionAction::SendToAgent);
        assert_eq!(runtime.sends().len(), 2);
    }

    #[tokio::test]
    async fn clear_all_for_session_drops_every_reaction_tracker() {
        // Covers the leak-guard added for `LifecycleManager::terminate`:
        // terminating a session must drop all its trackers regardless
        // of which reaction keys it touched.
        let mut ci = ReactionConfig::new(ReactionAction::SendToAgent);
        ci.message = Some("fix".into());
        let mut cr = ReactionConfig::new(ReactionAction::SendToAgent);
        cr.message = Some("review".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), ci);
        map.insert("changes-requested".into(), cr);

        let (engine, _runtime, _rx) = build(map);
        let a = fake_session("a");
        let b = fake_session("b");

        // Seed three trackers across two sessions.
        engine.dispatch(&a, "ci-failed").await.unwrap();
        engine.dispatch(&a, "changes-requested").await.unwrap();
        engine.dispatch(&b, "ci-failed").await.unwrap();
        assert_eq!(engine.attempts(&a.id, "ci-failed"), 1);
        assert_eq!(engine.attempts(&a.id, "changes-requested"), 1);
        assert_eq!(engine.attempts(&b.id, "ci-failed"), 1);

        // Wipe session a only.
        engine.clear_all_for_session(&a.id);

        assert_eq!(engine.attempts(&a.id, "ci-failed"), 0);
        assert_eq!(engine.attempts(&a.id, "changes-requested"), 0);
        // Session b's trackers survive.
        assert_eq!(engine.attempts(&b.id, "ci-failed"), 1);
    }

    #[tokio::test]
    async fn auto_false_notify_fires_once_per_transition_and_does_not_escalate() {
        // Guards the `auto: false` + Notify edge case: a disabled
        // notify has no retry budget, so even `retries: Some(0)` cannot
        // trigger spurious escalations on it.
        let mut cfg = ReactionConfig::new(ReactionAction::Notify);
        cfg.auto = false;
        cfg.retries = Some(0); // would escalate if retry path ran
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), cfg);

        let (engine, _runtime, mut rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        // Two consecutive dispatches both return a normal (non-escalated)
        // Notify outcome — neither increments the tracker.
        for _ in 0..2 {
            let r = engine
                .dispatch(&session, "approved-and-green")
                .await
                .unwrap()
                .unwrap();
            assert!(r.success);
            assert!(!r.escalated);
            assert_eq!(r.action, ReactionAction::Notify);
        }
        assert_eq!(engine.attempts(&session.id, "approved-and-green"), 0);

        // No ReactionEscalated emitted on the channel.
        let events = drain(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::ReactionEscalated { .. })),
            "auto:false notify must not escalate, got {events:?}"
        );
    }

    #[tokio::test]
    async fn clear_tracker_resets_attempts_for_next_transition() {
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.retries = Some(1);
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, _runtime, _rx) = build(map);
        let session = fake_session("s1");

        // First attempt uses one retry.
        engine.dispatch(&session, "ci-failed").await.unwrap();
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 1);

        // CI goes green then red again → tracker cleared by lifecycle.
        engine.clear_tracker(&session.id, "ci-failed");
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 0);

        // Fresh attempt sees a full budget.
        let r = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r.success);
        assert!(!r.escalated);
    }

    #[tokio::test]
    async fn trackers_are_scoped_per_reaction_key() {
        // An attempt on `ci-failed` must not consume budget for
        // `changes-requested` on the same session.
        let mut ci = ReactionConfig::new(ReactionAction::SendToAgent);
        ci.message = Some("fix ci".into());
        let mut cr = ReactionConfig::new(ReactionAction::SendToAgent);
        cr.message = Some("address review".into());

        let mut map = HashMap::new();
        map.insert("ci-failed".into(), ci);
        map.insert("changes-requested".into(), cr);

        let (engine, _runtime, _rx) = build(map);
        let session = fake_session("s1");

        engine.dispatch(&session, "ci-failed").await.unwrap();
        engine.dispatch(&session, "ci-failed").await.unwrap();
        engine
            .dispatch(&session, "changes-requested")
            .await
            .unwrap();

        assert_eq!(engine.attempts(&session.id, "ci-failed"), 2);
        assert_eq!(engine.attempts(&session.id, "changes-requested"), 1);
    }

    #[tokio::test]
    async fn trackers_are_scoped_per_session_id() {
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("fix".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), cfg);

        let (engine, _runtime, _rx) = build(map);
        let a = fake_session("a");
        let b = fake_session("b");

        engine.dispatch(&a, "ci-failed").await.unwrap();
        engine.dispatch(&a, "ci-failed").await.unwrap();
        engine.dispatch(&b, "ci-failed").await.unwrap();

        assert_eq!(engine.attempts(&a.id, "ci-failed"), 2);
        assert_eq!(engine.attempts(&b.id, "ci-failed"), 1);
    }
}
