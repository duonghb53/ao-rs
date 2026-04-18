//! GraphQL batch PR enrichment with 2-Guard ETag strategy.
//!
//! Mirrors `packages/plugins/scm-github/src/graphql-batch.ts` in the TS
//! reference. Reduces API calls from N×5 per poll tick to 1 GraphQL query
//! (or 0, when ETags confirm nothing changed).
//!
//! ## Architecture
//!
//! 1. **Guard 1 (PR List ETag)** — lightweight `GET /repos/{owner}/{repo}/pulls`
//!    with `If-None-Match`. Detects commits, reviews, labels, state changes.
//! 2. **Guard 2 (Commit Status ETag)** — per-PR `GET /repos/{owner}/{repo}/commits/{sha}/status`
//!    with `If-None-Match`. Detects CI status changes.
//! 3. **GraphQL Batch** — single `gh api graphql` call fetching all PR fields
//!    for up to 25 PRs at once using aliases.
//!
//! When both guards return 304 Not Modified, the GraphQL call is skipped
//! entirely — 0 rate-limit points consumed.

use ao_core::{
    CiStatus, MergeReadiness, PrState, PullRequest, Result, ReviewDecision, ScmObservation,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::Command;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum PRs per GraphQL batch (matches TS `MAX_BATCH_SIZE`).
pub const MAX_BATCH_SIZE: usize = 25;

const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(30);

const MAX_LRU_ETAG: usize = 100;
const MAX_LRU_COMMIT_ETAG: usize = 500;
const MAX_LRU_PR_META: usize = 200;
const MAX_LRU_PR_DATA: usize = 200;

// ---------------------------------------------------------------------------
// LRU cache (simple, bounded)
// ---------------------------------------------------------------------------

struct LruCache<V> {
    entries: Vec<(String, V)>,
    max: usize,
}

impl<V> LruCache<V> {
    fn new(max: usize) -> Self {
        Self {
            entries: Vec::new(),
            max,
        }
    }

    fn get(&mut self, key: &str) -> Option<&V> {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            let entry = self.entries.remove(pos);
            self.entries.push(entry);
            Some(&self.entries.last().unwrap().1)
        } else {
            None
        }
    }

    fn set(&mut self, key: String, value: V) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            self.entries.remove(pos);
        }
        self.entries.push((key, value));
        if self.entries.len() > self.max {
            self.entries.remove(0);
        }
    }
}

// ---------------------------------------------------------------------------
// Cached PR metadata (for ETag Guard 2 decisions)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct PrMetadata {
    head_sha: Option<String>,
    #[allow(dead_code)]
    ci_status: CiStatus,
}

// ---------------------------------------------------------------------------
// Batch enrichment state (process-global, behind Mutex)
// ---------------------------------------------------------------------------

struct BatchState {
    pr_list_etags: LruCache<String>,
    commit_status_etags: LruCache<String>,
    pr_metadata: LruCache<PrMetadata>,
    pr_enrichment: LruCache<ScmObservation>,
}

impl BatchState {
    fn new() -> Self {
        Self {
            pr_list_etags: LruCache::new(MAX_LRU_ETAG),
            commit_status_etags: LruCache::new(MAX_LRU_COMMIT_ETAG),
            pr_metadata: LruCache::new(MAX_LRU_PR_META),
            pr_enrichment: LruCache::new(MAX_LRU_PR_DATA),
        }
    }
}

fn global_state() -> &'static Mutex<BatchState> {
    use std::sync::OnceLock;
    static STATE: OnceLock<Mutex<BatchState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(BatchState::new()))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Batch-enrich multiple PRs using the 2-Guard ETag + GraphQL strategy.
///
/// Returns a map keyed by `"{owner}/{repo}#{number}"` with the observation
/// for each PR that was successfully fetched. Missing entries should fall
/// back to individual REST calls.
pub async fn enrich_prs_batch(prs: &[PullRequest]) -> Result<HashMap<String, ScmObservation>> {
    if prs.is_empty() {
        return Ok(HashMap::new());
    }

    // Step 1: ETag guards
    let should_refresh = should_refresh_pr_enrichment(prs).await;

    if !should_refresh {
        let mut result = HashMap::new();
        let mut missing = Vec::new();
        {
            let mut state = global_state().lock().unwrap();
            for pr in prs {
                let key = pr_key(pr);
                if let Some(cached) = state.pr_enrichment.get(&key).cloned() {
                    result.insert(key, cached);
                } else {
                    missing.push(pr.clone());
                }
            }
        }
        if missing.is_empty() {
            tracing::debug!(
                "[ETag Guard] Skipping GraphQL batch — all {} PRs cached",
                result.len()
            );
            return Ok(result);
        }
        tracing::debug!(
            "[ETag Guard] Partial cache: {} cached, {} missing",
            result.len(),
            missing.len()
        );
        let batch_result = run_graphql_batches(&missing).await?;
        result.extend(batch_result);
        return Ok(result);
    }

    // Guards detected changes — run full batch
    run_graphql_batches(prs).await
}

