//! GitHub Issues tracker plugin — shells out to the `gh` CLI.
//!
//! Mirrors `packages/plugins/tracker-github/src/index.ts`, trimmed to the
//! surface the Rust `Tracker` trait actually needs:
//!
//! - `get_issue` → `gh api repos/{owner}/{repo}/issues/{n}`
//! - `is_completed` → derived from `get_issue` (closed OR cancelled)
//! - `issue_url`, `branch_name` → pure string manipulation
//!
//! What TS has and we deliberately don't:
//!
//! - `generatePrompt` — lives in the CLI or agent plugin, not here. The
//!   tracker's job is issue fetch + URL formatting; prompt composition is
//!   one level up.
//! - `listIssues`/`updateIssue`/`createIssue` — no current use case in the
//!   Rust port. `ao-rs spawn --issue` only needs `get_issue`. Adding write
//!   methods means more shell-out surface and more things that can go
//!   wrong during a poll cycle; we'll revisit when a feature asks for it.
//! - `stateReason` fallback for older `gh` versions — we require a `gh`
//!   recent enough to know the field (roughly >= 2.40, late 2023). Most
//!   users already have it via `brew install gh`; the retry dance
//!   complicates the code for zero Phase C value.
//!
//! ## Scoping
//!
//! The Rust `Tracker` trait doesn't take a `ProjectConfig` parameter on
//! every method (unlike the TS reference), so the plugin carries its
//! scope on the `GitHubTracker` struct itself. `GitHubTracker::new(owner,
//! repo)` constructs a per-project instance; the orchestrator can hold
//! one in its plugin registry per project. This matches how `Runtime`
//! and `Agent` are already wired.

use ao_core::{AoError, Issue, IssueState, Result, Tracker};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Per-subprocess timeout. Same 30s bound as the SCM plugin — this is the
/// "network is wedged" ceiling, not the expected latency.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Rate limit resilience (hotfix #119)
// ---------------------------------------------------------------------------

const ISSUE_STATE_TTL: Duration = Duration::from_secs(30);
const ISSUE_STATE_CACHE_MAX: usize = 256;
const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(120);

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
    let Ok(mut guard) = cooldown_until().lock() else {
        return false;
    };
    if let Some(until) = *guard {
        if Instant::now() < until {
            return true;
        }
        // Cooldown expired — clear it so logs/state reflect recovery.
        *guard = None;
    }
    false
}

fn enter_cooldown() {
    if let Ok(mut guard) = cooldown_until().lock() {
        *guard = Some(Instant::now() + RATE_LIMIT_COOLDOWN);
    }
}

fn issue_state_cache() -> &'static Mutex<HashMap<String, (Instant, IssueState)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, IssueState)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn issue_state_cache_key(owner: &str, repo: &str, number: &str) -> String {
    format!("{owner}/{repo}#{number}")
}

#[derive(Debug, Deserialize)]
struct RawIssueState {
    #[serde(default)]
    state: Option<String>,
    #[serde(default, rename = "state_reason", alias = "stateReason")]
    state_reason: Option<String>,
}

fn parse_issue_state(json: &str) -> Result<IssueState> {
    let raw: RawIssueState =
        serde_json::from_str(json).map_err(|e| AoError::Scm(format!("parse issue state: {e}")))?;
    let state = raw.state.unwrap_or_default();
    if state.trim().is_empty() {
        return Err(AoError::Scm("parse issue state: missing `state`".into()));
    }
    Ok(map_state(&state, raw.state_reason.as_deref()))
}

// ---------------------------------------------------------------------------
// Plugin type
// ---------------------------------------------------------------------------

/// GitHub Issues tracker scoped to a single `owner/repo`. Cheap to clone;
/// share via `Arc<dyn Tracker>`.
#[derive(Debug, Clone)]
pub struct GitHubTracker {
    owner: String,
    repo: String,
}

