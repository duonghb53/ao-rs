//! Dashboard API server for ao-rs.
//!
//! Exposes REST endpoints + SSE stream so a frontend (or `curl`) can
//! inspect and interact with running sessions without the CLI.
//!
//! No frontend is included — this crate is the API only. Wire it up
//! with `ao-rs dashboard` which starts the lifecycle loop and this
//! server concurrently.

pub mod routes;
pub mod sse;
pub mod state;

use axum::response::Html;
use axum::{routing::get, routing::post, Json, Router};
use serde_json::json;
use state::AppState;
use tower_http::cors::CorsLayer;

async fn dashboard_root() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>ao-dashboard</title></head>
<body style="font-family:system-ui,sans-serif;max-width:42rem;margin:2rem;line-height:1.5">
  <h1>ao-dashboard</h1>
  <p>REST API for <code>ao-rs</code>. Use the desktop UI (Tauri/Vite) and set its <strong>Dashboard URL</strong> to this origin, or call the endpoints below.</p>
  <ul>
    <li><a href="/api/sessions"><code>GET /api/sessions</code></a> — list sessions</li>
    <li><a href="/api/sessions?pr=true"><code>GET /api/sessions?pr=true</code></a> — list with PR enrichment</li>
    <li><code>GET /api/events</code> — SSE event stream</li>
    <li><code>GET /health</code> — liveness JSON</li>
  </ul>
</body>
</html>"#,
    )
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
        .route("/", get(dashboard_root))
        .route("/health", get(health))
        .route("/api/sessions", get(routes::list_sessions))
        .route("/api/sessions/spawn", post(routes::spawn_session))
        .route("/api/sessions/{id}", get(routes::get_session))
        .route("/api/sessions/{id}/message", post(routes::send_message))
        .route("/api/sessions/{id}/kill", post(routes::kill_session))
        .route("/api/sessions/{id}/restore", post(routes::restore_session))
        .route("/api/sessions/{id}/terminal", get(routes::terminal_ws))
        .route("/api/events", get(sse::event_stream))
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
    use ao_core::{OrchestratorEvent, Scm, Session, SessionManager};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use tokio::sync::broadcast;
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
        // Use a unique temp dir per test invocation to avoid collision.
        let dir = std::env::temp_dir().join(format!("ao-dashboard-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sessions = Arc::new(SessionManager::new(dir));
        let (events_tx, _) = broadcast::channel(16);
        // Use a dummy runtime — tests that don't call send_message/kill don't need a real one.
        let runtime: Arc<dyn ao_core::Runtime> = Arc::new(DummyRuntime);
        let scm: Arc<dyn Scm> = Arc::new(DummyScm);
        let agent: Arc<dyn ao_core::Agent> = Arc::new(DummyAgent);
        AppState {
            sessions,
            events_tx,
            runtime,
            scm,
            agent,
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
        let app = router(test_state());
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

        let body = axum::body::to_bytes(resp.into_body(), 32 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        // SSE format: "data: <json>\n\n"
        let first_data = text.lines().find(|l| l.starts_with("data: ")).unwrap_or("");
        assert!(first_data.contains("\"type\":\"snapshot\""));
        assert!(first_data.contains("\"sessions\""));
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
