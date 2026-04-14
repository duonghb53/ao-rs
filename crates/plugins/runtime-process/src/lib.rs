//! Process runtime plugin — runs agents as plain OS processes (no tmux).
//!
//! Each ao session maps to one child process spawned via `sh -c <launch_command>`.
//! Messages are delivered by writing to the process's stdin, followed by a
//! newline.  Liveness is checked with [`Child::try_wait`].
//!
//! ## IO strategy
//! `send_message` writes the message text and a trailing `\n` directly to the
//! child process's stdin pipe.  The agent binary is expected to read prompts
//! from stdin one line at a time.  This mirrors how tmux would paste text into
//! a terminal — the key difference is that no terminal emulator is involved.
//!
//! ## Limitations vs `runtime-tmux`
//! - **No terminal multiplexer**: output goes to the process's inherited
//!   stdout/stderr, which is only visible while ao-cli is in the foreground.
//! - **Process lifetime is tied to ao-cli**: child processes are children of
//!   the ao-rs process.  If ao-rs exits they become orphans; they are *not*
//!   automatically re-attached on the next `ao-rs watch`.
//! - **`ao-rs session attach` is unsupported**: `attach` execs into a tmux
//!   session and cannot work with plain processes.
//! - **No scrollback**: without a pty, there is no terminal scrollback buffer.

use ao_core::{AoError, Result, Runtime};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;
use tracing::{debug, warn};
use uuid::Uuid;

// ── Internal state ────────────────────────────────────────────────────────────

struct ProcessEntry {
    /// Owned child handle; stdin has already been extracted into `stdin`.
    child: Child,
    /// Write end of the child's stdin pipe.
    stdin: ChildStdin,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Runtime that runs agents as plain child processes.
///
/// Create with [`ProcessRuntime::new`].  All methods are safe to call
/// concurrently from multiple async tasks.
pub struct ProcessRuntime {
    processes: Arc<Mutex<HashMap<String, ProcessEntry>>>,
}

impl ProcessRuntime {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for ProcessRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ── Runtime impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl Runtime for ProcessRuntime {
    /// Spawn a child process by running `sh -c <launch_command>`.
    ///
    /// Returns an opaque UUID handle that the caller stores in
    /// `Session::runtime_handle` and passes back to other methods.
    async fn create(
        &self,
        session_id: &str,
        cwd: &Path,
        launch_command: &str,
        env: &[(String, String)],
    ) -> Result<String> {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", launch_command]);
        cmd.current_dir(cwd);
        // Pipe stdin so we can write to it via send_message.
        cmd.stdin(std::process::Stdio::piped());
        // Inherit stdout/stderr: agent output appears in ao-cli's terminal.
        cmd.stdout(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            AoError::Runtime(format!(
                "process runtime: failed to spawn session {session_id}: {e}"
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            AoError::Runtime("process runtime: stdin pipe unavailable after spawn".to_string())
        })?;

        let handle = Uuid::new_v4().to_string();
        debug!(session_id, handle, "spawned process runtime");

        self.processes
            .lock()
            .await
            .insert(handle.clone(), ProcessEntry { child, stdin });

        Ok(handle)
    }

    /// Write `msg` followed by a newline to the child's stdin and flush.
    async fn send_message(&self, handle: &str, msg: &str) -> Result<()> {
        let mut guard = self.processes.lock().await;
        let entry = guard
            .get_mut(handle)
            .ok_or_else(|| AoError::Runtime(format!("process runtime: unknown handle {handle}")))?;

        entry
            .stdin
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| AoError::Runtime(format!("process runtime: stdin write failed: {e}")))?;
        entry
            .stdin
            .write_all(b"\n")
            .await
            .map_err(|e| AoError::Runtime(format!("process runtime: stdin newline failed: {e}")))?;
        entry
            .stdin
            .flush()
            .await
            .map_err(|e| AoError::Runtime(format!("process runtime: stdin flush failed: {e}")))?;

        Ok(())
    }

