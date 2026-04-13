//! `ao-rs` CLI.
//!
//! Subcommands:
//!   - `start`           — generate or load config file
//!   - `spawn`           — workspace-worktree → agent-claude-code → runtime-tmux
//!   - `status`          — list persisted sessions; `--pr` adds PR/CI columns
//!   - `watch`           — run the LifecycleManager and stream events to stdout
//!   - `send`            — forward a message to a running session's agent
//!   - `pr`              — inspect GitHub PR state + CI + review for a session
//!   - `session restore` — respawn a terminated session in-place
//!
//! `watch` is guarded by a pidfile at `~/.ao-rs/lifecycle.pid` so running
//! it twice concurrently fails fast instead of racing two polling loops.

use ao_core::{
    generate_config, install_skills, now_ms, paths, restore_session, Agent, AoConfig, CiStatus,
    LifecycleManager, LockError, MergeReadiness, NotificationRouting, NotifierRegistry,
    OrchestratorEvent, PidFile, PrState, PullRequest, ReactionEngine, ReviewDecision, Runtime, Scm,
    Session, SessionId, SessionManager, SessionStatus, Tracker, Workspace, WorkspaceCreateConfig,
};
use ao_plugin_agent_claude_code::ClaudeCodeAgent;
use ao_plugin_notifier_desktop::DesktopNotifier;
use ao_plugin_notifier_discord::DiscordNotifier;
use ao_plugin_notifier_ntfy::NtfyNotifier;
use ao_plugin_notifier_stdout::StdoutNotifier;
use ao_plugin_runtime_tmux::TmuxRuntime;
use ao_plugin_scm_github::GitHubScm;
use ao_plugin_tracker_github::GitHubTracker;
use ao_plugin_workspace_worktree::WorktreeWorkspace;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Typed error for duplicate issue detection so `batch_spawn` can distinguish
/// "skipped duplicate" from "real failure" without string matching.
#[derive(Debug)]
struct DuplicateIssue {
    issue_id: String,
    session_short: String,
}

impl std::fmt::Display for DuplicateIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "active session {} is already working on issue #{}. use --force to spawn anyway",
            self.session_short, self.issue_id
        )
    }
}

