//! Claude Code agent plugin.
//!
//! Launches `claude --dangerously-skip-permissions` in interactive mode
//! and delivers the task via post-launch `send_message`. Supports
//! `--append-system-prompt` for injecting structured agent rules from
//! the project's `ao-rs.yaml` config (dev-lifecycle phases, testing
//! requirements, coding standards).
//!
//! The prompt is intentionally **not** baked into the launch command —
//! claude-code uses "post-launch delivery", meaning the orchestrator runs
//! `claude` interactively first, then sends the task via the runtime's
//! `send_message`. Using `claude -p <prompt>` would put it in one-shot mode
//! and exit after responding, which defeats the whole orchestration.

use ao_core::{default_agent_rules, ActivityState, Agent, AgentConfig, Result, Session};
use async_trait::async_trait;

pub struct ClaudeCodeAgent {
    /// Agent rules injected via --append-system-prompt.
    rules: Option<String>,
}

impl ClaudeCodeAgent {
    pub fn new() -> Self {
        Self { rules: None }
    }

    /// Create from project agent config.
    pub fn from_config(config: &AgentConfig) -> Self {
        // rules_file takes precedence over inline rules.
        let rules = if let Some(ref path) = config.rules_file {
            match std::fs::read_to_string(path) {
                Ok(content) => Some(content),
                Err(e) => {
                    tracing::warn!("could not read rules file {path}: {e}, using inline rules");
                    config.rules.clone()
                }
            }
        } else {
            config.rules.clone()
        };
        Self { rules }
    }

    /// Create with default dev-lifecycle rules.
    pub fn with_default_rules() -> Self {
        Self {
            rules: Some(default_agent_rules().to_string()),
        }
    }
}

impl Default for ClaudeCodeAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for ClaudeCodeAgent {
    fn launch_command(&self, _session: &Session) -> String {
        let mut cmd = "claude --dangerously-skip-permissions".to_string();

        if let Some(ref rules) = self.rules {
            // Shell-escape the rules for --append-system-prompt.
            let escaped = rules.replace('\'', "'\\''");
            cmd.push_str(&format!(" --append-system-prompt '{escaped}'"));
        }

        cmd
    }

    fn environment(&self, session: &Session) -> Vec<(String, String)> {
        vec![
            ("CLAUDECODE".to_string(), String::new()),
            ("AO_SESSION_ID".to_string(), session.id.to_string()),
        ]
    }

    fn initial_prompt(&self, session: &Session) -> String {
        session.task.clone()
    }

    async fn detect_activity(&self, _session: &Session) -> Result<ActivityState> {
        Ok(ActivityState::Ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{now_ms, SessionId, SessionStatus};
    use std::path::PathBuf;

    fn fake_session() -> Session {
        Session {
            id: SessionId("test-id".into()),
            project_id: "demo".into(),
            status: SessionStatus::Spawning,
            branch: "feat-x".into(),
            task: "fix the typo in README".into(),
            workspace_path: Some(PathBuf::from("/tmp/demo")),
            runtime_handle: None,
            activity: None,
            created_at: now_ms(),
        }
    }

    #[test]
    fn launch_command_no_rules() {
        let agent = ClaudeCodeAgent::new();
        assert_eq!(
            agent.launch_command(&fake_session()),
            "claude --dangerously-skip-permissions"
        );
    }

    #[test]
    fn with_default_rules_appends_system_prompt() {
        let agent = ClaudeCodeAgent::with_default_rules();
        let cmd = agent.launch_command(&fake_session());
        assert!(cmd.starts_with("claude --dangerously-skip-permissions --append-system-prompt"));
        assert!(cmd.contains("UNDERSTAND"));
        assert!(cmd.contains("DELIVER"));
    }

    #[test]
    fn from_config_uses_inline_rules() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: Some("custom rule set".into()),
            rules_file: None,
        };
        let agent = ClaudeCodeAgent::from_config(&config);
        let cmd = agent.launch_command(&fake_session());
        assert!(cmd.contains("--append-system-prompt"));
        assert!(cmd.contains("custom rule set"));
    }

    #[test]
    fn from_config_rules_file_fallback() {
        // When rules_file points to a non-existent path, falls back to inline rules.
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: Some("fallback rules".into()),
            rules_file: Some("/tmp/nonexistent-ao-rules-file.md".into()),
        };
        let agent = ClaudeCodeAgent::from_config(&config);
        let cmd = agent.launch_command(&fake_session());
        assert!(cmd.contains("fallback rules"));
    }

    #[test]
    fn from_config_no_rules() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: None,
            rules_file: None,
        };
        let agent = ClaudeCodeAgent::from_config(&config);
        assert_eq!(
            agent.launch_command(&fake_session()),
            "claude --dangerously-skip-permissions"
        );
    }

    #[test]
    fn environment_includes_session_id_and_clears_claudecode() {
        let agent = ClaudeCodeAgent::new();
        let env = agent.environment(&fake_session());
        assert!(env
            .iter()
            .any(|(k, v)| k == "AO_SESSION_ID" && v == "test-id"));
        assert!(env.iter().any(|(k, v)| k == "CLAUDECODE" && v.is_empty()));
    }

    #[test]
    fn initial_prompt_returns_task() {
        let agent = ClaudeCodeAgent::new();
        assert_eq!(
            agent.initial_prompt(&fake_session()),
            "fix the typo in README"
        );
    }
}
