//! GitHub Issues tracker plugin — shells out to the `gh` CLI.
//!
//! Mirrors `packages/plugins/tracker-github/src/index.ts`, trimmed to the
//! surface the Rust `Tracker` trait actually needs:
//!
//! - `get_issue` → `gh api repos/{owner}/{repo}/issues/{n}`
//! - `is_completed` → `gh issue view <n> --json state,stateReason`
//!   (minimal payload, cached 30s) — closed OR cancelled counts as done
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

use ao_core::{
    AoError, CreateIssueInput, Issue, IssueFilters, IssueState, IssueUpdate, Result, Tracker,
};
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

// Rate-limit detection and cooldown live in ao-core so both GitHub
// plugins share one cooldown instant — see `ao_core::rate_limit`.
use ao_core::rate_limit::{enter_cooldown, in_cooldown_now, is_rate_limited_error};

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
        // Mirrors ao-ts `gh issue view --json state` — minimal payload,
        // but we also ask for `stateReason` so the shared state cache can
        // distinguish `Closed` from `Cancelled` (consumed by get_issue
        // callers on the same tick). Still one REST round-trip, but the
        // response is two fields instead of the full issue envelope.
        let slug = self.repo_slug();
        let json = match gh(&[
            "issue",
            "view",
            &number,
            "--repo",
            &slug,
            "--json",
            "state,stateReason",
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

    async fn assign_to_me(&self, identifier: &str) -> Result<()> {
        let number = normalize_identifier(identifier);
        // Use `gh issue edit --add-assignee @me` so we don't need to resolve the login
        // ourselves and we don't risk overwriting existing assignees.
        //
        // Note: GitHub uses a shared number space for issues and PRs; this works for both.
        let _ = gh(&[
            "--repo",
            &self.repo_slug(),
            "issue",
            "edit",
            &number,
            "--add-assignee",
            "@me",
        ])
        .await?;
        Ok(())
    }

    async fn list_issues(&self, filters: &IssueFilters) -> Result<Vec<Issue>> {
        let state = filters.state.as_deref().unwrap_or("open");
        let limit = filters.limit.unwrap_or(30);
        let mut url = format!(
            "repos/{}/{}/issues?state={}&per_page={}",
            self.owner, self.repo, state, limit
        );
        if let Some(assignee) = &filters.assignee {
            url.push_str(&format!("&assignee={assignee}"));
        }
        if !filters.labels.is_empty() {
            url.push_str(&format!("&labels={}", filters.labels.join(",")));
        }
        tracing::debug!("tracker-github: list_issues {}", url);
        let json = gh(&["api", &url]).await?;
        parse_issue_list(&json)
    }

    async fn update_issue(&self, identifier: &str, update: &IssueUpdate) -> Result<()> {
        let number = normalize_identifier(identifier);
        let slug = self.repo_slug();

        if let Some(state) = &update.state {
            match state.as_str() {
                "closed" => {
                    gh(&["--repo", &slug, "issue", "close", &number]).await?;
                }
                "open" => {
                    gh(&["--repo", &slug, "issue", "reopen", &number]).await?;
                }
                _ => {}
            }
        }

        // Build gh issue edit args for label/assignee changes.
        let mut args: Vec<String> = vec![
            "--repo".to_string(),
            slug.clone(),
            "issue".to_string(),
            "edit".to_string(),
            number.clone(),
        ];
        if !update.labels.is_empty() {
            args.push("--add-label".to_string());
            args.push(update.labels.join(","));
        }
        if !update.remove_labels.is_empty() {
            args.push("--remove-label".to_string());
            args.push(update.remove_labels.join(","));
        }
        if let Some(assignee) = &update.assignee {
            args.push("--add-assignee".to_string());
            args.push(assignee.clone());
        }
        // Only call if we added something beyond the 5 base args.
        if args.len() > 5 {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            gh(&refs).await?;
        }

        if let Some(body) = &update.comment {
            let path = format!(
                "repos/{}/{}/issues/{}/comments",
                self.owner, self.repo, number
            );
            let field = format!("body={body}");
            gh(&["--repo", &slug, "api", &path, "-f", &field]).await?;
        }
        Ok(())
    }

    async fn create_issue(&self, input: &CreateIssueInput) -> Result<Issue> {
        let mut args: Vec<String> = vec![
            "api".to_string(),
            format!("repos/{}/{}/issues", self.owner, self.repo),
            "-X".to_string(),
            "POST".to_string(),
            "-f".to_string(),
            format!("title={}", input.title),
            "-f".to_string(),
            format!("body={}", input.description),
        ];
        for label in &input.labels {
            args.push("-f".to_string());
            args.push(format!("labels[]={label}"));
        }
        if let Some(assignee) = &input.assignee {
            args.push("-f".to_string());
            args.push(format!("assignees[]={assignee}"));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let json = gh(&refs).await?;
        parse_issue(&json)
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
    #[serde(default, rename = "html_url")]
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

fn raw_to_issue(raw: RawIssue) -> Issue {
    Issue {
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
    }
}

fn parse_issue(json: &str) -> Result<Issue> {
    let raw: RawIssue =
        serde_json::from_str(json).map_err(|e| AoError::Scm(format!("parse issue: {e}")))?;
    Ok(raw_to_issue(raw))
}

fn parse_issue_list(json: &str) -> Result<Vec<Issue>> {
    let raws: Vec<RawIssue> =
        serde_json::from_str(json).map_err(|e| AoError::Scm(format!("parse issue list: {e}")))?;
    Ok(raws.into_iter().map(raw_to_issue).collect())
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

    // ---------- parse_issue_state (minimal is_completed payload) ----------
    //
    // These fixtures mirror what `gh issue view <n> --repo <slug> --json
    // state,stateReason` actually emits: a bare object with just the two
    // fields, upper-cased state values, camelCase `stateReason`, and
    // `null` when the reason is absent. Full-issue parsing is covered by
    // the `parse_issue` tests below.

    #[test]
    fn parse_issue_state_open() {
        let json = r#"{"state":"OPEN","stateReason":null}"#;
        assert_eq!(parse_issue_state(json).unwrap(), IssueState::Open);
    }

    #[test]
    fn parse_issue_state_closed_completed() {
        let json = r#"{"state":"CLOSED","stateReason":"COMPLETED"}"#;
        assert_eq!(parse_issue_state(json).unwrap(), IssueState::Closed);
    }

    #[test]
    fn parse_issue_state_closed_not_planned_is_cancelled() {
        let json = r#"{"state":"CLOSED","stateReason":"NOT_PLANNED"}"#;
        assert_eq!(parse_issue_state(json).unwrap(), IssueState::Cancelled);
    }

    #[test]
    fn parse_issue_state_missing_state_reason_defaults_to_closed() {
        // Older `gh` versions (< 2.40) omit `stateReason` entirely.
        // Missing field must not silently become Cancelled — it stays Closed.
        let json = r#"{"state":"CLOSED"}"#;
        assert_eq!(parse_issue_state(json).unwrap(), IssueState::Closed);
    }

    #[test]
    fn parse_issue_state_accepts_snake_case_alias() {
        // `gh api repos/.../issues/N` emits `state_reason` (REST snake_case)
        // while `gh issue view --json stateReason` emits camelCase. The
        // alias keeps one parser working for both entry points.
        let json = r#"{"state":"CLOSED","state_reason":"NOT_PLANNED"}"#;
        assert_eq!(parse_issue_state(json).unwrap(), IssueState::Cancelled);
    }

    #[test]
    fn parse_issue_state_missing_state_errors() {
        // If `gh` ever returns a payload with no `state` field we want to
        // surface an error rather than defaulting to Open and masking a
        // schema regression.
        let json = r#"{"stateReason":"NOT_PLANNED"}"#;
        let err = parse_issue_state(json).unwrap_err();
        assert!(format!("{err}").contains("missing `state`"));
    }

    #[test]
    fn parse_issue_state_garbage_errors() {
        let err = parse_issue_state("not json").unwrap_err();
        assert!(format!("{err}").contains("parse issue state"));
    }

    // ---------- parse_issue ----------

    #[test]
    fn parse_issue_full_payload() {
        let json = r#"
        {
          "number": 42,
          "title": "add dark mode",
          "body": "users keep asking",
          "html_url": "https://github.com/acme/widgets/issues/42",
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
          "html_url": null,
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

    // ---------- parse_issue_list ----------

    #[test]
    fn parse_issue_list_empty_array() {
        let issues = parse_issue_list("[]").unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn parse_issue_list_single_issue() {
        let json = r#"[{
          "number": 5,
          "title": "fix login",
          "body": "login is broken",
          "html_url": "https://github.com/acme/widgets/issues/5",
          "state": "OPEN",
          "stateReason": null,
          "labels": [{"name": "bug"}],
          "assignees": [{"login": "alice"}],
          "milestone": null
        }]"#;
        let issues = parse_issue_list(json).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "5");
        assert_eq!(issues[0].title, "fix login");
        assert_eq!(issues[0].state, IssueState::Open);
        assert_eq!(issues[0].labels, vec!["bug"]);
        assert_eq!(issues[0].assignee.as_deref(), Some("alice"));
    }

    #[test]
    fn parse_issue_list_multiple_issues() {
        let json = r#"[
          {"number": 1, "title": "a", "body": "", "html_url": "u1",
           "state": "OPEN", "labels": [], "assignees": []},
          {"number": 2, "title": "b", "body": null, "html_url": "u2",
           "state": "CLOSED", "stateReason": "COMPLETED", "labels": [], "assignees": []}
        ]"#;
        let issues = parse_issue_list(json).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "1");
        assert_eq!(issues[0].state, IssueState::Open);
        assert_eq!(issues[1].id, "2");
        assert_eq!(issues[1].state, IssueState::Closed);
    }

    #[test]
    fn parse_issue_list_garbage_errors() {
        let err = parse_issue_list("not json").unwrap_err();
        assert!(format!("{err}").contains("parse issue list"));
    }

    // ---------- list_issues / update_issue / create_issue wiring ----------

    #[tokio::test]
    async fn list_issues_method_exists() {
        use ao_core::IssueFilters;
        let tracker = GitHubTracker::new("acme", "foo");
        let _ = tracker.list_issues(&IssueFilters::default()).await;
    }

    #[tokio::test]
    async fn update_issue_method_exists() {
        use ao_core::IssueUpdate;
        let tracker = GitHubTracker::new("acme", "foo");
        let _ = tracker.update_issue("1", &IssueUpdate::default()).await;
    }

    #[tokio::test]
    async fn create_issue_method_exists() {
        use ao_core::CreateIssueInput;
        let tracker = GitHubTracker::new("acme", "foo");
        let _ = tracker
            .create_issue(&CreateIssueInput {
                title: "test issue".to_string(),
                description: "body".to_string(),
                labels: vec![],
                assignee: None,
            })
            .await;
    }
}
