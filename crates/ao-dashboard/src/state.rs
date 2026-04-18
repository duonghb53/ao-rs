//! Shared application state for the dashboard API.

use ao_core::{Agent, OrchestratorEvent, Runtime, Scm, SessionManager, Workspace};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Shared state injected into every axum handler via `Extension`.
#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<SessionManager>,
    pub events_tx: broadcast::Sender<OrchestratorEvent>,
    pub runtime: Arc<dyn Runtime>,
    pub scm: Arc<dyn Scm>,
    pub agent: Arc<dyn Agent>,
    /// Workspace plugin used by `restore` to probe `exists()` on the
    /// persisted `workspace_path` before attempting to respawn.
    pub workspace: Arc<dyn Workspace>,
    /// Path to the `ao-rs.yaml` the dashboard was started with.
    ///
    /// Reloaded per request by routes that need project→repo mapping
    /// (`GET /api/issues`) so config edits take effect without a restart.
    /// `None` in unit tests that construct `AppState` without a real
    /// config on disk.
    pub config_path: Option<PathBuf>,
}
