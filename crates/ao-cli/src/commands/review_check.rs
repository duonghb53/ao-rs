//! `ao-rs review-check` — forward new PR comments to agents.

use ao_core::{paths, Scm, Session, SessionManager};

use crate::cli::auto_scm::AutoScm;
use crate::cli::plugins::select_runtime;
use crate::cli::printing::short_id;

pub async fn review_check(
    project_filter: Option<String>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let manager = SessionManager::with_default();
    let all = manager.list().await?;

    // Filter to non-terminal sessions, optionally by project.
    let candidates: Vec<&Session> = all
        .iter()
        .filter(|s| !s.is_terminal())
        .filter(|s| project_filter.as_ref().is_none_or(|p| s.project_id == *p))
        .collect();

    if candidates.is_empty() {
        println!("no active sessions to check");
        return Ok(());
    }

    use std::fmt::Write as _;

    let scm = AutoScm::new();
    let fingerprint_dir = paths::data_dir().join("review-fingerprints");

    // Create fingerprint directory once, outside the loop.
    if !dry_run {
        tokio::fs::create_dir_all(&fingerprint_dir).await?;
    }

    let mut checked = 0u32;
    let mut no_pr = 0u32;
    let mut sent = 0u32;
    let mut skipped = 0u32;
    let mut errors = 0u32;

    for session in &candidates {
        let short = short_id(&session.id);
        let runtime = select_runtime(&session.runtime);

        // Detect PR — skip sessions that haven't opened one yet.
        let pr = match scm.detect_pr(session).await {
            Ok(Some(pr)) => pr,
            Ok(None) => {
                no_pr += 1;
                continue;
            }
            Err(e) => {
                eprintln!("  {short}  error detecting PR: {e}");
                errors += 1;
                continue;
            }
        };
        checked += 1;

        // Fetch pending (unresolved) comments.
        let comments = match scm.pending_comments(&pr).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  {short}  error fetching comments: {e}");
                errors += 1;
                continue;
            }
        };

        if comments.is_empty() {
            continue;
        }

        // Compute fingerprint from sorted comment IDs.
        let mut ids: Vec<&str> = comments.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        let fingerprint = ids.join(",");

        // Check if fingerprint changed since last run.
        let fp_path = fingerprint_dir.join(format!("{}.txt", session.id.0));
        let old_fp = tokio::fs::read_to_string(&fp_path)
            .await
            .unwrap_or_default();
        if old_fp.trim() == fingerprint {
            // Already sent for this set of comments.
            skipped += 1;
            continue;
        }

        // Format the review message using write! to avoid per-comment allocations.
        let mut msg = format!(
            "There are {} new review comment(s) on PR #{} that need your attention:\n\n",
            comments.len(),
            pr.number
        );
        for c in &comments {
            let _ = write!(msg, "- @{}", c.author);
            if let Some(ref path) = c.path {
                let _ = write!(msg, " on `{path}`");
                if let Some(line) = c.line {
                    let _ = write!(msg, ":{line}");
                }
            }
            let _ = writeln!(msg, ": {}", c.body.lines().next().unwrap_or(""));
        }
        msg.push_str(
            "\nAddress each comment, push your changes, and mark conversations as resolved.",
        );

        if dry_run {
            println!(
                "  {short}  PR #{} — {} comment(s) (dry-run, not sending)",
                pr.number,
                comments.len()
            );
            println!("    would send: {}", msg.lines().next().unwrap_or(""));
        } else {
            // Send to agent via runtime.
            if let Some(ref handle) = session.runtime_handle {
                match runtime.send_message(handle, &msg).await {
                    Ok(()) => {
                        println!(
                            "  {short}  PR #{} — sent {} comment(s) to agent",
                            pr.number,
                            comments.len()
                        );
                        sent += 1;
                        // Persist fingerprint — failure is per-session, not fatal.
                        if let Err(e) = tokio::fs::write(&fp_path, &fingerprint).await {
                            eprintln!("  {short}  warning: failed to persist fingerprint: {e}");
                        }
                    }
                    Err(e) => {
                        eprintln!("  {short}  error sending message: {e}");
                        errors += 1;
                    }
                }
            } else {
                eprintln!("  {short}  no runtime handle — skipping");
                skipped += 1;
            }
        }
    }

    println!();
    let mut summary = format!(
        "review-check: {checked} PR(s) checked, {sent} sent, {skipped} skipped, {errors} error(s)"
    );
    if no_pr > 0 {
        use std::fmt::Write as _;
        let _ = write!(summary, ", {no_pr} without PR");
    }
    println!("{summary}");

    Ok(())
}
