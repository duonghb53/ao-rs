//! GitLab SCM plugin — GitLab Merge Requests via the GitLab REST API.
//!
//! Unlike `ao-plugin-scm-github` (which shells out to `gh`), this plugin uses
//! HTTPS so we can test it with recorded fixtures (no real network).

use ao_core::{
    AoError, CheckRun, CiStatus, MergeMethod, MergeReadiness, PrState, PullRequest, Result, Review,
    ReviewComment, ReviewDecision, Scm, Session,
};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

pub(crate) mod parse;

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

    async fn ci_checks(&self, pr: &PullRequest) -> Result<Vec<CheckRun>> {
        let api = self.api_from_pr(pr)?;
        let mr = api.get_merge_request(&project_path(pr), pr.number).await?;
        Ok(parse::extract_ci_checks(&mr))
    }

    async fn ci_status(&self, pr: &PullRequest) -> Result<CiStatus> {
        let checks = self.ci_checks(pr).await?;
        Ok(parse::summarize_ci(&checks))
    }

    async fn reviews(&self, pr: &PullRequest) -> Result<Vec<Review>> {
        let api = self.api_from_pr(pr)?;
        let approvals = api.get_approvals(&project_path(pr), pr.number).await?;
        Ok(parse::approvals_into_reviews(approvals))
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

    async fn mergeability(&self, pr: &PullRequest) -> Result<MergeReadiness> {
        let api = self.api_from_pr(pr)?;

        if matches!(self.pr_state(pr).await?, PrState::Merged) {
            return Ok(MergeReadiness {
                mergeable: true,
                ci_passing: true,
                approved: true,
                no_conflicts: true,
                blockers: Vec::new(),
            });
        }

        let mr = api.get_merge_request(&project_path(pr), pr.number).await?;
        let approvals = api.get_approvals(&project_path(pr), pr.number).await?;
        let ci_status = self.ci_status(pr).await?;
        Ok(compose_merge_readiness(&mr, &approvals, ci_status))
    }

    async fn merge(&self, pr: &PullRequest, method: Option<MergeMethod>) -> Result<()> {
        let api = self.api_from_pr(pr)?;
        let squash = matches!(method.unwrap_or_default(), MergeMethod::Squash);
        api.merge_merge_request(&project_path(pr), pr.number, squash)
            .await?;
        Ok(())
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

    // Reviews / approvals
    let decision = parse::review_decision_from_approvals(approvals);
    let approved = matches!(decision, ReviewDecision::Approved | ReviewDecision::None);
    match decision {
        ReviewDecision::ChangesRequested => blockers.push("Changes requested in review".into()),
        ReviewDecision::Pending => blockers.push("Review required".into()),
        _ => {}
    }

    // Draft
    if mr.is_draft() {
        blockers.push("MR is still a draft".into());
    }

    // Conflicts / merge status
    let no_conflicts = !mr.has_conflicts.unwrap_or(false);
    if mr.has_conflicts.unwrap_or(false) {
        blockers.push("Merge conflicts".into());
    }
    match mr
        .merge_status
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "cannot_be_merged" => blockers.push("Merge is blocked".into()),
        "checking" | "" => blockers.push("Merge status unknown (GitLab is computing)".into()),
        _ => {}
    }

    // Mergeable is the conjunction of our blockers.
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
        let resp = self
            .client
            .get(url)
            .headers(self.headers()?)
            .send()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab mr list failed: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab mr list read failed: {e}")))?;
        if !status.is_success() {
            return Err(AoError::Scm(format!(
                "gitlab mr list failed: http {}: {}",
                status.as_u16(),
                body.trim()
            )));
        }
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
        let resp = self
            .client
            .get(url)
            .headers(self.headers()?)
            .send()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab mr view failed: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab mr view read failed: {e}")))?;
        if !status.is_success() {
            return Err(AoError::Scm(format!(
                "gitlab mr view failed: http {}: {}",
                status.as_u16(),
                body.trim()
            )));
        }
        parse::parse_mr_view(&body)
    }

    async fn get_approvals(&self, project_path: &str, iid: u32) -> Result<parse::Approvals> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/merge_requests/{}/approvals",
            self.api_base(),
            encoded,
            iid
        );
        let resp = self
            .client
            .get(url)
            .headers(self.headers()?)
            .send()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab approvals failed: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AoError::Scm(format!("gitlab approvals read failed: {e}")))?;
        if !status.is_success() {
            return Err(AoError::Scm(format!(
                "gitlab approvals failed: http {}: {}",
                status.as_u16(),
                body.trim()
            )));
        }
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
            let resp = self
                .client
                .get(url)
                .headers(self.headers()?)
                .send()
                .await
                .map_err(|e| AoError::Scm(format!("gitlab discussions failed: {e}")))?;
            let status = resp.status();
            let body = resp
                .text()
                .await
                .map_err(|e| AoError::Scm(format!("gitlab discussions read failed: {e}")))?;
            if !status.is_success() {
                return Err(AoError::Scm(format!(
                    "gitlab discussions failed: http {}: {}",
                    status.as_u16(),
                    body.trim()
                )));
            }
            let page_items = parse::parse_discussions(&body)?;
            let got = page_items.len();
            all.extend(page_items);
            if got < PER_PAGE {
                break;
            }
        }
        Ok(all)
    }

    async fn merge_merge_request(&self, project_path: &str, iid: u32, squash: bool) -> Result<()> {
        let encoded = urlencoding::encode(project_path);
        let url = format!(
            "{}/projects/{}/merge_requests/{}/merge",
            self.api_base(),
            encoded,
            iid
        );
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
            head_pipeline: None,
        };
        let approvals = parse::Approvals {
            approvals_required: Some(1),
            approvals_left: Some(1),
            approved_by: vec![],
        };
        let r = compose_merge_readiness(&mr, &approvals, CiStatus::Failing);
        assert!(!r.is_ready());
        assert!(r.blockers.iter().any(|b| b.contains("CI is failing")));
        assert!(r.blockers.iter().any(|b| b.contains("Review required")));
        assert!(r.blockers.iter().any(|b| b.contains("Merge conflicts")));
    }
}
