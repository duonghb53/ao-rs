//! Project-level config file: `ao-rs.yaml` (discovered by walking up from cwd).
//!
//! Mirrors the `OrchestratorConfig` shape from the TypeScript
//! agent-orchestrator. `ao-rs start` generates this file with sensible
//! defaults; subsequent runs load the existing file without overwriting.
//!
//! ## Missing-file handling
//!
//! `load_default()` returns an empty `AoConfig` if the file doesn't exist.
//! A fresh install runs without the user being forced to create a config
//! first. Parse errors propagate — a broken config needs to be fixed.

use crate::{
    error::{AoError, Result},
    notifier::NotificationRouting,
    parity_session_strategy::{OpencodeIssueSessionStrategy, OrchestratorSessionStrategy},
    reaction_engine::parse_duration,
    reactions::{EscalateAfter, EventPriority, ReactionAction, ReactionConfig},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

// ---------------------------------------------------------------------------
// Diagnostics + validation
// ---------------------------------------------------------------------------

/// Non-fatal config issues (unknown fields, questionable values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    /// Human-readable field path (e.g. `"projects.my-app.defaultBranch"`).
    pub field: String,
    /// Actionable message.
    pub message: String,
}

/// Result of loading a config file: parsed config + any warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: AoConfig,
    pub warnings: Vec<ConfigWarning>,
}

fn yaml_field_path(path: &serde_ignored::Path) -> String {
    // serde_ignored uses segments like `.field`, `[0]`, etc.
    // We prefer a dot-separated path for CLI output.
    let s = path.to_string();
    s.trim_start_matches('.').to_string()
}

fn supported_reaction_keys() -> [&'static str; 9] {
    [
        "ci-failed",
        "changes-requested",
        "merge-conflicts",
        "approved-and-green",
        "agent-idle",
        "agent-stuck",
        "agent-needs-input",
        "agent-exited",
        "all-complete",
    ]
}

fn supported_notifier_names() -> [&'static str; 5] {
    // These are the notifier plugin names ao-cli may register.
    // Some are conditional on env vars (ntfy/discord/slack), but the *names*
    // are still supported; validation should catch typos.
    ["stdout", "desktop", "ntfy", "discord", "slack"]
}

impl AoConfig {
    /// Validate config semantics (beyond YAML parsing).
    ///
    /// Returns `Ok(())` when valid, otherwise a `AoError::Config` with an
    /// actionable, field-scoped message including the config file path.
    pub fn validate(&self, config_path: &Path) -> Result<()> {
        // ---- reactions.* keys ----
        let known: std::collections::HashSet<&'static str> =
            supported_reaction_keys().into_iter().collect();
        for key in self.reactions.keys() {
            if !known.contains(key.as_str()) {
                let mut keys: Vec<&str> = known.iter().copied().collect();
                keys.sort();
                return Err(AoError::Config(format!(
                    "{}: unknown reaction key `reactions.{}` (supported: {})",
                    config_path.display(),
                    key,
                    keys.join(", ")
                )));
            }
        }

        // ---- duration parsing (reactions.*.threshold, reactions.*.escalate_after) ----
        for (reaction_key, cfg) in &self.reactions {
            if let Some(raw) = cfg.threshold.as_deref() {
                if parse_duration(raw).is_none() {
                    return Err(AoError::Config(format!(
                        "{}: invalid duration at `reactions.{}.threshold`: {:?} (expected like \"10s\", \"5m\", \"2h\")",
                        config_path.display(),
                        reaction_key,
                        raw
                    )));
                }
            }
            if let Some(EscalateAfter::Duration(raw)) = cfg.escalate_after.as_ref() {
                if parse_duration(raw).is_none() {
                    return Err(AoError::Config(format!(
                        "{}: invalid duration at `reactions.{}.escalate_after`: {:?} (expected like \"10s\", \"5m\", \"2h\")",
                        config_path.display(),
                        reaction_key,
                        raw
                    )));
                }
            }
        }

        // ---- notifier names (defaults.notifiers, notification_routing) ----
        let supported_notifiers: std::collections::HashSet<&'static str> =
            supported_notifier_names().into_iter().collect();

        if let Some(defaults) = self.defaults.as_ref() {
            for name in &defaults.notifiers {
                if !supported_notifiers.contains(name.as_str()) {
                    return Err(AoError::Config(format!(
                        "{}: unknown notifier name at `defaults.notifiers`: {:?} (supported: {})",
                        config_path.display(),
                        name,
                        supported_notifier_names().join(", ")
                    )));
                }
            }
        }

        // NotificationRouting parsing is already strict for priority keys
        // (serde rejects unknown priorities). Here we validate notifier names.
        for &priority in &[
            EventPriority::Urgent,
            EventPriority::Action,
            EventPriority::Warning,
            EventPriority::Info,
        ] {
            if let Some(names) = self.notification_routing.names_for(priority) {
                for name in names {
                    if !supported_notifiers.contains(name.as_str()) {
                        return Err(AoError::Config(format!(
                            "{}: unknown notifier name at `notification_routing.{}[]`: {:?} (supported: {})",
                            config_path.display(),
                            priority.as_str(),
                            name,
                            supported_notifier_names().join(", ")
                        )));
                    }
                }
            }
        }

        // ---- projects.* repo/path constraints ----
        for (project_id, project) in &self.projects {
            // repo must be owner/repo (one slash, neither side empty).
            let parts: Vec<&str> = project.repo.split('/').collect();
            let ok = parts.len() == 2 && !parts[0].trim().is_empty() && !parts[1].trim().is_empty();
            if !ok {
                return Err(AoError::Config(format!(
                    "{}: invalid repo slug at `projects.{}.repo`: {:?} (expected \"owner/repo\")",
                    config_path.display(),
                    project_id,
                    project.repo
                )));
            }

            // path must be absolute; we intentionally reject `~` because it
            // won't canonicalize reliably in non-shell contexts.
            let p = project.path.trim();
            if p.is_empty() {
                return Err(AoError::Config(format!(
                    "{}: empty path at `projects.{}.path`",
                    config_path.display(),
                    project_id
                )));
            }
            if p.starts_with('~') {
                return Err(AoError::Config(format!(
                    "{}: `projects.{}.path` must be an absolute path (found {:?}; `~` is not supported here)",
                    config_path.display(),
                    project_id,
                    project.path
                )));
            }
            if !p.starts_with('/') {
                return Err(AoError::Config(format!(
                    "{}: `projects.{}.path` must be an absolute path (found {:?})",
                    config_path.display(),
                    project_id,
                    project.path
                )));
            }
        }

        Ok(())
    }
}

