pub mod config;
pub mod error;
pub mod events;
pub mod lifecycle;
pub mod lockfile;
pub mod paths;
pub mod reaction_engine;
pub mod reactions;
pub mod restore;
pub mod scm;
pub mod session_manager;
pub mod traits;
pub mod types;

pub use config::AoConfig;
pub use error::{AoError, Result};
pub use events::{OrchestratorEvent, TerminationReason};
pub use lifecycle::{LifecycleHandle, LifecycleManager, DEFAULT_POLL_INTERVAL};
pub use lockfile::{is_process_alive, read_pidfile, LockError, PidFile};
pub use reaction_engine::{status_to_reaction_key, ReactionEngine};
pub use reactions::{
    EscalateAfter, EventPriority, ReactionAction, ReactionConfig, ReactionOutcome,
};
pub use restore::{restore_session, RestoreOutcome};
pub use scm::{
    CheckRun, CheckStatus, CiStatus, Issue, IssueState, MergeMethod, MergeReadiness, PrState,
    PullRequest, Review, ReviewComment, ReviewDecision, ReviewState,
};
pub use session_manager::SessionManager;
pub use traits::{Agent, Runtime, Scm, Tracker, Workspace};
pub use types::{
    now_ms, ActivityState, Project, Session, SessionId, SessionStatus, WorkspaceCreateConfig,
};
