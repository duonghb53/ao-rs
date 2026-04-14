//! SSE event stream for real-time dashboard updates.

use crate::state::AppState;
use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use std::convert::Infallible;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
/// GET /api/events — server-sent event stream of OrchestratorEvent.
///
/// Each event is serialized as JSON in the SSE `data:` field.
///
/// Snapshot/delta semantics:
/// - The first message is a `{"type":"snapshot","sessions":[...]}` payload so UIs can paint immediately.
/// - Subsequent messages are `OrchestratorEvent` deltas from the lifecycle loop.
pub async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let sessions = state.sessions.list().await.unwrap_or_default();
    let snapshot_json = serde_json::json!({
        "type": "snapshot",
        "sessions": sessions,
    });
    let snapshot = serde_json::to_string(&snapshot_json).unwrap_or_default();

    let rx = state.events_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let json = serde_json::to_string(&event).unwrap_or_default();
            Some(Ok(Event::default().data(json)))
        }
        // Lagged: skip lost events, stream continues.
        Err(_) => None,
    });

    Sse::new(tokio_stream::once(Ok(Event::default().data(snapshot))).chain(stream))
        .keep_alive(KeepAlive::default())
}
