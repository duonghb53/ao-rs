//! Domain types for the `Scm` and `Tracker` plugin slots.
//!
//! Mirrors the SCM/Tracker types in `packages/core/src/types.ts` (lines 500–820
//! in the reference repo), Rustified:
//!
//! - PascalCase struct fields become snake_case.
//! - `Date` becomes a plain `String` we don't interpret yet — the reaction
//!   engine only needs *ordering* of reviews, not wall-clock arithmetic, so
//!   we skip the chrono dep until something actually needs it.
//! - TS unions like `"open" | "merged" | "closed"` become snake_case enums
//!   with `#[serde(rename_all = "snake_case")]`.
//! - Several speculative fields from TS (batch enrichment, webhook parsing,
//!   GraphQL optimisation) are intentionally left out — Slice 2's reactions
//!   can be implemented without them and we can add them back if a real use
//!   case shows up.
//!
//! These types are consumed by `Scm` and `Tracker` in `traits.rs`, and later
//! by the reaction engine in `reactions.rs`.

use serde::{Deserialize, Serialize};

// =============================================================================
// PR types
// =============================================================================

/// Metadata about a pull request. Returned by `Scm::detect_pr` and carried
/// into every other `Scm` method. The TS reference calls this `PRInfo`.
///
/// Kept intentionally small — the lifecycle loop derives everything it needs
/// from `(owner, repo, number)` via follow-up calls to `pr_state`, `ci_status`,
/// and `review_decision`. Extra PR fields live on the enrichment structs the
/// plugin can return piecemeal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    /// GitHub/GitLab PR number. `u32` is deliberate and specific to
    /// SCM-style numeric PR ids — `Tracker::Issue::id` is `String`
    /// because issue trackers (Linear `LIN-1327`, Jira `PROJ-42`,
    /// GitHub `#42`) don't share a numeric type.
    pub number: u32,
    pub url: String,
    pub title: String,
    pub owner: String,
    pub repo: String,
    /// Head branch of the PR (the session's branch).
    pub branch: String,
    /// Base branch the PR targets.
    pub base_branch: String,
    pub is_draft: bool,
}

/// Open/merged/closed. TS exports this both as a string union and as
/// `PR_STATE` constants — we use a plain enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// How to merge a PR. Mirrors GitHub's three merge methods exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    /// Default merge commit. Safe, preserves history.
    #[default]
    Merge,
    Squash,
    Rebase,
}

// =============================================================================
// CI types
// =============================================================================

/// Status of an individual CI check. TS calls this `CICheck`; we use
/// `CheckRun` to match GitHub's own API naming (`check_run` events, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub name: String,
    pub status: CheckStatus,
    /// URL to the check run (for linking humans to logs). Optional because
    /// some CI providers don't publish a public URL until the run completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Provider-specific conclusion string — GitHub's `check_run.conclusion`:
    /// `success`, `failure`, `neutral`, `cancelled`, `skipped`, `timed_out`,
    /// `action_required`, `stale`. Kept as an opaque `String` because:
    /// (1) different providers emit different sets and we don't want an
    /// enum churn every time a new value appears; (2) the `status` field
    /// above is our normalized view — conclusion is the raw trailer the
    /// `ci-failed` reaction can include in its message to the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

/// Rolled-up CI summary for a PR. `None` means "no CI configured".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
    Pending,
    Passing,
    Failing,
    None,
}

// =============================================================================
// Review types
// =============================================================================

/// A review on a PR. TS includes a `submittedAt: Date` here — we drop it
/// until the reaction engine actually needs ordering beyond "is there one
/// newer than the last status check", at which point a string is enough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub author: String,
    pub state: ReviewState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

/// Overall review decision — what GitHub shows on the PR header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    Pending,
    /// No review required / no reviewers assigned.
    None,
}

/// A single unresolved review comment. The reaction engine forwards these
/// verbatim to the agent when handling `changes-requested`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: String,
    pub author: String,
    pub body: String,
    /// File path the comment is pinned to (if inline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Line number inside `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub is_resolved: bool,
    pub url: String,
}

// =============================================================================
// Merge readiness
// =============================================================================

/// Result of `Scm::mergeability`. The reaction engine reads this to decide
/// whether the `approved-and-green` reaction should fire. Every bool is
/// "true means this particular gate is green".
///
/// `mergeable` and `no_conflicts` look redundant but aren't quite:
/// - `mergeable` is the provider's top-line verdict — GitHub aggregates
///   branch protection, required reviews, required checks, *and* conflicts
///   into one bool.
/// - `no_conflicts` is specifically "the branch has no text-level merge
///   conflicts with base".
///
/// A PR can be `mergeable: false, no_conflicts: true` (branch is clean
/// but a required review is missing) and that distinction matters for
/// reaction routing: `changes-requested` vs `merge-conflicts` are
/// different reaction keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeReadiness {
    pub mergeable: bool,
    pub ci_passing: bool,
    pub approved: bool,
    pub no_conflicts: bool,
    /// Human-readable reasons the PR isn't mergeable yet. Empty when all
    /// gates are green.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
}

