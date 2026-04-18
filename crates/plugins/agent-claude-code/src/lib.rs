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

use ao_core::{
    default_agent_rules, ActivityState, Agent, AgentConfig, CostEstimate, Result, Session,
};
use async_trait::async_trait;
use std::io::{BufRead, Seek};
use std::path::PathBuf;

pub struct ClaudeCodeAgent {
    /// Agent rules injected via --append-system-prompt.
    rules: Option<String>,
    /// Model override passed via --model.
    model: Option<String>,
}

impl ClaudeCodeAgent {
    pub fn new() -> Self {
        Self {
            rules: None,
            model: None,
        }
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
        Self {
            rules,
            model: config.model.clone(),
        }
    }

    /// Create with default dev-lifecycle rules.
    pub fn with_default_rules() -> Self {
        Self {
            rules: Some(default_agent_rules().to_string()),
            model: None,
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

        if let Some(ref model) = self.model {
            cmd.push_str(&format!(" --model {model}"));
        }

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
        // NOTE: The CLI spawn flow uses `prompt_builder::build_prompt()` for
        // richer 3-layer prompts (session context + issue context + directive).
        // This method is a backward-compat fallback for callers that don't
        // have access to the full Issue / ProjectConfig context.
        //
        // Issue-first spawns get structured context so the agent knows its
        // branch, the issue source, and is explicitly told to open a PR.
        // Prompt-first spawns (`--task`) get the raw task as-is — the user
        // controls the framing themselves.
        if let Some(ref id) = session.issue_id {
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
        }
    }

    async fn detect_activity(&self, session: &Session) -> Result<ActivityState> {
        let Some(ref ws) = session.workspace_path else {
            return Ok(ActivityState::Ready);
        };
        // JSONL reads are blocking file I/O — run off the executor thread.
        let ws = ws.clone();
        tokio::task::spawn_blocking(move || detect_activity_from_jsonl(&ws))
            .await
            .map_err(|e| ao_core::AoError::Other(format!("detect_activity panicked: {e}")))?
    }

    async fn cost_estimate(&self, session: &Session) -> Result<Option<CostEstimate>> {
        let Some(ref ws) = session.workspace_path else {
            return Ok(None);
        };
        // JSONL discovery + parsing is blocking file I/O — run off the
        // executor thread to avoid starving other async tasks.
        let ws = ws.clone();
        let result = tokio::task::spawn_blocking(move || {
            let path = find_session_jsonl(&ws)?;
            parse_cost_from_jsonl(&path)
        })
        .await
        .unwrap_or(None);
        Ok(result)
    }
}

// ---- Claude Code session JSONL discovery and parsing ----

/// Claude Code stores session data in `~/.claude/projects/{encoded-path}/`.
/// The path encoding replaces `/` and `.` with `-`.
/// E.g. `/Users/foo/bar` → `-Users-foo-bar`.
fn encode_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace(['/', '.'], "-")
}

/// Find the most recent JSONL session file for a workspace path.
/// Claude Code writes to `~/.claude/projects/{encoded}/sessions/{uuid}.jsonl`.
fn find_session_jsonl(workspace_path: &std::path::Path) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let encoded = encode_path(workspace_path);
    let sessions_dir = PathBuf::from(&home)
        .join(".claude")
        .join("projects")
        .join(&encoded)
        .join("sessions");

    if !sessions_dir.is_dir() {
        return None;
    }

    // Pick the most recently modified .jsonl file.
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    let entries = std::fs::read_dir(&sessions_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Ok(meta) = path.metadata() {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(_, t)| mtime > *t) {
                    best = Some((path, mtime));
                }
            }
        }
    }
    best.map(|(p, _)| p)
}

// ---- Activity detection constants ----

/// If the JSONL file hasn't been modified in this many seconds, the agent is
/// considered idle. 5 minutes matches the TS reference's default.
const IDLE_THRESHOLD_SECS: u64 = 300;

/// How many bytes to read from the tail of the JSONL file. 64 KB is enough
/// to capture the last few conversation turns without reading the whole file.
const TAIL_READ_BYTES: u64 = 64 * 1024;