impl std::error::Error for DuplicateIssue {}

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
    /// Initialize ao-rs: generate config or load existing one.
    ///
    /// If `~/.ao-rs/config.yaml` exists, loads and prints a summary.
    /// Otherwise auto-detects the current git repo and generates a
    /// config with sensible defaults (reactions, notification routing,
    /// project settings).
    Start {
        /// Path to the git repo. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
    },

    /// Spawn a new agent session in an isolated git worktree.
    ///
    /// Provide either `--task` (free-form prompt) or `--issue` (fetch from
    /// the GitHub tracker). Exactly one is required.
    Spawn {
        /// Free-form task description sent to the agent as its first prompt.
        /// Mutually exclusive with `--issue`.
        #[arg(
            short,
            long,
            conflicts_with = "issue",
            required_unless_present = "issue"
        )]
        task: Option<String>,

        /// GitHub issue number (e.g. `42` or `#42`). Fetches the issue title
        /// and body, derives the branch name as `feat/issue-<n>`, and uses
        /// them as the agent task. Mutually exclusive with `--task`.
        #[arg(short, long, conflicts_with = "task", required_unless_present = "task")]
        issue: Option<String>,

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

        /// Spawn even if another active session is already working on the
        /// same issue. Without this flag, `--issue` rejects duplicates.
        #[arg(long)]
        force: bool,
    },

    /// Spawn multiple sessions from a list of GitHub issue numbers.
    ///
    /// Sequentially spawns one session per issue, skipping any that already
    /// have an active session (unless `--force` is set). Prints a summary
    /// when done.
    BatchSpawn {
        /// One or more issue numbers (e.g. `42 43 44`).
        #[arg(required = true)]
        issues: Vec<String>,

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

        /// Spawn even if another active session is already working on the
        /// same issue.
        #[arg(long)]
        force: bool,
    },

    /// List all known sessions, newest first.
    Status {
        /// Filter to a single project id.
        #[arg(long)]
        project: Option<String>,

        /// Also fetch PR state + CI rollup for each session.
        ///
        /// Off by default because it shells out to `gh` once per session
        /// (and skips sessions with no GitHub origin). Only pay the latency
        /// when you actually want the PR column.
        #[arg(long)]
        pr: bool,

        /// Show estimated cost (USD) for each session.
        #[arg(long)]
        cost: bool,
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

    /// Send a message to a running session's agent.
    ///
    /// Thin wrapper over `Runtime::send_message` — the session must have a
    /// live runtime handle (check `ao-rs status`). If the runtime is gone,
    /// `ao-rs session restore <id>` respawns it first.
    Send {
        /// Session uuid or unambiguous prefix (e.g. an 8-char short id).
        session: String,
        /// Message to deliver. Whitespace preserved verbatim.
        message: String,
    },

    /// Show PR state, CI, review decision, and merge readiness for a session.
    ///
    /// Shells out to `gh` via the GitHub SCM plugin. Requires the session's
    /// workspace to have a github.com-shaped `origin` remote — otherwise
    /// the plugin reports "no PR found".
    Pr {
        /// Session uuid or unambiguous prefix.
        session: String,
    },

    /// Run the dashboard API server alongside the lifecycle loop.
    ///
    /// Exposes REST + SSE endpoints at `http://localhost:<port>/api/`.
    /// Same pidfile guard as `watch` — only one instance at a time.
    Dashboard {
        /// Port to listen on.
        #[arg(long, default_value_t = 3000)]
        port: u16,

        /// Lifecycle polling interval in seconds.
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },

    /// Kill a running session: stop the runtime, remove the worktree,
    /// and archive the session file.
    ///
    /// Safe to run on already-terminal sessions — they get archived without
    /// touching the (already gone) runtime.
    Kill {
        /// Session uuid or unambiguous prefix (e.g. an 8-char short id).
        session: String,
    },

    /// Clean up terminal sessions: remove worktrees and archive YAML files.
    ///
    /// Scans all terminal sessions (killed, terminated, errored, merged, etc.)
    /// and for each one removes the git worktree (if it still exists) and
    /// moves the session YAML into `.archive/`. Use `--dry-run` to preview.
    Cleanup {
        /// Filter to a single project id.
        #[arg(long)]
        project: Option<String>,

        /// Show what would be cleaned up without actually doing it.
        #[arg(long)]
        dry_run: bool,
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

    /// Attach to a session's tmux terminal.
    ///
    /// Resolves the session and execs `tmux attach-session -t <handle>`,
    /// replacing the current process. Detach with `Ctrl-b d` as usual.
    Attach {
        /// Session uuid or unambiguous prefix.
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
        Command::Start { repo } => start(repo).await,
        Command::Spawn {
            task,
            issue,
            repo,
            default_branch,
            project,
            no_prompt,
            force,
        } => spawn(task, issue, repo, default_branch, project, no_prompt, force).await,
        Command::BatchSpawn {
            issues,
            repo,
            default_branch,
            project,
            no_prompt,
            force,
        } => batch_spawn(issues, repo, default_branch, project, no_prompt, force).await,
        Command::Status { project, pr, cost } => status(project, pr, cost).await,
        Command::Watch { interval } => watch(Duration::from_secs(interval)).await,
        Command::Dashboard { port, interval } => {
            dashboard(port, Duration::from_secs(interval)).await
        }
        Command::Send { session, message } => send(session, message).await,
        Command::Pr { session } => pr(session).await,
        Command::Kill { session } => kill(session).await,
        Command::Cleanup { project, dry_run } => cleanup(project, dry_run).await,
        Command::Session { action } => match action {
            SessionAction::Restore { session } => restore(session).await,
            SessionAction::Attach { session } => attach(session).await,
        },
    }
}

async fn start(repo: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = repo.unwrap_or_else(|| std::env::current_dir().expect("cannot determine cwd"));
    let config_path = AoConfig::path_in(&cwd);

    if config_path.exists() {
        // Load existing config and print summary.
        let config = AoConfig::load_from(&config_path)
            .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;
        println!("Config already exists: {}", config_path.display());
        println!();
        if let Some(ref defaults) = config.defaults {
            println!("  defaults:");
            println!("    runtime:   {}", defaults.runtime);
            println!("    agent:     {}", defaults.agent);
            println!("    workspace: {}", defaults.workspace);
            if !defaults.notifiers.is_empty() {
                println!("    notifiers: {}", defaults.notifiers.join(", "));
            }
        }
        if !config.projects.is_empty() {
            println!(
                "  projects:  {}",
                config
                    .projects
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("  reactions: {} configured", config.reactions.len());
        println!(
            "  routing:   {} priority level(s)",
            config.notification_routing.len()
        );
        println!();
        println!("Edit {} to customize.", config_path.display());
        return Ok(());
    }

    // Generate new config by detecting the current git repo.
    let config = generate_config(&cwd).map_err(|e| format!("failed to detect project: {e}"))?;

    config
        .save_to(&config_path)
        .map_err(|e| format!("failed to write {}: {e}", config_path.display()))?;

    // Install ai-devkit skills (non-fatal).
    println!("→ installing ai-devkit skills...");
    match install_skills(&cwd) {
        Ok(()) => println!("  ✓ skills installed"),
        Err(e) => println!("  ⚠ skill installation skipped: {e}"),
    }

    println!();
    println!("Created {}", config_path.display());
    println!();
    if let Some(ref defaults) = config.defaults {
        println!("  defaults:");
        println!("    runtime:   {}", defaults.runtime);
        println!("    agent:     {}", defaults.agent);
        println!("    workspace: {}", defaults.workspace);
    }
    for (name, project) in &config.projects {
        println!("  project \"{}\":", name);
        println!("    repo:           {}", project.repo);
        println!("    path:           {}", project.path);
        println!("    default_branch: {}", project.default_branch);
        if let Some(ref ac) = project.agent_config {
            println!("    permissions:    {}", ac.permissions);
        }
    }
    println!("  reactions: {} configured", config.reactions.len());
    println!(
        "  routing:   {} priority level(s)",
        config.notification_routing.len()
    );
    println!();
    println!("Edit {} to customize.", config_path.display());
    Ok(())
}

async fn spawn(
    task: Option<String>,
    issue: Option<String>,
    repo: Option<PathBuf>,
    default_branch: String,
    project: String,
    no_prompt: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // ---- 1. Resolve repo path ----
    let repo_path = match repo {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    if !repo_path.join(".git").exists() {
        return Err(format!("not a git repo: {}", repo_path.display()).into());
    }

    // ---- 1b. Resolve task, branch, and issue metadata ----
    // Either --issue <n> (fetch from GitHub) or --task "..." (free-form).
    let (resolved_task, branch_prefix, resolved_issue_id, resolved_issue_url) =
        if let Some(ref id) = issue {
            // Normalize: strip leading `#` so `#42` and `42` match the stored
            // `issue_id` (which is always a bare number from `gh issue view`).
            let normalized = id.strip_prefix('#').unwrap_or(id);

            // Duplicate detection: reject if another active session is on this issue.
            if !force {
                let manager = SessionManager::with_default();
                let dupes = manager.find_by_issue_id(normalized).await?;
                if !dupes.is_empty() {
                    let short = short_id(&dupes[0].id);
                    return Err(DuplicateIssue {
                        issue_id: normalized.to_string(),
                        session_short: short,
                    }
                    .into());
                }
            }
            println!("→ fetching issue {}...", id);
            let tracker = GitHubTracker::from_repo(&repo_path).await?;
            let fetched = tracker.get_issue(id).await?;
            let branch = tracker.branch_name(id);
            let task_body = if fetched.description.is_empty() {
                fetched.title.clone()
            } else {
                format!("{}\n\n{}", fetched.title, fetched.description)
            };
            println!("  issue:     #{} — {}", fetched.id, fetched.title);
            (
                task_body,
                Some(branch),
                Some(fetched.id.clone()),
                Some(fetched.url.clone()),
            )
        } else {
            (task.unwrap(), None, None, None)
        };

    // ---- 2. Allocate ids ----
    let session_id = SessionId::new();
    // Short id is what tmux + worktree dirs see — uuid is too long for a tmux name.
    let short_id: String = session_id.0.chars().take(8).collect();
    // Issue-first: prefix tracker branch with ao-<shortid> for uniqueness so
    // spawning the same issue twice doesn't collide on `git worktree add`.
    // Result: `ao-3a4b5c6d-feat-issue-42` (slashes → dashes for git compat).
    // Prompt-first: plain `ao-<shortid>`.
    let branch = match branch_prefix {
        Some(b) => format!("ao-{short_id}-{}", b.replace('/', "-")),
        None => format!("ao-{short_id}"),
    };

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

    // Guard: clean up the worktree on any subsequent error. Without this,
    // a failure in steps 4-6 (save, runtime create, send_message) leaves a
    // ghost worktree directory with no Session record pointing at it.
    let post_workspace_result: Result<Session, Box<dyn std::error::Error>> = async {
        // Build the Session and persist it. Slice 1 Phase A: disk-backed.
        let mut session = Session {
            id: session_id.clone(),
            project_id: project.clone(),
            status: SessionStatus::Spawning,
            branch: branch.clone(),
            task: resolved_task,
            workspace_path: Some(workspace_path.clone()),
            runtime_handle: None,
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: resolved_issue_id,
            issue_url: resolved_issue_url,
        };

        let manager = SessionManager::with_default();
        manager.save(&session).await?;

        // ---- 4. Agent: get launch command + env ----
        let agent = ClaudeCodeAgent::with_default_rules();
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
            // Claude Code takes a few seconds to initialize its TUI.
            // Without this delay, send-keys lands before the input is ready.
            tokio::time::sleep(Duration::from_millis(3000)).await;
            println!("→ sending initial prompt: {initial_prompt:?}");
            runtime.send_message(&handle, &initial_prompt).await?;
        }

        Ok(session)
    }
    .await;

    let session = match post_workspace_result {
        Ok(s) => s,
        Err(e) => {
            // Best-effort cleanup — if this also fails, log and surface the
            // original error so the user knows *why* the spawn failed.
            if let Err(cleanup_err) = workspace.destroy(&workspace_path).await {
                eprintln!("warning: failed to clean up worktree after spawn error: {cleanup_err}");
            }
            return Err(e);
        }
    };

    let handle = session.runtime_handle.as_deref().unwrap_or(&short_id);
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

/// `ao-rs batch-spawn <issues...>` — spawn one session per issue.
///
/// Iterates the issue list sequentially, running the same spawn logic per
/// issue. Skips duplicates (another active session on the same issue) unless
/// `--force` is set. Prints a summary at the end.
async fn batch_spawn(
    issues: Vec<String>,
    repo: Option<PathBuf>,
    default_branch: String,
    project: String,
    no_prompt: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let total = issues.len();
    let mut created = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;

    println!("→ batch-spawn: {} issue(s)", total);
    println!();

    for (i, issue_id) in issues.iter().enumerate() {
        println!("── [{}/{}] issue #{issue_id} ──", i + 1, total);

        match spawn(
            None,
            Some(issue_id.clone()),
            repo.clone(),
            default_branch.clone(),
            project.clone(),
            no_prompt,
            force,
        )
        .await
        {
            Ok(()) => {
                created += 1;
            }
            Err(e) => {
                if e.downcast_ref::<DuplicateIssue>().is_some() {
                    println!("  ⊘ skipped: {e}");
                    skipped += 1;
                } else {
                    eprintln!("  ✗ failed: {e}");
                    failed += 1;
                }
            }
        }
        println!();
    }

    println!("───────────────────────────────────────────────");
    println!("  batch-spawn summary:");
    println!("    created: {created}");
    if skipped > 0 {
        println!("    skipped: {skipped} (duplicate)");
    }
    if failed > 0 {
        println!("    failed:  {failed}");
    }
    println!("───────────────────────────────────────────────");

    if failed > 0 {
        Err(format!("{failed} spawn(s) failed").into())
    } else {
        Ok(())
    }
}

async fn status(
    project_filter: Option<String>,
    with_pr: bool,
    with_cost: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
    //
    // Header and row formatting adapt to the --pr and --cost flags.
    let cost_hdr = if with_cost {
        format!("{:<10} ", "COST")
    } else {
        String::new()
    };
    if with_pr {
        println!(
            "{:<10} {:<14} {:<18} {:<14} {:<18} {:<24} {}TASK",
            "ID", "PROJECT", "STATUS", "ACTIVITY", "BRANCH", "PR", cost_hdr
        );
    } else {
        println!(
            "{:<10} {:<14} {:<18} {:<14} {:<18} {}TASK",
            "ID", "PROJECT", "STATUS", "ACTIVITY", "BRANCH", cost_hdr
        );
    }

    // Build the SCM plugin once up front if `--pr` is on, rather than
    // per-row. `GitHubScm` is a zero-sized type, but allocating it in a
    // branch keeps the non-`--pr` path completely free of `gh` linkage at
    // call time.
    let scm = if with_pr {
        Some(GitHubScm::new())
    } else {
        None
    };

    for s in sessions {
        let short_id: String = s.id.0.chars().take(8).collect();
        let task = truncate(&s.task, 60);
        let activity = s
            .activity
            .map(|a| a.as_str().to_string())
            .unwrap_or_else(|| "-".to_string());
        let cost_cell = if with_cost {
            format!(
                "{:<10} ",
                s.cost
                    .as_ref()
                    .map(|c| format!("${:.2}", c.cost_usd))
                    .unwrap_or_else(|| "-".to_string())
            )
        } else {
            String::new()
        };

        if let Some(scm) = scm.as_ref() {
            let pr_cell = fetch_pr_column(scm, &s).await;
            println!(
                "{:<10} {:<14} {:<18} {:<14} {:<18} {:<24} {}{}",
                short_id,
                s.project_id,
                s.status.as_str(),
                activity,
                s.branch,
                pr_cell,
                cost_cell,
                task,
            );
        } else {
            println!(
                "{:<10} {:<14} {:<18} {:<14} {:<18} {}{}",
                short_id,
                s.project_id,
                s.status.as_str(),
                activity,
                s.branch,
                cost_cell,
                task,
            );
        }
    }
    Ok(())
}

/// Best-effort PR column for `ao-rs status --pr`.
///
/// Two failure tiers:
/// - `detect_pr` failure (or `Ok(None)`) → `-`, i.e. "this row has no PR
///   as far as we can tell". Mirrors the `detect_pr` tolerant contract.
/// - Post-detect failure (`pr_state`/`ci_status` err) → `pr_column`
///   renders `?` for the missing half, so the row still shows `#N ?/?`
///   or `#N open/?`. That's distinct from `-` on purpose: "there's a PR
///   here, we just couldn't read all of it this tick".
async fn fetch_pr_column(scm: &GitHubScm, session: &Session) -> String {
    let Ok(Some(pr)) = scm.detect_pr(session).await else {
        return "-".to_string();
    };
    // `pr_state` and `ci_status` are independent — run them concurrently
    // so `--pr` doesn't pay 2× RTT per session. Both results feed the
    // pure formatter so the column shape is testable.
    let (state, ci) = tokio::join!(scm.pr_state(&pr), scm.ci_status(&pr));
    pr_column(Some(&pr), state.ok(), ci.ok())
}

/// Compact PR column cell. Pulled out as a pure function so the width
/// and shape can be unit-tested without shelling out to `gh`.
///
/// Format:
///   `-`                 — no PR (or any upstream error)
///   `#42 open/passing`  — PR number, pr state, rolled-up CI
///   `#42 merged`        — merged PRs drop the CI suffix (GitHub discards it)
fn pr_column(pr: Option<&PullRequest>, state: Option<PrState>, ci: Option<CiStatus>) -> String {
    let Some(pr) = pr else {
        return "-".to_string();
    };
    let state_label = state.map(pr_state_label).unwrap_or("?");
    // Merged/closed PRs shouldn't advertise a CI column — GitHub drops the
    // check data for them and we want the table to read "it's done" rather
    // than "it's done but CI is also saying something".
    if matches!(state, Some(PrState::Merged) | Some(PrState::Closed)) {
        return format!("#{} {state_label}", pr.number);
    }
    let ci_label = ci.map(ci_status_label).unwrap_or("?");
    format!("#{} {state_label}/{ci_label}", pr.number)
}

/// `ao-rs send <session> <msg>` — forward a message to a live agent.
///
/// Resolves the session by uuid or prefix, checks the runtime is still
/// alive, and hands the message to `Runtime::send_message`. Dead runtimes
/// get a nudge toward `ao-rs session restore` rather than a raw tmux error.
async fn send(
    session_id_or_prefix: String,
    message: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let session = sessions.find_by_prefix(&session_id_or_prefix).await?;

    let handle = session.runtime_handle.as_deref().ok_or_else(|| {
        format!(
            "session {} has no runtime handle (status={}); nothing to send to",
            session.id,
            session.status.as_str()
        )
    })?;

    // Probe before send so a common failure mode — "the session crashed" —
    // produces an actionable error instead of a tmux stderr dump. Surface
    // probe-itself errors (tmux binary missing, spawn EMFILE, ...) directly
    // rather than collapsing them to "dead": restoring into the same broken
    // tmux would just fail again with less context.
    let runtime = TmuxRuntime::new();
    let alive = runtime
        .is_alive(handle)
        .await
        .map_err(|e| format!("failed to probe runtime {handle}: {e}"))?;
    if !alive {
        return Err(format!(
            "runtime handle {handle} is not alive. \
             try: ao-rs session restore {}",
            short_id(&session.id)
        )
        .into());
    }

    runtime.send_message(handle, &message).await?;
    println!("→ sent {} bytes to {handle}", message.len());
    Ok(())
}

/// `ao-rs pr <session>` — summarize the GitHub PR for a session.
///
/// Calls into the `GitHubScm` plugin: `detect_pr` first, then fans out
/// `pr_state`, `ci_status`, `review_decision`, `mergeability` in parallel.
/// `mergeability` internally re-invokes `pr_state` + `ci_status` + its own
/// `gh pr view --json mergeable,...` call, so the wall-clock total is
/// `1 (detect_pr) + max(4 parallel calls, mergeability's 3 sequential
/// inner calls) ≈ 7 gh subprocesses` per `ao-rs pr`. Accepted duplication
/// — keeping the `Scm` trait self-contained is worth more than shaving
/// two subprocesses off a manual debug command.
///
/// If there's no PR yet, exits 0 with a friendly message — the session
/// may simply not have pushed a branch.
async fn pr(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let session = sessions.find_by_prefix(&session_id_or_prefix).await?;

    let scm = GitHubScm::new();
    let Some(pr) = scm.detect_pr(&session).await? else {
        println!(
            "no PR found for session {} (branch {})",
            session.id, session.branch
        );
        return Ok(());
    };

    // Everything downstream is independent — fan out concurrently so `ao-rs
    // pr` doesn't pay 4× RTT. `mergeability` internally re-calls `pr_state`
    // and `ci_status`, so the total gh invocation count is ~7, not 4.
    // Accepted duplication — see the handler doc comment for rationale.
    let (state, ci, decision, readiness) = tokio::join!(
        scm.pr_state(&pr),
        scm.ci_status(&pr),
        scm.review_decision(&pr),
        scm.mergeability(&pr),
    );

    let report = format_pr_report(&session, &pr, state?, ci?, decision?, &readiness?);
    print!("{report}");
    Ok(())
}

/// Pretty-print a full PR report. Pulled out as a pure function — takes
/// everything already-fetched — so tests can exercise the blocker-list
/// formatting without shelling out to `gh`.
fn format_pr_report(
    session: &Session,
    pr: &PullRequest,
    state: PrState,
    ci: CiStatus,
    decision: ReviewDecision,
    readiness: &MergeReadiness,
) -> String {
    let mut out = String::new();
    out.push_str("───────────────────────────────────────────────\n");
    out.push_str(&format!(
        "  session: {} (short {})\n",
        session.id,
        short_id(&session.id)
    ));
    out.push_str(&format!("  branch:  {}\n", session.branch));
    out.push_str(&format!("  PR:      #{} {}\n", pr.number, pr.title));
    out.push_str(&format!("  url:     {}\n", pr.url));
    out.push('\n');
    out.push_str(&format!("  state:   {}\n", pr_state_label(state)));
    out.push_str(&format!("  CI:      {}\n", ci_status_label(ci)));
    out.push_str(&format!("  review:  {}\n", review_decision_label(decision)));
    out.push('\n');
    out.push_str(&format!(
        "  mergeable: {}\n",
        if readiness.is_ready() { "yes" } else { "no" }
    ));
    if !readiness.blockers.is_empty() {
        out.push_str("  blockers:\n");
        for b in &readiness.blockers {
            out.push_str(&format!("    - {b}\n"));
        }
    }
    out.push_str("───────────────────────────────────────────────\n");
    out
}

fn pr_state_label(s: PrState) -> &'static str {
    match s {
        PrState::Open => "open",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    }
}

fn ci_status_label(s: CiStatus) -> &'static str {
    match s {
        CiStatus::Pending => "pending",
        CiStatus::Passing => "passing",
        CiStatus::Failing => "failing",
        CiStatus::None => "none",
    }
}

fn review_decision_label(d: ReviewDecision) -> &'static str {
    match d {
        ReviewDecision::Approved => "approved",
        ReviewDecision::ChangesRequested => "changes_requested",
        ReviewDecision::Pending => "pending",
        ReviewDecision::None => "none",
    }
}

fn short_id(id: &SessionId) -> String {
    id.0.chars().take(8).collect()
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
    let agent: Arc<dyn Agent> = Arc::new(ClaudeCodeAgent::with_default_rules());
    // Phase F: SCM plugin is compile-time GitHubScm. Zero-sized, so the
    // Arc here is just for trait-object uniformity with Runtime/Agent.
    let scm: Arc<dyn Scm> = Arc::new(GitHubScm::new());

    // Load config from the local project directory (ao-rs.yaml).
    // Missing config is silently empty; a broken YAML is a loud error.
    let config_path = AoConfig::local_path();
    let config = AoConfig::load_from_or_default(&config_path)
        .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;
    if !config.reactions.is_empty() {
        println!(
            "→ loaded {} reaction(s) from {}",
            config.reactions.len(),
            config_path.display()
        );
    }

    // Build lifecycle first so we can hand its broadcast channel to the
    // engine — engine events share the lifecycle channel so subscribers
    // see `ReactionTriggered` interleaved with `StatusChanged` etc.
    let lifecycle_builder = LifecycleManager::new(sessions.clone(), runtime.clone(), agent)
        .with_poll_interval(interval);
    let events_tx = lifecycle_builder.events_sender();

    // Slice 3 Phase C: build the notifier registry. When the user has a
    // `notification-routing:` section in their config, honour it; when
    // they don't (empty routing table), default to routing every priority
    // to stdout so notifications are never silently dropped.
    let mut notifier_registry = if config.notification_routing.is_empty() {
        // Default: route everything to stdout.
        use ao_core::reactions::EventPriority;
        use std::collections::HashMap;
        let mut default_routing = HashMap::new();
        for &p in &[
            EventPriority::Urgent,
            EventPriority::Action,
            EventPriority::Warning,
            EventPriority::Info,
        ] {
            default_routing.insert(p, vec!["stdout".to_string()]);
        }
        NotifierRegistry::new(NotificationRouting::from_map(default_routing))
    } else {
        NotifierRegistry::new(config.notification_routing)
    };
    notifier_registry.register("stdout", Arc::new(StdoutNotifier::new()));

    // Phase D: register ntfy if the AO_NTFY_TOPIC env var is set.
    // The topic is required — without it, ntfy silently stays unregistered
    // and the routing table's "ntfy" entries warn-once on first resolve.
    if let Ok(topic) = std::env::var("AO_NTFY_TOPIC") {
        let base = std::env::var("AO_NTFY_URL").unwrap_or_else(|_| "https://ntfy.sh".to_string());
        notifier_registry.register("ntfy", Arc::new(NtfyNotifier::with_base_url(topic, base)));
    }

    // Slice 4: register desktop notifier (always available).
    notifier_registry.register("desktop", Arc::new(DesktopNotifier::new()));

    // Slice 4: register discord if the AO_DISCORD_WEBHOOK_URL env var is set.
    if let Ok(webhook_url) = std::env::var("AO_DISCORD_WEBHOOK_URL") {
        notifier_registry.register("discord", Arc::new(DiscordNotifier::new(webhook_url)));
    }

    // Phase F wires SCM into both engines. `LifecycleManager` uses it to
    // drive PR-driven status transitions; `ReactionEngine` uses it to
    // re-probe + actually merge on `approved-and-green`. Same
    // `Arc<dyn Scm>` shared by both so we only pay for one plugin
    // instance.
    let engine = Arc::new(
        ReactionEngine::new(config.reactions, runtime.clone(), events_tx)
            .with_scm(scm.clone())
            .with_notifier_registry(notifier_registry),
    );

    let lifecycle = Arc::new(lifecycle_builder.with_reaction_engine(engine).with_scm(scm));

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

/// Run the dashboard API server alongside the lifecycle loop.
///
/// Reuses the same plugin wiring as `watch` and adds an axum HTTP server.
/// Both run concurrently under `tokio::select!` so Ctrl-C stops them
/// together.
async fn dashboard(port: u16, interval: Duration) -> Result<(), Box<dyn std::error::Error>> {
    let pid_path = paths::lifecycle_pid_file();
    let _lock = match PidFile::acquire(&pid_path) {
        Ok(lock) => lock,
        Err(LockError::HeldBy { pid, path }) => {
            eprintln!(
                "ao-rs is already running (pid {pid}, lock {}).",
                path.display()
            );
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

    let sessions = Arc::new(SessionManager::with_default());
    let runtime: Arc<dyn Runtime> = Arc::new(TmuxRuntime::new());
    let agent: Arc<dyn Agent> = Arc::new(ClaudeCodeAgent::with_default_rules());
    let scm: Arc<dyn Scm> = Arc::new(GitHubScm::new());

    let config_path = AoConfig::local_path();
    let config = AoConfig::load_from_or_default(&config_path)
        .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;

    let lifecycle_builder = LifecycleManager::new(sessions.clone(), runtime.clone(), agent)
        .with_poll_interval(interval);
    let events_tx = lifecycle_builder.events_sender();

    // Notifier registry (same as watch).
    let mut notifier_registry = if config.notification_routing.is_empty() {
        use ao_core::reactions::EventPriority;
        use std::collections::HashMap;
        let mut default_routing = HashMap::new();
        for &p in &[
            EventPriority::Urgent,
            EventPriority::Action,
            EventPriority::Warning,
            EventPriority::Info,
        ] {
            default_routing.insert(p, vec!["stdout".to_string()]);
        }
        NotifierRegistry::new(NotificationRouting::from_map(default_routing))
    } else {
        NotifierRegistry::new(config.notification_routing)
    };
    notifier_registry.register("stdout", Arc::new(StdoutNotifier::new()));
    if let Ok(topic) = std::env::var("AO_NTFY_TOPIC") {
        let base = std::env::var("AO_NTFY_URL").unwrap_or_else(|_| "https://ntfy.sh".to_string());
        notifier_registry.register("ntfy", Arc::new(NtfyNotifier::with_base_url(topic, base)));
    }
    notifier_registry.register("desktop", Arc::new(DesktopNotifier::new()));
    if let Ok(webhook_url) = std::env::var("AO_DISCORD_WEBHOOK_URL") {
        notifier_registry.register("discord", Arc::new(DiscordNotifier::new(webhook_url)));
    }

    let engine = Arc::new(
        ReactionEngine::new(config.reactions, runtime.clone(), events_tx.clone())
            .with_scm(scm.clone())
            .with_notifier_registry(notifier_registry),
    );

    let lifecycle = Arc::new(lifecycle_builder.with_reaction_engine(engine).with_scm(scm));
    let lifecycle_handle = lifecycle.spawn();

    // Build dashboard state and start the HTTP server.
    let dashboard_state = ao_dashboard::state::AppState {
        sessions,
        events_tx,
        runtime,
    };

    println!(
        "→ dashboard listening on http://localhost:{port}/api/ (poll every {}s)",
        interval.as_secs()
    );

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    tokio::select! {
        _ = &mut ctrl_c => {
            println!();
            println!("→ shutdown requested");
        }
        result = ao_dashboard::run_server(dashboard_state, port) => {
            if let Err(e) = result {
                eprintln!("dashboard server error: {e}");
            }
        }
    }

    lifecycle_handle.stop().await;
    println!("→ stopped.");
    Ok(())
}

/// `ao-rs kill <session>` — stop the runtime, remove the worktree, archive.
///
/// Safe to run on already-terminal sessions: the runtime and worktree steps
/// are best-effort (a missing tmux session or already-removed worktree just
/// logs a warning), and the archive always runs.
async fn kill(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let mut session = match sessions.find_by_prefix(&session_id_or_prefix).await {
        Ok(s) => s,
        Err(ao_core::AoError::SessionNotFound(_)) => {
            // Check if already archived — give a clearer message than "not found".
            let all_projects = sessions.list().await.unwrap_or_default();
            // Collect unique project IDs to search archives.
            let project_ids: std::collections::HashSet<_> =
                all_projects.iter().map(|s| s.project_id.as_str()).collect();
            for pid in project_ids {
                let archived = sessions.list_archived(pid).await.unwrap_or_default();
                if archived
                    .iter()
                    .any(|s| s.id.0.starts_with(&session_id_or_prefix))
                {
                    return Err(format!(
                        "session {session_id_or_prefix} is already killed and archived"
                    )
                    .into());
                }
            }
            return Err(ao_core::AoError::SessionNotFound(session_id_or_prefix.clone()).into());
        }
        Err(e) => return Err(e.into()),
    };
    let short = short_id(&session.id);

    // 1. Kill runtime (best-effort — may already be gone).
    if let Some(ref handle) = session.runtime_handle {
        let runtime = TmuxRuntime::new();
        match runtime.destroy(handle).await {
            Ok(()) => println!("→ killed runtime {handle}"),
            Err(e) => eprintln!("  warning: runtime destroy failed (may already be gone): {e}"),
        }
    }

    // 2. Remove worktree (best-effort — destroy already handles missing dirs).
    if let Some(ref ws) = session.workspace_path {
        let workspace = WorktreeWorkspace::new();
        match workspace.destroy(ws).await {
            Ok(()) => println!("→ removed worktree {}", ws.display()),
            Err(e) => eprintln!("  warning: worktree cleanup failed: {e}"),
        }
    }

    // 3. Transition to Killed (unless already terminal).
    if !session.status.is_terminal() {
        session.status = SessionStatus::Killed;
        sessions.save(&session).await?;
    }

    // 4. Archive — moves YAML from active dir to .archive/.
    sessions.archive(&session).await?;

    println!("→ session {short} killed and archived");
    Ok(())
}

/// `ao-rs cleanup` — remove worktrees and archive terminal sessions.
///
/// Iterates every terminal session (optionally filtered by `--project`),
/// removes the git worktree if it still exists on disk, and moves the
/// session YAML into `.archive/`. `--dry-run` previews without acting.
async fn cleanup(
    project_filter: Option<String>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let all = match &project_filter {
        Some(p) => sessions.list_for_project(p).await?,
        None => sessions.list().await?,
    };

    let terminal: Vec<_> = all.into_iter().filter(|s| s.is_terminal()).collect();

    if terminal.is_empty() {
        println!("no terminal sessions to clean up");
        return Ok(());
    }

    let mut cleaned = 0u32;
    let mut errors = 0u32;

    for session in &terminal {
        let short = short_id(&session.id);

        if dry_run {
            let ws_note = session
                .workspace_path
                .as_ref()
                .filter(|p| p.exists())
                .map(|p| format!(" (worktree: {})", p.display()))
                .unwrap_or_default();
            println!(
                "  would clean: {short} ({}, {}){ws_note}",
                session.project_id,
                session.status.as_str(),
            );
            cleaned += 1;
            continue;
        }

        // Remove worktree if still on disk.
        if let Some(ref ws) = session.workspace_path {
            if ws.exists() {
                let workspace = WorktreeWorkspace::new();
                match workspace.destroy(ws).await {
                    Ok(()) => println!("  → removed worktree: {}", ws.display()),
                    Err(e) => {
                        eprintln!("  warning: worktree cleanup for {short}: {e}");
                        errors += 1;
                    }
                }
            }
        }

        // Archive session YAML.
        match sessions.archive(session).await {
            Ok(()) => {
                println!("  → archived: {short}");
                cleaned += 1;
            }
            Err(e) => {
                eprintln!("  error archiving {short}: {e}");
                errors += 1;
            }
        }
    }

    println!();
    if dry_run {
        println!("dry run: {cleaned} session(s) would be cleaned");
    } else {
        println!("cleaned: {cleaned}, errors: {errors}");
    }
    Ok(())
}

/// `ao-rs session attach <session>` — exec into a tmux session.
///
/// Replaces the current process with `tmux attach-session -t <handle>`.
/// Detach with the usual `Ctrl-b d`.
async fn attach(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let session = sessions.find_by_prefix(&session_id_or_prefix).await?;

    let handle = session.runtime_handle.as_deref().ok_or_else(|| {
        format!(
            "session {} has no runtime handle (status={})",
            short_id(&session.id),
            session.status.as_str()
        )
    })?;

    // exec() replaces the current process image — user is dropped straight
    // into tmux. If it returns at all, the exec failed.
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new("tmux")
        .args(["attach-session", "-t", handle])
        .exec();
    Err(format!("failed to exec tmux: {err}").into())
}

/// `ao-rs session restore <session>` — respawn a terminated session in place.
///
/// Delegates the real work to `ao_core::restore_session`, which mirrors
/// `restore()` in `packages/core/src/session-manager.ts`. The CLI only
/// handles argument parsing, plugin wiring, and error pretty-printing.
async fn restore(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let runtime = TmuxRuntime::new();
    let agent = ClaudeCodeAgent::with_default_rules();

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

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{now_ms, SessionStatus};

    fn fake_pr(number: u32) -> PullRequest {
        PullRequest {
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: "fix the widgets".into(),
            owner: "acme".into(),
            repo: "widgets".into(),
            branch: "ao-3a4b5c6d".into(),
            base_branch: "main".into(),
            is_draft: false,
        }
    }

    fn fake_session() -> Session {
        Session {
            id: SessionId("3a4b5c6d-aaaa-bbbb-cccc-dddd".into()),
            project_id: "demo".into(),
            status: SessionStatus::Working,
            branch: "ao-3a4b5c6d".into(),
            task: "fix the widgets".into(),
            workspace_path: None,
            runtime_handle: Some("3a4b5c6d".into()),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
        }
    }

    // ---- pr_column --------------------------------------------------------

    #[test]
    fn pr_column_none_pr_is_dash() {
        assert_eq!(pr_column(None, None, None), "-");
        // Even if somehow state/ci were available, no PR still means dash.
        assert_eq!(
            pr_column(None, Some(PrState::Open), Some(CiStatus::Passing)),
            "-"
        );
    }

    #[test]
    fn pr_column_open_pr_shows_state_and_ci() {
        let pr = fake_pr(42);
        assert_eq!(
            pr_column(Some(&pr), Some(PrState::Open), Some(CiStatus::Passing)),
            "#42 open/passing"
        );
        assert_eq!(
            pr_column(Some(&pr), Some(PrState::Open), Some(CiStatus::Failing)),
            "#42 open/failing"
        );
    }

    #[test]
    fn pr_column_merged_drops_ci_suffix() {
        // GitHub stops serving check data for merged PRs; reporting "passing"
        // would be a lie. Collapse to just `#N merged`.
        let pr = fake_pr(7);
        assert_eq!(
            pr_column(Some(&pr), Some(PrState::Merged), Some(CiStatus::Passing)),
            "#7 merged"
        );
        // Closed gets the same treatment.
        assert_eq!(
            pr_column(Some(&pr), Some(PrState::Closed), None),
            "#7 closed"
        );
    }

    #[test]
    fn pr_column_missing_state_or_ci_falls_back_to_question_mark() {
        // If `gh` flaked mid-row, show `?` for the unknown bit rather than
        // bailing the entire row. The `-` already means "no PR at all" — we
        // need a distinct cell for "PR exists but we couldn't read it".
        let pr = fake_pr(11);
        assert_eq!(pr_column(Some(&pr), None, None), "#11 ?/?");
        assert_eq!(
            pr_column(Some(&pr), Some(PrState::Open), None),
            "#11 open/?"
        );
        // And the symmetric case — state unknown but CI known. Fetches
        // are independent, either can succeed alone.
        assert_eq!(
            pr_column(Some(&pr), None, Some(CiStatus::Passing)),
            "#11 ?/passing"
        );
    }

    // ---- format_pr_report -------------------------------------------------

    #[test]
    fn format_pr_report_green_pr_has_no_blockers_section() {
        let pr = fake_pr(42);
        let session = fake_session();
        let readiness = MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        };
        let out = format_pr_report(
            &session,
            &pr,
            PrState::Open,
            CiStatus::Passing,
            ReviewDecision::Approved,
            &readiness,
        );
        assert!(out.contains("#42 fix the widgets"));
        assert!(out.contains("state:   open"));
        assert!(out.contains("CI:      passing"));
        assert!(out.contains("review:  approved"));
        assert!(out.contains("mergeable: yes"));
        // Blocker section is elided when the list is empty — keeps the
        // happy-path output compact.
        assert!(!out.contains("blockers:"), "got:\n{out}");
    }

    #[test]
    fn format_pr_report_blocked_pr_lists_every_blocker() {
        let pr = fake_pr(42);
        let session = fake_session();
        let readiness = MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: false,
            no_conflicts: false,
            blockers: vec![
                "CI is failing".into(),
                "Changes requested in review".into(),
                "Merge conflicts".into(),
            ],
        };
        let out = format_pr_report(
            &session,
            &pr,
            PrState::Open,
            CiStatus::Failing,
            ReviewDecision::ChangesRequested,
            &readiness,
        );
        assert!(out.contains("mergeable: no"));
        assert!(out.contains("blockers:"));
        assert!(out.contains("- CI is failing"));
        assert!(out.contains("- Changes requested in review"));
        assert!(out.contains("- Merge conflicts"));
        assert!(out.contains("review:  changes_requested"));
    }

    #[test]
    fn format_pr_report_includes_short_id_and_full_uuid() {
        // Both forms are useful: short-id for copy-paste into subsequent
        // commands, full uuid so the user can disambiguate if they've got
        // colliding short prefixes.
        let pr = fake_pr(1);
        let session = fake_session();
        let readiness = MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        };
        let out = format_pr_report(
            &session,
            &pr,
            PrState::Open,
            CiStatus::Passing,
            ReviewDecision::Approved,
            &readiness,
        );
        assert!(out.contains("3a4b5c6d-aaaa-bbbb-cccc-dddd"));
        assert!(out.contains("short 3a4b5c6d"));
    }

    // ---- label helpers ----------------------------------------------------

    #[test]
    fn label_helpers_match_variant_shape() {
        // Cheap guard so a future variant addition doesn't silently get an
        // empty or wrong label. Pairs with the `#[non_exhaustive]`-free
        // nature of these enums — adding a variant forces the match to
        // update.
        assert_eq!(pr_state_label(PrState::Open), "open");
        assert_eq!(pr_state_label(PrState::Merged), "merged");
        assert_eq!(pr_state_label(PrState::Closed), "closed");
        assert_eq!(ci_status_label(CiStatus::Pending), "pending");
        assert_eq!(ci_status_label(CiStatus::None), "none");
        assert_eq!(
            review_decision_label(ReviewDecision::ChangesRequested),
            "changes_requested"
        );
        assert_eq!(review_decision_label(ReviewDecision::None), "none");
    }
}