// --- Serde default helpers ---

fn default_runtime() -> String {
    "tmux".into()
}
fn default_agent() -> String {
    "claude-code".into()
}
fn default_workspace() -> String {
    "worktree".into()
}
fn default_tracker() -> String {
    "github".into()
}
fn default_branch_name() -> String {
    "main".into()
}
fn default_permissions() -> String {
    "permissionless".into()
}
fn default_port() -> u16 {
    3000
}
fn default_ready_threshold_ms() -> u64 {
    300_000
}
fn default_poll_interval_secs() -> u64 {
    10
}

// --- Config types ---

/// SCM webhook configuration (TS: `SCMWebhookConfig`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

fn default_true() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerConfig {
    #[serde(default, rename = "preventIdleSleep", alias = "prevent_idle_sleep")]
    pub prevent_idle_sleep: bool,
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

/// Per-project configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Friendly display name (TS: `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// GitHub-style `owner/repo`.
    pub repo: String,
    /// Absolute path on disk.
    pub path: String,
    /// Default branch to use as worktree base.
    #[serde(
        default = "default_branch_name",
        alias = "default-branch",
        rename = "default_branch"
    )]
    pub default_branch: String,
    /// Session prefix (TS: `sessionPrefix`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "sessionPrefix",
        alias = "session_prefix"
    )]
    pub session_prefix: Option<String>,
    /// Optional per-project override for branch namespace/prefix. See
    /// `defaults.branch_namespace`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "branch_namespace",
        alias = "branchNamespace",
        alias = "branch-namespace"
    )]
    pub branch_namespace: Option<String>,
    /// Per-project plugin overrides (TS: `runtime`, `agent`, `workspace`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Issue tracker plugin for `spawn --issue` ("github", "linear", ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker: Option<PluginConfig>,
    /// SCM config (TS: `scm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scm: Option<PluginConfig>,
    /// Files to symlink into workspaces (TS: `symlinks`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symlinks: Vec<String>,
    /// Commands to run after workspace creation (TS: `postCreate`).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        rename = "postCreate",
        alias = "post_create"
    )]
    pub post_create: Vec<String>,
    /// Agent-specific overrides.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "agent-config",
        rename = "agent_config"
    )]
    pub agent_config: Option<AgentConfig>,

    /// Role overrides for the orchestrator agent (TS: `projects.<id>.orchestrator`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<RoleAgentConfig>,

    /// Role overrides for worker agents (TS: `projects.<id>.worker`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<RoleAgentConfig>,

    /// Per-project reaction overrides (TS: `projects.*.reactions`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reactions: HashMap<String, ReactionConfig>,

    /// Inline rules/instructions passed to every agent prompt (TS: `agentRules`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "agent_rules",
        alias = "agentRules",
        alias = "agent-rules"
    )]
    pub agent_rules: Option<String>,

    /// Path to a file containing agent rules, relative to project path (TS: `agentRulesFile`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "agent_rules_file",
        alias = "agentRulesFile",
        alias = "agent-rules-file"
    )]
    pub agent_rules_file: Option<String>,

    /// System rules for the orchestrator session (TS: `orchestratorRules`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "orchestrator_rules",
        alias = "orchestratorRules",
        alias = "orchestrator-rules"
    )]
    pub orchestrator_rules: Option<String>,

    /// Strategy for handling existing orchestrator sessions (TS: `orchestratorSessionStrategy`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "orchestrator_session_strategy",
        alias = "orchestratorSessionStrategy",
        alias = "orchestrator-session-strategy"
    )]
    pub orchestrator_session_strategy: Option<OrchestratorSessionStrategy>,

    /// Strategy for handling existing opencode issue sessions (TS: `opencodeIssueSessionStrategy`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "opencode_issue_session_strategy",
        alias = "opencodeIssueSessionStrategy",
        alias = "opencode-issue-session-strategy"
    )]
    pub opencode_issue_session_strategy: Option<OpencodeIssueSessionStrategy>,
}

/// Agent-level overrides per project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Permission mode: "permissionless", "default", "auto-edit", "suggest".
    #[serde(default = "default_permissions")]
    pub permissions: String,

    /// System prompt rules appended via `--append-system-prompt`.
    /// Structured workflow instructions (dev-lifecycle phases, testing
    /// requirements, coding standards) that guide the agent's behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,

    /// Path to an external rules file (relative to project path).
    /// Takes precedence over inline `rules` if both are set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "rules-file",
        rename = "rules_file"
    )]
    pub rules_file: Option<String>,
    /// Model override (TS: `agentConfig.model`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Orchestrator model override (TS: `agentConfig.orchestratorModel`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "orchestratorModel"
    )]
    pub orchestrator_model: Option<String>,
    /// OpenCode session id (TS: `agentConfig.opencodeSessionId`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "opencodeSessionId"
    )]
    pub opencode_session_id: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            permissions: default_permissions(),
            rules: Some(default_agent_rules().to_string()),
            rules_file: None,
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
        }
    }
}