impl GitHubTracker {
    /// Construct a tracker scoped to one `owner/repo`.
    ///
    /// Note on multi-project evolution: the orchestrator currently holds
    /// a single `Arc<dyn Tracker>` at the plugin slot, which is fine for
    /// the "one user, one project" hobby case this port targets. When
    /// multi-project support lands (likely alongside `ao-rs spawn
    /// --project <name>` gaining a per-project tracker), this struct
    /// becomes one entry in a `HashMap<ProjectId, Arc<dyn Tracker>>` —
    /// no change to the trait, no breaking migration.
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
        }
    }

    /// Auto-detect `(owner, repo)` from the `origin` remote of a local git
    /// repo path.
    ///
    /// Accepts the same GitHub URL forms as the SCM plugin:
    /// `https://github.com/owner/repo[.git]`, `git@github.com:owner/repo[.git]`,
    /// `ssh://git@github.com/owner/repo[.git]`.
    ///
    /// Returns an error if the path is not inside a git repo, has no `origin`
    /// remote, or the remote is not a GitHub URL.
    pub async fn from_repo(repo_path: &Path) -> Result<Self> {
        let mut cmd = Command::new("git");
        cmd.args([
            "-C",
            &repo_path.to_string_lossy(),
            "remote",
            "get-url",
            "origin",
        ]);
        let output = tokio::time::timeout(SUBPROCESS_TIMEOUT, cmd.output())
            .await
            .map_err(|_| AoError::Other("git remote get-url timed out".into()))?
            .map_err(|e| AoError::Other(format!("failed to run git: {e}")))?;
        if !output.status.success() {
            return Err(AoError::Other(
                "no `origin` remote found in git repo".into(),
            ));
        }
        let url = String::from_utf8_lossy(&output.stdout);
        let (owner, repo) = parse_github_remote(url.trim())
            .ok_or_else(|| AoError::Other(format!("origin is not a GitHub remote: {url:?}")))?;
        Ok(Self { owner, repo })
    }

    /// `owner/repo` — the form `gh --repo` expects.
    fn repo_slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

// ---------------------------------------------------------------------------
// Tracker impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Tracker for GitHubTracker {
    fn name(&self) -> &str {
        "github"
    }

    async fn get_issue(&self, identifier: &str) -> Result<Issue> {
        if in_cooldown_now() {
            tracing::debug!(
                "tracker-github: cooldown active; skipping get_issue {} in {}",
                identifier,
                self.repo_slug()
            );
            return Err(AoError::Other(
                "GitHub rate-limit cooldown active; skipping full issue fetch".into(),
            ));
        }
        let number = normalize_identifier(identifier);
        tracing::debug!(
            "tracker-github: get_issue {} in {}",
            number,
            self.repo_slug()
        );
        let json = gh(&[
            "api",
            &format!("repos/{}/{}/issues/{}", self.owner, self.repo, number),
        ])
        .await?;
        parse_issue(&json)
    }

    async fn is_completed(&self, identifier: &str) -> Result<bool> {
        let number = normalize_identifier(identifier);
        let key = issue_state_cache_key(&self.owner, &self.repo, &number);

        if let Ok(cache) = issue_state_cache().lock() {
            if let Some((at, state)) = cache.get(&key) {
                if at.elapsed() < ISSUE_STATE_TTL {
                    tracing::debug!(
                        "tracker-github: is_completed cache hit {} in {}",
                        number,
                        self.repo_slug()
                    );
                    return Ok(matches!(state, IssueState::Closed | IssueState::Cancelled));
                }
            }
        }

        if in_cooldown_now() {
            // Prefer stale cache during cooldown to avoid hammering.
            if let Ok(cache) = issue_state_cache().lock() {
                if let Some((_at, state)) = cache.get(&key) {
                    tracing::debug!(
                        "tracker-github: cooldown active; using stale cached state for {} in {}",
                        number,
                        self.repo_slug()
                    );
                    return Ok(matches!(state, IssueState::Closed | IssueState::Cancelled));
                }
            }
            tracing::debug!(
                "tracker-github: cooldown active; skipping is_completed {} in {}",
                number,
                self.repo_slug()
            );
            return Err(AoError::Other(
                "GitHub rate-limit cooldown active; skipping issue completion check".into(),
            ));
        }

        tracing::debug!(
            "tracker-github: is_completed fetch (minimal) {} in {}",
            number,
            self.repo_slug()
        );
        let json = match gh(&[
            "api",
            &format!("repos/{}/{}/issues/{}", self.owner, self.repo, number),
        ])
        .await
        {
            Ok(j) => j,
            Err(e) => {
                let msg = e.to_string();
                if is_rate_limited_error(&msg) {
                    tracing::warn!(
                        "tracker-github: rate-limited during is_completed; entering cooldown: {e}"
                    );
                    enter_cooldown();
                }
                return Err(e);
            }
        };

        let state = parse_issue_state(&json)?;
        if let Ok(mut cache) = issue_state_cache().lock() {
            if cache.len() >= ISSUE_STATE_CACHE_MAX {
                cache.clear();
            }
            cache.insert(key, (Instant::now(), state));
        }
        Ok(matches!(state, IssueState::Closed | IssueState::Cancelled))
    }

    fn issue_url(&self, identifier: &str) -> String {
        let n = normalize_identifier(identifier);
        format!(
            "https://github.com/{}/{}/issues/{}",
            self.owner, self.repo, n
        )
    }

    fn branch_name(&self, identifier: &str) -> String {
        // Legacy suggestion API from the `Tracker` trait. The CLI spawn flow
        // does not call this — it derives `type/<issue>-<slug>` branches from
        // issue labels/title in `ao-cli`.
        let n = normalize_identifier(identifier);
        format!("feat/issue-{n}")
    }

    async fn comment_issue(&self, identifier: &str, body: &str) -> Result<()> {
        let number = normalize_identifier(identifier);
        // GitHub Issues comment API:
        //   POST /repos/{owner}/{repo}/issues/{issue_number}/comments
        //
        // Use `-f` so the message is passed as a field (handles newlines).
        let _ = gh(&[
            "--repo",
            &self.repo_slug(),
            "api",
            &format!(
                "repos/{}/{}/issues/{}/comments",
                self.owner, self.repo, number
            ),
            "-f",
            &format!("body={body}"),
        ])
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Parse + state mapping (pure, testable)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawIssue {
    number: u32,
    // `title`/`body`/`url`/`state` are wrapped in `Option<String>` even
    // though they're always emitted on success, because `#[serde(default)]`
    // only kicks in when the *field is missing* — not when it's present
    // but `null`. `gh` has historically emitted `"body": null` for issues
    // with no description, and we'd rather collapse a surprise null to
    // `""` at parse time than error the whole polling cycle. The
    // `.unwrap_or_default()` calls in `parse_issue` make the eventual
    // `Issue` fields look the same as before.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    #[serde(rename = "html_url", alias = "url")]
    url: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default, rename = "state_reason", alias = "stateReason")]
    state_reason: Option<String>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    assignees: Vec<RawLogin>,
    #[serde(default)]
    milestone: Option<RawMilestone>,
}