fn pr_key(pr: &PullRequest) -> String {
    format!("{}/{}#{}", pr.owner, pr.repo, pr.number)
}

// ---------------------------------------------------------------------------
// ETag guards
// ---------------------------------------------------------------------------

async fn should_refresh_pr_enrichment(prs: &[PullRequest]) -> bool {
    // Guard 1: PR list ETag per repo
    let mut repos: HashMap<String, Vec<&PullRequest>> = HashMap::new();
    for pr in prs {
        repos
            .entry(format!("{}/{}", pr.owner, pr.repo))
            .or_default()
            .push(pr);
    }

    let mut guard1_changed = false;
    for repo_key in repos.keys() {
        let parts: Vec<&str> = repo_key.split('/').collect();
        if parts.len() == 2 && check_pr_list_etag(parts[0], parts[1]).await {
            guard1_changed = true;
            break;
        }
    }

    if guard1_changed {
        return true;
    }

    // Guard 2: Commit status ETag per PR (only when Guard 1 didn't fire)
    for pr in prs {
        let key = pr_key(pr);
        let meta = {
            let mut state = global_state().lock().unwrap();
            state.pr_metadata.get(&key).cloned()
        };
        if let Some(meta) = meta {
            if let Some(sha) = &meta.head_sha {
                if check_commit_status_etag(&pr.owner, &pr.repo, sha).await {
                    return true;
                }
            } else {
                // Cached but no head SHA — need to refresh
                return true;
            }
        }
        // No cached metadata — skip Guard 2 for this PR
    }

    false
}

/// Guard 1: Check if the PR list for a repo has changed via ETag.
async fn check_pr_list_etag(owner: &str, repo: &str) -> bool {
    let repo_key = format!("{owner}/{repo}");
    let cached_etag = {
        let mut state = global_state().lock().unwrap();
        state.pr_list_etags.get(&repo_key).cloned()
    };

    let url = format!("repos/{repo_key}/pulls?state=open&sort=updated&direction=desc&per_page=1");
    let mut args = vec!["api", "--method", "GET", &url, "-i"];
    let header;
    if let Some(ref etag) = cached_etag {
        header = format!("If-None-Match: {etag}");
        args.extend(["-H", &header]);
    }

    match run_gh(&args).await {
        Ok(output) => {
            if output.contains("304") {
                return false;
            }
            if let Some(new_etag) = extract_etag(&output) {
                let mut state = global_state().lock().unwrap();
                state.pr_list_etags.set(repo_key, new_etag);
            }
            true
        }
        Err(e) => {
            tracing::warn!("[ETag Guard 1] PR list check failed for {repo_key}: {e}");
            true // assume changed on error
        }
    }
}

/// Guard 2: Check if commit status has changed for a specific SHA.
async fn check_commit_status_etag(owner: &str, repo: &str, sha: &str) -> bool {
    let commit_key = format!("{owner}/{repo}#{sha}");
    let cached_etag = {
        let mut state = global_state().lock().unwrap();
        state.commit_status_etags.get(&commit_key).cloned()
    };

    let url = format!("repos/{owner}/{repo}/commits/{sha}/status");
    let mut args = vec!["api", "--method", "GET", &url, "-i"];
    let header;
    if let Some(ref etag) = cached_etag {
        header = format!("If-None-Match: {etag}");
        args.extend(["-H", &header]);
    }

    match run_gh(&args).await {
        Ok(output) => {
            if output.contains("304") {
                return false;
            }
            if let Some(new_etag) = extract_etag(&output) {
                let mut state = global_state().lock().unwrap();
                state.commit_status_etags.set(commit_key, new_etag);
            }
            true
        }
        Err(e) => {
            tracing::warn!("[ETag Guard 2] Commit status check failed for {commit_key}: {e}");
            true
        }
    }
}

