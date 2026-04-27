//! Orchestrator spawn helper (TS `spawnOrchestrator` equivalent).
//!
//! Slice 2 of issue #165 — creates a new orchestrator session in its own
//! worktree (`orchestrator/<prefix>-orchestrator-N`) and launches the
//! configured agent with a rendered orchestrator system prompt.
//!
//! Kept deliberately plugin-generic: callers pass plugin instances as trait
//! objects so ao-core doesn't need to know about tmux, worktree,
//! claude-code, ...
//!
//! Parity notes:
//! - Identity reservation mirrors
//!   `reserveNextOrchestratorIdentity` in
//!   `packages/core/src/session-manager.ts` — smallest `N` not used by any
//!   active or archived session (limit 10k).
//! - Classification (`is_orchestrator_session`) mirrors
//!   `isOrchestratorSessionRecord` with the Rust port's simpler session
//!   shape (no `metadata.role` field yet, so the id pattern is the sole
//!   signal).
//! - Env vars follow the ao-ts orchestrator launch: `AO_CALLER_TYPE`,
//!   `AO_SESSION`, `AO_DATA_DIR`, `AO_PROJECT_ID`, `AO_CONFIG_PATH`,
//!   `AO_PORT`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::{
    config::{AgentConfig, AoConfig, DefaultsConfig, ProjectConfig},
    error::{AoError, Result},
    orchestrator_prompt::{generate_orchestrator_prompt, OrchestratorPromptConfig},
    session_manager::SessionManager,
    traits::{Agent, Runtime, Workspace},
    types::{now_ms, Session, SessionId, SessionStatus, WorkspaceCreateConfig},
};

fn inline_rules_file(mut cfg: AgentConfig, base: &std::path::Path) -> AgentConfig {
    let Some(path) = cfg.rules_file.as_deref() else {
        return cfg;
    };
    let full = if std::path::Path::new(path).is_absolute() {
        std::path::PathBuf::from(path)
    } else {
        base.join(path)
    };
    if let Ok(contents) = std::fs::read_to_string(&full) {
        cfg.rules = Some(contents);
        cfg.rules_file = None;
    }
    cfg
}

/// Inputs for `spawn_orchestrator`. Borrows everything so callers can
/// keep ownership of config/project structs across multiple spawns.
pub struct OrchestratorSpawnConfig<'a> {
    pub project_id: &'a str,
    pub project_config: &'a ProjectConfig,
    pub config: &'a AoConfig,
    /// Path to the loaded `ao-rs.yaml`, passed through as `AO_CONFIG_PATH`.
    pub config_path: Option<PathBuf>,
    /// Dashboard port — rendered into the orchestrator prompt and exported as `AO_PORT`.
    pub port: u16,
    pub agent_name: &'a str,
    pub runtime_name: &'a str,
    pub repo_path: PathBuf,
    pub default_branch: String,
    /// Skip sending the rendered orchestrator prompt after launch.
    pub no_prompt: bool,
}

/// Reserve the next unused `<prefix>-orchestrator-N` id for a project.
///
/// Scans `existing` (typically `list_for_project` ∪ `list_archived`) for
/// ids matching the orchestrator pattern and returns the smallest `N`
/// not already taken. Fails after 10k attempts — mirrors the TS cap.
pub fn reserve_orchestrator_identity(project_prefix: &str, existing: &[Session]) -> Result<String> {
    let orch_prefix = format!("{project_prefix}-orchestrator");
    let search = format!("{orch_prefix}-");
    let mut used: HashSet<u32> = HashSet::new();

    for s in existing {
        if let Some(rest) = s.id.0.strip_prefix(&search) {
            if let Ok(n) = rest.parse::<u32>() {
                used.insert(n);
            }
        }
    }

    for n in 1..=10_000u32 {
        if !used.contains(&n) {
            return Ok(format!("{orch_prefix}-{n}"));
        }
    }

    Err(AoError::Runtime(format!(
        "failed to reserve orchestrator id after 10000 attempts (prefix: {orch_prefix})"
    )))
}

