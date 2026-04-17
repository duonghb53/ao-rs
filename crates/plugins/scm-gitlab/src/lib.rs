//! GitLab SCM plugin — GitLab Merge Requests via the GitLab REST API.
//!
//! Behaviour parity target: `packages/plugins/scm-gitlab/src/index.ts` in
//! ao-ts. Transport differs by design: ao-ts shells out to `glab`; we use
//! HTTPS directly so we can hermetic-test the parser/plugin end-to-end with
//! wiremock fixtures. See `docs/issues/0103-scm-gitlab-parity.md` for the
//! parity matrix.
//!
//! ## Auth
//!
//! Reads `GITLAB_TOKEN` (or `GITLAB_PRIVATE_TOKEN` / `PRIVATE_TOKEN`) from
//! the environment and sends it as `PRIVATE-TOKEN` — the same header `glab`
//! uses, so both plugins accept the same tokens.
//!
//! ## Self-hosted GitLab
//!
//! The plugin derives `base_url` from the git remote (`https://gitlab.corp/…`
//! or `git@gitlab.corp:…`) so GitLab Cloud and self-hosted instances work
//! the same way. Tests pass an explicit base URL via
//! `GitLabScm::with_base_url_and_token`.

use ao_core::{
    config::ProjectConfig, AoError, AutomatedComment, CheckRun, CiStatus, MergeMethod,
    MergeReadiness, PrState, PrSummary, PullRequest, Result, Review, ReviewComment, ReviewDecision,
    Scm, ScmWebhookEvent, ScmWebhookRequest, ScmWebhookVerificationResult, Session,
};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

pub(crate) mod parse;
pub(crate) mod webhook;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct GitLabScm {
    client: reqwest::Client,
    /// Override `https://<host>` for tests (e.g. wiremock server base URL).
    base_url_override: Option<String>,
    /// Override token for tests; production reads from env each call.
    token_override: Option<String>,
}

impl Default for GitLabScm {
    fn default() -> Self {
        Self::new()
    }
}

impl GitLabScm {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("failed to build reqwest client");
        Self {
            client,
            base_url_override: None,
            token_override: None,
        }
    }

    pub fn with_base_url_and_token(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        let mut me = Self::new();
        me.base_url_override = Some(base_url.into());
        me.token_override = Some(token.into());
        me
    }
}

#[async_trait]
impl Scm for GitLabScm {
    fn name(&self) -> &str {
        "gitlab"
    }