fn extract_etag(response: &str) -> Option<String> {
    for line in response.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("etag:") {
            return Some(line[5..].trim().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// GraphQL batch query
// ---------------------------------------------------------------------------

const PR_FIELDS: &str = r#"
  title
  state
  isDraft
  mergeable
  mergeStateStatus
  reviewDecision
  headRefName
  headRefOid
  reviews(last: 5) {
    nodes {
      author { login }
      state
      submittedAt
    }
  }
  commits(last: 1) {
    nodes {
      commit {
        statusCheckRollup {
          state
          contexts(first: 20) {
            nodes {
              ... on CheckRun {
                name
                status
                conclusion
                detailsUrl
              }
              ... on StatusContext {
                context
                state
                targetUrl
              }
            }
            pageInfo {
              hasNextPage
            }
          }
        }
      }
    }
  }
"#;

fn generate_batch_query(prs: &[PullRequest]) -> (String, Vec<(String, serde_json::Value)>) {
    let mut selections = Vec::new();
    let mut variables = Vec::new();

    for (i, pr) in prs.iter().enumerate() {
        let alias = format!("pr{i}");
        selections.push(format!(
            r#"
      {alias}: repository(owner: ${alias}Owner, name: ${alias}Name) {{
        ... on Repository {{
          pullRequest(number: ${alias}Number) {{ {PR_FIELDS} }}
        }}
      }}
    "#
        ));
        variables.push((format!("{alias}Owner"), serde_json::json!(pr.owner)));
        variables.push((format!("{alias}Name"), serde_json::json!(pr.repo)));
        variables.push((format!("{alias}Number"), serde_json::json!(pr.number)));
    }

    let var_defs: Vec<String> = variables
        .iter()
        .map(|(key, val)| {
            let ty = if val.is_number() { "Int!" } else { "String!" };
            format!("${key}: {ty}")
        })
        .collect();

    let query = format!(
        "query BatchPRs({}) {{\n{}\n}}",
        var_defs.join(", "),
        selections.join("\n")
    );

    (query, variables)
}

async fn run_graphql_batches(prs: &[PullRequest]) -> Result<HashMap<String, ScmObservation>> {
    let mut result = HashMap::new();

    for chunk in prs.chunks(MAX_BATCH_SIZE) {
        match execute_batch_query(chunk).await {
            Ok(data) => {
                for (i, pr) in chunk.iter().enumerate() {
                    let alias = format!("pr{i}");
                    if let Some(repo_data) = data.get(&alias) {
                        if let Some(pr_data) = repo_data.get("pullRequest") {
                            if let Some((obs, head_sha)) = extract_pr_enrichment(pr_data) {
                                let key = pr_key(pr);
                                let mut state = global_state().lock().unwrap();
                                state.pr_metadata.set(
                                    key.clone(),
                                    PrMetadata {
                                        head_sha: head_sha.clone(),
                                        ci_status: obs.ci,
                                    },
                                );
                                state.pr_enrichment.set(key.clone(), obs.clone());
                                drop(state);
                                result.insert(key, obs);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("[GraphQL Batch] Batch failed ({} PRs): {e}", chunk.len());
                // Continue — individual REST calls will be the fallback
            }
        }
    }

    Ok(result)
}

async fn execute_batch_query(prs: &[PullRequest]) -> Result<HashMap<String, serde_json::Value>> {
    let (query, variables) = generate_batch_query(prs);

    let mut args: Vec<String> = vec!["api".into(), "graphql".into()];
    for (key, val) in &variables {
        if val.is_string() {
            args.push("-f".into());
            args.push(format!("{}={}", key, val.as_str().unwrap()));
        } else {
            args.push("-F".into());
            args.push(format!("{}={}", key, val));
        }
    }
    args.push("-f".into());
    args.push(format!("query={query}"));

    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let stdout = run_gh(&args_refs).await?;

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| ao_core::AoError::Scm(format!("GraphQL JSON parse: {e}")))?;

    if let Some(errors) = parsed.get("errors").and_then(|e| e.as_array()) {
        if !errors.is_empty() {
            let msgs: Vec<&str> = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .collect();
            return Err(ao_core::AoError::Scm(format!(
                "GraphQL errors: {}",
                msgs.join("; ")
            )));
        }
    }

    let data = parsed
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    if let serde_json::Value::Object(map) = data {
        Ok(map.into_iter().collect())
    } else {
        Ok(HashMap::new())
    }
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

fn extract_pr_enrichment(pr: &serde_json::Value) -> Option<(ScmObservation, Option<String>)> {
    let state = match pr
        .get("state")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_uppercase()
        .as_str()
    {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    };

    let head_sha = pr
        .get("headRefOid")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());

    let is_draft = pr.get("isDraft").and_then(|v| v.as_bool()).unwrap_or(false);

    // CI status from statusCheckRollup
    let rollup_state = pr
        .pointer("/commits/nodes/0/commit/statusCheckRollup/state")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_uppercase();

    let ci = match rollup_state.as_str() {
        "SUCCESS" => CiStatus::Passing,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" => CiStatus::Failing,
        "PENDING" | "EXPECTED" | "QUEUED" | "IN_PROGRESS" | "WAITING" => CiStatus::Pending,
        _ => CiStatus::None,
    };

    // Review decision
    let review_raw = pr
        .get("reviewDecision")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_uppercase();
    let review = match review_raw.as_str() {
        "APPROVED" => ReviewDecision::Approved,
        "CHANGES_REQUESTED" => ReviewDecision::ChangesRequested,
        "REVIEW_REQUIRED" => ReviewDecision::Pending,
        _ => ReviewDecision::None,
    };

    // Mergeability
    let mergeable_raw = pr
        .get("mergeable")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_uppercase();
    let merge_state_raw = pr
        .get("mergeStateStatus")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_uppercase();

    let no_conflicts = mergeable_raw == "MERGEABLE";
    let has_conflicts = mergeable_raw == "CONFLICTING";
    let is_behind = merge_state_raw == "BEHIND";

    let ci_passing = matches!(ci, CiStatus::Passing | CiStatus::None);
    let approved = matches!(review, ReviewDecision::Approved | ReviewDecision::None);

    let mut blockers: Vec<String> = Vec::new();
    if !ci_passing {
        blockers.push(format!("CI is {}", ci_label(ci)));
    }
    if review == ReviewDecision::ChangesRequested {
        blockers.push("Changes requested in review".into());
    } else if review == ReviewDecision::Pending {
        blockers.push("Review required".into());
    }
    if has_conflicts {
        blockers.push("Merge conflicts".into());
    } else if mergeable_raw.is_empty() || mergeable_raw == "UNKNOWN" {
        blockers.push("Merge status unknown (GitHub is computing)".into());
    }
    match merge_state_raw.as_str() {
        "BEHIND" => blockers.push("Branch is behind base branch".into()),
        "BLOCKED" => blockers.push("Branch protection requirements not satisfied".into()),
        "UNSTABLE" => blockers.push("Required checks are failing".into()),
        "DIRTY" => blockers.push("Merge is blocked (conflicts or failing requirements)".into()),
        _ => {}
    }
    if is_draft {
        blockers.push("PR is still a draft".into());
    }

    let is_open = state == PrState::Open;
    let merge_ready = is_open
        && ci_passing
        && approved
        && !has_conflicts
        && !is_behind
        && !is_draft
        && blockers.is_empty();

    let readiness = MergeReadiness {
        mergeable: merge_ready,
        ci_passing,
        approved,
        no_conflicts,
        blockers,
    };

    Some((
        ScmObservation {
            state,
            ci,
            review,
            readiness,
        },
        head_sha,
    ))
}

fn ci_label(ci: CiStatus) -> &'static str {
    match ci {
        CiStatus::Pending => "pending",
        CiStatus::Passing => "passing",
        CiStatus::Failing => "failing",
        CiStatus::None => "none",
    }
}

// ---------------------------------------------------------------------------
// Subprocess helper
// ---------------------------------------------------------------------------

async fn run_gh(args: &[&str]) -> Result<String> {
    if ao_core::rate_limit::in_cooldown_now() {
        return Err(ao_core::AoError::Scm(
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
        .map_err(|_| ao_core::AoError::Scm(format!("gh {} timed out", args.join(" "))))?
        .map_err(|e| ao_core::AoError::Scm(format!("gh spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if ao_core::rate_limit::is_rate_limited_error(stderr.as_ref()) {
            ao_core::rate_limit::enter_cooldown();
        }
        return Err(ao_core::AoError::Scm(format!(
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

    fn sample_pr(owner: &str, repo: &str, number: u32) -> PullRequest {
        PullRequest {
            number,
            url: format!("https://github.com/{owner}/{repo}/pull/{number}"),
            title: "test".into(),
            owner: owner.into(),
            repo: repo.into(),
            branch: "ao-test".into(),
            base_branch: "main".into(),
            is_draft: false,
        }
    }

    #[test]
    fn generate_batch_query_produces_valid_graphql() {
        let prs = vec![
            sample_pr("acme", "widgets", 42),
            sample_pr("acme", "widgets", 43),
        ];
        let (query, vars) = generate_batch_query(&prs);
        assert!(query.contains("pr0:"));
        assert!(query.contains("pr1:"));
        assert!(query.contains("$pr0Owner: String!"));
        assert!(query.contains("$pr1Number: Int!"));
        assert_eq!(vars.len(), 6); // 3 vars per PR
    }

    #[test]
    fn generate_batch_query_empty_prs() {
        let (query, vars) = generate_batch_query(&[]);
        assert!(query.contains("query BatchPRs()"));
        assert!(vars.is_empty());
    }

    #[test]
    fn extract_pr_enrichment_all_green() {
        let json = serde_json::json!({
            "state": "OPEN",
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN",
            "reviewDecision": "APPROVED",
            "headRefOid": "abc123",
            "commits": {
                "nodes": [{
                    "commit": {
                        "statusCheckRollup": {
                            "state": "SUCCESS",
                            "contexts": { "nodes": [], "pageInfo": { "hasNextPage": false } }
                        }
                    }
                }]
            }
        });
        let (obs, sha) = extract_pr_enrichment(&json).unwrap();
        assert_eq!(obs.state, PrState::Open);
        assert_eq!(obs.ci, CiStatus::Passing);
        assert_eq!(obs.review, ReviewDecision::Approved);
        assert!(obs.readiness.is_ready());
        assert_eq!(sha, Some("abc123".into()));
    }

    #[test]
    fn extract_pr_enrichment_ci_failing() {
        let json = serde_json::json!({
            "state": "OPEN",
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN",
            "reviewDecision": "APPROVED",
            "headRefOid": "def456",
            "commits": {
                "nodes": [{
                    "commit": {
                        "statusCheckRollup": {
                            "state": "FAILURE",
                            "contexts": { "nodes": [], "pageInfo": { "hasNextPage": false } }
                        }
                    }
                }]
            }
        });
        let (obs, _) = extract_pr_enrichment(&json).unwrap();
        assert_eq!(obs.ci, CiStatus::Failing);
        assert!(!obs.readiness.ci_passing);
        assert!(!obs.readiness.is_ready());
    }

    #[test]
    fn extract_pr_enrichment_merged() {
        let json = serde_json::json!({
            "state": "MERGED",
            "isDraft": false,
            "mergeable": "",
            "mergeStateStatus": "",
            "reviewDecision": null,
            "headRefOid": "xyz789",
            "commits": { "nodes": [] }
        });
        let (obs, _) = extract_pr_enrichment(&json).unwrap();
        assert_eq!(obs.state, PrState::Merged);
    }

    #[test]
    fn extract_etag_from_headers() {
        let response =
            "HTTP/2 200\r\netag: W/\"abc123\"\r\ncontent-type: application/json\r\n\r\n{}";
        assert_eq!(extract_etag(response), Some("W/\"abc123\"".into()));
    }

    #[test]
    fn extract_etag_missing() {
        let response = "HTTP/2 304\r\ncontent-type: application/json\r\n\r\n";
        assert_eq!(extract_etag(response), None);
    }

    #[test]
    fn lru_cache_evicts_oldest() {
        let mut cache: LruCache<i32> = LruCache::new(2);
        cache.set("a".into(), 1);
        cache.set("b".into(), 2);
        cache.set("c".into(), 3); // evicts "a"
        assert!(cache.get("a").is_none());
        assert_eq!(cache.get("b"), Some(&2));
        assert_eq!(cache.get("c"), Some(&3));
    }

    #[test]
    fn lru_cache_access_refreshes_order() {
        let mut cache: LruCache<i32> = LruCache::new(2);
        cache.set("a".into(), 1);
        cache.set("b".into(), 2);
        cache.get("a"); // refresh "a"
        cache.set("c".into(), 3); // evicts "b" (not "a")
        assert_eq!(cache.get("a"), Some(&1));
        assert!(cache.get("b").is_none());
    }

    #[test]
    fn pr_key_format() {
        let pr = sample_pr("acme", "widgets", 42);
        assert_eq!(pr_key(&pr), "acme/widgets#42");
    }
}
