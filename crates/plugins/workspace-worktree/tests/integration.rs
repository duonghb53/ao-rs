//! Integration test against a real on-disk git repo. Creates a tempdir,
//! `git init`s it, runs the plugin, and verifies the worktree appears
//! correctly and is cleaned up by `destroy`.

use ao_core::{Workspace, WorkspaceCreateConfig};
use ao_plugin_workspace_worktree::WorktreeWorkspace;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Monotonic suffix so two tests running in parallel don't race on the
/// same nanosecond reading and pick the same tempdir. Before this counter
/// was added, `cargo test` occasionally failed with a collision between
/// `create_and_destroy_worktree` and `rejects_unsafe_session_id`.
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ao-rs-test-{label}-{nanos}-{n}"))
}

fn run(cmd: &str, args: &[&str], cwd: &PathBuf) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {cmd}: {e}"));
    assert!(status.success(), "{cmd} {args:?} failed in {cwd:?}");
}

fn init_repo() -> PathBuf {
    let dir = unique_dir("repo");
    std::fs::create_dir_all(&dir).unwrap();
    run("git", &["init", "-q", "-b", "main"], &dir);
    run("git", &["config", "user.email", "test@example.com"], &dir);
    run("git", &["config", "user.name", "Test"], &dir);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    run("git", &["add", "README.md"], &dir);
    run("git", &["commit", "-q", "-m", "init"], &dir);
    dir
}

#[tokio::test]
async fn create_and_destroy_worktree() {
    let repo = init_repo();
    let base = unique_dir("worktrees");
    let workspace = WorktreeWorkspace::with_base_dir(base.clone());

    let cfg = WorkspaceCreateConfig {
        project_id: "demo".to_string(),
        session_id: "sess1".to_string(),
        branch: "feat-test".to_string(),
        repo_path: repo.clone(),
        default_branch: "main".to_string(),
        symlinks: vec![],
        post_create: vec![],
    };

    let path = workspace.create(&cfg).await.expect("create failed");
    assert!(path.exists(), "worktree path not created");
    assert!(
        path.join("README.md").exists(),
        "README.md not present in worktree"
    );
    assert_eq!(path, base.join("demo").join("sess1"));

    workspace.destroy(&path).await.expect("destroy failed");
    assert!(!path.exists(), "worktree path not cleaned up");

    // Best-effort cleanup of the temp dirs themselves.
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn rejects_unsafe_session_id() {
    let repo = init_repo();
    let base = unique_dir("worktrees-bad");
    let workspace = WorktreeWorkspace::with_base_dir(base.clone());

    let cfg = WorkspaceCreateConfig {
        project_id: "demo".to_string(),
        session_id: "../escape".to_string(),
        branch: "feat-test".to_string(),
        repo_path: repo.clone(),
        default_branch: "main".to_string(),
        symlinks: vec![],
        post_create: vec![],
    };

    let result = workspace.create(&cfg).await;
    assert!(
        result.is_err(),
        "should reject path traversal in session_id"
    );

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn destroy_refuses_paths_outside_base_dir() {
    let base = unique_dir("worktrees-safety-base");
    let workspace = WorktreeWorkspace::with_base_dir(base.clone());

    let victim_parent = unique_dir("worktrees-safety-victim");
    let victim = victim_parent.join("do-not-delete");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("sentinel.txt"), "keep\n").unwrap();

    let err = workspace.destroy(&victim).await.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("refusing to destroy workspace outside base dir"),
        "unexpected error: {msg}"
    );
    assert!(victim.exists(), "victim directory was deleted");
    assert!(
        victim.join("sentinel.txt").exists(),
        "victim contents were deleted"
    );

    let _ = std::fs::remove_dir_all(&victim_parent);
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn create_symlinks_and_post_create() {
    let repo = init_repo();
    std::fs::write(repo.join(".env"), "env=1\n").unwrap();

    let base = unique_dir("worktrees-hooks");
    let workspace = WorktreeWorkspace::with_base_dir(base.clone());

    let cfg = WorkspaceCreateConfig {
        project_id: "demo".to_string(),
        session_id: "sess-hooks".to_string(),
        branch: "feat-test-hooks".to_string(),
        repo_path: repo.clone(),
        default_branch: "main".to_string(),
        symlinks: vec![".env".to_string()],
        post_create: vec!["echo ok > post_create_marker.txt".to_string()],
    };

    let path = workspace
        .create(&cfg)
        .await
        .expect("create should succeed with hooks");

    let env = path.join(".env");
    let meta = std::fs::symlink_metadata(&env).expect("symlink metadata missing");
    assert!(meta.file_type().is_symlink(), ".env should be a symlink");

    #[cfg(unix)]
    {
        let target = std::fs::read_link(&env).expect("read_link failed");
        assert_eq!(target, repo.join(".env"));
    }

    let marker = path.join("post_create_marker.txt");
    assert!(
        marker.exists(),
        "postCreate command should create marker file"
    );
    let marker_text = std::fs::read_to_string(marker).unwrap();
    assert_eq!(marker_text.trim(), "ok");

    workspace
        .destroy(&path)
        .await
        .expect("destroy should succeed");
    assert!(!path.exists(), "workspace should be removed after destroy");

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn rejects_missing_symlink_source() {
    let repo = init_repo();

    let base = unique_dir("worktrees-hooks-missing");
    let workspace = WorktreeWorkspace::with_base_dir(base.clone());

    let cfg = WorkspaceCreateConfig {
        project_id: "demo".to_string(),
        session_id: "sess-missing".to_string(),
        branch: "feat-test-missing".to_string(),
        repo_path: repo.clone(),
        default_branch: "main".to_string(),
        symlinks: vec![".missing".to_string()],
        post_create: vec![],
    };

    let err = workspace.create(&cfg).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("symlink source missing"),
        "unexpected error: {msg}"
    );

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&base);
}
