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
/// Each event is serialized as JSON in the SSE `data:` field. Clients
/// that lag behind the broadcast buffer get a retry hint rather than a
/// disconnect — the KeepAlive sends periodic pings so proxies don't
/// time out idle connections.
pub async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let json = serde_json::to_string(&event).unwrap_or_default();
            Some(Ok(Event::default().data(json)))
        }
        // Lagged: skip lost events, stream continues.
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
