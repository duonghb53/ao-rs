//! Agent-level config types: `AgentConfig`, `PermissionsMode`,
//! default rules, and the `install_skills` helper.

use crate::error::{AoError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(super) fn default_permissions() -> PermissionsMode {
    PermissionsMode::Permissionless
}

/// Permission mode for agent execution.
///
/// Strict serde deserialization — unknown values fail at load time (TS parity: M4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionsMode {
    #[default]
    Permissionless,
    Default,
    AutoEdit,
    Suggest,
}

impl std::fmt::Display for PermissionsMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Permissionless => "permissionless",
            Self::Default => "default",
            Self::AutoEdit => "auto-edit",
            Self::Suggest => "suggest",
        };
        f.write_str(s)
    }
}

/// Agent-level overrides per project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Permission mode: permissionless, default, auto-edit, suggest.
    #[serde(default = "default_permissions")]
    pub permissions: PermissionsMode,

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
            permissions: PermissionsMode::Permissionless,
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
4. VERIFY — Run tests, linter, and formatter. Fix any failures before proceeding.
5. REVIEW — Re-read your changes. Check for security issues, missing edge cases, and unnecessary complexity.
6. DELIVER — Commit your changes, push the branch, and create a PR with `gh pr create`. Include a clear title and description.

Rules:
- When spawned from an issue, use the dev-lifecycle workflow to turn the issue content into concrete requirements and a plan, then execute it.
- Do not skip the verify step. Every change must pass tests and linting before you consider it done.
- Always push your branch and open a PR when the task is complete.
- Prefer editing existing files over creating new ones.
- Keep changes focused — fix what was asked, don't refactor surrounding code.
- If stuck for more than 5 minutes, explain what's blocking you.

Testing rules:
- Use `cargo t` (nextest alias) — NOT `cargo test`. Nextest is parallel/isolated and much faster.
- Run `cargo test --doc` separately for doctests (nextest skips them).
- Tests MUST be inline `#[cfg(test)] mod tests` at the bottom of the SAME file you changed.
- Run only your module's tests during development: `cargo t -p <crate> <module_name>`
- Before opening a PR: `cargo t --workspace && cargo test --doc --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`"#
}

/// Default orchestrator rules (read-only coordinator).
pub fn default_orchestrator_rules() -> &'static str {
    r#"After spawning a worker, do NOT stop. Run a monitoring loop:
1. Immediately confirm spawn with: ao-rs status
2. Every 5 minutes, check: ao-rs status --project <id>
3. When worker reaches pr_open/review_pending/merged/ci_failed → act
4. Only stop monitoring when all workers reach terminal state (merged/killed)

NEVER call `ao-rs cleanup` — it permanently archives sessions off-disk, making them
invisible in the dashboard. Merged/killed sessions must remain visible so the user can
review them. Only the user decides when to archive.

When writing tests (and when instructing workers to write tests):
- Tests MUST be inline `#[cfg(test)] mod tests` inside the SAME source file being changed.
- Do NOT create separate integration test files unless testing cross-module behavior.
- Run only the relevant module: `cargo t -p <crate> <module_name>`
- Never write tests for compiler-provable things (type correctness, exhaustive match, etc.)."#
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_mode_valid_values_parse() {
        for (yaml_val, expected) in [
            ("permissionless", PermissionsMode::Permissionless),
            ("default", PermissionsMode::Default),
            ("auto-edit", PermissionsMode::AutoEdit),
            ("suggest", PermissionsMode::Suggest),
        ] {
            let yaml = format!("permissions: {yaml_val}\n");
            let ac: AgentConfig = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(ac.permissions, expected, "failed for {yaml_val}");
        }
    }

    #[test]
    fn permissions_mode_display_roundtrip() {
        assert_eq!(
            PermissionsMode::Permissionless.to_string(),
            "permissionless"
        );
        assert_eq!(PermissionsMode::Default.to_string(), "default");
        assert_eq!(PermissionsMode::AutoEdit.to_string(), "auto-edit");
        assert_eq!(PermissionsMode::Suggest.to_string(), "suggest");
    }
}
