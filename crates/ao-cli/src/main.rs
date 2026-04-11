//! `ao-rs` — Slice 1 CLI.
//!
//! Subcommands:
//!   - `spawn`           — workspace-worktree → agent-claude-code → runtime-tmux
//!   - `status`          — list persisted sessions from disk
//!   - `watch`           — run the LifecycleManager and stream events to stdout
//!   - `session restore` — respawn a terminated session in-place (Phase D)
//!
//! `watch` is guarded by a pidfile at `~/.ao-rs/lifecycle.pid` so running
//! it twice concurrently fails fast instead of racing two polling loops.

use ao_core::{
    now_ms, paths, restore_session, Agent, LifecycleManager, LockError, OrchestratorEvent, PidFile,
    Runtime, Session, SessionId, SessionManager, SessionStatus, Workspace, WorkspaceCreateConfig,
};
use ao_plugin_agent_claude_code::ClaudeCodeAgent;
use ao_plugin_runtime_tmux::TmuxRuntime;
use ao_plugin_workspace_worktree::WorktreeWorkspace;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
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

    /// Run the lifecycle loop and stream events to stdout.
    ///
    /// Useful for watching a fleet of sessions live. Ctrl-C to stop —
    /// the loop cancels cleanly and persists any in-flight transitions.
    ///
    /// Guarded by `~/.ao-rs/lifecycle.pid` — a second `watch` while one is
    /// already running will exit with a message rather than fight the
    /// first instance over the event stream.
    Watch {
        /// Polling interval in seconds. Defaults to 5 s (matches the TS reference).
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },

    /// Session management subcommands.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// Restore a terminated/crashed session in place.
    ///
    /// Looks the session up on disk, verifies the worktree still exists,
    /// and respawns the runtime with the same launch command. The session
    /// identifier can be the full uuid or any unambiguous prefix.
    Restore {
        /// Full session uuid or a unique prefix (e.g. the 8-char short id).
        session: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Cheap tracing setup — honours RUST_LOG, defaults to warn for our crates.
    // Without this, tracing::warn! calls in the lifecycle loop would be silent.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,ao_core=info")),
        )
        .try_init();

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
        Command::Watch { interval } => watch(Duration::from_secs(interval)).await,
        Command::Session { action } => match action {
            SessionAction::Restore { session } => restore(session).await,
        },
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
    println!(
        "  cleanup: cd {} && git worktree remove --force {}",
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

/// Run the lifecycle loop and pretty-print events as they arrive.
///
/// Wires up real plugins (tmux runtime, claude-code agent) and subscribes
/// to the broadcast channel. Exits cleanly on Ctrl-C or when the channel
/// is closed.
///
/// Phase D added the pidfile guard: we grab `~/.ao-rs/lifecycle.pid`
/// before starting the loop so a second `ao-rs watch` can detect it and
/// back off instead of double-polling every session. The `PidFile` is an
/// RAII handle — it removes itself when this function returns, even on
/// early error.
async fn watch(interval: Duration) -> Result<(), Box<dyn std::error::Error>> {
    // Acquire the singleton lock before touching any plugins so a rejected
    // second watcher exits before spawning tmux/claude probes.
    let pid_path = paths::lifecycle_pid_file();
    let _lock = match PidFile::acquire(&pid_path) {
        Ok(lock) => lock,
        Err(LockError::HeldBy { pid, path }) => {
            eprintln!(
                "ao-rs watch is already running (pid {pid}, lock {}).",
                path.display()
            );
            eprintln!("stop the running watcher first, or delete the lock if it's stale.");
            return Err(format!("lifecycle lock held by pid {pid}").into());
        }
        Err(LockError::Io(e)) => {
            return Err(format!(
                "failed to take lifecycle lock at {}: {e}",
                pid_path.display()
            )
            .into());
        }
    };
    println!("→ acquired lifecycle lock at {}", pid_path.display());

    let sessions = Arc::new(SessionManager::with_default());
    let runtime: Arc<dyn Runtime> = Arc::new(TmuxRuntime::new());
    let agent: Arc<dyn Agent> = Arc::new(ClaudeCodeAgent::new());

    let lifecycle = Arc::new(
        LifecycleManager::new(sessions.clone(), runtime, agent).with_poll_interval(interval),
    );

    let mut events = lifecycle.subscribe();
    let handle = lifecycle.spawn();

    println!(
        "→ watching sessions from {} (poll every {}s). ctrl-c to stop.",
        sessions.base_dir().display(),
        interval.as_secs(),
    );
    println!("{:<10} {:<20} DETAIL", "SESSION", "EVENT");

    // Shutdown path: forward ctrl-c to the lifecycle handle, then break the
    // recv loop.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                println!();
                println!("→ shutdown requested, stopping lifecycle loop...");
                break;
            }
            recv = events.recv() => {
                match recv {
                    Ok(event) => print_event(&event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("(warn) watcher lagged, dropped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    handle.stop().await;
    println!("→ stopped.");
    Ok(())
}

/// `ao-rs session restore <session>` — respawn a terminated session in place.
///
/// Delegates the real work to `ao_core::restore_session`, which mirrors
/// `restore()` in `packages/core/src/session-manager.ts`. The CLI only
/// handles argument parsing, plugin wiring, and error pretty-printing.
async fn restore(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let runtime = TmuxRuntime::new();
    let agent = ClaudeCodeAgent::new();

    println!("→ restoring session: {session_id_or_prefix}");
    let outcome = restore_session(&session_id_or_prefix, &sessions, &runtime, &agent).await?;

    let short: String = outcome.session.id.0.chars().take(8).collect();
    println!();
    println!("───────────────────────────────────────────────");
    println!("  ✓ session restored");
    println!();
    println!("  session: {} (short {short})", outcome.session.id);
    println!("  status:  {}", outcome.session.status.as_str());
    println!("  handle:  {}", outcome.runtime_handle);
    println!("  launch:  {}", outcome.launch_command);
    if let Some(ws) = &outcome.session.workspace_path {
        println!("  worktree: {}", ws.display());
    }
    println!();
    println!("  attach:  tmux attach -t {}", outcome.runtime_handle);
    println!("  status:  ao-rs status");
    println!("───────────────────────────────────────────────");

    Ok(())
}

/// Pretty-print one `OrchestratorEvent` as a single table row.
fn print_event(event: &OrchestratorEvent) {
    let short = |id: &SessionId| -> String { id.0.chars().take(8).collect() };
    match event {
        OrchestratorEvent::Spawned { id, project_id } => {
            println!("{:<10} {:<20} project={project_id}", short(id), "spawned");
        }
        OrchestratorEvent::StatusChanged { id, from, to } => {
            println!(
                "{:<10} {:<20} {} → {}",
                short(id),
                "status_changed",
                from.as_str(),
                to.as_str()
            );
        }
        OrchestratorEvent::ActivityChanged { id, prev, next } => {
            let prev = prev.map(|a| a.as_str()).unwrap_or("-");
            println!(
                "{:<10} {:<20} {prev} → {}",
                short(id),
                "activity_changed",
                next.as_str()
            );
        }
        OrchestratorEvent::Terminated { id, reason } => {
            println!("{:<10} {:<20} {reason}", short(id), "terminated");
        }
        OrchestratorEvent::TickError { id, message } => {
            println!("{:<10} {:<20} {message}", short(id), "tick_error");
        }
        OrchestratorEvent::ReactionTriggered {
            id,
            reaction_key,
            action,
        } => {
            // Reaction events — Slice 2 Phase D. One line each, mirroring
            // the existing row shape so `ao-rs watch` stays grep-friendly.
            println!(
                "{:<10} {:<20} {reaction_key} → {action}",
                short(id),
                "reaction_fired"
            );
        }
        OrchestratorEvent::ReactionEscalated {
            id,
            reaction_key,
            attempts,
        } => {
            println!(
                "{:<10} {:<20} {reaction_key} ({attempts} attempts)",
                short(id),
                "reaction_escalated"
            );
        }
    }
}

/// Truncate a string to at most `max` characters, appending `…` if cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}