    /// Return `true` if the child process has not yet exited.
    async fn is_alive(&self, handle: &str) -> Result<bool> {
        let mut guard = self.processes.lock().await;
        let Some(entry) = guard.get_mut(handle) else {
            return Ok(false);
        };
        match entry.child.try_wait() {
            Ok(Some(_)) => Ok(false), // process has exited
            Ok(None) => Ok(true),     // still running
            Err(e) => Err(AoError::Runtime(format!(
                "process runtime: try_wait failed: {e}"
            ))),
        }
    }

    /// Kill the child process and remove it from the registry.
    ///
    /// Best-effort: if the process has already exited the kill is silently
    /// ignored, matching the behaviour of `runtime-tmux`.
    async fn destroy(&self, handle: &str) -> Result<()> {
        let mut guard = self.processes.lock().await;
        if let Some(mut entry) = guard.remove(handle) {
            if let Err(e) = entry.child.kill().await {
                warn!(
                    handle,
                    "process runtime: kill failed (may already be gone): {e}"
                );
            }
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_cwd() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    /// Two `create` calls must return distinct handles.
    #[tokio::test]
    async fn create_returns_unique_handles() {
        let rt = ProcessRuntime::new();
        let cwd = tmp_cwd();
        let h1 = rt.create("s1", &cwd, "cat", &[]).await.unwrap();
        let h2 = rt.create("s2", &cwd, "cat", &[]).await.unwrap();
        assert_ne!(h1, h2);
        rt.destroy(&h1).await.unwrap();
        rt.destroy(&h2).await.unwrap();
    }

    /// A freshly-spawned long-running process (`cat`) must be alive.
    #[tokio::test]
    async fn is_alive_true_for_running_process() {
        let rt = ProcessRuntime::new();
        let cwd = tmp_cwd();
        let handle = rt.create("s1", &cwd, "cat", &[]).await.unwrap();
        assert!(rt.is_alive(&handle).await.unwrap());
        rt.destroy(&handle).await.unwrap();
    }

    /// An unknown handle should report not-alive (not an error).
    #[tokio::test]
    async fn is_alive_false_for_unknown_handle() {
        let rt = ProcessRuntime::new();
        assert!(!rt.is_alive("no-such-handle").await.unwrap());
    }

    /// Destroying an unknown handle is a no-op.
    #[tokio::test]
    async fn destroy_unknown_handle_is_ok() {
        let rt = ProcessRuntime::new();
        rt.destroy("no-such-handle").await.unwrap();
    }

    /// After `destroy`, `is_alive` must return false (entry is removed).
    #[tokio::test]
    async fn is_alive_false_after_destroy() {
        let rt = ProcessRuntime::new();
        let cwd = tmp_cwd();
        let handle = rt.create("s1", &cwd, "cat", &[]).await.unwrap();
        rt.destroy(&handle).await.unwrap();
        assert!(!rt.is_alive(&handle).await.unwrap());
    }

    /// `send_message` must succeed for a running process that reads stdin.
    #[tokio::test]
    async fn send_message_delivers_to_stdin() {
        let rt = ProcessRuntime::new();
        let cwd = tmp_cwd();
        // `cat > /dev/null` consumes stdin silently.
        let handle = rt.create("s1", &cwd, "cat > /dev/null", &[]).await.unwrap();
        rt.send_message(&handle, "hello from test").await.unwrap();
        rt.destroy(&handle).await.unwrap();
    }

    /// A process that exits immediately should report as not-alive shortly after.
    #[tokio::test]
    async fn is_alive_false_after_process_exits() {
        let rt = ProcessRuntime::new();
        let cwd = tmp_cwd();
        // `true` exits immediately.
        let handle = rt.create("s1", &cwd, "true", &[]).await.unwrap();
        // Give the OS a moment to reap the child.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!rt.is_alive(&handle).await.unwrap());
        rt.destroy(&handle).await.unwrap();
    }
}
