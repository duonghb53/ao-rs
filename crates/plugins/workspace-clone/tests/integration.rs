//! Integration tests against a real on-disk git repo.
//!
//! Each test creates a temporary git repository, runs the plugin, and verifies
//! the clone appears correctly and is cleaned up by `destroy`.

use ao_core::{Workspace, WorkspaceCreateConfig};
use ao_plugin_workspace_clone::CloneWorkspace;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Monotonic suffix so parallel tests never pick the same tempdir.
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ao-rs-test-clone-{label}-{nanos}-{n}"))
}

fn run(cmd: &str, args: &[&str], cwd: &PathBuf) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {cmd}: {e}"));
    assert!(status.success(), "{cmd} {args:?} failed in {cwd:?}");
}

/// Create a minimal git repo with one commit on `main`.
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
async fn create_and_destroy_clone() {
    let repo = init_repo();
    let base = unique_dir("clones");
    let workspace = CloneWorkspace::with_base_dir(base.clone());

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

    // The clone must exist at the expected path.
    assert!(path.exists(), "clone path not created");
    assert_eq!(path, base.join("demo").join("sess1"));

    // The cloned content must be present.
    assert!(
        path.join("README.md").exists(),
        "README.md not present in clone"
    );

    // The session branch must be checked out.
    let branch_output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&path)
        .output()
        .expect("git rev-parse failed");
    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    assert_eq!(branch, "feat-test", "wrong branch checked out");

    workspace.destroy(&path).await.expect("destroy failed");
    assert!(!path.exists(), "clone path not cleaned up after destroy");

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn shallow_clone_creates_valid_workspace() {
    let repo = init_repo();
    let base = unique_dir("clones-shallow");
    let workspace = CloneWorkspace::with_base_dir(base.clone()).with_depth(1);

    let cfg = WorkspaceCreateConfig {
        project_id: "demo".to_string(),
        session_id: "shallow1".to_string(),
        branch: "feat-shallow".to_string(),
        repo_path: repo.clone(),
        default_branch: "main".to_string(),
        symlinks: vec![],
        post_create: vec![],
    };

    let path = workspace.create(&cfg).await.expect("shallow create failed");
    assert!(path.exists(), "shallow clone path not created");
    assert!(
        path.join("README.md").exists(),
        "README.md missing in shallow clone"
    );

    workspace.destroy(&path).await.expect("destroy failed");
    assert!(!path.exists(), "shallow clone not cleaned up");

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn rejects_unsafe_session_id() {
    let repo = init_repo();
    let base = unique_dir("clones-bad");
    let workspace = CloneWorkspace::with_base_dir(base.clone());

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
async fn rejects_unsafe_project_id() {
    let repo = init_repo();
    let base = unique_dir("clones-bad-proj");
    let workspace = CloneWorkspace::with_base_dir(base.clone());

    let cfg = WorkspaceCreateConfig {
        project_id: "bad/project".to_string(),
        session_id: "sess1".to_string(),
        branch: "feat-test".to_string(),
        repo_path: repo.clone(),
        default_branch: "main".to_string(),
        symlinks: vec![],
        post_create: vec![],
    };

    let result = workspace.create(&cfg).await;
    assert!(
        result.is_err(),
        "should reject path traversal in project_id"
    );

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn destroy_is_idempotent() {
    let base = unique_dir("clones-idempotent");
    let workspace = CloneWorkspace::with_base_dir(base.clone());

    // Destroying a path that does not exist must succeed (no error).
    let nonexistent = base.join("proj").join("no-such-session");
    workspace
        .destroy(&nonexistent)
        .await
        .expect("destroy of nonexistent path should succeed");

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn create_symlinks_and_post_create() {
    let repo = init_repo();
    std::fs::write(repo.join(".env"), "env=1\n").unwrap();

    let base = unique_dir("clones-hooks");
    let workspace = CloneWorkspace::with_base_dir(base.clone());

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
        .expect("clone create should succeed with hooks");

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

    let base = unique_dir("clones-hooks-missing");
    let workspace = CloneWorkspace::with_base_dir(base.clone());

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
