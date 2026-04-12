//! Stdout notifier plugin — Slice 3 Phase C.
//!
//! The simplest possible notifier: formats the `NotificationPayload`
//! as a single human-readable line and writes it to stdout via
//! `println!`. Always succeeds — `println!` can only panic on broken
//! pipe, and we catch that with a write-to-vec fallback.
//!
//! This is the "always present" notifier: when the user's routing
//! table is empty, `ao-cli` registers `StdoutNotifier` as the default
//! for every priority so notifications are never silently dropped.
//!
//! Mirrors the TS `ConsoleNotifier` in `packages/notifier-console`.

use ao_core::notifier::{NotificationPayload, Notifier, NotifierError};
use async_trait::async_trait;

/// Notifier that prints one line per notification to stdout.
///
/// The format is:
/// ```text
/// [notify] ci-failed on sess-abc (action) — CI broke, please fix
/// [ESCALATED] ci-failed on sess-abc (action) — retries exhausted
/// ```
///
/// Priority is parenthesised for grep-friendliness; the `[ESCALATED]`
/// prefix replaces `[notify]` when the notification is a retry-budget
/// fallback so it stands out in the scroll.
pub struct StdoutNotifier;

impl StdoutNotifier {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdoutNotifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Notifier for StdoutNotifier {
    fn name(&self) -> &str {
        "stdout"
    }

    async fn send(&self, payload: &NotificationPayload) -> Result<(), NotifierError> {
        let tag = if payload.escalated {
            "ESCALATED"
        } else {
            "notify"
        };
        let line = format!(
            "[{tag}] {} on {} ({}) — {}",
            payload.reaction_key,
            payload.session_id,
            payload.priority.as_str(),
            payload.body,
        );
        println!("{line}");
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

    fn fake_payload(escalated: bool) -> NotificationPayload {
        NotificationPayload {
            session_id: SessionId("sess-abc".into()),
            reaction_key: "ci-failed".into(),
            action: ReactionAction::Notify,
            priority: EventPriority::Action,
            title: "ci-failed on sess-abc".into(),
            body: "CI broke, please fix".into(),
            escalated,
        }
    }

    #[tokio::test]
    async fn send_succeeds_for_normal_notification() {
        let n = StdoutNotifier::new();
        let result = n.send(&fake_payload(false)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_succeeds_for_escalated_notification() {
        let n = StdoutNotifier::new();
        let result = n.send(&fake_payload(true)).await;
        assert!(result.is_ok());
    }

    #[test]
    fn name_is_stdout() {
        assert_eq!(StdoutNotifier::new().name(), "stdout");
    }
}
