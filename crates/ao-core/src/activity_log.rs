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

/// Best-effort activity probe from `{workspace}/.ao/activity.jsonl`.
///
/// Surfaces only the states a stale log can still describe honestly:
/// - `Exited` is terminal; staleness does not downgrade it.
/// - `WaitingInput` / `Blocked` surface when within the staleness cap.
///
/// Everything else (including `Active` / `Ready` / `Idle` entries, stale
/// actionable entries, and a missing or empty log) returns `None` so the
/// caller can fall through to its own default — matching the
/// `Agent::detect_activity` "no detection available" contract.
pub fn detect_activity_from_log(workspace_path: &Path) -> Option<ActivityState> {
    let (entry, _modified) = read_last_activity_entry(workspace_path).ok().flatten()?;
    if entry.state == ActivityState::Exited {
        return Some(ActivityState::Exited);
    }
    check_actionable_state(Some(&entry), std::time::SystemTime::now())
}

fn chrono_like_parse(s: &str) -> Option<std::time::SystemTime> {
    // Accept numeric unix-ms strings (ao-rs native writers) and RFC3339 strings
    // (ao-ts writers using `Date.toISOString()`, and any RFC3339-emitting source).
    // Kept dep-free on purpose — the accepted RFC3339 subset matches what
    // `activity.jsonl` producers actually emit.
    if let Ok(ms) = s.parse::<u128>() {
        return Some(std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64));
    }
    parse_rfc3339(s)
}

/// Minimal RFC3339 parser: `YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM|±HHMM]`.
/// Accepts `T`/`t`/space as the date-time separator. Returns `None` for any
/// malformed input — callers already treat `None` as "no usable timestamp".
fn parse_rfc3339(s: &str) -> Option<std::time::SystemTime> {
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    // Fixed-position punctuation check: YYYY-MM-DDTHH:MM:SS
    if b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    if b[10] != b'T' && b[10] != b't' && b[10] != b' ' {
        return None;
    }

    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let min: u32 = s.get(14..16)?.parse().ok()?;
    let sec: u32 = s.get(17..19)?.parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || min > 59 || sec > 60 {
        return None;
    }

    let mut rest = &s[19..];
    let mut nanos: u32 = 0;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let frac_end = after_dot
            .find(['Z', 'z', '+', '-'])
            .unwrap_or(after_dot.len());
        let frac = &after_dot[..frac_end];
        if frac.is_empty() || frac.len() > 9 || !frac.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n: u64 = frac.parse().ok()?;
        nanos = (n * 10u64.pow(9 - frac.len() as u32)) as u32;
        rest = &after_dot[frac_end..];
    }

    let offset_secs: i64 = if rest.eq_ignore_ascii_case("z") {
        0
    } else if let Some(rem) = rest.strip_prefix('+').or_else(|| rest.strip_prefix('-')) {
        let sign: i64 = if rest.starts_with('+') { 1 } else { -1 };
        let (hh, mm) = match rem.len() {
            5 if rem.as_bytes()[2] == b':' => (rem.get(0..2)?, rem.get(3..5)?),
            4 => (rem.get(0..2)?, rem.get(2..4)?),
            _ => return None,
        };
        let hh: i64 = hh.parse().ok()?;
        let mm: i64 = mm.parse().ok()?;
        if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) {
            return None;
        }
        sign * (hh * 3600 + mm * 60)
    } else {
        return None;
    };

    let days = days_from_civil(year, month, day);
    let total = days
        .checked_mul(86400)?
        .checked_add(hour as i64 * 3600 + min as i64 * 60 + sec as i64)?
        .checked_sub(offset_secs)?;
    if total < 0 {
        return None;
    }
    Some(
        std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(total as u64)
            + std::time::Duration::from_nanos(nanos as u64),
    )
}

