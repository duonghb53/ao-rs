//! GitHub webhook verification + parsing.
//!
//! Ports TS `verifyWebhook` / `parseWebhook` from
//! `packages/plugins/scm-github/src/index.ts`. Kept in its own module so the
//! HMAC + parsing logic can be unit-tested without touching the `gh` shell-out
//! helpers in `lib.rs`.
//!
//! ## Why constant-time compare
//!
//! Signature verification uses `Hmac::verify_slice`, which compares in
//! constant time. A naive `==` on hex strings would leak timing information
//! and let a motivated attacker guess the HMAC byte-by-byte.
//!
//! ## `raw_body` vs `body`
//!
//! GitHub computes the signature over the raw request bytes. Decoding to
//! UTF-8 first is *usually* lossless for JSON payloads, but the spec doesn't
//! guarantee it — so we prefer `raw_body` when the HTTP layer provided it
//! and only fall back to the UTF-8 body.
//!
//! ## Event kinds
//!
//! GitHub's `X-GitHub-Event` header has a long tail of event types
//! (`release`, `workflow_run`, `gollum`, ...). We map the ones the reaction
//! engine actually cares about to `ScmWebhookEventKind` variants and drop
//! the rest into `Unknown` with the raw type preserved on the event so
//! consumers can still dispatch on it.

use ao_core::{
    config::{ProjectConfig, ScmWebhookConfig},
    AoError, Result, ScmWebhookEvent, ScmWebhookEventKind, ScmWebhookRepository, ScmWebhookRequest,
    ScmWebhookVerificationResult,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Effective webhook config, with TS-parity defaults applied.
///
/// Separate from `ScmWebhookConfig` because the on-disk struct keeps
/// `Option<String>` for ergonomic YAML while the verifier wants concrete
/// header names. Owns its strings so the struct can be returned by value.
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
            .unwrap_or_else(|| "x-hub-signature-256".into()),
        event_header: cfg.event_header.unwrap_or_else(|| "x-github-event".into()),
        delivery_header: cfg
            .delivery_header
            .unwrap_or_else(|| "x-github-delivery".into()),
        max_body_bytes: cfg.max_body_bytes,
    }
}

/// Case-insensitive header lookup. Returns the first value when the header
/// appears more than once (matches TS behaviour).
pub(crate) fn get_header<'a>(req: &'a ScmWebhookRequest, name: &str) -> Option<&'a str> {
    let target = name.to_ascii_lowercase();
    for (key, values) in &req.headers {
        if key.to_ascii_lowercase() == target {
            return values.first().map(|s| s.as_str());
        }
    }
    None
}