// ---- Activity detection ----

/// Determine agent activity by tailing the Claude Code JSONL session file.
///
/// Strategy:
///   1. File mtime older than `IDLE_THRESHOLD_SECS` → `Idle`.
///   2. Seek to last ~64 KB, find the last `assistant` or `user` entry.
///   3. Map `stop_reason`:
///      - `"end_turn"` → `Ready` (agent finished, waiting for input)
///      - `"tool_use"` / `null` → `Active` (agent running tools or streaming)
///   4. Last entry is `user` → `Active` (tool results or human input flowing,
///      agent will respond).
///   5. No JSONL file or no entries → `Ready` (agent just started).
fn detect_activity_from_jsonl(workspace_path: &std::path::Path) -> ao_core::Result<ActivityState> {
    let Some(path) = find_session_jsonl(workspace_path) else {
        // No JSONL yet — agent just started or hasn't written anything.
        return Ok(ActivityState::Ready);
    };

    let metadata = std::fs::metadata(&path)?;

    // Check file modification time for idle detection.
    // If mtime is unavailable (exotic platform), assume fresh to avoid false
    // Idle — the lifecycle's stuck-detection clock handles prolonged inactivity.
    let modified = metadata
        .modified()
        .unwrap_or_else(|_| std::time::SystemTime::now());
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();

    if age.as_secs() > IDLE_THRESHOLD_SECS {
        return Ok(ActivityState::Idle);
    }

    // Read the tail of the file to find the last meaningful entry.
    // Note: does not detect process exit — that is handled by the runtime
    // probe (`Runtime::is_alive`) in the lifecycle's step 1.
    let file_len = metadata.len();
    let read_start = file_len.saturating_sub(TAIL_READ_BYTES);

    let mut file = std::fs::File::open(&path)?;
    if read_start > 0 {
        file.seek(std::io::SeekFrom::Start(read_start))?;
    }

    let mut reader = std::io::BufReader::new(file);

    // If we seeked into the middle of a line, discard the partial fragment
    // so the parse loop only sees complete JSON lines.
    if read_start > 0 {
        let mut _discard = String::new();
        let _ = reader.read_line(&mut _discard);
    }

    let mut last_entry: Option<serde_json::Value> = None;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // Only track interaction entries — `system`, `queue-operation`, etc.
        // don't reflect agent activity.
        let entry_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if matches!(entry_type, "assistant" | "user") {
            last_entry = Some(v);
        }
    }

    let Some(entry) = last_entry else {
        return Ok(ActivityState::Ready);
    };

    Ok(classify_entry(&entry))
}

/// Map a single JSONL entry to an `ActivityState`.
fn classify_entry(entry: &serde_json::Value) -> ActivityState {
    let entry_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match entry_type {
        "assistant" => {
            let stop_reason = entry
                .get("message")
                .and_then(|m| m.get("stop_reason"))
                .and_then(|s| s.as_str());

            match stop_reason {
                // end_turn: model chose to stop — waiting for user input.
                Some("end_turn") => ActivityState::Ready,
                // tool_use: model is invoking tools — more work coming.
                // null / streaming partial: model is mid-response.
                Some("tool_use") | None => ActivityState::Active,
                // Any other stop reason (shouldn't happen, but be safe).
                _ => ActivityState::Active,
            }
        }
        // user entry (tool_result or human message) → model will respond.
        _ => ActivityState::Active,
    }
}

// ---- Cost estimation ----

/// Pricing constants (USD per million tokens). Sonnet 4 pricing.
const INPUT_PRICE: f64 = 3.0;
const OUTPUT_PRICE: f64 = 15.0;
const CACHE_READ_PRICE: f64 = 0.30;
const CACHE_CREATION_PRICE: f64 = 3.75;

