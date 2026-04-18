//! ntfy.sh notifier plugin — Slice 3 Phase D.
//!
//! Delivers notifications via HTTP POST to an [ntfy](https://ntfy.sh)
//! server. One POST, no JSON body (just plain text). Public topics
//! need no auth; private / self-hosted servers can require bearer
//! tokens or HTTP Basic.
//!
//! ## Configuration
//!
//! Construction takes a topic (required) and an optional base URL
//! (defaults to `https://ntfy.sh`). An optional [`NtfyAuth`] adds
//! the `Authorization` header for private servers. All fields are
//! set once at startup via `ao-cli` and immutable thereafter.
//!
//! ## Priority mapping
//!
//! ntfy uses integer priorities 1–5. We map `EventPriority`:
//!
//! | ao-rs | ntfy | ntfy label |
//! |-------|------|------------|
//! | Urgent | 5 | max |
//! | Action | 4 | high |
//! | Warning | 3 | default |
//! | Info | 2 | low |
//!
//! ## Error handling
//!
//! `send` maps `reqwest` errors to `NotifierError`:
//! - Timeout → `NotifierError::Timeout`
//! - Connection/DNS → `NotifierError::Unavailable`
//! - Non-2xx response → `NotifierError::Service { status, body }`
//!
//! The engine logs and records `success = false` — a flaky ntfy
//! server never wedges the polling tick.

use ao_core::{
    notifier::{NotificationPayload, Notifier, NotifierError},
    reactions::EventPriority,
};
use async_trait::async_trait;

const DEFAULT_BASE_URL: &str = "https://ntfy.sh";
const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Authentication for private / self-hosted ntfy servers.
///
/// `Debug` is manually implemented to redact the secret so credentials
/// never leak through `tracing::debug!` or `{:?}` formatting.
#[derive(Clone)]
pub enum NtfyAuth {
    /// Bearer token — sends `Authorization: Bearer <token>`. Matches
    /// ntfy's access-token header format.
    Bearer(String),
    /// HTTP Basic auth — sends `Authorization: Basic base64(user:pass)`.
    Basic { username: String, password: String },
}

impl std::fmt::Debug for NtfyAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(_) => f.debug_tuple("Bearer").field(&"<redacted>").finish(),
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

/// Notifier that POSTs to an ntfy topic.
pub struct NtfyNotifier {
    topic: String,
    base_url: String,
    auth: Option<NtfyAuth>,
    client: reqwest::Client,
}

impl NtfyNotifier {
    /// Create a notifier for the given topic on the public ntfy.sh
    /// server. Timeout defaults to 5 seconds. No auth.
    pub fn new(topic: impl Into<String>) -> Self {
        Self::with_base_url(topic, DEFAULT_BASE_URL)
    }

    /// Create a notifier pointed at a custom ntfy server (e.g. a
    /// self-hosted instance at `http://ntfy.internal:8080`). No auth.
    pub fn with_base_url(topic: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_base_url_and_auth(topic, base_url, None)
    }

    /// Create a notifier with a custom base URL and optional auth
    /// header for private / self-hosted ntfy servers.
    pub fn with_base_url_and_auth(
        topic: impl Into<String>,
        base_url: impl Into<String>,
        auth: Option<NtfyAuth>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self {
            topic: topic.into(),
            base_url: base_url.into(),
            auth,
            client,
        }
    }

    /// `true` if this notifier is configured with auth credentials.
    pub fn has_auth(&self) -> bool {
        self.auth.is_some()
    }
}

/// Map `EventPriority` to ntfy's integer priority header.
fn ntfy_priority(p: EventPriority) -> &'static str {
    match p {
        EventPriority::Urgent => "5",
        EventPriority::Action => "4",
        EventPriority::Warning => "3",
        EventPriority::Info => "2",
    }
}

#[async_trait]
impl Notifier for NtfyNotifier {
    fn name(&self) -> &str {
        "ntfy"
    }

