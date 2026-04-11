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
//! - `StatusChanged` when lifecycle transitions a session between
//!   `SessionStatus` variants
//! - `ActivityChanged` when the polled `ActivityState` changes
//! - `Terminated` when the runtime is no longer alive — separate from
//!   `StatusChanged` because subscribers often want to react to *dead*
//!   specifically (e.g. start cleanup)
//! - `TickError` surfaces polling-loop errors without killing the loop

use crate::{
    reactions::ReactionAction,
    types::{ActivityState, SessionId, SessionStatus},
};

#[derive(Debug, Clone)]
pub enum OrchestratorEvent {
    /// A session was seen by the lifecycle loop for the first time.
    /// (Emitted on the tick where the loop first observes it on disk.)
    Spawned { id: SessionId, project_id: String },

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationReason {
    /// `Runtime::is_alive` returned false.
    RuntimeGone,
    /// The agent plugin reported `ActivityState::Exited`.
    AgentExited,
    /// Session had no runtime_handle to probe (e.g. crashed before create).
    NoHandle,
}

impl TerminationReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeGone => "runtime_gone",
            Self::AgentExited => "agent_exited",
            Self::NoHandle => "no_handle",
        }
    }
}

impl std::fmt::Display for TerminationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
