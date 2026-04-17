//! GitLab webhook verification + parsing.
//!
//! Ports TS `verifyWebhook` / `parseWebhook` from
//! `packages/plugins/scm-gitlab/src/index.ts`. GitLab's webhook auth is a
//! plaintext token in `x-gitlab-token` rather than a MAC over the body — we
//! follow the TS reference and compare SHA-256 digests of secret and provided
//! token in constant time to avoid byte-wise timing leaks.

use ao_core::{
    config::{ProjectConfig, ScmWebhookConfig},
    AoError, Result, ScmWebhookEvent, ScmWebhookEventKind, ScmWebhookRepository, ScmWebhookRequest,
    ScmWebhookVerificationResult,
};
use sha2::{Digest, Sha256};

/// Effective webhook config with GitLab-flavoured defaults applied. Mirrors
/// the TS `getGitLabWebhookConfig` helper.
pub(crate) struct EffectiveWebhookConfig {
    pub enabled: bool,
    pub secret_env_var: Option<String>,
    pub signature_header: String,
    pub event_header: String,
    pub delivery_header: String,
    pub max_body_bytes: Option<u64>,
}

pub(crate) fn effective_config(project: &ProjectConfig) -> EffectiveWebhookConfig {
    let cfg: ScmWebhookConfig = project
        .scm
        .as_ref()
        .and_then(|s| s.webhook.as_ref())
        .cloned()
        .unwrap_or_default();
    EffectiveWebhookConfig {
        enabled: cfg.enabled,
        secret_env_var: cfg.secret_env_var,
        signature_header: cfg
            .signature_header
            .unwrap_or_else(|| "x-gitlab-token".into()),
        event_header: cfg.event_header.unwrap_or_else(|| "x-gitlab-event".into()),
        delivery_header: cfg
            .delivery_header
            .unwrap_or_else(|| "x-gitlab-event-uuid".into()),
        max_body_bytes: cfg.max_body_bytes,
    }
}

/// Case-insensitive header lookup. Returns the first value when the header
/// appears more than once (matches TS behaviour and RFC 7230).
pub(crate) fn get_header<'a>(req: &'a ScmWebhookRequest, name: &str) -> Option<&'a str> {
    let target = name.to_ascii_lowercase();
    for (key, values) in &req.headers {
        if key.to_ascii_lowercase() == target {
            return values.first().map(|s| s.as_str());
        }
    }
    None
}

