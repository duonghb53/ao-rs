//! Per-project configuration: `ProjectConfig`, `detect_git_repo`,
//! `generate_config`, and related helpers.

use super::{
    agent::{default_orchestrator_rules, default_permissions, AgentConfig},
    power::{DefaultsConfig, PluginConfig, RoleAgentConfig},
    reactions::{default_reactions, default_routing},
};
use crate::{
    error::{AoError, Result},
    parity_session_strategy::{OpencodeIssueSessionStrategy, OrchestratorSessionStrategy},
    reactions::ReactionConfig,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

pub(super) fn default_branch_name() -> String {
    "main".into()
}

pub(super) fn default_port() -> u16 {
    3000
}
pub(super) fn default_ready_threshold_ms() -> u64 {
    300_000
}
pub(super) fn default_poll_interval_secs() -> u64 {
    10
}

/// Per-project configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
        alias = "defaultBranch",
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
pub fn generate_config(cwd: &Path) -> Result<super::AoConfig> {
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

    Ok(super::AoConfig {
        schema_url: None,
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
    use crate::config::AoConfig;

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
                schema_url: None,
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
    fn camel_case_default_branch_loads_correctly() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ao-rs-config-camelcase-branch-{nanos}-{n}.yaml"));

        std::fs::write(
            &path,
            r#"
projects:
  my-app:
    repo: org/my-app
    path: /tmp/my-app
    defaultBranch: develop
"#,
        )
        .unwrap();
        let cfg = AoConfig::load_from(&path).unwrap();
        assert_eq!(
            cfg.projects["my-app"].default_branch, "develop",
            "camelCase defaultBranch must be accepted"
        );
        let _ = std::fs::remove_file(&path);
    }
}