/// Days from 1970-01-01 to `(y, m, d)` using Howard Hinnant's civil→days algorithm.
/// Handles negative years correctly; valid for the full proleptic Gregorian range.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m_adj = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * m_adj + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_workspace(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("ao-rs-activity-log-{label}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

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

    #[test]
    fn detect_from_log_missing_returns_none() {
        let ws = unique_workspace("missing");
        assert!(detect_activity_from_log(&ws).is_none());
    }

    #[test]
    fn detect_from_log_exited_always_wins() {
        let ws = unique_workspace("exited");
        // Even with a timestamp that's way past the staleness cap,
        // an Exited entry should surface — terminal is one-way.
        let ancient = ActivityLogEntry {
            ts: "0".into(),
            state: ActivityState::Exited,
            source: "terminal".into(),
            trigger: None,
        };
        append_activity_entry(&ws, &ancient).unwrap();
        assert_eq!(
            detect_activity_from_log(&ws),
            Some(ActivityState::Exited),
            "stale Exited entries should still surface",
        );
    }

    #[test]
    fn detect_from_log_fresh_waiting_input_surfaces() {
        let ws = unique_workspace("fresh-waiting");
        let entry = ActivityLogEntry {
            ts: now_ms().to_string(),
            state: ActivityState::WaitingInput,
            source: "terminal".into(),
            trigger: Some("approve?".into()),
        };
        append_activity_entry(&ws, &entry).unwrap();
        assert_eq!(
            detect_activity_from_log(&ws),
            Some(ActivityState::WaitingInput)
        );
    }

    #[test]
    fn detect_from_log_fresh_blocked_surfaces() {
        let ws = unique_workspace("fresh-blocked");
        let entry = ActivityLogEntry {
            ts: now_ms().to_string(),
            state: ActivityState::Blocked,
            source: "terminal".into(),
            trigger: Some("error".into()),
        };
        append_activity_entry(&ws, &entry).unwrap();
        assert_eq!(detect_activity_from_log(&ws), Some(ActivityState::Blocked));
    }

    #[test]
    fn detect_from_log_stale_actionable_falls_through() {
        let ws = unique_workspace("stale-actionable");
        let stale_ms = now_ms().saturating_sub((ACTIVITY_INPUT_STALENESS_SECS + 60) * 1000);
        let entry = ActivityLogEntry {
            ts: stale_ms.to_string(),
            state: ActivityState::WaitingInput,
            source: "terminal".into(),
            trigger: None,
        };
        append_activity_entry(&ws, &entry).unwrap();
        assert!(detect_activity_from_log(&ws).is_none());
    }

    #[test]
    fn detect_from_log_ignores_active_and_ready() {
        let ws = unique_workspace("active-ready");
        // A fresh Active should not surface from the default detector —
        // Active is a noisy signal that belongs to the plugin's own logic.
        let entry = ActivityLogEntry {
            ts: now_ms().to_string(),
            state: ActivityState::Active,
            source: "terminal".into(),
            trigger: None,
        };
        append_activity_entry(&ws, &entry).unwrap();
        assert!(detect_activity_from_log(&ws).is_none());
    }

    #[test]
    fn parse_numeric_ms_still_works() {
        let got = chrono_like_parse("1700000000000").unwrap();
        let want = UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_000);
        assert_eq!(got, want);
    }

    #[test]
    fn parse_rfc3339_utc_z() {
        // 2024-01-15T10:30:00Z  =>  1705314600 seconds since epoch
        let got = chrono_like_parse("2024-01-15T10:30:00Z").unwrap();
        let want = UNIX_EPOCH + std::time::Duration::from_secs(1_705_314_600);
        assert_eq!(got, want);
    }

    #[test]
    fn parse_rfc3339_with_milliseconds() {
        // Matches `new Date().toISOString()` shape used by the TS writer.
        let got = chrono_like_parse("2024-01-15T10:30:00.123Z").unwrap();
        let want = UNIX_EPOCH
            + std::time::Duration::from_secs(1_705_314_600)
            + std::time::Duration::from_millis(123);
        assert_eq!(got, want);
    }

    #[test]
    fn parse_rfc3339_with_positive_offset() {
        // 10:30:00+02:00 == 08:30:00Z == 1705307400
        let got = chrono_like_parse("2024-01-15T10:30:00+02:00").unwrap();
        let want = UNIX_EPOCH + std::time::Duration::from_secs(1_705_307_400);
        assert_eq!(got, want);
    }

    #[test]
    fn parse_rfc3339_with_negative_offset_and_micros() {
        // 10:30:00.500000-05:00 == 15:30:00.5Z == 1705332600.5
        let got = chrono_like_parse("2024-01-15T10:30:00.500000-05:00").unwrap();
        let want = UNIX_EPOCH
            + std::time::Duration::from_secs(1_705_332_600)
            + std::time::Duration::from_millis(500);
        assert_eq!(got, want);
    }

    #[test]
    fn parse_rfc3339_lowercase_t_and_z() {
        // RFC3339 permits lowercase `t`/`z`; several emitters rely on it.
        let got = chrono_like_parse("2024-01-15t10:30:00z").unwrap();
        let want = UNIX_EPOCH + std::time::Duration::from_secs(1_705_314_600);
        assert_eq!(got, want);
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(chrono_like_parse("").is_none());
        assert!(chrono_like_parse("not-a-date").is_none());
        // Missing timezone indicator — ambiguous, reject.
        assert!(chrono_like_parse("2024-01-15T10:30:00").is_none());
        // Bad punctuation.
        assert!(chrono_like_parse("2024/01/15T10:30:00Z").is_none());
        // Out-of-range month.
        assert!(chrono_like_parse("2024-13-15T10:30:00Z").is_none());
        // Garbage fractional.
        assert!(chrono_like_parse("2024-01-15T10:30:00.abcZ").is_none());
    }

    #[test]
    fn actionable_state_respects_staleness_rfc3339() {
        // Same staleness logic as `actionable_state_respects_staleness`,
        // but the entry's `ts` is RFC3339 instead of numeric ms.
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_705_314_600);
        let fresh = ActivityLogEntry {
            ts: "2024-01-15T10:30:00Z".into(),
            state: ActivityState::WaitingInput,
            source: "terminal".into(),
            trigger: Some("prompt".into()),
        };
        assert_eq!(
            check_actionable_state(Some(&fresh), now),
            Some(ActivityState::WaitingInput)
        );

        let stale = ActivityLogEntry {
            // 10 minutes earlier — well past the 5-minute staleness cap.
            ts: "2024-01-15T10:20:00Z".into(),
            state: ActivityState::Blocked,
            source: "terminal".into(),
            trigger: None,
        };
        assert_eq!(check_actionable_state(Some(&stale), now), None);
    }

    #[test]
    fn detect_from_log_uses_last_entry() {
        let ws = unique_workspace("last-entry");
        // First: blocked (actionable). Then: active (noisy).
        // The last line wins — since it's Active, we return None.
        append_activity_entry(
            &ws,
            &ActivityLogEntry {
                ts: now_ms().to_string(),
                state: ActivityState::Blocked,
                source: "terminal".into(),
                trigger: None,
            },
        )
        .unwrap();
        append_activity_entry(
            &ws,
            &ActivityLogEntry {
                ts: now_ms().to_string(),
                state: ActivityState::Active,
                source: "terminal".into(),
                trigger: None,
            },
        )
        .unwrap();
        assert!(detect_activity_from_log(&ws).is_none());
    }
}
