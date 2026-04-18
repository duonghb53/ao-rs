//! Codex CLI agent plugin.
//!
//! Launches `codex` (interactive TUI) in a runtime session and delivers the
//! task via post-launch `send_message` (same delivery strategy as other agents).
//!
//! ## Launch strategy
//!
//! We launch the interactive `codex` TUI rather than `codex exec`: the
//! orchestrator keeps the process alive across multiple turns and uses
//! `Runtime::send_message` to feed follow-ups, which `codex exec` (a
//! single-shot runner) cannot support.
//!
//! When an `AgentConfig` is available, the command mirrors
//! `packages/plugins/agent-codex/src/index.ts` in ao-ts:
//! permission-mode-specific approval flags (`--dangerously-bypass-
//! approvals-and-sandbox`, `--ask-for-approval never|untrusted`), an
//! optional `--model` flag, and `-c check_for_update_on_startup=false`
//! so the TUI never stalls on an interactive update prompt. With no
//! config we keep the historical `--full-auto` preset to avoid
//! regressing existing users.
//!
//! ## Activity detection
//!
//! Codex stores local state under `CODEX_HOME` (defaults to `~/.codex`).
//! We use a multi-fallback heuristic:
//! 1. mtime of `CODEX_HOME/history.jsonl`
//! 2. mtime of `CODEX_HOME/log/codex-tui.log`
//! 3. recent git commits in the workspace (within 60s)
//! 4. fallback `Ready`
//!
//! This mirrors the other plugins' "artifact + git fallback" approach.
//!
//! ## Cost tracking
//!
//! Codex emits token counts in its JSONL logs but no USD figure, and
//! the CLI doesn't expose a stable pricing API. We aggregate the tokens
//! and leave `cost_usd` as `None` rather than emitting a placeholder
//! `0.0` — the status command renders `-` for unknown cost, which is
//! honest reporting vs. "this session was free".

use ao_core::{shell::shell_escape, ActivityState, Agent, AgentConfig, CostEstimate, Result, Session};
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// Idle threshold: no evidence of activity beyond this window → Idle.
const IDLE_THRESHOLD_SECS: u64 = 300;
/// Active window: evidence within this window → Active.
const ACTIVE_WINDOW_SECS: u64 = 30;

pub struct CodexAgent {
    /// Rules prepended to the prompt. Codex supports project instructions via
    /// files, but the orchestrator's `AgentConfig` rules are delivered as text.
    rules: Option<String>,
    /// When `Some`, drives permission-mode-specific approval flags to
    /// match ao-ts. `None` keeps the historical `--full-auto` launch
    /// (used by `CodexAgent::new()` and bare defaults).
    permissions: Option<String>,
    /// Model override, passed via `--model`. `None` lets codex pick.
    model: Option<String>,
}

impl CodexAgent {
    pub fn new() -> Self {
        Self {
            rules: None,
            permissions: None,
            model: None,
        }
    }

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
            permissions: Some(config.permissions.to_string()),
            model: config.model.clone(),
        }
    }
}

impl Default for CodexAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for CodexAgent {
    fn launch_command(&self, _session: &Session) -> String {
        build_launch_command(self.permissions.as_deref(), self.model.as_deref())
    }

    fn environment(&self, session: &Session) -> Vec<(String, String)> {
        vec![
            ("AO_SESSION_ID".to_string(), session.id.to_string()),
            (
                "AO_ISSUE_ID".to_string(),
                session.issue_id.clone().unwrap_or_default(),
            ),
        ]
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
        tokio::task::spawn_blocking(move || detect_codex_activity(&ws))
            .await
            .map_err(|e| ao_core::AoError::Other(format!("detect_activity panicked: {e}")))?
    }

    async fn cost_estimate(&self, session: &Session) -> Result<Option<CostEstimate>> {
        // Best-effort parsing: if Codex history/log files include token usage
        // fields, aggregate them; otherwise return None.
        let Some(ref ws) = session.workspace_path else {
            return Ok(None);
        };
        let _ws = ws.clone();
        tokio::task::spawn_blocking(parse_cost_best_effort)
            .await
            .unwrap_or(Ok(None))
    }
}

