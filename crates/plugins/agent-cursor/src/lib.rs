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
//! 2. Cursor log artifacts (if present) — `.cursor/logs/*` mtime.
//! 3. Workspace git activity — `.git/index` mtime (captures edits even without commits).
//! 4. Recent git commits in the workspace — if any within 60s, agent is active.
//! 5. Fallback: `ActivityState::Ready` (runtime liveness covers process exit).
//!
//! ## System prompt
//!
//! Cursor has no `--append-system-prompt` equivalent, so agent rules are
//! delivered by prepending them to the user prompt (see
//! [`CursorAgent::system_prompt`] and the CLI spawn flow).
//!
//! ## Cost tracking
//!
//! Cursor stores chat history in per-project SQLite databases under
//! `~/.cursor/chats/<hash>/<uuid>/store.db` and does not expose token /
//! cost metadata via its CLI. `cost_estimate` is intentionally left at
//! the trait default (`None`) — matching the TS reference, which also
//! reports cost as unsupported.

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
    /// Model override passed via `--model`.
    model: Option<String>,
}

impl CursorAgent {
    pub fn new() -> Self {
        Self {
            rules: None,
            model: None,
        }
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
        Self {
            rules,
            model: config.model.clone(),
        }
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
        let mut cmd = "agent --force --sandbox disabled --approve-mcps".to_string();

        if let Some(ref model) = self.model {
            // Shell-escape model value for safety (Cursor TS plugin does the same).
            let escaped = model.replace('\'', "'\\''");
            cmd.push_str(&format!(" --model '{escaped}'"));
        }

        cmd
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

    fn system_prompt(&self) -> Option<String> {
        // Cursor has no `--append-system-prompt` flag, so rules are
        // delivered by prepending them to the user prompt. Callers that
        // build a prompt externally (e.g. `ao-rs spawn`) should prepend
        // this before sending. Matches the TS plugin's behavior where
        // `getLaunchCommand` concatenates `systemPrompt + "\n\n" + prompt`.
        self.rules
            .as_ref()
            .map(|r| r.trim())
            .filter(|r| !r.is_empty())
            .map(|r| r.to_string())
    }

    fn initial_prompt(&self, session: &Session) -> String {
        // NOTE: The CLI spawn flow uses `prompt_builder::build_prompt()` for
        // richer 3-layer prompts. This is a backward-compat fallback for
        // callers (dashboard, restore) that send a single composed message.
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
    if let Some(state) = state_from_mtime(workspace_path.join(".cursor").join("chat.md"))? {
        return Ok(state);
    }

    // 2. Check Cursor log artifacts (if any).
    if let Some(state) = detect_cursor_log_activity(workspace_path)? {
        return Ok(state);
    }

    // 3. Check git activity via `.git/index` mtime (captures "work happened" even without commits).
    if let Some(state) = detect_git_index_activity(workspace_path)? {
        return Ok(state);
    }

    // 4. Check for recent git commits.
    if has_recent_commits(workspace_path) {
        return Ok(ActivityState::Active);
    }

    // 5. Fallback — no cursor artifacts, agent may have just started.
    Ok(ActivityState::Ready)
}

fn age_to_state(age_secs: u64) -> ActivityState {
    if age_secs <= ACTIVE_WINDOW_SECS {
        ActivityState::Active
    } else if age_secs <= IDLE_THRESHOLD_SECS {
        ActivityState::Ready
    } else {
        ActivityState::Idle
    }
}

fn state_from_mtime(path: impl AsRef<Path>) -> Result<Option<ActivityState>> {
    let path = path.as_ref();
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(None);
    };
    let Ok(modified) = metadata.modified() else {
        // Platform doesn't support mtime — fall back to Ready rather
        // than silently reporting Active with a faked timestamp.
        return Ok(Some(ActivityState::Ready));
    };
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default()
        .as_secs();
    Ok(Some(age_to_state(age)))
}

fn detect_cursor_log_activity(workspace_path: &Path) -> Result<Option<ActivityState>> {
    let cursor_dir = workspace_path.join(".cursor");
    let logs_dir = cursor_dir.join("logs");
    let Ok(entries) = std::fs::read_dir(&logs_dir) else {
        return Ok(None);
    };

    let mut newest: Option<std::time::SystemTime> = None;
    // Bound cost even if logs dir is large.
    for (i, entry) in entries.flatten().enumerate() {
        if i >= 200 {
            break;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        newest = Some(match newest {
            Some(prev) if prev > modified => prev,
            _ => modified,
        });
    }

    let Some(newest) = newest else {
        return Ok(None);
    };
    let age = std::time::SystemTime::now()
        .duration_since(newest)
        .unwrap_or_default()
        .as_secs();
    Ok(Some(age_to_state(age)))
}

fn detect_git_index_activity(workspace_path: &Path) -> Result<Option<ActivityState>> {
    // Fast path: `.git/index` exists directly under worktree.
    let direct = workspace_path.join(".git").join("index");
    if let Some(state) = state_from_mtime(&direct)? {
        return Ok(Some(state));
    }

    // Worktrees sometimes have `.git` as a file pointing at the real git dir.
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(workspace_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(o) = output else {
        return Ok(None);
    };
    if !o.status.success() {
        return Ok(None);
    }
    let git_dir = String::from_utf8_lossy(&o.stdout).trim().to_string();
    if git_dir.is_empty() {
        return Ok(None);
    }
    let git_dir = if Path::new(&git_dir).is_absolute() {
        std::path::PathBuf::from(git_dir)
    } else {
        workspace_path.join(git_dir)
    };
    let idx = git_dir.join("index");
    state_from_mtime(&idx)
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
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: None,
            spawned_by: None,
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
            model: None,
        };
        let prompt = agent.initial_prompt(&fake_session());
        assert!(prompt.starts_with("Always run tests"));
        assert!(prompt.contains("---"));
        assert!(prompt.contains("fix the bug"));
    }

    // ---- system_prompt (parity with TS systemPrompt injection) ----

    #[test]
    fn system_prompt_none_when_no_rules() {
        let agent = CursorAgent::new();
        assert!(agent.system_prompt().is_none());
    }

    #[test]
    fn system_prompt_returns_rules_when_configured() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: Some("Always run tests before committing.".into()),
            rules_file: None,
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let agent = CursorAgent::from_config(&config);
        assert_eq!(
            agent.system_prompt().as_deref(),
            Some("Always run tests before committing.")
        );
    }

