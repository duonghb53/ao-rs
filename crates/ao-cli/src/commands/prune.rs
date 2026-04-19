//! `ao-rs prune` — remove the branch worktrees created at spawn time.
//!
//! Each session has its own git worktree checkout (e.g. `~/.ao-rs/worktrees/<id>/`).
//! This command removes those per-session checkouts via `git worktree remove`,
//! freeing disk space without touching the main repository or archiving session YAMLs.
//! Sessions remain fully visible in the dashboard after pruning.

use ao_core::{SessionManager, Workspace};
use ao_plugin_workspace_worktree::WorktreeWorkspace;

pub async fn prune(
    project_filter: Option<String>,
    all_sessions: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();

    let candidates = match &project_filter {
        Some(p) => sessions.list_for_project(p).await?,
        None => sessions.list().await?,
    };

    let to_prune: Vec<_> = candidates
        .into_iter()
        .filter(|s| all_sessions || s.is_terminal())
        .filter(|s| s.workspace_path.as_ref().is_some_and(|p| p.exists()))
        .collect();

    if to_prune.is_empty() {
        println!("no worktrees to remove (use --all to include active sessions)");
        return Ok(());
    }

    let workspace = WorktreeWorkspace::new();
    let mut removed = 0u32;
    let mut failed = 0u32;

    for session in &to_prune {
        let short = &session.id.0[..8.min(session.id.0.len())];
        let ws = session.workspace_path.as_ref().unwrap();

        if dry_run {
            println!(
                "  would remove: {short} ({})  {}",
                session.project_id,
                ws.display()
            );
            removed += 1;
            continue;
        }

        match workspace.destroy(ws).await {
            Ok(()) => {
                println!(
                    "  → removed: {short} ({})  {}",
                    session.project_id,
                    ws.display()
                );
                removed += 1;
            }
            Err(e) => {
                eprintln!("  error: {short} — {e}");
                failed += 1;
            }
        }
    }

    println!();
    if dry_run {
        println!("dry run: {removed} worktree(s) would be removed");
    } else {
        println!("removed: {removed} worktree(s), failed: {failed}");
        println!("(session records kept — sessions still visible in dashboard)");
    }
    Ok(())
}