    async fn detect_pr(&self, session: &Session) -> Result<Option<PullRequest>> {
        // Same asymmetry as the GitHub plugin: polling-tolerant and returns
        // `Ok(None)` for any "can't detect" failure mode.
        let Some(workspace) = session.workspace_path.as_deref() else {
            return Ok(None);
        };
        let origin = match discover_origin(workspace).await {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!("detect_pr: no gitlab origin in {:?}: {e}", workspace);
                return Ok(None);
            }
        };
        let base_url = self
            .base_url_override
            .clone()
            .unwrap_or_else(|| origin.base_url.clone());
        let token = match token_from_env(self.token_override.as_deref()) {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!("detect_pr: missing gitlab token: {e}");
                return Ok(None);
            }
        };

        let api = GitLabApi::new(&self.client, &base_url, &token);
        let mrs = match api
            .list_open_merge_requests_by_source_branch(&origin.project_path, &session.branch)
            .await
        {
            Ok(mrs) => mrs,
            Err(e) => {
                tracing::debug!("detect_pr: gitlab mr list failed: {e}");
                return Ok(None);
            }
        };

        Ok(mrs.into_iter().next().map(|mr| {
            let (owner, repo) = split_owner_repo(&origin.project_path);
            mr.into_pull_request(&owner, &repo)
        }))
    }

    async fn pr_state(&self, pr: &PullRequest) -> Result<PrState> {
        let api = self.api_from_pr(pr)?;
        let mr = api.get_merge_request(&project_path(pr), pr.number).await?;
        Ok(parse::map_pr_state(mr.state.as_deref().unwrap_or("")))
    }

    async fn pr_summary(&self, pr: &PullRequest) -> Result<PrSummary> {
        // Parity with ao-ts: additions/deletions come back as 0 because the
        // GitLab MR view doesn't expose per-line counts; callers that want
        // real diff stats should call out separately. This keeps the signature
        // identical so dashboards get a usable title + state.
        let api = self.api_from_pr(pr)?;
        let mr = api.get_merge_request(&project_path(pr), pr.number).await?;
        Ok(PrSummary {
            state: parse::map_pr_state(mr.state.as_deref().unwrap_or("")),
            title: mr.title.unwrap_or_default(),
            additions: 0,
            deletions: 0,
        })
    }

    async fn ci_checks(&self, pr: &PullRequest) -> Result<Vec<CheckRun>> {
        let api = self.api_from_pr(pr)?;
        match api.list_mr_pipelines(&project_path(pr), pr.number).await {
            Ok(pipelines) => {
                let Some(latest) = pipelines.first() else {
                    return Ok(Vec::new());
                };
                let jobs = api
                    .list_pipeline_jobs(&project_path(pr), latest.id)
                    .await?;
                Ok(parse::jobs_into_check_runs(jobs))
            }
            Err(e) => Err(AoError::Scm(format!("Failed to fetch CI checks: {e}"))),
        }
    }

    async fn ci_status(&self, pr: &PullRequest) -> Result<CiStatus> {
        // Parity with ao-ts `getCISummary`: on CI fetch failure, fall back to
        // "none" when the PR is already merged/closed (common once CI history
        // has been pruned). Any other failure bubbles "failing" so the
        // reaction engine treats it conservatively.
        match self.ci_checks(pr).await {
            Ok(checks) => Ok(parse::summarize_ci(&checks)),
            Err(ci_err) => match self.pr_state(pr).await {
                Ok(PrState::Merged) | Ok(PrState::Closed) => {
                    tracing::debug!(
                        "ci_status: CI fetch failed for MR !{}, PR state fallback says closed/merged: {ci_err}",
                        pr.number
                    );
                    Ok(CiStatus::None)
                }
                _ => {
                    tracing::debug!("ci_status: CI fetch failed for MR !{}: {ci_err}", pr.number);
                    Ok(CiStatus::Failing)
                }
            },
        }
    }

    async fn reviews(&self, pr: &PullRequest) -> Result<Vec<Review>> {
        let api = self.api_from_pr(pr)?;
        let approvals = api.get_approvals(&project_path(pr), pr.number).await?;
        let mut reviews = parse::approvals_into_reviews(&approvals);
        let approvers = parse::approver_usernames(&approvals);
        // Discussions can fail independently (rate limits, auth on older
        // GitLab versions); match ao-ts by logging and returning approvals
        // alone rather than erroring the reactions-engine tick.
        match api
            .list_all_discussions(&project_path(pr), pr.number)
            .await
        {
            Ok(discussions) => reviews.extend(parse::synthesise_changes_requested_reviews(
                &discussions,
                &approvers,
            )),
            Err(e) => tracing::warn!(
                "reviews: discussions fetch failed for MR !{}: {e}",
                pr.number
            ),
        }
        Ok(reviews)
    }

    async fn review_decision(&self, pr: &PullRequest) -> Result<ReviewDecision> {
        let api = self.api_from_pr(pr)?;
        let approvals = api.get_approvals(&project_path(pr), pr.number).await?;
        Ok(parse::review_decision_from_approvals(&approvals))
    }

    async fn pending_comments(&self, pr: &PullRequest) -> Result<Vec<ReviewComment>> {
        let api = self.api_from_pr(pr)?;
        let discussions = api
            .list_all_discussions(&project_path(pr), pr.number)
            .await?;
        Ok(parse::extract_pending_comments(&discussions, &pr.url))
    }

    async fn automated_comments(&self, pr: &PullRequest) -> Result<Vec<AutomatedComment>> {
        let api = self.api_from_pr(pr)?;
        let discussions = api
            .list_all_discussions(&project_path(pr), pr.number)
            .await?;
        Ok(parse::extract_automated_comments(&discussions, &pr.url))
    }

    async fn mergeability(&self, pr: &PullRequest) -> Result<MergeReadiness> {
        let api = self.api_from_pr(pr)?;
        let mr = api.get_merge_request(&project_path(pr), pr.number).await?;
        let state = parse::map_pr_state(mr.state.as_deref().unwrap_or(""));
        if matches!(state, PrState::Merged) {
            return Ok(MergeReadiness {
                mergeable: true,
                ci_passing: true,
                approved: true,
                no_conflicts: true,
                blockers: Vec::new(),
            });
        }
        if matches!(state, PrState::Closed) {
            return Ok(MergeReadiness {
                mergeable: false,
                ci_passing: false,
                approved: false,
                no_conflicts: true,
                blockers: vec!["MR is closed".into()],
            });
        }

        let approvals = api.get_approvals(&project_path(pr), pr.number).await?;
        let ci_status = self.ci_status(pr).await?;
        Ok(compose_merge_readiness(&mr, &approvals, ci_status))
    }

    async fn merge(&self, pr: &PullRequest, method: Option<MergeMethod>) -> Result<()> {
        let api = self.api_from_pr(pr)?;
        api.merge_merge_request(&project_path(pr), pr.number, method.unwrap_or_default())
            .await
    }

    async fn close_pr(&self, pr: &PullRequest) -> Result<()> {
        let api = self.api_from_pr(pr)?;
        api.close_merge_request(&project_path(pr), pr.number).await
    }

    async fn verify_webhook(
        &self,
        request: &ScmWebhookRequest,
        project: &ProjectConfig,
    ) -> Result<ScmWebhookVerificationResult> {
        webhook::verify(request, project).await
    }

    async fn parse_webhook(
        &self,
        request: &ScmWebhookRequest,
        project: &ProjectConfig,
    ) -> Result<Option<ScmWebhookEvent>> {
        webhook::parse(request, project)
    }
}

