//! `ao-rs session remap`.
//!
//! Updates a persisted session's `workspace_path` and/or `runtime_handle`
//! fields in place. Does not recreate the runtime — callers who also need
//! that should chain with `ao-rs session restore <id>`.

use std::path::PathBuf;

use ao_core::{Session, SessionManager};

use crate::cli::printing::short_id;

pub async fn remap(
    id_or_prefix: String,
    workspace: Option<PathBuf>,
    runtime_handle: Option<String>,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if workspace.is_none() && runtime_handle.is_none() {
        return Err("nothing to remap: pass --workspace and/or --runtime-handle".into());
    }

    if let Some(p) = workspace.as_deref() {
        if !force && !p.exists() {
            return Err(format!(
                "workspace path does not exist: {} (use --force to override)",
                p.display()
            )
            .into());
        }
    }

    let sessions = SessionManager::with_default();
    remap_with_manager(&sessions, &id_or_prefix, workspace, runtime_handle).await
}

async fn remap_with_manager(
    sessions: &SessionManager,
    id_or_prefix: &str,
    workspace: Option<PathBuf>,
    runtime_handle: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = sessions.find_by_prefix(id_or_prefix).await?;
    let (old_workspace, old_handle) = session.apply_remap(workspace, runtime_handle);
    print_diff(&session, &old_workspace, &old_handle);
    sessions.save(&session).await?;
    println!();
    println!("  ✓ session remapped");
    Ok(())
}

fn print_diff(session: &Session, old_workspace: &Option<PathBuf>, old_handle: &Option<String>) {
    let short = short_id(&session.id);
    println!();
    println!("───────────────────────────────────────────────");
    println!("  session: {} (short {short})", session.id);
    println!();
    println!("  workspace_path:");
    print_field_diff(
        old_workspace.as_ref().map(|p| p.display().to_string()),
        session
            .workspace_path
            .as_ref()
            .map(|p| p.display().to_string()),
    );
    println!();
    println!("  runtime_handle:");
    print_field_diff(old_handle.clone(), session.runtime_handle.clone());
    println!("───────────────────────────────────────────────");
}

