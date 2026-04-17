//! Monthly-rotated cost ledger for permanent per-session cost backup.
//!
//! Layout: `~/.ao-rs/cost-ledger/YYYY-MM.yaml`
//!
//! Each file contains a list of `CostEntry` records keyed by session id.
//! The lifecycle loop appends/updates entries whenever `Agent::cost_estimate`
//! returns a value — this means cost data survives even if the JSONL source
//! and session YAML are both deleted.
//!
//! Monthly rotation keeps individual files small. The file for a session is
//! determined by `created_at`, so a session that spans two months still
//! writes to one file.

use crate::{paths, types::CostEstimate};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One row in the ledger file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEntry {
    pub session_id: String,
    pub project_id: String,
    pub branch: String,
    pub cost: CostEstimate,
    /// Unix epoch ms — copied from `Session::created_at` so we can sort
    /// without parsing the filename.
    pub created_at: u64,
    /// Unix epoch ms of the last update to this entry.
    pub updated_at: u64,
}

/// Ledger file: a thin wrapper around `Vec<CostEntry>`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CostLedger {
    pub entries: Vec<CostEntry>,
}

/// Directory for cost ledger files: `~/.ao-rs/cost-ledger/`.
/// Thin wrapper over [`paths::cost_ledger_dir`] — kept for readability at call sites.
pub fn ledger_dir() -> PathBuf {
    paths::cost_ledger_dir()
}

/// Ledger file path for a given session's `created_at` timestamp.
/// E.g. `~/.ao-rs/cost-ledger/2026-04.yaml`.
pub fn ledger_path_for(created_at_ms: u64) -> PathBuf {
    let secs = created_at_ms / 1000;
    // chrono would be nicer but we avoid a dep for a trivial conversion.
    let dt = time_from_epoch_secs(secs);
    ledger_dir().join(format!("{}.yaml", dt))
}

/// Format epoch seconds as "YYYY-MM".
fn time_from_epoch_secs(secs: u64) -> String {
    // Manual conversion — no chrono dependency needed.
    // Days since 1970-01-01.
    let days = secs / 86400;
    let (year, month, _day) = days_to_ymd(days);
    format!("{year:04}-{month:02}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Upsert a cost entry for the given session into the appropriate monthly ledger.
pub fn record_cost(
    session_id: &str,
    project_id: &str,
    branch: &str,
    cost: &CostEstimate,
    created_at: u64,
) -> std::io::Result<()> {
    let path = ledger_path_for(created_at);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut ledger = if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        match serde_yaml::from_str::<CostLedger>(&contents) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    "corrupt cost ledger at {}: {e}, starting fresh",
                    path.display()
                );
                CostLedger::default()
            }
        }
    } else {
        CostLedger::default()
    };

    let now = crate::types::now_ms();

    // Upsert: update existing entry or append a new one.
    if let Some(entry) = ledger
        .entries
        .iter_mut()
        .find(|e| e.session_id == session_id)
    {
        entry.cost = cost.clone();
        entry.updated_at = now;
    } else {
        ledger.entries.push(CostEntry {
            session_id: session_id.to_string(),
            project_id: project_id.to_string(),
            branch: branch.to_string(),
            cost: cost.clone(),
            created_at,
            updated_at: now,
        });
    }

    let yaml = serde_yaml::to_string(&ledger).map_err(std::io::Error::other)?;
    std::fs::write(&path, yaml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CostEstimate;

    #[test]
    fn ledger_path_month_rotation() {
        // 2026-04-12 00:00:00 UTC → April 2026
        let ts = 1_776_124_800_000u64; // approx 2026-04-12
        let p = ledger_path_for(ts);
        assert!(
            p.to_str().unwrap().ends_with("2026-04.yaml"),
            "got: {}",
            p.display()
        );
    }

    #[test]
    fn days_to_ymd_known_dates() {
        // 1970-01-01
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // 2000-01-01 is day 10957
        assert_eq!(days_to_ymd(10957), (2000, 1, 1));
    }

    #[test]
    fn record_and_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ao-ledger-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Override HOME so ledger_dir() points to our temp dir.
        // Instead, write directly to a temp path.
        let path = dir.join("2026-04.yaml");

        let cost = CostEstimate {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: 200,
            cache_creation_tokens: 100,
            cost_usd: Some(0.05),
        };

        // Manually create ledger to test serialization.
        let mut ledger = CostLedger::default();
        ledger.entries.push(CostEntry {
            session_id: "s1".into(),
            project_id: "p1".into(),
            branch: "feat-x".into(),
            cost: cost.clone(),
            created_at: 1_776_124_800_000,
            updated_at: 1_776_124_800_000,
        });

        let yaml = serde_yaml::to_string(&ledger).unwrap();
        std::fs::write(&path, &yaml).unwrap();

        let read_back: CostLedger =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(read_back.entries.len(), 1);
        assert_eq!(read_back.entries[0].session_id, "s1");
        assert_eq!(read_back.entries[0].cost, cost);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upsert_updates_existing_entry() {
        let mut ledger = CostLedger::default();
        let cost_v1 = CostEstimate {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: Some(0.01),
        };
        ledger.entries.push(CostEntry {
            session_id: "s1".into(),
            project_id: "p1".into(),
            branch: "feat-x".into(),
            cost: cost_v1,
            created_at: 1000,
            updated_at: 1000,
        });

        // Simulate upsert
        let cost_v2 = CostEstimate {
            input_tokens: 500,
            output_tokens: 250,
            cache_read_tokens: 100,
            cache_creation_tokens: 50,
            cost_usd: Some(0.05),
        };
        if let Some(entry) = ledger.entries.iter_mut().find(|e| e.session_id == "s1") {
            entry.cost = cost_v2.clone();
            entry.updated_at = 2000;
        }

        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].cost, cost_v2);
        assert_eq!(ledger.entries[0].updated_at, 2000);
    }
}
