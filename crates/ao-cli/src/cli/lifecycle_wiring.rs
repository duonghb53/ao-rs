//! Shared notifier registration for `watch` and `dashboard`.

use std::sync::Arc;

use ao_core::{AoConfig, NotificationRouting, NotifierRegistry};
use ao_plugin_notifier_desktop::DesktopNotifier;
use ao_plugin_notifier_discord::DiscordNotifier;
use ao_plugin_notifier_ntfy::{NtfyAuth, NtfyNotifier};
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

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve ntfy auth from resolved config/env strings. Bearer token wins
/// over Basic when both are set (private-server setups usually prefer
/// access tokens).
fn resolve_ntfy_auth(
    token: Option<String>,
    username: Option<String>,
    password: Option<String>,
) -> Option<NtfyAuth> {
    if let Some(t) = token.filter(|s| !s.is_empty()) {
        return Some(NtfyAuth::Bearer(t));
    }
    match (
        username.filter(|s| !s.is_empty()),
        password.filter(|s| !s.is_empty()),
    ) {
        (Some(u), Some(p)) => Some(NtfyAuth::Basic {
            username: u,
            password: p,
        }),
        _ => None,
    }
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
    //       token: my-topic                 # ntfy topic (not the auth token)
    //       token_auth: tk_...              # optional bearer for private servers
    //       username: alice                 # or basic auth
    //       password: hunter2
    //
    // For backwards compatibility we also accept `notifiers.ntfy.{url,topic,
    // token_auth,username,password}`.
    let mut ntfy_topic: Option<String> = None;
    let mut ntfy_base: Option<String> = None;
    let mut ntfy_token_auth: Option<String> = None;
    let mut ntfy_user: Option<String> = None;
    let mut ntfy_pass: Option<String> = None;

    if let Some(openclaw) = config.notifiers.get("openclaw") {
        ntfy_topic = notifier_extra_string(openclaw, "token");
        ntfy_base = notifier_extra_string(openclaw, "url");
        ntfy_token_auth = notifier_extra_string(openclaw, "token_auth");
        ntfy_user = notifier_extra_string(openclaw, "username");
        ntfy_pass = notifier_extra_string(openclaw, "password");
    }
    if ntfy_topic.is_none() {
        if let Some(ntfy) = config.notifiers.get("ntfy") {
            ntfy_topic = notifier_extra_string(ntfy, "topic")
                .or_else(|| notifier_extra_string(ntfy, "token"));
            ntfy_base = ntfy_base.or_else(|| notifier_extra_string(ntfy, "url"));
            ntfy_token_auth = ntfy_token_auth.or_else(|| notifier_extra_string(ntfy, "token_auth"));
            ntfy_user = ntfy_user.or_else(|| notifier_extra_string(ntfy, "username"));
            ntfy_pass = ntfy_pass.or_else(|| notifier_extra_string(ntfy, "password"));
        }
    }

    if ntfy_topic.is_none() {
        ntfy_topic = env_non_empty("AO_NTFY_TOPIC");
    }
    if ntfy_base.is_none() {
        ntfy_base = env_non_empty("AO_NTFY_URL");
    }
    if ntfy_token_auth.is_none() {
        ntfy_token_auth = env_non_empty("AO_NTFY_TOKEN");
    }
    if ntfy_user.is_none() {
        ntfy_user = env_non_empty("AO_NTFY_USERNAME");
    }
    if ntfy_pass.is_none() {
        ntfy_pass = env_non_empty("AO_NTFY_PASSWORD");
    }

    if let Some(topic) = ntfy_topic.filter(|s| !s.is_empty()) {
        let base = ntfy_base
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://ntfy.sh".to_string());
        let auth = resolve_ntfy_auth(ntfy_token_auth, ntfy_user, ntfy_pass);
        notifier_registry.register(
            "ntfy",
            Arc::new(NtfyNotifier::with_base_url_and_auth(topic, base, auth)),
        );
    }

    notifier_registry.register("desktop", Arc::new(DesktopNotifier::new()));

    // Discord: prefer config `notifiers.discord.webhookUrl`, fall back to env var.
    //
    // Config format:
    //   notifiers:
    //     discord:
    //       webhookUrl: https://discord.com/api/webhooks/...
    let discord_url = config
        .notifiers
        .get("discord")
        .and_then(|c| {
            notifier_extra_string(c, "webhookUrl")
                .or_else(|| notifier_extra_string(c, "webhook_url"))
        })
        .or_else(|| env_non_empty("AO_DISCORD_WEBHOOK_URL"));
    if let Some(url) = discord_url {
        notifier_registry.register("discord", Arc::new(DiscordNotifier::new(url)));
    }

    // Slack: prefer config `notifiers.slack.webhookUrl`, fall back to env var.
    //
    // Config format:
    //   notifiers:
    //     slack:
    //       webhookUrl: https://hooks.slack.com/services/...
    let slack_url = config
        .notifiers
        .get("slack")
        .and_then(|c| {
            notifier_extra_string(c, "webhookUrl")
                .or_else(|| notifier_extra_string(c, "webhook_url"))
        })
        .or_else(|| env_non_empty("AO_SLACK_WEBHOOK_URL"));
    if let Some(url) = slack_url {
        notifier_registry.register("slack", Arc::new(SlackNotifier::new(url)));
    }

    notifier_registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ntfy_auth_none_when_all_empty() {
        assert!(resolve_ntfy_auth(None, None, None).is_none());
        assert!(resolve_ntfy_auth(Some(String::new()), None, None).is_none());
    }

    #[test]
    fn resolve_ntfy_auth_prefers_bearer_over_basic() {
        let out = resolve_ntfy_auth(
            Some("tk_abc".into()),
            Some("alice".into()),
            Some("pw".into()),
        )
        .expect("should resolve auth");
        match out {
            NtfyAuth::Bearer(t) => assert_eq!(t, "tk_abc"),
            other => panic!("expected Bearer, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ntfy_auth_builds_basic_when_both_user_and_pass_present() {
        let out = resolve_ntfy_auth(None, Some("alice".into()), Some("pw".into()))
            .expect("should resolve auth");
        match out {
            NtfyAuth::Basic { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "pw");
            }
            other => panic!("expected Basic, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ntfy_auth_requires_both_user_and_pass_for_basic() {
        assert!(resolve_ntfy_auth(None, Some("alice".into()), None).is_none());
        assert!(resolve_ntfy_auth(None, None, Some("pw".into())).is_none());
        assert!(resolve_ntfy_auth(None, Some("alice".into()), Some(String::new())).is_none());
    }
}
