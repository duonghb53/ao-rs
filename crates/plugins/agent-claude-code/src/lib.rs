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
use std::io::BufRead;
use std::path::PathBuf;

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
                if best.as_ref().map_or(true, |(_, t)| mtime > *t) {
                    best = Some((path, mtime));
                }
            }
        }
    }
    best.map(|(p, _)| p)
}

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
        cost_usd,
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
            branch: "feat-x".into(),
            task: "fix the typo in README".into(),
            workspace_path: Some(PathBuf::from("/tmp/demo")),
            runtime_handle: None,
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
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
        assert!((cost.cost_usd - expected).abs() < 1e-10);

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
