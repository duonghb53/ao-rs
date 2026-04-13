pub mod config;
pub mod cost_ledger;
pub mod error;
pub mod events;
pub mod lifecycle;
pub mod lockfile;
pub mod notifier;
pub mod paths;
pub mod prompt_builder;
pub mod reaction_engine;
pub mod reactions;
pub mod restore;
pub mod scm;
pub mod scm_transitions;
pub mod session_manager;
pub mod traits;
pub mod types;

pub use config::{
    default_agent_rules, default_reactions, default_routing, detect_git_repo, generate_config,
    install_skills, AgentConfig, AoConfig, DefaultsConfig, ProjectConfig,
};
pub use error::{AoError, Result};
pub use events::{OrchestratorEvent, TerminationReason};
pub use lifecycle::{LifecycleHandle, LifecycleManager, DEFAULT_POLL_INTERVAL};
pub use lockfile::{is_process_alive, read_pidfile, LockError, PidFile};
pub use notifier::{
    NotificationPayload, NotificationRouting, Notifier, NotifierError, NotifierRegistry,
};
pub use prompt_builder::build_prompt;
pub use reaction_engine::{status_to_reaction_key, ReactionEngine};
pub use reactions::{
    EscalateAfter, EventPriority, ReactionAction, ReactionConfig, ReactionOutcome,
};
pub use restore::{restore_session, RestoreOutcome};
pub use scm::{
    CheckRun, CheckStatus, CiStatus, Issue, IssueState, MergeMethod, MergeReadiness, PrState,
    PullRequest, Review, ReviewComment, ReviewDecision, ReviewState,
};
pub use scm_transitions::{derive_scm_status, ScmObservation};
pub use session_manager::SessionManager;
pub use traits::{Agent, Runtime, Scm, Tracker, Workspace};
pub use types::{
    now_ms, ActivityState, CostEstimate, Project, Session, SessionId, SessionStatus,
    WorkspaceCreateConfig,
};
