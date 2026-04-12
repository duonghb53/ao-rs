//! User-level config file: `~/.ao-rs/config.yaml`.
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
    paths,
    reactions::{EscalateAfter, EventPriority, ReactionAction, ReactionConfig},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

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
fn default_branch_name() -> String {
    "main".into()
}
fn default_permissions() -> String {
    "permissionless".into()
}

// --- Config types ---

/// Orchestrator-wide defaults for plugin selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultsConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(default = "default_workspace")]
    pub workspace: String,
    #[serde(default)]
    pub notifiers: Vec<String>,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            runtime: default_runtime(),
            agent: default_agent(),
            workspace: default_workspace(),
            notifiers: vec![],
        }
    }
}

/// Per-project configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
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
    /// Agent-specific overrides.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "agent-config",
        rename = "agent_config"
    )]
    pub agent_config: Option<AgentConfig>,
}

/// Agent-level overrides per project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Permission mode: "permissionless", "default", "auto-edit", "suggest".
    #[serde(default = "default_permissions")]
    pub permissions: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            permissions: default_permissions(),
        }
    }
}

/// Top-level ao-rs config file shape. All fields use `#[serde(default)]`
/// so partial config files parse without error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AoConfig {
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
            priority: Some(EventPriority::Action),
            retries: Some(2),
            escalate_after: Some(EscalateAfter::Attempts(2)),
            threshold: None,
            include_summary: false,
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
            priority: Some(EventPriority::Action),
            retries: None,
            escalate_after: Some(EscalateAfter::Duration("30m".into())),
            threshold: None,
            include_summary: false,
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
            priority: Some(EventPriority::Action),
            retries: None,
            escalate_after: Some(EscalateAfter::Duration("15m".into())),
            threshold: None,
            include_summary: false,
        },
    );
    m.insert(
        "approved-and-green".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::AutoMerge,
            message: None,
            priority: Some(EventPriority::Action),
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
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
            priority: Some(EventPriority::Info),
            retries: Some(2),
            escalate_after: Some(EscalateAfter::Duration("15m".into())),
            threshold: None,
            include_summary: false,
        },
    );
    m.insert(
        "agent-stuck".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: Some(EventPriority::Urgent),
            retries: None,
            escalate_after: None,
            threshold: Some("10m".into()),
            include_summary: false,
        },
    );
    m.insert(
        "agent-needs-input".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: Some(EventPriority::Urgent),
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
        },
    );
    m.insert(
        "agent-exited".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: Some(EventPriority::Urgent),
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
        },
    );
    m.insert(
        "all-complete".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: Some(EventPriority::Info),
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: true,
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
            repo: owner_repo,
            path: abs_path.to_string_lossy().to_string(),
            default_branch,
            agent_config: Some(AgentConfig::default()),
        },
    );

    Ok(AoConfig {
        defaults: Some(DefaultsConfig::default()),
        projects,
        reactions: default_reactions(),
        notification_routing: default_routing(),
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

    // --- New tests for Slice 5 Phase A ---

    #[test]
    fn defaults_config_roundtrip() {
        let dc = DefaultsConfig::default();
        assert_eq!(dc.runtime, "tmux");
        assert_eq!(dc.agent, "claude-code");
        assert_eq!(dc.workspace, "worktree");
        assert!(dc.notifiers.is_empty());

        let yaml = serde_yaml::to_string(&dc).unwrap();
        let dc2: DefaultsConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(dc, dc2);
    }

    #[test]
    fn project_config_roundtrip() {
        let pc = ProjectConfig {
            repo: "owner/repo".into(),
            path: "/tmp/test".into(),
            default_branch: "main".into(),
            agent_config: Some(AgentConfig::default()),
        };
        let yaml = serde_yaml::to_string(&pc).unwrap();
        let pc2: ProjectConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(pc, pc2);
    }

    #[test]
    fn project_config_without_agent_config() {
        let pc = ProjectConfig {
            repo: "owner/repo".into(),
            path: "/tmp/test".into(),
            default_branch: "develop".into(),
            agent_config: None,
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
                repo: "org/my-app".into(),
                path: "/home/user/my-app".into(),
                default_branch: "main".into(),
                agent_config: Some(AgentConfig {
                    permissions: "default".into(),
                }),
            },
        );

        let config = AoConfig {
            defaults: Some(DefaultsConfig::default()),
            projects,
            reactions: default_reactions(),
            notification_routing: default_routing(),
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
            defaults: Some(DefaultsConfig::default()),
            projects: HashMap::new(),
            reactions: default_reactions(),
            notification_routing: default_routing(),
        };
        config.save_to(&path).unwrap();

        let loaded = AoConfig::load_from(&path).unwrap();
        assert_eq!(config, loaded);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn default_reactions_has_nine_keys() {
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