/// Classify a session as an orchestrator based on its id.
///
/// The Rust session YAML doesn't yet carry a `role` field, so we rely
/// entirely on the id pattern — matches either a literal `-orchestrator`
/// suffix or the standard `<prefix>-orchestrator-<digits>` form.
pub fn is_orchestrator_session(s: &Session) -> bool {
    let id = &s.id.0;
    if id.ends_with("-orchestrator") {
        return true;
    }
    if let Some(pos) = id.rfind("-orchestrator-") {
        let suffix = &id[pos + "-orchestrator-".len()..];
        return !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit());
    }
    false
}

/// Spawn a new orchestrator session: reserve id, create worktree, launch
/// runtime, and deliver the rendered orchestrator prompt.
///
/// On error after the worktree is created, the worktree is destroyed
/// (best-effort) so a failed spawn doesn't leave dangling state on disk.
pub async fn spawn_orchestrator(
    cfg: OrchestratorSpawnConfig<'_>,
    sessions: &SessionManager,
    workspace: &dyn Workspace,
    agent: &dyn Agent,
    runtime: &dyn Runtime,
) -> Result<Session> {
    let project_prefix = cfg
        .project_config
        .session_prefix
        .as_deref()
        .unwrap_or(cfg.project_id);

    // Combine active + archived so a reused N can't collide with a historical
    // session yaml sitting in `.archive/`.
    let mut pool = sessions.list_for_project(cfg.project_id).await?;
    pool.extend(
        sessions
            .list_archived(cfg.project_id)
            .await
            .unwrap_or_default(),
    );
    let session_id_str = reserve_orchestrator_identity(project_prefix, &pool)?;
    let session_id = SessionId(session_id_str.clone());
    let branch = format!("orchestrator/{session_id_str}");

    let system_prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
        config: cfg.config,
        project_id: cfg.project_id,
        project: cfg.project_config,
        dashboard_port: cfg.port,
    })?;

    // Create the worktree. From this point on any failure must clean it up.
    let workspace_cfg = WorkspaceCreateConfig {
        project_id: cfg.project_id.to_string(),
        session_id: session_id_str.clone(),
        branch: branch.clone(),
        repo_path: cfg.repo_path.clone(),
        default_branch: cfg.default_branch.clone(),
        symlinks: cfg.project_config.symlinks.clone(),
        post_create: cfg.project_config.post_create.clone(),
    };
    let workspace_path = workspace.create(&workspace_cfg).await?;

    let spawn_result = async {
        let agent_config =
            resolve_orchestrator_agent_config(cfg.project_config, cfg.config.defaults.as_ref());
        let agent_config = agent_config.map(|c| inline_rules_file(c, &cfg.repo_path));

        let mut session = Session {
            id: session_id.clone(),
            project_id: cfg.project_id.to_string(),
            status: SessionStatus::Spawning,
            agent: cfg.agent_name.to_string(),
            agent_config,
            branch: branch.clone(),
            task: "orchestrator".to_string(),
            workspace_path: Some(workspace_path.clone()),
            runtime_handle: None,
            runtime: cfg.runtime_name.to_string(),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: Some(system_prompt.clone()),
            spawned_by: None,
            last_merge_conflict_dispatched: None,
            last_review_backlog_fingerprint: None,
            last_automated_review_fingerprint: None,
            last_automated_review_dispatch_hash: None,
        };
        sessions.save(&session).await?;

        let launch_command = agent.launch_command(&session);
        let mut env = agent.environment(&session);
        env.push(("AO_CALLER_TYPE".into(), "orchestrator".into()));
        env.push(("AO_SESSION".into(), session_id_str.clone()));
        env.push(("AO_SESSION_NAME".into(), session_id_str.clone()));
        env.push((
            "AO_DATA_DIR".into(),
            sessions.base_dir().to_string_lossy().into_owned(),
        ));
        env.push(("AO_PROJECT_ID".into(), cfg.project_id.to_string()));
        if let Some(cp) = cfg.config_path.as_ref() {
            env.push(("AO_CONFIG_PATH".into(), cp.to_string_lossy().into_owned()));
        }
        env.push(("AO_PORT".into(), cfg.port.to_string()));

        let handle = runtime
            .create(&session_id_str, &workspace_path, &launch_command, &env)
            .await?;
        session.runtime_handle = Some(handle.clone());
        session.status = SessionStatus::Working;
        sessions.save(&session).await?;

        // Orchestrator prompt delivery:
        // - Cursor receives its initial prompt via launch args (see agent-cursor),
        //   because Cursor can ignore post-launch input while showing its
        //   trust/startup UI.
        // - Other agents (e.g. claude-code) receive it via post-launch message.
        if !cfg.no_prompt && cfg.agent_name != "cursor" {
            tokio::time::sleep(Duration::from_millis(2500)).await;
            let prompt = match agent.system_prompt() {
                Some(rules) if !rules.trim().is_empty() => {
                    format!("{}\n\n---\n\n{}", rules.trim(), system_prompt)
                }
                _ => system_prompt.clone(),
            };
            runtime.send_message(&handle, &prompt).await?;
        }

        Ok::<Session, AoError>(session)
    }
    .await;

    match spawn_result {
        Ok(s) => Ok(s),
        Err(e) => {
            // Best-effort cleanup; swallow any secondary error so the user
            // sees the original failure cause.
            let _ = workspace.destroy(&workspace_path).await;
            let _ = sessions.delete(cfg.project_id, &session_id).await;
            Err(e)
        }
    }
}