/// Constant-time comparison of SHA-256(secret) and SHA-256(provided).
///
/// Uses digests of equal length so the compare stays constant-time regardless
/// of the token's size. Mirrors TS `verifyGitLabToken`.
pub(crate) fn verify_token(secret: &str, provided: &str) -> bool {
    let a = Sha256::digest(secret.as_bytes());
    let b = Sha256::digest(provided.as_bytes());
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(crate) async fn verify(
    request: &ScmWebhookRequest,
    project: &ProjectConfig,
) -> Result<ScmWebhookVerificationResult> {
    let cfg = effective_config(project);
    if !cfg.enabled {
        return Ok(rejected("Webhook is disabled for this project"));
    }
    if !request.method.eq_ignore_ascii_case("POST") {
        return Ok(rejected("Webhook requests must use POST"));
    }
    if let Some(max) = cfg.max_body_bytes {
        let len = request
            .raw_body
            .as_ref()
            .map(|b| b.len() as u64)
            .unwrap_or_else(|| request.body.len() as u64);
        if len > max {
            return Ok(rejected("Webhook payload exceeds configured maxBodyBytes"));
        }
    }

    let event_type = match get_header(request, &cfg.event_header) {
        Some(e) => e.to_string(),
        None => return Ok(rejected(&format!("Missing {} header", cfg.event_header))),
    };
    let delivery_id = get_header(request, &cfg.delivery_header).map(str::to_string);

    let Some(secret_env) = cfg.secret_env_var.as_deref() else {
        // No secret configured → skip the token check (local/dev path).
        return Ok(ScmWebhookVerificationResult {
            ok: true,
            reason: None,
            delivery_id,
            event_type: Some(event_type),
        });
    };

    let Ok(secret) = std::env::var(secret_env) else {
        return Ok(rejected(&format!(
            "Webhook secret env var {secret_env} is not configured"
        )));
    };

    let Some(provided) = get_header(request, &cfg.signature_header) else {
        return Ok(rejected(&format!(
            "Missing {} header",
            cfg.signature_header
        )));
    };

    if !verify_token(&secret, provided) {
        return Ok(ScmWebhookVerificationResult {
            ok: false,
            reason: Some("Webhook token verification failed".into()),
            delivery_id,
            event_type: Some(event_type),
        });
    }

    Ok(ScmWebhookVerificationResult {
        ok: true,
        reason: None,
        delivery_id,
        event_type: Some(event_type),
    })
}

fn rejected(reason: &str) -> ScmWebhookVerificationResult {
    ScmWebhookVerificationResult {
        ok: false,
        reason: Some(reason.into()),
        delivery_id: None,
        event_type: None,
    }
}

pub(crate) fn parse(
    request: &ScmWebhookRequest,
    project: &ProjectConfig,
) -> Result<Option<ScmWebhookEvent>> {
    let cfg = effective_config(project);
    let Some(raw_event_type) = get_header(request, &cfg.event_header) else {
        return Ok(None);
    };
    let raw_event_type = raw_event_type.to_string();
    let normalized = raw_event_type.to_ascii_lowercase();
    let delivery_id = get_header(request, &cfg.delivery_header).map(str::to_string);

    let payload: serde_json::Value = serde_json::from_str(&request.body)
        .map_err(|e| AoError::Scm(format!("webhook payload is not valid JSON: {e}")))?;
    if !payload.is_object() {
        return Err(AoError::Scm("webhook payload must be a JSON object".into()));
    }

    let object_kind = payload
        .get("object_kind")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let object_attributes = payload.get("object_attributes").and_then(|v| v.as_object());
    let repository = parse_repository(&payload);

    let action = object_attributes
        .and_then(|o| o.get("action"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("action")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| raw_event_type.clone());

    // Merge request
    if normalized == "merge request hook" || object_kind.as_deref() == Some("merge_request") {
        let Some(mr) = payload.get("object_attributes").and_then(|v| v.as_object()) else {
            return Ok(None);
        };
        let pr_number = mr
            .get("iid")
            .and_then(|v| v.as_u64())
            .or_else(|| mr.get("id").and_then(|v| v.as_u64()))
            .map(|n| n as u32);
        let branch = mr
            .get("source_branch")
            .and_then(|v| v.as_str())
            .and_then(parse_branch_ref)
            .map(str::to_string);
        let sha = mr
            .get("last_commit")
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        return Ok(Some(ScmWebhookEvent {
            provider: "gitlab".into(),
            kind: ScmWebhookEventKind::PullRequest,
            action,
            raw_event_type,
            delivery_id,
            repository,
            pr_number,
            branch,
            sha,
            data: payload,
        }));
    }

    // Note (comment) — only MR-bound notes
    if normalized == "note hook" || object_kind.as_deref() == Some("note") {
        let Some(mr) = payload.get("merge_request").and_then(|v| v.as_object()) else {
            return Ok(None);
        };
        let noteable = object_attributes
            .and_then(|o| o.get("noteable_type"))
            .and_then(|v| v.as_str());
        if noteable != Some("MergeRequest") {
            return Ok(None);
        }
        let pr_number = mr
            .get("iid")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let branch = mr
            .get("source_branch")
            .and_then(|v| v.as_str())
            .and_then(parse_branch_ref)
            .map(str::to_string);
        let sha = mr
            .get("last_commit")
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        return Ok(Some(ScmWebhookEvent {
            provider: "gitlab".into(),
            kind: ScmWebhookEventKind::Comment,
            action,
            raw_event_type,
            delivery_id,
            repository,
            pr_number,
            branch,
            sha,
            data: payload,
        }));
    }

    // Pipeline / Job
    if normalized == "pipeline hook"
        || normalized == "job hook"
        || object_kind.as_deref() == Some("pipeline")
        || object_kind.as_deref() == Some("build")
    {
        let pr_number = payload
            .get("merge_request")
            .and_then(|v| v.as_object())
            .and_then(|mr| {
                mr.get("iid")
                    .and_then(|v| v.as_u64())
                    .or_else(|| mr.get("id").and_then(|v| v.as_u64()))
            })
            .map(|n| n as u32);
        let branch = parse_ci_branch(&payload);
        let sha = payload
            .get("checkout_sha")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("sha").and_then(|v| v.as_str()))
            .or_else(|| {
                object_attributes
                    .and_then(|o| o.get("sha"))
                    .and_then(|v| v.as_str())
            })
            .map(str::to_string);
        return Ok(Some(ScmWebhookEvent {
            provider: "gitlab".into(),
            kind: ScmWebhookEventKind::Ci,
            action,
            raw_event_type,
            delivery_id,
            repository,
            pr_number,
            branch,
            sha,
            data: payload,
        }));
    }

    // Push / tag push
    if normalized == "push hook"
        || normalized == "tag push hook"
        || object_kind.as_deref() == Some("push")
        || object_kind.as_deref() == Some("tag_push")
    {
        let branch = if is_tag_ref(&payload) {
            None
        } else {
            payload
                .get("ref")
                .and_then(|v| v.as_str())
                .and_then(parse_branch_ref)
                .map(str::to_string)
        };
        let sha = payload
            .get("after")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("checkout_sha").and_then(|v| v.as_str()))
            .map(str::to_string);
        return Ok(Some(ScmWebhookEvent {
            provider: "gitlab".into(),
            kind: ScmWebhookEventKind::Push,
            action,
            raw_event_type,
            delivery_id,
            repository,
            pr_number: None,
            branch,
            sha,
            data: payload,
        }));
    }

    Ok(Some(ScmWebhookEvent {
        provider: "gitlab".into(),
        kind: ScmWebhookEventKind::Unknown,
        action,
        raw_event_type,
        delivery_id,
        repository,
        pr_number: None,
        branch: None,
        sha: None,
        data: payload,
    }))
}