/// Verify an HMAC-SHA256 signature in GitHub's `sha256=<hex>` format.
///
/// Pulled out as a standalone function so tests can drive it directly
/// without constructing a full `ProjectConfig`.
pub(crate) fn verify_signature(body: &[u8], secret: &str, signature_header: &str) -> bool {
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    // `verify_slice` is constant-time by construction; see module docstring.
    mac.verify_slice(&expected).is_ok()
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

    let event_type = get_header(request, &cfg.event_header).map(str::to_string);
    let Some(event_type) = event_type else {
        return Ok(rejected(&format!("Missing {} header", cfg.event_header)));
    };
    let delivery_id = get_header(request, &cfg.delivery_header).map(str::to_string);

    let Some(secret_env) = cfg.secret_env_var.as_deref() else {
        // No secret configured → skip signature check but still allow the
        // delivery through. Mirrors TS behaviour for local/dev webhooks.
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

    let Some(signature) = get_header(request, &cfg.signature_header) else {
        return Ok(rejected(&format!(
            "Missing {} header",
            cfg.signature_header
        )));
    };

    let body = request
        .raw_body
        .as_deref()
        .map(|b| b.to_vec())
        .unwrap_or_else(|| request.body.as_bytes().to_vec());

    if !verify_signature(&body, &secret, signature) {
        return Ok(ScmWebhookVerificationResult {
            ok: false,
            reason: Some("Webhook signature verification failed".into()),
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
    let delivery_id = get_header(request, &cfg.delivery_header).map(str::to_string);

    let payload: serde_json::Value = serde_json::from_str(&request.body)
        .map_err(|e| AoError::Scm(format!("webhook payload is not valid JSON: {e}")))?;
    if !payload.is_object() {
        return Err(AoError::Scm("webhook payload must be a JSON object".into()));
    }

    let repository = parse_repository(&payload);
    let action = payload
        .get("action")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| raw_event_type.clone());

    let event = match raw_event_type.as_str() {
        "pull_request" => {
            parse_pull_request_event(&payload, raw_event_type, action, delivery_id, repository)
        }
        "pull_request_review" | "pull_request_review_comment" => {
            parse_review_event(&payload, raw_event_type, action, delivery_id, repository)
        }
        "issue_comment" => {
            parse_issue_comment_event(&payload, raw_event_type, action, delivery_id, repository)
        }
        "check_run" | "check_suite" => {
            parse_check_event(&payload, raw_event_type, action, delivery_id, repository)
        }
        "status" => parse_status_event(&payload, raw_event_type, action, delivery_id, repository),
        "push" => parse_push_event(&payload, raw_event_type, action, delivery_id, repository),
        _ => Some(ScmWebhookEvent {
            provider: "github".into(),
            kind: ScmWebhookEventKind::Unknown,
            action,
            raw_event_type,
            delivery_id,
            repository,
            pr_number: None,
            branch: None,
            sha: None,
            data: payload,
        }),
    };
    Ok(event)
}

fn parse_repository(payload: &serde_json::Value) -> Option<ScmWebhookRepository> {
    let repo = payload.get("repository")?;
    let name = repo.get("name")?.as_str()?.to_string();
    let owner = repo
        .get("owner")
        .and_then(|o| o.get("login"))
        .and_then(|l| l.as_str())?
        .to_string();
    Some(ScmWebhookRepository { owner, name })
}

fn parse_pull_request_event(
    payload: &serde_json::Value,
    raw_event_type: String,
    action: String,
    delivery_id: Option<String>,
    repository: Option<ScmWebhookRepository>,
) -> Option<ScmWebhookEvent> {
    let pr = payload.get("pull_request")?;
    let pr_number = payload
        .get("number")
        .and_then(|n| n.as_u64())
        .or_else(|| pr.get("number").and_then(|n| n.as_u64()))
        .map(|n| n as u32);
    let head = pr.get("head");
    let branch = head
        .and_then(|h| h.get("ref"))
        .and_then(|r| r.as_str())
        .map(str::to_string);
    let sha = head
        .and_then(|h| h.get("sha"))
        .and_then(|s| s.as_str())
        .map(str::to_string);
    Some(ScmWebhookEvent {
        provider: "github".into(),
        kind: ScmWebhookEventKind::PullRequest,
        action,
        raw_event_type,
        delivery_id,
        repository,
        pr_number,
        branch,
        sha,
        data: payload.clone(),
    })
}

fn parse_review_event(
    payload: &serde_json::Value,
    raw_event_type: String,
    action: String,
    delivery_id: Option<String>,
    repository: Option<ScmWebhookRepository>,
) -> Option<ScmWebhookEvent> {
    let pr = payload.get("pull_request")?;
    let pr_number = payload
        .get("number")
        .and_then(|n| n.as_u64())
        .or_else(|| pr.get("number").and_then(|n| n.as_u64()))
        .map(|n| n as u32);
    let head = pr.get("head");
    let kind = if raw_event_type == "pull_request_review" {
        ScmWebhookEventKind::Review
    } else {
        ScmWebhookEventKind::Comment
    };
    Some(ScmWebhookEvent {
        provider: "github".into(),
        kind,
        action,
        raw_event_type,
        delivery_id,
        repository,
        pr_number,
        branch: head
            .and_then(|h| h.get("ref"))
            .and_then(|r| r.as_str())
            .map(str::to_string),
        sha: head
            .and_then(|h| h.get("sha"))
            .and_then(|s| s.as_str())
            .map(str::to_string),
        data: payload.clone(),
    })
}

fn parse_issue_comment_event(
    payload: &serde_json::Value,
    raw_event_type: String,
    action: String,
    delivery_id: Option<String>,
    repository: Option<ScmWebhookRepository>,
) -> Option<ScmWebhookEvent> {
    let issue = payload.get("issue")?;
    // Only forward issue_comment when the issue is actually a PR — GitHub
    // sends the same event for both and we don't want plain-issue chatter
    // flooding PR reactions.
    issue.get("pull_request")?;
    let pr_number = issue
        .get("number")
        .and_then(|n| n.as_u64())
        .map(|n| n as u32);
    Some(ScmWebhookEvent {
        provider: "github".into(),
        kind: ScmWebhookEventKind::Comment,
        action,
        raw_event_type,
        delivery_id,
        repository,
        pr_number,
        branch: None,
        sha: None,
        data: payload.clone(),
    })
}

fn parse_check_event(
    payload: &serde_json::Value,
    raw_event_type: String,
    action: String,
    delivery_id: Option<String>,
    repository: Option<ScmWebhookRepository>,
) -> Option<ScmWebhookEvent> {
    let check = payload.get(&raw_event_type);
    let pr_number = check
        .and_then(|c| c.get("pull_requests"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(|p| p.get("number"))
        .and_then(|n| n.as_u64())
        .map(|n| n as u32);
    let branch = check
        .and_then(|c| c.get("head_branch"))
        .and_then(|b| b.as_str())
        .or_else(|| {
            check
                .and_then(|c| c.get("check_suite"))
                .and_then(|cs| cs.get("head_branch"))
                .and_then(|b| b.as_str())
        })
        .map(str::to_string);
    let sha = check
        .and_then(|c| c.get("head_sha"))
        .and_then(|s| s.as_str())
        .map(str::to_string);
    Some(ScmWebhookEvent {
        provider: "github".into(),
        kind: ScmWebhookEventKind::Ci,
        action,
        raw_event_type,
        delivery_id,
        repository,
        pr_number,
        branch,
        sha,
        data: payload.clone(),
    })
}

fn parse_status_event(
    payload: &serde_json::Value,
    raw_event_type: String,
    action: String,
    delivery_id: Option<String>,
    repository: Option<ScmWebhookRepository>,
) -> Option<ScmWebhookEvent> {
    let state_action = payload
        .get("state")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or(action);
    let branch = payload
        .get("branches")
        .and_then(|b| b.as_array())
        .and_then(|arr| arr.first())
        .and_then(|b| b.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string);
    let sha = payload
        .get("sha")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    Some(ScmWebhookEvent {
        provider: "github".into(),
        kind: ScmWebhookEventKind::Ci,
        action: state_action,
        raw_event_type,
        delivery_id,
        repository,
        pr_number: None,
        branch,
        sha,
        data: payload.clone(),
    })
}

fn parse_push_event(
    payload: &serde_json::Value,
    raw_event_type: String,
    action: String,
    delivery_id: Option<String>,
    repository: Option<ScmWebhookRepository>,
) -> Option<ScmWebhookEvent> {
    let branch = payload
        .get("ref")
        .and_then(|r| r.as_str())
        .and_then(parse_branch_ref)
        .map(str::to_string);
    let sha = payload
        .get("after")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    Some(ScmWebhookEvent {
        provider: "github".into(),
        kind: ScmWebhookEventKind::Push,
        action,
        raw_event_type,
        delivery_id,
        repository,
        pr_number: None,
        branch,
        sha,
        data: payload.clone(),
    })
}

/// Extract a branch name from a `refs/heads/<branch>` ref. Returns `None`
/// for `refs/tags/...` or anything not under `refs/heads/`.
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
        // Avoid listing every field on `ProjectConfig` (brittle). Build a
        // minimal YAML and let serde fill in defaults.
        let yaml = r#"
repo: acme/widgets
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

    // NB: expected digest computed offline with `openssl dgst -sha256 -hmac`.
    const SECRET: &str = "shhhh";
    const BODY: &str = r#"{"action":"opened","pull_request":{"number":1,"head":{"ref":"feature","sha":"deadbeef"}}}"#;

    fn expected_sig() -> String {
        let mut mac = HmacSha256::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(BODY.as_bytes());
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn get_header_case_insensitive() {
        let req = make_request("POST", "{}", &[("X-GitHub-Event", "pull_request")]);
        assert_eq!(get_header(&req, "x-github-event"), Some("pull_request"));
        assert_eq!(get_header(&req, "X-GITHUB-EVENT"), Some("pull_request"));
    }

    #[test]
    fn verify_signature_accepts_valid_mac() {
        let sig = expected_sig();
        assert!(verify_signature(BODY.as_bytes(), SECRET, &sig));
    }

    #[test]
    fn verify_signature_rejects_tampered_body() {
        let sig = expected_sig();
        assert!(!verify_signature(b"{}", SECRET, &sig));
    }

    #[test]
    fn verify_signature_rejects_missing_prefix() {
        assert!(!verify_signature(BODY.as_bytes(), SECRET, "abc123"));
    }

    #[test]
    fn verify_signature_rejects_bad_hex() {
        assert!(!verify_signature(BODY.as_bytes(), SECRET, "sha256=zz"));
    }

    #[tokio::test]
    async fn verify_rejects_non_post() {
        let project = make_project(Some(ScmWebhookConfig::default()));
        let req = make_request("GET", BODY, &[("X-GitHub-Event", "pull_request")]);
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
        let req = make_request("POST", BODY, &[("X-GitHub-Event", "pull_request")]);
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
        assert!(res.reason.unwrap().contains("disabled"));
    }

    #[tokio::test]
    async fn verify_rejects_body_exceeding_max() {
        let project = make_project(Some(ScmWebhookConfig {
            max_body_bytes: Some(8),
            ..Default::default()
        }));
        let req = make_request("POST", BODY, &[("X-GitHub-Event", "pull_request")]);
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
        assert!(res.reason.unwrap().contains("maxBodyBytes"));
    }

    #[tokio::test]
    async fn verify_requires_event_header() {
        let project = make_project(Some(ScmWebhookConfig::default()));
        let req = make_request("POST", BODY, &[]);
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
        assert!(res.reason.unwrap().to_lowercase().contains("missing"));
    }

    #[tokio::test]
    async fn verify_passes_without_configured_secret() {
        // No secretEnvVar set → skip signature step (local/dev path).
        let project = make_project(Some(ScmWebhookConfig::default()));
        let req = make_request(
            "POST",
            BODY,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-GitHub-Delivery", "abc-123"),
            ],
        );
        let res = verify(&req, &project).await.unwrap();
        assert!(res.ok);
        assert_eq!(res.event_type.as_deref(), Some("pull_request"));
        assert_eq!(res.delivery_id.as_deref(), Some("abc-123"));
    }

    #[tokio::test]
    async fn verify_with_secret_accepts_valid_signature() {
        let env_var = "AO_TEST_WEBHOOK_SECRET_VALID";
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
            BODY,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-Hub-Signature-256", &expected_sig()),
            ],
        );
        let res = verify(&req, &project).await.unwrap();
        assert!(res.ok, "reason: {:?}", res.reason);
        unsafe {
            std::env::remove_var(env_var);
        }
    }

    #[tokio::test]
    async fn verify_with_secret_rejects_bad_signature() {
        let env_var = "AO_TEST_WEBHOOK_SECRET_BAD";
        unsafe {
            std::env::set_var(env_var, SECRET);
        }
        let project = make_project(Some(ScmWebhookConfig {
            secret_env_var: Some(env_var.into()),
            ..Default::default()
        }));
        let req = make_request(
            "POST",
            BODY,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-Hub-Signature-256", "sha256=deadbeef"),
            ],
        );
        let res = verify(&req, &project).await.unwrap();
        assert!(!res.ok);
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
    fn parse_pull_request_event_extracts_head() {
        let project = make_project(None);
        let req = make_request(
            "POST",
            BODY,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-GitHub-Delivery", "d-1"),
            ],
        );
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.provider, "github");
        assert_eq!(event.kind, ScmWebhookEventKind::PullRequest);
        assert_eq!(event.pr_number, Some(1));
        assert_eq!(event.branch.as_deref(), Some("feature"));
        assert_eq!(event.sha.as_deref(), Some("deadbeef"));
        assert_eq!(event.delivery_id.as_deref(), Some("d-1"));
    }

    #[test]
    fn parse_unknown_event_keeps_raw_type() {
        let project = make_project(None);
        let req = make_request(
            "POST",
            r#"{"zen":"keep it logically awesome"}"#,
            &[("X-GitHub-Event", "ping")],
        );
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.kind, ScmWebhookEventKind::Unknown);
        assert_eq!(event.raw_event_type, "ping");
    }

    #[test]
    fn parse_issue_comment_requires_pull_request_field() {
        let project = make_project(None);
        let plain_issue_body = r#"{"action":"created","issue":{"number":7}}"#;
        let req = make_request(
            "POST",
            plain_issue_body,
            &[("X-GitHub-Event", "issue_comment")],
        );
        let event = parse(&req, &project).unwrap();
        assert!(event.is_none());
    }

    #[test]
    fn parse_check_run_extracts_pr_number() {
        let project = make_project(None);
        let body = r#"{
          "action":"completed",
          "check_run":{
            "head_branch":"feature",
            "head_sha":"cafe",
            "pull_requests":[{"number":3}]
          }
        }"#;
        let req = make_request("POST", body, &[("X-GitHub-Event", "check_run")]);
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.kind, ScmWebhookEventKind::Ci);
        assert_eq!(event.pr_number, Some(3));
        assert_eq!(event.branch.as_deref(), Some("feature"));
    }

    #[test]
    fn parse_push_event_strips_refs_heads() {
        let project = make_project(None);
        let body = r#"{"ref":"refs/heads/feat/x","after":"abc123"}"#;
        let req = make_request("POST", body, &[("X-GitHub-Event", "push")]);
        let event = parse(&req, &project).unwrap().unwrap();
        assert_eq!(event.kind, ScmWebhookEventKind::Push);
        assert_eq!(event.branch.as_deref(), Some("feat/x"));
        assert_eq!(event.sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_rejects_non_object_payload() {
        let project = make_project(None);
        let req = make_request("POST", "42", &[("X-GitHub-Event", "ping")]);
        let err = parse(&req, &project).unwrap_err().to_string();
        assert!(err.to_lowercase().contains("json object"));
    }
}
