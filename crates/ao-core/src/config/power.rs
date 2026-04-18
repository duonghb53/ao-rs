//! Power-related config types: `PowerConfig`, `ScmWebhookConfig`,
//! `PluginConfig`, `DefaultsConfig`, and `RoleAgentConfig`.

use super::agent::{default_orchestrator_rules, AgentConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub(super) fn default_runtime() -> String {
    "tmux".into()
}
pub(super) fn default_agent() -> String {
    "claude-code".into()
}
pub(super) fn default_workspace() -> String {
    "worktree".into()
}
pub(super) fn default_tracker() -> String {
    "github".into()
}

fn default_true() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

fn default_prevent_idle_sleep() -> bool {
    cfg!(target_os = "macos")
}

/// SCM webhook configuration (TS: `SCMWebhookConfig`).
///
/// `Default` sets `enabled = true`, matching the serde default and TS
/// behaviour (`enabled: webhook?.enabled !== false`). A zero-value
/// `Default` would silently disable webhooks for anyone constructing
/// this struct in Rust, which is the opposite of what the YAML path does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScmWebhookConfig {
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "secretEnvVar",
        alias = "secret_env_var"
    )]
    pub secret_env_var: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "signatureHeader",
        alias = "signature_header"
    )]
    pub signature_header: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "eventHeader",
        alias = "event_header"
    )]
    pub event_header: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "deliveryHeader",
        alias = "delivery_header"
    )]
    pub delivery_header: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "maxBodyBytes",
        alias = "max_body_bytes"
    )]
    pub max_body_bytes: Option<u64>,
}

impl Default for ScmWebhookConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            secret_env_var: None,
            signature_header: None,
            event_header: None,
            delivery_header: None,
            max_body_bytes: None,
        }
    }
}

/// Shared plugin config shape (tracker/scm/notifier). Allows arbitrary extra keys.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// SCM-only: webhook configuration (TS: `scm.webhook`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<ScmWebhookConfig>,
    #[serde(flatten, default)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

/// Power management settings (TS: `power.preventIdleSleep`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerConfig {
    #[serde(
        default = "default_prevent_idle_sleep",
        rename = "preventIdleSleep",
        alias = "prevent_idle_sleep"
    )]
    pub prevent_idle_sleep: bool,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            prevent_idle_sleep: cfg!(target_os = "macos"),
        }
    }
}

/// Per-role agent config (TS `orchestrator` / `worker` blocks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleAgentConfig {
    /// Override the agent plugin for this role (e.g. "claude-code", "codex", ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    /// Role-specific agent config overrides.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "agent_config",
        alias = "agentConfig"
    )]
    pub agent_config: Option<AgentConfig>,
}

/// Orchestrator-wide defaults for plugin selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultsConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    #[serde(default = "default_agent")]
    pub agent: String,
    /// Role defaults (TS: `defaults.orchestrator`, `defaults.worker`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<RoleAgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<RoleAgentConfig>,
    /// Default system rules for the orchestrator session (TS: `defaults.orchestratorRules`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "orchestrator_rules",
        alias = "orchestratorRules",
        alias = "orchestrator-rules"
    )]
    pub orchestrator_rules: Option<String>,
    #[serde(default = "default_workspace")]
    pub workspace: String,
    #[serde(default = "default_tracker")]
    pub tracker: String,
    /// Optional branch namespace/prefix for agent-created worktree branches.
    ///
    /// If set, `ao-rs spawn` will create branches like:
    /// - `<branch_namespace>/<short_id>` (task-first)
    /// - `<branch_namespace>/<short_id>/<issue_branch>` (issue-first)
    ///
    /// Example: `ao/agent/5c452025/feat-issue-30`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "branch_namespace",
        alias = "branchNamespace",
        alias = "branch-namespace"
    )]
    pub branch_namespace: Option<String>,
    #[serde(default)]
    pub notifiers: Vec<String>,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            runtime: default_runtime(),
            agent: default_agent(),
            orchestrator: None,
            worker: None,
            orchestrator_rules: Some(default_orchestrator_rules().to_string()),
            workspace: default_workspace(),
            tracker: default_tracker(),
            branch_namespace: None,
            notifiers: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_config_roundtrip() {
        let dc = DefaultsConfig::default();
        assert_eq!(dc.runtime, "tmux");
        assert_eq!(dc.agent, "claude-code");
        assert_eq!(dc.workspace, "worktree");
        assert_eq!(dc.tracker, "github");
        assert!(dc.notifiers.is_empty());

        let yaml = serde_yaml::to_string(&dc).unwrap();
        let dc2: DefaultsConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(dc, dc2);
    }

    #[test]
    fn power_config_default_is_platform_aware() {
        let pc = PowerConfig::default();
        if cfg!(target_os = "macos") {
            assert!(
                pc.prevent_idle_sleep,
                "macOS: prevent_idle_sleep should default to true"
            );
        } else {
            assert!(
                !pc.prevent_idle_sleep,
                "non-macOS: prevent_idle_sleep should default to false"
            );
        }
    }
}
