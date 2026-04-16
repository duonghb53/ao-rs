//! GitHub SCM plugin — shells out to the `gh` CLI.
//!
//! Mirrors `packages/plugins/scm-github/src/index.ts` in the reference repo,
//! Rustified per ao-rs's shell-out philosophy: every method invokes `gh`
//! (or `git`) as a subprocess, parses the JSON output, and maps onto the
//! domain types in `ao-core::scm`. See `docs/architecture.md` for why we
//! pick `gh` over a native GitHub client.
//!
//! ## Why `gh` and not octocrab / a handwritten API client
//!
//! - Users who have `gh` already have auth configured — we inherit `gh`'s
//!   keyring/env handling for free.
//! - `gh` smooths over GitHub/GHES differences (self-hosted, enterprise).
//! - Zero added build-time dependencies on `reqwest`/`hyper`/etc.
//! - The TS reference takes the same approach; shelling out means the two
//!   ports agree byte-for-byte on which PR fields they read.
//!
//! ## Scoping to a repo
//!
//! Slice 1's `Session` doesn't carry an owner/repo tuple — the plugin
//! derives one from `session.workspace_path` by running
//! `git -C <path> remote get-url origin` and parsing the remote URL. That
//! keeps `Session` SCM-agnostic (no hardcoded GitHub fields on disk) and
//! lets the plugin discover project scope the same way a human would.
//!
//! ## GraphQL batch enrichment
//!
//! The `graphql_batch` module implements the 2-Guard ETag + GraphQL batch
//! strategy from the TS reference. The lifecycle loop calls
//! `enrich_prs_batch()` once per tick to pre-populate a cache, then
//! individual `poll_scm` calls skip their 4× REST fan-out when the cache
//! has a hit. See `graphql_batch.rs` for details.
//!
//! ## What's intentionally *not* here
//!
//! - **Webhooks** — requires a long-running HTTP server; the polling
//!   lifecycle loop doesn't need them.
//! - **Automated-bot-comment severity classifier** — that's a reaction-
//!   engine concern (`bugbot-comments` reaction), not an SCM-plugin one.

use ao_core::{
    AoError, CheckRun, CiStatus, MergeMethod, MergeReadiness, PrState, PullRequest, Result, Review,
    ReviewComment, ReviewDecision, Scm, ScmObservation, Session,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::process::Command;

pub mod graphql_batch;
pub(crate) mod parse;

fn is_no_checks_reported_error(msg: &str) -> bool {
    msg.to_lowercase().contains("no checks reported")
}

fn is_not_found_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("not found") || m.contains("404")
}

/// Per-subprocess timeout. Mirrors `DEFAULT_TIMEOUT_MS = 30_000` in the
/// TS reference's `execCli` helper. `gh pr checks` on a large monorepo can
/// easily take 5–10s; 30s is the "the network is wedged, kill it" bound,
/// not the expected latency.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Short-lived caches (reduce GitHub API fan-out)
// ---------------------------------------------------------------------------

const PENDING_COMMENTS_TTL: Duration = Duration::from_secs(30);
const PENDING_COMMENTS_CACHE_MAX: usize = 128;
const GITHUB_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(120);

type PendingCommentsCacheMap = HashMap<String, (Instant, Vec<ReviewComment>)>;
type PendingCommentsCacheLock = Mutex<PendingCommentsCacheMap>;

fn is_rate_limited_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("api rate limit")
        || m.contains("secondary rate limit")
        || m.contains("rate limit exceeded")
        || m.contains("graphql: api rate limit")
}

fn cooldown_until() -> &'static Mutex<Option<Instant>> {
    static COOLDOWN: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    COOLDOWN.get_or_init(|| Mutex::new(None))
}

fn in_cooldown_now() -> bool {
    let Ok(guard) = cooldown_until().lock() else {
        return false;
    };
    guard.is_some_and(|until| Instant::now() < until)
}

fn enter_cooldown() {
    if let Ok(mut guard) = cooldown_until().lock() {
        *guard = Some(Instant::now() + GITHUB_RATE_LIMIT_COOLDOWN);
    }
}