#[derive(Debug, Deserialize)]
struct RawLabel {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct RawLogin {
    #[serde(default)]
    login: String,
}

#[derive(Debug, Deserialize)]
struct RawMilestone {
    #[serde(default)]
    title: String,
}

fn parse_issue(json: &str) -> Result<Issue> {
    let raw: RawIssue =
        serde_json::from_str(json).map_err(|e| AoError::Scm(format!("parse issue: {e}")))?;
    Ok(Issue {
        id: raw.number.to_string(),
        title: raw.title.unwrap_or_default(),
        description: raw.body.unwrap_or_default(),
        url: raw.url.unwrap_or_default(),
        state: map_state(
            raw.state.as_deref().unwrap_or(""),
            raw.state_reason.as_deref(),
        ),
        labels: raw
            .labels
            .into_iter()
            .map(|l| l.name)
            .filter(|s| !s.is_empty())
            .collect(),
        // TS uses only the first assignee — issues can have many but the
        // Rust `Issue` type carries a single Option<String>, matching the
        // single-responsibility assumption the reaction engine works with.
        assignee: raw
            .assignees
            .into_iter()
            .next()
            .map(|a| a.login)
            .filter(|s| !s.is_empty()),
        milestone: raw
            .milestone
            .map(|m| m.title)
            .filter(|s| !s.trim().is_empty()),
    })
}

/// Fold GitHub's `state` + `stateReason` into our four-variant
/// `IssueState`. GitHub never emits `InProgress` for Issues (that's a
/// Projects concept), so this mapping deliberately can't produce it.
fn map_state(state: &str, state_reason: Option<&str>) -> IssueState {
    match state.trim().to_ascii_uppercase().as_str() {
        "CLOSED" => match state_reason
            .map(|s| s.trim().to_ascii_uppercase())
            .as_deref()
        {
            // GitHub's "not planned" corresponds to our "cancelled" — the
            // distinction matters for the reaction engine, which might
            // want to stop polling a cancelled-issue session differently
            // than a merged-PR session.
            Some("NOT_PLANNED") => IssueState::Cancelled,
            _ => IssueState::Closed,
        },
        // Empty or unknown state → treat as open so a surprise from a
        // future `gh` release doesn't mark live issues as closed.
        _ => IssueState::Open,
    }
}

/// Strip a single leading `#` and surrounding whitespace. `#42`, `42`,
/// and ` 42 ` all normalize to `42`. We don't validate that the result
/// is numeric — `gh` will reject bad input with a clear error and that's
/// a better user experience than a silent "invalid identifier" here.
///
/// Uses `strip_prefix` (not `trim_start_matches`) so `##42` becomes `#42`
/// rather than `42` — a typo'd `##` is almost certainly user error we
/// should surface at the `gh` layer, not silently paper over.
fn normalize_identifier(id: &str) -> String {
    let trimmed = id.trim();
    trimmed.strip_prefix('#').unwrap_or(trimmed).to_string()
}

// ---------------------------------------------------------------------------
// GitHub remote URL parser (mirrors scm-github's version)
// ---------------------------------------------------------------------------

/// Extract `(owner, repo)` from a GitHub remote URL.
///
/// Accepted forms:
/// - `https://github.com/owner/repo[.git]`
/// - `git@github.com:owner/repo[.git]`
/// - `ssh://git@github.com/owner/repo[.git]`
fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let trimmed = url.strip_suffix(".git").unwrap_or(url);
    let rest = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("git@github.com:"))
        .or_else(|| trimmed.strip_prefix("ssh://git@github.com/"))?;
    let mut parts = rest.split('/');
    let owner = parts.next()?.trim().to_string();
    let repo = parts.next()?.trim().to_string();
    // Reject any extra non-empty path segments (e.g. `owner/repo/tree/main`).
    // A single trailing slash (`owner/repo/`) is tolerated.
    if parts.any(|s| !s.is_empty()) {
        return None;
    }
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

