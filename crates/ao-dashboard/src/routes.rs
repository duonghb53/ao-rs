//! REST handlers for the dashboard API.

use crate::state::AppState;
use ao_core::{
    AoError, CiStatus, MergeReadiness, PrState, PullRequest, ReviewDecision, Scm, Session,
};
use axum::{
    extract::{Path, Query as AxumQuery, State},
    http::StatusCode,
    response::Json,
};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use serde::Deserialize;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

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
    AxumQuery(query): AxumQuery<ListSessionsQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let sessions = state
        .sessions
        .list()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let out = if query.pr.unwrap_or(false) {
        let enriched = enrich_sessions_with_pr(sessions, state.scm.clone()).await;
        serde_json::to_value(enriched).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        serde_json::to_value(sessions).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    #[serde(default)]
    pr: Option<bool>,
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

// ---------------------------------------------------------------------------
// Enrichment helpers (Slice 6)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
struct DashboardPr {
    number: u32,
    url: String,
    title: String,
    owner: String,
    repo: String,
    branch: String,
    base_branch: String,
    is_draft: bool,

    state: PrState,
    ci_status: CiStatus,
    review_decision: ReviewDecision,
    mergeable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blockers: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct DashboardSession {
    #[serde(flatten)]
    session: Session,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pr: Option<DashboardPr>,
    attention_level: String,
}

fn attention_level(session: &Session, pr: Option<&DashboardPr>) -> String {
    // Mirrors ao-ts "working/pending/review/respond/merge/done" at a coarse level.
    if session.is_terminal() {
        return "done".into();
    }

    if let Some(pr) = pr {
        if pr.state == PrState::Open && pr.mergeable && pr.ci_status == CiStatus::Passing {
            return "merge".into();
        }
        if pr.review_decision == ReviewDecision::ChangesRequested || pr.ci_status == CiStatus::Failing {
            return "respond".into();
        }
        if pr.review_decision == ReviewDecision::Pending {
            return "review".into();
        }
        if pr.ci_status == CiStatus::Pending {
            return "pending".into();
        }
    }

    "working".into()
}

async fn enrich_sessions_with_pr(sessions: Vec<Session>, scm: Arc<dyn Scm>) -> Vec<DashboardSession> {
    let mut out = Vec::with_capacity(sessions.len());
    for s in sessions {
        let pr = match scm.detect_pr(&s).await {
            Ok(Some(pr)) => Some(enrich_pr(&scm, &pr).await),
            _ => None,
        };
        let level = attention_level(&s, pr.as_ref());
        out.push(DashboardSession {
            session: s,
            pr,
            attention_level: level,
        });
    }
    out
}

async fn enrich_pr(scm: &Arc<dyn Scm>, pr: &PullRequest) -> DashboardPr {
    let state = scm.pr_state(pr).await.unwrap_or(PrState::Open);
    let ci = scm.ci_status(pr).await.unwrap_or(CiStatus::None);
    let review = scm.review_decision(pr).await.unwrap_or(ReviewDecision::None);
    let merge = scm
        .mergeability(pr)
        .await
        .unwrap_or(MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: false,
            no_conflicts: false,
            blockers: vec!["mergeability probe failed".to_string()],
        });
    DashboardPr {
        number: pr.number,
        url: pr.url.clone(),
        title: pr.title.clone(),
        owner: pr.owner.clone(),
        repo: pr.repo.clone(),
        branch: pr.branch.clone(),
        base_branch: pr.base_branch.clone(),
        is_draft: pr.is_draft,
        state,
        ci_status: ci,
        review_decision: review,
        mergeable: merge.mergeable,
        blockers: merge.blockers,
    }
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

/// GET /api/sessions/:id/terminal — websocket stream of captured tmux output.
///
/// Phase 3.2: read-only terminal proxy. Sends periodic full-screen snapshots
/// captured via `tmux capture-pane -p`. Input is not supported yet.
pub async fn terminal_ws(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, StatusCode> {
    let session = state
        .sessions
        .find_by_prefix(&id)
        .await
        .map_err(session_error_status)?;

    let handle = session
        .runtime_handle
        .as_deref()
        .ok_or(StatusCode::CONFLICT)?
        .to_string();

    Ok(ws.on_upgrade(move |socket| async move {
        stream_tmux_capture(socket, handle).await;
    }))
}

async fn stream_tmux_capture(mut socket: WebSocket, handle: String) {
    // Best-effort loop: capture pane every 500ms and send to client.
    // If client disconnects or tmux errors, exit silently.
    loop {
        let out = Command::new("tmux")
            .args(["capture-pane", "-t", &handle, "-p"])
            .output()
            .await;

        let snapshot = match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                let msg = format!(
                    "tmux capture-pane failed for handle {handle} (exit={}):\n{}\n",
                    o.status.code().unwrap_or(-1),
                    stderr.trim()
                );
                let _ = socket.send(Message::Text(msg.into())).await;
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: 1011,
                        reason: "tmux capture failed".into(),
                    })))
                    .await;
                return;
            }
            Err(e) => {
                let msg = format!("failed to spawn tmux for handle {handle}: {e}\n");
                let _ = socket.send(Message::Text(msg.into())).await;
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: 1011,
                        reason: "tmux spawn failed".into(),
                    })))
                    .await;
                return;
            }
        };

        if socket.send(Message::Text(snapshot.into())).await.is_err() {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }
}
