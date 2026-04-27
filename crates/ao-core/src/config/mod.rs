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

pub mod agent;
pub mod power;
pub mod project;
pub mod reactions;

pub use agent::{
    default_agent_rules, default_orchestrator_rules, install_skills, AgentConfig, PermissionsMode,
};
pub use power::{DefaultsConfig, PluginConfig, PowerConfig, RoleAgentConfig, ScmWebhookConfig};
pub use project::{detect_git_repo, generate_config, ProjectConfig};
pub use reactions::{default_reactions, default_routing};

use crate::{
    error::{AoError, Result},
    notifier::NotificationRouting,
    parity_config_validation::{
        validate_project_uniqueness, TsOrchestratorConfig, TsProjectConfig,
    },
    reaction_engine::parse_duration,
    reactions::{EscalateAfter, EventPriority, ReactionConfig},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

/// Canonical URL for the committed JSON Schema file.
///
/// Editors (VS Code + YAML extension, JetBrains) use this to provide
/// IntelliSense and validation on `ao-rs.yaml` files.
pub const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/duonghb53/ao-rs/main/schema/ao-rs.schema.json";

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

impl AoConfig {
    /// Validate config semantics (beyond YAML parsing).
    ///
    /// Returns `Ok(())` when valid, otherwise a `AoError::Config` with an
    /// actionable, field-scoped message including the config file path.
    pub fn validate(&self, config_path: &Path) -> Result<()> {
        // ---- reactions.* keys ----
        for key in self.reactions.keys() {
            if !reactions::supported_reaction_keys().contains(&key.as_str()) {
                let mut keys: Vec<&str> = reactions::supported_reaction_keys().to_vec();
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
        if let Some(defaults) = self.defaults.as_ref() {
            for name in &defaults.notifiers {
                if !reactions::supported_notifier_names().contains(&name.as_str()) {
                    return Err(AoError::Config(format!(
                        "{}: unknown notifier name at `defaults.notifiers`: {:?} (supported: {})",
                        config_path.display(),
                        name,
                        reactions::supported_notifier_names().join(", ")
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
                    if !reactions::supported_notifier_names().contains(&name.as_str()) {
                        return Err(AoError::Config(format!(
                            "{}: unknown notifier name at `notification_routing.{}[]`: {:?} (supported: {})",
                            config_path.display(),
                            priority.as_str(),
                            name,
                            reactions::supported_notifier_names().join(", ")
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

        // ---- duplicate project basenames / session-prefix (H4) ----
        if self.projects.len() > 1 {
            let ts_config = TsOrchestratorConfig {
                projects: self
                    .projects
                    .iter()
                    .map(|(k, p)| {
                        (
                            k.clone(),
                            TsProjectConfig {
                                repo: p.repo.clone(),
                                path: p.path.clone(),
                                default_branch: p.default_branch.clone(),
                                session_prefix: p.session_prefix.clone(),
                            },
                        )
                    })
                    .collect(),
            };
            validate_project_uniqueness(&ts_config)
                .map_err(|msg| AoError::Config(format!("{}: {}", config_path.display(), msg)))?;
        }

        Ok(())
    }
}

/// Top-level ao-rs config file shape. All fields use `#[serde(default)]`
/// so partial config files parse without error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AoConfig {
    /// JSON Schema URL for editor IntelliSense/validation.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    #[schemars(rename = "$schema")]
    pub schema_url: Option<String>,

    /// Dashboard port (TS: `port`).
    #[serde(default = "project::default_port")]
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
        default = "project::default_ready_threshold_ms",
        rename = "ready_threshold_ms",
        alias = "readyThresholdMs",
        alias = "ready-threshold-ms"
    )]
    pub ready_threshold_ms: u64,
    /// Lifecycle polling interval in seconds (default 10).
    #[serde(
        default = "project::default_poll_interval_secs",
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
    #[schemars(skip)]
    pub plugins: Vec<HashMap<String, serde_yaml::Value>>,
}

impl Default for AoConfig {
    fn default() -> Self {
        Self {
            schema_url: None,
            port: project::default_port(),
            ready_threshold_ms: project::default_ready_threshold_ms(),
            poll_interval: project::default_poll_interval_secs(),
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
    ///
    /// Always stamps `$schema:` so editors get IntelliSense. Existing
    /// non-empty schema URLs are preserved; absent or blank values are
    /// replaced with `SCHEMA_URL`.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut to_write = self.clone();
        if to_write.schema_url.as_deref().map_or(true, str::is_empty) {
            to_write.schema_url = Some(SCHEMA_URL.to_string());
        }
        let yaml = serde_yaml::to_string(&to_write).map_err(|e| AoError::Yaml(e.to_string()))?;
        std::fs::write(path, yaml)?;
        Ok(())
    }
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
        assert_eq!(ci.action, crate::reactions::ReactionAction::SendToAgent);
        assert_eq!(ci.message.as_deref(), Some("CI broke — please fix."));

        let _ = std::fs::remove_file(&path);
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
            crate::reactions::ReactionAction::SendToAgent
        );
        assert_eq!(cfg.reactions["ci-failed"].retries, Some(3));
        assert_eq!(
            cfg.reactions["changes-requested"].action,
            crate::reactions::ReactionAction::SendToAgent
        );
        assert_eq!(
            cfg.reactions["approved-and-green"].action,
            crate::reactions::ReactionAction::AutoMerge
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
            crate::reactions::ReactionAction::SendToAgent
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
                    permissions: PermissionsMode::Default,
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
            schema_url: None,
            port: project::default_port(),
            ready_threshold_ms: project::default_ready_threshold_ms(),
            poll_interval: project::default_poll_interval_secs(),
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
            schema_url: None,
            port: project::default_port(),
            ready_threshold_ms: project::default_ready_threshold_ms(),
            poll_interval: project::default_poll_interval_secs(),
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
        // save_to injects $schema; loaded config will have schema_url set.
        let mut expected = config.clone();
        expected.schema_url = Some(SCHEMA_URL.to_string());
        assert_eq!(expected, loaded);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_to_injects_schema_url() {
        let path = unique_temp_file("schema-inject");
        let config = AoConfig::default();
        config.save_to(&path).unwrap();

        let yaml = std::fs::read_to_string(&path).unwrap();
        assert!(
            yaml.contains("$schema:"),
            "saved YAML must contain $schema key"
        );
        assert!(
            yaml.contains(SCHEMA_URL),
            "saved YAML must contain canonical schema URL"
        );

        let loaded = AoConfig::load_from(&path).unwrap();
        assert_eq!(loaded.schema_url.as_deref(), Some(SCHEMA_URL));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_to_preserves_existing_schema_url() {
        let path = unique_temp_file("schema-preserve");
        let custom_url = "https://example.com/my-schema.json";
        let mut config = AoConfig::default();
        config.schema_url = Some(custom_url.to_string());
        config.save_to(&path).unwrap();

        let loaded = AoConfig::load_from(&path).unwrap();
        assert_eq!(
            loaded.schema_url.as_deref(),
            Some(custom_url),
            "custom schema URL must not be overwritten"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_rejects_duplicate_project_basename() {
        let path = unique_temp_file("dup-basename");
        std::fs::write(
            &path,
            r#"
projects:
  proj-a:
    repo: org/app
    path: /home/user/app
  proj-b:
    repo: org/app2
    path: /home/other/app
"#,
        )
        .unwrap();
        let err = AoConfig::load_from_with_warnings(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Duplicate project ID"),
            "expected duplicate basename error, got: {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_rejects_duplicate_session_prefix() {
        let path = unique_temp_file("dup-prefix");
        std::fs::write(
            &path,
            r#"
projects:
  proj-a:
    repo: org/app
    path: /home/user/my-app
    sessionPrefix: myapp
  proj-b:
    repo: org/other
    path: /home/user/other-app
    sessionPrefix: myapp
"#,
        )
        .unwrap();
        let err = AoConfig::load_from_with_warnings(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Duplicate session prefix"),
            "expected duplicate session prefix error, got: {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn permissions_mode_typo_fails_to_load() {
        let path = unique_temp_file("bad-permissions");
        std::fs::write(
            &path,
            r#"
projects:
  my-app:
    repo: org/my-app
    path: /tmp/my-app
    agent_config:
      permissions: permisionless
"#,
        )
        .unwrap();
        let err = AoConfig::load_from(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("permisionless") || msg.contains("unknown variant"),
            "expected deserialization error for typo, got: {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_file_matches_aconfig_derive() {
        let schema = schemars::schema_for!(AoConfig);
        let generated = serde_json::to_string_pretty(&schema).unwrap();

        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let schema_path = workspace_root.join("schema/ao-rs.schema.json");

        if !schema_path.exists() {
            panic!(
                "schema/ao-rs.schema.json not found at {}.\n\
                 Run: cargo t -p ao-core config::schema_regenerate_committed_file",
                schema_path.display()
            );
        }

        let committed = std::fs::read_to_string(&schema_path).unwrap();
        assert_eq!(
            generated.trim(),
            committed.trim(),
            "schema/ao-rs.schema.json is out of date.\n\
             Run the `schema_regenerate_committed_file` test with UPDATE_SCHEMA=1 to regenerate:\n\
             UPDATE_SCHEMA=1 cargo t -p ao-core config::schema_regenerate_committed_file"
        );
    }

    /// Not a real test — run with `UPDATE_SCHEMA=1` to regenerate the schema file.
    #[test]
    fn schema_regenerate_committed_file() {
        if std::env::var("UPDATE_SCHEMA").unwrap_or_default() != "1" {
            return;
        }
        let schema = schemars::schema_for!(AoConfig);
        let generated = serde_json::to_string_pretty(&schema).unwrap();

        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let schema_dir = workspace_root.join("schema");
        std::fs::create_dir_all(&schema_dir).unwrap();
        let schema_path = schema_dir.join("ao-rs.schema.json");
        std::fs::write(&schema_path, format!("{generated}\n")).unwrap();
        println!("wrote {}", schema_path.display());
    }
}