/// Resolve `AgentConfig` for the orchestrator role by layering config
/// sources from lowest to highest priority.
///
/// Layers (bottom → top):
/// 1. `defaults.orchestrator.agent_config` — global role default
/// 2. `projects.<id>.agent_config` — shared project config
/// 3. `projects.<id>.orchestrator.agent_config` — role-specific override
///
/// Each higher layer's non-`None` fields overlay the previous. The
/// final `model` is then re-resolved using the orchestrator-aware chain
/// (mirrors ao-ts `resolveAgentSelection`, with an extra tier for
/// `defaults.orchestrator.*` so a single entry in `defaults` applies to
/// every project):
///
/// ```text
/// role.orchestratorModel  ?? role.model            (highest)
/// shared.orchestratorModel ?? shared.model
/// defaults.role.orchestratorModel ?? defaults.role.model  (lowest)
/// ```
///
/// Returns `None` when no layer sets an agent_config — keeps the old
/// behavior where `Session::agent_config` stays `None` and the agent
/// plugin falls back to its own defaults.
pub fn resolve_orchestrator_agent_config(
    project: &ProjectConfig,
    defaults: Option<&DefaultsConfig>,
) -> Option<AgentConfig> {
    let role_cfg = project
        .orchestrator
        .as_ref()
        .and_then(|r| r.agent_config.as_ref());
    let shared_cfg = project.agent_config.as_ref();
    let defaults_role_cfg = defaults
        .and_then(|d| d.orchestrator.as_ref())
        .and_then(|r| r.agent_config.as_ref());

    if role_cfg.is_none() && shared_cfg.is_none() && defaults_role_cfg.is_none() {
        return None;
    }

    let mut merged = defaults_role_cfg.cloned().unwrap_or_default();
    if let Some(sc) = shared_cfg {
        overlay_agent_config(&mut merged, sc);
    }
    if let Some(rc) = role_cfg {
        overlay_agent_config(&mut merged, rc);
    }

    let orchestrator_model = role_cfg
        .and_then(|c| c.orchestrator_model.clone())
        .or_else(|| role_cfg.and_then(|c| c.model.clone()))
        .or_else(|| shared_cfg.and_then(|c| c.orchestrator_model.clone()))
        .or_else(|| shared_cfg.and_then(|c| c.model.clone()))
        .or_else(|| defaults_role_cfg.and_then(|c| c.orchestrator_model.clone()))
        .or_else(|| defaults_role_cfg.and_then(|c| c.model.clone()));

    if orchestrator_model.is_some() {
        merged.model = orchestrator_model;
    }
    // Bake-down: the resolved model already reflects orchestrator_model,
    // so clear the field to avoid leaking confusing config into session YAML.
    merged.orchestrator_model = None;

    Some(merged)
}

