//! Events broadcast by the `LifecycleManager` to anyone watching the
//! session fleet — the CLI's `ao-rs watch`, future reaction engines,
//! future notifier plugins, an eventual SSE API.
//!
//! **All variants must be `Clone`** because they ride on
//! `tokio::sync::broadcast`, which fans a single send out to every
//! subscriber by cloning.
//!
//! We keep the event surface intentionally small for Phase C:
//! - `Spawned` when a brand-new session is observed for the first time
//! - `SessionRestored` when a session that already existed on disk is
//!   observed on the loop's first tick — separate from `Spawned` so
//!   `watch` and dashboard consumers don't mislabel pre-existing
//!   sessions as new
//! - `StatusChanged` when lifecycle transitions a session between
//!   `SessionStatus` variants
//! - `ActivityChanged` when the polled `ActivityState` changes
//! - `Terminated` when the runtime is no longer alive — separate from
//!   `StatusChanged` because subscribers often want to react to *dead*
//!   specifically (e.g. start cleanup)
//! - `TickError` surfaces polling-loop errors without killing the loop

use crate::{
    dashboard_payload::DashboardPr,
    reactions::ReactionAction,
    types::{ActivityState, SessionId, SessionStatus},
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct UiNotification {
    pub id: SessionId,
    pub reaction_key: String,
    pub action: ReactionAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrchestratorEvent {
    /// A session was created after the lifecycle loop was already running.
    /// The loop decides "new" by comparing `session.created_at` against its
    /// own startup timestamp — a session observed on the first tick whose
    /// `created_at` predates startup is reported via `SessionRestored`
    /// instead, so `watch` output distinguishes "brand new spawn" from
    /// "restored from disk".
    Spawned { id: SessionId, project_id: String },

    /// A session that already existed on disk was observed by the
    /// lifecycle loop on its first tick after startup. Emitted at most
    /// once per session and only during the first tick — subsequent
    /// appearances use `Spawned`. Consumers use this to suppress the
    /// "N sessions just spawned" flood on reconnect.
    SessionRestored {
        id: SessionId,
        project_id: String,
        /// On-disk status at the moment of observation. Useful for UI
        /// filtering (e.g. skip terminal sessions) without an extra
        /// snapshot round-trip.
        status: SessionStatus,
    },

    /// Lifecycle-driven status transition. `from == to` is never emitted.
    StatusChanged {
        id: SessionId,
        from: SessionStatus,
        to: SessionStatus,
    },

    /// Polled activity changed. `prev` is `None` on the first successful poll.
    ActivityChanged {
        id: SessionId,
        prev: Option<ActivityState>,
        next: ActivityState,
    },

    /// Runtime process is gone. Emitted exactly once per session.
    Terminated {
        id: SessionId,
        reason: TerminationReason,
    },

    /// Polling-loop error for one session. The loop itself keeps running.
    TickError { id: SessionId, message: String },

    /// A configured reaction successfully ran its action. The engine emits
    /// this on every successful dispatch — subscribers use it to surface
    /// "ao-rs just fired X" in the CLI and for assertions in tests.
    ///
    /// `action` is the action the engine *actually* took, which may differ
    /// from the configured action if the engine escalated mid-flight
    /// (`SendToAgent` → `Notify`). Pair with `ReactionEscalated` to tell
    /// first-time successes apart from escalations.
    ReactionTriggered {
        id: SessionId,
        /// Reaction key from config (e.g. `"ci-failed"`).
        reaction_key: String,
        /// Action the engine actually executed this attempt.
        action: ReactionAction,
    },

    /// The retry budget for a reaction was exhausted and the engine fell
    /// back to `Notify`. Emitted *in addition to* the `ReactionTriggered`
    /// that represents the escalated notify — so subscribers that only
    /// care about "something was escalated" can filter on this event
    /// alone without having to join on attempts counts.
    ReactionEscalated {
        id: SessionId,
        reaction_key: String,
        /// How many attempts had been made when escalation was decided.
        /// The value is the attempt count *that triggered* escalation,
        /// not `retries + 1`, so a user reading logs sees exactly how
        /// many times the agent was poked before the notify fell through.
        attempts: u32,
    },

    /// UI-friendly notification event (dashboard/desktop toasts).
    ///
    /// Emitted in addition to `ReactionTriggered` for reactions that should
    /// surface to users in real time.
    UiNotification { notification: UiNotification },

    /// PR enrichment for a session changed (CI status, review decision,
    /// mergeability, check runs, diff size, …). Emitted by the lifecycle
    /// loop after each batch enrichment when the new `BatchedPrEnrichment`
    /// differs from the previous tick's value. `pr: None` means the PR
    /// disappeared (closed/branch-deleted) and the dashboard should clear
    /// its cached enrichment for this session.
    ///
    /// Lets the dashboard UI subscribe to PR-state deltas instead of
    /// polling `/api/sessions?pr=true` on a timer — see Layer 3 of the
    /// rate-limit fix plan.
    PrEnrichmentChanged {
        id: SessionId,
        pr: Option<DashboardPr>,
        attention_level: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    /// `Runtime::is_alive` returned false.
    RuntimeGone,
    /// The agent plugin reported `ActivityState::Exited`.
    AgentExited,
    /// Session had no runtime_handle to probe (e.g. crashed before create).
    NoHandle,
    /// Session's PR was merged; auto-terminate on merge fired (issue #220).
    PrMerged,
}

impl TerminationReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeGone => "runtime_gone",
            Self::AgentExited => "agent_exited",
            Self::NoHandle => "no_handle",
            Self::PrMerged => "pr_merged",
        }
    }
}