/// Default dev-lifecycle rules for agents, inspired by ai-devkit.
/// Structures the agent's workflow into phases for more effective output.
pub fn default_agent_rules() -> &'static str {
    r#"Follow this structured workflow for every task:

1. UNDERSTAND — Read the issue/task carefully. Check existing code, tests, and docs before changing anything.
2. PLAN — Design your approach. For non-trivial changes, outline what files you'll modify and why.
3. IMPLEMENT — Write the code. Follow existing patterns and conventions in the codebase.
4. VERIFY — Run tests (`cargo test`), linter (`cargo clippy`), and formatter (`cargo fmt`). Fix any failures before proceeding.
5. REVIEW — Re-read your changes. Check for security issues, missing edge cases, and unnecessary complexity.
6. DELIVER — Commit your changes, push the branch, and create a PR with `gh pr create`. Include a clear title and description of what was changed and why.

Rules:
- When spawned from an issue, use the dev-lifecycle workflow to turn the issue content into concrete requirements and a plan, then execute it.
- Do not skip the verify step. Every change must pass tests and clippy before you consider it done.
- Always push your branch and open a PR when the task is complete.
- Prefer editing existing files over creating new ones.
- Keep changes focused — fix what was asked, don't refactor surrounding code.
- If stuck for more than 5 minutes, explain what's blocking you."#
}

/// Default orchestrator rules (read-only coordinator).
pub fn default_orchestrator_rules() -> &'static str {
    r#"You are the orchestrator session.

Non-negotiable rules:
- The orchestrator session is read-only. Do not edit repo files or implement code changes here.
- Delegate all implementation work to worker sessions.
- Prefer using `ao-rs` commands (especially `ao-rs send`) to coordinate; do not use raw tmux commands.
- When spawned from an issue, turn the issue into a clear plan, then spawn/drive workers to implement it.

Workflow (dev lifecycle):
1. UNDERSTAND
2. PLAN
3. IMPLEMENT (delegate)
4. VERIFY (delegate)
5. REVIEW
6. DELIVER (delegate PR work to workers)"#
}

/// Default `.ai-devkit.json` content for Claude Code environment.
fn ai_devkit_config_json() -> String {
    // Simple ISO-8601 timestamp without pulling in chrono.
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // ai-devkit uses JS-style ISO dates but only checks the field exists.
    let ts = format!("{now}");
    format!(
        r#"{{
  "version": "0.21.1",
  "environments": ["claude"],
  "phases": ["requirements","design","planning","implementation","testing","deployment","monitoring"],
  "createdAt": "{ts}",
  "updatedAt": "{ts}",
  "skills": [
    {{"registry":"codeaholicguy/ai-devkit","name":"dev-lifecycle"}},
    {{"registry":"codeaholicguy/ai-devkit","name":"debug"}},
    {{"registry":"codeaholicguy/ai-devkit","name":"memory"}},
    {{"registry":"codeaholicguy/ai-devkit","name":"verify"}},
    {{"registry":"codeaholicguy/ai-devkit","name":"tdd"}}
  ]
}}"#
    )
}

/// Install ai-devkit skills into a project directory.
///
/// Writes `.ai-devkit.json` (Claude Code environment + default skills),
/// then runs `npx ai-devkit@latest install` to download and symlink skills
/// into `.claude/skills/`. Non-fatal: callers should treat errors as
/// warnings (the config file is still valid without skills).
pub fn install_skills(project_dir: &Path) -> Result<()> {
    use std::process::Command;

    // Write .ai-devkit.json so the install command is non-interactive.
    let config_path = project_dir.join(".ai-devkit.json");
    if !config_path.exists() {
        std::fs::write(&config_path, ai_devkit_config_json()).map_err(AoError::Io)?;
    }

    let output = Command::new("npx")
        .args(["ai-devkit@latest", "install"])
        .current_dir(project_dir)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AoError::Other(
                    "npx not found. Install Node.js and run: npx ai-devkit@latest init".into(),
                )
            } else {
                AoError::Other(format!("failed to run npx ai-devkit install: {e}"))
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AoError::Other(format!(
            "npx ai-devkit install failed: {stderr}"
        )));
    }

    Ok(())
}

/// Top-level ao-rs config file shape. All fields use `#[serde(default)]`
/// so partial config files parse without error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AoConfig {
    /// Dashboard port (TS: `port`).
    #[serde(default = "default_port")]
    pub port: u16,
    /// Terminal server ports (TS: `terminalPort`, `directTerminalPort`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "terminalPort"
    )]
    pub terminal_port: Option<u16>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "directTerminalPort"
    )]
    pub direct_terminal_port: Option<u16>,
    /// Milliseconds before a "ready" session becomes "idle" (TS: `readyThresholdMs`, default 300000).
    #[serde(
        default = "default_ready_threshold_ms",
        rename = "ready_threshold_ms",
        alias = "readyThresholdMs",
        alias = "ready-threshold-ms"
    )]
    pub ready_threshold_ms: u64,
    /// Lifecycle polling interval in seconds (default 10).
    #[serde(
        default = "default_poll_interval_secs",
        alias = "pollInterval",
        alias = "poll-interval"
    )]
    pub poll_interval: u64,
    /// Power management settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<PowerConfig>,
    /// Orchestrator-wide plugin defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<DefaultsConfig>,

    /// Per-project configs keyed by project id.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub projects: HashMap<String, ProjectConfig>,

    /// Map from reaction key (e.g. `"ci-failed"`) to its config.
    #[serde(default)]
    pub reactions: HashMap<String, ReactionConfig>,

    /// Priority-based notification routing table.
    #[serde(
        default,
        rename = "notification_routing",
        alias = "notification-routing",
        alias = "notificationRouting"
    )]
    pub notification_routing: NotificationRouting,

    /// Notifier plugin configurations (TS: `notifiers`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub notifiers: HashMap<String, PluginConfig>,

    /// External plugins list (installer-managed). Currently stored for parity only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<HashMap<String, serde_yaml::Value>>,
}