// ---------------------------------------------------------------------------
// Launch command assembly
// ---------------------------------------------------------------------------

/// Assemble the `codex` launch command.
///
/// When `permissions` is `Some`, mirrors the ao-ts `appendApprovalFlags`
/// mapping:
/// - `permissionless` → `--dangerously-bypass-approvals-and-sandbox`
/// - `auto-edit`      → `--ask-for-approval never`
/// - `suggest`        → `--ask-for-approval untrusted`
/// - anything else (including `default`) → no approval flag, codex's
///   built-in default applies.
///
/// Either branch also disables the startup update check
/// (`-c check_for_update_on_startup=false`) so the TUI doesn't wedge on
/// a "new version available" prompt, and appends `--model <name>` when
/// the agent config supplies a model.
///
/// When `permissions` is `None` (i.e. `CodexAgent::new()` with no
/// `AgentConfig`), keep the historical `codex --full-auto` string so
/// existing users don't see a behaviour change.
fn build_launch_command(permissions: Option<&str>, model: Option<&str>) -> String {
    let Some(permissions) = permissions else {
        return "codex --full-auto".to_string();
    };

    let mut parts: Vec<String> = vec![
        "codex".to_string(),
        "-c".to_string(),
        "check_for_update_on_startup=false".to_string(),
    ];

    match permissions {
        "permissionless" => parts.push("--dangerously-bypass-approvals-and-sandbox".to_string()),
        "auto-edit" => {
            parts.push("--ask-for-approval".to_string());
            parts.push("never".to_string());
        }
        "suggest" => {
            parts.push("--ask-for-approval".to_string());
            parts.push("untrusted".to_string());
        }
        // `default` and unknown values: no approval flag, codex picks.
        _ => {}
    }

    if let Some(model) = model {
        parts.push("--model".to_string());
        parts.push(shell_escape(model));
        // Match ao-ts: auto-enable high reasoning effort for o-series
        // models via the codex config override (no `--reasoning` flag
        // exists; `model_reasoning_effort` is the supported key).
        if is_o_series_model(model) {
            parts.push("-c".to_string());
            parts.push("model_reasoning_effort=high".to_string());
        }
    }

    parts.join(" ")
}

/// Detect OpenAI o-series reasoning models (o3, o3-mini, o4-mini, …).
fn is_o_series_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let mut chars = lower.chars();
    matches!(chars.next(), Some('o')) && matches!(chars.next(), Some('3' | '4'))
}

// ---------------------------------------------------------------------------
// Activity detection helpers
// ---------------------------------------------------------------------------

fn codex_home() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("CODEX_HOME") {
        if !v.trim().is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".codex"))
}

fn most_recent_mtime_secs(paths: &[PathBuf]) -> Option<u64> {
    let mut best: Option<std::time::SystemTime> = None;
    for p in paths {
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        let Ok(m) = meta.modified() else { continue };
        if best.is_none_or(|b| m > b) {
            best = Some(m);
        }
    }
    let t = best?;
    let age = std::time::SystemTime::now()
        .duration_since(t)
        .unwrap_or_default();
    Some(age.as_secs())
}

