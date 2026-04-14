//! Slack webhook notifier plugin — Issue #19 Phase 2.
//!
//! Delivers notifications via Slack Incoming Webhooks (HTTP POST).
//! Uses a simple attachment + blocks layout with a color stripe so
//! priority is visually scannable in-channel.
//!
//! ## Configuration
//!
//! Construction takes a webhook URL (required), typically passed from
//! `ao-cli` via the `AO_SLACK_WEBHOOK_URL` environment variable.
//!
//! ## Priority mapping (attachment color)
//!
//! | ao-rs | Slack color |
//! |-------|------------|
//! | Urgent | red |
//! | Action | orange |
//! | Warning | yellow |
//! | Info | green |
//!
//! Escalated notifications always use red regardless of priority and
//! prefix the title with `[ESCALATED]`.

use ao_core::{
    notifier::{NotificationPayload, Notifier, NotifierError},
    reactions::EventPriority,
};
use async_trait::async_trait;
use serde::Serialize;

const DEFAULT_TIMEOUT_SECS: u64 = 5;

// Slack "color" field accepts a hex string like "#36a64f" (or named colors).
const COLOR_RED: &str = "#E01E5A";
const COLOR_ORANGE: &str = "#FF8C00";
const COLOR_YELLOW: &str = "#FFD700";
const COLOR_GREEN: &str = "#2EB67D";

/// Notifier that POSTs to a Slack incoming webhook URL.
pub struct SlackNotifier {
    webhook_url: String,
    client: reqwest::Client,
}

impl SlackNotifier {
    /// Create a notifier for the given Slack webhook URL.
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

pub(crate) fn attachment_color(priority: EventPriority, escalated: bool) -> &'static str {
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

// --- Slack incoming webhook JSON types (private) ---

#[derive(Serialize)]
struct WebhookPayload {
    attachments: Vec<Attachment>,
}

#[derive(Serialize)]
struct Attachment {
    color: &'static str,
    blocks: Vec<Block>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum Block {
    #[serde(rename = "header")]
    Header { text: Text },
    #[serde(rename = "section")]
    Section { text: Text },
    #[serde(rename = "context")]
    Context { elements: Vec<Text> },
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum Text {
    #[serde(rename = "plain_text")]
    PlainText { text: String },
    #[serde(rename = "mrkdwn")]
    Mrkdwn { text: String },
}

fn build_webhook_payload(payload: &NotificationPayload) -> WebhookPayload {
    let title = if payload.escalated {
        format!("[ESCALATED] {}", payload.title)
    } else {
        payload.title.clone()
    };

    let context = if payload.escalated {
        format!(
            "*ao-rs* · `{}` · `{}` · `{}` · escalated",
            payload.session_id,
            payload.reaction_key,
            payload.priority.as_str()
        )
    } else {
        format!(
            "*ao-rs* · `{}` · `{}` · `{}`",
            payload.session_id,
            payload.reaction_key,
            payload.priority.as_str()
        )
    };

    WebhookPayload {
        attachments: vec![Attachment {
            color: attachment_color(payload.priority, payload.escalated),
            blocks: vec![
                Block::Header {
                    text: Text::PlainText { text: title },
                },
                Block::Section {
                    text: Text::Mrkdwn {
                        text: payload.body.clone(),
                    },
                },
                Block::Context {
                    elements: vec![Text::Mrkdwn { text: context }],
                },
            ],
        }],
    }
}

#[async_trait]
impl Notifier for SlackNotifier {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&self, payload: &NotificationPayload) -> Result<(), NotifierError> {
        let body = build_webhook_payload(payload);

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
                    NotifierError::Unavailable(format!("slack connection failed: {e}"))
                } else {
                    NotifierError::Io(format!("slack request failed: {e}"))
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

        tracing::debug!("slack notification sent");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{
        reactions::{EventPriority, ReactionAction},
        types::SessionId,
    };

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
    fn name_is_slack() {
        let n = SlackNotifier::new("https://hooks.slack.com/services/T000/B000/XXX");
        assert_eq!(n.name(), "slack");
    }

    #[test]
    fn webhook_url_is_preserved() {
        let url = "https://hooks.slack.com/services/T000/B000/XXX";
        let n = SlackNotifier::new(url);
        assert_eq!(n.webhook_url(), url);
    }

    #[test]
    fn color_mapping_covers_all_variants() {
        assert_eq!(attachment_color(EventPriority::Urgent, false), COLOR_RED);
        assert_eq!(attachment_color(EventPriority::Action, false), COLOR_ORANGE);
        assert_eq!(
            attachment_color(EventPriority::Warning, false),
            COLOR_YELLOW
        );
        assert_eq!(attachment_color(EventPriority::Info, false), COLOR_GREEN);
    }

    #[test]
    fn escalated_color_is_always_red() {
        assert_eq!(attachment_color(EventPriority::Urgent, true), COLOR_RED);
        assert_eq!(attachment_color(EventPriority::Action, true), COLOR_RED);
        assert_eq!(attachment_color(EventPriority::Warning, true), COLOR_RED);
        assert_eq!(attachment_color(EventPriority::Info, true), COLOR_RED);
    }

    #[test]
    fn webhook_payload_structure_serializes_correctly() {
        let payload = fake_payload(false, EventPriority::Action);
        let body = build_webhook_payload(&payload);
        let json = serde_json::to_value(&body).unwrap();

        assert!(json["attachments"].is_array());
        assert_eq!(json["attachments"][0]["color"], COLOR_ORANGE);

        // Blocks: header + section + context
        assert_eq!(json["attachments"][0]["blocks"][0]["type"], "header");
        assert_eq!(
            json["attachments"][0]["blocks"][0]["text"]["type"],
            "plain_text"
        );
        assert_eq!(
            json["attachments"][0]["blocks"][0]["text"]["text"],
            "CI failed"
        );

        assert_eq!(json["attachments"][0]["blocks"][1]["type"], "section");
        assert_eq!(
            json["attachments"][0]["blocks"][1]["text"]["type"],
            "mrkdwn"
        );
        assert_eq!(
            json["attachments"][0]["blocks"][1]["text"]["text"],
            "Tests failed on main"
        );

        assert_eq!(json["attachments"][0]["blocks"][2]["type"], "context");
        assert!(json["attachments"][0]["blocks"][2]["elements"].is_array());
    }

    #[test]
    fn escalated_prefix_is_applied_in_header_text() {
        let payload = fake_payload(true, EventPriority::Warning);
        let body = build_webhook_payload(&payload);
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(
            json["attachments"][0]["blocks"][0]["text"]["text"],
            "[ESCALATED] CI failed"
        );
        assert_eq!(json["attachments"][0]["color"], COLOR_RED);
    }
}
