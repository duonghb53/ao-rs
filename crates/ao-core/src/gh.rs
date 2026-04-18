//! Centralized `gh` CLI subprocess runner with rate-limit integration.
//!
//! Consolidates the per-plugin copies previously in `scm-github/src/lib.rs`,
//! `scm-github/src/graphql_batch.rs`, and `tracker-github/src/lib.rs`.

use crate::error::{AoError, Result};
use crate::rate_limit::{enter_cooldown, in_cooldown_now, is_rate_limited_error};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `gh <args>` with env hardening, a 30 s timeout, and rate-limit
/// detection integrated. Returns stdout as a `String`.
///
/// Checks `in_cooldown_now()` before spawning and calls `enter_cooldown()`
/// when a rate-limit error is detected in stderr.
pub async fn run_gh(args: &[&str]) -> Result<String> {
    run_gh_impl(args, None).await
}

/// Like [`run_gh`], but runs inside a specific working directory.
/// Needed for `gh pr checkout` which invokes `git` under the hood.
pub async fn run_gh_in(cwd: &Path, args: &[&str]) -> Result<String> {
    run_gh_impl(args, Some(cwd)).await
}

async fn run_gh_impl(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    if in_cooldown_now() {
        return Err(AoError::Scm(
            "GitHub rate-limit cooldown active; skipping gh subprocess".into(),
        ));
    }

    let mut cmd = Command::new("gh");
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    // Strip env vars that can make `gh`'s output non-deterministic or
    // interactive. `GH_PAGER=cat` disables any pager; the update notifier
    // has occasionally corrupted JSON output with its banner.
    cmd.env("GH_PAGER", "cat");
    cmd.env("GH_NO_UPDATE_NOTIFIER", "1");
    cmd.env("NO_COLOR", "1");

    let output = tokio::time::timeout(SUBPROCESS_TIMEOUT, cmd.output())
        .await
        .map_err(|_| AoError::Scm(format!("gh {} timed out", args.join(" "))))?
        .map_err(|e| AoError::Scm(format!("gh spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_rate_limited_error(stderr.as_ref()) {
            enter_cooldown();
        }
        return Err(AoError::Scm(format!(
            "gh {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
