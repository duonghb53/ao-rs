//! Shared notifier registration for `watch` and `dashboard`.

use std::sync::Arc;

use ao_core::{AoConfig, NotificationRouting, NotifierRegistry};
use ao_plugin_notifier_desktop::DesktopNotifier;
use ao_plugin_notifier_discord::DiscordNotifier;
use ao_plugin_notifier_ntfy::NtfyNotifier;
use ao_plugin_notifier_slack::SlackNotifier;
use ao_plugin_notifier_stdout::StdoutNotifier;

fn yaml_string(v: &serde_yaml::Value) -> Option<String> {
    v.as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn notifier_extra_string(cfg: &ao_core::config::PluginConfig, key: &str) -> Option<String> {
    cfg.extra.get(key).and_then(yaml_string)
}

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

    // Prefer config-driven ntfy setup (via `ao-rs setup openclaw`), then fall back to env vars.
    //
    // Config format:
    //   notifiers:
    //     openclaw:
    //       url: https://ntfy.sh
    //       token: my-topic
    //
    // For backwards compatibility we also accept `notifiers.ntfy.{url,topic}`.
    let mut ntfy_topic: Option<String> = None;
    let mut ntfy_base: Option<String> = None;

    if let Some(openclaw) = config.notifiers.get("openclaw") {
        ntfy_topic = notifier_extra_string(openclaw, "token");
        ntfy_base = notifier_extra_string(openclaw, "url");
    }
    if ntfy_topic.is_none() {
        if let Some(ntfy) = config.notifiers.get("ntfy") {
            ntfy_topic = notifier_extra_string(ntfy, "topic")
                .or_else(|| notifier_extra_string(ntfy, "token"));
            ntfy_base = notifier_extra_string(ntfy, "url");
        }
    }

    if ntfy_topic.is_none() {
        ntfy_topic = std::env::var("AO_NTFY_TOPIC")
            .ok()
            .map(|s| s.trim().to_string());
    }
    if ntfy_base.is_none() {
        ntfy_base = std::env::var("AO_NTFY_URL")
            .ok()
            .map(|s| s.trim().to_string());
    }

    if let Some(topic) = ntfy_topic.filter(|s| !s.is_empty()) {
        let base = ntfy_base
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://ntfy.sh".to_string());
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
