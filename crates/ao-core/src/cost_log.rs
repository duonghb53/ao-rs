//! Workspace-local usage JSONL (one line per agent turn).
//!
//! Companion to `activity_log.rs`: lives at `{workspace}/.ao/usage.jsonl`
//! and carries aggregated token usage for plugins that don't have a
//! native JSONL source of their own. The default `Agent::cost_estimate`
//! reads this file; plugins that override `cost_estimate` (e.g.
//! `agent-claude-code` reading `~/.claude/projects/**`) ignore it.
//!
//! Distinct from `cost_ledger.rs`:
//! - `cost_ledger` is the **daemon-side** monthly rotation under
//!   `~/.ao-rs/cost-ledger/YYYY-MM.yaml` — keyed by session id across
//!   workspaces.
//! - `cost_log` is the **per-workspace** turn-level log — one file per
//!   session's worktree.
//!
//! Format: newline-delimited JSON, one `UsageLogEntry` per line:
//!
//! ```json
//! {"ts":"2026-04-17T03:07:00Z","input_tokens":500,"output_tokens":200,
//!  "cache_read_tokens":10,"cache_creation_tokens":5,"cost_usd":0.0034}
//! ```
//!
//! Every field is `#[serde(default)]` so partial writes contribute what
//! they can. Unknown fields are ignored (forward compat).

use crate::types::CostEstimate;
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLogEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
}

/// Canonical log path: `{workspace}/.ao/usage.jsonl`.
pub fn usage_log_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ao").join("usage.jsonl")
}

/// Aggregate every parseable line in `{workspace}/.ao/usage.jsonl`
/// into a single `CostEstimate`.
///
/// Returns `None` when the file is missing, empty, unreadable, or
/// aggregates to zero tokens — matching the "no data == no estimate"
/// rule used by plugins with native parsers.
pub fn parse_usage_jsonl(workspace_path: &Path) -> Option<CostEstimate> {
    let path = usage_log_path(workspace_path);
    let file = std::fs::File::open(&path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut cache_creation_tokens = 0u64;
    let mut cost_usd = 0f64;

    for line in reader.lines().map_while(std::result::Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(e) = serde_json::from_str::<UsageLogEntry>(&line) else {
            continue;
        };
        input_tokens = input_tokens.saturating_add(e.input_tokens);
        output_tokens = output_tokens.saturating_add(e.output_tokens);
        cache_read_tokens = cache_read_tokens.saturating_add(e.cache_read_tokens);
        cache_creation_tokens = cache_creation_tokens.saturating_add(e.cache_creation_tokens);
        cost_usd += e.cost_usd;
    }

    if input_tokens == 0 && output_tokens == 0 {
        return None;
    }

    Some(CostEstimate {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        cost_usd: if cost_usd > 0.0 { Some(cost_usd) } else { None },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_workspace(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("ao-rs-cost-log-{label}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_log(workspace: &Path, lines: &[&str]) {
        let path = usage_log_path(workspace);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    #[test]
    fn missing_file_returns_none() {
        let ws = unique_workspace("missing");
        assert!(parse_usage_jsonl(&ws).is_none());
    }

    #[test]
    fn empty_file_returns_none() {
        let ws = unique_workspace("empty");
        write_log(&ws, &[]);
        assert!(parse_usage_jsonl(&ws).is_none());
    }

    #[test]
    fn zero_tokens_returns_none() {
        let ws = unique_workspace("zero");
        write_log(
            &ws,
            &[r#"{"input_tokens":0,"output_tokens":0,"cost_usd":0.0}"#],
        );
        assert!(parse_usage_jsonl(&ws).is_none());
    }

    #[test]
    fn single_line_round_trip() {
        let ws = unique_workspace("single");
        write_log(
            &ws,
            &[
                r#"{"input_tokens":100,"output_tokens":50,"cache_read_tokens":10,"cache_creation_tokens":5,"cost_usd":0.0012}"#,
            ],
        );
        let got = parse_usage_jsonl(&ws).expect("some");
        assert_eq!(got.input_tokens, 100);
        assert_eq!(got.output_tokens, 50);
        assert_eq!(got.cache_read_tokens, 10);
        assert_eq!(got.cache_creation_tokens, 5);
        assert!((got.cost_usd - 0.0012).abs() < 1e-9);
    }

    #[test]
    fn multi_line_sums_all_fields() {
        let ws = unique_workspace("multi");
        write_log(
            &ws,
            &[
                r#"{"input_tokens":100,"output_tokens":50,"cost_usd":0.5}"#,
                r#"{"input_tokens":200,"output_tokens":75,"cache_read_tokens":4,"cost_usd":0.25}"#,
                r#"{"input_tokens":50,"output_tokens":25,"cache_creation_tokens":2,"cost_usd":0.125}"#,
            ],
        );
        let got = parse_usage_jsonl(&ws).expect("some");
        assert_eq!(got.input_tokens, 350);
        assert_eq!(got.output_tokens, 150);
        assert_eq!(got.cache_read_tokens, 4);
        assert_eq!(got.cache_creation_tokens, 2);
        assert!((got.cost_usd - 0.875).abs() < 1e-9);
    }

    #[test]
    fn garbage_lines_are_skipped() {
        let ws = unique_workspace("garbage");
        write_log(
            &ws,
            &[
                "not json",
                r#"{"input_tokens":100,"output_tokens":50}"#,
                "{",
                r#"{"input_tokens":10,"output_tokens":5}"#,
                "",
            ],
        );
        let got = parse_usage_jsonl(&ws).expect("some");
        assert_eq!(got.input_tokens, 110);
        assert_eq!(got.output_tokens, 55);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let ws = unique_workspace("unknown");
        write_log(
            &ws,
            &[r#"{"input_tokens":10,"output_tokens":5,"model":"opus","unknown":"ok"}"#],
        );
        let got = parse_usage_jsonl(&ws).expect("some");
        assert_eq!(got.input_tokens, 10);
        assert_eq!(got.output_tokens, 5);
    }

    #[test]
    fn usage_log_path_shape() {
        let ws = PathBuf::from("/tmp/ao-ws");
        assert_eq!(
            usage_log_path(&ws),
            PathBuf::from("/tmp/ao-ws/.ao/usage.jsonl")
        );
    }
}
