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

use crate::types::{ActivityState, SessionId, SessionStatus};

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
