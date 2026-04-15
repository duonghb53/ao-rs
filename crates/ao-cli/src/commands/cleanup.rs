//! `ao-rs cleanup` — archive terminal sessions.

use ao_core::{SessionManager, Workspace};

use ao_plugin_workspace_worktree::WorktreeWorkspace;

use crate::cli::printing::short_id;

pub async fn cleanup(
    project_filter: Option<String>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let all = match &project_filter {
        Some(p) => sessions.list_for_project(p).await?,
        None => sessions.list().await?,
    };

    let terminal: Vec<_> = all.into_iter().filter(|s| s.is_terminal()).collect();

    if terminal.is_empty() {
        println!("no terminal sessions to clean up");
        return Ok(());
    }

    let mut cleaned = 0u32;
    let mut errors = 0u32;

    for session in &terminal {
        let short = short_id(&session.id);

        if dry_run {
            let ws_note = session
                .workspace_path
                .as_ref()
                .filter(|p| p.exists())
                .map(|p| format!(" (worktree: {})", p.display()))
                .unwrap_or_default();
            println!(
                "  would clean: {short} ({}, {}){ws_note}",
                session.project_id,
                session.status.as_str(),
            );
            cleaned += 1;
            continue;
        }

        // Remove worktree if still on disk.
        if let Some(ref ws) = session.workspace_path {
            if ws.exists() {
                let workspace = WorktreeWorkspace::new();
                match workspace.destroy(ws).await {
                    Ok(()) => println!("  → removed worktree: {}", ws.display()),
                    Err(e) => {
                        eprintln!("  warning: worktree cleanup for {short}: {e}");
                        errors += 1;
                    }
                }
            }
        }

        // Archive session YAML.
        match sessions.archive(session).await {
            Ok(()) => {
                println!("  → archived: {short}");
                cleaned += 1;
            }
            Err(e) => {
                eprintln!("  error archiving {short}: {e}");
                errors += 1;
            }
        }
    }

    println!();
    if dry_run {
        println!("dry run: {cleaned} session(s) would be cleaned");
    } else {
        println!("cleaned: {cleaned}, errors: {errors}");
    }
    Ok(())
}
