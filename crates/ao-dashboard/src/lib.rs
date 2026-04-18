//! Dashboard server for ao-rs: REST API + SSE stream + embedded React UI.
//!
//! The React UI (built from `crates/ao-desktop/ui/`) is embedded at compile
//! time via `rust-embed`. Served at `/`; API endpoints live under `/api/`.
//!
//! Build the UI first if you haven't:
//!   cd crates/ao-desktop/ui && npm install && npm run build

pub mod routes;
pub mod sse;
pub mod state;

use axum::body::Body;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, routing::post, Json, Router};
use rust_embed::Embed;
use serde_json::json;
use state::AppState;
use tower_http::cors::CorsLayer;

/// Embedded React UI built from `crates/ao-desktop/ui/dist/`.
#[derive(Embed)]
#[folder = "../ao-desktop/ui/dist/"]
struct Assets;

/// Serve an embedded static file, or fall back to `index.html` for SPA routes.
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    serve_asset(path)
}

fn serve_asset(path: &str) -> Response {
    match Assets::get(path) {
        Some(content) => {
            let mime = content.metadata.mimetype();
            Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .body(Body::from(content.data.into_owned()))
                .unwrap()
        }
        None => {
            // SPA fallback — unknown paths serve index.html so React Router works.
            match Assets::get("index.html") {
                Some(index) => Response::builder()
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(Body::from(index.data.into_owned()))
                    .unwrap(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "service": "ao-dashboard",
    }))
}

/// Build the axum router with all dashboard routes.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/sessions", get(routes::list_sessions))
        .route("/api/sessions/spawn", post(routes::spawn_session))
        .route("/api/sessions/{id}", get(routes::get_session))
        .route("/api/sessions/{id}/message", post(routes::send_message))
        .route("/api/sessions/{id}/kill", post(routes::kill_session))
        .route("/api/sessions/{id}/restore", post(routes::restore_session))
        .route("/api/sessions/{id}/terminal", get(routes::terminal_ws))
        .route(
            "/api/orchestrators",
            get(routes::list_orchestrators).post(routes::spawn_orchestrator_route),
        )
        .route("/api/issues", get(routes::list_issues_route))
        .route("/api/events", get(sse::event_stream))
        .fallback(static_handler)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Start the dashboard server on the given port.
pub async fn run_server(state: AppState, port: u16) -> std::io::Result<()> {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    tracing::info!("dashboard listening on port {port}");
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{OrchestratorEvent, Scm, Session, SessionManager, SessionStatus};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use tokio::time::{timeout, Duration};
    use tokio_stream::StreamExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn spawn_route_exists_and_returns_json_error() {
        let app = router(test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/api/sessions/spawn")
            .header("content-type", "application/json")
            // repo_path is not a git repo in this test state; we should get a structured error.
            .body(Body::from(
                r#"{"project_id":"demo","repo_path":"/tmp/not-a-repo","task":"x","no_prompt":true}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    fn test_state() -> AppState {
        test_state_with_broadcast_capacity(16)
    }

    fn test_state_with_broadcast_capacity(capacity: usize) -> AppState {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Use a unique temp dir per test invocation to avoid collision between tests
        // that write sessions to disk.
        let dir =
            std::env::temp_dir().join(format!("ao-dashboard-test-{}-{}", std::process::id(), n));
        let _ = std::fs::create_dir_all(&dir);
        let sessions = Arc::new(SessionManager::new(dir));
        let (events_tx, _) = broadcast::channel(capacity);
        // Use a dummy runtime — tests that don't call send_message/kill don't need a real one.
        let runtime: Arc<dyn ao_core::Runtime> = Arc::new(DummyRuntime);
        let scm: Arc<dyn Scm> = Arc::new(DummyScm);
        let agent: Arc<dyn ao_core::Agent> = Arc::new(DummyAgent);
        let workspace: Arc<dyn ao_core::Workspace> = Arc::new(DummyWorkspace);
        AppState {
            sessions,
            events_tx,
            runtime,
            scm,
            agent,
            workspace,
            config_path: None,
        }
    }

    struct DummyWorkspace;

    #[async_trait::async_trait]
    impl ao_core::Workspace for DummyWorkspace {
        async fn create(
            &self,
            _cfg: &ao_core::WorkspaceCreateConfig,
        ) -> ao_core::Result<std::path::PathBuf> {
            Ok(std::path::PathBuf::from("/tmp/dummy-ws"))
        }
        async fn destroy(&self, _workspace_path: &std::path::Path) -> ao_core::Result<()> {
            Ok(())
        }
    }

    struct DummyAgent;

    #[async_trait::async_trait]
    impl ao_core::Agent for DummyAgent {
        fn launch_command(&self, _s: &Session) -> String {
            "dummy".into()
        }
        fn environment(&self, _s: &Session) -> Vec<(String, String)> {
            vec![]
        }
        fn initial_prompt(&self, _s: &Session) -> String {
            "".into()
        }
    }

    struct DummyRuntime;

    #[async_trait::async_trait]
    impl ao_core::Runtime for DummyRuntime {
        async fn create(
            &self,
            _id: &str,
            _cwd: &std::path::Path,
            _cmd: &str,
            _env: &[(String, String)],
        ) -> ao_core::Result<String> {
            Ok("dummy".into())
        }
        async fn send_message(&self, _handle: &str, _msg: &str) -> ao_core::Result<()> {
            Ok(())
        }
        async fn is_alive(&self, _handle: &str) -> ao_core::Result<bool> {
            Ok(false)
        }
        async fn destroy(&self, _handle: &str) -> ao_core::Result<()> {
            Ok(())
        }
    }

    struct DummyScm;

    #[async_trait::async_trait]
    impl Scm for DummyScm {
        fn name(&self) -> &str {
            "dummy"
        }

        async fn detect_pr(
            &self,
            _session: &Session,
        ) -> ao_core::Result<Option<ao_core::PullRequest>> {
            Ok(None)
        }
        async fn pr_state(&self, _pr: &ao_core::PullRequest) -> ao_core::Result<ao_core::PrState> {
            Ok(ao_core::PrState::Open)
        }
        async fn ci_checks(
            &self,
            _pr: &ao_core::PullRequest,
        ) -> ao_core::Result<Vec<ao_core::CheckRun>> {
            Ok(vec![])
        }
        async fn ci_status(
            &self,
            _pr: &ao_core::PullRequest,
        ) -> ao_core::Result<ao_core::CiStatus> {
            Ok(ao_core::CiStatus::None)
        }
        async fn reviews(
            &self,
            _pr: &ao_core::PullRequest,
        ) -> ao_core::Result<Vec<ao_core::Review>> {
            Ok(vec![])
        }
        async fn review_decision(
            &self,
            _pr: &ao_core::PullRequest,
        ) -> ao_core::Result<ao_core::ReviewDecision> {
            Ok(ao_core::ReviewDecision::None)
        }
        async fn pending_comments(
            &self,
            _pr: &ao_core::PullRequest,
        ) -> ao_core::Result<Vec<ao_core::ReviewComment>> {
            Ok(vec![])
        }
        async fn mergeability(
            &self,
            _pr: &ao_core::PullRequest,
        ) -> ao_core::Result<ao_core::MergeReadiness> {
            Ok(ao_core::MergeReadiness {
                mergeable: false,
                ci_passing: false,
                approved: false,
                no_conflicts: false,
                blockers: vec!["dummy".into()],
            })
        }
        async fn merge(
            &self,
            _pr: &ao_core::PullRequest,
            _method: Option<ao_core::MergeMethod>,
        ) -> ao_core::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn list_sessions_empty() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn list_sessions_pr_true_empty() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions?pr=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn health_ok() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn root_ok() {
        let app = router(test_state());
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn events_starts_with_snapshot() {
        let state = test_state();
        let app = router(state.clone());

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let mut jsons = read_n_sse_data_jsons(resp.into_body(), 1).await;
        assert_eq!(jsons.len(), 1);
        let first = jsons.pop().unwrap();
        assert_eq!(first["type"], "snapshot");
        assert!(first.get("sessions").is_some());
    }

    #[tokio::test]
    async fn events_snapshot_then_one_delta_serializes() {
        let state = test_state();
        let events_tx = state.events_tx.clone();
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Send a delta after the stream is created.
        let _ = events_tx.send(OrchestratorEvent::Spawned {
            id: ao_core::SessionId("delta-1".into()),
            project_id: "demo".into(),
        });

        let jsons = read_n_sse_data_jsons(resp.into_body(), 2).await;
        assert_eq!(jsons[0]["type"], "snapshot");
        assert_eq!(jsons[1]["type"], "spawned");
        assert_eq!(jsons[1]["id"], "delta-1");
        assert_eq!(jsons[1]["project_id"], "demo");
    }

    #[tokio::test]
    async fn events_lagged_broadcast_doesnt_break_stream() {
        // Capacity 1 makes it easy to trigger Lagged on the receiver.
        let state = test_state_with_broadcast_capacity(1);
        let events_tx = state.events_tx.clone();
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Spam events so the receiver falls behind and yields Err(Lagged(_)).
        for i in 0..20 {
            let _ = events_tx.send(OrchestratorEvent::Spawned {
                id: ao_core::SessionId(format!("spam-{i}")),
                project_id: "demo".into(),
            });
        }
        // Send a final event we expect to still observe after lag.
        let _ = events_tx.send(OrchestratorEvent::Spawned {
            id: ao_core::SessionId("final".into()),
            project_id: "demo".into(),
        });

        let jsons = read_n_sse_data_jsons(resp.into_body(), 2).await;
        assert_eq!(jsons[0]["type"], "snapshot");
        // We don't assert how many spams were dropped; only that the stream continues to yield a valid delta.
        assert_eq!(jsons[1]["type"], "spawned");
        assert_eq!(jsons[1]["id"], "final");
    }

    async fn read_n_sse_data_jsons(body: Body, n: usize) -> Vec<Value> {
        let mut buf = String::new();
        let mut stream = body.into_data_stream();

        let read_fut = async {
            let mut out = Vec::with_capacity(n);
            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));

                // SSE events are separated by a blank line. We only care about `data:` lines.
                while let Some(idx) = buf.find("\n\n") {
                    let event_block = buf[..idx].to_string();
                    buf.drain(..idx + 2);

                    for line in event_block.lines() {
                        if let Some(rest) = line.strip_prefix("data: ") {
                            if let Ok(v) = serde_json::from_str::<Value>(rest) {
                                out.push(v);
                                if out.len() >= n {
                                    return out;
                                }
                            }
                        }
                    }
                }
            }
            out
        };

        timeout(Duration::from_millis(500), read_fut)
            .await
            .unwrap_or_else(|_| vec![])
    }

    #[tokio::test]
    async fn get_session_not_found() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn send_message_session_not_found() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sessions/nonexistent/message")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn kill_session_not_found() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sessions/nonexistent/kill")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn orchestrators_list_empty() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestrators")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn orchestrators_list_filters_out_workers() {
        use ao_core::{now_ms, SessionId};
        let state = test_state();

        // Worker session (uuid-style id) should be excluded.
        let worker = Session {
            id: SessionId("deadbeef-aaaa".into()),
            project_id: "demo".into(),
            status: SessionStatus::Working,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "ao-deadbeef".into(),
            task: "work".into(),
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
        };
        // Orchestrator session should be included.
        let orch = Session {
            id: SessionId("demo-orchestrator-1".into()),
            project_id: "demo".into(),
            status: SessionStatus::Working,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "orchestrator/demo-orchestrator-1".into(),
            task: "orchestrator".into(),
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
        };
        state.sessions.save(&worker).await.unwrap();
        state.sessions.save(&orch).await.unwrap();

        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestrators")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["id"], "demo-orchestrator-1");
    }

    #[tokio::test]
    async fn issues_route_without_config_path_returns_422() {
        // Default test_state leaves config_path = None.
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/issues")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["error"].as_str().is_some_and(|s| s.contains("config")),
            "expected config-related error, got {:?}",
            json
        );
    }

    #[tokio::test]
    async fn issues_route_with_empty_config_returns_empty_array() {
        // Write a valid but project-less config file and point the dashboard at it.
        let mut state = test_state();
        let dir = std::env::temp_dir().join(format!(
            "ao-dashboard-issues-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("ao-rs.yaml");
        std::fs::write(&config_path, "port: 3000\nprojects: {}\n").unwrap();
        state.config_path = Some(config_path);

        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/issues")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn spawn_without_repo_path_and_without_config_returns_422() {
        // Default test_state has config_path = None.
        let app = router(test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/api/sessions/spawn")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"project_id":"demo","task":"x","no_prompt":true,"issue_id":"42"}"#,
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["error"]
                .as_str()
                .is_some_and(|s| s.contains("repo_path")),
            "expected repo_path error, got {:?}",
            json
        );
    }

    #[tokio::test]
    async fn spawn_accepts_issue_id_and_url_fields() {
        let app = router(test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/api/sessions/spawn")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"project_id":"demo","repo_path":"/tmp/not-a-repo","task":"x","no_prompt":true,"issue_id":"42","issue_url":"https://example.com/1"}"#,
            ))
            .unwrap();
        // Still 422 because repo_path is not a git repo; the assertion is
        // that the body *parses* with the new optional fields.
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn orchestrators_spawn_bad_repo_path_returns_422() {
        let app = router(test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/api/orchestrators")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"project_id":"demo","repo_path":"/tmp/ao-rs-not-a-repo"}"#,
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn orchestrator_event_serializes_as_tagged_json() {
        let event = OrchestratorEvent::Spawned {
            id: ao_core::SessionId("abc".into()),
            project_id: "demo".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "spawned");
        assert_eq!(json["id"], "abc");
        assert_eq!(json["project_id"], "demo");
    }
}