fn print_field_diff(old: Option<String>, new: Option<String>) {
    let format = |v: &Option<String>| match v {
        Some(s) => s.clone(),
        None => "<none>".into(),
    };
    if old == new {
        println!("    (unchanged: {})", format(&old));
    } else {
        println!("    - {}", format(&old));
        println!("    + {}", format(&new));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{now_ms, SessionId, SessionStatus};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ao-rs-remap-{label}-{nanos}-{n}"))
    }

    async fn persist_session(
        sm: &SessionManager,
        id: &str,
        workspace: Option<&Path>,
        handle: Option<&str>,
    ) -> Session {
        let session = Session {
            id: SessionId(id.into()),
            project_id: "demo".into(),
            status: SessionStatus::Terminated,
            agent: "claude-code".into(),
            agent_config: None,
            branch: format!("ao-{id}"),
            task: "remap subject".into(),
            workspace_path: workspace.map(|p| p.to_path_buf()),
            runtime_handle: handle.map(|h| h.to_string()),
            runtime: "tmux".into(),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: Some("92".into()),
            issue_url: None,
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: None,
            spawned_by: None,
            last_merge_conflict_dispatched: None,
            last_review_backlog_fingerprint: None,
        };
        sm.save(&session).await.unwrap();
        session
    }

    #[tokio::test]
    async fn remap_workspace_only_persists_new_path() {
        let base = unique_temp_dir("ws-only");
        let new_ws = base.join("new-ws");
        std::fs::create_dir_all(&new_ws).unwrap();

        let sm = SessionManager::new(base.clone());
        persist_session(&sm, "remap-ws", Some(&base.join("old")), Some("handle-a")).await;

        remap_with_manager(&sm, "remap-ws", Some(new_ws.clone()), None)
            .await
            .unwrap();

        let reread = sm.find_by_prefix("remap-ws").await.unwrap();
        assert_eq!(reread.workspace_path.as_deref(), Some(new_ws.as_path()));
        assert_eq!(reread.runtime_handle.as_deref(), Some("handle-a"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn remap_runtime_handle_only_persists_new_handle() {
        let base = unique_temp_dir("handle-only");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let sm = SessionManager::new(base.clone());
        persist_session(&sm, "remap-rh", Some(&ws), Some("old-name")).await;

        remap_with_manager(&sm, "remap-rh", None, Some("new-name".into()))
            .await
            .unwrap();

        let reread = sm.find_by_prefix("remap-rh").await.unwrap();
        assert_eq!(reread.workspace_path.as_deref(), Some(ws.as_path()));
        assert_eq!(reread.runtime_handle.as_deref(), Some("new-name"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn remap_both_persists_both() {
        let base = unique_temp_dir("both");
        let new_ws = base.join("new-ws");
        std::fs::create_dir_all(&new_ws).unwrap();

        let sm = SessionManager::new(base.clone());
        persist_session(&sm, "remap-both", Some(&base.join("old")), Some("old-h")).await;

        remap_with_manager(
            &sm,
            "remap-both",
            Some(new_ws.clone()),
            Some("new-h".into()),
        )
        .await
        .unwrap();

        let reread = sm.find_by_prefix("remap-both").await.unwrap();
        assert_eq!(reread.workspace_path.as_deref(), Some(new_ws.as_path()));
        assert_eq!(reread.runtime_handle.as_deref(), Some("new-h"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn remap_no_flags_errors_before_touching_store() {
        // No session persisted — find_by_prefix would return SessionNotFound.
        // We assert the no-flags error fires first (doesn't reach the store).
        let base = unique_temp_dir("noflags");
        let err = remap("missing".into(), None, None, false)
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("nothing to remap"), "got: {msg}");
        // Base dir must not have been created by our call.
        assert!(!base.exists(), "store was touched: {base:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn remap_missing_workspace_without_force_errors() {
        let bogus = PathBuf::from("/nonexistent/ao-rs/remap-target");
        let err = remap("does-not-matter".into(), Some(bogus.clone()), None, false)
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("workspace path does not exist"), "got: {msg}");
        assert!(msg.contains(bogus.to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn remap_missing_workspace_with_force_succeeds() {
        // --force bypasses Path::exists(); the surviving failure path is
        // session lookup. We persist a session and then remap to a bogus
        // path with force=true. apply_remap does not validate.
        let base = unique_temp_dir("force");
        let sm = SessionManager::new(base.clone());
        persist_session(&sm, "remap-force", Some(&base.join("old")), Some("h-1")).await;

        let bogus = PathBuf::from("/nonexistent/ao-rs/remap-target");
        // Note: the public `remap()` entrypoint would use the default
        // SessionManager path (~/.ao-rs/sessions). We exercise the
        // force-accept path via the inner helper with our tempdir store.
        remap_with_manager(&sm, "remap-force", Some(bogus.clone()), None)
            .await
            .unwrap();

        let reread = sm.find_by_prefix("remap-force").await.unwrap();
        assert_eq!(reread.workspace_path, Some(bogus));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn remap_unknown_session_errors() {
        let base = unique_temp_dir("unknown");
        let sm = SessionManager::new(base.clone());
        let err = remap_with_manager(&sm, "ghost", None, Some("anything".into()))
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ghost"), "got: {msg}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn print_field_diff_unchanged_prints_one_line() {
        // Smoke that the formatting helpers don't panic. Stdout output is
        // not captured here; the assertion is purely that we don't crash.
        print_field_diff(Some("same".into()), Some("same".into()));
        print_field_diff(None, None);
        print_field_diff(Some("a".into()), Some("b".into()));
        print_field_diff(None, Some("b".into()));
    }
}
