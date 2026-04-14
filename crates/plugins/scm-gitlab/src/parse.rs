//! Pure JSON → domain-type parsers for the GitLab SCM plugin.

use ao_core::{
    AoError, CheckRun, CheckStatus, CiStatus, PrState, PullRequest, Result, Review, ReviewComment,
    ReviewDecision, ReviewState,
};
use serde::Deserialize;

fn bad(msg: impl Into<String>, err: impl std::fmt::Display) -> AoError {
    AoError::Scm(format!("{}: {}", msg.into(), err))
}

// ---------------------------------------------------------------------------
// Merge requests (list + view)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MergeRequest {
    #[serde(default)]
    pub iid: Option<u32>,
    #[serde(default)]
    pub web_url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub source_branch: Option<String>,
    #[serde(default)]
    pub target_branch: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub draft: Option<bool>,
    // Older GitLab versions used `work_in_progress` before `draft`.
    #[serde(default)]
    pub work_in_progress: Option<bool>,
    #[serde(default)]
    pub merge_status: Option<String>,
    #[serde(default)]
    pub has_conflicts: Option<bool>,
    #[serde(default)]
    pub head_pipeline: Option<HeadPipeline>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HeadPipeline {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub web_url: Option<String>,
}

impl MergeRequest {
    pub(crate) fn into_pull_request(self, owner: &str, repo: &str) -> PullRequest {
        let is_draft = self.is_draft();
        PullRequest {
            number: self.iid.unwrap_or_default(),
            url: self
                .web_url
                .unwrap_or_else(|| format!("https://gitlab.com/{owner}/{repo}")),
            title: self.title.unwrap_or_default(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: self.source_branch.unwrap_or_default(),
            base_branch: self.target_branch.unwrap_or_default(),
            is_draft,
        }
    }

    pub(crate) fn is_draft(&self) -> bool {
        if self.draft.unwrap_or(false) || self.work_in_progress.unwrap_or(false) {
            return true;
        }
        let t = self
            .title
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        t.starts_with("draft:") || t.starts_with("wip:")
    }
}

pub(crate) fn parse_mr_list(json: &str) -> Result<Vec<MergeRequest>> {
    serde_json::from_str(json).map_err(|e| bad("parse mr list", e))
}

pub(crate) fn parse_mr_view(json: &str) -> Result<MergeRequest> {
    serde_json::from_str(json).map_err(|e| bad("parse mr view", e))
}

pub(crate) fn map_pr_state(raw: &str) -> PrState {
    match raw.trim().to_ascii_lowercase().as_str() {
        "merged" => PrState::Merged,
        "closed" => PrState::Closed,
        _ => PrState::Open,
    }
}

// ---------------------------------------------------------------------------
// CI (head pipeline)
// ---------------------------------------------------------------------------

pub(crate) fn extract_ci_checks(mr: &MergeRequest) -> Vec<CheckRun> {
    let Some(p) = mr.head_pipeline.as_ref() else {
        return Vec::new();
    };
    let status_raw = p.status.as_deref().unwrap_or("").to_string();
    let status = map_pipeline_status(&status_raw);
    let url = p.web_url.clone().filter(|s| !s.is_empty());
    let conclusion = if status_raw.trim().is_empty() {
        None
    } else {
        Some(status_raw)
    };
    vec![CheckRun {
        name: "pipeline".into(),
        status,
        url,
        conclusion,
    }]
}

fn map_pipeline_status(raw: &str) -> CheckStatus {
    match raw.trim().to_ascii_lowercase().as_str() {
        "running" => CheckStatus::Running,
        "pending" => CheckStatus::Pending,
        "created" => CheckStatus::Pending,
        "waiting_for_resource" => CheckStatus::Pending,
        "preparing" => CheckStatus::Pending,
        "manual" => CheckStatus::Pending,
        "scheduled" => CheckStatus::Pending,
        "success" => CheckStatus::Passed,
        "failed" | "canceled" | "cancelled" => CheckStatus::Failed,
        "skipped" => CheckStatus::Skipped,
        _ => CheckStatus::Skipped,
    }
}

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
// Approvals → reviews + decision
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Approvals {
    #[serde(default)]
    pub approvals_required: Option<u32>,
    #[serde(default)]
    pub approvals_left: Option<u32>,
    #[serde(default)]
    pub approved_by: Vec<ApprovedBy>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ApprovedBy {
    #[serde(default)]
    pub user: Option<User>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct User {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

pub(crate) fn parse_approvals(json: &str) -> Result<Approvals> {
    serde_json::from_str(json).map_err(|e| bad("parse approvals", e))
}

pub(crate) fn approvals_into_reviews(a: Approvals) -> Vec<Review> {
    a.approved_by
        .into_iter()
        .map(|ab| {
            let author = ab
                .user
                .and_then(|u| u.username.or(u.name))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            Review {
                author,
                state: ReviewState::Approved,
                body: None,
            }
        })
        .collect()
}

pub(crate) fn review_decision_from_approvals(a: &Approvals) -> ReviewDecision {
    let required = a.approvals_required.unwrap_or(0);
    if required == 0 {
        return ReviewDecision::None;
    }
    let left = a.approvals_left.unwrap_or(required);
    if left == 0 {
        ReviewDecision::Approved
    } else {
        ReviewDecision::Pending
    }
}

// ---------------------------------------------------------------------------
// Discussions → pending comments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Discussion {
    #[serde(default)]
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Note {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub resolvable: bool,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub author: Option<User>,
    #[serde(default)]
    pub position: Option<Position>,
    #[serde(default)]
    pub web_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Position {
    #[serde(default)]
    pub new_path: Option<String>,
    #[serde(default)]
    pub old_path: Option<String>,
    #[serde(default)]
    pub new_line: Option<u32>,
    #[serde(default)]
    pub old_line: Option<u32>,
}

pub(crate) fn parse_discussions(json: &str) -> Result<Vec<Discussion>> {
    serde_json::from_str(json).map_err(|e| bad("parse discussions", e))
}

pub(crate) fn extract_pending_comments(
    discussions: &[Discussion],
    default_url: &str,
) -> Vec<ReviewComment> {
    let mut out = Vec::new();
    for d in discussions {
        for n in &d.notes {
            if !n.resolvable || n.resolved {
                continue;
            }
            let author = n
                .author
                .as_ref()
                .and_then(|u| u.username.clone().or_else(|| u.name.clone()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            let (path, line) = n
                .position
                .as_ref()
                .map(|p| {
                    let path = p.new_path.clone().or_else(|| p.old_path.clone());
                    let line = p.new_line.or(p.old_line);
                    (path.filter(|s| !s.is_empty()), line)
                })
                .unwrap_or((None, None));
            out.push(ReviewComment {
                id: n.id.to_string(),
                author,
                body: n.body.clone(),
                path,
                line,
                is_resolved: false,
                url: n.web_url.clone().unwrap_or_else(|| default_url.to_string()),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_request_is_draft_detects_title_prefix() {
        let mr = MergeRequest {
            iid: Some(1),
            web_url: None,
            title: Some("WIP: test".into()),
            source_branch: None,
            target_branch: None,
            state: None,
            draft: None,
            work_in_progress: None,
            merge_status: None,
            has_conflicts: None,
            head_pipeline: None,
        };
        assert!(mr.is_draft());
    }

    #[test]
    fn summarize_ci_empty_is_none() {
        assert_eq!(summarize_ci(&[]), CiStatus::None);
    }

    #[test]
    fn review_decision_required_zero_is_none() {
        let a = Approvals {
            approvals_required: Some(0),
            approvals_left: Some(0),
            approved_by: vec![],
        };
        assert_eq!(review_decision_from_approvals(&a), ReviewDecision::None);
    }
}
