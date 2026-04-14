//! Activity JSONL log (TS `activity-log.ts`-inspired).
//!
//! Used as an optional fallback for agents that don't have native session logs.
//! Format: one JSON object per line at `{workspace}/.ao/activity.jsonl`.

use crate::types::ActivityState;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const ACTIVITY_INPUT_STALENESS_SECS: u64 = 5 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityLogEntry {
    pub ts: String,
    pub state: ActivityState,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
}

pub fn activity_log_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ao").join("activity.jsonl")
}

pub fn append_activity_entry(
    workspace_path: &Path,
    entry: &ActivityLogEntry,
) -> std::io::Result<()> {
    let p = activity_log_path(workspace_path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(entry).unwrap_or_default();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)?
        .write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}

pub fn read_last_activity_entry(
    workspace_path: &Path,
) -> std::io::Result<Option<(ActivityLogEntry, std::time::SystemTime)>> {
    use std::io::{BufRead, Seek};

    let p = activity_log_path(workspace_path);
    let meta = match std::fs::metadata(&p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if meta.len() == 0 {
        return Ok(None);
    }
    let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);

    // Tail-read last 4KB.
    let tail_size: u64 = 4096;
    let offset = meta.len().saturating_sub(tail_size);
    let mut f = std::fs::File::open(&p)?;
    if offset > 0 {
        f.seek(std::io::SeekFrom::Start(offset))?;
    }
    let mut r = std::io::BufReader::new(f);
    if offset > 0 {
        // Drop partial line.
        let mut _discard = String::new();
        let _ = r.read_line(&mut _discard);
    }

    let mut last_ok: Option<ActivityLogEntry> = None;
    for line in r.lines().map_while(std::result::Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<ActivityLogEntry>(&line) {
            last_ok = Some(e);
        }
    }
    Ok(last_ok.map(|e| (e, modified)))
}

/// Returns actionable states only (`waiting_input` / `blocked`) with staleness cap.
pub fn check_actionable_state(
    entry: Option<&ActivityLogEntry>,
    now: std::time::SystemTime,
) -> Option<ActivityState> {
    let e = entry?;
    if !matches!(
        e.state,
        ActivityState::WaitingInput | ActivityState::Blocked
    ) {
        return None;
    }
    let ts = chrono_like_parse(&e.ts)?;
    let age = now.duration_since(ts).ok()?.as_secs();
    (age <= ACTIVITY_INPUT_STALENESS_SECS).then_some(e.state)
}

fn chrono_like_parse(s: &str) -> Option<std::time::SystemTime> {
    // Minimal ISO-ish parsing without pulling in chrono. Accepts RFC3339 via `DateTime::parse_from_rfc3339`
    // would be nicer, but keep deps minimal: only accept unix ms encoded strings as fallback.
    if let Ok(ms) = s.parse::<u128>() {
        return Some(std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actionable_state_respects_staleness() {
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let fresh = ActivityLogEntry {
            ts: (1_000_000u64 * 1000).to_string(),
            state: ActivityState::WaitingInput,
            source: "terminal".into(),
            trigger: Some("prompt".into()),
        };
        assert_eq!(
            check_actionable_state(Some(&fresh), now),
            Some(ActivityState::WaitingInput)
        );

        let stale = ActivityLogEntry {
            ts: ((1_000_000u64 - (ACTIVITY_INPUT_STALENESS_SECS + 1)) * 1000).to_string(),
            state: ActivityState::Blocked,
            source: "terminal".into(),
            trigger: None,
        };
        assert_eq!(check_actionable_state(Some(&stale), now), None);
    }
}
