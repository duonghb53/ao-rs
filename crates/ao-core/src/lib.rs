pub mod error;
pub mod events;
pub mod lifecycle;
pub mod lockfile;
pub mod paths;
pub mod restore;
pub mod session_manager;
pub mod traits;
pub mod types;

pub use error::{AoError, Result};
pub use events::{OrchestratorEvent, TerminationReason};
pub use lifecycle::{LifecycleHandle, LifecycleManager, DEFAULT_POLL_INTERVAL};
pub use lockfile::{is_process_alive, read_pidfile, LockError, PidFile};
pub use restore::{restore_session, RestoreOutcome};
pub use session_manager::SessionManager;
pub use traits::{Agent, Runtime, Workspace};
pub use types::{
    now_ms, ActivityState, Project, Session, SessionId, SessionStatus, WorkspaceCreateConfig,
};