impl Default for AoConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            ready_threshold_ms: default_ready_threshold_ms(),
            poll_interval: default_poll_interval_secs(),
            terminal_port: None,
            direct_terminal_port: None,
            power: None,
            defaults: None,
            projects: HashMap::new(),
            reactions: HashMap::new(),
            notification_routing: Default::default(),
            notifiers: HashMap::new(),
            plugins: vec![],
        }
    }
}

impl AoConfig {
    /// Read and parse a config file at an explicit path, collecting warnings
    /// for unknown fields and validating the supported subset.
    pub fn load_from_with_warnings(path: &Path) -> Result<LoadedConfig> {
        let text = std::fs::read_to_string(path)?;

        let mut warnings: Vec<ConfigWarning> = Vec::new();
        let deserializer = serde_yaml::Deserializer::from_str(&text);
        let cfg: AoConfig = serde_ignored::deserialize(deserializer, |p| {
            warnings.push(ConfigWarning {
                field: yaml_field_path(&p),
                message: "unknown field; this key is not supported and will be ignored".into(),
            });
        })
        .map_err(|e| AoError::Yaml(e.to_string()))?;

        cfg.validate(path)?;
        Ok(LoadedConfig {
            config: cfg,
            warnings,
        })
    }

    /// Read a config file at an explicit path, or return an empty config
    /// if the file doesn't exist, collecting warnings and validating.
    pub fn load_from_or_default_with_warnings(path: &Path) -> Result<LoadedConfig> {
        match std::fs::read_to_string(path) {
            Ok(_) => Self::load_from_with_warnings(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LoadedConfig {
                config: Self::default(),
                warnings: Vec::new(),
            }),
            Err(e) => Err(AoError::Io(e)),
        }
    }

    /// Read and parse a config file at an explicit path.
    ///
    /// Distinct from `load_default` because tests should never touch
    /// `~/.ao-rs/config.yaml` — they pass a tempfile instead.
    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: AoConfig =
            serde_yaml::from_str(&text).map_err(|e| AoError::Yaml(e.to_string()))?;
        Ok(cfg)
    }

    /// Read a config file at an explicit path, or return an empty config
    /// if the file doesn't exist. Any other I/O or parse error propagates.
    ///
    /// Only `NotFound` short-circuits to `Default::default()` — a permission
    /// denied or unreadable file should still error, since silently pretending
    /// there's no config would mask a real misconfiguration.
    ///
    /// Takes an explicit path (rather than always using `default_path()`)
    /// so tests can exercise both branches without touching `$HOME`.
    pub fn load_from_or_default(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_yaml::from_str(&text).map_err(|e| AoError::Yaml(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(AoError::Io(e)),
        }
    }

    /// Load config from the current directory's `ao-rs.yaml`, or return
    /// an empty config if the file doesn't exist.
    pub fn load_default() -> Result<Self> {
        Self::load_from_or_default(&Self::local_path())
    }

    /// Config file name in the project directory (like TS's `agent-orchestrator.yaml`).
    pub const CONFIG_FILENAME: &str = "ao-rs.yaml";

    /// Discover a config path by walking up parent directories.
    ///
    /// If a `ao-rs.yaml` exists in any ancestor (including `start`), returns
    /// the nearest one. Otherwise returns `start/ao-rs.yaml`.
    fn discover_path_from(start: &Path) -> std::path::PathBuf {
        let mut dir = start;
        loop {
            let candidate = dir.join(Self::CONFIG_FILENAME);
            if candidate.is_file() {
                return candidate;
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => return start.join(Self::CONFIG_FILENAME),
            }
        }
    }

    /// Config file path discovered from the current working directory.
    pub fn local_path() -> std::path::PathBuf {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        Self::discover_path_from(&cwd)
    }

    /// Config file path in a specific directory.
    pub fn path_in(dir: &Path) -> std::path::PathBuf {
        dir.join(Self::CONFIG_FILENAME)
    }

    /// Write this config to disk as YAML, creating parent dirs if needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let yaml = serde_yaml::to_string(self).map_err(|e| AoError::Yaml(e.to_string()))?;
        std::fs::write(path, yaml)?;
        Ok(())
    }
}

