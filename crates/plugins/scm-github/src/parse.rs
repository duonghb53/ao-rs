//! Pure JSON → domain-type parsers for the GitHub SCM plugin.
//!
//! Split out from `lib.rs` so every method's wire format can be unit-tested
//! without spawning `gh`. Each parser takes a `&str` of JSON (exactly what
//! `gh ... --json ...` prints on stdout) and returns the domain type defined
//! in `ao-core::scm`.
//!
//! Mirrors the JSON shapes consumed by the TS plugin in
//! `packages/plugins/scm-github/src/index.ts` — see the `JSON.parse` sites
//! around lines 541, 569, 603, 664, 737, 767, 896, 979 in the reference.
//!
//! The parsers are deliberately tolerant: missing optional fields become
//! `None`/`""`, unknown enum variants collapse to the safest fallback
//! (`CheckStatus::Skipped`, `ReviewState::Commented`, `ReviewDecision::None`).
//! That matches how the TS reference treats its own `any` JSON — if GitHub
//! adds a new check state tomorrow we don't want the lifecycle loop to
//! crash, we want it to keep ticking with a slightly degraded view.

use ao_core::{
    AoError, CheckRun, CheckStatus, CiStatus, PrState, PullRequest, Result, Review, ReviewComment,
    ReviewDecision, ReviewState,
};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

fn bad(msg: impl Into<String>, err: impl std::fmt::Display) -> AoError {
    AoError::Scm(format!("{}: {}", msg.into(), err))
}

// ---------------------------------------------------------------------------
// PR list / view → PullRequest
// ---------------------------------------------------------------------------

/// Raw shape of a single PR as returned by `gh pr view/list --json
/// number,url,title,headRefName,baseRefName,isDraft`.
#[derive(Debug, Deserialize)]
struct RawPr {
    number: u32,
    url: String,
    title: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
}

impl RawPr {
    fn into_pull_request(self, owner: &str, repo: &str) -> PullRequest {
        PullRequest {
            number: self.number,
            url: self.url,
            title: self.title,
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: self.head_ref_name,
            base_branch: self.base_ref_name,
            is_draft: self.is_draft,
        }
    }
}

/// Parse `gh pr list ... --json number,url,title,headRefName,baseRefName,isDraft --limit 1`.
/// Returns `None` when the list is empty (no PR for the branch yet).
pub(crate) fn parse_pr_list(json: &str, owner: &str, repo: &str) -> Result<Option<PullRequest>> {
    let raw: Vec<RawPr> = serde_json::from_str(json).map_err(|e| bad("parse pr list", e))?;
    Ok(raw
        .into_iter()
        .next()
        .map(|r| r.into_pull_request(owner, repo)))
}

/// Parse `gh pr view <num> ... --json state`. Mirrors lines 593–608 of the TS.
pub(crate) fn parse_pr_state(json: &str) -> Result<PrState> {
    #[derive(Deserialize)]
    struct Wrap {
        state: String,
    }
    let w: Wrap = serde_json::from_str(json).map_err(|e| bad("parse pr state", e))?;
    Ok(match w.state.to_ascii_uppercase().as_str() {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    })
}

// ---------------------------------------------------------------------------
// CI checks
// ---------------------------------------------------------------------------

/// Map GitHub's raw check state (any provider) onto our normalized
/// `CheckStatus`. Mirrors `mapRawCheckStateToStatus` in the TS reference —
/// the goal is a stable 5-value enum the reaction engine can switch on,
/// even as providers churn their own terminology.
///
/// Anything we don't recognize becomes `Skipped` so a surprise value from
/// a new CI system doesn't get folded into `Failed` and trigger spurious
/// `ci-failed` reactions.
pub(crate) fn map_check_state(raw: &str) -> CheckStatus {
    match raw.trim().to_ascii_uppercase().as_str() {
        "IN_PROGRESS" => CheckStatus::Running,
        "PENDING" | "QUEUED" | "REQUESTED" | "WAITING" | "EXPECTED" => CheckStatus::Pending,
        "SUCCESS" => CheckStatus::Passed,
        "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "ERROR" => CheckStatus::Failed,
        // SKIPPED, NEUTRAL, STALE, NOT_REQUIRED, NONE, "" → skipped.
        _ => CheckStatus::Skipped,
    }
}

#[derive(Debug, Deserialize)]
struct RawCheck {
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    link: String,
}

