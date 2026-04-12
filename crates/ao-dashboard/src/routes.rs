//! REST handlers for the dashboard API.

use crate::state::AppState;
use ao_core::AoError;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;

/// Map session-lookup errors to HTTP status codes.
fn session_error_status(e: AoError) -> StatusCode {
    match e {
        AoError::SessionNotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

/// GET /api/sessions — list all sessions as JSON.
pub async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let sessions = state
        .sessions
        .list()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    serde_json::to_value(sessions)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// GET /api/sessions/:id — single session by id or prefix.
pub async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let session = state
        .sessions
        .find_by_prefix(&id)
        .await
        .map_err(session_error_status)?;
    serde_json::to_value(session)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
pub struct MessageBody {
    pub message: String,
}

/// POST /api/sessions/:id/message — forward a message to the agent.
pub async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<MessageBody>,
) -> Result<StatusCode, StatusCode> {
    let session = state
        .sessions
        .find_by_prefix(&id)
        .await
        .map_err(session_error_status)?;

    let handle = session
        .runtime_handle
        .as_deref()
        .ok_or(StatusCode::CONFLICT)?;

    state
        .runtime
        .send_message(handle, &body.message)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::OK)
}

/// POST /api/sessions/:id/kill — terminate a session's runtime.
pub async fn kill_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let session = state
        .sessions
        .find_by_prefix(&id)
        .await
        .map_err(session_error_status)?;

    let handle = session
        .runtime_handle
        .as_deref()
        .ok_or(StatusCode::CONFLICT)?;

    state
        .runtime
        .destroy(handle)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::OK)
}
