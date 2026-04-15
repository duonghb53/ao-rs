//! REST handlers for the dashboard API.

use crate::state::AppState;
use ao_core::{
    now_ms, restore_session as restore_core_session, AoError, CiStatus, MergeReadiness, PrState,
    PullRequest, ReviewDecision, Scm, Session, SessionId, SessionStatus, Workspace,
    WorkspaceCreateConfig,
};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::{
    extract::{Path, Query as AxumQuery, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::Duration;

/// Map session-lookup errors to HTTP status codes.
fn session_error_status(e: &AoError) -> StatusCode {
    match e {
        AoError::SessionNotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

#[derive(serde::Serialize)]
pub struct ApiErrorBody {
    error: String,
}

fn session_error_response(e: AoError) -> (StatusCode, Json<ApiErrorBody>) {
    let status = session_error_status(&e);
    (
        status,
        Json(ApiErrorBody {
            error: e.to_string(),
        }),
    )
}

/// GET /api/sessions — list sessions as JSON.
///
/// By default, killed/terminated sessions are excluded. Pass `?all=true`
/// to include them.
pub async fn list_sessions(
    State(state): State<AppState>,
    AxumQuery(query): AxumQuery<ListSessionsQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut sessions = state
        .sessions
        .list()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !query.all.unwrap_or(false) {
        sessions.retain(|s| !s.is_terminal());
    }

    let out = if query.pr.unwrap_or(false) {
        let enriched = enrich_sessions_with_pr(sessions, state.scm.clone())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        serde_json::to_value(enriched).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        serde_json::to_value(sessions).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct SpawnSessionBody {
    pub project_id: String,
    pub repo_path: String,
    pub task: String,
    #[serde(default = "default_default_branch")]
    pub default_branch: String,
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(default)]
    pub no_prompt: bool,
}

fn default_default_branch() -> String {
    "main".to_string()
}

fn default_agent() -> String {
    "claude-code".to_string()
}

/// POST /api/sessions/spawn — create a new session (worktree + tmux runtime).
pub async fn spawn_session(
    State(state): State<AppState>,
    Json(body): Json<SpawnSessionBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiErrorBody>)> {
    let repo_path = PathBuf::from(body.repo_path);
    if !repo_path.join(".git").exists() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiErrorBody {
                error: format!("not a git repo: {}", repo_path.display()),
            }),
        ));
    }

    let session_id = SessionId::new();
    let short_id: String = session_id.0.chars().take(8).collect();
    let branch = format!("ao-{short_id}");

    let workspace = ao_plugin_workspace_worktree::WorktreeWorkspace::new();
    let workspace_cfg = WorkspaceCreateConfig {
        project_id: body.project_id.clone(),
        session_id: short_id.clone(),
        branch: branch.clone(),
        repo_path: repo_path.clone(),
        default_branch: body.default_branch.clone(),
    };

    let workspace_path = workspace
        .create(&workspace_cfg)
        .await
        .map_err(session_error_response)?;

    let mut session = Session {
        id: session_id.clone(),
        project_id: body.project_id,
        status: SessionStatus::Spawning,
        agent: body.agent,
        agent_config: None,
        branch,
        task: body.task,
        workspace_path: Some(workspace_path.clone()),
        runtime_handle: None,
        runtime: "tmux".into(),
        activity: None,
        created_at: now_ms(),
        cost: None,
        issue_id: None,
        issue_url: None,
    };

    state
        .sessions
        .save(&session)
        .await
        .map_err(session_error_response)?;

    // Runtime: spawn tmux session running the agent.
    let launch_command = state.agent.launch_command(&session);
    let env = state.agent.environment(&session);
    let handle = state
        .runtime
        .create(&short_id, &workspace_path, &launch_command, &env)
        .await
        .map_err(session_error_response)?;

    session.runtime_handle = Some(handle.clone());
    session.status = SessionStatus::Working;
    state
        .sessions
        .save(&session)
        .await
        .map_err(session_error_response)?;

    if !body.no_prompt {
        // Let TUI initialize (mirrors CLI behavior).
        tokio::time::sleep(Duration::from_millis(3000)).await;
        let prompt = state.agent.initial_prompt(&session);
        let _ = state.runtime.send_message(&handle, &prompt).await;
    }

    serde_json::to_value(session).map(Json).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorBody {
                error: "failed to serialize session".to_string(),
            }),
        )
    })
}

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    #[serde(default)]
    pr: Option<bool>,
    /// Include killed/terminated sessions. Default: false (only active).
    #[serde(default)]
    all: Option<bool>,
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
        .map_err(|e| session_error_status(&e))?;
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
    // Board columns map to ao-ts-style attention buckets:
    // - pending (Backlog): CI still running / waiting on checks
    // - review (In review): open PR, human-review / idle PR lane (`pr_open`, `review_pending`)
    // - respond: CI red, changes requested, stuck, needs-input
    // - merge (Ready): green + mergeable
    // - working: agent still in pre-PR / coding phase
    //
    // See docs/state-machine.md — `SessionStatus::PrOpen` is "waiting for CI / review",
    // which belongs in **review**, not **working**, once CI is not actively pending.
    if session.is_terminal() {
        return "done".into();
    }

    if let Some(pr) = pr {
        if pr.state == PrState::Open && pr.mergeable && pr.ci_status == CiStatus::Passing {
            return "merge".into();
        }
        if pr.review_decision == ReviewDecision::ChangesRequested
            || pr.ci_status == CiStatus::Failing
        {
            return "respond".into();
        }
        if pr.review_decision == ReviewDecision::Pending {
            return "review".into();
        }
        if pr.ci_status == CiStatus::Pending {
            return "pending".into();
        }
        // Open PR: CI passing/unknown/none, not merge-ready — still the "in review" lane
        // (covers `ReviewDecision::None`, draft flow, branch protection, etc.).
        if pr.state == PrState::Open {
            return "review".into();
        }
    }

    // No PR row (`detect_pr` failed or not enriched) — use lifecycle status like the TS dashboard.
    match session.status {
        SessionStatus::PrOpen | SessionStatus::ReviewPending | SessionStatus::Approved => {
            "review".into()
        }
        SessionStatus::CiFailed | SessionStatus::ChangesRequested => "respond".into(),
        SessionStatus::Mergeable | SessionStatus::MergeFailed => "merge".into(),
        SessionStatus::NeedsInput | SessionStatus::Stuck => "respond".into(),
        _ => "working".into(),
    }
}