// ---------------------------------------------------------------------------
// Merge-readiness composer (pure, testable)
// ---------------------------------------------------------------------------

pub(crate) fn compose_merge_readiness(
    mr: &parse::MergeRequest,
    approvals: &parse::Approvals,
    ci_status: CiStatus,
) -> MergeReadiness {
    let mut blockers: Vec<String> = Vec::new();

    // CI
    let ci_passing = matches!(ci_status, CiStatus::Passing | CiStatus::None);
    if !ci_passing {
        blockers.push(format!("CI is {}", ci_status_label(ci_status)));
    }

    // Reviews / approvals (parity with ao-ts: only "pending" adds a blocker).
    let decision = parse::review_decision_from_approvals(approvals);
    let approved = matches!(decision, ReviewDecision::Approved);
    if matches!(decision, ReviewDecision::Pending) {
        blockers.push("Approval required".into());
    }

    // Conflicts — check before merge_status so the more specific message wins.
    let has_conflicts = mr.has_conflicts.unwrap_or(false);
    let no_conflicts = !has_conflicts;
    if has_conflicts {
        blockers.push("Merge conflicts".into());
    }

    // GitLab's merge_status narrows why the branch can't merge when the
    // conflict check doesn't already explain it.
    let merge_status = mr.merge_status.as_deref().unwrap_or("").to_ascii_lowercase();
    match merge_status.as_str() {
        "cannot_be_merged" if no_conflicts => {
            blockers.push("Merge status: cannot be merged".into());
        }
        "checking" => blockers.push("Merge status unknown (GitLab is computing)".into()),
        _ => {}
    }

    // Unresolved blocking discussions (parity with ao-ts).
    if mr
        .blocking_discussions_resolved
        .map(|b| !b)
        .unwrap_or(false)
    {
        blockers.push("Unresolved discussions blocking merge".into());
    }

    // Draft MRs never merge cleanly.
    if mr.is_draft() {
        blockers.push("MR is still a draft".into());
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

// ---------------------------------------------------------------------------
// API client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct GitLabApi<'a> {
    client: &'a reqwest::Client,
    base_url: String,
    token: String,
}

impl<'a> GitLabApi<'a> {
    fn new(client: &'a reqwest::Client, base_url: &str, token: &str) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    fn api_base(&self) -> String {
        format!("{}/api/v4", self.base_url)
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        // GitLab supports both `PRIVATE-TOKEN` and `Authorization: Bearer`.
        // `PRIVATE-TOKEN` is simplest for PATs and is what glab uses.
        let v = HeaderValue::from_str(&self.token)
            .map_err(|e| AoError::Scm(format!("invalid gitlab token header: {e}")))?;
        h.insert("PRIVATE-TOKEN", v);
        Ok(h)
    }

    async fn get_text(&self, url: String, ctx: &str) -> Result<String> {
        let resp = self
            .client
            .get(url)
            .headers(self.headers()?)
            .send()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab {ctx} failed: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab {ctx} read failed: {e}")))?;
        if !status.is_success() {
            return Err(AoError::Scm(format!(
                "gitlab {ctx} failed: http {}: {}",
                status.as_u16(),
                body.trim()
            )));
        }
        Ok(body)
    }

    async fn list_open_merge_requests_by_source_branch(
        &self,
        project_path: &str,
        branch: &str,
    ) -> Result<Vec<parse::MergeRequest>> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/merge_requests?state=opened&source_branch={}&per_page=1",
            self.api_base(),
            encoded,
            urlencoding::encode(branch)
        );
        let body = self.get_text(url, "mr list").await?;
        parse::parse_mr_list(&body)
    }

    async fn get_merge_request(&self, project_path: &str, iid: u32) -> Result<parse::MergeRequest> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/merge_requests/{}",
            self.api_base(),
            encoded,
            iid
        );
        let body = self.get_text(url, "mr view").await?;
        parse::parse_mr_view(&body)
    }

    async fn list_mr_pipelines(
        &self,
        project_path: &str,
        iid: u32,
    ) -> Result<Vec<parse::Pipeline>> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/merge_requests/{}/pipelines",
            self.api_base(),
            encoded,
            iid
        );
        let body = self.get_text(url, "pipelines").await?;
        parse::parse_pipelines(&body)
    }

    async fn list_pipeline_jobs(
        &self,
        project_path: &str,
        pipeline_id: u64,
    ) -> Result<Vec<parse::Job>> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/pipelines/{}/jobs",
            self.api_base(),
            encoded,
            pipeline_id
        );
        let body = self.get_text(url, "pipeline jobs").await?;
        parse::parse_jobs(&body)
    }

    async fn get_approvals(&self, project_path: &str, iid: u32) -> Result<parse::Approvals> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/merge_requests/{}/approvals",
            self.api_base(),
            encoded,
            iid
        );
        let body = self.get_text(url, "approvals").await?;
        parse::parse_approvals(&body)
    }

    async fn list_all_discussions(
        &self,
        project_path: &str,
        iid: u32,
    ) -> Result<Vec<parse::Discussion>> {
        const PER_PAGE: usize = 100;
        const MAX_PAGES: u32 = 100;

        let mut all: Vec<parse::Discussion> = Vec::new();
        for page in 1..=MAX_PAGES {
            let encoded = urlencoding::encode(project_path);
            let url = format!(
                "{}/projects/{}/merge_requests/{}/discussions?per_page={PER_PAGE}&page={page}",
                self.api_base(),
                encoded,
                iid
            );
            let body = self.get_text(url, "discussions").await?;
            let page_items = parse::parse_discussions(&body)?;
            let got = page_items.len();
            all.extend(page_items);
            if got < PER_PAGE {
                break;
            }
        }
        Ok(all)
    }

    async fn merge_merge_request(
        &self,
        project_path: &str,
        iid: u32,
        method: MergeMethod,
    ) -> Result<()> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/merge_requests/{}/merge",
            self.api_base(),
            encoded,
            iid
        );
        // GitLab MR merge accepts `squash` + `should_remove_source_branch`;
        // rebase is a separate endpoint (`/merge_requests/:id/rebase`). The TS
        // reference runs `glab mr merge --rebase` which rebases then merges in
        // one shot — we approximate by calling `/rebase` first when asked.
        if matches!(method, MergeMethod::Rebase) {
            let rebase_url = format!(
                "{}/projects/{}/merge_requests/{}/rebase",
                self.api_base(),
                encoded,
                iid
            );
            let resp = self
                .client
                .put(rebase_url)
                .headers(self.headers()?)
                .send()
                .await
                .map_err(|e| AoError::Scm(format!("gitlab rebase failed: {e}")))?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(AoError::Scm(format!(
                    "gitlab rebase failed: http {}: {}",
                    status.as_u16(),
                    body.trim()
                )));
            }
        }

        let squash = matches!(method, MergeMethod::Squash);
        let resp = self
            .client
            .put(url)
            .headers(self.headers()?)
            .json(&serde_json::json!({
                "squash": squash,
                "should_remove_source_branch": true
            }))
            .send()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab merge failed: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab merge read failed: {e}")))?;
        if !status.is_success() {
            return Err(AoError::Scm(format!(
                "gitlab merge failed: http {}: {}",
                status.as_u16(),
                body.trim()
            )));
        }
        Ok(())
    }

    async fn close_merge_request(&self, project_path: &str, iid: u32) -> Result<()> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/merge_requests/{}",
            self.api_base(),
            encoded,
            iid
        );
        let resp = self
            .client
            .put(url)
            .headers(self.headers()?)
            .json(&serde_json::json!({ "state_event": "close" }))
            .send()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab close failed: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab close read failed: {e}")))?;
        if !status.is_success() {
            return Err(AoError::Scm(format!(
                "gitlab close failed: http {}: {}",
                status.as_u16(),
                body.trim()
            )));
        }
        Ok(())
    }
}

