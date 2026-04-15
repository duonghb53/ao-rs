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
/// ## SSE event schema contract
///
/// This endpoint is **append-only** and intentionally simple: every message is a JSON object in the
/// `data:` field (no custom `event:` name). Consumers should parse `data` as JSON and switch on
/// the `type` discriminator.
///
/// - **First message**: a snapshot payload:
///   - `{"type":"snapshot","sessions":[<Session JSON>...]}` where `sessions` is the same shape as
///     `GET /api/sessions`.
/// - **Subsequent messages**: deltas, encoded as `ao_core::OrchestratorEvent` (tagged JSON with a
///   stable `type` field such as `"spawned"`, `"status_changed"`, etc.).
/// - **Keep-alive**: the server emits periodic keep-alives (SSE comments) so intermediaries don't
///   close idle connections. Browsers' `EventSource` does not surface these as messages.
/// - **Lagged receivers**: if a client falls behind the broadcast buffer, missed events are dropped
///   and the stream continues (no disconnect/crash).
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
