//! Clap CLI definitions for `ao-rs`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ao-rs",
    about = "Rust port of agent-orchestrator (learning project — Slice 0)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
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

        /// After ensuring `ao-rs.yaml` exists, run the dashboard + orchestrator.
        ///
        /// Equivalent to running `ao-rs dashboard` after `ao-rs start`.
        #[arg(long)]
        run: bool,

        /// Port to listen on when `--run` is set.
        #[arg(long, default_value_t = 3000)]
        port: u16,

        /// Lifecycle polling interval in seconds when `--run` is set. When omitted, uses `poll_interval` from `ao-rs.yaml` (default 10).
        #[arg(long)]
        interval: Option<u64>,

        /// Open the dashboard root URL in the default browser (requires `--run`).
        #[arg(long)]
        open: bool,
    },

    /// Spawn a new agent session in an isolated git worktree.
    ///
    /// Provide exactly one of `--task`, `--issue`, or `--local-issue`.
    Spawn {
        /// Free-form task description sent to the agent as its first prompt.
        #[arg(
            short,
            long,
            conflicts_with_all = ["issue", "local_issue"],
            required_unless_present_any = ["issue", "local_issue"]
        )]
        task: Option<String>,

        /// GitHub issue number (e.g. `42` or `#42`). Fetches the issue title
        /// and body, derives the branch name as `<type>/<issue>-<slug>` (type
        /// is derived from issue labels first, then title), and
        /// uses the issue title as the agent task.
        #[arg(
            short,
            long,
            conflicts_with_all = ["task", "local_issue"],
            required_unless_present_any = ["task", "local_issue"]
        )]
        issue: Option<String>,

        /// Local markdown issue (`docs/issues/NNNN-slug.md` from `ao-rs issue new`).
        /// Resolved relative to `--repo` when not absolute. Stores `issue_id` as
        /// `local-NNNN` for duplicate detection with `--force`.
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with_all = ["task", "issue"],
            required_unless_present_any = ["task", "issue"]
        )]
        local_issue: Option<PathBuf>,

        /// Path to the git repo. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Default branch of the repo, used as the worktree base.
        #[arg(long, default_value = "main")]
        default_branch: String,

        /// Project id; namespaces worktrees under `~/.worktrees/<project>/`.
        ///
        /// Defaults to the current repo directory name.
        #[arg(long)]
        project: Option<String>,

        /// Skip sending the initial prompt (useful when `claude` isn't installed).
        #[arg(long)]
        no_prompt: bool,

        /// Spawn even if another active session is already working on the
        /// same GitHub issue or local issue id. Without this flag, duplicates are rejected.
        #[arg(long)]
        force: bool,

        /// Agent plugin to use (overrides `projects.*.worker.agent` / `defaults.worker.agent` / `defaults.agent`).
        /// Supported: `claude-code`, `cursor`, `aider`, `codex`.
        #[arg(long)]
        agent: Option<String>,

        /// Runtime plugin to use (overrides `defaults.runtime` in `ao-rs.yaml`).
        /// Supported: `tmux`, `process`.
        #[arg(long)]
        runtime: Option<String>,

        /// Optional spawn template to append to the initial prompt.
        ///
        /// Built-ins:
        /// - `bugfix`
        /// - `feature`
        /// - `refactor`
        /// - `docs`
        /// - `test`
        #[arg(long)]
        template: Option<String>,
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
        ///
        /// Defaults to the current repo directory name.
        #[arg(long)]
        project: Option<String>,

        /// Skip sending the initial prompt (useful when `claude` isn't installed).
        #[arg(long)]
        no_prompt: bool,

        /// Spawn even if another active session is already working on the
        /// same issue.
        #[arg(long)]
        force: bool,

        /// Agent plugin to use (overrides `projects.*.worker.agent` / `defaults.worker.agent` / `defaults.agent`).
        /// Supported: `claude-code`, `cursor`, `aider`, `codex`.
        #[arg(long)]
        agent: Option<String>,

        /// Runtime plugin to use (overrides `defaults.runtime` in `ao-rs.yaml`).
        /// Supported: `tmux`, `process`.
        #[arg(long)]
        runtime: Option<String>,

        /// Optional spawn template to append to each session's initial prompt.
        #[arg(long)]
        template: Option<String>,
    },

    /// List active sessions, newest first.
    ///
    /// Killed/terminated sessions are hidden by default. Use `--all` to
    /// include them.
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

        /// Include killed/terminated sessions in the output.
        #[arg(long)]
        all: bool,
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
        /// Polling interval in seconds. When omitted, uses `poll_interval` from `ao-rs.yaml` (default 10).
        #[arg(long)]
        interval: Option<u64>,
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

        /// Lifecycle polling interval in seconds. When omitted, uses `poll_interval` from `ao-rs.yaml` (default 10).
        #[arg(long)]
        interval: Option<u64>,

        /// Open the dashboard root URL in the default browser after a short delay.
        #[arg(long)]
        open: bool,
    },

    /// Open dashboard or session targets in your browser / file manager.
    ///
    /// Defaults to opening the dashboard root URL.
    Open {
        /// Port where the dashboard is expected to be reachable.
        #[arg(long, default_value_t = 3000)]
        port: u16,

        /// Prefer opening a new window (best-effort; platform-dependent).
        #[arg(short = 'w', long = "new-window")]
        new_window: bool,

        /// What to open. Defaults to `dashboard`.
        #[command(subcommand)]
        target: Option<OpenTarget>,
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

    /// Check that required tools and environment are healthy.
    ///
    /// Verifies: `git`, `gh`, `tmux`, `claude` on PATH; `gh auth status`;
    /// config file loads; sessions directory exists. Reports PASS / WARN /
    /// FAIL per check.
    Doctor,

    /// Scan active sessions' PRs for new review comments.
    ///
    /// For each non-terminal session that has a PR, fetches pending comments
    /// via the SCM plugin. If new comments are found (compared to the last
    /// check), sends a fix prompt to the agent. Use `--dry-run` to preview
    /// without sending.
    ReviewCheck {
        /// Filter to a single project.
        #[arg(long)]
        project: Option<String>,

        /// Show what would be sent without actually messaging agents.
        #[arg(long)]
        dry_run: bool,
    },

    /// Session management subcommands.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },

    /// Lightweight local issue helper (non-GitHub workflows).
    ///
    /// Creates markdown files under `docs/issues/` inside the repo.
    Issue {
        #[command(subcommand)]
        action: IssueAction,
    },
}

#[derive(Subcommand)]
pub enum SessionAction {
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

#[derive(Subcommand, Clone, Debug)]
pub enum OpenTarget {
    /// Open the dashboard root URL.
    Dashboard,
    /// Open a session: dashboard detail URL if available, otherwise the workspace folder.
    Session {
        /// Session uuid or unambiguous prefix (e.g. an 8-char short id).
        id: String,
    },
}

#[derive(Subcommand)]
pub enum IssueAction {
    /// Create a new local issue markdown file under `docs/issues/`.
    New {
        /// Issue title (used for filename + heading).
        #[arg(long)]
        title: String,

        /// Optional body text (written below the title).
        #[arg(long)]
        body: Option<String>,

        /// Repo root (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },

    /// List local issues (`NNNN-*.md` under `docs/issues/`), newest id last.
    List {
        /// Repo root (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },

    /// Print a local issue file to stdout.
    ///
    /// `TARGET` is either a path (relative to `--repo` if not absolute), or a
    /// numeric id (`1`, `01`, `0001`) matching `docs/issues/0001-*.md`.
    Show {
        /// Path to `.md` or short id (digits only, max 4 for the `NNNN-` scheme).
        target: String,

        /// Repo root (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}
