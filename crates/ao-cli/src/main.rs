//! `ao-rs` — Slice 0 CLI.
//!
//! One subcommand: `spawn`. Wires together the three plugins built so far:
//!     workspace-worktree → agent-claude-code → runtime-tmux
//! No YAML config, no daemon, no persistence. Slice 1 will add those.

use ao_core::{
    now_ms, Agent, Runtime, Session, SessionId, SessionManager, SessionStatus, Workspace,
    WorkspaceCreateConfig,
};
use ao_plugin_agent_claude_code::ClaudeCodeAgent;
use ao_plugin_runtime_tmux::TmuxRuntime;
use ao_plugin_workspace_worktree::WorktreeWorkspace;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "ao-rs",
    about = "Rust port of agent-orchestrator (learning project — Slice 0)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Spawn a new agent session in an isolated git worktree.
    Spawn {
        /// The task description; sent to the agent as its first prompt.
        #[arg(short, long)]
        task: String,

        /// Path to the git repo. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Default branch of the repo, used as the worktree base.
        #[arg(long, default_value = "main")]
        default_branch: String,

        /// Project id; namespaces worktrees under `~/.worktrees/<project>/`.
        #[arg(long, default_value = "demo")]
        project: String,

        /// Skip sending the initial prompt (useful when `claude` isn't installed).
        #[arg(long)]
        no_prompt: bool,
    },

    /// List all known sessions, newest first.
    Status {
        /// Filter to a single project id.
        #[arg(long)]
        project: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Spawn {
            task,
            repo,
            default_branch,
            project,
            no_prompt,
        } => spawn(task, repo, default_branch, project, no_prompt).await,
        Command::Status { project } => status(project).await,
    }
}

async fn spawn(
    task: String,
    repo: Option<PathBuf>,
    default_branch: String,
    project: String,
    no_prompt: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // ---- 1. Resolve repo path ----
    let repo_path = match repo {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    if !repo_path.join(".git").exists() {
        return Err(format!("not a git repo: {}", repo_path.display()).into());
    }

    // ---- 2. Allocate ids ----
    let session_id = SessionId::new();
    // Short id is what tmux + worktree dirs see — uuid is too long for a tmux name.
    let short_id: String = session_id.0.chars().take(8).collect();
    let branch = format!("ao-{short_id}");

    println!("→ project:   {project}");
    println!("→ repo:      {}", repo_path.display());
    println!("→ session:   {session_id}");
    println!("→ short id:  {short_id}");
    println!("→ branch:    {branch}");
    println!();

    // ---- 3. Workspace: git worktree add ----
    let workspace = WorktreeWorkspace::new();
    let workspace_cfg = WorkspaceCreateConfig {
        project_id: project.clone(),
        session_id: short_id.clone(),
        branch: branch.clone(),
        repo_path: repo_path.clone(),
        default_branch,
    };

    println!("→ creating worktree...");
    let workspace_path = workspace.create(&workspace_cfg).await?;
    println!("  worktree:  {}", workspace_path.display());

    // Build the Session and persist it. Slice 1 Phase A: disk-backed.
    let mut session = Session {
        id: session_id.clone(),
        project_id: project.clone(),
        status: SessionStatus::Spawning,
        branch: branch.clone(),
        task,
        workspace_path: Some(workspace_path.clone()),
        runtime_handle: None,
        activity: None,
        created_at: now_ms(),
    };

    let manager = SessionManager::with_default();
    manager.save(&session).await?;

    // ---- 4. Agent: get launch command + env ----
    let agent = ClaudeCodeAgent::new();
    let launch_command = agent.launch_command(&session);
    let env = agent.environment(&session);
    let initial_prompt = agent.initial_prompt(&session);

    // ---- 5. Runtime: spawn tmux session running the agent ----
    let runtime = TmuxRuntime::new();
    println!("→ spawning runtime: `{launch_command}` in tmux");
    let handle = runtime
        .create(&short_id, &workspace_path, &launch_command, &env)
        .await?;

    // Persist the runtime handle + transition status — so `ao-rs status` shows
    // the spawned session as Working, not Spawning.
    session.runtime_handle = Some(handle.clone());
    session.status = SessionStatus::Working;
    manager.save(&session).await?;

    // ---- 6. Deliver initial prompt (post-launch for claude-code) ----
    if no_prompt {
        println!("→ skipping initial prompt (--no-prompt)");
    } else {
        // claude takes a moment to actually become interactive.
        // Without this delay, send-keys can land in a terminal that hasn't
        // finished drawing claude's TUI yet.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        println!("→ sending initial prompt: {initial_prompt:?}");
        runtime.send_message(&handle, &initial_prompt).await?;
    }

    println!();
    println!("───────────────────────────────────────────────");
    println!("  ✓ session spawned & persisted");
    println!();
    println!("  attach:  tmux attach -t {handle}");
    println!("  kill:    tmux kill-session -t {handle}");
    println!("  status:  ao-rs status");
    println!("  cleanup: cd {} && git worktree remove --force {}",
        repo_path.display(),
        workspace_path.display(),
    );
    println!("───────────────────────────────────────────────");

    Ok(())
}

async fn status(project_filter: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let manager = SessionManager::with_default();
    let sessions = match &project_filter {
        Some(p) => manager.list_for_project(p).await?,
        None => manager.list().await?,
    };

    if sessions.is_empty() {
        match project_filter {
            Some(p) => println!("no sessions in project '{p}'"),
            None => println!("no sessions"),
        }
        return Ok(());
    }

    // Columns wide enough for the longest status (`changes_requested` = 17
    // chars) and the longest activity (`waiting_input` = 13 chars). Trying
    // to autosize is not worth it for a tool that prints ~10 rows max.
    println!(
        "{:<10} {:<14} {:<18} {:<14} {:<18} TASK",
        "ID", "PROJECT", "STATUS", "ACTIVITY", "BRANCH"
    );
    for s in sessions {
        let short_id: String = s.id.0.chars().take(8).collect();
        let task = truncate(&s.task, 60);
        let activity = s
            .activity
            .map(|a| a.as_str().to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<10} {:<14} {:<18} {:<14} {:<18} {}",
            short_id,
            s.project_id,
            s.status.as_str(),
            activity,
            s.branch,
            task,
        );
    }
    Ok(())
}

/// Truncate a string to at most `max` characters, appending `…` if cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}
