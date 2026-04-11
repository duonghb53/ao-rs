//! Disk layout helpers for the `~/.ao-rs/` data dir.
//!
//! Equivalent of `packages/core/src/paths.ts` in the reference repo, scoped
//! down to what Slice 1 needs.

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
