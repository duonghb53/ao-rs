use crate::{
    error::Result,
    prompt_builder,
    scm::{
        CheckRun, CiStatus, Issue, MergeMethod, MergeReadiness, PrState, PullRequest, Review,
        ReviewComment, ReviewDecision,
    },
    scm_transitions::ScmObservation,
    types::{ActivityState, CostEstimate, Session, WorkspaceCreateConfig},
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How an agent process is executed (tmux, raw process, docker, ...).
///
/// The runtime returns an opaque `handle` string that the caller stores in
/// `Session::runtime_handle` and passes back to other methods.
#[async_trait]
pub trait Runtime: Send + Sync {
    /// Spawn a new isolated execution context (e.g. tmux session) and run the
    /// given launch command in it. `launch_command` is a single shell string
    /// — the runtime is responsible for any escaping/wrapping it needs.
    async fn create(
        &self,
        session_id: &str,
        cwd: &Path,
        launch_command: &str,
        env: &[(String, String)],
    ) -> Result<String>;

    async fn send_message(&self, handle: &str, msg: &str) -> Result<()>;
    async fn is_alive(&self, handle: &str) -> Result<bool>;
    async fn destroy(&self, handle: &str) -> Result<()>;
}

/// How a session's working directory is materialized (git worktree, full clone, ...).
#[async_trait]
pub trait Workspace: Send + Sync {
    /// Create an isolated copy of the repo on a new branch, returning its path.
    async fn create(&self, cfg: &WorkspaceCreateConfig) -> Result<PathBuf>;
    async fn destroy(&self, workspace_path: &Path) -> Result<()>;
}

/// A specific AI coding tool (Claude Code, Codex, Aider, Cursor, ...).
///
/// Mostly a metadata provider (launch command, env, prompt), plus one async
/// hook — `detect_activity` — which the lifecycle loop polls to learn what
/// the underlying agent process is currently doing. The TS reference does
/// this by tailing a JSONL log file the agent writes; Slice 1 Phase C's
/// stub just returns `Ready` so the polling loop has something to drive.
#[async_trait]
pub trait Agent: Send + Sync {
    /// Single shell string the runtime will run inside its execution context.
    fn launch_command(&self, session: &Session) -> String;
    fn environment(&self, session: &Session) -> Vec<(String, String)>;
    /// First prompt to deliver after the process is up.
    fn initial_prompt(&self, session: &Session) -> String;

    /// Inspect whatever evidence this agent leaves behind (log files,
    /// terminal scrollback, pid probes, ...) and report its current
    /// activity state. Called once per lifecycle tick.
    ///
    /// A default impl returns `Ready` so plugins can opt in gradually —
    /// matches the TS "no detection available" fallback.
    async fn detect_activity(&self, _session: &Session) -> Result<ActivityState> {
        Ok(ActivityState::Ready)
    }

    /// Poll current aggregated token usage / cost from the agent's logs.
    ///
    /// Called by the lifecycle loop when a session's status changes (not
    /// every tick). Returns `None` when cost tracking is unavailable or
    /// the session has no log data yet. The default impl returns `None`
    /// so agents that don't track cost just work.
    async fn cost_estimate(&self, _session: &Session) -> Result<Option<CostEstimate>> {
        Ok(None)
    }
}

/// Source-code-management plugin — PR lifecycle, CI, reviews.
///
/// Slice 2's richest plugin slot. Mirrors the TS `SCM` interface in
/// `packages/core/src/types.ts` (line ~577), trimmed to the surface the
/// reaction engine actually needs:
///
/// - PR discovery (`detect_pr`) is called once per session per tick.
/// - CI + review summaries drive status transitions inside
///   `LifecycleManager::poll_one` (e.g. `working → ci_failed`).
/// - `pending_comments` feeds the `changes-requested` reaction.
/// - `mergeability` + `merge` implement the `approved-and-green` flow.
///
/// Deliberately left out vs TS: webhook verification, GraphQL batch
/// enrichment, automated-bot-comment fetch, PR check-out helper,
/// per-session `project: ProjectConfig` plumbing. Most of those are
/// optimisations or features the Rust port doesn't need for a hobby
/// orchestrator; the missing ones are noted in `docs/reactions.md`.
#[async_trait]
pub trait Scm: Send + Sync {
    /// Human-readable plugin name for logs and CLI output (`"github"`).
    fn name(&self) -> &str;

    /// Look up the open PR for a session, usually by branch name.
    /// `None` means "no PR yet" — the lifecycle loop stays in `working`
    /// until one appears.
    async fn detect_pr(&self, session: &Session) -> Result<Option<PullRequest>>;

    /// Current open/merged/closed state of the PR.
    async fn pr_state(&self, pr: &PullRequest) -> Result<PrState>;

    /// Individual CI check results. Used by the reaction engine to
    /// format a useful `ci-failed` message with which checks broke.
    async fn ci_checks(&self, pr: &PullRequest) -> Result<Vec<CheckRun>>;

    /// Rolled-up CI status (pending/passing/failing/none).
    async fn ci_status(&self, pr: &PullRequest) -> Result<CiStatus>;

    /// All reviews on the PR (human + bot).
    async fn reviews(&self, pr: &PullRequest) -> Result<Vec<Review>>;

    /// Overall review decision, as GitHub shows it on the PR header.
    async fn review_decision(&self, pr: &PullRequest) -> Result<ReviewDecision>;

    /// Unresolved review comments — forwarded verbatim to the agent by
    /// the `changes-requested` reaction.
    async fn pending_comments(&self, pr: &PullRequest) -> Result<Vec<ReviewComment>>;

    /// Can the PR be merged right now, and if not, why?
    async fn mergeability(&self, pr: &PullRequest) -> Result<MergeReadiness>;

    /// Merge the PR. Called by the `approved-and-green` reaction and by
    /// `ao-rs merge <id>`. `None` lets the plugin pick its default method.
    async fn merge(&self, pr: &PullRequest, method: Option<MergeMethod>) -> Result<()>;

    /// Batch-enrich multiple PRs in a single API round-trip.
    ///
    /// Returns a map keyed by `"{owner}/{repo}#{number}"`. The lifecycle
    /// loop calls this once per tick before iterating sessions; individual
    /// `poll_scm` calls skip their REST fan-out when the cache has a hit.
    ///
    /// Default: empty map (no batch support). Plugins that implement
    /// GraphQL batch enrichment (e.g. GitHub) override this.
    async fn enrich_prs_batch(
        &self,
        _prs: &[PullRequest],
    ) -> Result<HashMap<String, ScmObservation>> {
        Ok(HashMap::new())
    }
}

/// Issue/task tracker plugin — GitHub Issues, Linear, Jira, ...
///
/// Much thinner than `Scm`. The reaction engine doesn't consume tracker
/// data directly yet; `Tracker` is wired in so `ao-rs spawn --issue` can
/// pull issue metadata and derive a sensible branch name / initial prompt.
///
/// Differences from TS `Tracker`:
///
/// - No `project: ProjectConfig` parameter on every method. The plugin
///   holds any project config it needs via `::new()`, matching how
///   `Runtime` / `Agent` already work.
/// - `list_issues`, `update_issue`, `create_issue` are cut. The port
///   needs exactly `get_issue` + `branch_name` + `generate_prompt` today;
///   the rest can come back when a real use case demands them.
#[async_trait]
pub trait Tracker: Send + Sync {
    /// Human-readable plugin name for logs (`"github"`, `"linear"`, ...).
    fn name(&self) -> &str;

    /// Fetch an issue by identifier. `identifier` is whatever the user
    /// types on the CLI — `#42`, `LIN-1327`, or a full URL. The plugin
    /// is responsible for understanding its own conventions.
    async fn get_issue(&self, identifier: &str) -> Result<Issue>;

    /// `true` if the issue is closed / completed / cancelled. Used by
    /// `ao-rs status` filtering and by future reactions that might
    /// auto-close an issue when the PR merges.
    async fn is_completed(&self, identifier: &str) -> Result<bool>;

    /// Canonical URL for the issue. Synchronous because it's usually
    /// string concatenation — no network needed.
    fn issue_url(&self, identifier: &str) -> String;

    /// Suggested git branch name for a new session on this issue. The
    /// plugin decides the format (`issue-42-add-dark-mode`, `LIN-1327`,
    /// etc.); `ao-rs spawn` prepends its own short-id prefix if needed.
    fn branch_name(&self, identifier: &str) -> String;

    /// Format an issue into a structured prompt section suitable for
    /// inclusion in the agent's initial message.
    ///
    /// Default impl uses `prompt_builder::format_issue_context` which
    /// renders title, URL, labels, assignee, and description. Override
    /// in tracker plugins that need platform-specific context (e.g.
    /// Linear cycle info, Jira sprint fields).
    fn generate_prompt(&self, issue: &Issue) -> String {
        prompt_builder::format_issue_context(issue)
    }
}
