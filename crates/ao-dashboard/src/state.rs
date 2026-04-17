//! Shared application state for the dashboard API.

use ao_core::{Agent, OrchestratorEvent, Runtime, Scm, SessionManager, Workspace};
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
}
