//! Auto-select GitHub vs GitLab SCM from PR URL shape.

use ao_core::{
    CiStatus, MergeReadiness, PrState, PullRequest, ReviewDecision, Scm, Session,
};
use ao_plugin_scm_github::GitHubScm;
use ao_plugin_scm_gitlab::GitLabScm;
use async_trait::async_trait;

#[derive(Debug, Default, Clone)]
pub(crate) struct AutoScm {
    github: GitHubScm,
    gitlab: GitLabScm,
}

impl AutoScm {
    pub(crate) fn new() -> Self {
        Self {
            github: GitHubScm::new(),
            gitlab: GitLabScm::new(),
        }
    }

    fn is_gitlab_pr(pr: &PullRequest) -> bool {
        // Self-hosted GitLab still uses this path segment.
        pr.url.contains("/-/merge_requests/")
    }

    fn delegate<'a>(&'a self, pr: &PullRequest) -> &'a dyn Scm {
        if Self::is_gitlab_pr(pr) {
            &self.gitlab
        } else {
            &self.github
        }
    }
}

#[async_trait]
impl Scm for AutoScm {
    fn name(&self) -> &str {
        "auto"
    }

    async fn detect_pr(&self, session: &Session) -> ao_core::Result<Option<PullRequest>> {
        // Try GitLab first — safe because GitLab `detect_pr` is tolerant and
        // returns `Ok(None)` for "can't detect".
        if let Ok(Some(pr)) = self.gitlab.detect_pr(session).await {
            return Ok(Some(pr));
        }
        self.github.detect_pr(session).await
    }

    async fn pr_state(&self, pr: &PullRequest) -> ao_core::Result<PrState> {
        self.delegate(pr).pr_state(pr).await
    }

    async fn ci_checks(&self, pr: &PullRequest) -> ao_core::Result<Vec<ao_core::CheckRun>> {
        self.delegate(pr).ci_checks(pr).await
    }

    async fn ci_status(&self, pr: &PullRequest) -> ao_core::Result<CiStatus> {
        self.delegate(pr).ci_status(pr).await
    }

    async fn reviews(&self, pr: &PullRequest) -> ao_core::Result<Vec<ao_core::Review>> {
        self.delegate(pr).reviews(pr).await
    }

    async fn review_decision(&self, pr: &PullRequest) -> ao_core::Result<ReviewDecision> {
        self.delegate(pr).review_decision(pr).await
    }

    async fn pending_comments(
        &self,
        pr: &PullRequest,
    ) -> ao_core::Result<Vec<ao_core::ReviewComment>> {
        self.delegate(pr).pending_comments(pr).await
    }

    async fn mergeability(&self, pr: &PullRequest) -> ao_core::Result<MergeReadiness> {
        self.delegate(pr).mergeability(pr).await
    }

    async fn merge(
        &self,
        pr: &PullRequest,
        method: Option<ao_core::MergeMethod>,
    ) -> ao_core::Result<()> {
        self.delegate(pr).merge(pr, method).await
    }
}
