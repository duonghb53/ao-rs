//! Pure JSON → domain-type parsers for the GitLab SCM plugin.

use ao_core::{
    AoError, AutomatedComment, AutomatedCommentSeverity, CheckRun, CheckStatus, CiStatus, PrState,
    PullRequest, Result, Review, ReviewComment, ReviewDecision, ReviewState,
};
use serde::Deserialize;

/// GitLab-specific bot usernames whose comments should be surfaced as
/// `AutomatedComment`s rather than human review feedback. Mirrors the
/// `BOT_AUTHORS` set in `packages/plugins/scm-gitlab/src/index.ts`.
const BOT_AUTHORS: &[&str] = &[
    "gitlab-bot",
    "ghost",
    "dependabot[bot]",
    "renovate[bot]",
    "sast-bot",
    "codeclimate[bot]",
    "sonarcloud[bot]",
    "snyk-bot",
];

pub(crate) fn is_bot(username: &str) -> bool {
    if BOT_AUTHORS.contains(&username) {
        return true;
    }
    if username.ends_with("[bot]") {
        return true;
    }
    // GitLab project bot convention: `project_<N>_bot`.
    if let Some(rest) = username.strip_prefix("project_") {
        if let Some(tail) = rest.strip_suffix("_bot") {
            return !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

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
    pub blocking_discussions_resolved: Option<bool>,
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
// CI: pipelines + jobs (ao-ts parity shape)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Pipeline {
    #[serde(default)]
    pub id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Job {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub web_url: Option<String>,
}

pub(crate) fn parse_pipelines(json: &str) -> Result<Vec<Pipeline>> {
    serde_json::from_str(json).map_err(|e| bad("parse pipelines", e))
}

pub(crate) fn parse_jobs(json: &str) -> Result<Vec<Job>> {
    serde_json::from_str(json).map_err(|e| bad("parse jobs", e))
}

pub(crate) fn jobs_into_check_runs(jobs: Vec<Job>) -> Vec<CheckRun> {
    jobs.into_iter()
        .map(|j| {
            let status = map_job_status(&j.status);
            let url = j.web_url.filter(|s| !s.is_empty());
            let conclusion = if j.status.trim().is_empty() {
                None
            } else {
                Some(j.status)
            };
            CheckRun {
                name: j.name,
                status,
                url,
                conclusion,
            }
        })
        .collect()
}

/// Map a GitLab job status to the normalised `CheckStatus`. Mirrors
/// `mapJobStatus` in the TS plugin — unknown statuses fall through to
/// `Failed` (fail-closed) so we never hide a broken pipeline.
pub(crate) fn map_job_status(raw: &str) -> CheckStatus {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pending" | "waiting_for_resource" | "preparing" | "created" | "scheduled" | "manual" => {
            CheckStatus::Pending
        }
        "running" => CheckStatus::Running,
        "success" => CheckStatus::Passed,
        "failed" | "canceled" | "cancelled" => CheckStatus::Failed,
        "skipped" => CheckStatus::Skipped,
        _ => CheckStatus::Failed,
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
    pub approved: Option<bool>,
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

/// Collect approver usernames (bots *included* so we don't double-count bot
/// approvals as changes-requested below).
pub(crate) fn approver_usernames(a: &Approvals) -> Vec<String> {
    a.approved_by
        .iter()
        .filter_map(|ab| {
            ab.user
                .as_ref()
                .and_then(|u| u.username.clone().or_else(|| u.name.clone()))
        })
        .filter(|s| !s.is_empty())
        .collect()
}

pub(crate) fn approvals_into_reviews(a: &Approvals) -> Vec<Review> {
    a.approved_by
        .iter()
        .map(|ab| {
            let author = ab
                .user
                .as_ref()
                .and_then(|u| u.username.clone().or_else(|| u.name.clone()))
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
    if a.approved.unwrap_or(false) {
        return ReviewDecision::Approved;
    }
    let left = a.approvals_left.unwrap_or(0);
    if left > 0 {
        return ReviewDecision::Pending;
    }
    // `approved` is false but no approvals are required — treat as "no gate".
    ReviewDecision::None
}

// ---------------------------------------------------------------------------
// Discussions → pending comments + synthesised "changes_requested" reviews
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

fn note_author(n: &Note) -> String {
    n.author
        .as_ref()
        .and_then(|u| u.username.clone().or_else(|| u.name.clone()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Unresolved, resolvable, non-bot first-notes become pending review comments.
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
            let author = note_author(n);
            if is_bot(&author) {
                continue;
            }
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

/// Each unresolved discussion from a non-bot author turns into a
/// `changes_requested` review (deduped by author, skipping anyone already
/// counted as an approver). Mirrors the TS reference.
pub(crate) fn synthesise_changes_requested_reviews(
    discussions: &[Discussion],
    approver_usernames: &[String],
) -> Vec<Review> {
    let approved: std::collections::HashSet<&str> =
        approver_usernames.iter().map(String::as_str).collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for d in discussions {
        let Some(note) = d.notes.first() else {
            continue;
        };
        if !note.resolvable || note.resolved {
            continue;
        }
        let author = note_author(note);
        if is_bot(&author) || approved.contains(author.as_str()) {
            continue;
        }
        if !seen.insert(author.clone()) {
            continue;
        }
        out.push(Review {
            author,
            state: ReviewState::ChangesRequested,
            body: None,
        });
    }
    out
}

/// Infer a severity from the comment body. Mirrors TS `inferSeverity`.
pub(crate) fn infer_severity(body: &str) -> AutomatedCommentSeverity {
    let lower = body.to_ascii_lowercase();
    if lower.contains("error")
        || lower.contains("bug")
        || lower.contains("critical")
        || lower.contains("potential issue")
    {
        return AutomatedCommentSeverity::Error;
    }
    if lower.contains("warning") || lower.contains("suggest") || lower.contains("consider") {
        return AutomatedCommentSeverity::Warning;
    }
    AutomatedCommentSeverity::Info
}

/// Bot discussions become `AutomatedComment`s with a severity heuristic.
pub(crate) fn extract_automated_comments(
    discussions: &[Discussion],
    default_url: &str,
) -> Vec<AutomatedComment> {
    let mut out = Vec::new();
    for d in discussions {
        let Some(n) = d.notes.first() else {
            continue;
        };
        let author = note_author(n);
        if !is_bot(&author) {
            continue;
        }
        let (path, line) = n
            .position
            .as_ref()
            .map(|p| {
                let path = p.new_path.clone().or_else(|| p.old_path.clone());
                let line = p.new_line.or(p.old_line);
                (path.filter(|s| !s.is_empty()), line)
            })
            .unwrap_or((None, None));
        out.push(AutomatedComment {
            id: n.id.to_string(),
            bot_name: author,
            body: n.body.clone(),
            path,
            line,
            severity: infer_severity(&n.body),
            url: n.web_url.clone().unwrap_or_else(|| default_url.to_string()),
        });
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
            blocking_discussions_resolved: None,
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
            approved: Some(false),
            approvals_left: Some(0),
            approved_by: vec![],
        };
        assert_eq!(review_decision_from_approvals(&a), ReviewDecision::None);
    }

    #[test]
    fn review_decision_maps_approved_flag() {
        let a = Approvals {
            approved: Some(true),
            approvals_left: Some(0),
            approved_by: vec![],
        };
        assert_eq!(review_decision_from_approvals(&a), ReviewDecision::Approved);
    }

    #[test]
    fn review_decision_pending_when_approvals_left() {
        let a = Approvals {
            approved: Some(false),
            approvals_left: Some(2),
            approved_by: vec![],
        };
        assert_eq!(review_decision_from_approvals(&a), ReviewDecision::Pending);
    }

    #[test]
    fn is_bot_matches_suffix_and_project_bots() {
        assert!(is_bot("gitlab-bot"));
        assert!(is_bot("renovate[bot]"));
        assert!(is_bot("project_42_bot"));
        assert!(!is_bot("project__bot"));
        assert!(!is_bot("project_abc_bot"));
        assert!(!is_bot("alice"));
    }

    #[test]
    fn map_job_status_covers_all_gitlab_statuses() {
        assert_eq!(map_job_status("success"), CheckStatus::Passed);
        assert_eq!(map_job_status("failed"), CheckStatus::Failed);
        assert_eq!(map_job_status("canceled"), CheckStatus::Failed);
        assert_eq!(map_job_status("running"), CheckStatus::Running);
        assert_eq!(map_job_status("pending"), CheckStatus::Pending);
        assert_eq!(map_job_status("manual"), CheckStatus::Pending);
        assert_eq!(map_job_status("created"), CheckStatus::Pending);
        assert_eq!(map_job_status("waiting_for_resource"), CheckStatus::Pending);
        assert_eq!(map_job_status("preparing"), CheckStatus::Pending);
        assert_eq!(map_job_status("scheduled"), CheckStatus::Pending);
        assert_eq!(map_job_status("skipped"), CheckStatus::Skipped);
        // Fail-closed on unknown status.
        assert_eq!(map_job_status("new_status"), CheckStatus::Failed);
    }

    #[test]
    fn jobs_into_check_runs_preserves_name_and_url() {
        let jobs = vec![
            Job {
                name: "build".into(),
                status: "success".into(),
                web_url: Some("https://ci/1".into()),
            },
            Job {
                name: "lint".into(),
                status: "failed".into(),
                web_url: Some("".into()),
            },
        ];
        let checks = jobs_into_check_runs(jobs);
        assert_eq!(checks[0].name, "build");
        assert_eq!(checks[0].status, CheckStatus::Passed);
        assert_eq!(checks[0].url.as_deref(), Some("https://ci/1"));
        assert_eq!(checks[1].name, "lint");
        assert_eq!(checks[1].status, CheckStatus::Failed);
        assert!(checks[1].url.is_none());
    }

    #[test]
    fn approvals_into_reviews_returns_unknown_for_null_user() {
        let a = Approvals {
            approved: Some(false),
            approvals_left: Some(1),
            approved_by: vec![ApprovedBy { user: None }],
        };
        let reviews = approvals_into_reviews(&a);
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].author, "unknown");
    }

    #[test]
    fn synthesise_changes_requested_dedupes_and_respects_approvers() {
        let discussions: Vec<Discussion> = serde_json::from_str(
            r#"[
                {"notes":[{"id":1,"author":{"username":"alice"},"body":"x","resolvable":true,"resolved":false}]},
                {"notes":[{"id":2,"author":{"username":"alice"},"body":"y","resolvable":true,"resolved":false}]},
                {"notes":[{"id":3,"author":{"username":"gitlab-bot"},"body":"z","resolvable":true,"resolved":false}]},
                {"notes":[{"id":4,"author":{"username":"carol"},"body":"w","resolvable":true,"resolved":false}]}
            ]"#,
        )
        .unwrap();
        let approvers = vec!["bob".to_string()];
        let reviews = synthesise_changes_requested_reviews(&discussions, &approvers);
        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].author, "alice");
        assert_eq!(reviews[0].state, ReviewState::ChangesRequested);
        assert_eq!(reviews[1].author, "carol");

        // Alice as approver suppresses the synthesised review.
        let approvers = vec!["alice".to_string()];
        let reviews = synthesise_changes_requested_reviews(&discussions, &approvers);
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].author, "carol");
    }

    #[test]
    fn extract_pending_comments_skips_bots_and_resolved() {
        let discussions: Vec<Discussion> = serde_json::from_str(
            r#"[
                {"notes":[{"id":1,"author":{"username":"alice"},"body":"Fix this","resolvable":true,"resolved":false,"position":{"new_path":"src/foo.ts","new_line":10}}]},
                {"notes":[{"id":2,"author":{"username":"bob"},"body":"resolved","resolvable":true,"resolved":true}]},
                {"notes":[{"id":3,"author":{"username":"project_99_bot"},"body":"bot comment","resolvable":true,"resolved":false}]},
                {"notes":[{"id":4,"author":{"username":"carol"},"body":"system","resolvable":false,"resolved":false}]}
            ]"#,
        )
        .unwrap();
        let comments = extract_pending_comments(&discussions, "https://gitlab/mr");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "alice");
        assert_eq!(comments[0].path.as_deref(), Some("src/foo.ts"));
        assert_eq!(comments[0].line, Some(10));
    }

    #[test]
    fn extract_automated_comments_severity_buckets() {
        let discussions: Vec<Discussion> = serde_json::from_str(
            r#"[
                {"notes":[{"id":1,"author":{"username":"sast-bot"},"body":"Error: build failed","resolvable":true,"resolved":false}]},
                {"notes":[{"id":2,"author":{"username":"sast-bot"},"body":"Warning: deprecated API","resolvable":true,"resolved":false}]},
                {"notes":[{"id":3,"author":{"username":"sast-bot"},"body":"Deployed to staging","resolvable":true,"resolved":false}]},
                {"notes":[{"id":4,"author":{"username":"alice"},"body":"nit","resolvable":true,"resolved":false}]}
            ]"#,
        )
        .unwrap();
        let comments = extract_automated_comments(&discussions, "https://gitlab/mr");
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].severity, AutomatedCommentSeverity::Error);
        assert_eq!(comments[1].severity, AutomatedCommentSeverity::Warning);
        assert_eq!(comments[2].severity, AutomatedCommentSeverity::Info);
        assert!(comments.iter().all(|c| c.bot_name == "sast-bot"));
    }

    #[test]
    fn approver_usernames_skips_null_users() {
        let a = Approvals {
            approved: Some(true),
            approvals_left: Some(0),
            approved_by: vec![
                ApprovedBy {
                    user: Some(User {
                        username: Some("alice".into()),
                        name: None,
                    }),
                },
                ApprovedBy { user: None },
                ApprovedBy {
                    user: Some(User {
                        username: None,
                        name: Some("Bob".into()),
                    }),
                },
            ],
        };
        let names = approver_usernames(&a);
        assert_eq!(names, vec!["alice", "Bob"]);
    }
}