    #[test]
    fn system_prompt_none_when_rules_blank() {
        // Whitespace-only rules shouldn't round-trip as a system prompt —
        // matches the TS plugin's `if (config.systemPrompt)` truthy check.
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: Some("   \n  \t".into()),
            rules_file: None,
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let agent = CursorAgent::from_config(&config);
        assert!(agent.system_prompt().is_none());
    }

    // ---- --model flag (parity with TS getLaunchCommand) ----

    #[test]
    fn launch_command_no_model_flag_by_default() {
        let agent = CursorAgent::new();
        let cmd = agent.launch_command(&fake_session());
        assert!(!cmd.contains("--model"));
    }

    #[test]
    fn launch_command_includes_model_when_set() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: None,
            rules_file: None,
            model: Some("gpt-4o".into()),
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let agent = CursorAgent::from_config(&config);
        let cmd = agent.launch_command(&fake_session());
        assert!(cmd.contains("--model 'gpt-4o'"));
    }

    #[test]
    fn launch_command_model_is_shell_escaped() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: None,
            rules_file: None,
            model: Some("it's-a-model".into()),
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let agent = CursorAgent::from_config(&config);
        let cmd = agent.launch_command(&fake_session());
        // Single quotes escape via close-escape-reopen.
        assert!(cmd.contains(r"--model 'it'\''s-a-model'"));
    }

    #[test]
    fn from_config_reads_inline_rules() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: Some("custom cursor rules".into()),
            rules_file: None,
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
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
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
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

    #[test]
    fn detect_activity_falls_back_to_cursor_logs_when_chat_missing() {
        let ws = std::env::temp_dir().join("ao-cursor-active-logs");
        let logs_dir = ws.join(".cursor").join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::write(logs_dir.join("cursor-agent.log"), "hello").unwrap();

        let result = detect_cursor_activity(&ws).unwrap();
        assert_eq!(result, ActivityState::Active);

        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn detect_activity_falls_back_to_git_index_mtime_when_no_cursor_artifacts() {
        let ws = std::env::temp_dir().join("ao-cursor-active-git-index");
        let git_dir = ws.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("index"), "fake index").unwrap();

        let result = detect_cursor_activity(&ws).unwrap();
        assert_eq!(result, ActivityState::Active);

        std::fs::remove_dir_all(&ws).ok();
    }
}
