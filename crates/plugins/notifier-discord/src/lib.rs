//! Discord webhook notifier plugin — Slice 4 Phase B.
//!
//! Delivers notifications via HTTP POST to a Discord webhook URL.
//! Uses rich embeds with color-coded priority levels so notifications
//! are visually scannable in a Discord channel.
//!
//! ## Configuration
//!
//! Construction takes a webhook URL (required), set once at startup
//! via `ao-cli` from the `AO_DISCORD_WEBHOOK_URL` environment variable.
//!
//! ## Color mapping
//!
//! | ao-rs | Color | Hex |
//! |-------|-------|-----|
//! | Urgent | Red | #FF0000 |
//! | Action | Orange | #FF8C00 |
//! | Warning | Yellow | #FFD700 |
//! | Info | Green | #00C853 |
//!
//! Escalated notifications always use red regardless of priority.
//!
//! ## Error handling
//!
//! Same pattern as the ntfy plugin:
//! - Timeout → `NotifierError::Timeout`
//! - Connection/DNS → `NotifierError::Unavailable`
//! - Non-2xx → `NotifierError::Service { status, body }`
//!
//! ## Rate limiting
//!
//! Discord webhooks allow ~30 requests per minute. At the default 5s
//! poll interval with a small session fleet, this is not a concern.
//! The plugin does not implement its own rate limiting.

use ao_core::{
    notifier::{NotificationPayload, Notifier, NotifierError},
    reactions::EventPriority,
};
use async_trait::async_trait;
use serde::Serialize;

const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Discord embed color constants (decimal RGB).
const COLOR_RED: u32 = 0xFF0000;
const COLOR_ORANGE: u32 = 0xFF8C00;
const COLOR_YELLOW: u32 = 0xFFD700;
const COLOR_GREEN: u32 = 0x00C853;

/// Notifier that POSTs to a Discord webhook.
pub struct DiscordNotifier {
    webhook_url: String,
    client: reqwest::Client,
}

impl DiscordNotifier {
    /// Create a notifier for the given Discord webhook URL.
    pub fn new(webhook_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self {
            webhook_url: webhook_url.into(),
            client,
        }
    }

    /// Exposed for testing.
    pub fn webhook_url(&self) -> &str {
        &self.webhook_url
    }
}

/// Map `EventPriority` to a Discord embed color.
pub(crate) fn embed_color(priority: EventPriority, escalated: bool) -> u32 {
    if escalated {
        return COLOR_RED;
    }
    match priority {
        EventPriority::Urgent => COLOR_RED,
        EventPriority::Action => COLOR_ORANGE,
        EventPriority::Warning => COLOR_YELLOW,
        EventPriority::Info => COLOR_GREEN,
    }
}

// --- Discord webhook JSON types (private) ---

#[derive(Serialize)]
struct WebhookPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    embeds: Vec<Embed>,
}

#[derive(Serialize)]
struct Embed {
    title: String,
    description: String,
    color: u32,
    footer: Footer,
}

#[derive(Serialize)]
struct Footer {
    text: String,
}

#[async_trait]
impl Notifier for DiscordNotifier {
    fn name(&self) -> &str {
        "discord"
    }

    async fn send(&self, payload: &NotificationPayload) -> Result<(), NotifierError> {
        let title = if payload.escalated {
            format!("[ESCALATED] {}", payload.title)
        } else {
            payload.title.clone()
        };

        let content = if payload.escalated {
            Some("Escalated notification — retries exhausted".to_string())
        } else {
            None
        };

        let body = WebhookPayload {
            content,
            embeds: vec![Embed {
                title,
                description: payload.body.clone(),
                color: embed_color(payload.priority, payload.escalated),
                footer: Footer {
                    text: format!(
                        "ao-rs | {} | {}",
                        payload.reaction_key,
                        payload.priority.as_str()
                    ),
                },
            }],
        };

        let response = self
            .client
            .post(&self.webhook_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    NotifierError::Timeout {
                        elapsed_ms: DEFAULT_TIMEOUT_SECS * 1000,
                    }
                } else if e.is_connect() {
                    NotifierError::Unavailable(format!("discord connection failed: {e}"))
                } else {
                    NotifierError::Io(format!("discord request failed: {e}"))
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

        tracing::debug!("discord notification sent");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_discord() {
        let n = DiscordNotifier::new("https://discord.com/api/webhooks/test");
        assert_eq!(n.name(), "discord");
    }

    #[test]
    fn webhook_url_is_preserved() {
        let url = "https://discord.com/api/webhooks/123/abc";
        let n = DiscordNotifier::new(url);
        assert_eq!(n.webhook_url(), url);
    }

    #[test]
    fn color_mapping_covers_all_variants() {
        assert_eq!(embed_color(EventPriority::Urgent, false), COLOR_RED);
        assert_eq!(embed_color(EventPriority::Action, false), COLOR_ORANGE);
        assert_eq!(embed_color(EventPriority::Warning, false), COLOR_YELLOW);
        assert_eq!(embed_color(EventPriority::Info, false), COLOR_GREEN);
    }

    #[test]
    fn escalated_color_is_always_red() {
        assert_eq!(embed_color(EventPriority::Urgent, true), COLOR_RED);
        assert_eq!(embed_color(EventPriority::Action, true), COLOR_RED);
        assert_eq!(embed_color(EventPriority::Warning, true), COLOR_RED);
        assert_eq!(embed_color(EventPriority::Info, true), COLOR_RED);
    }

    #[test]
    fn embed_structure_serializes_correctly() {
        let body = WebhookPayload {
            content: None,
            embeds: vec![Embed {
                title: "CI failed".into(),
                description: "Tests broke".into(),
                color: COLOR_RED,
                footer: Footer {
                    text: "ao-rs | ci-failed | urgent".into(),
                },
            }],
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json["content"].is_null());
        assert_eq!(json["embeds"][0]["title"], "CI failed");
        assert_eq!(json["embeds"][0]["color"], COLOR_RED);
        assert_eq!(
            json["embeds"][0]["footer"]["text"],
            "ao-rs | ci-failed | urgent"
        );
    }
}
