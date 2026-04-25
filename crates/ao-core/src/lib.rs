pub mod activity_log;
pub mod config;
pub mod cost_ledger;
pub mod cost_log;
pub mod dashboard_payload;
pub mod error;
pub mod events;
pub mod gh;
pub mod lifecycle;
pub mod lockfile;
pub mod notifier;
pub mod opencode_session_id;
pub mod orchestrator_prompt;
pub mod orchestrator_spawn;
pub mod parity_config_validation;
pub mod parity_feedback_tools;
pub mod parity_metadata;
pub mod parity_notifier_resolution;
pub mod parity_observability;
pub mod parity_plugin_registry;
pub mod parity_session_strategy;
pub mod parity_utils;
pub mod paths;
pub mod prompt_builder;
pub mod rate_limit;
pub mod reaction_engine;
pub mod reactions;
pub mod restore;
pub mod scm;
pub mod scm_transitions;
pub mod session_manager;
pub mod shell;
pub mod traits;
pub mod types;
pub mod workspace_hooks;

pub use config::{
    default_agent_rules, default_orchestrator_rules, default_reactions, default_routing,
    detect_git_repo, generate_config, install_skills, AgentConfig, AoConfig, ConfigWarning,
    DefaultsConfig, LoadedConfig, PermissionsMode, ProjectConfig, RoleAgentConfig,
    ScmWebhookConfig,
};
pub use dashboard_payload::{attention_level, BatchedPrEnrichment, DashboardPr, DashboardSession};
pub use error::{AoError, Result};
pub use events::{OrchestratorEvent, TerminationReason};
pub use lifecycle::{LifecycleHandle, LifecycleManager, DEFAULT_POLL_INTERVAL};
pub use lockfile::{is_process_alive, read_pidfile, LockError, PidFile};
pub use notifier::{
    NotificationPayload, NotificationRouting, Notifier, NotifierError, NotifierRegistry,
};
pub use orchestrator_prompt::{generate_orchestrator_prompt, OrchestratorPromptConfig};
pub use orchestrator_spawn::{
    is_orchestrator_session, reserve_orchestrator_identity, resolve_orchestrator_agent_config,
    spawn_orchestrator, OrchestratorSpawnConfig,
};
pub use parity_session_strategy::{OpencodeIssueSessionStrategy, OrchestratorSessionStrategy};
pub use prompt_builder::build_prompt;
pub use reaction_engine::{status_to_reaction_key, ReactionEngine};
pub use reactions::{
    default_priority_for_reaction_key, EscalateAfter, EventPriority, ReactionAction,
    ReactionConfig, ReactionOutcome,
};
pub use restore::{restore_session, RestoreOutcome};
pub use scm::{
    AutomatedComment, AutomatedCommentSeverity, CheckRun, CheckStatus, CiStatus, CreateIssueInput,
    Issue, IssueFilters, IssueState, IssueUpdate, MergeMethod, MergeReadiness, PrState, PrSummary,
    PullRequest, Review, ReviewComment, ReviewDecision, ReviewState, ScmWebhookEvent,
    ScmWebhookEventKind, ScmWebhookRepository, ScmWebhookRequest, ScmWebhookVerificationResult,
};
pub use scm_transitions::{derive_scm_status, ScmObservation};
pub use session_manager::SessionManager;
pub use traits::{Agent, Runtime, Scm, Tracker, Workspace};
pub use types::{
    now_ms, ActivityState, CostEstimate, Project, Session, SessionId, SessionStatus,
    WorkspaceCreateConfig,
};
