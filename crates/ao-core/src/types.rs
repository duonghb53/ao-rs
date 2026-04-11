use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Slice 0 status set — kept intentionally minimal.
/// Slice 1 will expand to ~10 states (pr_open, ci_failed, review_pending, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Spawning,
    Working,
    Done,
    Errored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub repo_path: PathBuf,
    pub default_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub project_id: String,
    pub status: SessionStatus,
    pub branch: String,
    pub task: String,
    pub workspace_path: Option<PathBuf>,
    /// Opaque handle returned by the Runtime plugin (e.g. tmux session name).
    pub runtime_handle: Option<String>,
    /// Unix epoch milliseconds when this session was first persisted.
    /// Used for sorting newest-first in `ao-rs status`.
    pub created_at: u64,
}

/// Current Unix time in milliseconds. Helper for `Session::created_at`.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Input to `Workspace::create`. Carries everything the plugin needs to
/// materialize an isolated working directory for a session.
#[derive(Debug, Clone)]
pub struct WorkspaceCreateConfig {
    pub project_id: String,
    pub session_id: String,
    pub branch: String,
    pub repo_path: PathBuf,
    pub default_branch: String,
}