/// Parse `gh pr checks <num> --json name,state,link,...`.
pub(crate) fn parse_ci_checks(json: &str) -> Result<Vec<CheckRun>> {
    let raw: Vec<RawCheck> = serde_json::from_str(json).map_err(|e| bad("parse ci checks", e))?;
    Ok(raw
        .into_iter()
        .map(|c| {
            let status = map_check_state(&c.state);
            // `conclusion` on `CheckRun` is the raw provider string — preserve
            // it verbatim so the `ci-failed` reaction can include it in the
            // message to the agent. Empty → None.
            let conclusion = if c.state.is_empty() {
                None
            } else {
                Some(c.state)
            };
            let url = if c.link.is_empty() {
                None
            } else {
                Some(c.link)
            };
            CheckRun {
                name: c.name,
                status,
                url,
                conclusion,
            }
        })
        .collect())
}

/// Fold a vector of checks into a rolled-up CI summary. Mirrors
/// `getCISummary` logic at lines 686–718 of the TS reference.
///
/// Order matters — we report the worst state first:
///
/// - any `failed` → `Failing`
/// - any `pending`/`running` → `Pending`
/// - any `passed` → `Passing`
/// - otherwise (all skipped or empty) → `None`
pub(crate) fn summarize_ci(checks: &[CheckRun]) -> CiStatus {
    if checks.is_empty() {
        return CiStatus::None;
    }
    if checks.iter().any(|c| c.status == CheckStatus::Failed) {
        return CiStatus::Failing;
    }
    if checks
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Pending | CheckStatus::Running))
    {
        return CiStatus::Pending;
    }
    if checks.iter().any(|c| c.status == CheckStatus::Passed) {
        return CiStatus::Passing;
    }
    CiStatus::None
}

// ---------------------------------------------------------------------------
// Reviews
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawReviewEnvelope {
    #[serde(default)]
    reviews: Vec<RawReview>,
}

#[derive(Debug, Deserialize)]
struct RawReview {
    #[serde(default)]
    author: Option<RawLogin>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: String,
}

#[derive(Debug, Deserialize)]
struct RawLogin {
    #[serde(default)]
    login: String,
}

fn map_review_state(raw: &str) -> ReviewState {
    match raw.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "DISMISSED" => ReviewState::Dismissed,
        "PENDING" => ReviewState::Pending,
        // COMMENTED + unknowns fall through — TS does the same.
        _ => ReviewState::Commented,
    }
}

/// Parse `gh pr view <num> --json reviews`.
pub(crate) fn parse_reviews(json: &str) -> Result<Vec<Review>> {
    let env: RawReviewEnvelope = serde_json::from_str(json).map_err(|e| bad("parse reviews", e))?;
    Ok(env
        .reviews
        .into_iter()
        .map(|r| Review {
            author: r
                .author
                .map(|a| a.login)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string()),
            state: map_review_state(&r.state),
            body: if r.body.is_empty() {
                None
            } else {
                Some(r.body)
            },
        })
        .collect())
}

/// Parse `gh pr view <num> --json reviewDecision`. The field is a bare
/// string like `"APPROVED"`, `"CHANGES_REQUESTED"`, `"REVIEW_REQUIRED"` or
/// `null` (no reviewers assigned).
pub(crate) fn parse_review_decision(json: &str) -> Result<ReviewDecision> {
    #[derive(Deserialize)]
    struct Wrap {
        #[serde(default)]
        #[serde(rename = "reviewDecision")]
        review_decision: Option<String>,
    }
    let w: Wrap = serde_json::from_str(json).map_err(|e| bad("parse review decision", e))?;
    let raw = w.review_decision.unwrap_or_default();
    Ok(match raw.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => ReviewDecision::Approved,
        "CHANGES_REQUESTED" => ReviewDecision::ChangesRequested,
        "REVIEW_REQUIRED" => ReviewDecision::Pending,
        _ => ReviewDecision::None,
    })
}

// ---------------------------------------------------------------------------
// Review comments (REST endpoint)
// ---------------------------------------------------------------------------