fn parse_repository(payload: &serde_json::Value) -> Option<ScmWebhookRepository> {
    let project = payload.get("project")?.as_object()?;
    if let Some(p) = project
        .get("path_with_namespace")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let parts: Vec<&str> = p.split('/').collect();
        if parts.len() >= 2 {
            let name = parts[parts.len() - 1].to_string();
            let owner = parts[..parts.len() - 1].join("/");
            if !owner.is_empty() && !name.is_empty() {
                return Some(ScmWebhookRepository { owner, name });
            }
        }
    }
    let namespace = project.get("namespace").and_then(|v| v.as_str())?;
    let name = project.get("path").and_then(|v| v.as_str())?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(ScmWebhookRepository {
        owner: namespace.to_string(),
        name: name.to_string(),
    })
}

fn is_tag_ref(payload: &serde_json::Value) -> bool {
    let oa = payload.get("object_attributes").and_then(|v| v.as_object());
    if oa
        .and_then(|o| o.get("tag"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if payload.get("ref_type").and_then(|v| v.as_str()) == Some("tag") {
        return true;
    }
    payload.get("object_kind").and_then(|v| v.as_str()) == Some("tag_push")
}

fn parse_ci_branch(payload: &serde_json::Value) -> Option<String> {
    if is_tag_ref(payload) {
        return None;
    }
    let oa = payload.get("object_attributes").and_then(|v| v.as_object());
    let r = payload
        .get("ref")
        .and_then(|v| v.as_str())
        .or_else(|| oa.and_then(|o| o.get("ref")).and_then(|v| v.as_str()))?;
    parse_branch_ref(r).map(str::to_string)
}

/// Extract a branch name from a `refs/heads/<branch>` ref. Returns `None`
/// for `refs/tags/...`; passes plain branch names through.
pub(crate) fn parse_branch_ref(ref_str: &str) -> Option<&str> {
    if let Some(rest) = ref_str.strip_prefix("refs/heads/") {
        if rest.is_empty() {
            return None;
        }
        return Some(rest);
    }
    if ref_str.starts_with("refs/") {
        return None;
    }
    if ref_str.is_empty() {
        None
    } else {
        Some(ref_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::config::PluginConfig;
    use std::collections::HashMap;

    fn make_project(webhook: Option<ScmWebhookConfig>) -> ProjectConfig {
        let yaml = r#"
repo: acme/repo
path: /tmp
default_branch: main
"#;
        let mut project: ProjectConfig = serde_yaml::from_str(yaml).unwrap();
        if let Some(w) = webhook {
            project.scm = Some(PluginConfig {
                plugin: None,
                package: None,
                path: None,
                webhook: Some(w),
                extra: HashMap::new(),
            });
        }
        project
    }

    fn make_request(method: &str, body: &str, headers: &[(&str, &str)]) -> ScmWebhookRequest {
        let mut h: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in headers {
            h.entry(k.to_string()).or_default().push(v.to_string());
        }
        ScmWebhookRequest {
            method: method.into(),
            headers: h,
            body: body.into(),
            raw_body: Some(body.as_bytes().to_vec()),
            path: None,
        }
    }

    const SECRET: &str = "topsecret";

    #[test]
    fn get_header_case_insensitive() {
        let req = make_request("POST", "{}", &[("X-Gitlab-Event", "Merge Request Hook")]);
        assert_eq!(
            get_header(&req, "x-gitlab-event"),
            Some("Merge Request Hook")
        );
        assert_eq!(
            get_header(&req, "X-GITLAB-EVENT"),
            Some("Merge Request Hook")
        );
    }

    #[test]
    fn verify_token_accepts_matching_tokens() {
        assert!(verify_token(SECRET, SECRET));
        assert!(!verify_token(SECRET, "wrong"));
        assert!(!verify_token(SECRET, ""));
    }

    #[tokio::test]
    async fn verify_rejects_non_post() {
        let project = make_project(Some(ScmWebhookConfig::default()));
        let req = make_request("GET", "{}", &[("X-Gitlab-Event", "Push Hook")]);
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
        assert!(res.reason.unwrap().contains("POST"));
    }

    #[tokio::test]
    async fn verify_rejects_disabled_webhook() {
        let project = make_project(Some(ScmWebhookConfig {
            enabled: false,
            ..Default::default()
        }));
        let req = make_request("POST", "{}", &[("X-Gitlab-Event", "Push Hook")]);
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
        assert!(res.reason.unwrap().contains("disabled"));
    }

    #[tokio::test]
    async fn verify_rejects_body_exceeding_max() {
        let project = make_project(Some(ScmWebhookConfig {
            max_body_bytes: Some(2),
            ..Default::default()
        }));
        let req = make_request("POST", "abcd", &[("X-Gitlab-Event", "Push Hook")]);
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
        assert!(res.reason.unwrap().contains("maxBodyBytes"));
    }

    #[tokio::test]
    async fn verify_requires_event_header() {
        let project = make_project(Some(ScmWebhookConfig::default()));
        let req = make_request("POST", "{}", &[]);
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
        assert!(res.reason.unwrap().to_lowercase().contains("missing"));
    }

    #[tokio::test]
    async fn verify_passes_without_configured_secret() {
        let project = make_project(Some(ScmWebhookConfig::default()));
        let req = make_request(
            "POST",
            "{}",
            &[
                ("X-Gitlab-Event", "Push Hook"),
                ("X-Gitlab-Event-UUID", "delivery-1"),
            ],
        );
        let res = verify(&req, &project).await.unwrap();
        assert!(res.ok);
        assert_eq!(res.event_type.as_deref(), Some("Push Hook"));
        assert_eq!(res.delivery_id.as_deref(), Some("delivery-1"));
    }

    #[tokio::test]
    async fn verify_with_secret_accepts_valid_token() {
        let env_var = "AO_TEST_GITLAB_WEBHOOK_SECRET_VALID";
        // SAFETY: test-only env mutation; no concurrent writers to this name.
        unsafe {
            std::env::set_var(env_var, SECRET);
        }
        let project = make_project(Some(ScmWebhookConfig {
            secret_env_var: Some(env_var.into()),
            ..Default::default()
        }));
        let req = make_request(
            "POST",
            "{}",
            &[
                ("X-Gitlab-Event", "Merge Request Hook"),
                ("X-Gitlab-Token", SECRET),
            ],
        );
        let res = verify(&req, &project).await.unwrap();
        assert!(res.ok, "reason: {:?}", res.reason);
        unsafe {
            std::env::remove_var(env_var);
        }
    }

    #[tokio::test]
    async fn verify_with_secret_rejects_bad_token() {
        let env_var = "AO_TEST_GITLAB_WEBHOOK_SECRET_BAD";
        unsafe {
            std::env::set_var(env_var, SECRET);
        }
        let project = make_project(Some(ScmWebhookConfig {
            secret_env_var: Some(env_var.into()),
            ..Default::default()
        }));
        let req = make_request(
            "POST",
            "{}",
            &[
                ("X-Gitlab-Event", "Merge Request Hook"),
                ("X-Gitlab-Token", "wrong"),
            ],
        );
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
        assert!(res.reason.unwrap().contains("verification failed"));
        unsafe {
            std::env::remove_var(env_var);
        }
    }

    #[test]
    fn parse_branch_ref_handles_refs_and_plain() {
        assert_eq!(parse_branch_ref("refs/heads/main"), Some("main"));
        assert_eq!(parse_branch_ref("refs/heads/feat/x"), Some("feat/x"));
        assert_eq!(parse_branch_ref("refs/tags/v1"), None);
        assert_eq!(parse_branch_ref("refs/heads/"), None);
        assert_eq!(parse_branch_ref(""), None);
        assert_eq!(parse_branch_ref("main"), Some("main"));
    }

    #[test]
    fn parse_merge_request_event_extracts_fields() {
        let project = make_project(None);
        let body = r#"{
            "object_kind":"merge_request",
            "object_attributes":{
                "action":"open",
                "iid":42,
                "source_branch":"feat/x",
                "updated_at":"2026-03-11T00:00:00Z",
                "last_commit":{"id":"abc123"}
            },
            "project":{"path_with_namespace":"acme/repo"}
        }"#;
        let req = make_request(
            "POST",
            body,
            &[
                ("X-Gitlab-Event", "Merge Request Hook"),
                ("X-Gitlab-Event-UUID", "delivery-1"),
            ],
        );
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.provider, "gitlab");
        assert_eq!(event.kind, ScmWebhookEventKind::PullRequest);
        assert_eq!(event.action, "open");
        assert_eq!(event.pr_number, Some(42));
        assert_eq!(event.branch.as_deref(), Some("feat/x"));
        assert_eq!(event.sha.as_deref(), Some("abc123"));
        assert_eq!(event.delivery_id.as_deref(), Some("delivery-1"));
        let repo = event.repository.as_ref().unwrap();
        assert_eq!(repo.owner, "acme");
        assert_eq!(repo.name, "repo");
    }

    #[test]
    fn parse_push_event_strips_refs_heads() {
        let project = make_project(None);
        let body = r#"{
            "object_kind":"push",
            "ref":"refs/heads/feat/x",
            "after":"def456",
            "project":{"path_with_namespace":"acme/repo"}
        }"#;
        let req = make_request("POST", body, &[("X-Gitlab-Event", "Push Hook")]);
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.kind, ScmWebhookEventKind::Push);
        assert_eq!(event.branch.as_deref(), Some("feat/x"));
        assert_eq!(event.sha.as_deref(), Some("def456"));
    }

    #[test]
    fn parse_tag_push_has_no_branch() {
        let project = make_project(None);
        let body = r#"{
            "object_kind":"tag_push",
            "ref":"refs/tags/v1.0.0",
            "after":"def456",
            "project":{"path_with_namespace":"acme/repo"}
        }"#;
        let req = make_request("POST", body, &[("X-Gitlab-Event", "Tag Push Hook")]);
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.kind, ScmWebhookEventKind::Push);
        assert!(event.branch.is_none());
        assert_eq!(event.sha.as_deref(), Some("def456"));
    }

    #[test]
    fn parse_plain_tag_ref_has_no_branch() {
        let project = make_project(None);
        let body = r#"{
            "object_kind":"tag_push",
            "ref":"v1.0.0",
            "ref_type":"tag",
            "after":"def456",
            "project":{"path_with_namespace":"acme/repo"}
        }"#;
        let req = make_request("POST", body, &[("X-Gitlab-Event", "Tag Push Hook")]);
        let event = parse(&req, &project).unwrap().unwrap();
        assert!(event.branch.is_none());
    }

    #[test]
    fn parse_pipeline_tag_has_no_branch() {
        let project = make_project(None);
        let body = r#"{
            "object_kind":"pipeline",
            "ref":"v1.0.0",
            "ref_type":"tag",
            "checkout_sha":"def456",
            "project":{"path_with_namespace":"acme/repo"},
            "object_attributes":{"ref":"v1.0.0","tag":true}
        }"#;
        let req = make_request("POST", body, &[("X-Gitlab-Event", "Pipeline Hook")]);
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.kind, ScmWebhookEventKind::Ci);
        assert!(event.branch.is_none());
        assert_eq!(event.sha.as_deref(), Some("def456"));
    }

    #[test]
    fn parse_note_on_plain_issue_is_dropped() {
        let project = make_project(None);
        let body = r#"{
            "object_kind":"note",
            "object_attributes":{"noteable_type":"Issue"},
            "project":{"path_with_namespace":"acme/repo"}
        }"#;
        let req = make_request("POST", body, &[("X-Gitlab-Event", "Note Hook")]);
        let event = parse(&req, &project).unwrap();
        assert!(event.is_none());
    }

    #[test]
    fn parse_unknown_event_keeps_raw_type() {
        let project = make_project(None);
        let body = r#"{"project":{"path_with_namespace":"acme/repo"}}"#;
        let req = make_request("POST", body, &[("X-Gitlab-Event", "Feature Flag Hook")]);
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.kind, ScmWebhookEventKind::Unknown);
        assert_eq!(event.raw_event_type, "Feature Flag Hook");
    }

    #[test]
    fn parse_rejects_non_object_payload() {
        let project = make_project(None);
        let req = make_request("POST", "42", &[("X-Gitlab-Event", "Push Hook")]);
        let err = parse(&req, &project).unwrap_err().to_string();
        assert!(err.to_lowercase().contains("json object"));
    }
}
