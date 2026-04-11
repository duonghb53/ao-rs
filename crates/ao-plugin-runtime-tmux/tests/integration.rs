//! Integration tests against real tmux. These are skipped if `tmux` is not
//! installed. Each test uses a unique session id (uuid prefix) so concurrent
//! runs don't collide and a crashed test doesn't poison subsequent runs.

use ao_core::Runtime;
use ao_plugin_runtime_tmux::TmuxRuntime;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn unique_session_id(label: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("aorstest-{label}-{nanos}")
}

#[tokio::test]
async fn create_then_destroy_session() {
    if !tmux_available() {
        eprintln!("tmux not installed — skipping");
        return;
    }

    let runtime = TmuxRuntime::new();
    let session_id = unique_session_id("create");
    let cwd = PathBuf::from("/tmp");

    // Use a long-running launch command so the session stays alive while we check.
    let handle = runtime
        .create(&session_id, &cwd, "sleep 30", &[])
        .await
        .expect("create failed");

    assert_eq!(handle, session_id);
    assert!(
        runtime.is_alive(&handle).await.unwrap(),
        "session not alive"
    );

    runtime.destroy(&handle).await.expect("destroy failed");

    // Give tmux a beat to actually tear it down.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !runtime.is_alive(&handle).await.unwrap(),
        "session still alive after destroy"
    );
}

#[tokio::test]
async fn send_message_to_running_session() {
    if !tmux_available() {
        eprintln!("tmux not installed — skipping");
        return;
    }

    let runtime = TmuxRuntime::new();
    let session_id = unique_session_id("send");
    let cwd = PathBuf::from("/tmp");

    let handle = runtime
        .create(&session_id, &cwd, "sleep 30", &[])
        .await
        .expect("create failed");

    // Should not error even if the message lands on a sleeping shell.
    let result = runtime.send_message(&handle, "echo hello").await;

    // Always destroy, even if send_message failed.
    let _ = runtime.destroy(&handle).await;

    result.expect("send_message failed");
}

#[tokio::test]
async fn rejects_invalid_session_id() {
    if !tmux_available() {
        eprintln!("tmux not installed — skipping");
        return;
    }

    let runtime = TmuxRuntime::new();
    let result = runtime
        .create("../escape", &PathBuf::from("/tmp"), "true", &[])
        .await;
    assert!(result.is_err());
}