/// Layer-overlay: copy every `Some(_)` field from `top` onto `base`.
/// `permissions` is non-optional (serde defaults it) so always overrides.
fn overlay_agent_config(base: &mut AgentConfig, top: &AgentConfig) {
    base.permissions = top.permissions.clone();
    if top.rules.is_some() {
        base.rules.clone_from(&top.rules);
    }
    if top.rules_file.is_some() {
        base.rules_file.clone_from(&top.rules_file);
    }
    if top.model.is_some() {
        base.model.clone_from(&top.model);
    }
    if top.orchestrator_model.is_some() {
        base.orchestrator_model.clone_from(&top.orchestrator_model);
    }
    if top.opencode_session_id.is_some() {
        base.opencode_session_id
            .clone_from(&top.opencode_session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{now_ms, SessionId, SessionStatus};

    fn session_with_id(id: &str, project: &str) -> Session {
        Session {
            id: SessionId(id.into()),
            project_id: project.into(),
            status: SessionStatus::Working,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "orchestrator/x".into(),
            task: "orchestrator".into(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: None,
            spawned_by: None,
            last_merge_conflict_dispatched: None,
            last_review_backlog_fingerprint: None,
            last_automated_review_fingerprint: None,
            last_automated_review_dispatch_hash: None,
        }
    }

    #[test]
    fn reserve_starts_at_one_when_no_existing() {
        let got = reserve_orchestrator_identity("app", &[]).unwrap();
        assert_eq!(got, "app-orchestrator-1");
    }

    #[test]
    fn reserve_skips_used_numbers_in_order() {
        let existing = vec![
            session_with_id("app-orchestrator-1", "app"),
            session_with_id("app-orchestrator-3", "app"),
        ];
        let got = reserve_orchestrator_identity("app", &existing).unwrap();
        assert_eq!(got, "app-orchestrator-2");

        let existing_full = vec![
            session_with_id("app-orchestrator-1", "app"),
            session_with_id("app-orchestrator-2", "app"),
        ];
        let got = reserve_orchestrator_identity("app", &existing_full).unwrap();
        assert_eq!(got, "app-orchestrator-3");
    }

    #[test]
    fn reserve_ignores_other_projects_and_worker_ids() {
        let existing = vec![
            session_with_id("app-1", "app"), // worker with uuid-style id
            session_with_id("other-orchestrator-1", "other"), // other project
            session_with_id("app-orchestrator-abc", "app"), // non-numeric
        ];
        let got = reserve_orchestrator_identity("app", &existing).unwrap();
        assert_eq!(got, "app-orchestrator-1");
    }

    #[test]
    fn is_orchestrator_session_matches_numbered_pattern() {
        assert!(is_orchestrator_session(&session_with_id(
            "app-orchestrator-1",
            "app"
        )));
        assert!(is_orchestrator_session(&session_with_id(
            "my-project-orchestrator-42",
            "my-project"
        )));
    }

    #[test]
    fn is_orchestrator_session_matches_suffix_only() {
        assert!(is_orchestrator_session(&session_with_id(
            "app-orchestrator",
            "app"
        )));
    }

    // ---- resolve_orchestrator_agent_config: layered model fallback ----

    fn project_with(agent_config: Option<AgentConfig>) -> ProjectConfig {
        use std::collections::HashMap;
        ProjectConfig {
            name: None,
            repo: "o/r".into(),
            path: "/tmp/p".into(),
            default_branch: "main".into(),
            session_prefix: None,
            branch_namespace: None,
            runtime: None,
            agent: None,
            workspace: None,
            tracker: None,
            scm: None,
            symlinks: vec![],
            post_create: vec![],
            agent_config,
            orchestrator: None,
            worker: None,
            reactions: HashMap::new(),
            agent_rules: None,
            agent_rules_file: None,
            orchestrator_rules: None,
            orchestrator_session_strategy: None,
            opencode_issue_session_strategy: None,
        }
    }

    fn defaults_with_orchestrator_config(agent_config: Option<AgentConfig>) -> DefaultsConfig {
        DefaultsConfig {
            orchestrator: Some(crate::config::RoleAgentConfig {
                agent: None,
                agent_config,
            }),
            ..DefaultsConfig::default()
        }
    }

    #[test]
    fn returns_none_when_no_layer_is_set() {
        let project = project_with(None);
        assert!(resolve_orchestrator_agent_config(&project, None).is_none());
    }

    #[test]
    fn orchestrator_model_overrides_shared_model() {
        let project = project_with(Some(AgentConfig {
            model: Some("sonnet".into()),
            orchestrator_model: Some("opus".into()),
            ..AgentConfig::default()
        }));
        let resolved = resolve_orchestrator_agent_config(&project, None).unwrap();
        assert_eq!(resolved.model.as_deref(), Some("opus"));
        // orchestrator_model is baked into model and cleared from output
        assert!(resolved.orchestrator_model.is_none());
    }

    #[test]
    fn defaults_role_model_applies_when_project_has_no_config() {
        let project = project_with(None);
        let defaults = defaults_with_orchestrator_config(Some(AgentConfig {
            model: Some("opus".into()),
            ..AgentConfig::default()
        }));
        let resolved = resolve_orchestrator_agent_config(&project, Some(&defaults)).unwrap();
        assert_eq!(resolved.model.as_deref(), Some("opus"));
    }

    #[test]
    fn project_shared_model_overrides_defaults_role_model() {
        let project = project_with(Some(AgentConfig {
            model: Some("sonnet".into()),
            ..AgentConfig::default()
        }));
        let defaults = defaults_with_orchestrator_config(Some(AgentConfig {
            model: Some("opus".into()),
            ..AgentConfig::default()
        }));
        let resolved = resolve_orchestrator_agent_config(&project, Some(&defaults)).unwrap();
        assert_eq!(resolved.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn project_role_override_beats_shared_and_defaults() {
        let mut project = project_with(Some(AgentConfig {
            model: Some("sonnet".into()),
            ..AgentConfig::default()
        }));
        project.orchestrator = Some(crate::config::RoleAgentConfig {
            agent: None,
            agent_config: Some(AgentConfig {
                model: Some("haiku".into()),
                ..AgentConfig::default()
            }),
        });
        let defaults = defaults_with_orchestrator_config(Some(AgentConfig {
            model: Some("opus".into()),
            ..AgentConfig::default()
        }));
        let resolved = resolve_orchestrator_agent_config(&project, Some(&defaults)).unwrap();
        assert_eq!(resolved.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn role_orchestrator_model_is_highest_priority() {
        let mut project = project_with(Some(AgentConfig {
            model: Some("sonnet".into()),
            orchestrator_model: Some("shared-orch".into()),
            ..AgentConfig::default()
        }));
        project.orchestrator = Some(crate::config::RoleAgentConfig {
            agent: None,
            agent_config: Some(AgentConfig {
                orchestrator_model: Some("role-orch".into()),
                ..AgentConfig::default()
            }),
        });
        let resolved = resolve_orchestrator_agent_config(&project, None).unwrap();
        assert_eq!(resolved.model.as_deref(), Some("role-orch"));
    }

    #[test]
    fn is_orchestrator_session_rejects_workers_and_unrelated() {
        assert!(!is_orchestrator_session(&session_with_id("app-1", "app")));
        assert!(!is_orchestrator_session(&session_with_id(
            "deadbeef-aaaa-bbbb",
            "app"
        )));
        assert!(!is_orchestrator_session(&session_with_id(
            "app-orchestrator-abc",
            "app"
        )));
        assert!(!is_orchestrator_session(&session_with_id(
            "app-orchestrator-",
            "app"
        )));
    }
}
