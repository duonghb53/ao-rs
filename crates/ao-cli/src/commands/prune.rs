//! `ao-rs prune` — free disk space from worktrees without archiving sessions.
//!
//! Unlike `ao-rs cleanup`, this command does **not** archive session YAML files.
//! Sessions remain fully visible in the dashboard.
//!
//! Default: removes only `target/` (1–5 GB savings for Rust workspaces).
//! `--worktree`: removes the entire worktree directory (source + target).

use ao_core::SessionManager;
use ao_plugin_workspace_worktree::WorktreeWorkspace;
use tokio::fs;

/// Return the disk usage of `path` in bytes using `du -sk`.
///
/// Returns `None` on any error (non-existent path, `du` not available, etc.).
async fn disk_usage_bytes(path: &std::path::Path) -> Option<u64> {
    let out = tokio::process::Command::new("du")
        .args(["-sk", &path.display().to_string()])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `du -sk` output: "<kibibytes>\t<path>"
    let stdout = String::from_utf8_lossy(&out.stdout);
    let kb: u64 = stdout.split_whitespace().next()?.parse().ok()?;
    Some(kb * 1024)
}

fn fmt_bytes(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", b / 1024)
    }
}

pub async fn prune(
    project_filter: Option<String>,
    all_sessions: bool,
    remove_worktree: bool,
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
        .collect();

    if to_prune.is_empty() {
        println!("no sessions match (use --all to include active sessions)");
        return Ok(());
    }

    let workspace = WorktreeWorkspace::new();
    let mut pruned = 0u32;
    let mut freed_bytes: u64 = 0;
    let mut skipped = 0u32;

    for session in &to_prune {
        let short = &session.id.0[..8.min(session.id.0.len())];
        let Some(ref ws) = session.workspace_path else {
            skipped += 1;
            continue;
        };

        if remove_worktree {
            // Remove entire worktree directory.
            if !ws.exists() {
                skipped += 1;
                continue;
            }
            let size = disk_usage_bytes(ws).await;
            let size_label = size.map(fmt_bytes).unwrap_or_else(|| "?".to_string());

            if dry_run {
                println!("  would remove worktree: {short} ({}) — {size_label}", session.project_id);
                pruned += 1;
                if let Some(b) = size { freed_bytes += b; }
                continue;
            }

            use ao_core::Workspace;
            match workspace.destroy(ws).await {
                Ok(()) => {
                    println!("  → removed worktree: {short} ({}) — freed {size_label}", session.project_id);
                    pruned += 1;
                    if let Some(b) = size { freed_bytes += b; }
                }
                Err(e) => eprintln!("  error removing worktree {}: {e}", ws.display()),
            }
        } else {
            // Remove only target/ (build cache).
            let target = ws.join("target");
            if !target.exists() {
                skipped += 1;
                continue;
            }
            let size = disk_usage_bytes(&target).await;
            let size_label = size.map(fmt_bytes).unwrap_or_else(|| "?".to_string());

            if dry_run {
                println!("  would prune: {short} ({}) — target/ {size_label}", session.project_id);
                pruned += 1;
                if let Some(b) = size { freed_bytes += b; }
                continue;
            }

            match fs::remove_dir_all(&target).await {
                Ok(()) => {
                    println!("  → pruned: {short} ({}) — freed {size_label}", session.project_id);
                    pruned += 1;
                    if let Some(b) = size { freed_bytes += b; }
                }
                Err(e) => eprintln!("  error removing {}: {e}", target.display()),
            }
        }
    }

    println!();
    let kept_note = if remove_worktree {
        "(session records kept — sessions still visible in dashboard)"
    } else {
        "(session records and worktrees kept — sessions still visible in dashboard)"
    };

    if dry_run {
        println!("dry run: {} session(s) would free ~{}", pruned, fmt_bytes(freed_bytes));
    } else {
        println!("pruned: {pruned} session(s), skipped: {skipped}, freed: ~{}", fmt_bytes(freed_bytes));
        println!("{kept_note}");
    }
    Ok(())
}