impl MergeReadiness {
    /// `true` iff every gate passes and `blockers` is empty. Convenience
    /// for reaction-engine decision points that don't care *why* a PR is
    /// blocked, only *whether*.
    pub fn is_ready(&self) -> bool {
        self.mergeable
            && self.ci_passing
            && self.approved
            && self.no_conflicts
            && self.blockers.is_empty()
    }
}

// =============================================================================
// Issue tracker types
// =============================================================================

/// An issue in a tracker (GitHub Issues, Linear, Jira, ...). Slice 2 only
/// needs this for `Tracker::get_issue`; `Tracker::branch_name` and friends
/// are string-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub state: IssueState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    Open,
    InProgress,
    Closed,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_roundtrips_yaml() {
        let pr = PullRequest {
            number: 42,
            url: "https://github.com/acme/widgets/pull/42".into(),
            title: "fix the widgets".into(),
            owner: "acme".into(),
            repo: "widgets".into(),
            branch: "ao-3a4b5c6d".into(),
            base_branch: "main".into(),
            is_draft: false,
        };
        let yaml = serde_yaml::to_string(&pr).unwrap();
        let back: PullRequest = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(pr, back);
    }

    #[test]
    fn pr_state_uses_snake_case() {
        let yaml = serde_yaml::to_string(&PrState::Merged).unwrap();
        assert_eq!(yaml.trim(), "merged");
        let parsed: PrState = serde_yaml::from_str("open").unwrap();
        assert_eq!(parsed, PrState::Open);
    }

    #[test]
    fn merge_method_default_is_merge() {
        assert_eq!(MergeMethod::default(), MergeMethod::Merge);
    }

    #[test]
    fn check_run_optional_fields_skip_when_none() {
        let run = CheckRun {
            name: "ci/build".into(),
            status: CheckStatus::Passed,
            url: None,
            conclusion: None,
        };
        let yaml = serde_yaml::to_string(&run).unwrap();
        // No `url:` or `conclusion:` keys at all — skip_serializing_if eats them.
        assert!(!yaml.contains("url"));
        assert!(!yaml.contains("conclusion"));
        let back: CheckRun = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(run, back);
    }

    #[test]
    fn check_status_variants_serialize_snake_case() {
        assert_eq!(
            serde_yaml::to_string(&CheckStatus::Running).unwrap().trim(),
            "running"
        );
        assert_eq!(
            serde_yaml::to_string(&CheckStatus::Failed).unwrap().trim(),
            "failed"
        );
    }

    #[test]
    fn ci_status_none_variant_roundtrips() {
        // "None" the variant, not `Option::None` — if this ever starts
        // serializing as the YAML null `~` we've broken config compat.
        let yaml = serde_yaml::to_string(&CiStatus::None).unwrap();
        assert_eq!(yaml.trim(), "none");
        let back: CiStatus = serde_yaml::from_str("none").unwrap();
        assert_eq!(back, CiStatus::None);
    }

    #[test]
    fn review_state_changes_requested_serializes_correctly() {
        let review = Review {
            author: "alice".into(),
            state: ReviewState::ChangesRequested,
            body: Some("needs work".into()),
        };
        let yaml = serde_yaml::to_string(&review).unwrap();
        assert!(yaml.contains("state: changes_requested"));
        let back: Review = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(review, back);
    }

    #[test]
    fn review_comment_inline_fields_optional() {
        let comment = ReviewComment {
            id: "c1".into(),
            author: "bot".into(),
            body: "nit: rename foo".into(),
            path: Some("src/foo.rs".into()),
            line: Some(42),
            is_resolved: false,
            url: "https://github.com/acme/widgets/pull/42#discussion_r1".into(),
        };
        let back: ReviewComment =
            serde_yaml::from_str(&serde_yaml::to_string(&comment).unwrap()).unwrap();
        assert_eq!(comment, back);
    }

    #[test]
    fn merge_readiness_is_ready_requires_every_gate() {
        let green = MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        };
        assert!(green.is_ready());

        // Any single false flips it.
        for mutate in [
            |r: &mut MergeReadiness| r.mergeable = false,
            |r: &mut MergeReadiness| r.ci_passing = false,
            |r: &mut MergeReadiness| r.approved = false,
            |r: &mut MergeReadiness| r.no_conflicts = false,
            |r: &mut MergeReadiness| r.blockers.push("branch protection".into()),
        ] {
            let mut r = green.clone();
            mutate(&mut r);
            assert!(!r.is_ready());
        }
    }

    #[test]
    fn issue_roundtrip_with_labels() {
        let issue = Issue {
            id: "#7".into(),
            title: "add dark mode".into(),
            description: "users keep asking".into(),
            url: "https://github.com/acme/widgets/issues/7".into(),
            state: IssueState::InProgress,
            labels: vec!["feature".into(), "ui".into()],
            assignee: Some("bob".into()),
        };
        let back: Issue = serde_yaml::from_str(&serde_yaml::to_string(&issue).unwrap()).unwrap();
        assert_eq!(issue, back);
    }

    #[test]
    fn issue_state_in_progress_uses_snake_case() {
        let yaml = serde_yaml::to_string(&IssueState::InProgress).unwrap();
        assert_eq!(yaml.trim(), "in_progress");
    }
}
