//! Cursor Agent CLI plugin.
//!
//! Launches the Cursor background agent (`agent`) in permissionless mode
//! inside a tmux session. Mirrors `packages/plugins/agent-cursor/src/index.ts`
//! in the TypeScript agent-orchestrator.
//!
//! ## Launch strategy
//!
//! The TS reference embeds the full prompt in the launch command's positional
//! argument (`agent -- <prompt>`). This Rust port uses **post-launch delivery**
//! instead — same as the Claude Code plugin — where the orchestrator runs
//! `agent` interactively, then sends the task via `Runtime::send_message`.
//! This keeps the architecture consistent across agent plugins and avoids
//! shell-escaping multi-kilobyte prompts in tmux commands.
//!
//! ## Activity detection
//!
//! Cursor doesn't write JSONL session logs like Claude Code. Detection uses:
//! 1. `.cursor/chat.md` file mtime — if recently modified, agent is active.
//! 2. Recent git commits in the workspace — if any within 60s, agent is active.
//! 3. Fallback: `ActivityState::Ready` (runtime liveness covers process exit).
//!
//! ## Cost tracking
//!
//! Cursor doesn't expose token/cost data via CLI. Returns `None`.

use ao_core::{ActivityState, Agent, AgentConfig, Result, Session};
use async_trait::async_trait;
use std::path::Path;

/// Idle threshold: if `.cursor/chat.md` hasn't been modified in this many
/// seconds, consider the agent idle. Matches the Claude Code plugin's 5 min.
const IDLE_THRESHOLD_SECS: u64 = 300;

/// Active window: if `.cursor/chat.md` was modified within this many seconds,
/// the agent is actively working (not just "ready").
const ACTIVE_WINDOW_SECS: u64 = 30;

pub struct CursorAgent {
    /// Rules prepended to the prompt. Cursor doesn't have a system prompt
    /// flag, so rules are delivered as part of the initial prompt.
    rules: Option<String>,
}

impl CursorAgent {
    pub fn new() -> Self {
        Self { rules: None }
    }

    /// Create from project agent config.
    pub fn from_config(config: &AgentConfig) -> Self {
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
}

impl Default for CursorAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for CursorAgent {
    fn launch_command(&self, _session: &Session) -> String {
        // Cursor agent in permissionless mode:
        //   --force: auto-approve all changes (alias: --yolo)
        //   --sandbox disabled: skip workspace trust prompts
        //   --approve-mcps: auto-approve MCP servers
        "agent --force --sandbox disabled --approve-mcps".to_string()
    }

    fn environment(&self, session: &Session) -> Vec<(String, String)> {
        vec![
            ("AO_SESSION_ID".to_string(), session.id.to_string()),
            // Issue ID for workspace hooks / prompt enrichment.
            (
                "AO_ISSUE_ID".to_string(),
                session.issue_id.clone().unwrap_or_default(),
            ),
        ]
    }

    fn initial_prompt(&self, session: &Session) -> String {
        // NOTE: The CLI spawn flow uses `prompt_builder::build_prompt()` for
        // richer 3-layer prompts. This is a backward-compat fallback.
        //
        // Cursor doesn't have --append-system-prompt, so if rules are
        // configured, prepend them to the task.
        let task_part = if let Some(ref id) = session.issue_id {
            let url_line = session
                .issue_url
                .as_deref()
                .map(|u| format!("\nIssue URL: {u}"))
                .unwrap_or_default();
            format!(
                "You are working on issue #{id} on branch `{branch}`.{url_line}\n\n\
                 Task:\n{task}\n\n\
                 When complete, push your branch and open a pull request.",
                branch = session.branch,
                task = session.task,
            )
        } else {
            session.task.clone()
        };

        match &self.rules {
            Some(rules) => format!("{rules}\n\n---\n\n{task_part}"),
            None => task_part,
        }
    }

    async fn detect_activity(&self, session: &Session) -> Result<ActivityState> {
        let Some(ref ws) = session.workspace_path else {
            return Ok(ActivityState::Ready);
        };
        // File I/O is blocking — run off the executor thread.
        let ws = ws.clone();
        tokio::task::spawn_blocking(move || detect_cursor_activity(&ws))
            .await
            .map_err(|e| ao_core::AoError::Other(format!("detect_activity panicked: {e}")))?
    }

