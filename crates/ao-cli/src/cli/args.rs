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

        /// Don't start the dashboard HTTP server.
        ///
        /// Implies starting the orchestrator (lifecycle loop) after ensuring
        /// `ao-rs.yaml` exists.
        #[arg(long, conflicts_with = "no_orchestrator")]
        no_dashboard: bool,

        /// Don't start the orchestrator (lifecycle loop).
        ///
        /// Implies starting the dashboard HTTP server after ensuring `ao-rs.yaml`
        /// exists. In this mode the dashboard is "read-only" (no lifecycle events).
        #[arg(long, conflicts_with = "no_dashboard")]
        no_orchestrator: bool,

        /// Port to listen on when `--run` is set.
        #[arg(long, default_value_t = 3000)]
        port: u16,

        /// Lifecycle polling interval in seconds when `--run` is set. When omitted, uses `poll_interval` from `ao-rs.yaml` (default 10).
        #[arg(long)]
        interval: Option<u64>,

        /// Open the dashboard root URL in the default browser (requires `--run`).
        #[arg(long)]
        open: bool,

        /// Re-generate `ao-rs.yaml` even if it already exists (overwrites).
        ///
        /// Also re-runs skill installation. Use `--interactive` to confirm before overwriting.
        #[arg(long)]
        rebuild: bool,

        /// Enable verbose debug logging for this invocation (unless `RUST_LOG` is already set).
        #[arg(long)]
        dev: bool,

        /// Prompt before destructive actions (currently only affects `--rebuild`).
        #[arg(long)]
        interactive: bool,
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

        /// Override the generated initial prompt text.
        ///
        /// When set, this string is used verbatim as the first prompt delivered to the agent
        /// (instead of composing from issue/task/template context).
        #[arg(long)]
        prompt: Option<String>,

        /// Record an existing PR as the one this session is working on.
        ///
        /// Accepts a PR number (e.g. `123` / `#123`) or a PR URL.
        #[arg(long, value_name = "PR")]
        claim_pr: Option<String>,

        /// Assign the spawned issue/PR to the current user on GitHub (best-effort).
        ///
        /// Only supported when the tracker is GitHub and `gh` is authenticated.
        #[arg(long)]
        assign_on_github: bool,

        /// After spawning, automatically attach/open the session (best-effort).
        ///
        /// For the default tmux runtime, this attaches to the new tmux session.
        #[arg(long)]
        open: bool,

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

        /// Output machine-readable JSON to stdout (array of sessions).
        #[arg(long)]
        json: bool,

        /// Re-print status snapshots until interrupted (Ctrl-C).
        #[arg(long)]
        watch: bool,

        /// Polling interval in seconds when `--watch` is set.
        #[arg(long, default_value_t = 2, requires = "watch")]
        interval: u64,
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
        /// Message words to deliver; multiple words are joined with a space.
        /// Required unless `--file` is provided.
        #[arg(required_unless_present = "file")]
        message: Vec<String>,
        /// Send the contents of a file as the message body.
        ///
        /// When combined with inline words, the file content is appended after
        /// the inline text (separated by a newline).
        #[arg(short, long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Skip waiting for the session to become idle before sending.
        ///
        /// Accepted for parity with ao-ts; idle detection is not yet implemented
        /// in the Rust runtime so this flag is currently a no-op.
        #[arg(long)]
        no_wait: bool,
        /// Maximum seconds to allow for the send operation before timing out.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
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

    /// Stop the background lifecycle service (`watch` / `dashboard`).
    ///
    /// Reads `~/.ao-rs/lifecycle.pid`, sends SIGTERM to the owning process,
    /// waits briefly, and removes the pidfile if it is stale.
    Stop {
        /// Stop all supervisor-managed services (reserved for future expansion).
        #[arg(long)]
        all: bool,

        /// Purge supervisor-managed state (reserved for future expansion).
        #[arg(long)]
        purge_session: bool,
    },

    /// Kill a running session: stop the runtime, remove the worktree,
    /// and archive the session file.
    ///
    /// Safe to run on already-terminal sessions — they get archived without
    /// touching the (already gone) runtime.
    Kill {
        /// Session uuid or unambiguous prefix (e.g. an 8-char short id).
        session: String,
        /// Delete the persisted session record instead of moving it to `.archive/`.
        ///
        /// Destructive: there will be no YAML left for this session under the
        /// sessions directory ao-rs uses (`~/.ao-rs/sessions/` by default).
        #[arg(long)]
        purge_session: bool,
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

    /// Update the `ao-rs` CLI.
    ///
    /// `--check` compares the current version against the latest GitHub release tag.
    /// Without `--check`, attempts to upgrade using a supported install method.
    Update {
        /// Compare current version vs latest release, but don't upgrade.
        #[arg(long)]
        check: bool,

        /// Skip smoke test instructions after upgrading.
        #[arg(long)]
        skip_smoke: bool,

        /// Only print smoke test instructions (no check, no upgrade).
        #[arg(long, conflicts_with = "check")]
        smoke_only: bool,
    },

    /// Check that required tools and environment are healthy.
    ///
    /// Verifies: `git`, `gh`, `tmux`, `claude` on PATH; `gh auth status`;
    /// config file loads; sessions directory exists. Reports PASS / WARN /
    /// FAIL per check.
    Doctor {
        /// Apply safe, idempotent fixes (create missing `~/.ao-rs`
        /// directories, suggest `ao-rs start` when the config is missing).
        ///
        /// Never overwrites existing files and never touches repo state.
        #[arg(long)]
        fix: bool,

        /// Send a test notification through every configured notifier for
        /// each priority (`urgent`, `action`, `warning`, `info`).
        ///
        /// Uses the same registry the lifecycle loop builds, so Slack /
        /// Discord / ntfy will receive real messages when configured.
        #[arg(long)]
        test_notify: bool,
    },

    /// Print a concise guide to configuring `ao-rs`.
    ///
    /// Includes config discovery rules, common keys, the example config file,
    /// and links to the full docs.
    ConfigHelp,

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

    /// Verify issue/session completion (minimal parity with ao-ts).
    ///
    /// Rules (read-only):
    /// - A session exists for the target issue, and
    /// - At least one matching session is in a terminal success state (`merged` or `done`).
    ///
    /// Use `--list` to show candidates without any network calls.
    Verify {
        /// List verify targets (issues with at least one session in `merged`/`done`).
        #[arg(long)]
        list: bool,

        /// Exit non-zero when verification fails.
        #[arg(long)]
        fail: bool,

        /// Optional comment to attach on success/failure (not yet implemented; prints a note).
        #[arg(long)]
        comment: Option<String>,

        /// Issue id (e.g. `81` or `#81` or `local-0001`) or session id/prefix.
        #[arg(required_unless_present = "list")]
        target: Option<String>,
    },

    /// Plugin management (crate-based registry).
    ///
    /// Lists plugins compiled into this `ao-rs` binary and shows
    /// selection/config hints.
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
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

    /// Guided setup helpers (parity with `ao setup ...` in ao-ts).
    Setup {
        #[command(subcommand)]
        action: SetupAction,
    },
}

