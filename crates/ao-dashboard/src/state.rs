//! Shared application state for the dashboard API.

use ao_core::{OrchestratorEvent, Runtime, Scm, SessionManager};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Shared state injected into every axum handler via `Extension`.
#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<SessionManager>,
    pub events_tx: broadcast::Sender<OrchestratorEvent>,
    pub runtime: Arc<dyn Runtime>,
    pub scm: Arc<dyn Scm>,
}
