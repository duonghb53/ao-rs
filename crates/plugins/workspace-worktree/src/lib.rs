//! Git worktree workspace plugin.
//!
//! Creates an isolated working directory under `~/.worktrees/{project}/{session}`
//! by running `git worktree add` against the source repo. Equivalent to
//! `packages/plugins/workspace-worktree/src/index.ts` in the reference repo,
//! but with Slice 0 scope: no symlinks, no postCreate hooks, no list/restore.

use ao_core::workspace_hooks::apply_workspace_hooks;
use ao_core::{AoError, Result, Workspace, WorkspaceCreateConfig};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Workspace implementation backed by `git worktree`.
pub struct WorktreeWorkspace {
    base_dir: PathBuf,
}

impl WorktreeWorkspace {
    /// Create with the default base dir `~/.worktrees`.
    pub fn new() -> Self {
        Self {
            base_dir: home_dir().join(".worktrees"),
        }
    }

    /// Create with an explicit base dir (useful for tests).
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn assert_under_base_dir(&self, workspace_path: &Path) -> Result<()> {
        // `destroy()` may be called with a path loaded from disk (session YAML).
        // Treat the configured base dir as an allowlist: never delete anything
        // outside it, even on fallback cleanup.
        //
        // Canonicalize when possible to defend against `..` segments and symlinks.
        // If canonicalization fails (e.g. path doesn't exist yet), fall back to a
        // lexical check.
        let base = canonical_or_clean(&self.base_dir);
        let ws = canonical_or_clean(workspace_path);

        if !path_is_within(&ws, &base) {
            return Err(AoError::Workspace(format!(
                "refusing to destroy workspace outside base dir: base={} workspace={}",
                base.display(),
                ws.display()
            )));
        }
        Ok(())
    }
}

impl Default for WorktreeWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Workspace for WorktreeWorkspace {
    async fn create(&self, cfg: &WorkspaceCreateConfig) -> Result<PathBuf> {
        assert_safe_segment(&cfg.project_id, "project_id")?;
        assert_safe_segment(&cfg.session_id, "session_id")?;

        let project_dir = self.base_dir.join(&cfg.project_id);
        let worktree_path = project_dir.join(&cfg.session_id);

        tokio::fs::create_dir_all(&project_dir).await?;

        let has_origin = has_origin_remote(&cfg.repo_path).await;

        // Best-effort fetch — offline is fine.
        if has_origin {
            let _ = git(&cfg.repo_path, &["fetch", "origin", "--quiet"]).await;
        }

        let base_ref = resolve_base_ref(&cfg.repo_path, &cfg.default_branch, has_origin).await?;
        let worktree_str = path_to_str(&worktree_path)?;

        // Happy path: create worktree on a brand-new branch.
        let first_attempt = git(
            &cfg.repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &cfg.branch,
                &worktree_str,
                &base_ref,
            ],
        )
        .await;

        let created_path = match first_attempt {
            Ok(_) => worktree_path,
            Err(AoError::Workspace(msg)) if msg.contains("already exists") => {
                // Branch already exists — create the worktree without -b, then check it out.
                git(
                    &cfg.repo_path,
                    &["worktree", "add", &worktree_str, &base_ref],
                )
                .await?;

                if let Err(checkout_err) = git(&worktree_path, &["checkout", &cfg.branch]).await {
                    // Checkout failed — best-effort cleanup of orphaned worktree.
                    let _ = git(
                        &cfg.repo_path,
                        &["worktree", "remove", "--force", &worktree_str],
                    )
                    .await;
                    return Err(checkout_err);
                }

                worktree_path
            }
            Err(e) => return Err(e),
        };

        // Workspace hooks: symlinks + `postCreate`.
        if let Err(hook_err) = apply_workspace_hooks(
            &cfg.repo_path,
            &created_path,
            &cfg.symlinks,
            &cfg.post_create,
        )
        .await
        {
            // Avoid leaving a half-materialized workspace behind.
            let _ = self.destroy(&created_path).await;
            return Err(hook_err);
        }

        Ok(created_path)
    }

    async fn destroy(&self, workspace_path: &Path) -> Result<()> {
        self.assert_under_base_dir(workspace_path)?;
        let worktree_str = path_to_str(workspace_path)?;

        // Try to find the parent repo via git itself.
        if let Ok(common_dir) = git(
            workspace_path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await
        {
            let common_dir = PathBuf::from(common_dir);
            // git-common-dir returns `.../repo/.git` — repo path is the parent.
            let repo_path = common_dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| common_dir.clone());

            let _ = git(
                &repo_path,
                &["worktree", "remove", "--force", &worktree_str],
            )
            .await;
        }

        // Fallback: best-effort directory removal if git couldn't clean it.
        if workspace_path.exists() {
            let _ = tokio::fs::remove_dir_all(workspace_path).await;
        }

        Ok(())
    }
}

// ---------- helpers ----------

fn canonical_or_clean(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| {
        // Best-effort lexical normalization: strip trailing separators and
        // remove `.` segments. We do *not* try to resolve `..` here.
        let mut out = PathBuf::new();
        for part in p.components() {
            use std::path::Component;
            match part {
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    })
}

fn path_is_within(child: &Path, base: &Path) -> bool {
    // `starts_with` is path-component aware; with canonical paths it provides
    // the containment guarantee we want.
    child.starts_with(base)
}

/// Run `git <args>` in `cwd`, return trimmed stdout or a structured error.
async fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| AoError::Workspace(format!("git spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AoError::Workspace(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

async fn has_origin_remote(cwd: &Path) -> bool {
    git(cwd, &["remote", "get-url", "origin"]).await.is_ok()
}

async fn ref_exists(cwd: &Path, reference: &str) -> bool {
    git(cwd, &["rev-parse", "--verify", "--quiet", reference])
        .await
        .is_ok()
}

/// Mirror the TS `resolveBaseRef` logic: prefer `origin/<default>` if origin
/// exists, fall back to `refs/heads/<default>`.
async fn resolve_base_ref(
    repo_path: &Path,
    default_branch: &str,
    has_origin: bool,
) -> Result<String> {
    if has_origin {
        let remote_default = format!("origin/{default_branch}");
        if ref_exists(repo_path, &remote_default).await {
            return Ok(remote_default);
        }
    }

    let local_default = format!("refs/heads/{default_branch}");
    if ref_exists(repo_path, &local_default).await {
        return Ok(local_default);
    }

    Err(AoError::Workspace(format!(
        "unable to resolve base ref for default branch \"{default_branch}\""
    )))
}

/// Reject anything that isn't `[a-zA-Z0-9_-]+` to prevent path traversal.
/// Mirrors the TS `assertSafePathSegment`.
fn assert_safe_segment(value: &str, label: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err(AoError::Workspace(format!(
            "invalid {label} \"{value}\": must be [a-zA-Z0-9_-]+"
        )));
    }
    Ok(())
}

fn path_to_str(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AoError::Workspace(format!("path is not valid UTF-8: {}", p.display())))
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_segment_accepts_normal_ids() {
        assert!(assert_safe_segment("my-project_42", "x").is_ok());
        assert!(assert_safe_segment("a", "x").is_ok());
    }

    #[test]
    fn safe_segment_rejects_traversal() {
        assert!(assert_safe_segment("../etc", "x").is_err());
        assert!(assert_safe_segment("foo/bar", "x").is_err());
        assert!(assert_safe_segment("", "x").is_err());
        assert!(assert_safe_segment("foo bar", "x").is_err());
    }
}