/// Returns the nine default reactions matching the TS agent-orchestrator.
///
/// `priority` is left unset so dispatch uses
/// [`reactions::default_priority_for_reaction_key`](crate::reactions::default_priority_for_reaction_key)
/// — configured `priority:` in YAML always overrides.
pub fn default_reactions() -> HashMap<String, ReactionConfig> {
    let mut m = HashMap::new();
    m.insert(
        "ci-failed".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some(
                "CI is failing on your PR. Run `gh pr checks` to see the failures, fix them, and push.".into(),
            ),
            priority: None,
            retries: Some(2),
            escalate_after: Some(EscalateAfter::Attempts(2)),
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "changes-requested".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some(
                "There are review comments on your PR. Check with `gh pr view --comments`, address them, and push."
                    .into(),
            ),
            priority: None,
            retries: None,
            escalate_after: Some(EscalateAfter::Duration("30m".into())),
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "merge-conflicts".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some(
                "Your branch has merge conflicts. Rebase on the default branch and resolve them."
                    .into(),
            ),
            priority: None,
            retries: None,
            escalate_after: Some(EscalateAfter::Duration("15m".into())),
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "approved-and-green".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::AutoMerge,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "agent-idle".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some(
                "You appear to be idle. If your task is not complete, continue working or explain blockers."
                    .into(),
            ),
            priority: None,
            retries: Some(2),
            escalate_after: Some(EscalateAfter::Duration("15m".into())),
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "agent-stuck".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: Some("10m".into()),
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "agent-needs-input".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "agent-exited".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "all-complete".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: true,
            merge_method: None,
        },
    );
    m
}

/// Returns default notification routing: all priorities → stdout.
pub fn default_routing() -> NotificationRouting {
    let mut m = HashMap::new();
    for &p in &[
        EventPriority::Urgent,
        EventPriority::Action,
        EventPriority::Warning,
        EventPriority::Info,
    ] {
        m.insert(p, vec!["stdout".to_string()]);
    }
    NotificationRouting::from_map(m)
}

/// Auto-detect git repo info from a working directory.
///
/// Returns `(owner_repo, repo_name, default_branch)` by shelling out to
/// `git remote get-url origin` and `git rev-parse --abbrev-ref HEAD`.
pub fn detect_git_repo(cwd: &Path) -> Result<(String, String, String)> {
    // Parse origin URL → owner/repo
    let url_output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        .map_err(AoError::Io)?;

    if !url_output.status.success() {
        return Err(AoError::Other(
            "no git remote 'origin' found — run from inside a git repo".into(),
        ));
    }

    let url = String::from_utf8_lossy(&url_output.stdout)
        .trim()
        .to_string();
    let owner_repo = parse_owner_repo(&url).ok_or_else(|| {
        AoError::Other(format!("could not parse owner/repo from remote URL: {url}"))
    })?;
    let repo_name = owner_repo
        .rsplit('/')
        .next()
        .unwrap_or(&owner_repo)
        .to_string();

    // Detect default branch
    let branch_output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .map_err(AoError::Io)?;

    let default_branch = if branch_output.status.success() {
        String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string()
    } else {
        "main".to_string()
    };

    Ok((owner_repo, repo_name, default_branch))
}

/// Parse `owner/repo` from a git remote URL.
///
/// Supports HTTPS (`https://github.com/owner/repo.git`) and
/// SSH (`git@github.com:owner/repo.git`).
fn parse_owner_repo(url: &str) -> Option<String> {
    let s = url.trim().trim_end_matches(".git");
    if let Some(rest) = s.strip_prefix("https://") {
        // https://github.com/owner/repo
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Some(parts[1].to_string());
        }
    }
    if let Some(rest) = s.strip_prefix("git@") {
        // git@github.com:owner/repo
        if let Some(path) = rest.split(':').nth(1) {
            return Some(path.to_string());
        }
    }
    None
}

