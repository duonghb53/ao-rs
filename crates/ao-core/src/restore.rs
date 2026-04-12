//! Restore a previously-terminated session back into a live runtime.
//!
//! Mirrors `restore()` in `packages/core/src/session-manager.ts` (`restore`
//! starting at line 2254), stripped to what Slice 1 needs:
//!
//! 1. Find the session on disk by full uuid or a short-id prefix.
//! 2. **Enrich with live runtime state** — if the stored status still says
//!    e.g. `working` but the tmux session is actually gone, flip it to
//!    `terminated` in-memory so the `is_restorable` check passes. This
//!    matches `enrichSessionWithRuntimeState(session, plugins, true)` in TS.
//! 3. Refuse restore if the session is non-restorable (e.g. `merged`).
//! 4. Verify the workspace path still exists on disk. Slice 1 does not
//!    auto-recreate worktrees — if it's gone, the user has to `ao-rs spawn`
//!    a fresh session. (TS has an optional `workspace.restore` plugin hook
//!    for this; Phase D keeps the trait surface small.)
//! 5. Best-effort `runtime.destroy` on the old handle in case it's still
//!    lingering (tmux can survive the agent process exiting — see the
//!    `// step 6` comment in the TS reference).
//! 6. `runtime.create` with the agent's launch command/env, reusing the
//!    previous handle string as the tmux name so users can re-attach by
//!    the same identifier.
//! 7. Persist: `status = spawning`, `activity = None`, new `runtime_handle`.
//!
//! Slice 1 intentionally does **not** re-deliver the initial prompt — it's
//! left to the caller (CLI will do that once Slice 2 introduces `ao-rs send`).

use crate::{
    error::{AoError, Result},
    session_manager::SessionManager,
    traits::{Agent, Runtime},
    types::{Session, SessionStatus},
};
use std::path::Path;

/// Outcome of a successful restore, returned so the caller can pretty-print.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub session: Session,
    /// Launch command actually handed to the runtime. Useful for CLI output.
    pub launch_command: String,
    /// New runtime handle (usually the same tmux name as before).
    pub runtime_handle: String,
}

/// Restore a session by full uuid or any unambiguous prefix.
///
/// Takes the plugin deps as `&dyn` references so the same code runs under
/// tests (with mocks) and in the real CLI (with tmux + claude-code). No
/// generic parameters — keeps callers clean.
pub async fn restore_session(
    id_or_prefix: &str,
    sessions: &SessionManager,
    runtime: &dyn Runtime,
    agent: &dyn Agent,
) -> Result<RestoreOutcome> {
    // ---- 1. Locate the session on disk ----
    let mut session = sessions.find_by_prefix(id_or_prefix).await?;

    // ---- 2. Enrich status with live runtime liveness ----
    //
    // A session that crashed mid-`working` never gets a chance to transition
    // through the lifecycle loop — so without this probe, `is_restorable`
    // would see `working` (non-terminal) and refuse.
    if let Some(handle) = session.runtime_handle.as_deref() {
        let alive = runtime.is_alive(handle).await.unwrap_or(false);
        if !alive && !session.status.is_terminal() {
            session.status = SessionStatus::Terminated;
        }
    } else if !session.status.is_terminal() {
        // No handle at all and still says running → definitely dead.
        session.status = SessionStatus::Terminated;
    }

    // ---- 3. Restorability gate ----
    if !session.is_restorable() {
        return Err(AoError::Runtime(format!(
            "session {} is not restorable (status={})",
            session.id,
            session.status.as_str()
        )));
    }

    // ---- 4. Workspace must still exist ----
    let workspace_path = session
        .workspace_path
        .clone()
        .ok_or_else(|| AoError::Workspace("session has no workspace_path".into()))?;
    if !workspace_path.exists() {
        return Err(AoError::Workspace(format!(
            "workspace missing: {}",
            workspace_path.display()
        )));
    }

    // ---- 5. Best-effort cleanup of the stale runtime ----
    if let Some(handle) = session.runtime_handle.as_deref() {
        // We don't care if this errors — the whole point is that the old
        // runtime is expected to be gone already.
        let _ = runtime.destroy(handle).await;
    }

    // ---- 6. Re-spawn via the runtime ----
    //
    // Reuse the previous handle as the new tmux session name so users who
    // muscle-memory'd `tmux attach -t <short-id>` still land on the right
    // pane. Falls back to the first 8 chars of the uuid (same rule as
    // `spawn`) if there was no prior handle for some reason.
    let new_name = session
        .runtime_handle
        .clone()
        .unwrap_or_else(|| session.id.0.chars().take(8).collect());

    let launch_command = agent.launch_command(&session);
    let env = agent.environment(&session);

    let new_handle = runtime
        .create(&new_name, &workspace_path, &launch_command, &env)
        .await?;

    // ---- 7. Persist ----
    session.runtime_handle = Some(new_handle.clone());
    session.status = SessionStatus::Spawning;
    session.activity = None;
    sessions.save(&session).await?;

    Ok(RestoreOutcome {
        session,
        launch_command,
        runtime_handle: new_handle,
    })
}

