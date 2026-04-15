//! Shared notifier registration for `watch` and `dashboard`.

use std::sync::Arc;

use ao_core::{AoConfig, NotificationRouting, NotifierRegistry};
use ao_plugin_notifier_desktop::DesktopNotifier;
use ao_plugin_notifier_discord::DiscordNotifier;
use ao_plugin_notifier_ntfy::NtfyNotifier;
use ao_plugin_notifier_slack::SlackNotifier;
use ao_plugin_notifier_stdout::StdoutNotifier;

/// Build the notifier registry from `ao-rs.yaml` routing + env-discovered plugins.
pub(crate) fn notifier_registry_from_config(config: &AoConfig) -> NotifierRegistry {
    let mut notifier_registry = if config.notification_routing.is_empty() {
        use ao_core::reactions::EventPriority;
        use std::collections::HashMap;
        let mut default_routing = HashMap::new();
        for &p in &[
            EventPriority::Urgent,
            EventPriority::Action,
            EventPriority::Warning,
            EventPriority::Info,
        ] {
            default_routing.insert(p, vec!["stdout".to_string()]);
        }
        NotifierRegistry::new(NotificationRouting::from_map(default_routing))
    } else {
        NotifierRegistry::new(config.notification_routing.clone())
    };
    notifier_registry.register("stdout", Arc::new(StdoutNotifier::new()));

    if let Ok(topic) = std::env::var("AO_NTFY_TOPIC") {
        let base = std::env::var("AO_NTFY_URL").unwrap_or_else(|_| "https://ntfy.sh".to_string());
        notifier_registry.register("ntfy", Arc::new(NtfyNotifier::with_base_url(topic, base)));
    }

    notifier_registry.register("desktop", Arc::new(DesktopNotifier::new()));

    if let Ok(webhook_url) = std::env::var("AO_DISCORD_WEBHOOK_URL") {
        notifier_registry.register("discord", Arc::new(DiscordNotifier::new(webhook_url)));
    }

    if let Ok(webhook_url) = std::env::var("AO_SLACK_WEBHOOK_URL") {
        notifier_registry.register("slack", Arc::new(SlackNotifier::new(webhook_url)));
    }

    notifier_registry
}
