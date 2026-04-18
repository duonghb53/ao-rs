//! REST handlers for the dashboard API.

use crate::state::AppState;
use ao_core::{
    is_orchestrator_session, now_ms, rate_limit as ao_rate_limit,
    restore_session as restore_core_session, spawn_orchestrator as core_spawn_orchestrator,
    AoConfig, AoError, CiStatus, IssueFilters, LoadedConfig, MergeReadiness,
    OrchestratorSpawnConfig, PrState, PullRequest, ReviewDecision, Scm, Session, SessionId,
    SessionStatus, Tracker, Workspace, WorkspaceCreateConfig,
};
use ao_plugin_tracker_github::GitHubTracker;
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
    /// Absolute path to the source repo.
    ///
    /// Optional since #163 — when absent (e.g. spawning from the
    /// Backlog card in the UI), the dashboard falls back to
    /// `config.projects[project_id].path` from the loaded `ao-rs.yaml`.
    #[serde(default)]
    pub repo_path: Option<String>,
    pub task: String,
    #[serde(default = "default_default_branch")]
    pub default_branch: String,
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(default)]
    pub no_prompt: bool,
    /// If set, records the parent orchestrator session id so the lifecycle
    /// loop can notify it when this worker changes state. See issue #169.
    #[serde(default)]
    pub spawned_by: Option<String>,
    /// Tracker issue identifier this session was spawned from, e.g. `"42"`.
    /// Persisted on the resulting `Session` so reactions and status views
    /// can correlate session → issue. See issue #163.
    #[serde(default)]
    pub issue_id: Option<String>,
    /// Canonical issue URL (e.g. `https://github.com/owner/repo/issues/42`).
    #[serde(default)]
    pub issue_url: Option<String>,
}

fn default_default_branch() -> String {
    "main".to_string()
}

fn default_agent() -> String {
    "claude-code".to_string()
}

/// Resolve `repo_path` for a spawn request.
///
/// Order of precedence:
/// 1. Explicit `repo_path` in the body (original behavior).
/// 2. `config.projects[project_id].path` when the dashboard was started
///    with a config (`AppState.config_path`) and the project is known.
///
/// Returns a 422 with a structured error if neither source resolves.
fn resolve_spawn_repo_path(
    state: &AppState,
    project_id: &str,
    body_repo_path: Option<&str>,
) -> Result<PathBuf, (StatusCode, Json<ApiErrorBody>)> {
    if let Some(p) = body_repo_path {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    let Some(config_path) = state.config_path.as_ref() else {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiErrorBody {
                error: "repo_path missing and dashboard was started without a config file"
                    .to_string(),
            }),
        ));
    };

    let LoadedConfig { config, .. } = AoConfig::load_from_or_default_with_warnings(config_path)
        .map_err(|e| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErrorBody {
                    error: format!("failed to load {}: {e}", config_path.display()),
                }),
            )
        })?;

    let project = config.projects.get(project_id).ok_or_else(|| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiErrorBody {
                error: format!(
                    "repo_path missing and project '{project_id}' is not in {}",
                    config_path.display()
                ),
            }),
        )
    })?;
    Ok(PathBuf::from(&project.path))
}

