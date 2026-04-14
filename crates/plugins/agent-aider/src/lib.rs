//! Aider agent plugin.
//!
//! Launches the `aider` CLI in interactive mode and delivers the initial task
//! via post-launch `send_message` (same flow as Claude Code).
//!
//! ## Activity detection
//!
//! Aider writes local history files in the workspace by default:
//! - `.aider.chat.history.md`
//! - `.aider.input.history`
//!
//! Detection mirrors the TS plugin strategy:
//! 1. If `.aider.chat.history.md` mtime is fresh → Active/Ready/Idle.
//! 2. Else if `.aider.input.history` mtime is fresh → Active/Ready/Idle.
//! 3. Else if git has recent commits → Active.
//! 4. Fallback: Ready.

use ao_core::{ActivityState, Agent, AgentConfig, Result, Session};
use async_trait::async_trait;
use std::path::Path;

/// If the history file was modified within this many seconds, consider the
/// agent actively working.
const ACTIVE_WINDOW_SECS: u64 = 30;

/// If the history file was modified within this many seconds, consider the
/// agent alive and ready (but not actively writing right now).
const IDLE_THRESHOLD_SECS: u64 = 300;

pub struct AiderAgent {
    /// Rules prepended to the task. Aider doesn't expose a stable system-prompt
    /// flag across providers, so we deliver rules as part of the first message.
    rules: Option<String>,
}

impl AiderAgent {
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

impl Default for AiderAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for AiderAgent {
    fn launch_command(&self, _session: &Session) -> String {
        // Keep it minimal: users can configure model/provider via env or aider config.
        // `--yes` would auto-accept all changes; we intentionally don't default to it.
        "aider".to_string()
    }

    fn environment(&self, session: &Session) -> Vec<(String, String)> {
        vec![("AO_SESSION_ID".to_string(), session.id.to_string())]
    }

    fn initial_prompt(&self, session: &Session) -> String {
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
        let ws = ws.clone();
        tokio::task::spawn_blocking(move || detect_aider_activity(&ws))
            .await
            .map_err(|e| ao_core::AoError::Other(format!("detect_activity panicked: {e}")))?
    }
}

// ---------------------------------------------------------------------------
// Activity detection
// ---------------------------------------------------------------------------

fn detect_aider_activity(workspace_path: &Path) -> Result<ActivityState> {
    let chat = workspace_path.join(".aider.chat.history.md");
    if let Ok(s) = classify_mtime(&chat) {
        return Ok(s);
    }

    let input = workspace_path.join(".aider.input.history");
    if let Ok(s) = classify_mtime(&input) {
        return Ok(s);
    }

    if has_recent_commits(workspace_path) {
        return Ok(ActivityState::Active);
    }

    Ok(ActivityState::Ready)
}

fn classify_mtime(path: &Path) -> std::io::Result<ActivityState> {
    let meta = std::fs::metadata(path)?;
    let modified = meta.modified()?;
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();

    if age.as_secs() <= ACTIVE_WINDOW_SECS {
        Ok(ActivityState::Active)
    } else if age.as_secs() <= IDLE_THRESHOLD_SECS {
        Ok(ActivityState::Ready)
    } else {
        Ok(ActivityState::Idle)
    }
}

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
            id: SessionId("aider-test".into()),
            project_id: "demo".into(),
            status: SessionStatus::Working,
            agent: "aider".into(),
            agent_config: None,
            branch: "ao-abc123-feat-test".into(),
            task: "fix the bug".into(),
            workspace_path: Some(PathBuf::from("/tmp/aider-demo")),
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
    fn launch_command_is_aider() {
        let agent = AiderAgent::new();
        assert_eq!(agent.launch_command(&fake_session()), "aider");
    }

    #[test]
    fn environment_includes_session_id() {
        let agent = AiderAgent::new();
        let env = agent.environment(&fake_session());
        assert!(env
            .iter()
            .any(|(k, v)| k == "AO_SESSION_ID" && v == "aider-test"));
    }

    #[test]
    fn initial_prompt_task_first() {
        let agent = AiderAgent::new();
        assert_eq!(agent.initial_prompt(&fake_session()), "fix the bug");
    }

    #[test]
    fn initial_prompt_issue_first() {
        let agent = AiderAgent::new();
        let mut session = fake_session();
        session.issue_id = Some("22".into());
        session.issue_url = Some("https://github.com/org/repo/issues/22".into());
        session.task = "Port plugin".into();
        let p = agent.initial_prompt(&session);
        assert!(p.contains("issue #22"));
        assert!(p.contains("https://github.com/org/repo/issues/22"));
        assert!(p.contains("Port plugin"));
        assert!(p.contains("open a pull request"));
    }

    #[test]
    fn initial_prompt_with_rules_prepends_rules() {
        let agent = AiderAgent {
            rules: Some("Always run tests.".into()),
        };
        let p = agent.initial_prompt(&fake_session());
        assert!(p.starts_with("Always run tests."));
        assert!(p.contains("---"));
        assert!(p.contains("fix the bug"));
    }

    #[test]
    fn from_config_reads_inline_rules() {
        let cfg = AgentConfig {
            permissions: "permissionless".into(),
            rules: Some("custom rules".into()),
            rules_file: None,
        };
        let agent = AiderAgent::from_config(&cfg);
        let p = agent.initial_prompt(&fake_session());
        assert!(p.contains("custom rules"));
    }

    #[test]
    fn detect_activity_no_files_returns_ready() {
        let ws = std::env::temp_dir().join("ao-aider-no-files");
        std::fs::create_dir_all(&ws).unwrap();
        let s = detect_aider_activity(&ws).unwrap();
        assert_eq!(s, ActivityState::Ready);
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn detect_activity_fresh_chat_file_returns_active() {
        let ws = std::env::temp_dir().join("ao-aider-fresh-chat");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join(".aider.chat.history.md"), "hi").unwrap();
        let s = detect_aider_activity(&ws).unwrap();
        assert_eq!(s, ActivityState::Active);
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn detect_activity_stale_chat_file_returns_idle() {
        let ws = std::env::temp_dir().join("ao-aider-stale-chat");
        std::fs::create_dir_all(&ws).unwrap();
        let p = ws.join(".aider.chat.history.md");
        std::fs::write(&p, "hi").unwrap();

        let old_time = filetime::FileTime::from_unix_time(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                - IDLE_THRESHOLD_SECS as i64
                - 60,
            0,
        );
        filetime::set_file_mtime(&p, old_time).unwrap();

        let s = detect_aider_activity(&ws).unwrap();
        assert_eq!(s, ActivityState::Idle);
        std::fs::remove_dir_all(&ws).ok();
    }
}