// ---------------------------------------------------------------------------
// Subprocess helper
// ---------------------------------------------------------------------------

/// Run `gh <args>` with a timeout, returning stdout as a `String`. Same
/// env hardening as the SCM plugin (`GH_PAGER=cat`, etc.) so stdout stays
/// deterministic regardless of the user's shell config.
async fn gh(args: &[&str]) -> Result<String> {
    if in_cooldown_now() {
        return Err(AoError::Scm(
            "GitHub rate-limit cooldown active; skipping gh subprocess".into(),
        ));
    }
    let mut cmd = Command::new("gh");
    cmd.args(args);
    cmd.env("GH_PAGER", "cat");
    cmd.env("GH_NO_UPDATE_NOTIFIER", "1");
    cmd.env("NO_COLOR", "1");

    let output = tokio::time::timeout(SUBPROCESS_TIMEOUT, cmd.output())
        .await
        .map_err(|_| AoError::Scm(format!("gh {} timed out", args.join(" "))))?
        .map_err(|e| AoError::Scm(format!("gh spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_rate_limited_error(stderr.as_ref()) {
            enter_cooldown();
        }
        return Err(AoError::Scm(format!(
            "gh {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- identifier normalization ----------

    #[test]
    fn normalize_identifier_strips_leading_hash() {
        assert_eq!(normalize_identifier("#42"), "42");
        assert_eq!(normalize_identifier("42"), "42");
    }

    #[test]
    fn normalize_identifier_trims_whitespace() {
        assert_eq!(normalize_identifier("  #42  "), "42");
        assert_eq!(normalize_identifier("\t42\n"), "42");
    }

    #[test]
    fn normalize_identifier_only_strips_one_hash() {
        // Defensive: `##42` is almost certainly a typo, but silently
        // eating both `#`s would mask a user error. We strip just one.
        assert_eq!(normalize_identifier("##42"), "#42");
    }

    // ---------- state mapping ----------

    #[test]
    fn map_state_open_ignores_reason() {
        assert_eq!(map_state("OPEN", None), IssueState::Open);
        assert_eq!(map_state("open", Some("REOPENED")), IssueState::Open);
    }

    #[test]
    fn map_state_closed_completed_is_closed() {
        assert_eq!(map_state("CLOSED", Some("COMPLETED")), IssueState::Closed);
        // Closed with no reason at all still maps to Closed — GitHub
        // older than ~2.40 wouldn't surface stateReason, and a missing
        // field should not silently become Cancelled.
        assert_eq!(map_state("CLOSED", None), IssueState::Closed);
    }

    #[test]
    fn map_state_closed_not_planned_is_cancelled() {
        assert_eq!(
            map_state("CLOSED", Some("NOT_PLANNED")),
            IssueState::Cancelled
        );
        // Case-insensitive on the reason.
        assert_eq!(
            map_state("CLOSED", Some("not_planned")),
            IssueState::Cancelled
        );
    }

    #[test]
    fn map_state_is_case_insensitive_on_state_itself() {
        // Case-insensitivity on `state_reason` is covered above; this
        // locks in that `map_state` upper-cases the `state` arg too, so
        // a future `gh` that emits `"closed"` in lowercase can't slip
        // through as `Open` and keep a closed issue forever-polling.
        assert_eq!(map_state("closed", None), IssueState::Closed);
        assert_eq!(
            map_state("Closed", Some("NOT_PLANNED")),
            IssueState::Cancelled
        );
    }

    #[test]
    fn map_state_unknown_state_falls_back_to_open() {
        // A surprise new state from a future gh release should not
        // silently mark live issues as closed. `Open` is the only safe
        // fallback (the reaction engine treats Open as "still work to
        // do", Closed as "terminal" — false-Open is recoverable, false-
        // Closed can cause premature session cleanup).
        assert_eq!(map_state("TRIAGED", None), IssueState::Open);
        assert_eq!(map_state("", None), IssueState::Open);
    }

    // ---------- parse_issue ----------

    #[test]
    fn parse_issue_full_payload() {
        let json = r#"
        {
          "number": 42,
          "title": "add dark mode",
          "body": "users keep asking",
          "url": "https://github.com/acme/widgets/issues/42",
          "state": "OPEN",
          "stateReason": null,
          "labels": [{"name": "feature"}, {"name": "ui"}],
          "assignees": [{"login": "bob"}, {"login": "alice"}],
          "milestone": {"title": "Q2"}
        }
        "#;
        let issue = parse_issue(json).unwrap();
        assert_eq!(issue.id, "42");
        assert_eq!(issue.title, "add dark mode");
        assert_eq!(issue.description, "users keep asking");
        assert_eq!(issue.url, "https://github.com/acme/widgets/issues/42");
        assert_eq!(issue.state, IssueState::Open);
        assert_eq!(issue.labels, vec!["feature", "ui"]);
        // Only the first assignee survives — see `parse_issue` comment.
        assert_eq!(issue.assignee.as_deref(), Some("bob"));
        assert_eq!(issue.milestone.as_deref(), Some("Q2"));
    }

    #[test]
    fn parse_issue_tolerates_null_body_and_title() {
        // `gh` has emitted `"body": null` for bodyless issues in the
        // past, and `#[serde(default)]` on a `String` only catches
        // *missing* fields, not present-but-null. Regression guard for
        // a real polling-loop stall we'd otherwise hit on any issue
        // with no description.
        let json = r#"
        {
          "number": 3,
          "title": null,
          "body": null,
          "url": null,
          "state": "OPEN",
          "stateReason": null,
          "labels": [],
          "assignees": [],
          "milestone": null
        }
        "#;
        let issue = parse_issue(json).unwrap();
        assert_eq!(issue.id, "3");
        assert_eq!(issue.title, "");
        assert_eq!(issue.description, "");
        assert_eq!(issue.url, "");
        assert_eq!(issue.state, IssueState::Open);
        assert_eq!(issue.milestone, None);
    }

    #[test]
    fn parse_issue_rejects_negative_number_cleanly() {
        // `u32` for `number` is a deliberate type choice (matches the
        // SCM plugin's `PullRequest::number`). This test pins it: a
        // switch to `i64` would silently start accepting nonsense ids.
        let json = r#"
        {
          "number": -1, "title": "t", "body": "", "url": "u",
          "state": "OPEN", "labels": [], "assignees": []
        }
        "#;
        let err = parse_issue(json).unwrap_err();
        assert!(format!("{err}").contains("parse issue"));
    }

    #[test]
    fn parse_issue_missing_optional_fields_default_sensibly() {
        // `gh` on an issue with no body / no labels / no assignees
        // returns empty strings and empty arrays. Make sure our
        // deserializer doesn't choke on any of them.
        let json = r#"
        {
          "number": 7,
          "title": "t",
          "body": "",
          "url": "u",
          "state": "OPEN",
          "labels": [],
          "assignees": []
        }
        "#;
        let issue = parse_issue(json).unwrap();
        assert_eq!(issue.id, "7");
        assert_eq!(issue.description, "");
        assert!(issue.labels.is_empty());
        assert_eq!(issue.assignee, None);
        assert_eq!(issue.milestone, None);
    }

    #[test]
    fn parse_issue_cancelled_via_state_reason() {
        let json = r#"
        {
          "number": 99,
          "title": "wontfix",
          "body": "",
          "url": "u",
          "state": "CLOSED",
          "stateReason": "NOT_PLANNED",
          "labels": [],
          "assignees": []
        }
        "#;
        let issue = parse_issue(json).unwrap();
        assert_eq!(issue.state, IssueState::Cancelled);
    }

    #[test]
    fn parse_issue_filters_empty_label_names() {
        // Defensive: a malformed label object `{"name": ""}` shouldn't
        // show up as a visible empty-string label in the CLI. Drop it
        // at parse time.
        let json = r#"
        {
          "number": 1,
          "title": "t",
          "body": "",
          "url": "u",
          "state": "OPEN",
          "labels": [{"name": ""}, {"name": "bug"}],
          "assignees": []
        }
        "#;
        let issue = parse_issue(json).unwrap();
        assert_eq!(issue.labels, vec!["bug"]);
    }

    #[test]
    fn parse_issue_garbage_input_errors() {
        let err = parse_issue("not json at all").unwrap_err();
        assert!(format!("{err}").contains("parse issue"));
    }

    // ---------- sync helpers on the trait ----------

    #[test]
    fn issue_url_builds_canonical_github_url() {
        let t = GitHubTracker::new("acme", "widgets");
        assert_eq!(
            t.issue_url("#42"),
            "https://github.com/acme/widgets/issues/42"
        );
        assert_eq!(
            t.issue_url("42"),
            "https://github.com/acme/widgets/issues/42"
        );
    }

    #[test]
    fn branch_name_matches_ts_convention() {
        let t = GitHubTracker::new("acme", "widgets");
        assert_eq!(t.branch_name("#42"), "feat/issue-42");
        assert_eq!(t.branch_name("42"), "feat/issue-42");
    }

    #[test]
    fn name_is_github() {
        assert_eq!(GitHubTracker::new("a", "b").name(), "github");
    }

    #[test]
    fn repo_slug_formats_as_owner_slash_repo() {
        let t = GitHubTracker::new("acme", "widgets");
        assert_eq!(t.repo_slug(), "acme/widgets");
    }

    // ---------- parse_github_remote ----------

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
        // Trailing slash is tolerated (cosmetic artifact from some git configs).
        assert_eq!(
            parse_github_remote("https://github.com/acme/widgets/"),
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
    fn parse_github_remote_rejects_extra_path_segments() {
        // A tree/branch URL should not silently yield (owner, repo).
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo/tree/main"),
            None
        );
        assert_eq!(parse_github_remote("git@github.com:owner/repo/extra"), None);
    }

    #[test]
    fn parse_github_remote_strips_git_suffix_once() {
        // `.git` is stripped exactly once; `.git.git` leaves one `.git`.
        assert_eq!(
            parse_github_remote("https://github.com/acme/foo.git"),
            Some(("acme".into(), "foo".into()))
        );
        assert_eq!(
            parse_github_remote("https://github.com/acme/foo.git.git"),
            Some(("acme".into(), "foo.git".into()))
        );
    }

    #[tokio::test]
    async fn comment_issue_method_exists() {
        // Compile-level wiring test only. We don't assert success since it
        // depends on `gh` auth/network. This ensures the tracker exposes the
        // method and it can be called.
        let tracker = GitHubTracker::new("acme", "foo");
        let _ = tracker.comment_issue("1", "hello from ao-rs").await;
    }
}
