use crate::{AoError, Result};
use std::path::{Component, Path};
use tokio::process::Command;

/// Create configured symlinks and execute `postCreate` commands for a
/// workspace, matching ao-ts behavior at a pragmatic level.
///
/// - `symlinks` entries are treated as *relative paths* from `project_root`
///   (git repo root) to create inside `workspace_root`.
/// - Each `postCreate` command is executed via a shell with
///   `workspace_root` as the current working directory.
pub async fn apply_workspace_hooks(
    project_root: &Path,
    workspace_root: &Path,
    symlinks: &[String],
    post_create: &[String],
) -> Result<()> {
    for entry in symlinks {
        validate_symlink_entry(entry)?;
        let src = project_root.join(entry);
        let dest = workspace_root.join(entry);

        if !src.exists() {
            return Err(AoError::Workspace(format!(
                "symlink source missing for entry {entry:?}: {}",
                src.display()
            )));
        }

        // Avoid clobbering anything users may already have staged into a
        // workspace. Workspaces are expected to be fresh per session.
        if dest.symlink_metadata().is_ok() {
            return Err(AoError::Workspace(format!(
                "workspace path already exists for symlink {entry:?}: {}",
                dest.display()
            )));
        }

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        create_symlink(&src, &dest).map_err(|e| {
            AoError::Workspace(format!(
                "failed to create symlink {entry:?}: {} -> {} ({e})",
                src.display(),
                dest.display()
            ))
        })?;
    }

    for cmd in post_create {
        run_post_create_command(workspace_root, cmd).await?;
    }

    Ok(())
}

/// Reject `symlinks` path segments that could escape the workspace root.
///
/// We only allow *relative* paths made of `Normal` components and we
/// disallow:
/// - absolute paths
/// - `..` parent traversal
/// - `.` segments (keeps behavior predictable)
pub fn validate_symlink_entry(entry: &str) -> Result<()> {
    let p = Path::new(entry);

    if entry.is_empty() {
        return Err(AoError::Workspace("symlink entry must not be empty".into()));
    }
    // Reject `.` segments explicitly. `Path::components()` may represent
    // `./` inconsistently (e.g. as `CurDir` vs `Normal(".")`) across
    // platforms, so we also validate at the string level.
    if entry == "." || entry.starts_with("./") || entry.contains("/./") || entry.ends_with("/.") {
        return Err(AoError::Workspace(format!(
            "symlink entry must not contain '.' segments: {entry:?}"
        )));
    }
    if p.is_absolute() {
        return Err(AoError::Workspace(format!(
            "symlink entry must be relative, got {entry:?}"
        )));
    }

    for c in p.components() {
        match c {
            Component::Normal(s) => {
                if s == "." {
                    return Err(AoError::Workspace(format!(
                        "symlink entry must not contain '.': {entry:?}"
                    )));
                }
            }
            Component::CurDir => {
                return Err(AoError::Workspace(format!(
                    "symlink entry must not contain '.': {entry:?}"
                )));
            }
            Component::ParentDir => {
                return Err(AoError::Workspace(format!(
                    "symlink entry must not contain '..': {entry:?}"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(AoError::Workspace(format!(
                    "symlink entry must not contain absolute components: {entry:?}"
                )));
            }
        }
    }

    Ok(())
}

fn create_symlink(src: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(src, dest)
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::{symlink_dir, symlink_file};

        let src_meta = std::fs::metadata(src)?;
        if src_meta.is_dir() {
            symlink_dir(src, dest)
        } else {
            symlink_file(src, dest)
        }
    }
}

async fn run_post_create_command(workspace_root: &Path, cmd: &str) -> Result<()> {
    // User-provided string: intended to be executed intentionally via shell.
    let mut command = shell_command();
    command.arg(cmd).current_dir(workspace_root);

    let output = command
        .output()
        .await
        .map_err(|e| AoError::Workspace(format!("postCreate spawn failed: {e}")))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        return Err(AoError::Workspace(format!(
            "postCreate command failed (exit={}) cmd={cmd:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            truncate(&stdout, 4000),
            truncate(&stderr, 4000),
        )));
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    let mut iter = s.chars();
    let mut out = String::new();
    for _ in 0..max {
        match iter.next() {
            Some(ch) => out.push(ch),
            None => return out,
        }
    }

    // We consumed `max` chars and there may be more.
    out.push('…');
    out
}

/// Shell that can interpret `cmd` strings.
fn shell_command() -> Command {
    #[cfg(unix)]
    {
        let mut c = Command::new("sh");
        c.arg("-c");
        c
    }

    #[cfg(not(unix))]
    {
        let mut c = Command::new("cmd");
        c.args(["/C"]);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ao-rs-hooks-{label}-{n}"))
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(validate_symlink_entry("").is_err());
    }

    #[test]
    fn validate_accepts_relative_normals() {
        assert!(validate_symlink_entry(".env").is_ok());
        assert!(validate_symlink_entry("dir/file.txt").is_ok());
    }

    #[test]
    fn validate_rejects_traversal() {
        for s in ["../x", "a/../b", "./x", "a/./b", "..", ""] {
            if s.is_empty() {
                continue;
            }
            assert!(validate_symlink_entry(s).is_err(), "expected reject: {s}");
        }
    }

    #[tokio::test]
    async fn apply_fails_when_source_missing() {
        let project_root = unique_dir("project-missing");
        let workspace_root = unique_dir("ws-missing");
        let _ = tokio::fs::create_dir_all(&project_root).await;
        let _ = tokio::fs::create_dir_all(&workspace_root).await;

        let symlinks = [".env".to_string()];
        let post_create: [String; 0] = [];
        let err = apply_workspace_hooks(&project_root, &workspace_root, &symlinks, &post_create)
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("symlink source missing"),
            "unexpected error: {msg}"
        );

        let _ = tokio::fs::remove_dir_all(&project_root).await;
        let _ = tokio::fs::remove_dir_all(&workspace_root).await;
    }
}