/// Parse `gh api repos/{owner}/{repo}/pulls/{n}/comments`.
///
/// **Phase B limitation**: this endpoint doesn't tell us which comments
/// belong to resolved threads, so every returned comment carries
/// `is_resolved: false`. The `changes-requested` reaction will therefore
/// treat resolved comments as pending until Phase D switches this to the
/// GraphQL `reviewThreads` query.
pub(crate) fn parse_review_comments(json: &str) -> Result<Vec<ReviewComment>> {
    #[derive(Deserialize)]
    struct Raw {
        id: u64,
        #[serde(default)]
        user: Option<RawLogin>,
        #[serde(default)]
        body: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        line: Option<u32>,
        #[serde(default)]
        original_line: Option<u32>,
        #[serde(default)]
        html_url: String,
    }
    let raw: Vec<Raw> = serde_json::from_str(json).map_err(|e| bad("parse review comments", e))?;
    Ok(raw
        .into_iter()
        .map(|c| ReviewComment {
            id: c.id.to_string(),
            author: c
                .user
                .map(|u| u.login)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string()),
            body: c.body,
            // Skip pinning if it's the zero-width string GitHub sometimes
            // returns. `None` means "not an inline comment".
            path: c.path.filter(|p| !p.is_empty()),
            // Prefer current line; fall back to original (the line the
            // comment was pinned to when first authored, which survives
            // force-pushes). Matches TS line 934.
            line: c.line.or(c.original_line),
            is_resolved: false,
            url: c.html_url,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Mergeability
// ---------------------------------------------------------------------------

/// Raw fields we pull out of `gh pr view <num> --json
/// mergeable,reviewDecision,mergeStateStatus,isDraft` — i.e. everything
/// `compose_merge_readiness` needs except the CI status (which the caller
/// computes separately via `summarize_ci` or a cached `ci_checks` result).
#[derive(Debug, Deserialize)]
pub(crate) struct RawMergeability {
    #[serde(default)]
    pub mergeable: String,
    #[serde(default, rename = "reviewDecision")]
    pub review_decision: Option<String>,
    #[serde(default, rename = "mergeStateStatus")]
    pub merge_state_status: String,
    #[serde(default, rename = "isDraft")]
    pub is_draft: bool,
}

pub(crate) fn parse_raw_mergeability(json: &str) -> Result<RawMergeability> {
    serde_json::from_str(json).map_err(|e| bad("parse mergeability", e))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_list_empty_returns_none() {
        let pr = parse_pr_list("[]", "acme", "widgets").unwrap();
        assert!(pr.is_none());
    }

    #[test]
    fn parse_pr_list_maps_head_and_base_branch() {
        let json = r#"
        [
          {
            "number": 42,
            "url": "https://github.com/acme/widgets/pull/42",
            "title": "fix things",
            "headRefName": "ao-3a4b5c6d",
            "baseRefName": "main",
            "isDraft": false
          }
        ]
        "#;
        let pr = parse_pr_list(json, "acme", "widgets").unwrap().unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.owner, "acme");
        assert_eq!(pr.repo, "widgets");
        assert_eq!(pr.branch, "ao-3a4b5c6d");
        assert_eq!(pr.base_branch, "main");
        assert!(!pr.is_draft);
    }

    #[test]
    fn parse_pr_list_draft_flag_survives() {
        let json = r#"[{
            "number": 7, "url": "u", "title": "t",
            "headRefName": "h", "baseRefName": "main", "isDraft": true
        }]"#;
        let pr = parse_pr_list(json, "o", "r").unwrap().unwrap();
        assert!(pr.is_draft);
    }

    #[test]
    fn parse_pr_state_uppercases_input() {
        assert_eq!(
            parse_pr_state(r#"{"state":"merged"}"#).unwrap(),
            PrState::Merged
        );
        assert_eq!(
            parse_pr_state(r#"{"state":"CLOSED"}"#).unwrap(),
            PrState::Closed
        );
        assert_eq!(
            parse_pr_state(r#"{"state":"open"}"#).unwrap(),
            PrState::Open
        );
        // Anything unrecognized collapses to Open — TS does the same.
        assert_eq!(
            parse_pr_state(r#"{"state":"weird"}"#).unwrap(),
            PrState::Open
        );
    }

    #[test]
    fn map_check_state_covers_every_documented_github_value() {
        // GitHub's check_run states — making this explicit so a future
        // refactor of `map_check_state` can't accidentally drop one.
        assert_eq!(map_check_state("IN_PROGRESS"), CheckStatus::Running);
        for s in ["PENDING", "QUEUED", "REQUESTED", "WAITING", "EXPECTED"] {
            assert_eq!(map_check_state(s), CheckStatus::Pending, "input: {s}");
        }
        assert_eq!(map_check_state("SUCCESS"), CheckStatus::Passed);
        for s in [
            "FAILURE",
            "TIMED_OUT",
            "CANCELLED",
            "ACTION_REQUIRED",
            "ERROR",
        ] {
            assert_eq!(map_check_state(s), CheckStatus::Failed, "input: {s}");
        }
        for s in ["SKIPPED", "NEUTRAL", "STALE", "NOT_REQUIRED", "NONE", ""] {
            assert_eq!(map_check_state(s), CheckStatus::Skipped, "input: {s}");
        }
    }

    #[test]
    fn map_check_state_unknown_provider_value_is_skipped_not_failed() {
        // Defensive: a future CI system emitting "FLAKY" must NOT fold into
        // `Failed` — that would trigger a bogus `ci-failed` reaction. The
        // safe fallback is `Skipped` (excluded from the CI-failing summary).
        assert_eq!(map_check_state("FLAKY"), CheckStatus::Skipped);
    }

    #[test]
    fn map_check_state_is_case_insensitive_and_trims() {
        assert_eq!(map_check_state("  success  "), CheckStatus::Passed);
        assert_eq!(map_check_state("In_Progress"), CheckStatus::Running);
    }

    #[test]
    fn parse_ci_checks_handles_missing_state_field() {
        // Defensive: a check with no `state` at all must land on `Skipped`
        // and `conclusion: None` — not unwind, not default to `Passed`. A
        // future `RawCheck` refactor that drops the `#[serde(default)]` on
        // `state` would break this immediately, which is the point.
        let json = r#"[{"name":"mystery","link":""}]"#;
        let checks = parse_ci_checks(json).unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Skipped);
        assert_eq!(checks[0].conclusion, None);
        assert_eq!(checks[0].url, None);
    }

    #[test]
    fn parse_ci_checks_unknown_state_is_skipped_not_failed() {
        // End-to-end of the "new CI system emits FLAKY" path — we want
        // the unit test to exercise the full `parse_ci_checks` flow, not
        // just `map_check_state` in isolation.
        let json = r#"[{"name":"flaky-runner","state":"FLAKY","link":""}]"#;
        let checks = parse_ci_checks(json).unwrap();
        assert_eq!(checks[0].status, CheckStatus::Skipped);
        // `conclusion` still preserves the raw string for the
        // `ci-failed` reaction to show the user what happened.
        assert_eq!(checks[0].conclusion.as_deref(), Some("FLAKY"));
    }

    #[test]
    fn parse_ci_checks_preserves_conclusion_verbatim() {
        let json = r#"
        [
          {"name":"test","state":"FAILURE","link":"https://ci/1"},
          {"name":"lint","state":"SUCCESS","link":""}
        ]
        "#;
        let checks = parse_ci_checks(json).unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].status, CheckStatus::Failed);
        assert_eq!(checks[0].conclusion.as_deref(), Some("FAILURE"));
        assert_eq!(checks[0].url.as_deref(), Some("https://ci/1"));
        // Empty `link` string becomes None — cleaner for downstream.
        assert_eq!(checks[1].url, None);
    }

    #[test]
    fn summarize_ci_empty_is_none() {
        assert_eq!(summarize_ci(&[]), CiStatus::None);
    }

    #[test]
    fn summarize_ci_any_failed_dominates() {
        let checks = vec![
            CheckRun {
                name: "a".into(),
                status: CheckStatus::Passed,
                url: None,
                conclusion: None,
            },
            CheckRun {
                name: "b".into(),
                status: CheckStatus::Failed,
                url: None,
                conclusion: None,
            },
            CheckRun {
                name: "c".into(),
                status: CheckStatus::Running,
                url: None,
                conclusion: None,
            },
        ];
        assert_eq!(summarize_ci(&checks), CiStatus::Failing);
    }

    #[test]
    fn summarize_ci_pending_before_passing() {
        let checks = vec![
            CheckRun {
                name: "a".into(),
                status: CheckStatus::Passed,
                url: None,
                conclusion: None,
            },
            CheckRun {
                name: "b".into(),
                status: CheckStatus::Pending,
                url: None,
                conclusion: None,
            },
        ];
        assert_eq!(summarize_ci(&checks), CiStatus::Pending);
    }

    #[test]
    fn summarize_ci_all_skipped_is_none() {
        // Matches TS "only report passing if at least one check actually
        // passed (not all skipped)".
        let checks = vec![CheckRun {
            name: "a".into(),
            status: CheckStatus::Skipped,
            url: None,
            conclusion: None,
        }];
        assert_eq!(summarize_ci(&checks), CiStatus::None);
    }

    #[test]
    fn summarize_ci_at_least_one_pass_is_passing() {
        let checks = vec![
            CheckRun {
                name: "a".into(),
                status: CheckStatus::Passed,
                url: None,
                conclusion: None,
            },
            CheckRun {
                name: "b".into(),
                status: CheckStatus::Skipped,
                url: None,
                conclusion: None,
            },
        ];
        assert_eq!(summarize_ci(&checks), CiStatus::Passing);
    }

    #[test]
    fn parse_reviews_maps_states_and_missing_author() {
        let json = r#"
        {
          "reviews": [
            {"author": {"login": "alice"}, "state": "APPROVED", "body": "lgtm"},
            {"author": {"login": "bob"},   "state": "CHANGES_REQUESTED", "body": "nope"},
            {"author": null,               "state": "COMMENTED", "body": ""}
          ]
        }
        "#;
        let reviews = parse_reviews(json).unwrap();
        assert_eq!(reviews.len(), 3);
        assert_eq!(reviews[0].author, "alice");
        assert_eq!(reviews[0].state, ReviewState::Approved);
        assert_eq!(reviews[0].body.as_deref(), Some("lgtm"));
        assert_eq!(reviews[1].state, ReviewState::ChangesRequested);
        // Null author → "unknown" sentinel (matches TS line 749).
        assert_eq!(reviews[2].author, "unknown");
        // Empty body → None (don't carry empty strings downstream).
        assert_eq!(reviews[2].body, None);
    }

    #[test]
    fn parse_reviews_unknown_state_falls_back_to_commented() {
        let json = r#"{"reviews":[{"author":{"login":"x"},"state":"BAZINGA","body":""}]}"#;
        let reviews = parse_reviews(json).unwrap();
        assert_eq!(reviews[0].state, ReviewState::Commented);
    }

    #[test]
    fn parse_review_decision_handles_null_and_known_values() {
        assert_eq!(
            parse_review_decision(r#"{"reviewDecision":"APPROVED"}"#).unwrap(),
            ReviewDecision::Approved
        );
        assert_eq!(
            parse_review_decision(r#"{"reviewDecision":"CHANGES_REQUESTED"}"#).unwrap(),
            ReviewDecision::ChangesRequested
        );
        assert_eq!(
            parse_review_decision(r#"{"reviewDecision":"REVIEW_REQUIRED"}"#).unwrap(),
            ReviewDecision::Pending
        );
        // `null` from gh becomes None in Rust → ReviewDecision::None.
        assert_eq!(
            parse_review_decision(r#"{"reviewDecision":null}"#).unwrap(),
            ReviewDecision::None
        );
        // Missing key → same thing.
        assert_eq!(
            parse_review_decision(r#"{}"#).unwrap(),
            ReviewDecision::None
        );
    }

    #[test]
    fn parse_review_comments_prefers_line_over_original_line() {
        let json = r#"
        [
          {
            "id": 12345,
            "user": {"login": "alice"},
            "body": "nit: rename foo",
            "path": "src/foo.rs",
            "line": 42,
            "original_line": 40,
            "html_url": "https://github.com/a/b/pull/1#r12345"
          }
        ]
        "#;
        let comments = parse_review_comments(json).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, "12345");
        assert_eq!(comments[0].author, "alice");
        assert_eq!(comments[0].body, "nit: rename foo");
        assert_eq!(comments[0].path.as_deref(), Some("src/foo.rs"));
        assert_eq!(comments[0].line, Some(42));
        // Phase B limitation: resolution status isn't fetched via REST.
        assert!(!comments[0].is_resolved);
    }

    #[test]
    fn parse_review_comments_falls_back_to_original_line_when_current_is_null() {
        // After a force-push, GitHub nulls out `line` but keeps
        // `original_line`. We want to surface *something* rather than a
        // bare comment with no coordinates.
        let json = r#"
        [{
            "id": 9, "user": {"login": "a"}, "body": "b",
            "path": "f.rs", "line": null, "original_line": 12,
            "html_url": "u"
        }]
        "#;
        let comments = parse_review_comments(json).unwrap();
        assert_eq!(comments[0].line, Some(12));
    }

    #[test]
    fn parse_raw_mergeability_pulls_four_fields() {
        let json = r#"{
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "mergeStateStatus": "CLEAN",
            "isDraft": false
        }"#;
        let raw = parse_raw_mergeability(json).unwrap();
        assert_eq!(raw.mergeable, "MERGEABLE");
        assert_eq!(raw.review_decision.as_deref(), Some("APPROVED"));
        assert_eq!(raw.merge_state_status, "CLEAN");
        assert!(!raw.is_draft);
    }

    #[test]
    fn parse_raw_mergeability_missing_fields_default_sensibly() {
        // Closed PRs can return nulls / omit fields — our deserializer
        // fills in empty strings / false / None. The caller is responsible
        // for interpreting "" as "unknown" (matches TS line 1003).
        let raw = parse_raw_mergeability("{}").unwrap();
        assert_eq!(raw.mergeable, "");
        assert_eq!(raw.review_decision, None);
        assert_eq!(raw.merge_state_status, "");
        assert!(!raw.is_draft);
    }
}
