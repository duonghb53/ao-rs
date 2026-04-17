//! Disk layout helpers for the `~/.ao-rs/` data dir.
//!
//! Equivalent of `packages/core/src/paths.ts` in the reference repo, scoped
//! down to what ao-rs features actually use today. Keep this module minimal:
//! add a helper when a feature needs a new on-disk location, not before.

use std::path::PathBuf;

/// Root of the ao-rs data directory: `~/.ao-rs`.
pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    home.join(".ao-rs")
}

/// `~/.ao-rs/sessions` — where `SessionManager` stores per-project session yaml files.
pub fn default_sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

/// `~/.ao-rs/lifecycle.pid` — pidfile used by `ao-rs watch` to coordinate a
/// singleton daemon process. See `lockfile.rs` and `packages/cli/src/lib/lifecycle-service.ts`
/// in the reference repo.
pub fn lifecycle_pid_file() -> PathBuf {
    data_dir().join("lifecycle.pid")
}

/// `~/.ao-rs/cost-ledger/` — monthly-rotated cost ledger files.
/// See `cost_ledger.rs` for the `YYYY-MM.yaml` layout.
pub fn cost_ledger_dir() -> PathBuf {
    data_dir().join("cost-ledger")
}

/// `~/.ao-rs/review-fingerprints/` — per-session fingerprints used by
/// `ao-rs review-check` to detect new PR comments since the last run.
pub fn review_fingerprint_dir() -> PathBuf {
    data_dir().join("review-fingerprints")
}

/// `~/.ao-rs/review-fingerprints/{session_id}.txt` — fingerprint file for
/// a single session.
pub fn review_fingerprint_file(session_id: &str) -> PathBuf {
    review_fingerprint_dir().join(format!("{session_id}.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_ends_with_dot_ao_rs() {
        let p = data_dir();
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some(".ao-rs"));
    }

    #[test]
    fn default_sessions_dir_is_under_data_dir() {
        let p = default_sessions_dir();
        assert_eq!(p.parent(), Some(data_dir().as_path()));
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("sessions"));
    }

    #[test]
    fn lifecycle_pid_file_is_under_data_dir() {
        let p = lifecycle_pid_file();
        assert_eq!(p.parent(), Some(data_dir().as_path()));
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("lifecycle.pid"));
    }

    #[test]
    fn cost_ledger_dir_is_under_data_dir() {
        let p = cost_ledger_dir();
        assert_eq!(p.parent(), Some(data_dir().as_path()));
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("cost-ledger"));
    }

    #[test]
    fn review_fingerprint_dir_is_under_data_dir() {
        let p = review_fingerprint_dir();
        assert_eq!(p.parent(), Some(data_dir().as_path()));
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("review-fingerprints")
        );
    }

    #[test]
    fn review_fingerprint_file_is_under_fingerprint_dir() {
        let p = review_fingerprint_file("abcd-1234");
        assert_eq!(p.parent(), Some(review_fingerprint_dir().as_path()));
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("abcd-1234.txt")
        );
    }

    #[test]
    fn review_fingerprint_file_respects_arbitrary_session_id() {
        // Session ids are UUIDs in practice; the helper shouldn't mangle them.
        let id = "9f2c5a0e-d8f4-4b2a-9e7c-123456789abc";
        let p = review_fingerprint_file(id);
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("9f2c5a0e-d8f4-4b2a-9e7c-123456789abc.txt")
        );
    }
}