fn detect_codex_activity(workspace_path: &Path) -> Result<ActivityState> {
    if let Some(home) = codex_home() {
        let candidates = vec![
            home.join("history.jsonl"),
            home.join("log").join("codex-tui.log"),
        ];
        if let Some(age) = most_recent_mtime_secs(&candidates) {
            if age <= ACTIVE_WINDOW_SECS {
                return Ok(ActivityState::Active);
            }
            if age <= IDLE_THRESHOLD_SECS {
                return Ok(ActivityState::Ready);
            }
            return Ok(ActivityState::Idle);
        }
    }

    if has_recent_commits(workspace_path) {
        return Ok(ActivityState::Active);
    }

    Ok(ActivityState::Ready)
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

// ---------------------------------------------------------------------------
// Cost (best-effort)
// ---------------------------------------------------------------------------

fn parse_cost_best_effort() -> Result<Option<CostEstimate>> {
    let Some(home) = codex_home() else {
        return Ok(None);
    };
    let history = home.join("history.jsonl");
    let log = home.join("log").join("codex-tui.log");

    let mut agg = UsageAgg::default();
    agg.ingest_jsonl_file(&history);
    agg.ingest_jsonl_file(&log);

    Ok(agg.into_cost_estimate())
}

#[derive(Default)]
struct UsageAgg {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
}

impl UsageAgg {
    fn ingest_jsonl_file(&mut self, path: &Path) {
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        let reader = std::io::BufReader::new(file);
        for line in std::io::BufRead::lines(reader).map_while(|r| r.ok()) {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            self.ingest_value(&v);
        }
    }

    fn ingest_value(&mut self, v: &serde_json::Value) {
        // Be liberal: accept either a flat `usage` object or direct token keys.
        let usage = v.get("usage").unwrap_or(v);

        self.input_tokens += usage
            .get("input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        self.output_tokens += usage
            .get("output_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        self.cache_read_tokens += usage
            .get("cache_read_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        self.cache_creation_tokens += usage
            .get("cache_creation_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
    }

    fn into_cost_estimate(self) -> Option<CostEstimate> {
        if self.input_tokens == 0 && self.output_tokens == 0 {
            return None;
        }
        // Codex JSONL carries token counts but no reliable USD figure,
        // and we don't maintain a provider pricing table here. Emit
        // `None` for USD so consumers render it as "unknown" instead of
        // the misleading `$0.00` the previous hard-coded zero produced.
        Some(CostEstimate {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cost_usd: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{now_ms, PermissionsMode, SessionId, SessionStatus};
    use std::io::Write;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fake_session() -> Session {
        Session {
            id: SessionId("codex-test".into()),
            project_id: "demo".into(),
            status: SessionStatus::Working,
            agent: "codex".into(),
            agent_config: None,
            branch: "ao-abc123-feat-test".into(),
            task: "fix the bug".into(),
            workspace_path: Some(std::env::temp_dir().join("ao-codex-ws")),
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
            last_review_backlog_fingerprint: None,
        }
    }

    #[test]
    fn launch_command_uses_full_auto_without_config() {
        // Backward compatibility: callers using `CodexAgent::new()` (no
        // AgentConfig) keep the historical `--full-auto` preset. Users
        // who don't opt into fine-grained permission modes shouldn't
        // see a launch-flag change when they upgrade.
        let agent = CodexAgent::new();
        let cmd = agent.launch_command(&fake_session());
        assert_eq!(cmd, "codex --full-auto");
    }

    #[test]
    fn launch_command_permissionless_bypasses_approvals_and_sandbox() {
        let cfg = AgentConfig {
            permissions: PermissionsMode::Permissionless,
            rules: None,
            rules_file: None,
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let cmd = CodexAgent::from_config(&cfg).launch_command(&fake_session());
        assert!(cmd.starts_with("codex "));
        assert!(cmd.contains("-c check_for_update_on_startup=false"));
        assert!(cmd.contains("--dangerously-bypass-approvals-and-sandbox"));
        assert!(!cmd.contains("--full-auto"));
    }

    #[test]
    fn launch_command_auto_edit_sets_ask_for_approval_never() {
        let cfg = AgentConfig {
            permissions: PermissionsMode::AutoEdit,
            rules: None,
            rules_file: None,
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let cmd = CodexAgent::from_config(&cfg).launch_command(&fake_session());
        assert!(cmd.contains("--ask-for-approval never"));
        assert!(!cmd.contains("--dangerously-bypass-approvals-and-sandbox"));
    }

    #[test]
    fn launch_command_suggest_sets_ask_for_approval_untrusted() {
        let cfg = AgentConfig {
            permissions: PermissionsMode::Suggest,
            rules: None,
            rules_file: None,
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let cmd = CodexAgent::from_config(&cfg).launch_command(&fake_session());
        assert!(cmd.contains("--ask-for-approval untrusted"));
    }

    #[test]
    fn launch_command_default_omits_approval_flag() {
        // `default` permissions maps to codex's own default approval
        // policy — i.e. no flag, matching ao-ts `appendApprovalFlags`.
        let cfg = AgentConfig {
            permissions: PermissionsMode::Default,
            rules: None,
            rules_file: None,
            model: None,
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let cmd = CodexAgent::from_config(&cfg).launch_command(&fake_session());
        assert!(!cmd.contains("--ask-for-approval"));
        assert!(!cmd.contains("--dangerously-bypass-approvals-and-sandbox"));
        assert!(!cmd.contains("--full-auto"));
        assert!(cmd.contains("-c check_for_update_on_startup=false"));
    }

    #[test]
    fn launch_command_appends_model_flag() {
        let cfg = AgentConfig {
            permissions: PermissionsMode::Default,
            rules: None,
            rules_file: None,
            model: Some("gpt-5-codex".into()),
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let cmd = CodexAgent::from_config(&cfg).launch_command(&fake_session());
        // Always-wrap semantics: model name is always single-quoted.
        assert!(cmd.contains("--model 'gpt-5-codex'"));
        // Non-reasoning model should NOT pull in the reasoning config.
        assert!(!cmd.contains("model_reasoning_effort"));
    }

    #[test]
    fn launch_command_o_series_enables_high_reasoning_effort() {
        // Auto-detect reasoning models (o3/o4…) and add the codex
        // config override so reasoning effort is "high". Mirrors
        // `appendModelFlags` in ao-ts.
        let cfg = AgentConfig {
            permissions: PermissionsMode::Default,
            rules: None,
            rules_file: None,
            model: Some("o4-mini".into()),
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let cmd = CodexAgent::from_config(&cfg).launch_command(&fake_session());
        // Always-wrap semantics: model name is always single-quoted.
        assert!(cmd.contains("--model 'o4-mini'"));
        assert!(cmd.contains("-c model_reasoning_effort=high"));
    }

    #[test]
    fn launch_command_shell_escapes_model_with_special_chars() {
        // Any characters outside the allow-list get single-quoted so a
        // malformed or adversarial model name can't break out of the
        // launch string.
        let cfg = AgentConfig {
            permissions: PermissionsMode::Default,
            rules: None,
            rules_file: None,
            model: Some("weird model with spaces".into()),
            orchestrator_model: None,
            opencode_session_id: None,
        };
        let cmd = CodexAgent::from_config(&cfg).launch_command(&fake_session());
        assert!(cmd.contains("--model 'weird model with spaces'"));
    }

    #[test]
    fn shell_escape_handles_single_quotes() {
        // Canonical always-wrap semantics from ao_core::shell::shell_escape.
        assert_eq!(shell_escape("a'b"), "'a'\\''b'");
        assert_eq!(shell_escape(""), "''");
        // Safe strings are also wrapped — always-wrap is the canonical choice.
        assert_eq!(shell_escape("gpt-5-codex"), "'gpt-5-codex'");
    }

    #[test]
    fn is_o_series_model_matches_reasoning_models() {
        assert!(is_o_series_model("o3"));
        assert!(is_o_series_model("o3-mini"));
        assert!(is_o_series_model("o4-mini"));
        assert!(is_o_series_model("O4-MINI"));
        assert!(!is_o_series_model("o1"));
        assert!(!is_o_series_model("gpt-5-codex"));
        assert!(!is_o_series_model(""));
    }

    #[test]
    fn environment_includes_session_id_and_issue_id() {
        let agent = CodexAgent::new();
        let mut session = fake_session();
        session.issue_id = Some("21".into());
        let env = agent.environment(&session);
        assert!(env
            .iter()
            .any(|(k, v)| k == "AO_SESSION_ID" && v == "codex-test"));
        assert!(env.iter().any(|(k, v)| k == "AO_ISSUE_ID" && v == "21"));
    }

    #[test]
    fn initial_prompt_task_first() {
        let agent = CodexAgent::new();
        assert_eq!(agent.initial_prompt(&fake_session()), "fix the bug");
    }

    #[test]
    fn initial_prompt_issue_first_includes_branch_and_url() {
        let agent = CodexAgent::new();
        let mut session = fake_session();
        session.issue_id = Some("7".into());
        session.issue_url = Some("https://github.com/acme/repo/issues/7".into());
        session.task = "Add dark mode".into();

        let p = agent.initial_prompt(&session);
        assert!(p.contains("issue #7"));
        assert!(p.contains("ao-abc123-feat-test"));
        assert!(p.contains("https://github.com/acme/repo/issues/7"));
        assert!(p.contains("Add dark mode"));
        assert!(p.contains("open a pull request"));
    }

    #[test]
    fn initial_prompt_with_rules_prepends_rules() {
        let agent = CodexAgent {
            rules: Some("Always run tests.".into()),
            permissions: None,
            model: None,
        };
        let p = agent.initial_prompt(&fake_session());
        assert!(p.starts_with("Always run tests."));
        assert!(p.contains("---"));
        assert!(p.contains("fix the bug"));
    }

    #[test]
    fn detect_activity_uses_codex_home_history_mtime() {
        let _lock = ENV_LOCK.lock().unwrap();
        // Point CODEX_HOME at a temp dir so tests don't touch real ~/.codex.
        let dir = std::env::temp_dir().join(format!("ao-codex-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("log")).unwrap();
        let history = dir.join("history.jsonl");
        let mut f = std::fs::File::create(&history).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","usage":{{"input_tokens":1,"output_tokens":2}}}}"#
        )
        .unwrap();

        std::env::set_var("CODEX_HOME", &dir);

        let ws = std::env::temp_dir().join("ao-codex-ws-detect");
        std::fs::create_dir_all(&ws).unwrap();
        let state = detect_codex_activity(&ws).unwrap();
        assert_eq!(state, ActivityState::Active);

        // Backdate the file to force Idle.
        let old_time = filetime::FileTime::from_unix_time(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                - IDLE_THRESHOLD_SECS as i64
                - 60,
            0,
        );
        filetime::set_file_mtime(&history, old_time).unwrap();
        let state2 = detect_codex_activity(&ws).unwrap();
        assert_eq!(state2, ActivityState::Idle);

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&ws);
        std::env::remove_var("CODEX_HOME");
    }

    #[test]
    fn parse_cost_best_effort_reads_usage() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("ao-codex-cost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("log")).unwrap();
        std::env::set_var("CODEX_HOME", &dir);

        let history = dir.join("history.jsonl");
        std::fs::write(
            &history,
            r#"{"type":"assistant","usage":{"input_tokens":10,"output_tokens":5}}
{"type":"assistant","usage":{"input_tokens":3,"output_tokens":7}}
"#,
        )
        .unwrap();

        let cost = parse_cost_best_effort().unwrap().unwrap();
        assert_eq!(cost.input_tokens, 13);
        assert_eq!(cost.output_tokens, 12);
        // Codex has no reliable pricing source, so USD stays unknown.
        // Locking this in prevents a regression back to the misleading
        // placeholder `0.0` that issue #100 flagged.
        assert!(cost.cost_usd.is_none());

        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("CODEX_HOME");
    }

    #[test]
    fn parse_cost_best_effort_returns_none_when_no_tokens() {
        // An otherwise-empty JSONL shouldn't materialise a zero-cost
        // estimate — the `cost` column stays `-` in status.
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("ao-codex-cost-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("log")).unwrap();
        std::env::set_var("CODEX_HOME", &dir);
        std::fs::write(dir.join("history.jsonl"), "\n").unwrap();

        let result = parse_cost_best_effort().unwrap();
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("CODEX_HOME");
    }
}
