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
//! 4. Verify the workspace is still usable via `Workspace::exists()`. The
//!    plugin's own check catches corrupted/partially-removed workspaces
//!    (e.g. directory present but `.git` gone) that a plain
//!    `Path::exists()` would miss. Slice 1 does not auto-recreate
//!    workspaces — if it's gone, the user has to `ao-rs spawn` a fresh
//!    session. (TS has an optional `workspace.restore` plugin hook for
//!    this; Phase D keeps the trait surface small.)
//! 5. Best-effort `runtime.destroy` on the old handle in case it's still
//!    lingering (tmux can survive the agent process exiting — see the
//!    `// step 6` comment in the TS reference).
//! 6. `runtime.create` with the agent's launch command/env, reusing the
//!    previous handle string as the tmux name so users can re-attach by
//!    the same identifier.
//! 7. Persist: `status = spawning`, `activity = None`, new `runtime_handle`.
//!
//! 8. Re-deliver the session's initial prompt (spawn parity): after the runtime
//!    is recreated, send the same initial prompt the agent would receive at
//!    spawn-time so it can resume context without manual `ao-rs send`.

use crate::{
    error::{AoError, Result},
    session_manager::SessionManager,
    traits::{Agent, Runtime, Workspace},
    types::{Session, SessionStatus},
};