/// Does anything exist at `p`? Thin wrapper so tests can stub this out.
/// Currently unused — kept for when Slice 2 replaces `Path::exists` with
/// a plugin-provided `Workspace::exists`.
#[allow(dead_code)]
fn path_exists(p: &Path) -> bool {
    p.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{now_ms, ActivityState, SessionId, WorkspaceCreateConfig};
    use crate::Workspace;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ao-rs-restore-{label}-{nanos}-{n}"))
    }

    /// Records every runtime call so tests can assert on ordering.
    #[derive(Default)]
    struct RecorderRuntime {
        alive: AtomicBool,
        calls: Mutex<Vec<String>>,
    }

    impl RecorderRuntime {
        fn new(alive: bool) -> Self {
            Self {
                alive: AtomicBool::new(alive),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Runtime for RecorderRuntime {
        async fn create(
            &self,
            session_id: &str,
            _cwd: &Path,
            _launch_command: &str,
            _env: &[(String, String)],
        ) -> Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create:{session_id}"));
            // Echo the name back like the real tmux runtime does.
            Ok(session_id.to_string())
        }
        async fn send_message(&self, handle: &str, _msg: &str) -> Result<()> {
            self.calls.lock().unwrap().push(format!("send:{handle}"));
            Ok(())
        }
        async fn is_alive(&self, _handle: &str) -> Result<bool> {
            Ok(self.alive.load(Ordering::SeqCst))
        }
        async fn destroy(&self, handle: &str) -> Result<()> {
            self.calls.lock().unwrap().push(format!("destroy:{handle}"));
            Ok(())
        }
    }

    struct StubAgent;
    #[async_trait]
    impl Agent for StubAgent {
        fn launch_command(&self, _s: &Session) -> String {
            "mock-launch".into()
        }
        fn environment(&self, _s: &Session) -> Vec<(String, String)> {
            vec![]
        }
        fn initial_prompt(&self, _s: &Session) -> String {
            "".into()
        }
        async fn detect_activity(&self, _s: &Session) -> Result<ActivityState> {
            Ok(ActivityState::Ready)
        }
    }

    #[allow(dead_code)]
    struct StubWorkspace;
    #[async_trait]
    impl Workspace for StubWorkspace {
        async fn create(&self, _cfg: &WorkspaceCreateConfig) -> Result<PathBuf> {
            Ok(PathBuf::from("/tmp/ws"))
        }
        async fn destroy(&self, _workspace_path: &Path) -> Result<()> {
            Ok(())
        }
    }

    /// Build a persisted session whose workspace is a real directory we
    /// can `cd` into — restore() insists the worktree still exists.
    async fn persist_session(
        manager: &SessionManager,
        id: &str,
        status: SessionStatus,
        workspace: &Path,
    ) -> Session {
        let session = Session {
            id: SessionId(id.into()),
            project_id: "demo".into(),
            status,
            branch: format!("ao-{id}"),
            task: "restored task".into(),
            workspace_path: Some(workspace.to_path_buf()),
            runtime_handle: Some("old-handle".into()),
            activity: None,
            created_at: now_ms(),
            cost: None,
        };
        manager.save(&session).await.unwrap();
        session
    }

    #[tokio::test]
    async fn restore_terminal_session_respawns_runtime_and_persists_spawning() {
        let base = unique_temp_dir("ok");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let manager = SessionManager::new(base.clone());
        persist_session(&manager, "sess-ok", SessionStatus::Terminated, &ws).await;

        let rt = RecorderRuntime::new(false);
        let agent = StubAgent;

        let out = restore_session("sess-ok", &manager, &rt, &agent)
            .await
            .unwrap();

        // Destroy (best-effort cleanup) must precede create in the call log.
        let calls = rt.calls();
        let destroy_idx = calls.iter().position(|c| c == "destroy:old-handle");
        let create_idx = calls.iter().position(|c| c == "create:old-handle");
        assert!(destroy_idx.is_some(), "destroy not called: {calls:?}");
        assert!(create_idx.is_some(), "create not called: {calls:?}");
        assert!(destroy_idx < create_idx, "destroy must come before create");

        assert_eq!(out.session.status, SessionStatus::Spawning);
        assert_eq!(out.session.activity, None);
        assert_eq!(out.runtime_handle, "old-handle");
        assert_eq!(out.launch_command, "mock-launch");

        // And the persisted state matches.
        let reread = manager.list().await.unwrap();
        assert_eq!(reread.len(), 1);
        assert_eq!(reread[0].status, SessionStatus::Spawning);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn crashed_working_session_is_enriched_to_terminated_then_restored() {
        // Session on disk says `working` but the runtime probe reports dead
        // — exactly the TS `enrichSessionWithRuntimeState` case.
        let base = unique_temp_dir("enrich");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let manager = SessionManager::new(base.clone());
        persist_session(&manager, "sess-crash", SessionStatus::Working, &ws).await;

        let rt = RecorderRuntime::new(false); // dead
        let out = restore_session("sess-crash", &manager, &rt, &StubAgent)
            .await
            .unwrap();

        assert_eq!(out.session.status, SessionStatus::Spawning);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merged_session_is_not_restorable() {
        let base = unique_temp_dir("merged");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let manager = SessionManager::new(base.clone());
        persist_session(&manager, "sess-merged", SessionStatus::Merged, &ws).await;

        let rt = RecorderRuntime::new(false);
        let err = restore_session("sess-merged", &manager, &rt, &StubAgent)
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("not restorable"),
            "unexpected: {err}"
        );

        // Persisted state must be untouched.
        let reread = manager.list().await.unwrap();
        assert_eq!(reread[0].status, SessionStatus::Merged);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn missing_workspace_errors_before_touching_runtime() {
        let base = unique_temp_dir("nows");
        let manager = SessionManager::new(base.clone());
        persist_session(
            &manager,
            "sess-ghost",
            SessionStatus::Terminated,
            &PathBuf::from("/nonexistent/ao-rs/does-not-exist"),
        )
        .await;

        let rt = RecorderRuntime::new(false);
        let err = restore_session("sess-ghost", &manager, &rt, &StubAgent)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("workspace missing"), "got: {err}");
        // Runtime must not have been touched.
        assert!(
            rt.calls().is_empty(),
            "runtime was called: {:?}",
            rt.calls()
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn unknown_session_id_errors() {
        let base = unique_temp_dir("missing");
        let manager = SessionManager::new(base.clone());
        let rt = RecorderRuntime::new(false);
        let err = restore_session("nope", &manager, &rt, &StubAgent)
            .await
            .unwrap_err();
        assert!(matches!(err, AoError::SessionNotFound(_)), "got {err:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn ambiguous_prefix_errors() {
        let base = unique_temp_dir("ambig");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let manager = SessionManager::new(base.clone());
        persist_session(&manager, "abcd-1111", SessionStatus::Terminated, &ws).await;
        persist_session(&manager, "abcd-2222", SessionStatus::Terminated, &ws).await;

        let rt = RecorderRuntime::new(false);
        let err = restore_session("abcd", &manager, &rt, &StubAgent)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("ambiguous"), "got: {err}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn prefix_match_resolves_to_unique_session() {
        let base = unique_temp_dir("prefix");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let manager = SessionManager::new(base.clone());
        persist_session(
            &manager,
            "deadbeef-uuid-long",
            SessionStatus::Terminated,
            &ws,
        )
        .await;

        let rt = RecorderRuntime::new(false);
        let out = restore_session("deadbeef", &manager, &rt, &StubAgent)
            .await
            .unwrap();
        assert_eq!(out.session.id.0, "deadbeef-uuid-long");
        let _ = std::fs::remove_dir_all(&base);
    }
}