    // Cursor doesn't expose token/cost data — use the default (None).
}

// ---------------------------------------------------------------------------
// Activity detection
// ---------------------------------------------------------------------------

/// Determine agent activity by checking Cursor workspace artifacts.
///
/// Strategy (multi-fallback, mirrors TS):
///   1. `.cursor/chat.md` mtime — direct evidence of Cursor writes.
///   2. Recent git commits (within 60s) — indirect evidence of agent work.
///   3. Fallback: `Ready` — runtime liveness covers process exit.
fn detect_cursor_activity(workspace_path: &Path) -> Result<ActivityState> {
    // 1. Check .cursor/chat.md mtime.
    let chat_file = workspace_path.join(".cursor").join("chat.md");
    if let Ok(metadata) = std::fs::metadata(&chat_file) {
        let Ok(modified) = metadata.modified() else {
            // Platform doesn't support mtime — fall back to Ready rather
            // than silently reporting Active with a faked timestamp.
            return Ok(ActivityState::Ready);
        };
        let age = std::time::SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();

        if age.as_secs() <= ACTIVE_WINDOW_SECS {
            return Ok(ActivityState::Active);
        }
        if age.as_secs() <= IDLE_THRESHOLD_SECS {
            return Ok(ActivityState::Ready);
        }
        return Ok(ActivityState::Idle);
    }

    // 2. Check for recent git commits.
    if has_recent_commits(workspace_path) {
        return Ok(ActivityState::Active);
    }

    // 3. Fallback — no cursor artifacts, agent may have just started.
    Ok(ActivityState::Ready)
}

/// Check if any git commits were made in the workspace within the last 60s.
fn has_recent_commits(workspace_path: &Path) -> bool {
    let output = std::process::Command::new("git")
        .args(["log", "--since=60 seconds ago", "--format=%H"])
        .current_dir(workspace_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{now_ms, SessionId, SessionStatus};
    use std::path::PathBuf;

    fn fake_session() -> Session {
        Session {
            id: SessionId("cursor-test".into()),
            project_id: "demo".into(),
            status: SessionStatus::Working,
            agent: "cursor".into(),
            agent_config: None,
            branch: "ao-abc123-feat-test".into(),
            task: "fix the bug".into(),
            workspace_path: Some(PathBuf::from("/tmp/cursor-demo")),
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
        }
    }

    #[test]
    fn launch_command_is_permissionless() {
        let agent = CursorAgent::new();
        let cmd = agent.launch_command(&fake_session());
        assert!(cmd.contains("agent"));
        assert!(cmd.contains("--force"));
        assert!(cmd.contains("--sandbox disabled"));
        assert!(cmd.contains("--approve-mcps"));
    }

    #[test]
    fn environment_includes_session_id() {
        let agent = CursorAgent::new();
        let env = agent.environment(&fake_session());
        assert!(env
            .iter()
            .any(|(k, v)| k == "AO_SESSION_ID" && v == "cursor-test"));
    }

    #[test]
    fn environment_includes_empty_issue_id_when_none() {
        let agent = CursorAgent::new();
        let env = agent.environment(&fake_session());
        assert!(env.iter().any(|(k, v)| k == "AO_ISSUE_ID" && v.is_empty()));
    }

    #[test]
    fn environment_includes_issue_id_when_set() {
        let agent = CursorAgent::new();
        let mut session = fake_session();
        session.issue_id = Some("42".into());
        let env = agent.environment(&session);
        assert!(env.iter().any(|(k, v)| k == "AO_ISSUE_ID" && v == "42"));
    }

    #[test]
    fn initial_prompt_task_first() {
        let agent = CursorAgent::new();
        assert_eq!(agent.initial_prompt(&fake_session()), "fix the bug");
    }

    #[test]
    fn initial_prompt_issue_first() {
        let agent = CursorAgent::new();
        let mut session = fake_session();
        session.issue_id = Some("7".into());
        session.issue_url = Some("https://github.com/acme/repo/issues/7".into());
        session.task = "Add dark mode".into();

        let prompt = agent.initial_prompt(&session);
        assert!(prompt.contains("issue #7"));
        assert!(prompt.contains("https://github.com/acme/repo/issues/7"));
        assert!(prompt.contains("Add dark mode"));
        assert!(prompt.contains("open a pull request"));
    }

    #[test]
    fn initial_prompt_with_rules_prepends_rules() {
        let agent = CursorAgent {
            rules: Some("Always run tests before committing.".into()),
        };
        let prompt = agent.initial_prompt(&fake_session());
        assert!(prompt.starts_with("Always run tests"));
        assert!(prompt.contains("---"));
        assert!(prompt.contains("fix the bug"));
    }

    #[test]
    fn from_config_reads_inline_rules() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: Some("custom cursor rules".into()),
            rules_file: None,
        };
        let agent = CursorAgent::from_config(&config);
        let prompt = agent.initial_prompt(&fake_session());
        assert!(prompt.contains("custom cursor rules"));
    }

    #[test]
    fn from_config_no_rules() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: None,
            rules_file: None,
        };
        let agent = CursorAgent::from_config(&config);
        assert_eq!(agent.initial_prompt(&fake_session()), "fix the bug");
    }

    // ---- activity detection ----

    #[test]
    fn detect_activity_no_workspace_returns_ready() {
        let ws = std::env::temp_dir().join("ao-cursor-no-ws");
        std::fs::create_dir_all(&ws).unwrap();

        // No .cursor dir, no git commits → fallback Ready.
        let result = detect_cursor_activity(&ws).unwrap();
        assert_eq!(result, ActivityState::Ready);

        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn detect_activity_fresh_chat_file_returns_active() {
        let ws = std::env::temp_dir().join("ao-cursor-active-chat");
        let cursor_dir = ws.join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        std::fs::write(cursor_dir.join("chat.md"), "# Session\nHello").unwrap();

        let result = detect_cursor_activity(&ws).unwrap();
        assert_eq!(result, ActivityState::Active);

        std::fs::remove_dir_all(&ws).ok();
    }
}
