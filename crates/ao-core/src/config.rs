//! User-level config file: `~/.ao-rs/config.yaml`.
//!
//! Slice 2 Phase D introduces this loader because the reaction engine needs
//! to know which reactions are configured, with what actions and retry
//! budgets. Mirrors the top-level `OrchestratorConfig.reactions` field in
//! `packages/core/src/types.ts`.
//!
//! ## Scope for Phase D
//!
//! Only the `reactions` subtree is read. Everything else the TS config
//! carries (`projects`, `notifications`, `logging`, ...) is deliberately
//! dropped — those are either derived per-session from the workspace git
//! remote (SCM plugin) or handled by other Slice 2 phases. Adding them here
//! would force premature design decisions about multi-project layout that
//! we're not ready to make.
//!
//! ## Missing-file handling
//!
//! `load_default()` returns an empty `AoConfig` if the file doesn't exist.
//! Rationale: a fresh install should run without the user being forced to
//! create a config first, and "no reactions configured" is a legitimate
//! state (the lifecycle loop still polls, it just doesn't auto-react).
//! Parse errors, on the other hand, propagate — a broken config is
//! something the user needs to fix.

use crate::{
    error::{AoError, Result},
    notifier::NotificationRouting,
    paths,
    reactions::ReactionConfig,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

/// Top-level ao-rs config file shape. `#[serde(default)]` on every
/// field lets us tolerate a config file that hasn't set some of them —
/// every section is individually optional so a user can start with
/// `reactions:` only and add `notification-routing:` later without
/// breaking the parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AoConfig {
    /// Map from reaction key (e.g. `"ci-failed"`) to its config. The engine
    /// looks up by key at dispatch time; unknown keys mean "no reaction
    /// configured for that trigger, skip it".
    #[serde(default)]
    pub reactions: HashMap<String, ReactionConfig>,

    /// Priority-based routing table read by `NotifierRegistry` (Slice 3
    /// Phase A). Missing from the config file → empty routing table →
    /// the registry warn-onces per priority on its first resolve and
    /// drops the notification. The `ao-cli` wiring layer (Phase C)
    /// applies the "default to stdout when empty" fallback, not this
    /// type.
    ///
    /// `alias = "notification-routing"` so config files can use the
    /// kebab-case form on the wire (consistent with `escalate-after`
    /// from Phase H); canonical write-back is the snake_case field
    /// name from `rename`.
    #[serde(
        default,
        rename = "notification_routing",
        alias = "notification-routing"
    )]
    pub notification_routing: NotificationRouting,
}

impl AoConfig {
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

    /// `load_from_or_default` wired to `~/.ao-rs/config.yaml`. The binary
    /// calls this at startup; tests should prefer `load_from_or_default`.
    pub fn load_default() -> Result<Self> {
        Self::load_from_or_default(&Self::default_path())
    }

    /// Canonical config file path under `~/.ao-rs/`.
    pub fn default_path() -> std::path::PathBuf {
        paths::data_dir().join("config.yaml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactions::{EventPriority, ReactionAction};
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
}