impl std::fmt::Display for TerminationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    //! Serde tag checks — the wire form is public (SSE / logs), so a rename
    //! is a breaking change. These tests pin every variant's `type` tag.

    use super::*;
    use serde_json::{json, Value};

    fn tag_of(ev: &OrchestratorEvent) -> String {
        let v: Value = serde_json::to_value(ev).unwrap();
        v.get("type")
            .and_then(Value::as_str)
            .expect("event missing `type` tag")
            .to_string()
    }

    fn sid(s: &str) -> SessionId {
        SessionId(s.into())
    }

    #[test]
    fn every_variant_has_expected_tag() {
        let cases: &[(&str, OrchestratorEvent)] = &[
            (
                "spawned",
                OrchestratorEvent::Spawned {
                    id: sid("s1"),
                    project_id: "demo".into(),
                },
            ),
            (
                "session_restored",
                OrchestratorEvent::SessionRestored {
                    id: sid("s1"),
                    project_id: "demo".into(),
                    status: SessionStatus::Spawning,
                },
            ),
            (
                "status_changed",
                OrchestratorEvent::StatusChanged {
                    id: sid("s1"),
                    from: SessionStatus::Spawning,
                    to: SessionStatus::Working,
                },
            ),
            (
                "activity_changed",
                OrchestratorEvent::ActivityChanged {
                    id: sid("s1"),
                    prev: None,
                    next: ActivityState::Ready,
                },
            ),
            (
                "terminated",
                OrchestratorEvent::Terminated {
                    id: sid("s1"),
                    reason: TerminationReason::AgentExited,
                },
            ),
            (
                "tick_error",
                OrchestratorEvent::TickError {
                    id: sid("s1"),
                    message: "boom".into(),
                },
            ),
            (
                "reaction_triggered",
                OrchestratorEvent::ReactionTriggered {
                    id: sid("s1"),
                    reaction_key: "ci-failed".into(),
                    action: ReactionAction::Notify,
                },
            ),
            (
                "reaction_escalated",
                OrchestratorEvent::ReactionEscalated {
                    id: sid("s1"),
                    reaction_key: "ci-failed".into(),
                    attempts: 3,
                },
            ),
            (
                "ui_notification",
                OrchestratorEvent::UiNotification {
                    notification: UiNotification {
                        id: sid("s1"),
                        reaction_key: "ci-failed".into(),
                        action: ReactionAction::Notify,
                        message: None,
                        priority: None,
                    },
                },
            ),
            (
                "pr_enrichment_changed",
                OrchestratorEvent::PrEnrichmentChanged {
                    id: sid("s1"),
                    pr: None,
                    attention_level: "review".into(),
                },
            ),
        ];

        for (expected, ev) in cases {
            assert_eq!(&tag_of(ev), expected, "wrong tag for {ev:?}");
        }
    }

    #[test]
    fn session_restored_carries_status_field() {
        let ev = OrchestratorEvent::SessionRestored {
            id: sid("s1"),
            project_id: "demo".into(),
            status: SessionStatus::Working,
        };
        let v: Value = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "session_restored",
                "id": "s1",
                "project_id": "demo",
                "status": "working",
            })
        );
    }

    #[test]
    fn termination_reason_wire_form_is_snake_case() {
        assert_eq!(
            serde_json::to_value(TerminationReason::RuntimeGone).unwrap(),
            json!("runtime_gone")
        );
        assert_eq!(
            serde_json::to_value(TerminationReason::AgentExited).unwrap(),
            json!("agent_exited")
        );
        assert_eq!(
            serde_json::to_value(TerminationReason::NoHandle).unwrap(),
            json!("no_handle")
        );
        assert_eq!(
            serde_json::to_value(TerminationReason::PrMerged).unwrap(),
            json!("pr_merged")
        );
    }
}
