//! TS core utilities (ported from `packages/core/src/utils.ts` and friends).
//!
//! Parity status: test-only.
//!
//! Not wired into the ao-rs runtime. Consumed only by
//! `tests/parity_utils_parity_test.rs`. Duplicate `shell_escape`
//! implementations live in the `runtime-tmux`, `agent-codex`, and
//! `agent-aider` plugin crates; consolidation is deferred until a concrete
//! runtime need makes it worth the churn. See
//! `docs/ts-core-parity-report.md` → "Parity-only modules".

use std::path::Path;

pub fn shell_escape(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r#"'\''"#))
}

pub fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn validate_url(url: &str, label: &str) -> Result<(), String> {
    if url.starts_with("https://") || url.starts_with("http://") {
        Ok(())
    } else {
        Err(format!(
            "[{label}] Invalid url: must be http(s), got \"{url}\""
        ))
    }
}

pub fn is_git_branch_name_safe(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name == "@" || name.starts_with('.') || name.ends_with('.') || name.ends_with('/') {
        return false;
    }
    if name.ends_with(".lock") {
        return false;
    }
    if name.contains("..") || name.contains("//") || name.contains("/.") || name.contains("@{") {
        return false;
    }
    if name.starts_with('/') {
        return false;
    }
    for b in name.bytes() {
        if b <= 0x1f || b == 0x7f {
            return false;
        }
    }
    // whitespace and git-forbidden punctuation: ~ ^ : ? * [ \ (and space)
    if name.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    if name.contains('~')
        || name.contains('^')
        || name.contains(':')
        || name.contains('?')
        || name.contains('*')
        || name.contains('[')
        || name.contains('\\')
    {
        return false;
    }
    true
}

pub fn is_retryable_http_status(status: u16) -> bool {
    status == 429 || status >= 500
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryConfig {
    pub retries: u32,
    pub retry_delay_ms: u64,
}

pub fn normalize_retry_config(
    config: Option<&std::collections::HashMap<String, serde_json::Value>>,
    defaults: RetryConfig,
) -> RetryConfig {
    let raw_retries = config
        .and_then(|m| m.get("retries"))
        .and_then(|v| v.as_i64());
    let raw_delay = config
        .and_then(|m| m.get("retryDelayMs"))
        .and_then(|v| v.as_i64());

    let retries = match raw_retries {
        Some(n) => n.max(0) as u32,
        None => defaults.retries,
    };
    let retry_delay_ms = match raw_delay {
        Some(n) if n >= 0 => n as u64,
        _ => defaults.retry_delay_ms,
    };
    RetryConfig {
        retries,
        retry_delay_ms,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LastJsonlEntry {
    pub last_type: Option<String>,
    pub modified_at: std::time::SystemTime,
}

pub fn read_last_jsonl_entry(path: &Path) -> Option<LastJsonlEntry> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 {
        return None;
    }
    let modified_at = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
    let tail_size = 4096u64.min(meta.len());
    let offset = meta.len().saturating_sub(tail_size);
    let bytes = std::fs::read(path).ok()?;
    let tail = if offset as usize >= bytes.len() {
        &bytes[..]
    } else {
        &bytes[offset as usize..]
    };
    let s = String::from_utf8_lossy(tail);
    let mut lines: Vec<&str> = s.split('\n').filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    // If we started mid-file, first line might be partial; drop it if there are other lines.
    if offset > 0 && lines.len() > 1 {
        lines.remove(0);
    }
    for line in lines.iter().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let last_type = v
            .get("type")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        return Some(LastJsonlEntry {
            last_type,
            modified_at,
        });
    }
    None
}
