//! Git clone workspace plugin.
//!
//! Creates an isolated working directory under `~/.clones/{project}/{session}`
//! by running `git clone` against the source repo. Each session gets a fully
//! independent clone — no shared `.git` — so processes inside cannot interfere
//! with each other or with the origin repo.
//!
//! Equivalent to `packages/plugins/workspace-clone` in the reference TypeScript
//! repo but scoped to the same minimal surface as the worktree plugin:
//! no symlinks, no postCreate hooks, no list/restore.

use ao_core::workspace_hooks::apply_workspace_hooks;
use ao_core::{AoError, Result, Workspace, WorkspaceCreateConfig};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Workspace implementation backed by `git clone`.
///
/// The clone layout is `base_dir/{project_id}/{session_id}`.
///
/// # Shallow clones
///
/// By default a full clone is performed. Pass a depth via [`CloneWorkspace::with_depth`]
/// to get a shallow clone (`--depth N`). Depth `1` is the most common choice and
/// produces the smallest clone at the cost of missing earlier history.
pub struct CloneWorkspace {
    base_dir: PathBuf,
    /// When `Some(n)`, passes `--depth n` to `git clone` for a shallow clone.
    /// `None` (default) performs a full clone.
    depth: Option<u32>,
}

impl CloneWorkspace {
    /// Create with the default base dir `~/.clones` and a full (non-shallow) clone.
    pub fn new() -> Self {
        Self {
            base_dir: home_dir().join(".clones"),
            depth: None,
        }
    }

    /// Create with an explicit base dir (useful for tests).
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            depth: None,
        }
    }

    /// Set the clone depth for shallow clones (e.g. `1`).
    /// Call after `new()` or `with_base_dir()`.
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = Some(depth);
        self
    }
}

impl Default for CloneWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Workspace for CloneWorkspace {
    /// Clone the repo into `base_dir/{project_id}/{session_id}` and create
    /// a new branch `cfg.branch` starting from `cfg.default_branch`.
    async fn create(&self, cfg: &WorkspaceCreateConfig) -> Result<PathBuf> {
        assert_safe_segment(&cfg.project_id, "project_id")?;
        assert_safe_segment(&cfg.session_id, "session_id")?;

        let project_dir = self.base_dir.join(&cfg.project_id);
        let clone_path = project_dir.join(&cfg.session_id);

        tokio::fs::create_dir_all(&project_dir).await?;

        let repo_str = path_to_str(&cfg.repo_path)?;
        let clone_str = path_to_str(&clone_path)?;
        let depth_str = self.depth.map(|d| d.to_string());

        // Build the clone argument list.
        // --local + --no-hardlinks: safe copy even across filesystems.
        // --single-branch: only fetch the default branch history, keeping the
        //   clone lean while still allowing new branches to be created locally.
        let mut args: Vec<&str> = vec![
            "clone",
            "--local",
            "--no-hardlinks",
            "--single-branch",
            "--branch",
            &cfg.default_branch,
        ];

        if let Some(ref d) = depth_str {
            args.extend_from_slice(&["--depth", d]);
        }

        args.push(&repo_str);
        args.push(&clone_str);

        git(&project_dir, &args).await?;

        // Create the target session branch from the cloned default branch.
        git(&clone_path, &["checkout", "-b", &cfg.branch]).await?;

        tracing::debug!(
            clone = %clone_path.display(),
            branch = %cfg.branch,
            "workspace clone created"
        );

        // Workspace hooks: symlinks + `postCreate`.
        if let Err(hook_err) =
            apply_workspace_hooks(&cfg.repo_path, &clone_path, &cfg.symlinks, &cfg.post_create)
                .await
        {
            let _ = self.destroy(&clone_path).await;
            return Err(hook_err);
        }

        Ok(clone_path)
    }

    /// Remove the cloned directory entirely. Unlike worktrees, there is no
    /// shared git bookkeeping to update — a plain directory removal is enough.
    async fn destroy(&self, workspace_path: &Path) -> Result<()> {
        if workspace_path.exists() {
            tokio::fs::remove_dir_all(workspace_path).await?;
            tracing::debug!(path = %workspace_path.display(), "workspace clone destroyed");
        }
        Ok(())
    }
}

// ---------- helpers ----------

/// Run `git <args>` with `cwd` as the working directory.
/// Returns trimmed stdout on success, or a structured `AoError::Workspace` on failure.
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

/// Reject anything that isn't `[a-zA-Z0-9_-]+` to prevent path traversal.
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

    #[test]
    fn with_depth_sets_field() {
        let ws = CloneWorkspace::with_base_dir(PathBuf::from("/tmp")).with_depth(1);
        assert_eq!(ws.depth, Some(1));
    }

    #[test]
    fn default_depth_is_none() {
        let ws = CloneWorkspace::with_base_dir(PathBuf::from("/tmp"));
        assert_eq!(ws.depth, None);
    }
}