/// POST /api/sessions/spawn — create a new session (worktree + tmux runtime).
pub async fn spawn_session(
    State(state): State<AppState>,
    Json(body): Json<SpawnSessionBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiErrorBody>)> {
    let repo_path = resolve_spawn_repo_path(&state, &body.project_id, body.repo_path.as_deref())?;
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
        symlinks: vec![],
        post_create: vec![],
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
        issue_id: body.issue_id.clone(),
        issue_url: body.issue_url.clone(),
        claimed_pr_number: None,
        claimed_pr_url: None,
        initial_prompt_override: None,
        spawned_by: body.spawned_by.clone().map(ao_core::SessionId),
        last_merge_conflict_dispatched: None,
        last_review_backlog_fingerprint: None,
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
        state.workspace.as_ref(),
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

// ---------------------------------------------------------------------------
// Orchestrator routes (issue #165 Slice 4)
// ---------------------------------------------------------------------------

/// GET /api/orchestrators — list sessions that are classified as orchestrators.
///
/// Filtered from the full session list via `is_orchestrator_session` so the
/// dashboard can show a separate panel for meta-agents without a dedicated
/// session role field on disk.
pub async fn list_orchestrators(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let all = state
        .sessions
        .list()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let orchestrators: Vec<Session> = all.into_iter().filter(is_orchestrator_session).collect();
    Ok(Json(
        serde_json::to_value(orchestrators).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct SpawnOrchestratorBody {
    pub project_id: String,
    pub repo_path: String,
    #[serde(default = "default_default_branch")]
    pub default_branch: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub no_prompt: bool,
}

fn default_port() -> u16 {
    3000
}

/// POST /api/orchestrators — spawn a new orchestrator session.
///
/// Delegates to `ao_core::spawn_orchestrator` after loading `ao-rs.yaml`
/// from `repo_path`. Returns the persisted `Session` as JSON.
pub async fn spawn_orchestrator_route(
    State(state): State<AppState>,
    Json(body): Json<SpawnOrchestratorBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiErrorBody>)> {
    let repo_path = PathBuf::from(&body.repo_path);
    if !repo_path.join(".git").exists() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiErrorBody {
                error: format!("not a git repo: {}", repo_path.display()),
            }),
        ));
    }

    let config_path = AoConfig::path_in(&repo_path);
    let LoadedConfig { config, .. } = AoConfig::load_from_or_default_with_warnings(&config_path)
        .map_err(|e| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErrorBody {
                    error: format!("failed to load {}: {e}", config_path.display()),
                }),
            )
        })?;

    let project_config = config
        .projects
        .get(&body.project_id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErrorBody {
                    error: format!(
                        "project '{}' is not configured in {}",
                        body.project_id,
                        config_path.display()
                    ),
                }),
            )
        })?;

    let agent_name = body
        .agent
        .clone()
        .or_else(|| {
            project_config
                .orchestrator
                .as_ref()
                .and_then(|r| r.agent.clone())
                .or_else(|| project_config.agent.clone())
        })
        .or_else(|| config.defaults.as_ref().map(|d| d.agent.clone()))
        .unwrap_or_else(|| "claude-code".to_string());
    let runtime_name = body
        .runtime
        .clone()
        .or_else(|| project_config.runtime.clone())
        .or_else(|| config.defaults.as_ref().map(|d| d.runtime.clone()))
        .unwrap_or_else(|| "tmux".to_string());

    let workspace = ao_plugin_workspace_worktree::WorktreeWorkspace::new();

    let session = core_spawn_orchestrator(
        OrchestratorSpawnConfig {
            project_id: &body.project_id,
            project_config: &project_config,
            config: &config,
            config_path: Some(config_path.clone()),
            port: body.port,
            agent_name: &agent_name,
            runtime_name: &runtime_name,
            repo_path,
            default_branch: body.default_branch,
            no_prompt: body.no_prompt,
        },
        state.sessions.as_ref(),
        &workspace,
        state.agent.as_ref(),
        state.runtime.as_ref(),
    )
    .await
    .map_err(session_error_response)?;

    serde_json::to_value(session).map(Json).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorBody {
                error: "failed to serialize session".to_string(),
            }),
        )
    })
}

// ---------------------------------------------------------------------------
// Issues backlog route (issue #163)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListIssuesQuery {
    /// Limit to a single project. Omitted → aggregate across all
    /// configured projects.
    #[serde(default)]
    pub project_id: Option<String>,
    /// `"open"`, `"closed"`, or `"all"`. Defaults to `"open"`.
    #[serde(default)]
    pub state: Option<String>,
    /// Comma-separated labels, forwarded to the tracker.
    #[serde(default)]
    pub labels: Option<String>,
    /// Per-project cap on issues returned. Tracker picks a default (30)
    /// when absent.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct DashboardIssue {
    pub project_id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub labels: Vec<String>,
    /// `owner/repo` slug the issue belongs to.
    pub repo: String,
    /// `"open"`, `"closed"`, `"cancelled"`, `"in_progress"` (tracker-dependent).
    pub state: String,
}

