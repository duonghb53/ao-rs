//! SSE event stream for real-time dashboard updates.

use crate::state::AppState;
use ao_core::{attention_level, DashboardSession};
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
///   - `{"type":"snapshot","sessions":[<DashboardSession JSON>...]}` where each entry carries the
///     same `pr` + `attention_level` fields as `GET /api/sessions?pr=true`. The PR enrichment is
///     pulled from the lifecycle's shared `pr_enrichment_payload` cache so freshly connected
///     clients don't need a follow-up `?pr=true` HTTP fetch.
/// - **Subsequent messages**: deltas, encoded as `ao_core::OrchestratorEvent` (tagged JSON with a
///   stable `type` field such as `"spawned"`, `"status_changed"`, etc.).
/// - **Keep-alive**: the server emits periodic keep-alives (SSE comments) so intermediaries don't
///   close idle connections. Browsers' `EventSource` does not surface these as messages.
/// - **Lagged receivers**: if a client falls behind the broadcast buffer, missed events are dropped
///   and the stream continues (no disconnect/crash).
pub async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let sessions = state.sessions.list().await.unwrap_or_else(|e| {
        tracing::error!("SSE snapshot: failed to list sessions: {e}");
        Vec::new()
    });
    // Snapshot the lifecycle's per-session PR enrichment so a freshly
    // connected client sees the current PR state without an extra
    // `?pr=true` HTTP fetch. Falls back to an empty map when the
    // dashboard runs without a lifecycle loop attached.
    let pr_enrichment = state
        .pr_enrichment_payload
        .as_ref()
        .map(|payload| {
            let map = payload.lock().unwrap_or_else(|e| {
                tracing::error!(
                    "pr_enrichment_payload mutex poisoned; recovering inner state: {e}"
                );
                e.into_inner()
            });
            map.clone()
        })
        .unwrap_or_default();

    let dashboard_sessions: Vec<DashboardSession> = sessions
        .into_iter()
        .map(|session| {
            let pr = pr_enrichment.get(&session.id).cloned();
            let attention = attention_level(&session, pr.as_ref());
            DashboardSession {
                session,
                pr,
                attention_level: attention,
            }
        })
        .collect();

    let snapshot_json = serde_json::json!({
        "type": "snapshot",
        "sessions": dashboard_sessions,
    });
    // An empty snapshot frame would silently strand newly-connected clients
    // (browsers skip empty `data:`). Fall back to `{"type":"snapshot","sessions":[]}`
    // and log loudly so the misbehaviour is observable.
    let snapshot = serde_json::to_string(&snapshot_json).unwrap_or_else(|e| {
        tracing::error!("SSE snapshot: failed to serialize snapshot frame: {e}");
        r#"{"type":"snapshot","sessions":[]}"#.to_string()
    });

    let rx = state.events_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(json) => Some(Ok(Event::default().data(json))),
            Err(e) => {
                tracing::warn!("SSE: dropping event that failed to serialize: {e}");
                None
            }
        },
        // Lagged: skip lost events, stream continues.
        Err(_) => None,
    });

    Sse::new(tokio_stream::once(Ok(Event::default().data(snapshot))).chain(stream))
        .keep_alive(KeepAlive::default())
}
