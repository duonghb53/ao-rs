//! `ao-rs kill` — stop runtime, remove worktree, archive session.

use ao_core::{SessionManager, SessionStatus, Workspace};

use ao_plugin_workspace_worktree::WorktreeWorkspace;

use crate::cli::plugins::select_runtime;
use crate::cli::printing::short_id;

pub async fn kill(
    session_id_or_prefix: String,
    purge_session: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    kill_with_manager(&sessions, session_id_or_prefix, purge_session).await
}

/// Like [`kill`] but uses the given session store (used by tests with a temp directory).
pub(crate) async fn kill_with_manager(
    sessions: &SessionManager,
    session_id_or_prefix: String,
    purge_session: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = match sessions.find_by_prefix(&session_id_or_prefix).await {
        Ok(s) => s,
        Err(ao_core::AoError::SessionNotFound(_)) => {
            // Check if already archived — give a clearer message than "not found".
            let all_projects = sessions.list().await.unwrap_or_default();
            // Collect unique project IDs to search archives.
            let project_ids: std::collections::HashSet<_> =
                all_projects.iter().map(|s| s.project_id.as_str()).collect();
            for pid in project_ids {
                let archived = sessions.list_archived(pid).await.unwrap_or_default();
                if archived
                    .iter()
                    .any(|s| s.id.0.starts_with(&session_id_or_prefix))
                {
                    return Err(format!(
                        "session {session_id_or_prefix} is already killed and archived"
                    )
                    .into());
                }
            }
            return Err(ao_core::AoError::SessionNotFound(session_id_or_prefix.clone()).into());
        }
        Err(e) => return Err(e.into()),
    };
    let short = short_id(&session.id);

    // 1. Kill runtime (best-effort — may already be gone).
    if let Some(ref handle) = session.runtime_handle {
        let runtime = select_runtime(&session.runtime);
        match runtime.destroy(handle).await {
            Ok(()) => println!("→ killed runtime {handle}"),
            Err(e) => eprintln!("  warning: runtime destroy failed (may already be gone): {e}"),
        }
    }

    // 2. Remove worktree (best-effort — destroy already handles missing dirs).
    if let Some(ref ws) = session.workspace_path {
        let workspace = WorktreeWorkspace::new();
        match workspace.destroy(ws).await {
            Ok(()) => println!("→ removed worktree {}", ws.display()),
            Err(e) => eprintln!("  warning: worktree cleanup failed: {e}"),
        }
    }

    if purge_session {
        eprintln!(
            "  warning: --purge-session removes the session record permanently (no archive)."
        );
        sessions.delete(&session.project_id, &session.id).await?;
        println!("→ session {short} killed; session record purged from disk");
        return Ok(());
    }

    // 3. Transition to Killed (unless already terminal).
    if !session.status.is_terminal() {
        session.status = SessionStatus::Killed;
        sessions.save(&session).await?;
    }

    // 4. Archive — moves YAML from active dir to .archive/.
    sessions.archive(&session).await?;

    println!("→ session {short} killed and archived");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::kill_with_manager;
    use ao_core::types::{now_ms, Session, SessionId, SessionStatus};
    use ao_core::SessionManager;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ao-rs-kill-{label}-{nanos}"))
    }

    fn minimal_session(id: &str, project: &str) -> Session {
        Session {
            id: SessionId(id.into()),
            project_id: project.into(),
            status: SessionStatus::Working,
            agent: "claude-code".into(),
            agent_config: None,
            branch: format!("ao-{id}"),
            task: "test".into(),
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

    #[tokio::test]
    async fn without_purge_archives_yaml_under_dot_archive() {
        let base = unique_temp_dir("archive");
        let manager = SessionManager::new(base.clone());
        let s = minimal_session("purge-test-aaaa-bbbb", "demo");
        manager.save(&s).await.unwrap();

        kill_with_manager(&manager, "purge-test".into(), false)
            .await
            .unwrap();

        assert!(manager.list().await.unwrap().is_empty());
        assert_eq!(manager.list_archived("demo").await.unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn purge_removes_session_yaml_no_archive() {
        let base = unique_temp_dir("purge");
        let manager = SessionManager::new(base.clone());
        let s = minimal_session("deadbeef-purge-bbbb", "demo");
        manager.save(&s).await.unwrap();

        kill_with_manager(&manager, "deadbeef".into(), true)
            .await
            .unwrap();

        assert!(manager.list().await.unwrap().is_empty());
        assert!(manager.list_archived("demo").await.unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }
}
