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
    config::{AoConfig, ProjectConfig},
    error::{AoError, Result},
    orchestrator_prompt::{generate_orchestrator_prompt, OrchestratorPromptConfig},
    session_manager::SessionManager,
    traits::{Agent, Runtime, Workspace},
    types::{now_ms, Session, SessionId, SessionStatus, WorkspaceCreateConfig},
};

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
    });

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
        let mut session = Session {
            id: session_id.clone(),
            project_id: cfg.project_id.to_string(),
            status: SessionStatus::Spawning,
            agent: cfg.agent_name.to_string(),
            agent_config: cfg.project_config.agent_config.clone(),
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

        if !cfg.no_prompt {
            tokio::time::sleep(Duration::from_millis(2500)).await;
            runtime.send_message(&handle, &system_prompt).await?;
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
