//! `ao-rs orchestrator` subcommand (Slice 3 of issue #165).
//!
//! Thin CLI wrapper over `ao_core::spawn_orchestrator`: resolves repo +
//! project config, selects the orchestrator role's agent/runtime, and
//! hands everything to the core helper.
//!
//! Spawns and/or lists long-lived orchestrator sessions. A worker session
//! is created via `ao-rs spawn`; this command is distinct because
//! orchestrators are singletons per invocation (always a new `-N`), never
//! own a PR, and get a rendered orchestrator system prompt instead of
//! issue/task context.

use std::path::PathBuf;

use ao_core::{
    is_orchestrator_session, resolve_orchestrator_agent_config, spawn_orchestrator, AoConfig,
    LoadedConfig, OrchestratorSpawnConfig, SessionManager,
};
use ao_plugin_workspace_worktree::WorktreeWorkspace;

use crate::cli::agent_config::resolve_agent_config;
use crate::cli::plugins::{select_agent, select_runtime};
use crate::cli::printing::print_config_warnings;
use crate::cli::project::{resolve_project_id, resolve_repo_root};

#[allow(clippy::too_many_arguments)]
pub async fn spawn(
    repo: Option<PathBuf>,
    default_branch: String,
    project: Option<String>,
    port: u16,
    agent_override: Option<String>,
    runtime_override: Option<String>,
    no_prompt: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo_path = resolve_repo_root(repo)?;
    if !repo_path.join(".git").exists() {
        return Err(format!("not a git repo: {}", repo_path.display()).into());
    }

    let config_path = AoConfig::path_in(&repo_path);
    let LoadedConfig {
        config: ao_config,
        warnings,
    } = AoConfig::load_from_or_default_with_warnings(&config_path)
        .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;
    print_config_warnings(&config_path, &warnings);

    let project_id = resolve_project_id(&repo_path, &ao_config, project);
    let project_config = ao_config
        .projects
        .get(&project_id)
        .ok_or_else(|| format!("project '{project_id}' is not configured in ao-rs.yaml"))?
        .clone();

    // Agent resolution for orchestrator role: CLI --agent →
    // `projects.*.orchestrator.agent` → `projects.*.agent` →
    // `defaults.orchestrator.agent` → `defaults.agent` → claude-code.
    let agent_name = agent_override
        .or_else(|| {
            project_config
                .orchestrator
                .as_ref()
                .and_then(|r| r.agent.clone())
                .or_else(|| project_config.agent.clone())
        })
        .or_else(|| {
            ao_config
                .defaults
                .as_ref()
                .and_then(|d| d.orchestrator.as_ref().and_then(|r| r.agent.clone()))
        })
        .or_else(|| ao_config.defaults.as_ref().map(|d| d.agent.clone()))
        .unwrap_or_else(|| "claude-code".to_string());

    let runtime_name = runtime_override
        .or_else(|| project_config.runtime.clone())
        .or_else(|| ao_config.defaults.as_ref().map(|d| d.runtime.clone()))
        .unwrap_or_else(|| "tmux".to_string());

    // Resolve the orchestrator's effective agent_config (role override →
    // shared project → defaults.orchestrator), then inline any rules_file
    // so the agent plugin sees a fully self-contained config.
    let resolved = resolve_orchestrator_agent_config(&project_config, ao_config.defaults.as_ref());
    let agent_config = resolve_agent_config(resolved.as_ref(), &repo_path);
    let agent = select_agent(&agent_name, agent_config.as_ref());
    let runtime = select_runtime(&runtime_name);
    let workspace = WorktreeWorkspace::new();
    let sessions = SessionManager::with_default();

    println!("→ project:   {project_id}");
    println!("→ agent:     {agent_name}");
    println!("→ runtime:   {runtime_name}");
    println!("→ port:      {port}");
    println!();

    let session = spawn_orchestrator(
        OrchestratorSpawnConfig {
            project_id: &project_id,
            project_config: &project_config,
            config: &ao_config,
            config_path: Some(config_path.clone()),
            port,
            agent_name: &agent_name,
            runtime_name: &runtime_name,
            repo_path,
            default_branch,
            no_prompt,
        },
        &sessions,
        &workspace,
        agent.as_ref(),
        runtime.as_ref(),
    )
    .await?;

    let handle = session.runtime_handle.as_deref().unwrap_or(&session.id.0);
    println!("───────────────────────────────────────────────");
    println!("  ✓ orchestrator spawned & persisted");
    println!();
    println!("  session:  {}", session.id.0);
    println!("  branch:   {}", session.branch);
    if let Some(ref ws) = session.workspace_path {
        println!("  worktree: {}", ws.display());
    }
    println!("  attach:   tmux attach -t {handle}");
    println!("  send:     ao-rs send {} <message>", session.id.0);
    println!("───────────────────────────────────────────────");

    Ok(())
}

pub async fn list(project_filter: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let all = match project_filter.as_deref() {
        Some(id) => sessions.list_for_project(id).await?,
        None => sessions.list().await?,
    };
    let orchestrators: Vec<_> = all.into_iter().filter(is_orchestrator_session).collect();

    if orchestrators.is_empty() {
        println!("no orchestrator sessions found");
        return Ok(());
    }

    println!(
        "{:<32} {:<16} {:<12} BRANCH",
        "SESSION", "PROJECT", "STATUS"
    );
    for s in orchestrators {
        println!(
            "{:<32} {:<16} {:<12} {}",
            s.id.0,
            s.project_id,
            s.status.as_str(),
            s.branch
        );
    }
    Ok(())
}
