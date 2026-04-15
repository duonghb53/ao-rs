//! Agent/runtime plugin selection and multi-agent delegation.

use std::sync::Arc;

use ao_core::{ActivityState, Agent, AgentConfig, Runtime, Session};
use ao_plugin_agent_aider::AiderAgent;
use ao_plugin_agent_claude_code::ClaudeCodeAgent;
use ao_plugin_agent_codex::CodexAgent;
use ao_plugin_agent_cursor::CursorAgent;
use ao_plugin_runtime_process::ProcessRuntime;
use ao_plugin_runtime_tmux::TmuxRuntime;
use async_trait::async_trait;

/// Typed error for duplicate issue detection so `batch_spawn` can distinguish
/// "skipped duplicate" from "real failure" without string matching.
#[derive(Debug)]
pub(crate) struct DuplicateIssue {
    pub(crate) issue_id: String,
    pub(crate) session_short: String,
}

impl std::fmt::Display for DuplicateIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "active session {} is already working on issue #{}. use --force to spawn anyway",
            self.session_short, self.issue_id
        )
    }
}

impl std::error::Error for DuplicateIssue {}

/// Select an agent plugin by name, optionally reading rules from config.
///
/// Warns (but does not error) if the name is unknown — falls back to
/// `claude-code` so that older configs still work.
pub(crate) fn select_agent(name: &str, agent_config: Option<&AgentConfig>) -> Box<dyn Agent> {
    match name {
        "codex" => match agent_config {
            Some(cfg) => Box::new(CodexAgent::from_config(cfg)),
            None => Box::new(CodexAgent::new()),
        },
        "aider" => match agent_config {
            Some(cfg) => Box::new(AiderAgent::from_config(cfg)),
            None => Box::new(AiderAgent::new()),
        },
        "cursor" => match agent_config {
            Some(cfg) => Box::new(CursorAgent::from_config(cfg)),
            None => Box::new(CursorAgent::new()),
        },
        "claude-code" => match agent_config {
            Some(cfg) => Box::new(ClaudeCodeAgent::from_config(cfg)),
            None => Box::new(ClaudeCodeAgent::with_default_rules()),
        },
        _ => {
            eprintln!("warning: unknown agent '{name}', falling back to claude-code");
            match agent_config {
                Some(cfg) => Box::new(ClaudeCodeAgent::from_config(cfg)),
                None => Box::new(ClaudeCodeAgent::with_default_rules()),
            }
        }
    }
}

pub(crate) fn select_runtime(name: &str) -> Arc<dyn Runtime> {
    match name {
        "process" => Arc::new(ProcessRuntime::new()),
        "tmux" => Arc::new(TmuxRuntime::new()),
        _ => {
            eprintln!("warning: unknown runtime '{name}', falling back to tmux");
            Arc::new(TmuxRuntime::new())
        }
    }
}

pub(crate) struct MultiAgent;

#[async_trait]
impl Agent for MultiAgent {
    fn launch_command(&self, session: &Session) -> String {
        select_agent(&session.agent, session.agent_config.as_ref()).launch_command(session)
    }

    fn environment(&self, session: &Session) -> Vec<(String, String)> {
        select_agent(&session.agent, session.agent_config.as_ref()).environment(session)
    }

    fn initial_prompt(&self, session: &Session) -> String {
        select_agent(&session.agent, session.agent_config.as_ref()).initial_prompt(session)
    }

    async fn detect_activity(&self, session: &Session) -> ao_core::Result<ActivityState> {
        select_agent(&session.agent, session.agent_config.as_ref())
            .detect_activity(session)
            .await
    }

    async fn cost_estimate(
        &self,
        session: &Session,
    ) -> ao_core::Result<Option<ao_core::CostEstimate>> {
        select_agent(&session.agent, session.agent_config.as_ref())
            .cost_estimate(session)
            .await
    }
}