/// Outcome of a successful restore, returned so the caller can pretty-print.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub session: Session,
    /// Launch command actually handed to the runtime. Useful for CLI output.
    pub launch_command: String,
    /// New runtime handle (usually the same tmux name as before).
    pub runtime_handle: String,
    /// Whether we successfully re-delivered the initial prompt to the agent.
    ///
    /// Restore still succeeds if this fails (best-effort), but callers may
    /// want to surface a warning suggesting a manual resend.
    pub prompt_sent: bool,
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
    workspace: &dyn Workspace,
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

    // ---- 4. Workspace must still be usable ----
    //
    // Delegate the check to the plugin so it can apply backend-specific
    // validation (e.g. git-backed workspaces verify the working tree is
    // still recognised by git, not just present on disk).
    let workspace_path = session
        .workspace_path
        .clone()
        .ok_or_else(|| AoError::Workspace("session has no workspace_path".into()))?;
    if !workspace.exists(&workspace_path).await? {
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

    // ---- 8. Re-deliver the initial prompt (best-effort) ----
    let prompt = agent.initial_prompt(&session);
    let prompt_sent = if prompt.trim().is_empty() {
        false
    } else {
        runtime.send_message(&new_handle, &prompt).await.is_ok()
    };

    Ok(RestoreOutcome {
        session,
        launch_command,
        runtime_handle: new_handle,
        prompt_sent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{now_ms, ActivityState, SessionId, WorkspaceCreateConfig};
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};
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
        messages: Mutex<Vec<String>>,
    }

    impl RecorderRuntime {
        fn new(alive: bool) -> Self {
            Self {
                alive: AtomicBool::new(alive),
                calls: Mutex::new(Vec::new()),
                messages: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn messages(&self) -> Vec<String> {
            self.messages.lock().unwrap().clone()
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
            self.messages.lock().unwrap().push(_msg.to_string());
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
            "hello from restore".into()
        }
        async fn detect_activity(&self, _s: &Session) -> Result<ActivityState> {
            Ok(ActivityState::Ready)
        }
    }

    /// Thin workspace stub that relies on the trait's default `exists()`
    /// (i.e. a plain `Path::exists` probe). Restore tests use it to drive
    /// the "workspace is on disk → proceed" branch.
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

    /// Workspace stub whose `exists()` returns a configurable value without
    /// touching the filesystem. Lets the "restore fails cleanly when the
    /// workspace plugin reports corrupted" test run even if the directory
    /// itself is present on disk.
    struct ExistsWorkspace {
        reports_exists: bool,
    }
    #[async_trait]
    impl Workspace for ExistsWorkspace {
        async fn create(&self, _cfg: &WorkspaceCreateConfig) -> Result<PathBuf> {
            Ok(PathBuf::from("/tmp/ws"))
        }
        async fn destroy(&self, _workspace_path: &Path) -> Result<()> {
            Ok(())
        }
        async fn exists(&self, _workspace_path: &Path) -> Result<bool> {
            Ok(self.reports_exists)
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
            agent: "claude-code".into(),
            agent_config: None,
            branch: format!("ao-{id}"),
            task: "restored task".into(),
            workspace_path: Some(workspace.to_path_buf()),
            runtime_handle: Some("old-handle".into()),
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

        let out = restore_session("sess-ok", &manager, &rt, &agent, &StubWorkspace)
            .await
            .unwrap();

        // Destroy (best-effort cleanup) must precede create in the call log.
        let calls = rt.calls();
        let destroy_idx = calls.iter().position(|c| c == "destroy:old-handle");
        let create_idx = calls.iter().position(|c| c == "create:old-handle");
        let send_idx = calls.iter().position(|c| c == "send:old-handle");
        assert!(destroy_idx.is_some(), "destroy not called: {calls:?}");
        assert!(create_idx.is_some(), "create not called: {calls:?}");
        assert!(destroy_idx < create_idx, "destroy must come before create");
        assert!(send_idx.is_some(), "send not called: {calls:?}");
        assert!(create_idx < send_idx, "create must come before send");

        assert_eq!(out.session.status, SessionStatus::Spawning);
        assert_eq!(out.session.activity, None);
        assert_eq!(out.runtime_handle, "old-handle");
        assert_eq!(out.launch_command, "mock-launch");
        assert!(out.prompt_sent, "expected prompt_sent=true");

        let msgs = rt.messages();
        assert_eq!(msgs.len(), 1, "expected exactly one message: {msgs:?}");
        assert!(
            !msgs[0].trim().is_empty(),
            "expected non-empty prompt, got: {:?}",
            msgs[0]
        );

        // And the persisted state matches.
        let reread = manager.list().await.unwrap();
        assert_eq!(reread.len(), 1);
        assert_eq!(reread[0].status, SessionStatus::Spawning);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn restore_missing_runtime_handle_creates_new_handle_without_destroy() {
        let base = unique_temp_dir("no-handle");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let manager = SessionManager::new(base.clone());
        // Persist a terminal session that somehow lost its runtime_handle.
        let mut s =
            persist_session(&manager, "sess-nohandle", SessionStatus::Terminated, &ws).await;
        s.runtime_handle = None;
        manager.save(&s).await.unwrap();

        let rt = RecorderRuntime::new(false);
        let out = restore_session("sess-nohandle", &manager, &rt, &StubAgent, &StubWorkspace)
            .await
            .unwrap();

        // No prior handle → no destroy call.
        let calls = rt.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("destroy:")),
            "unexpected destroy call(s): {calls:?}"
        );
        // The name used should fall back to first 8 chars of the uuid.
        assert!(
            calls.iter().any(|c| c == "create:sess-noh"),
            "expected create with short id (sess-noh), got calls: {calls:?}"
        );
        assert_eq!(out.runtime_handle, "sess-noh");
        assert_eq!(out.session.status, SessionStatus::Spawning);
        assert!(out.prompt_sent, "expected prompt_sent=true");

        let reread = manager.find_by_prefix("sess-nohandle").await.unwrap();
        assert_eq!(reread.runtime_handle.as_deref(), Some("sess-noh"));

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
        let out = restore_session("sess-crash", &manager, &rt, &StubAgent, &StubWorkspace)
            .await
            .unwrap();

        assert_eq!(out.session.status, SessionStatus::Spawning);
        assert!(out.prompt_sent, "expected prompt_sent=true");

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
        let err = restore_session("sess-merged", &manager, &rt, &StubAgent, &StubWorkspace)
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
        let err = restore_session("sess-ghost", &manager, &rt, &StubAgent, &StubWorkspace)
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
    async fn corrupted_workspace_reports_missing_via_plugin_exists() {
        // Directory is on disk but the plugin reports it as not usable
        // (e.g. worktree's `git rev-parse` check failed). Restore must
        // surface "workspace missing" and never touch the runtime.
        let base = unique_temp_dir("corrupt");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let manager = SessionManager::new(base.clone());
        persist_session(&manager, "sess-corrupt", SessionStatus::Terminated, &ws).await;

        let rt = RecorderRuntime::new(false);
        let workspace = ExistsWorkspace {
            reports_exists: false,
        };
        let err = restore_session("sess-corrupt", &manager, &rt, &StubAgent, &workspace)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("workspace missing"), "got: {err}");
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
        let err = restore_session("nope", &manager, &rt, &StubAgent, &StubWorkspace)
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
        let err = restore_session("abcd", &manager, &rt, &StubAgent, &StubWorkspace)
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
        let out = restore_session("deadbeef", &manager, &rt, &StubAgent, &StubWorkspace)
            .await
            .unwrap();
        assert_eq!(out.session.id.0, "deadbeef-uuid-long");
        assert!(out.prompt_sent, "expected prompt_sent=true");
        let _ = std::fs::remove_dir_all(&base);
    }
}
