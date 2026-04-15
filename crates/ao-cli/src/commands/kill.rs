//! `ao-rs kill` — stop runtime, remove worktree, archive session.

use ao_core::{SessionManager, SessionStatus, Workspace};

use ao_plugin_workspace_worktree::WorktreeWorkspace;

use crate::cli::plugins::select_runtime;
use crate::cli::printing::short_id;

pub async fn kill(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
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