fn pending_comments_cache() -> &'static PendingCommentsCacheLock {
    static CACHE: OnceLock<PendingCommentsCacheLock> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_comments_cache_key(pr: &PullRequest) -> String {
    format!("{}/{}/{}", pr.owner, pr.repo, pr.number)
}

// ---------------------------------------------------------------------------
// Plugin type
// ---------------------------------------------------------------------------

/// Stateless GitHub SCM plugin. Constructed once per process and shared via
/// `Arc<dyn Scm>` — no config, no auth tokens in memory (we rely on `gh`'s
/// own keyring).
#[derive(Debug, Default, Clone, Copy)]
pub struct GitHubScm;

impl GitHubScm {
    pub fn new() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Scm impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Scm for GitHubScm {
    fn name(&self) -> &str {
        "github"
    }

    async fn detect_pr(&self, session: &Session) -> Result<Option<PullRequest>> {
        // `detect_pr` is polling-tolerant by design: every failure mode —
        // missing workspace, no github remote, gh offline, transient API
        // error — collapses to `Ok(None)`. The lifecycle loop calls this
        // every tick, and a flaky network shouldn't flip a session to
        // `Errored`. All *other* methods on this trait assume a valid
        // `PullRequest` and propagate errors normally; the asymmetry is
        // intentional, not a bug to "fix".
        let Some(workspace) = session.workspace_path.as_deref() else {
            return Ok(None);
        };
        let (owner, repo) = match discover_origin(workspace).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::debug!("detect_pr: no github origin in {:?}: {e}", workspace);
                return Ok(None);
            }
        };
        let repo_flag = format!("{owner}/{repo}");