impl GitLabScm {
    fn api_from_pr(&self, pr: &PullRequest) -> Result<GitLabApi<'_>> {
        let base_url = self
            .base_url_override
            .clone()
            .unwrap_or_else(|| base_url_from_http_url(&pr.url));
        let token = token_from_env(self.token_override.as_deref())?;
        Ok(GitLabApi::new(&self.client, &base_url, &token))
    }
}

// ---------------------------------------------------------------------------
// Git helpers (discover origin)
// ---------------------------------------------------------------------------

async fn discover_origin(workspace: &Path) -> Result<GitLabOrigin> {
    let url = git_in(workspace, &["remote", "get-url", "origin"]).await?;
    parse_gitlab_remote(url.trim())
        .ok_or_else(|| AoError::Scm(format!("origin is not a gitlab remote: {url:?}")))
}

async fn git_in(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", cwd.to_string_lossy().as_ref()])
        .args(args)
        .output()
        .await
        .map_err(|e| AoError::Scm(format!("git spawn failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AoError::Scm(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitLabOrigin {
    base_url: String,
    project_path: String,
}

pub(crate) fn parse_gitlab_remote(url: &str) -> Option<GitLabOrigin> {
    let trimmed = url.strip_suffix(".git").unwrap_or(url).trim();

    // https://gitlab.example.com/group/sub/repo
    if let Some(rest) = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
    {
        let mut parts = rest.splitn(2, '/');
        let host = parts.next()?.trim();
        let path = parts.next()?.trim().trim_matches('/');
        if host.is_empty() || path.is_empty() {
            return None;
        }
        if split_owner_repo(path).1.is_empty() {
            return None;
        }
        let scheme = if trimmed.starts_with("http://") {
            "http"
        } else {
            "https"
        };
        return Some(GitLabOrigin {
            base_url: format!("{scheme}://{host}"),
            project_path: path.to_string(),
        });
    }

    // git@gitlab.example.com:group/sub/repo
    if let Some(rest) = trimmed.strip_prefix("git@") {
        let mut parts = rest.splitn(2, ':');
        let host = parts.next()?.trim();
        let path = parts.next()?.trim().trim_matches('/');
        if host.is_empty() || path.is_empty() {
            return None;
        }
        if split_owner_repo(path).1.is_empty() {
            return None;
        }
        return Some(GitLabOrigin {
            base_url: format!("https://{host}"),
            project_path: path.to_string(),
        });
    }

    // ssh://git@gitlab.example.com/group/sub/repo
    if let Some(rest) = trimmed.strip_prefix("ssh://git@") {
        let mut parts = rest.splitn(2, '/');
        let host = parts.next()?.trim();
        let path = parts.next()?.trim().trim_matches('/');
        if host.is_empty() || path.is_empty() {
            return None;
        }
        if split_owner_repo(path).1.is_empty() {
            return None;
        }
        return Some(GitLabOrigin {
            base_url: format!("https://{host}"),
            project_path: path.to_string(),
        });
    }

    None
}

fn split_owner_repo(project_path: &str) -> (String, String) {
    let path = project_path.trim_matches('/');
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        return ("".into(), "".into());
    }
    let repo = parts.pop().unwrap_or_default().to_string();
    let owner = parts.join("/");
    (owner, repo)
}

fn project_path(pr: &PullRequest) -> String {
    format!("{}/{}", pr.owner, pr.repo)
}

fn base_url_from_http_url(url: &str) -> String {
    // Extract scheme://host from an https URL without extra deps.
    if let Some(idx) = url.find("://") {
        let after = &url[(idx + 3)..];
        if let Some(slash) = after.find('/') {
            return format!("{}://{}", &url[..idx], &after[..slash]);
        }
        return url.to_string();
    }
    // Fall back to gitlab.com; better than panicking.
    "https://gitlab.com".to_string()
}

fn token_from_env(override_token: Option<&str>) -> Result<String> {
    if let Some(t) = override_token {
        if !t.trim().is_empty() {
            return Ok(t.to_string());
        }
    }
    for k in ["GITLAB_TOKEN", "GITLAB_PRIVATE_TOKEN", "PRIVATE_TOKEN"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return Ok(v);
            }
        }
    }
    Err(AoError::Scm(
        "missing GitLab token (set GITLAB_TOKEN)".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Tests (fixtures + wiremock)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_pr() -> PullRequest {
        PullRequest {
            number: 42,
            url: "https://gitlab.example.com/acme/widgets/-/merge_requests/42".into(),
            title: "feat: add feature".into(),
            owner: "acme".into(),
            repo: "widgets".into(),
            branch: "feat/x".into(),
            base_branch: "main".into(),
            is_draft: false,
        }
    }

    #[test]
    fn parse_gitlab_remote_accepts_https_and_ssh_and_nested_groups() {
        let o = parse_gitlab_remote("https://gitlab.com/acme/sub/widgets.git").unwrap();
        assert_eq!(o.base_url, "https://gitlab.com");
        assert_eq!(o.project_path, "acme/sub/widgets");

        let o = parse_gitlab_remote("git@gitlab.example.com:acme/sub/widgets.git").unwrap();
        assert_eq!(o.base_url, "https://gitlab.example.com");
        assert_eq!(o.project_path, "acme/sub/widgets");

        let o = parse_gitlab_remote("ssh://git@gitlab.example.com/acme/widgets.git").unwrap();
        assert_eq!(o.base_url, "https://gitlab.example.com");
        assert_eq!(o.project_path, "acme/widgets");
    }

    #[tokio::test]
    async fn detect_pr_uses_list_endpoint_and_maps_to_pull_request() {
        let server = MockServer::start().await;

        let body = include_str!("../tests/fixtures/mr_list_open.json");

        Mock::given(method("GET"))
            .and(path("/api/v4/projects/acme%2Fwidgets/merge_requests"))
            .and(query_param("state", "opened"))
            .and(query_param("source_branch", "ao-abc"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        // We don't hit git in this unit test: call API parser directly.
        let mrs = parse::parse_mr_list(body).unwrap();
        let pr = mrs[0].clone().into_pull_request("acme", "widgets");
        assert_eq!(pr.number, 7);
        assert!(pr.is_draft);
        assert_eq!(pr.branch, "ao-abc");
        assert_eq!(pr.base_branch, "main");

        // Also smoke the API client path and encoding.
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let api = GitLabApi::new(&scm.client, &server.uri(), "t");
        let got = api
            .list_open_merge_requests_by_source_branch("acme/widgets", "ao-abc")
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].iid, Some(7));
    }

    #[tokio::test]
    async fn ci_checks_fans_out_pipelines_then_jobs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/pipelines",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"[{"id":100},{"id":99}]"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/pipelines/100/jobs",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[
                    {"name":"build","status":"success","web_url":"https://ci/1"},
                    {"name":"lint","status":"failed","web_url":""},
                    {"name":"deploy","status":"running"}
                ]"#,
            ))
            .mount(&server)
            .await;

        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let checks = scm.ci_checks(&make_pr()).await.unwrap();
        assert_eq!(checks.len(), 3);
        assert_eq!(checks[0].name, "build");
        assert_eq!(checks[0].url.as_deref(), Some("https://ci/1"));
        assert_eq!(checks[1].name, "lint");
        assert!(checks[1].url.is_none());
        assert_eq!(checks[2].name, "deploy");
    }

    #[tokio::test]
    async fn ci_checks_empty_when_no_pipelines() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/pipelines",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let checks = scm.ci_checks(&make_pr()).await.unwrap();
        assert!(checks.is_empty());
    }

    #[tokio::test]
    async fn ci_status_falls_back_to_none_when_merged_and_fetch_fails() {
        let server = MockServer::start().await;
        // Pipelines endpoint 500s → ci_checks errors.
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/pipelines",
            ))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        // MR view returns "merged" → fallback sets status to None.
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"state":"merged"}"#))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        assert_eq!(scm.ci_status(&make_pr()).await.unwrap(), CiStatus::None);
    }

    #[tokio::test]
    async fn ci_status_is_failing_when_both_fetches_fail() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/pipelines",
            ))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42",
            ))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        assert_eq!(scm.ci_status(&make_pr()).await.unwrap(), CiStatus::Failing);
    }

    #[tokio::test]
    async fn mergeability_returns_merged_clean() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"state":"merged"}"#))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let r = scm.mergeability(&make_pr()).await.unwrap();
        assert!(r.mergeable);
        assert!(r.blockers.is_empty());
    }

    #[tokio::test]
    async fn mergeability_closed_returns_blocker() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"state":"closed"}"#))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let r = scm.mergeability(&make_pr()).await.unwrap();
        assert!(!r.mergeable);
        assert!(r.blockers.iter().any(|b| b == "MR is closed"));
    }

    #[tokio::test]
    async fn mergeability_reports_draft_and_conflicts_and_unresolved_discussions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{
                    "state":"opened",
                    "draft":true,
                    "has_conflicts":true,
                    "merge_status":"cannot_be_merged",
                    "blocking_discussions_resolved":false
                }"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/approvals",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"approved":false,"approvals_required":1,"approvals_left":1}"#),
            )
            .mount(&server)
            .await;
        // ci_status → need pipelines + jobs; return empty pipelines so CI = None.
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/pipelines",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;

        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let r = scm.mergeability(&make_pr()).await.unwrap();
        assert!(!r.mergeable);
        // Conflicts + draft + unresolved discussions + approval required.
        assert!(r.blockers.iter().any(|b| b == "Merge conflicts"));
        assert!(r.blockers.iter().any(|b| b == "MR is still a draft"));
        assert!(r
            .blockers
            .iter()
            .any(|b| b == "Unresolved discussions blocking merge"));
        assert!(r.blockers.iter().any(|b| b == "Approval required"));
    }

    #[tokio::test]
    async fn close_pr_sends_state_event_close() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        scm.close_pr(&make_pr()).await.unwrap();
    }

    #[tokio::test]
    async fn merge_honours_squash_method() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/merge",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        scm.merge(&make_pr(), Some(MergeMethod::Squash)).await.unwrap();
    }

    #[tokio::test]
    async fn merge_rebase_hits_rebase_then_merge_endpoints() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/rebase",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/merge",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        scm.merge(&make_pr(), Some(MergeMethod::Rebase)).await.unwrap();
    }

    #[tokio::test]
    async fn pr_summary_returns_state_title_zero_diff() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"state":"opened","title":"feat: add feature"}"#),
            )
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let summary = scm.pr_summary(&make_pr()).await.unwrap();
        assert_eq!(summary.state, PrState::Open);
        assert_eq!(summary.title, "feat: add feature");
        assert_eq!(summary.additions, 0);
        assert_eq!(summary.deletions, 0);
    }

    #[tokio::test]
    async fn reviews_merges_approvals_and_unresolved_discussions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/approvals",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"approved":false,"approvals_required":1,"approvals_left":1,"approved_by":[{"user":{"username":"alice"}}]}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/discussions",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[
                    {"notes":[{"id":1,"author":{"username":"bob"},"body":"needs work","resolvable":true,"resolved":false}]},
                    {"notes":[{"id":2,"author":{"username":"alice"},"body":"drift","resolvable":true,"resolved":false}]}
                ]"#,
            ))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let reviews = scm.reviews(&make_pr()).await.unwrap();
        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].author, "alice");
        assert_eq!(reviews[0].state, ao_core::ReviewState::Approved);
        assert_eq!(reviews[1].author, "bob");
        assert_eq!(reviews[1].state, ao_core::ReviewState::ChangesRequested);
    }

    #[tokio::test]
    async fn automated_comments_returns_bot_notes_only() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/acme%2Fwidgets/merge_requests/42/discussions",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[
                    {"notes":[{"id":1,"author":{"username":"gitlab-bot"},"body":"found a critical error","resolvable":true,"resolved":false}]},
                    {"notes":[{"id":2,"author":{"username":"alice"},"body":"lgtm","resolvable":true,"resolved":false}]}
                ]"#,
            ))
            .mount(&server)
            .await;
        let scm = GitLabScm::with_base_url_and_token(server.uri(), "t");
        let comments = scm.automated_comments(&make_pr()).await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].bot_name, "gitlab-bot");
        assert_eq!(
            comments[0].severity,
            ao_core::AutomatedCommentSeverity::Error
        );
    }

    #[test]
    fn compose_merge_readiness_blocks_on_ci_and_conflicts_and_review_required() {
        let mr = parse::MergeRequest {
            iid: Some(1),
            web_url: Some("u".into()),
            title: Some("t".into()),
            source_branch: Some("b".into()),
            target_branch: Some("main".into()),
            state: Some("opened".into()),
            draft: Some(false),
            work_in_progress: Some(false),
            merge_status: Some("cannot_be_merged".into()),
            has_conflicts: Some(true),
            blocking_discussions_resolved: Some(true),
        };
        let approvals = parse::Approvals {
            approved: Some(false),
            approvals_left: Some(1),
            approved_by: vec![],
        };
        let r = compose_merge_readiness(&mr, &approvals, CiStatus::Failing);
        assert!(!r.is_ready());
        assert!(r.blockers.iter().any(|b| b.contains("CI is failing")));
        assert!(r.blockers.iter().any(|b| b.contains("Approval required")));
        assert!(r.blockers.iter().any(|b| b.contains("Merge conflicts")));
    }

    #[test]
    fn compose_merge_readiness_clean_when_gates_pass() {
        let mr = parse::MergeRequest {
            iid: Some(1),
            web_url: None,
            title: Some("feat".into()),
            source_branch: Some("feat/x".into()),
            target_branch: Some("main".into()),
            state: Some("opened".into()),
            draft: Some(false),
            work_in_progress: Some(false),
            merge_status: Some("can_be_merged".into()),
            has_conflicts: Some(false),
            blocking_discussions_resolved: Some(true),
        };
        let approvals = parse::Approvals {
            approved: Some(true),
            approvals_left: Some(0),
            approved_by: vec![],
        };
        let r = compose_merge_readiness(&mr, &approvals, CiStatus::Passing);
        assert!(r.is_ready());
    }

    #[test]
    fn compose_merge_readiness_checking_status_is_blocker() {
        let mr = parse::MergeRequest {
            iid: Some(1),
            web_url: None,
            title: Some("feat".into()),
            source_branch: Some("feat/x".into()),
            target_branch: Some("main".into()),
            state: Some("opened".into()),
            draft: Some(false),
            work_in_progress: Some(false),
            merge_status: Some("checking".into()),
            has_conflicts: Some(false),
            blocking_discussions_resolved: Some(true),
        };
        let approvals = parse::Approvals {
            approved: Some(true),
            approvals_left: Some(0),
            approved_by: vec![],
        };
        let r = compose_merge_readiness(&mr, &approvals, CiStatus::Passing);
        assert!(r
            .blockers
            .iter()
            .any(|b| b.contains("GitLab is computing")));
    }
}
