//! Desktop notification plugin — Slice 4 Phase A.
//!
//! Delivers notifications via the OS notification daemon using the
//! [`notify-rust`](https://docs.rs/notify-rust) crate. Works on macOS
//! (Notification Center), Linux (libnotify / D-Bus), and Windows
//! (toast notifications).
//!
//! ## Urgency (Linux/Windows only)
//!
//! On Linux and Windows, `notify-rust` supports urgency levels:
//!
//! | ao-rs | notify-rust | Escalated |
//! |-------|-------------|-----------|
//! | Urgent | Critical | Critical |
//! | Action | Critical | Critical |
//! | Warning | Normal | Critical |
//! | Info | Low | Critical |
//!
//! macOS does not support urgency — notifications are displayed with
//! the OS default styling. Escalated notifications still get the
//! `[ESCALATED]` title prefix on all platforms.
//!
//! ## Error handling
//!
//! All `notify_rust::error::Error` variants map to
//! `NotifierError::Unavailable` — desktop notification failures are
//! almost always the notification daemon being unreachable (headless
//! server, SSH session, daemon crashed).

use ao_core::{
    notifier::{NotificationPayload, Notifier, NotifierError},
    reactions::EventPriority,
};
use async_trait::async_trait;
use notify_rust::Notification;

/// Notifier that shows OS-native desktop notifications.
pub struct DesktopNotifier;

impl DesktopNotifier {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DesktopNotifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Map `EventPriority` to a `notify_rust::Urgency` value.
///
/// Only used on Linux and Windows — macOS does not support urgency.
#[cfg(not(target_os = "macos"))]
fn urgency(priority: EventPriority, escalated: bool) -> notify_rust::Urgency {
    use notify_rust::Urgency;
    if escalated {
        return Urgency::Critical;
    }
    match priority {
        EventPriority::Urgent | EventPriority::Action => Urgency::Critical,
        EventPriority::Warning => Urgency::Normal,
        EventPriority::Info => Urgency::Low,
    }
}

/// Build the notification with platform-appropriate settings.
fn build_notification(
    title: &str,
    body: &str,
    priority: EventPriority,
    escalated: bool,
) -> Notification {
    let mut n = Notification::new();
    n.summary(title).body(body);

    // Urgency is only available on Linux and Windows.
    #[cfg(not(target_os = "macos"))]
    {
        n.urgency(urgency(priority, escalated));
    }

    // Suppress unused-variable warning on macOS.
    #[cfg(target_os = "macos")]
    {
        let _ = (priority, escalated);
    }

    n
}

#[async_trait]
impl Notifier for DesktopNotifier {
    fn name(&self) -> &str {
        "desktop"
    }

    async fn send(&self, payload: &NotificationPayload) -> Result<(), NotifierError> {
        let title = if payload.escalated {
            format!("[ESCALATED] {}", payload.title)
        } else {
            payload.title.clone()
        };

        let notification =
            build_notification(&title, &payload.body, payload.priority, payload.escalated);

        // On Linux/Windows, use show_async() to avoid blocking the tokio runtime
        // during D-Bus IPC. On macOS, show() is a fast native API call.
        #[cfg(not(target_os = "macos"))]
        notification
            .show_async()
            .await
            .map_err(|e| NotifierError::Unavailable(format!("desktop notification failed: {e}")))?;

        #[cfg(target_os = "macos")]
        notification
            .show()
            .map_err(|e| NotifierError::Unavailable(format!("desktop notification failed: {e}")))?;

        tracing::debug!("desktop notification sent");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_desktop() {
        assert_eq!(DesktopNotifier::new().name(), "desktop");
    }

    #[test]
    fn default_impl_works() {
        let n: DesktopNotifier = Default::default();
        assert_eq!(n.name(), "desktop");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn urgency_mapping_covers_all_variants() {
        use notify_rust::Urgency;
        assert_eq!(urgency(EventPriority::Urgent, false), Urgency::Critical);
        assert_eq!(urgency(EventPriority::Action, false), Urgency::Critical);
        assert_eq!(urgency(EventPriority::Warning, false), Urgency::Normal);
        assert_eq!(urgency(EventPriority::Info, false), Urgency::Low);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn escalated_always_critical() {
        use notify_rust::Urgency;
        assert_eq!(urgency(EventPriority::Urgent, true), Urgency::Critical);
        assert_eq!(urgency(EventPriority::Action, true), Urgency::Critical);
        assert_eq!(urgency(EventPriority::Warning, true), Urgency::Critical);
        assert_eq!(urgency(EventPriority::Info, true), Urgency::Critical);
    }

    #[test]
    fn build_notification_sets_title_and_body() {
        let n = build_notification("Test Title", "Test Body", EventPriority::Info, false);
        assert_eq!(n.summary, "Test Title");
        assert_eq!(n.body, "Test Body");
    }
}