        let json = match gh(&[
            "pr",
            "list",
            "--repo",
            &repo_flag,
            // Default is `open` only; include merged/closed so dashboard PR enrichment
            // can still link a session after its PR has been merged.
            "--state",
            "all",
            "--head",
            &session.branch,
            "--json",
            "number,url,title,headRefName,baseRefName,isDraft",
            "--limit",
            "1",
        ])
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("detect_pr: gh pr list failed: {e}");
                return Ok(None);
            }
        };
        parse::parse_pr_list(&json, &owner, &repo)
    }

    async fn pr_state(&self, pr: &PullRequest) -> Result<PrState> {
        let json = gh(&[
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &repo_flag(pr),
            "--json",
            "state",
        ])
        .await?;
        parse::parse_pr_state(&json)
    }

    async fn ci_checks(&self, pr: &PullRequest) -> Result<Vec<CheckRun>> {
        match gh(&[
            "pr",
            "checks",
            &pr.number.to_string(),
            "--repo",
            &repo_flag(pr),
            "--json",
            "name,state,link,startedAt,completedAt",
        ])
        .await
        {
            Ok(json) => parse::parse_ci_checks(&json),
            Err(e) => {
                // Fallback for repos that publish commit statuses but not
                // check runs. `gh pr checks` reports "no checks reported"
                // for those repos, but CI *does* exist (as statuses).
                if is_no_checks_reported_error(&e.to_string()) {
                    return self.commit_status_checks(pr).await;
                }
                Err(e)
            }
        }
    }

    async fn ci_status(&self, pr: &PullRequest) -> Result<CiStatus> {
        // Mirrors TS `getCISummary` — fetch checks, fold, and handle the
        // "gh errored out" case carefully:
        //
        // - Merged/closed PR → `None`. GitHub often drops check data for
        //   non-open PRs and reporting "failing" on a merged PR is wrong.
        // - Open PR, `gh` errored → `Pending`. A transient API hiccup
        //   shouldn't flip a session into the `ci-failed` reaction path
        //   and spam the agent; the next poll tick will retry. `Failing`
        //   is reserved for "we parsed the checks and at least one was
        //   `Failed`".
        let checks = match self.ci_checks(pr).await {
            Ok(c) => c,
            Err(e) => {
                if let Ok(state) = self.pr_state(pr).await {
                    if matches!(state, PrState::Merged | PrState::Closed) {
                        return Ok(CiStatus::None);
                    }
                }
                // If `gh pr checks` failed with "no checks reported", we may
                // still have commit statuses. `ci_checks()` already tries
                // the fallback, so reaching this branch means either:
                // - the fallback also failed, or
                // - a different error occurred.
                tracing::warn!(
                    "ci_status: gh checks failed for PR #{} (reporting pending): {e}",
                    pr.number
                );
                return Ok(CiStatus::Pending);
            }
        };
        Ok(parse::summarize_ci(&checks))
    }

    async fn reviews(&self, pr: &PullRequest) -> Result<Vec<Review>> {
        let json = gh(&[
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &repo_flag(pr),
            "--json",
            "reviews",
        ])
        .await?;
        parse::parse_reviews(&json)
    }

    async fn review_decision(&self, pr: &PullRequest) -> Result<ReviewDecision> {
        let json = gh(&[
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &repo_flag(pr),
            "--json",
            "reviewDecision",
        ])
        .await?;
        parse::parse_review_decision(&json)
    }

    async fn pending_comments(&self, pr: &PullRequest) -> Result<Vec<ReviewComment>> {
        let key = pending_comments_cache_key(pr);
        if let Ok(cache) = pending_comments_cache().lock() {
            if let Some((at, cached)) = cache.get(&key) {
                if at.elapsed() < PENDING_COMMENTS_TTL {
                    return Ok(cached.clone());
                }
            }
        }

        let fetched = match pending_comments_graphql(pr).await {
            Ok(comments) => comments,
            Err(e) => {
                // Keep resilience: GH GraphQL can fail due to auth scope,
                // enterprise quirks, or transient outages. Fall back to the
                // REST endpoint so consumers still get *some* signal (but
                // without resolution status).
                tracing::warn!(
                    "pending_comments: GraphQL reviewThreads failed for PR #{} (falling back to REST): {e}",
                    pr.number
                );
                pending_comments_rest(pr).await?
            }
        };

        if let Ok(mut cache) = pending_comments_cache().lock() {
            if cache.len() >= PENDING_COMMENTS_CACHE_MAX {
                cache.clear();
            }
            cache.insert(key, (Instant::now(), fetched.clone()));
        }

        Ok(fetched)
    }

    async fn mergeability(&self, pr: &PullRequest) -> Result<MergeReadiness> {
        // Merged PRs: GitHub returns `mergeable: null`, which would
        // otherwise surface as "UNKNOWN (GitHub is computing)". Short-
        // circuit so the CLI doesn't print a confusing blocker for a PR
        // that's already merged. Mirrors TS lines 948–961.
        if matches!(self.pr_state(pr).await?, PrState::Merged) {
            return Ok(MergeReadiness {
                mergeable: true,
                ci_passing: true,
                approved: true,
                no_conflicts: true,
                blockers: Vec::new(),
            });
        }

        let json = gh(&[
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &repo_flag(pr),
            "--json",
            "mergeable,reviewDecision,mergeStateStatus,isDraft",
        ])
        .await?;
        let raw = parse::parse_raw_mergeability(&json)?;

        let ci_status = self.ci_status(pr).await?;
        Ok(compose_merge_readiness(raw, ci_status))
    }

    async fn enrich_prs_batch(
        &self,
        prs: &[PullRequest],
    ) -> Result<HashMap<String, ScmObservation>> {
        graphql_batch::enrich_prs_batch(prs).await
    }

    async fn merge(&self, pr: &PullRequest, method: Option<MergeMethod>) -> Result<()> {
        // TS defaults to `squash`; we default to `merge` (see `MergeMethod`
        // doc) because squash rewrites commit history and that's the kind
        // of thing you want a human to opt into.
        let flag = match method.unwrap_or_default() {
            MergeMethod::Merge => "--merge",
            MergeMethod::Squash => "--squash",
            MergeMethod::Rebase => "--rebase",
        };
        let res = gh(&[
            "pr",
            "merge",
            &pr.number.to_string(),
            "--repo",
            &repo_flag(pr),
            flag,
            "--delete-branch",
        ])
        .await;
        if let Err(e) = res {
            return Err(normalize_merge_error(e));
        }
        Ok(())
    }
}

impl GitHubScm {
    async fn pr_head_sha(&self, pr: &PullRequest) -> Result<String> {
        let json = gh(&[
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &repo_flag(pr),
            "--json",
            "headRefOid",
        ])
        .await?;
        parse::parse_head_ref_oid(&json)
    }

