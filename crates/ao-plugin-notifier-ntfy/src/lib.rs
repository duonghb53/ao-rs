//! ntfy.sh notifier plugin — Slice 3 Phase D.
//!
//! Delivers notifications via HTTP POST to an [ntfy](https://ntfy.sh)
//! server. The simplest HTTP-based notifier — one POST, no auth
//! required for public topics, no JSON body (just plain text).
//!
//! ## Configuration
//!
//! Construction takes a topic (required) and an optional base URL
//! (defaults to `https://ntfy.sh`). Both are set once at startup via
//! `ao-cli` and immutable thereafter. Future phases may add auth
//! token support for private ntfy servers.
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

/// Notifier that POSTs to an ntfy topic.
pub struct NtfyNotifier {
    topic: String,
    base_url: String,
    client: reqwest::Client,
}

impl NtfyNotifier {
    /// Create a notifier for the given topic on the public ntfy.sh
    /// server. Timeout defaults to 5 seconds.
    pub fn new(topic: impl Into<String>) -> Self {
        Self::with_base_url(topic, DEFAULT_BASE_URL)
    }

    /// Create a notifier pointed at a custom ntfy server (e.g. a
    /// self-hosted instance at `http://ntfy.internal:8080`).
    pub fn with_base_url(topic: impl Into<String>, base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self {
            topic: topic.into(),
            base_url: base_url.into(),
            client,
        }
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

        let response = self
            .client
            .post(&url)
            .header("X-Title", &payload.title)
            .header("X-Priority", ntfy_priority(payload.priority))
            .header("X-Tags", tag)
            .body(payload.body.clone())
            .send()
            .await
            .map_err(|e| {
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

        tracing::debug!(topic = %self.topic, "ntfy notification sent");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // NOTE: We do not test the actual HTTP POST here — that would
    // require either mocking reqwest or standing up a real ntfy server.
    // The plugin's `send` method is covered by the Phase B integration
    // test pattern (FailNotifier / TestNotifier) at the engine level.
    // End-to-end smoke testing against ntfy.sh is manual.
}
