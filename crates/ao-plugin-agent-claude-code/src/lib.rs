//! claude-code agent plugin — Slice 0 stub.
//!
//! The TS reference (`packages/plugins/agent-claude-code/src/index.ts`,
//! ~865 LOC) handles a lot more: permission flags, model selection,
//! `--append-system-prompt` with file substitution, activity detection
//! via JSONL log parsing, resume on restart, and a metadata-updater hook
//! script. This Slice 0 stub provides only the bare minimum needed for
//! `ao-rs spawn` to start a `claude` process and deliver an initial prompt.
//!
//! The prompt is intentionally **not** baked into the launch command —
//! claude-code uses "post-launch delivery", meaning the orchestrator runs
//! `claude` interactively first, then sends the task via the runtime's
//! `send_message`. Using `claude -p <prompt>` would put it in one-shot mode
//! and exit after responding, which defeats the whole orchestration.

use ao_core::{Agent, Session};

pub struct ClaudeCodeAgent;

impl ClaudeCodeAgent {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeCodeAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for ClaudeCodeAgent {
    fn launch_command(&self, _session: &Session) -> String {
        // Slice 0: bare invocation. Slice 1+ will add --model,
        // --append-system-prompt, --dangerously-skip-permissions, etc.
        "claude".to_string()
    }

    fn environment(&self, session: &Session) -> Vec<(String, String)> {
        vec![
            // Unset CLAUDECODE so nested-agent detection in claude itself
            // doesn't refuse to start when ao-rs is run from inside claude.
            ("CLAUDECODE".to_string(), String::new()),
            // Let the running agent know its own session id (for introspection,
            // future hook scripts, and metadata writes).
            ("AO_SESSION_ID".to_string(), session.id.to_string()),
        ]
    }

    fn initial_prompt(&self, session: &Session) -> String {
        // The user-supplied task is the first thing the agent sees.
        session.task.clone()
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
            created_at: now_ms(),
        }
    }

    #[test]
    fn launch_command_is_bare_claude() {
        let agent = ClaudeCodeAgent::new();
        assert_eq!(agent.launch_command(&fake_session()), "claude");
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