    async fn commit_status_checks(&self, pr: &PullRequest) -> Result<Vec<CheckRun>> {
        let sha = self.pr_head_sha(pr).await?;
        let endpoint = format!("repos/{}/{}/commits/{}/status", pr.owner, pr.repo, sha);
        match gh(&["api", "--method", "GET", &endpoint]).await {
            Ok(json) => parse::parse_commit_statuses(&json),
            Err(e) => {
                // Some repos (or permissions) may not expose the statuses
                // endpoint; treat as "no CI signal" rather than hard-erroring
                // into a perpetual Pending state.
                if is_not_found_error(&e.to_string()) {
                    return Ok(Vec::new());
                }
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Merge-readiness composer (pure, testable)
// ---------------------------------------------------------------------------

/// Fold the raw `gh pr view --json mergeable,reviewDecision,...` output
/// plus a pre-computed `CiStatus` into a `MergeReadiness`.
///
/// Pulled out of `mergeability()` so the blocker-assembly logic — which
/// has half a dozen branches and is the most likely to regress — can be
/// unit-tested without any subprocess. Mirrors TS lines 981–1025.
pub(crate) fn compose_merge_readiness(
    raw: parse::RawMergeability,
    ci_status: CiStatus,
) -> MergeReadiness {
    let mut blockers: Vec<String> = Vec::new();

    // CI
    let ci_passing = matches!(ci_status, CiStatus::Passing | CiStatus::None);
    if !ci_passing {
        blockers.push(format!("CI is {}", ci_status_label(ci_status)));
    }

    // Review decision
    let review_decision = raw
        .review_decision
        .as_deref()
        .unwrap_or("")
        .to_ascii_uppercase();
    // TS treats "no review required / no decision" as effectively approved
    // for the purpose of merge readiness so CI-green PRs can reach the
    // `mergeable` session status.
    let approved = review_decision.is_empty() || review_decision == "APPROVED";
    if review_decision == "CHANGES_REQUESTED" {
        blockers.push("Changes requested in review".into());
    } else if review_decision == "REVIEW_REQUIRED" {
        blockers.push("Review required".into());
    }

    // Merge state / conflicts
    let mergeable_raw = raw.mergeable.to_ascii_uppercase();
    let no_conflicts = mergeable_raw == "MERGEABLE";
    if mergeable_raw == "CONFLICTING" {
        blockers.push("Merge conflicts".into());
    } else if mergeable_raw.is_empty() || mergeable_raw == "UNKNOWN" {
        blockers.push("Merge status unknown (GitHub is computing)".into());
    }

    let merge_state = raw.merge_state_status.to_ascii_uppercase();
    match merge_state.as_str() {
        "BEHIND" => blockers.push("Branch is behind base branch".into()),
        "BLOCKED" => blockers.push("Branch protection requirements not satisfied".into()),
        "UNSTABLE" => blockers.push("Required checks are failing".into()),
        "DIRTY" => {
            // This can overlap with `mergeable: CONFLICTING`, but some repos
            // report only mergeStateStatus. Provide an actionable umbrella.
            blockers.push("Merge is blocked (conflicts or failing requirements)".into())
        }
        _ => {}
    }

    if raw.is_draft {
        blockers.push("PR is still a draft".into());
    }

    MergeReadiness {
        mergeable: blockers.is_empty(),
        ci_passing,
        approved,
        no_conflicts,
        blockers,
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

fn normalize_merge_error(e: AoError) -> AoError {
    let msg = e.to_string();
    let lower = msg.to_lowercase();

    // Keep the original detail but make the prefix actionable.
    if lower.contains("protected branch")
        || lower.contains("branch protection")
        || lower.contains("base branch policy")
    {
        return AoError::Scm(format!("merge blocked by branch protection: {msg}"));
    }
    if lower.contains("not mergeable") || lower.contains("cannot be merged") {
        return AoError::Scm(format!("merge blocked (PR not mergeable yet): {msg}"));
    }
    if lower.contains("merge method")
        && (lower.contains("not allowed") || lower.contains("disabled"))
    {
        return AoError::Scm(format!("merge method not allowed for this repo: {msg}"));
    }
    AoError::Scm(format!("merge failed: {msg}"))
}

// ---------------------------------------------------------------------------
// Subprocess helpers
// ---------------------------------------------------------------------------

fn repo_flag(pr: &PullRequest) -> String {
    format!("{}/{}", pr.owner, pr.repo)
}

/// Run `gh <args>` with a timeout, returning stdout as a `String`.
/// Non-zero exit, timeout, or spawn failure → `AoError::Scm(...)` with the
/// stderr suffix (trimmed) so callers get an actionable message.
async fn gh(args: &[&str]) -> Result<String> {
    if in_cooldown_now() {
        return Err(AoError::Scm(
            "GitHub rate-limit cooldown active; skipping gh subprocess".into(),
        ));
    }
    run("gh", args, None).await
}

async fn pending_comments_rest(pr: &PullRequest) -> Result<Vec<ReviewComment>> {
    // REST fallback: loses thread resolution status (`is_resolved` will be
    // `false` for everything). Kept for resilience.
    const PER_PAGE: usize = 100;
    const MAX_PAGES: u32 = 100;
    let mut all = Vec::new();
    for page in 1..=MAX_PAGES {
        let endpoint = format!(
            "repos/{}/{}/pulls/{}/comments?per_page={PER_PAGE}&page={page}",
            pr.owner, pr.repo, pr.number
        );
        let json = gh(&["api", "--method", "GET", &endpoint]).await?;
        let page_comments = parse::parse_review_comments(&json)?;
        let got = page_comments.len();
        all.extend(page_comments);
        if got < PER_PAGE {
            break;
        }
    }
    Ok(all)
}

const REVIEW_THREADS_QUERY: &str = r#"
query ReviewThreads($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          isResolved
          comments(first: 100) {
            nodes {
              id
              databaseId
              body
              url
              path
              line
              originalLine
              position
              originalPosition
              author { login }
            }
          }
        }
      }
    }
  }
}
"#;

async fn pending_comments_graphql(pr: &PullRequest) -> Result<Vec<ReviewComment>> {
    // Paginate review threads so `is_resolved` is accurate even on large PRs.
    // Keep a hard cap as a safety valve against pathological pagination loops.
    const MAX_PAGES: u32 = 100;
    let mut after: Option<String> = None;
    let mut all = Vec::new();

    for _ in 0..MAX_PAGES {
        let mut args: Vec<String> = vec!["api".into(), "graphql".into()];
        args.push("-f".into());
        args.push(format!("owner={}", pr.owner));
        args.push("-f".into());
        args.push(format!("name={}", pr.repo));
        args.push("-F".into());
        args.push(format!("number={}", pr.number));
        if let Some(ref cursor) = after {
            args.push("-f".into());
            args.push(format!("after={cursor}"));
        }
        args.push("-f".into());
        args.push(format!("query={REVIEW_THREADS_QUERY}"));

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let json = gh(&args_refs).await?;
        let page = parse::parse_review_threads_page(&json)?;
        all.extend(page.comments);

        if page.has_next_page {
            // GitHub should provide an endCursor if it claims there's another page.
            // If it doesn't, bail out so the caller can fall back to REST.
            after = page.end_cursor;
            if after.is_none() {
                return Err(AoError::Scm(
                    "GraphQL reviewThreads signaled hasNextPage but omitted endCursor".into(),
                ));
            }
            continue;
        }
        // No next page: clear cursor so we don't trip the MAX_PAGES guard below.
        after = None;
        break;
    }

    if after.is_some() {
        // We hit MAX_PAGES but still had an `after` cursor, implying pagination
        // didn't converge. Treat as an error so callers can fall back to REST.
        return Err(AoError::Scm(format!(
            "GraphQL reviewThreads pagination exceeded max pages ({MAX_PAGES})"
        )));
    }

    Ok(all)
}

/// Run `git -C <cwd> <args>`. Separate from `gh` because `git` is the
/// right tool for workspace-scoped queries (`remote get-url`,
/// `branch --show-current`) and keeps the timeout tuned there too.
async fn git_in(cwd: &Path, args: &[&str]) -> Result<String> {
    run("git", args, Some(cwd)).await
}

async fn run(bin: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    // Strip env vars that can make `gh`'s output non-deterministic or
    // interactive. `GH_PAGER=cat` disables any pager the user has set
    // globally (otherwise `gh` can hang waiting on less). The update
    // notifier has occasionally corrupted JSON output with its banner.
    // Applied unconditionally — doesn't affect `git`, which ignores them.
    cmd.env("GH_PAGER", "cat");
    cmd.env("GH_NO_UPDATE_NOTIFIER", "1");
    cmd.env("NO_COLOR", "1");

    let output = tokio::time::timeout(SUBPROCESS_TIMEOUT, cmd.output())
        .await
        .map_err(|_| AoError::Scm(format!("{bin} {} timed out", args.join(" "))))?
        .map_err(|e| AoError::Scm(format!("{bin} spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_rate_limited_error(stderr.as_ref()) {
            enter_cooldown();
        }
        return Err(AoError::Scm(format!(
            "{bin} {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Discover `(owner, repo)` from the workspace's `origin` remote.
///
/// Accepts GitHub's three canonical URL forms:
///
/// - `https://github.com/owner/repo.git`
/// - `https://github.com/owner/repo`
/// - `git@github.com:owner/repo.git`
///
/// Returns an error for anything that isn't plausibly github.com-shaped.
/// This is the single choke point for "is this session on GitHub?" — every
/// `gh` call flows through `detect_pr`, which uses this.
async fn discover_origin(workspace: &Path) -> Result<(String, String)> {
    let url = git_in(workspace, &["remote", "get-url", "origin"]).await?;
    parse_github_remote(url.trim())
        .ok_or_else(|| AoError::Scm(format!("origin is not a github remote: {url:?}")))
}

/// Extract `(owner, repo)` from a GitHub remote URL. Pulled out as a pure
/// function so the matrix of accepted URL shapes can be unit-tested.
pub(crate) fn parse_github_remote(url: &str) -> Option<(String, String)> {
    // Trim a trailing `.git` once; leave the rest untouched.
    let trimmed = url.strip_suffix(".git").unwrap_or(url);

    // Case 1: https://github.com/owner/repo[/...]
    if let Some(rest) = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
    {
        return split_owner_repo(rest);
    }
    // Case 2: git@github.com:owner/repo
    if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        return split_owner_repo(rest);
    }
    // Case 3: ssh://git@github.com/owner/repo
    if let Some(rest) = trimmed.strip_prefix("ssh://git@github.com/") {
        return split_owner_repo(rest);
    }
    None
}

/// Split an `owner/repo` (possibly with trailing path) into the two parts.
///
/// Strict: rejects anything after `owner/repo/`. A `remote get-url origin`
/// for a GitHub clone is always exactly `owner/repo(.git)?` — if we see
/// `owner/repo/something` the URL is malformed (or a GHE path we don't
/// understand) and falling through to "not a github remote" is safer than
/// silently using the wrong repo.
fn split_owner_repo(rest: &str) -> Option<(String, String)> {
    let mut parts = rest.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    // Any extra non-empty segment means this isn't a bare `owner/repo`.
    // A single trailing slash (`owner/repo/`) is tolerated — that's just
    // a cosmetic artifact. Anything else is rejected.
    if parts.any(|p| !p.is_empty()) {
        return None;
    }
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::CheckStatus;

    #[test]
    fn is_no_checks_reported_error_matches_gh_message() {
        // This matches the `gh pr checks` stderr substring we see in the wild.
        let msg =
            "gh pr checks 26 --repo x/y --json name,state failed: no checks reported on the 'ao-abc' branch";
        assert!(is_no_checks_reported_error(msg));
    }

    #[test]
    fn is_not_found_error_matches_common_gh_api_failures() {
        assert!(is_not_found_error(
            "gh api ... failed: Not Found (HTTP 404)"
        ));
        assert!(is_not_found_error("HTTP 404: Not Found"));
        assert!(!is_not_found_error("HTTP 401: Bad credentials"));
    }

    #[test]
    fn normalize_merge_error_adds_actionable_prefixes() {
        let fixture = include_str!("../tests/fixtures/merge_error_branch_protection.txt");
        let e = AoError::Scm(fixture.trim().to_string());
        let n = normalize_merge_error(e).to_string();
        assert!(n.to_lowercase().contains("branch protection"));

        let e = AoError::Scm("gh pr merge failed: Pull request is not mergeable".into());
        let n = normalize_merge_error(e).to_string();
        assert!(n.to_lowercase().contains("not mergeable"));

        let e = AoError::Scm("gh pr merge failed: Merge method 'rebase' is disabled".into());
        let n = normalize_merge_error(e).to_string();
        assert!(n.to_lowercase().contains("merge method"));
    }

    #[test]
    fn mergeability_blocked_fixture_formats_actionable_blocker() {
        let json = include_str!("../tests/fixtures/mergeability_blocked.json");
        let raw = parse::parse_raw_mergeability(json).unwrap();
        let r = compose_merge_readiness(raw, CiStatus::Passing);
        assert!(!r.is_ready());
        assert!(r
            .blockers
            .iter()
            .any(|b| b.to_lowercase().contains("branch protection")));
    }

    #[test]
    fn parse_github_remote_accepts_https() {
        assert_eq!(
            parse_github_remote("https://github.com/acme/widgets.git"),
            Some(("acme".into(), "widgets".into()))
        );
        assert_eq!(
            parse_github_remote("https://github.com/acme/widgets"),
            Some(("acme".into(), "widgets".into()))
        );
    }

    #[test]
    fn parse_github_remote_accepts_ssh() {
        assert_eq!(
            parse_github_remote("git@github.com:acme/widgets.git"),
            Some(("acme".into(), "widgets".into()))
        );
        assert_eq!(
            parse_github_remote("ssh://git@github.com/acme/widgets.git"),
            Some(("acme".into(), "widgets".into()))
        );
    }

    #[test]
    fn parse_github_remote_rejects_non_github() {
        assert_eq!(parse_github_remote("https://gitlab.com/a/b.git"), None);
        assert_eq!(parse_github_remote("not a url at all"), None);
        assert_eq!(parse_github_remote(""), None);
    }

    #[test]
    fn parse_github_remote_trims_trailing_git_suffix_only_once() {
        // A repo literally named `foo.git` (rare but legal) shouldn't have
        // its name eaten twice. We only strip one `.git`.
        assert_eq!(
            parse_github_remote("https://github.com/acme/foo.git.git"),
            Some(("acme".into(), "foo.git".into()))
        );
    }

    #[test]
    fn compose_merge_readiness_all_green_has_no_blockers() {
        let raw = parse::RawMergeability {
            mergeable: "MERGEABLE".into(),
            review_decision: Some("APPROVED".into()),
            merge_state_status: "CLEAN".into(),
            is_draft: false,
        };
        let r = compose_merge_readiness(raw, CiStatus::Passing);
        assert!(r.is_ready());
        assert!(r.mergeable);
        assert!(r.ci_passing);
        assert!(r.approved);
        assert!(r.no_conflicts);
        assert!(r.blockers.is_empty());
    }

    #[test]
    fn compose_merge_readiness_draft_is_a_blocker() {
        let raw = parse::RawMergeability {
            mergeable: "MERGEABLE".into(),
            review_decision: Some("APPROVED".into()),
            merge_state_status: "CLEAN".into(),
            is_draft: true,
        };
        let r = compose_merge_readiness(raw, CiStatus::Passing);
        assert!(!r.mergeable);
        assert!(r.blockers.iter().any(|b| b.contains("draft")));
    }

    #[test]
    fn compose_merge_readiness_conflicts_flip_no_conflicts() {
        let raw = parse::RawMergeability {
            mergeable: "CONFLICTING".into(),
            review_decision: Some("APPROVED".into()),
            merge_state_status: "DIRTY".into(),
            is_draft: false,
        };
        let r = compose_merge_readiness(raw, CiStatus::Passing);
        assert!(!r.no_conflicts);
        assert!(r.blockers.iter().any(|b| b.contains("conflicts")));
    }

    #[test]
    fn compose_merge_readiness_ci_none_still_passes_ci_gate() {
        // TS special case: "CI is none" is treated as passing (no CI
        // configured is fine — the PR just doesn't have a CI gate). Lock
        // this in so a later refactor doesn't accidentally flip it.
        let raw = parse::RawMergeability {
            mergeable: "MERGEABLE".into(),
            review_decision: Some("APPROVED".into()),
            merge_state_status: "CLEAN".into(),
            is_draft: false,
        };
        let r = compose_merge_readiness(raw, CiStatus::None);
        assert!(r.ci_passing);
        assert!(r.is_ready());
    }

    #[test]
    fn compose_merge_readiness_ci_failing_is_a_blocker() {
        let raw = parse::RawMergeability {
            mergeable: "MERGEABLE".into(),
            review_decision: Some("APPROVED".into()),
            merge_state_status: "CLEAN".into(),
            is_draft: false,
        };
        let r = compose_merge_readiness(raw, CiStatus::Failing);
        assert!(!r.ci_passing);
        assert!(r.blockers.iter().any(|b| b.contains("failing")));
    }

    #[test]
    fn compose_merge_readiness_changes_requested_blocks_approval() {
        let raw = parse::RawMergeability {
            mergeable: "MERGEABLE".into(),
            review_decision: Some("CHANGES_REQUESTED".into()),
            merge_state_status: "BLOCKED".into(),
            is_draft: false,
        };
        let r = compose_merge_readiness(raw, CiStatus::Passing);
        assert!(!r.approved);
        assert!(r.blockers.iter().any(|b| b.contains("Changes requested")));
    }

    #[test]
    fn compose_merge_readiness_unknown_mergeable_blocks_with_message() {
        let raw = parse::RawMergeability {
            mergeable: "UNKNOWN".into(),
            review_decision: Some("APPROVED".into()),
            merge_state_status: "".into(),
            is_draft: false,
        };
        let r = compose_merge_readiness(raw, CiStatus::Passing);
        assert!(!r.mergeable);
        assert!(r
            .blockers
            .iter()
            .any(|b| b.contains("Merge status unknown")));
    }

    #[test]
    fn compose_merge_readiness_empty_mergeable_treated_as_unknown() {
        // `gh` can return an empty string for `mergeable` on fresh PRs.
        // We fold that into the "computing" blocker rather than silently
        // claiming the PR is clean.
        let raw = parse::RawMergeability {
            mergeable: "".into(),
            review_decision: Some("APPROVED".into()),
            merge_state_status: "".into(),
            is_draft: false,
        };
        let r = compose_merge_readiness(raw, CiStatus::Passing);
        assert!(r
            .blockers
            .iter()
            .any(|b| b.contains("Merge status unknown")));
    }

    #[test]
    fn ci_status_labels_match_enum() {
        // Cheap guard: if someone adds a `CiStatus` variant they also have
        // to add a label here, which forces them past `ci_status_label`
        // and into the rest of the fold logic.
        assert_eq!(ci_status_label(CiStatus::Pending), "pending");
        assert_eq!(ci_status_label(CiStatus::Passing), "passing");
        assert_eq!(ci_status_label(CiStatus::Failing), "failing");
        assert_eq!(ci_status_label(CiStatus::None), "none");
    }

    #[test]
    fn github_scm_name_is_github() {
        assert_eq!(GitHubScm::new().name(), "github");
    }

    // Spot-check that the parse module and lib.rs agree on `CheckStatus`
    // — a guard in case `parse` gets moved or `ao-core::CheckStatus` gets
    // renamed and one side updates without the other.
    #[test]
    fn check_status_is_reachable_from_both_modules() {
        let raw = r#"[{"name":"a","state":"SUCCESS","link":""}]"#;
        let checks = parse::parse_ci_checks(raw).unwrap();
        // Use `.first()` rather than `checks[0]` so a regression that
        // returns an empty vec fails with a clear message instead of a
        // panic on a bogus index.
        assert_eq!(checks.first().map(|c| c.status), Some(CheckStatus::Passed));
    }

    #[test]
    fn split_owner_repo_rejects_extra_path_segments() {
        // Regression guard: `github.com/owner/repo/tree/main` or
        // `github.com/enterprise/owner/repo` (wrong shape) must not
        // quietly collapse to `(owner, repo)` and send `gh` commands at
        // the wrong repo. A bare remote URL never has a third segment.
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo/tree/main"),
            None
        );
        assert_eq!(parse_github_remote("git@github.com:owner/repo/extra"), None);
        // Trailing slash is tolerated (cosmetic only).
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo/"),
            Some(("owner".into(), "repo".into()))
        );
    }
}