    async fn send(&self, payload: &NotificationPayload) -> Result<(), NotifierError> {
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), self.topic);

        let tag = if payload.escalated {
            "ao-rs,escalated"
        } else {
            "ao-rs"
        };

        let mut request = self
            .client
            .post(&url)
            .header("X-Title", &payload.title)
            .header("X-Priority", ntfy_priority(payload.priority))
            .header("X-Tags", tag)
            .body(payload.body.clone());

        if let Some(auth) = &self.auth {
            request = match auth {
                NtfyAuth::Bearer(token) => request.bearer_auth(token),
                NtfyAuth::Basic { username, password } => {
                    request.basic_auth(username, Some(password))
                }
            };
        }

        let response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                NotifierError::Timeout {
                    elapsed_ms: DEFAULT_TIMEOUT_SECS * 1000,
                }
            } else if e.is_connect() {
                NotifierError::Unavailable(format!("ntfy connection failed: {e}"))
            } else {
                NotifierError::Io(format!("ntfy request failed: {e}"))
            }
        })?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".into());
            return Err(NotifierError::Service {
                status,
                message: body,
            });
        }

        tracing::debug!(topic = %self.topic, authed = self.auth.is_some(), "ntfy notification sent");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{notifier::NotificationPayload, reactions::ReactionAction, types::SessionId};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fake_payload(escalated: bool, priority: EventPriority) -> NotificationPayload {
        NotificationPayload {
            session_id: SessionId("sess-test".into()),
            reaction_key: "ci-failed".into(),
            action: ReactionAction::Notify,
            priority,
            title: "CI failed".into(),
            body: "Tests failed on main".into(),
            escalated,
        }
    }

    #[test]
    fn name_is_ntfy() {
        let n = NtfyNotifier::new("test-topic");
        assert_eq!(n.name(), "ntfy");
    }

    #[test]
    fn default_base_url_is_ntfy_sh() {
        let n = NtfyNotifier::new("test-topic");
        assert_eq!(n.base_url, "https://ntfy.sh");
    }

    #[test]
    fn custom_base_url_is_preserved() {
        let n = NtfyNotifier::with_base_url("t", "http://localhost:8080");
        assert_eq!(n.base_url, "http://localhost:8080");
    }

    #[test]
    fn priority_mapping_covers_all_variants() {
        assert_eq!(ntfy_priority(EventPriority::Urgent), "5");
        assert_eq!(ntfy_priority(EventPriority::Action), "4");
        assert_eq!(ntfy_priority(EventPriority::Warning), "3");
        assert_eq!(ntfy_priority(EventPriority::Info), "2");
    }

    #[test]
    fn has_auth_reports_configured_state() {
        let none = NtfyNotifier::new("t");
        assert!(!none.has_auth());

        let bearer = NtfyNotifier::with_base_url_and_auth(
            "t",
            "http://x",
            Some(NtfyAuth::Bearer("tk_secret".into())),
        );
        assert!(bearer.has_auth());
    }

    #[test]
    fn debug_redacts_bearer_token() {
        let a = NtfyAuth::Bearer("tk_supersecret_12345".into());
        let rendered = format!("{a:?}");
        assert!(!rendered.contains("supersecret"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn debug_redacts_basic_password_but_keeps_username() {
        let a = NtfyAuth::Basic {
            username: "alice".into(),
            password: "hunter2".into(),
        };
        let rendered = format!("{a:?}");
        assert!(rendered.contains("alice"));
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("<redacted>"));
    }

    // --- wiremock-backed integration tests ---

    #[tokio::test]
    async fn send_posts_to_topic_with_expected_headers_and_body() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/my-topic"))
            .and(header("X-Title", "CI failed"))
            .and(header("X-Priority", "4"))
            .and(header("X-Tags", "ao-rs"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let n = NtfyNotifier::with_base_url("my-topic", server.uri());
        let payload = fake_payload(false, EventPriority::Action);

        let result = n.send(&payload).await;
        assert!(result.is_ok(), "send failed: {result:?}");

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let body = String::from_utf8_lossy(&received[0].body);
        assert_eq!(body, "Tests failed on main");
        assert!(
            received[0].headers.get("authorization").is_none(),
            "no auth header should be sent when auth is None"
        );
    }

    #[tokio::test]
    async fn send_tags_escalated_when_payload_is_escalated() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/t"))
            .and(header("X-Priority", "5"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let n = NtfyNotifier::with_base_url("t", server.uri());
        n.send(&fake_payload(true, EventPriority::Urgent))
            .await
            .unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let tags = received[0]
            .headers
            .get("x-tags")
            .expect("X-Tags header should be present")
            .to_str()
            .unwrap();
        assert_eq!(tags, "ao-rs,escalated");
    }

    #[tokio::test]
    async fn send_includes_bearer_auth_header() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/t"))
            .and(header("authorization", "Bearer tk_abc123"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let n = NtfyNotifier::with_base_url_and_auth(
            "t",
            server.uri(),
            Some(NtfyAuth::Bearer("tk_abc123".into())),
        );
        n.send(&fake_payload(false, EventPriority::Info))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_includes_basic_auth_header() {
        let server = MockServer::start().await;

        // base64("alice:hunter2") = "YWxpY2U6aHVudGVyMg=="
        Mock::given(method("POST"))
            .and(path("/t"))
            .and(header("authorization", "Basic YWxpY2U6aHVudGVyMg=="))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let n = NtfyNotifier::with_base_url_and_auth(
            "t",
            server.uri(),
            Some(NtfyAuth::Basic {
                username: "alice".into(),
                password: "hunter2".into(),
            }),
        );
        n.send(&fake_payload(false, EventPriority::Info))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_trims_trailing_slash_on_base_url() {
        let server = MockServer::start().await;

        // If base_url keeps its trailing slash, reqwest hits "//t" and the
        // mock below (which matches the canonical "/t") will fail.
        Mock::given(method("POST"))
            .and(path("/t"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let base_with_slash = format!("{}/", server.uri());
        let n = NtfyNotifier::with_base_url("t", base_with_slash);
        n.send(&fake_payload(false, EventPriority::Warning))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_maps_non_2xx_to_service_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/t"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let n = NtfyNotifier::with_base_url("t", server.uri());
        let err = n
            .send(&fake_payload(false, EventPriority::Info))
            .await
            .unwrap_err();

        match err {
            NotifierError::Service { status, message } => {
                assert_eq!(status, 401);
                assert!(message.contains("unauthorized"), "body was {message:?}");
            }
            other => panic!("expected Service error, got {other:?}"),
        }
    }
}