#[derive(Subcommand)]
pub enum PluginAction {
    /// List compiled-in plugins grouped by slot.
    List,

    /// Show config keys and env vars for a plugin name.
    Info {
        /// Plugin name (e.g. `claude-code`, `tmux`, `github`, `stdout`).
        name: String,
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

    /// Bind an existing PR to a session so `ao pr` can resolve it even if
    /// branch detection fails.
    ///
    /// Accepts a PR number (`123`, `#123`) or a full GitHub pull-request URL.
    /// The session defaults to the value of `$AO_SESSION_NAME` / `$AO_SESSION`
    /// if omitted, falling back to the most-recently-created active session.
    ClaimPr {
        /// PR number (`123`, `#123`) or full GitHub URL.
        pr: String,

        /// Session uuid or unambiguous prefix (defaults to most recent).
        session: Option<String>,

        /// Assign the PR to the current GitHub user via `gh`.
        #[arg(long, default_value_t = false)]
        assign_on_github: bool,
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

#[derive(Subcommand)]
pub enum SetupAction {
    /// Configure the Openclaw notifier (ntfy-compatible) and routing presets.
    Openclaw {
        /// Repo root to write `ao-rs.yaml` into (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Openclaw server base URL (maps to ntfy base URL).
        ///
        /// If omitted, uses `AO_OPENCLAW_URL` or `AO_NTFY_URL` when present,
        /// otherwise defaults to `https://ntfy.sh`.
        #[arg(long)]
        url: Option<String>,

        /// Openclaw token (maps to ntfy topic).
        ///
        /// If omitted, uses `AO_OPENCLAW_TOKEN` or `AO_NTFY_TOPIC` when present.
        #[arg(long)]
        token: Option<String>,

        /// Routing preset for which priorities should send to Openclaw/ntfy.
        ///
        /// - `urgent-only`: urgent → stdout+ntfy, others → stdout
        /// - `urgent-action`: urgent+action → stdout+ntfy, others → stdout
        /// - `all`: all priorities → stdout+ntfy
        #[arg(long, default_value = "urgent-action")]
        routing_preset: String,

        /// Fail instead of prompting; requires values from flags or env vars.
        #[arg(long)]
        non_interactive: bool,

        /// Print the updated YAML to stdout, but don't write any files.
        #[arg(long)]
        dry_run: bool,
    },
}