/// Max concurrent sessions being PR-enriched (each may spawn several `gh` calls).
const ENRICH_SESSION_CONCURRENCY: usize = 6;

async fn enrich_sessions_with_pr(
    sessions: Vec<Session>,
    scm: Arc<dyn Scm>,
) -> Result<Vec<DashboardSession>, ()> {
    let n = sessions.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    let sem = Arc::new(Semaphore::new(ENRICH_SESSION_CONCURRENCY));
    let mut join_set = tokio::task::JoinSet::new();

    for (idx, s) in sessions.into_iter().enumerate() {
        let scm = scm.clone();
        let sem = sem.clone();
        join_set.spawn(async move {
            let _permit = sem
                .acquire_owned()
                .await
                .expect("dashboard PR enrich semaphore");
            let dash = enrich_one_session(s, scm).await;
            (idx, dash)
        });
    }

    let mut pairs = Vec::with_capacity(n);
    while let Some(joined) = join_set.join_next().await {
        let (idx, dash) = joined.map_err(|_| ())?;
        pairs.push((idx, dash));
    }
    pairs.sort_by_key(|(i, _)| *i);
    Ok(pairs.into_iter().map(|(_, d)| d).collect())
}

async fn enrich_one_session(s: Session, scm: Arc<dyn Scm>) -> DashboardSession {
    let pr = match scm.detect_pr(&s).await {
        Ok(Some(pr)) => Some(enrich_pr(&scm, &pr).await),
        _ => None,
    };
    let level = attention_level(&s, pr.as_ref());
    DashboardSession {
        session: s,
        pr,
        attention_level: level,
    }
}

async fn enrich_pr(scm: &Arc<dyn Scm>, pr: &PullRequest) -> DashboardPr {
    let (state, ci, review, merge) = tokio::join!(
        scm.pr_state(pr),
        scm.ci_status(pr),
        scm.review_decision(pr),
        scm.mergeability(pr),
    );
    let state = state.unwrap_or(PrState::Open);
    let ci = ci.unwrap_or(CiStatus::None);
    let review = review.unwrap_or(ReviewDecision::None);
    let merge = merge.unwrap_or(MergeReadiness {
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
        .map_err(|e| session_error_status(&e))?;

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
        .map_err(|e| session_error_status(&e))?;

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

/// POST /api/sessions/:id/restore — restore a previously terminated session.
pub async fn restore_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiErrorBody>)> {
    let outcome = restore_core_session(
        &id,
        state.sessions.as_ref(),
        state.runtime.as_ref(),
        state.agent.as_ref(),
    )
    .await
    .map_err(session_error_response)?;

    serde_json::to_value(outcome.session)
        .map(Json)
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorBody {
                    error: "failed to serialize session".to_string(),
                }),
            )
        })
}