/// Split `owner/repo` into `(owner, repo)`. Returns `None` if the slug
/// does not match that shape — mirrors the validation in
/// `AoConfig::validate`. Split into a helper so the new issues route
/// and future Tracker-fan-out callers share the same rule (and we can
/// unit-test it without a full config fixture).
fn parse_repo_slug(slug: &str) -> Option<(String, String)> {
    let mut parts = slug.splitn(2, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn issue_state_label(state: ao_core::IssueState) -> &'static str {
    match state {
        ao_core::IssueState::Open => "open",
        ao_core::IssueState::InProgress => "in_progress",
        ao_core::IssueState::Closed => "closed",
        ao_core::IssueState::Cancelled => "cancelled",
    }
}

fn issue_filters_from_query(q: &ListIssuesQuery) -> IssueFilters {
    let labels = q
        .labels
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    IssueFilters {
        state: q.state.clone(),
        labels,
        assignee: None,
        limit: q.limit,
    }
}

/// GET /api/issues — aggregate open issues across configured projects.
///
/// Loads `ao-rs.yaml` on every call so newly added projects show up
/// without restarting the dashboard. If a single project errors
/// (invalid repo slug, cooldown active, missing `gh`), we log and skip
/// it so the rest of the list still loads.
pub async fn list_issues_route(
    State(state): State<AppState>,
    AxumQuery(query): AxumQuery<ListIssuesQuery>,
) -> Result<Json<Vec<DashboardIssue>>, (StatusCode, Json<ApiErrorBody>)> {
    let Some(config_path) = state.config_path.as_ref() else {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiErrorBody {
                error: "dashboard was started without a config path; \
                        GET /api/issues requires a loaded ao-rs.yaml"
                    .to_string(),
            }),
        ));
    };

    let LoadedConfig { config, .. } = AoConfig::load_from_or_default_with_warnings(config_path)
        .map_err(|e| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErrorBody {
                    error: format!("failed to load {}: {e}", config_path.display()),
                }),
            )
        })?;

    if ao_rate_limit::in_cooldown_now() {
        tracing::debug!(
            "/api/issues: rate-limit cooldown active; returning empty list to avoid hammering gh"
        );
        return Ok(Json(Vec::new()));
    }

    let filters = issue_filters_from_query(&query);
    let mut out: Vec<DashboardIssue> = Vec::new();
    for (project_id, project) in config.projects.iter() {
        if let Some(filter_id) = query.project_id.as_deref() {
            if filter_id != project_id.as_str() {
                continue;
            }
        }

        let Some((owner, repo)) = parse_repo_slug(&project.repo) else {
            tracing::warn!(
                "/api/issues: skipping project {} with invalid repo slug {:?}",
                project_id,
                project.repo
            );
            continue;
        };

        let tracker = GitHubTracker::new(owner, repo);
        let issues = match tracker.list_issues(&filters).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "/api/issues: tracker.list_issues failed for {}: {}",
                    project_id,
                    e
                );
                continue;
            }
        };

        for issue in issues {
            let number = issue.id.parse::<u64>().unwrap_or(0);
            out.push(DashboardIssue {
                project_id: project_id.clone(),
                number,
                title: issue.title,
                url: issue.url,
                labels: issue.labels,
                repo: project.repo.clone(),
                state: issue_state_label(issue.state).to_string(),
            });
        }
    }

    // Stable order: project_id asc, then issue number desc (newest first within a project).
    out.sort_by(|a, b| {
        a.project_id
            .cmp(&b.project_id)
            .then_with(|| b.number.cmp(&a.number))
    });

    Ok(Json(out))
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
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: None,
            spawned_by: None,
            last_merge_conflict_dispatched: None,
            last_review_backlog_fingerprint: None,
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

#[cfg(test)]
mod issues_route_tests {
    use super::{issue_filters_from_query, parse_repo_slug, ListIssuesQuery};

    #[test]
    fn parse_repo_slug_happy_path() {
        assert_eq!(
            parse_repo_slug("owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
        );
    }

    #[test]
    fn parse_repo_slug_rejects_malformed() {
        assert!(parse_repo_slug("no-slash").is_none());
        assert!(parse_repo_slug("/trailing").is_none());
        assert!(parse_repo_slug("leading/").is_none());
        assert!(parse_repo_slug("  /  ").is_none());
        // Extra path segments (would break `--repo`) are rejected.
        assert!(parse_repo_slug("owner/repo/extra").is_none());
    }

    #[test]
    fn issue_filters_parses_comma_separated_labels() {
        let q = ListIssuesQuery {
            project_id: None,
            state: Some("open".into()),
            labels: Some("bug, good first issue , ,enhancement".into()),
            limit: Some(10),
        };
        let f = issue_filters_from_query(&q);
        assert_eq!(f.state.as_deref(), Some("open"));
        assert_eq!(f.labels, vec!["bug", "good first issue", "enhancement"]);
        assert_eq!(f.limit, Some(10));
    }

    #[test]
    fn issue_filters_empty_labels_when_absent() {
        let q = ListIssuesQuery {
            project_id: None,
            state: None,
            labels: None,
            limit: None,
        };
        let f = issue_filters_from_query(&q);
        assert!(f.labels.is_empty());
    }
}