/// Parse all `"type":"assistant"` lines from a JSONL file and aggregate
/// their `usage` fields into a single `CostEstimate`.
fn parse_cost_from_jsonl(path: &std::path::Path) -> Option<CostEstimate> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut cache_creation_tokens = 0u64;

    for line in reader.lines() {
        // Skip I/O errors (e.g. mid-file truncation) rather than
        // discarding all accumulated tokens.
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only assistant messages carry usage data.
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }

        if let Some(usage) = v.get("usage") {
            input_tokens += usage
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            output_tokens += usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            cache_read_tokens += usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            cache_creation_tokens += usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        }
    }

    if input_tokens == 0 && output_tokens == 0 {
        return None;
    }

    let cost_usd = (input_tokens as f64 * INPUT_PRICE
        + output_tokens as f64 * OUTPUT_PRICE
        + cache_read_tokens as f64 * CACHE_READ_PRICE
        + cache_creation_tokens as f64 * CACHE_CREATION_PRICE)
        / 1_000_000.0;

    Some(CostEstimate {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        cost_usd: Some(cost_usd),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{now_ms, SessionId, SessionStatus};
    use std::io::Write;
    use std::path::PathBuf;

    fn fake_session() -> Session {
        Session {
            id: SessionId("test-id".into()),
            project_id: "demo".into(),
            status: SessionStatus::Spawning,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "feat-x".into(),
            task: "fix the typo in README".into(),
            workspace_path: Some(PathBuf::from("/tmp/demo")),
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
            last_merge_conflict_dispatched: None,
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
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
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
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
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
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let agent = ClaudeCodeAgent::from_config(&config);
        assert_eq!(
            agent.launch_command(&fake_session()),
            "claude --dangerously-skip-permissions"
        );
    }

    #[test]
    fn from_config_model_appends_model_flag() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: None,
            rules_file: None,
            model: Some("claude-opus-4-7-20250514".into()),
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let agent = ClaudeCodeAgent::from_config(&config);
        assert_eq!(
            agent.launch_command(&fake_session()),
            "claude --dangerously-skip-permissions --model claude-opus-4-7-20250514"
        );
    }

    #[test]
    fn from_config_model_and_rules_order() {
        let config = AgentConfig {
            permissions: "permissionless".into(),
            rules: Some("my rules".into()),
            rules_file: None,
            model: Some("claude-sonnet-4-6".into()),
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let agent = ClaudeCodeAgent::from_config(&config);
        let cmd = agent.launch_command(&fake_session());
        // --model comes before --append-system-prompt
        let model_pos = cmd.find("--model").unwrap();
        let prompt_pos = cmd.find("--append-system-prompt").unwrap();
        assert!(model_pos < prompt_pos);
    }

    #[test]
    fn no_model_flag_when_model_not_set() {
        let agent = ClaudeCodeAgent::new();
        let cmd = agent.launch_command(&fake_session());
        assert!(!cmd.contains("--model"));
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
    fn initial_prompt_returns_task_for_prompt_first() {
        let agent = ClaudeCodeAgent::new();
        // --task mode: raw task, no structured wrapper.
        assert_eq!(
            agent.initial_prompt(&fake_session()),
            "fix the typo in README"
        );
    }

    #[test]
    fn initial_prompt_adds_context_for_issue_first() {
        let agent = ClaudeCodeAgent::new();
        let mut session = fake_session();
        session.issue_id = Some("42".into());
        session.issue_url = Some("https://github.com/acme/repo/issues/42".into());
        session.branch = "ao-abc123-feat-issue-42".into();
        session.task = "Add hello world endpoint\n\nThe /hello route should return 200 OK.".into();

        let prompt = agent.initial_prompt(&session);
        assert!(prompt.contains("issue #42"));
        assert!(prompt.contains("ao-abc123-feat-issue-42"));
        assert!(prompt.contains("https://github.com/acme/repo/issues/42"));
        assert!(prompt.contains("Add hello world endpoint"));
        assert!(prompt.contains("open a pull request"));
    }

    #[test]
    fn initial_prompt_omits_url_line_when_no_issue_url() {
        let agent = ClaudeCodeAgent::new();
        let mut session = fake_session();
        session.issue_id = Some("7".into());
        session.issue_url = None;
        session.task = "Fix the bug".into();

        let prompt = agent.initial_prompt(&session);
        assert!(prompt.contains("issue #7"));
        assert!(!prompt.contains("Issue URL:"));
        assert!(prompt.contains("open a pull request"));
    }

    // ---- classify_entry unit tests ----

    #[test]
    fn classify_assistant_end_turn_is_ready() {
        let entry: serde_json::Value =
            serde_json::from_str(r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#)
                .unwrap();
        assert_eq!(classify_entry(&entry), ActivityState::Ready);
    }

    #[test]
    fn classify_assistant_tool_use_is_active() {
        let entry: serde_json::Value =
            serde_json::from_str(r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}"#)
                .unwrap();
        assert_eq!(classify_entry(&entry), ActivityState::Active);
    }

    #[test]
    fn classify_assistant_null_stop_reason_is_active() {
        // Streaming partial — stop_reason is null in JSON.
        let entry: serde_json::Value =
            serde_json::from_str(r#"{"type":"assistant","message":{"stop_reason":null}}"#).unwrap();
        assert_eq!(classify_entry(&entry), ActivityState::Active);
    }

    #[test]
    fn classify_assistant_no_message_is_active() {
        // Defensive: assistant entry without message object.
        let entry: serde_json::Value = serde_json::from_str(r#"{"type":"assistant"}"#).unwrap();
        assert_eq!(classify_entry(&entry), ActivityState::Active);
    }

    #[test]
    fn classify_user_entry_is_active() {
        // User entry (tool_result or human input) means agent will respond.
        let entry: serde_json::Value = serde_json::from_str(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result"}]}}"#,
        )
        .unwrap();
        assert_eq!(classify_entry(&entry), ActivityState::Active);
    }

    // ---- detect_activity_from_jsonl integration tests ----

    /// Build a fake `~/.claude/projects/{encoded}/sessions/` tree so
    /// `find_session_jsonl` discovers the test file. Returns the workspace
    /// path (the key `detect_activity_from_jsonl` uses).
    fn setup_jsonl_env(label: &str, lines: &[&str]) -> (PathBuf, PathBuf) {
        let workspace = std::env::temp_dir().join(format!("ao-activity-ws-{label}"));
        std::fs::create_dir_all(&workspace).unwrap();

        let encoded = encode_path(&workspace);
        let home = std::env::var("HOME").unwrap();
        let sessions_dir = PathBuf::from(&home)
            .join(".claude")
            .join("projects")
            .join(&encoded)
            .join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let jsonl_path = sessions_dir.join("test-detect-activity.jsonl");
        let mut f = std::fs::File::create(&jsonl_path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }

        (workspace, jsonl_path)
    }

    fn teardown_jsonl_env(workspace: &std::path::Path) {
        let encoded = encode_path(workspace);
        let home = std::env::var("HOME").unwrap();
        let sessions_dir = PathBuf::from(&home)
            .join(".claude")
            .join("projects")
            .join(&encoded);
        std::fs::remove_dir_all(&sessions_dir).ok();
        std::fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn detect_activity_no_jsonl_returns_ready() {
        let workspace = std::env::temp_dir().join("ao-activity-no-jsonl");
        std::fs::create_dir_all(&workspace).unwrap();
        let result = detect_activity_from_jsonl(&workspace).unwrap();
        assert_eq!(result, ActivityState::Ready);
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn detect_activity_end_turn_returns_ready() {
        let (workspace, _jsonl) = setup_jsonl_env(
            "end-turn",
            &[
                r#"{"type":"user","message":{"role":"user"}}"#,
                r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result"}]}}"#,
                r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#,
            ],
        );
        let result = detect_activity_from_jsonl(&workspace).unwrap();
        assert_eq!(result, ActivityState::Ready);
        teardown_jsonl_env(&workspace);
    }

    #[test]
    fn detect_activity_tool_use_returns_active() {
        let (workspace, _jsonl) = setup_jsonl_env(
            "tool-use",
            &[
                r#"{"type":"user","message":{"role":"user"}}"#,
                r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}"#,
            ],
        );
        let result = detect_activity_from_jsonl(&workspace).unwrap();
        assert_eq!(result, ActivityState::Active);
        teardown_jsonl_env(&workspace);
    }

    #[test]
    fn detect_activity_user_last_returns_active() {
        let (workspace, _jsonl) = setup_jsonl_env(
            "user-last",
            &[
                r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result"}]}}"#,
            ],
        );
        let result = detect_activity_from_jsonl(&workspace).unwrap();
        assert_eq!(result, ActivityState::Active);
        teardown_jsonl_env(&workspace);
    }

    #[test]
    fn detect_activity_stale_file_returns_idle() {
        let (workspace, jsonl) = setup_jsonl_env(
            "stale",
            &[r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#],
        );
        // Backdate the file modification time by IDLE_THRESHOLD + 60s.
        let old_time = filetime::FileTime::from_unix_time(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                - IDLE_THRESHOLD_SECS as i64
                - 60,
            0,
        );
        filetime::set_file_mtime(&jsonl, old_time).unwrap();

        let result = detect_activity_from_jsonl(&workspace).unwrap();
        assert_eq!(result, ActivityState::Idle);
        teardown_jsonl_env(&workspace);
    }

    #[test]
    fn detect_activity_skips_system_entries() {
        // System and queue-operation entries should not affect activity.
        let (workspace, _jsonl) = setup_jsonl_env(
            "skip-system",
            &[
                r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#,
                r#"{"type":"system","message":{}}"#,
                r#"{"type":"queue-operation"}"#,
            ],
        );
        // Last interaction entry is the assistant end_turn → Ready.
        let result = detect_activity_from_jsonl(&workspace).unwrap();
        assert_eq!(result, ActivityState::Ready);
        teardown_jsonl_env(&workspace);
    }

    // ---- JSONL cost parsing tests ----

    fn write_jsonl(dir: &std::path::Path, lines: &[&str]) -> PathBuf {
        let path = dir.join("test-session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    #[test]
    fn parse_cost_from_jsonl_aggregates_usage() {
        let dir = std::env::temp_dir().join(format!("ao-jsonl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let path = write_jsonl(
            &dir,
            &[
                r#"{"type":"human","message":"hello"}"#,
                r#"{"type":"assistant","usage":{"input_tokens":1000,"output_tokens":200,"cache_read_input_tokens":500,"cache_creation_input_tokens":100}}"#,
                r#"{"type":"assistant","usage":{"input_tokens":2000,"output_tokens":300,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#,
            ],
        );

        let cost = parse_cost_from_jsonl(&path).unwrap();
        assert_eq!(cost.input_tokens, 3000);
        assert_eq!(cost.output_tokens, 500);
        assert_eq!(cost.cache_read_tokens, 500);
        assert_eq!(cost.cache_creation_tokens, 100);
        // (3000*3 + 500*15 + 500*0.3 + 100*3.75) / 1_000_000
        let expected = (9000.0 + 7500.0 + 150.0 + 375.0) / 1_000_000.0;
        let usd = cost.cost_usd.expect("claude-code always reports USD");
        assert!((usd - expected).abs() < 1e-10);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_cost_empty_file_returns_none() {
        let dir = std::env::temp_dir().join(format!("ao-jsonl-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_jsonl(&dir, &[]);
        assert!(parse_cost_from_jsonl(&path).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_cost_no_assistant_lines_returns_none() {
        let dir = std::env::temp_dir().join(format!("ao-jsonl-noast-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_jsonl(&dir, &[r#"{"type":"human","message":"hi"}"#]);
        assert!(parse_cost_from_jsonl(&path).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_cost_tolerates_malformed_lines() {
        let dir = std::env::temp_dir().join(format!("ao-jsonl-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_jsonl(
            &dir,
            &[
                "not json at all",
                r#"{"type":"assistant","usage":{"input_tokens":100,"output_tokens":50}}"#,
            ],
        );
        let cost = parse_cost_from_jsonl(&path).unwrap();
        assert_eq!(cost.input_tokens, 100);
        assert_eq!(cost.output_tokens, 50);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn encode_path_strips_leading_slash_and_replaces_separators() {
        let p = std::path::Path::new("/Users/foo/my.project");
        assert_eq!(encode_path(p), "-Users-foo-my-project");
    }
}