/// GET /api/sessions/:id/terminal — WebSocket interactive terminal (PTY + `tmux attach`).
///
/// Binary frames carry PTY output; client may send UTF-8 text or JSON `{"type":"resize","cols","rows"}`.
pub async fn terminal_ws(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, StatusCode> {
    let session = state
        .sessions
        .find_by_prefix(&id)
        .await
        .map_err(|e| session_error_status(&e))?;

    let handle = session
        .runtime_handle
        .as_deref()
        .ok_or(StatusCode::CONFLICT)?
        .to_string();

    Ok(ws.on_upgrade(move |socket| async move {
        stream_tmux_pty(socket, handle).await;
    }))
}

#[derive(serde::Deserialize)]
struct TerminalClientMsg {
    #[serde(rename = "type")]
    kind: String,
    cols: Option<u16>,
    rows: Option<u16>,
}

#[derive(Debug, PartialEq, Eq)]
enum TerminalClientAction {
    Resize { cols: u16, rows: u16 },
    InputBytes(Vec<u8>),
}

fn parse_terminal_client_action(msg: Message) -> Option<TerminalClientAction> {
    match msg {
        Message::Text(text) => {
            let text = text.to_string();
            // JSON control messages (resize). Any other text is treated as input bytes.
            if text.starts_with('{') {
                if let Ok(msg) = serde_json::from_str::<TerminalClientMsg>(&text) {
                    if msg.kind == "resize" {
                        if let (Some(cols), Some(rows)) = (msg.cols, msg.rows) {
                            return Some(TerminalClientAction::Resize { cols, rows });
                        }
                        return None;
                    }
                }
            }
            Some(TerminalClientAction::InputBytes(text.into_bytes()))
        }
        Message::Binary(bytes) => Some(TerminalClientAction::InputBytes(bytes.to_vec())),
        _ => None,
    }
}

async fn stream_tmux_pty(mut socket: WebSocket, handle: String) {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::io::{Read, Write};

    const WS_OUT_CAPACITY: usize = 128;
    const DROP_NOTICE_INTERVAL_MS: u64 = 1000;

    // ---- 1) Create PTY + spawn `tmux attach` inside it ----
    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("failed to open pty: {e}\n").into()))
                .await;
            return;
        }
    };

    let mut cmd = CommandBuilder::new("tmux");
    cmd.args(["attach", "-t", &handle]);

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    format!("failed to spawn tmux attach: {e}\n").into(),
                ))
                .await;
            return;
        }
    };

    // PTY IO is blocking; bridge through threads.
    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    format!("failed to clone pty reader: {e}\n").into(),
                ))
                .await;
            return;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    format!("failed to take pty writer: {e}\n").into(),
                ))
                .await;
            return;
        }
    };

    let master = pair.master;
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(WS_OUT_CAPACITY);
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(128);
    let dropped_chunks = Arc::new(AtomicU64::new(0));
    let dropped_chunks_reader = dropped_chunks.clone();

    // Reader thread
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Backpressure: if the websocket is slow, do not block the PTY reader thread.
                    // Dropping output is preferable to stalling the tmux session.
                    if out_tx.try_send(buf[..n].to_vec()).is_err() {
                        // channel full or closed; drop
                        dropped_chunks_reader.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Writer thread
    tokio::task::spawn_blocking(move || {
        while let Some(chunk) = in_rx.blocking_recv() {
            if writer.write_all(&chunk).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });

    // ---- 2) WS loop: forward PTY output + accept input/resize ----
    let mut drop_notice_tick =
        tokio::time::interval(Duration::from_millis(DROP_NOTICE_INTERVAL_MS));
    loop {
        tokio::select! {
            maybe_out = out_rx.recv() => {
                match maybe_out {
                    Some(bytes) => {
                        if socket.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = drop_notice_tick.tick() => {
                let dropped = dropped_chunks.swap(0, Ordering::Relaxed);
                if dropped > 0 {
                    // Keep as a JSON text frame so clients can render a compact notice.
                    let notice = serde_json::json!({
                        "type": "dropped",
                        "dropped_chunks": dropped,
                        "policy": "drop_newest"
                    });
                    let _ = socket.send(Message::Text(notice.to_string().into())).await;
                }
            }
            recv = socket.recv() => {
                match recv {
                    Some(Ok(msg)) => {
                        match msg {
                            Message::Close(_) => break,
                            msg => {
                                if let Some(action) = parse_terminal_client_action(msg) {
                                    match action {
                                        TerminalClientAction::Resize { cols, rows } => {
                                            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                                        }
                                        TerminalClientAction::InputBytes(bytes) => {
                                            let _ = in_tx.send(bytes).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None => break,
                    Some(Err(_)) => break,
                }
            }
        }
    }

    // Best-effort cleanup
    let _ = child.kill();
}

#[cfg(test)]
mod attention_tests {
    use super::{attention_level, DashboardPr};
    use ao_core::{now_ms, CiStatus, PrState, ReviewDecision, Session, SessionId, SessionStatus};

    fn sess(status: SessionStatus) -> Session {
        Session {
            id: SessionId("00000000-0000-0000-0000-000000000001".into()),
            project_id: "p".into(),
            status,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "br".into(),
            task: "t".into(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
        }
    }

    fn pr_fixture(
        state: PrState,
        ci: CiStatus,
        review: ReviewDecision,
        mergeable: bool,
    ) -> DashboardPr {
        DashboardPr {
            number: 1,
            url: String::new(),
            title: String::new(),
            owner: String::new(),
            repo: String::new(),
            branch: String::new(),
            base_branch: String::new(),
            is_draft: false,
            state,
            ci_status: ci,
            review_decision: review,
            mergeable,
            blockers: vec![],
        }
    }

    #[test]
    fn open_pr_passing_review_none_not_mergeable_is_review() {
        let s = sess(SessionStatus::PrOpen);
        let p = pr_fixture(
            PrState::Open,
            CiStatus::Passing,
            ReviewDecision::None,
            false,
        );
        assert_eq!(attention_level(&s, Some(&p)), "review");
    }

    #[test]
    fn open_pr_mergeable_green_is_merge() {
        let s = sess(SessionStatus::PrOpen);
        let p = pr_fixture(PrState::Open, CiStatus::Passing, ReviewDecision::None, true);
        assert_eq!(attention_level(&s, Some(&p)), "merge");
    }

    #[test]
    fn open_pr_ci_pending_is_pending_backlog() {
        let s = sess(SessionStatus::PrOpen);
        let p = pr_fixture(
            PrState::Open,
            CiStatus::Pending,
            ReviewDecision::None,
            false,
        );
        assert_eq!(attention_level(&s, Some(&p)), "pending");
    }

    #[test]
    fn session_pr_open_without_pr_row_is_review() {
        let s = sess(SessionStatus::PrOpen);
        assert_eq!(attention_level(&s, None), "review");
    }

    #[test]
    fn review_pending_status_without_pr_row_is_review() {
        let s = sess(SessionStatus::ReviewPending);
        assert_eq!(attention_level(&s, None), "review");
    }
}

#[cfg(test)]
mod terminal_ws_tests {
    use super::{parse_terminal_client_action, TerminalClientAction};
    use axum::extract::ws::Message;

    #[test]
    fn parse_resize_json() {
        let msg = Message::Text(r#"{"type":"resize","cols":120,"rows":40}"#.into());
        assert_eq!(
            parse_terminal_client_action(msg),
            Some(TerminalClientAction::Resize {
                cols: 120,
                rows: 40
            })
        );
    }

    #[test]
    fn parse_text_input_bytes() {
        let msg = Message::Text("ls -la\n".into());
        assert_eq!(
            parse_terminal_client_action(msg),
            Some(TerminalClientAction::InputBytes(b"ls -la\n".to_vec()))
        );
    }

    #[test]
    fn parse_binary_input_bytes() {
        let msg = Message::Binary(vec![0x1b, b'[', b'A'].into());
        assert_eq!(
            parse_terminal_client_action(msg),
            Some(TerminalClientAction::InputBytes(vec![0x1b, b'[', b'A']))
        );
    }

    #[test]
    fn resize_missing_fields_is_ignored() {
        let msg = Message::Text(r#"{"type":"resize","cols":80}"#.into());
        assert_eq!(parse_terminal_client_action(msg), None);
    }
}
