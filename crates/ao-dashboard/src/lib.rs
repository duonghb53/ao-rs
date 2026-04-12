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

use axum::{routing::get, routing::post, Router};
use state::AppState;
use tower_http::cors::CorsLayer;

/// Build the axum router with all dashboard routes.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/sessions", get(routes::list_sessions))
        .route("/api/sessions/{id}", get(routes::get_session))
        .route("/api/sessions/{id}/message", post(routes::send_message))
        .route("/api/sessions/{id}/kill", post(routes::kill_session))
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
    use ao_core::{OrchestratorEvent, SessionManager};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use tower::ServiceExt;

    fn test_state() -> AppState {
        // Use a unique temp dir per test invocation to avoid collision.
        let dir = std::env::temp_dir().join(format!("ao-dashboard-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sessions = Arc::new(SessionManager::new(dir));
        let (events_tx, _) = broadcast::channel(16);
        // Use a dummy runtime — tests that don't call send_message/kill don't need a real one.
        let runtime: Arc<dyn ao_core::Runtime> = Arc::new(DummyRuntime);
        AppState {
            sessions,
            events_tx,
            runtime,
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