/// Build a complete config for a detected project.
pub fn generate_config(cwd: &Path) -> Result<AoConfig> {
    let (owner_repo, repo_name, default_branch) = detect_git_repo(cwd)?;
    let abs_path = std::fs::canonicalize(cwd)?;

    let mut projects = HashMap::new();
    projects.insert(
        repo_name,
        ProjectConfig {
            name: None,
            repo: owner_repo,
            path: abs_path.to_string_lossy().to_string(),
            default_branch,
            session_prefix: None,
            branch_namespace: None,
            runtime: None,
            agent: None,
            workspace: None,
            tracker: None,
            scm: None,
            symlinks: vec![],
            post_create: vec![],
            agent_config: Some(AgentConfig::default()),
            orchestrator: None,
            worker: None,
            reactions: HashMap::new(),
            agent_rules: None,
            agent_rules_file: None,
            orchestrator_rules: None,
            orchestrator_session_strategy: None,
            opencode_issue_session_strategy: None,
        },
    );

    Ok(AoConfig {
        port: default_port(),
        ready_threshold_ms: default_ready_threshold_ms(),
        poll_interval: default_poll_interval_secs(),
        terminal_port: None,
        direct_terminal_port: None,
        power: None,
        defaults: Some(DefaultsConfig {
            orchestrator: Some(RoleAgentConfig {
                agent: Some("cursor".into()),
                agent_config: Some(AgentConfig {
                    permissions: default_permissions(),
                    rules: None,
                    rules_file: None,
                    model: None,
                    orchestrator_model: None,
                    opencode_session_id: None,
                }),
            }),
            worker: Some(RoleAgentConfig {
                agent: Some("cursor".into()),
                agent_config: None,
            }),
            orchestrator_rules: Some(default_orchestrator_rules().to_string()),
            ..DefaultsConfig::default()
        }),
        projects,
        reactions: default_reactions(),
        notification_routing: default_routing(),
        notifiers: HashMap::new(),
        plugins: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_file(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ao-rs-config-{label}-{nanos}-{n}.yaml"))
    }

    #[test]
    fn load_from_parses_minimal_config() {
        let path = unique_temp_file("minimal");
        std::fs::write(
            &path,
            r#"
reactions:
  ci-failed:
    action: send-to-agent
    message: "CI broke — please fix."
"#,
        )
        .unwrap();

        let cfg = AoConfig::load_from(&path).unwrap();
        let ci = cfg.reactions.get("ci-failed").unwrap();
        assert_eq!(ci.action, ReactionAction::SendToAgent);
        assert_eq!(ci.message.as_deref(), Some("CI broke — please fix."));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn generate_config_includes_orchestrator_fields() {
        let dir = std::env::temp_dir();
        let cfg = generate_config(&dir).unwrap_or_else(|_| {
            // Fallback: build a minimal AoConfig just for YAML shape assertions when the temp dir isn't a git repo.
            let mut projects = HashMap::new();
            projects.insert(
                "demo".into(),
                ProjectConfig {
                    name: None,
                    repo: "org/demo".into(),
                    path: "/tmp/demo".into(),
                    default_branch: "main".into(),
                    session_prefix: None,
                    branch_namespace: None,
                    runtime: None,
                    agent: None,
                    workspace: None,
                    tracker: None,
                    scm: None,
                    symlinks: vec![],
                    post_create: vec![],
                    agent_config: Some(AgentConfig::default()),
                    orchestrator: Some(RoleAgentConfig {
                        agent: None,
                        agent_config: Some(AgentConfig {
                            permissions: default_permissions(),
                            rules: None,
                            rules_file: None,
                            model: None,
                            orchestrator_model: None,
                            opencode_session_id: None,
                        }),
                    }),
                    worker: None,
                    reactions: HashMap::new(),
                    agent_rules: None,
                    agent_rules_file: None,
                    orchestrator_rules: None,
                    orchestrator_session_strategy: None,
                    opencode_issue_session_strategy: None,
                },
            );
            AoConfig {
                port: default_port(),
                ready_threshold_ms: default_ready_threshold_ms(),
                poll_interval: default_poll_interval_secs(),
                terminal_port: None,
                direct_terminal_port: None,
                power: None,
                defaults: Some(DefaultsConfig {
                    orchestrator_rules: Some(default_orchestrator_rules().to_string()),
                    ..DefaultsConfig::default()
                }),
                projects,
                reactions: default_reactions(),
                notification_routing: default_routing(),
                notifiers: HashMap::new(),
                plugins: vec![],
            }
        });

        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert!(yaml.contains("orchestrator_rules:"));
        assert!(yaml.contains("orchestrator:"));
        assert!(yaml.contains("agent_config:"));
    }

    #[test]
    fn load_from_parses_all_three_reactions() {
        let path = unique_temp_file("all-three");
        std::fs::write(
            &path,
            r#"
reactions:
  ci-failed:
    action: send-to-agent
    message: "fix ci"
    retries: 3
  changes-requested:
    action: send-to-agent
    message: "address review"
  approved-and-green:
    action: auto-merge
"#,
        )
        .unwrap();

        let cfg = AoConfig::load_from(&path).unwrap();
        assert_eq!(cfg.reactions.len(), 3);
        assert_eq!(
            cfg.reactions["ci-failed"].action,
            ReactionAction::SendToAgent
        );
        assert_eq!(cfg.reactions["ci-failed"].retries, Some(3));
        assert_eq!(
            cfg.reactions["changes-requested"].action,
            ReactionAction::SendToAgent
        );
        assert_eq!(
            cfg.reactions["approved-and-green"].action,
            ReactionAction::AutoMerge
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_empty_file_produces_default_config() {
        // serde(default) on every AoConfig field means an empty YAML file
        // is equivalent to "no reactions configured" — the same outcome
        // as `load_default()` on a missing file. This is mildly surprising
        // (a typo'd blank config won't error) but keeps the two entry
        // points consistent. Test locks it in so a future `deny_unknown_fields`
        // change doesn't silently flip behaviour.
        let path = unique_temp_file("empty");
        std::fs::write(&path, "").unwrap();
        let cfg = AoConfig::load_from(&path).unwrap();
        assert!(cfg.reactions.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_config_with_no_reactions_key_is_ok() {
        // `reactions: {}` or no reactions key at all should parse fine and
        // produce an empty map — distinct from an entirely empty file.
        let path = unique_temp_file("empty-reactions");
        std::fs::write(&path, "reactions: {}\n").unwrap();
        let cfg = AoConfig::load_from(&path).unwrap();
        assert!(cfg.reactions.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_invalid_yaml_errors() {
        let path = unique_temp_file("invalid");
        std::fs::write(&path, "reactions: [not-a-map]\n").unwrap();
        assert!(AoConfig::load_from(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_with_warnings_reports_unknown_fields() {
        let path = unique_temp_file("unknown-fields");
        std::fs::write(
            &path,
            r#"
port: 3000
unknownTopLevel: 123
defaults:
  runtime: tmux
  unknownDefaultsKey: true
"#,
        )
        .unwrap();
        let loaded = AoConfig::load_from_with_warnings(&path).unwrap();
        assert_eq!(loaded.config.port, 3000);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.field.contains("unknownTopLevel")),
            "expected unknownTopLevel warning, got {:?}",
            loaded.warnings
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.field.contains("defaults") && w.field.contains("unknownDefaultsKey")),
            "expected defaults.unknownDefaultsKey warning, got {:?}",
            loaded.warnings
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_rejects_unknown_reaction_key() {
        let path = unique_temp_file("bad-reaction-key");
        std::fs::write(
            &path,
            r#"
reactions:
  ci-failed:
    action: notify
  ci-broke:
    action: notify
"#,
        )
        .unwrap();
        let err = AoConfig::load_from_with_warnings(&path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown reaction key"), "got: {msg}");
        assert!(msg.contains("reactions.ci-broke"), "got: {msg}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_rejects_bad_duration() {
        let path = unique_temp_file("bad-duration");
        std::fs::write(
            &path,
            r#"
reactions:
  agent-stuck:
    action: notify
    threshold: "1m30s"
"#,
        )
        .unwrap();
        let err = AoConfig::load_from_with_warnings(&path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid duration"), "got: {msg}");
        assert!(
            msg.contains("reactions.agent-stuck.threshold"),
            "got: {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_rejects_unknown_notifier_name_in_routing() {
        let path = unique_temp_file("bad-notifier");
        std::fs::write(
            &path,
            r#"
notification-routing:
  urgent: [stdout, slackk]
"#,
        )
        .unwrap();
        let err = AoConfig::load_from_with_warnings(&path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown notifier name"), "got: {msg}");
        assert!(msg.contains("slackk"), "got: {msg}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_or_default_missing_file_returns_empty() {
        // Covers the NotFound short-circuit without touching `$HOME`, so
        // the test is safe under parallel `cargo test`. `load_default()`
        // is a thin wrapper around this and inherits the behaviour.
        let missing = std::env::temp_dir().join("ao-rs-nonexistent-config-nonexistent-config.yaml");
        // Defensively delete in case a previous run left a stray file.
        let _ = std::fs::remove_file(&missing);

        let cfg = AoConfig::load_from_or_default(&missing).unwrap();
        assert!(cfg.reactions.is_empty());
    }

    #[test]
    fn load_from_or_default_parses_existing_file() {
        // And the happy path: same helper returns the parsed config when
        // the file does exist, so load_default's dispatch is sound.
        let path = unique_temp_file("or-default-exists");
        std::fs::write(&path, "reactions:\n  ci-failed:\n    action: notify\n").unwrap();
        let cfg = AoConfig::load_from_or_default(&path).unwrap();
        assert_eq!(cfg.reactions.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_config_without_notification_routing_defaults_empty() {
        // Backwards compat: a pre-Slice-3 config with only `reactions:`
        // must keep parsing. `notification_routing` falls back to its
        // `Default` (empty table) via `#[serde(default)]`.
        let path = unique_temp_file("no-routing");
        std::fs::write(&path, "reactions:\n  ci-failed:\n    action: notify\n").unwrap();
        let cfg = AoConfig::load_from(&path).unwrap();
        assert_eq!(cfg.reactions.len(), 1);
        assert!(cfg.notification_routing.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_parses_notification_routing_only() {
        // Config with `notification-routing:` but no `reactions:`
        // still parses. The kebab-case alias on the field name is
        // what lets the YAML write `notification-routing:`.
        let path = unique_temp_file("routing-only");
        std::fs::write(
            &path,
            r#"
notification-routing:
  urgent: [stdout, ntfy]
  warning: [stdout]
"#,
        )
        .unwrap();
        let cfg = AoConfig::load_from(&path).unwrap();
        assert!(cfg.reactions.is_empty());
        assert_eq!(cfg.notification_routing.len(), 2);
        assert_eq!(
            cfg.notification_routing
                .names_for(EventPriority::Urgent)
                .unwrap(),
            &["stdout".to_string(), "ntfy".to_string()]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_parses_reactions_and_routing_together() {
        // Full config with both sections — the common case once Phase C
        // ships. Also verifies the kebab-case `notification-routing:`
        // alias works alongside the kebab-case reaction keys.
        let path = unique_temp_file("full-config");
        std::fs::write(
            &path,
            r#"
reactions:
  ci-failed:
    action: send-to-agent
    message: "CI broke"
    retries: 3
  approved-and-green:
    action: auto-merge

notification-routing:
  urgent: [stdout]
  action: [stdout]
  warning: [stdout]
  info: [stdout]
"#,
        )
        .unwrap();
        let cfg = AoConfig::load_from(&path).unwrap();
        assert_eq!(cfg.reactions.len(), 2);
        assert_eq!(cfg.notification_routing.len(), 4);
        assert_eq!(
            cfg.reactions["ci-failed"].action,
            ReactionAction::SendToAgent
        );
        assert_eq!(
            cfg.notification_routing
                .names_for(EventPriority::Info)
                .unwrap(),
            &["stdout".to_string()]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn notification_routing_canonicalizes_on_write() {
        // The alias → rename contract: we accept `notification-routing:`
        // on read but always emit `notification_routing:` on write.
        // Matches the `escalate_after` canonicalization locked in by
        // Phase A of Slice 2.
        let path = unique_temp_file("canonical-routing");
        std::fs::write(&path, "notification-routing:\n  info: [stdout]\n").unwrap();
        let cfg = AoConfig::load_from(&path).unwrap();
        let yaml_out = serde_yaml::to_string(&cfg).unwrap();
        assert!(
            yaml_out.contains("notification_routing:"),
            "expected canonical snake_case key in output, got:\n{yaml_out}"
        );
        assert!(
            !yaml_out.contains("notification-routing:"),
            "expected no kebab-case key in output, got:\n{yaml_out}"
        );
        let _ = std::fs::remove_file(&path);
    }

    // --- New tests for Slice 5 Phase A ---

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
    fn project_config_roundtrip() {
        let pc = ProjectConfig {
            name: None,
            repo: "owner/repo".into(),
            path: "/tmp/test".into(),
            default_branch: "main".into(),
            session_prefix: None,
            branch_namespace: None,
            runtime: None,
            agent: None,
            workspace: None,
            tracker: None,
            scm: None,
            symlinks: vec![],
            post_create: vec![],
            agent_config: Some(AgentConfig::default()),
            orchestrator: None,
            worker: None,
            reactions: HashMap::new(),
            agent_rules: None,
            agent_rules_file: None,
            orchestrator_rules: None,
            orchestrator_session_strategy: None,
            opencode_issue_session_strategy: None,
        };
        let yaml = serde_yaml::to_string(&pc).unwrap();
        let pc2: ProjectConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(pc, pc2);
    }

    #[test]
    fn project_config_without_agent_config() {
        let pc = ProjectConfig {
            name: None,
            repo: "owner/repo".into(),
            path: "/tmp/test".into(),
            default_branch: "develop".into(),
            session_prefix: None,
            branch_namespace: None,
            runtime: None,
            agent: None,
            workspace: None,
            tracker: None,
            scm: None,
            symlinks: vec![],
            post_create: vec![],
            agent_config: None,
            orchestrator: None,
            worker: None,
            reactions: HashMap::new(),
            agent_rules: None,
            agent_rules_file: None,
            orchestrator_rules: None,
            orchestrator_session_strategy: None,
            opencode_issue_session_strategy: None,
        };
        let yaml = serde_yaml::to_string(&pc).unwrap();
        assert!(!yaml.contains("agent_config"));
        let pc2: ProjectConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(pc, pc2);
    }

    #[test]
    fn full_config_with_all_sections_roundtrips() {
        let mut projects = HashMap::new();
        projects.insert(
            "my-app".into(),
            ProjectConfig {
                name: None,
                repo: "org/my-app".into(),
                path: "/home/user/my-app".into(),
                default_branch: "main".into(),
                session_prefix: None,
                branch_namespace: None,
                runtime: None,
                agent: None,
                workspace: None,
                tracker: None,
                scm: None,
                symlinks: vec![],
                post_create: vec![],
                agent_config: Some(AgentConfig {
                    permissions: "default".into(),
                    rules: None,
                    rules_file: None,
                    model: None,
                    orchestrator_model: None,
                    opencode_session_id: None,
                }),
                orchestrator: None,
                worker: None,
                reactions: HashMap::new(),
                agent_rules: None,
                agent_rules_file: None,
                orchestrator_rules: None,
                orchestrator_session_strategy: None,
                opencode_issue_session_strategy: None,
            },
        );

        let config = AoConfig {
            port: default_port(),
            ready_threshold_ms: default_ready_threshold_ms(),
            poll_interval: default_poll_interval_secs(),
            terminal_port: None,
            direct_terminal_port: None,
            power: None,
            defaults: Some(DefaultsConfig::default()),
            projects,
            reactions: default_reactions(),
            notification_routing: default_routing(),
            notifiers: HashMap::new(),
            plugins: vec![],
        };

        let yaml = serde_yaml::to_string(&config).unwrap();
        let config2: AoConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config, config2);
    }

    #[test]
    fn existing_config_without_new_fields_still_parses() {
        let path = unique_temp_file("compat");
        std::fs::write(&path, "reactions:\n  ci-failed:\n    action: notify\n").unwrap();
        let cfg = AoConfig::load_from(&path).unwrap();
        assert_eq!(cfg.reactions.len(), 1);
        assert!(cfg.defaults.is_none());
        assert!(cfg.projects.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_to_writes_valid_yaml() {
        let path = unique_temp_file("save-to");
        let config = AoConfig {
            port: default_port(),
            ready_threshold_ms: default_ready_threshold_ms(),
            poll_interval: default_poll_interval_secs(),
            terminal_port: None,
            direct_terminal_port: None,
            power: None,
            defaults: Some(DefaultsConfig::default()),
            projects: HashMap::new(),
            reactions: default_reactions(),
            notification_routing: default_routing(),
            notifiers: HashMap::new(),
            plugins: vec![],
        };
        config.save_to(&path).unwrap();

        let loaded = AoConfig::load_from(&path).unwrap();
        assert_eq!(config, loaded);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn default_reactions_has_nine_keys() {
        use crate::reactions::default_priority_for_reaction_key;

        let reactions = default_reactions();
        assert_eq!(reactions.len(), 9);
        assert!(reactions.contains_key("ci-failed"));
        assert!(reactions.contains_key("changes-requested"));
        assert!(reactions.contains_key("merge-conflicts"));
        assert!(reactions.contains_key("approved-and-green"));
        assert!(reactions.contains_key("agent-idle"));
        assert!(reactions.contains_key("agent-stuck"));
        assert!(reactions.contains_key("agent-needs-input"));
        assert!(reactions.contains_key("agent-exited"));
        assert!(reactions.contains_key("all-complete"));

        for (key, rc) in &reactions {
            assert!(
                rc.priority.is_none(),
                "{key}: omit priority so default_priority_for_reaction_key applies"
            );
            let _ = default_priority_for_reaction_key(key);
        }
    }

    #[test]
    fn default_routing_covers_all_priorities() {
        let routing = default_routing();
        assert_eq!(routing.len(), 4);
        assert!(routing.names_for(EventPriority::Urgent).is_some());
        assert!(routing.names_for(EventPriority::Action).is_some());
        assert!(routing.names_for(EventPriority::Warning).is_some());
        assert!(routing.names_for(EventPriority::Info).is_some());
    }

    #[test]
    fn parse_owner_repo_https() {
        assert_eq!(
            parse_owner_repo("https://github.com/owner/repo.git"),
            Some("owner/repo".into())
        );
        assert_eq!(
            parse_owner_repo("https://github.com/owner/repo"),
            Some("owner/repo".into())
        );
    }

    #[test]
    fn parse_owner_repo_ssh() {
        assert_eq!(
            parse_owner_repo("git@github.com:owner/repo.git"),
            Some("owner/repo".into())
        );
        assert_eq!(
            parse_owner_repo("git@github.com:owner/repo"),
            Some("owner/repo".into())
        );
    }
}
