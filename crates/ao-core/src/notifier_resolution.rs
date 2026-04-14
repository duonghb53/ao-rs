use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifierConfig {
    pub plugin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNotifierTarget {
    pub reference: String,
    pub plugin_name: String,
}

pub fn resolve_notifier_target(
    notifiers: Option<&HashMap<String, NotifierConfig>>,
    reference: &str,
) -> ResolvedNotifierTarget {
    let plugin_name = notifiers
        .and_then(|m| m.get(reference))
        .map(|c| c.plugin.clone())
        .unwrap_or_else(|| reference.to_string());

    ResolvedNotifierTarget {
        reference: reference.to_string(),
        plugin_name,
    }
}

