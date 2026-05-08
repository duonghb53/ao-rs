//! Batch GraphQL issue-state prefetch with 10 s cache.
//!
//! Mirrors the aliased-query pattern in `scm-github/src/graphql_batch.rs` but
//! for issues. Reduces N individual `gh issue view` REST calls to a single
//! GraphQL query per tick when `batch_prefetch_issue_states` is wired into the
//! lifecycle pre-pass.

use ao_core::{
    gh::run_gh,
    rate_limit::{enter_cooldown, in_cooldown_now, is_rate_limited_error},
    IssueState,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const ISSUE_BATCH_TTL: Duration = Duration::from_secs(10);
const MAX_BATCH_SIZE: usize = 50;

// ---------------------------------------------------------------------------
// Process-global cache
// ---------------------------------------------------------------------------

struct BatchCache {
    entries: HashMap<String, (Instant, IssueState)>,
}

fn batch_cache() -> &'static Mutex<BatchCache> {
    static CACHE: OnceLock<Mutex<BatchCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(BatchCache {
            entries: HashMap::new(),
        })
    })
}

fn cache_key(owner: &str, repo: &str, number: u64) -> String {
    format!("{owner}/{repo}#{number}")
}

/// Returns cached state if fresh (< 10 s), else `None`.
pub fn get_cached_state(owner: &str, repo: &str, number: u64) -> Option<IssueState> {
    let cache = batch_cache().lock().unwrap_or_else(|e| {
        tracing::error!("batch_cache mutex poisoned; recovering inner state");
        e.into_inner()
    });
    let key = cache_key(owner, repo, number);
    if let Some((at, state)) = cache.entries.get(&key) {
        if at.elapsed() < ISSUE_BATCH_TTL {
            return Some(*state);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// GraphQL response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct GqlResponse {
    data: HashMap<String, GqlRepoData>,
}

#[derive(Deserialize)]
struct GqlRepoData {
    issue: Option<GqlIssueState>,
}

#[derive(Deserialize)]
struct GqlIssueState {
    state: String,
    #[serde(rename = "stateReason")]
    state_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Batch-prefetch issue states for all `numbers` via one GraphQL call per
/// `MAX_BATCH_SIZE` chunk. Populates the 10 s cache. On rate-limit silently
/// returns — stale cache entries are still served by `get_cached_state`.
pub async fn prefetch_issue_states(owner: &str, repo: &str, numbers: &[u64]) {
    // Filter out numbers already fresh in the cache.
    let stale: Vec<u64> = {
        let cache = batch_cache().lock().unwrap_or_else(|e| {
            tracing::error!("batch_cache mutex poisoned; recovering inner state");
            e.into_inner()
        });
        numbers
            .iter()
            .filter(|&&n| {
                cache
                    .entries
                    .get(&cache_key(owner, repo, n))
                    .is_none_or(|(at, _)| at.elapsed() >= ISSUE_BATCH_TTL)
            })
            .copied()
            .collect()
    };

    if stale.is_empty() || in_cooldown_now() {
        return;
    }

    for chunk in stale.chunks(MAX_BATCH_SIZE) {
        let query = build_query(owner, repo, chunk);
        match run_gh(&["api", "graphql", "-f", &format!("query={query}")]).await {
            Ok(json) => populate_cache(owner, repo, chunk, &json),
            Err(e) => {
                let msg = e.to_string();
                if is_rate_limited_error(&msg) {
                    tracing::warn!(
                        "tracker-github: rate-limited during batch issue prefetch; entering cooldown"
                    );
                    enter_cooldown();
                } else {
                    tracing::debug!("tracker-github: batch issue prefetch chunk failed: {e}");
                }
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_query(owner: &str, repo: &str, numbers: &[u64]) -> String {
    let mut q = String::from("{");
    for &n in numbers {
        q.push_str(&format!(
            " i_{n}: repository(owner: \"{owner}\", name: \"{repo}\") \
             {{ issue(number: {n}) {{ state stateReason }} }}"
        ));
    }
    q.push('}');
    q
}

fn populate_cache(owner: &str, repo: &str, numbers: &[u64], json: &str) {
    let resp: GqlResponse = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("tracker-github: batch issue parse failed: {e}");
            return;
        }
    };
    let mut cache = batch_cache().lock().unwrap_or_else(|e| {
        tracing::error!("batch_cache mutex poisoned; recovering inner state");
        e.into_inner()
    });
    for &n in numbers {
        let alias = format!("i_{n}");
        let Some(repo_data) = resp.data.get(&alias) else {
            continue;
        };
        let Some(issue) = &repo_data.issue else {
            continue;
        };
        let state = super::map_state(&issue.state, issue.state_reason.as_deref());
        cache
            .entries
            .insert(cache_key(owner, repo, n), (Instant::now(), state));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_query_single_issue() {
        let q = build_query("acme", "widgets", &[42]);
        assert!(q.contains("i_42:"));
        assert!(q.contains("repository(owner: \"acme\", name: \"widgets\")"));
        assert!(q.contains("issue(number: 42)"));
        assert!(q.contains("stateReason"));
    }

    #[test]
    fn build_query_multiple_issues() {
        let q = build_query("acme", "widgets", &[1, 2, 3]);
        assert!(q.contains("i_1:"));
        assert!(q.contains("i_2:"));
        assert!(q.contains("i_3:"));
    }

    #[test]
    fn cache_key_format() {
        assert_eq!(cache_key("owner", "repo", 42), "owner/repo#42");
    }

    #[test]
    fn get_cached_state_returns_none_for_unknown() {
        assert_eq!(get_cached_state("no-owner", "no-repo", 99999), None);
    }
}
