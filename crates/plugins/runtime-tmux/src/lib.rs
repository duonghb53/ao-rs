//! tmux runtime plugin.
//!
//! Each session maps to one detached tmux session named after `session_id`.
//! The launch command is delivered via `send-keys` (or `load-buffer` +
//! `paste-buffer` if it's long, to avoid pasting a wall of shell into the
//! pane). Mirrors `packages/plugins/runtime-tmux/src/index.ts`.

use ao_core::{AoError, Result, Runtime};
use async_trait::async_trait;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// Per-tmux-command timeout. Mirrors `TMUX_COMMAND_TIMEOUT_MS` in the TS plugin.
const TMUX_TIMEOUT: Duration = Duration::from_secs(5);

/// Threshold above which we wrap the launch command in a temp script instead
/// of pasting it directly. Keeps the pane visually clean for long commands.
const LONG_COMMAND_THRESHOLD: usize = 200;

pub struct TmuxRuntime;

impl TmuxRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TmuxRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Runtime for TmuxRuntime {
    async fn create(
        &self,
        session_id: &str,
        cwd: &Path,
        launch_command: &str,
        env: &[(String, String)],
    ) -> Result<String> {
        assert_safe_session_id(session_id)?;
        let cwd_str = cwd
            .to_str()
            .ok_or_else(|| AoError::Runtime(format!("cwd not valid UTF-8: {}", cwd.display())))?;

        // Build `tmux new-session -d -s <id> -c <cwd> [-e KEY=VAL ...]`
        let mut args: Vec<String> = vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            session_id.into(),
            "-c".into(),
            cwd_str.into(),
        ];
        for (k, v) in env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        tmux(&args).await?;

        // Send the launch command. Long commands go through a temp script.
        let send_result = if launch_command.len() > LONG_COMMAND_THRESHOLD {
            send_long_launch(session_id, launch_command).await
        } else {
            tmux(&[
                "send-keys".into(),
                "-t".into(),
                session_id.into(),
                launch_command.into(),
                "Enter".into(),
            ])
            .await
            .map(|_| ())
        };

        if let Err(e) = send_result {
            // Best-effort cleanup of the orphaned session.
            let _ = tmux(&["kill-session".into(), "-t".into(), session_id.into()]).await;
            return Err(e);
        }

        Ok(session_id.to_string())
    }

    async fn send_message(&self, handle: &str, message: &str) -> Result<()> {
        // Clear any partial input first.
        tmux(&["send-keys".into(), "-t".into(), handle.into(), "C-u".into()]).await?;

        if message.contains('\n') || message.len() > LONG_COMMAND_THRESHOLD {
            paste_via_buffer(handle, message).await?;
        } else {
            // -l = literal, so words like "Enter" or "Space" aren't interpreted
            // as tmux key names.
            tmux(&[
                "send-keys".into(),
                "-t".into(),
                handle.into(),
                "-l".into(),
                message.into(),
            ])
            .await?;
        }

        // Small delay to let tmux render before we hit Enter — without this,
        // Enter can race ahead of the pasted text.
        tokio::time::sleep(Duration::from_millis(300)).await;
        tmux(&[
            "send-keys".into(),
            "-t".into(),
            handle.into(),
            "Enter".into(),
        ])
        .await?;

        Ok(())
    }

    async fn is_alive(&self, handle: &str) -> Result<bool> {
        Ok(tmux(&["has-session".into(), "-t".into(), handle.into()])
            .await
            .is_ok())
    }

    async fn destroy(&self, handle: &str) -> Result<()> {
        // Already-dead sessions are fine — best effort.
        let _ = tmux(&["kill-session".into(), "-t".into(), handle.into()]).await;
        Ok(())
    }
}

// ---------- helpers ----------

/// Run `tmux <args>` with a timeout, returning trimmed stdout.
async fn tmux(args: &[String]) -> Result<String> {
    let fut = Command::new("tmux").args(args).output();

    let output = tokio::time::timeout(TMUX_TIMEOUT, fut)
        .await
        .map_err(|_| AoError::Runtime(format!("tmux {} timed out", args.join(" "))))?
        .map_err(|e| AoError::Runtime(format!("tmux spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AoError::Runtime(format!(
            "tmux {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// Write the long launch command to a self-deleting bash script and feed
/// `bash <script>` into the pane instead. Mirrors `writeLaunchScript` in TS.
async fn send_long_launch(session_id: &str, command: &str) -> Result<()> {
    let script_path = std::env::temp_dir().join(format!("ao-launch-{}.sh", uuid::Uuid::new_v4()));
    let script_str = script_path
        .to_str()
        .ok_or_else(|| AoError::Runtime("temp path not UTF-8".into()))?
        .to_string();

    // The `rm -- "$0"` line makes the script delete itself on first run.
    let content = format!("#!/usr/bin/env bash\nrm -- \"$0\" 2>/dev/null || true\n{command}\n");
    tokio::fs::write(&script_path, content)
        .await
        .map_err(|e| AoError::Runtime(format!("write launch script: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(&script_path)
            .await
            .map_err(|e| AoError::Runtime(format!("stat launch script: {e}")))?
            .permissions();
        perms.set_mode(0o700);
        tokio::fs::set_permissions(&script_path, perms)
            .await
            .map_err(|e| AoError::Runtime(format!("chmod launch script: {e}")))?;
    }

    let invocation = format!("bash {}", shell_escape(&script_str));

    tmux(&[
        "send-keys".into(),
        "-t".into(),
        session_id.into(),
        "-l".into(),
        invocation,
    ])
    .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    tmux(&[
        "send-keys".into(),
        "-t".into(),
        session_id.into(),
        "Enter".into(),
    ])
    .await?;
    Ok(())
}

/// For long/multiline messages, write to a temp file → `load-buffer` →
/// `paste-buffer -d`. Avoids issues with newline interpretation in `send-keys`.
async fn paste_via_buffer(handle: &str, message: &str) -> Result<()> {
    let buffer_name = format!("ao-{}", uuid::Uuid::new_v4());
    let tmp_path = std::env::temp_dir().join(format!("ao-send-{}.txt", uuid::Uuid::new_v4()));
    let tmp_str = tmp_path
        .to_str()
        .ok_or_else(|| AoError::Runtime("temp path not UTF-8".into()))?
        .to_string();

    tokio::fs::write(&tmp_path, message)
        .await
        .map_err(|e| AoError::Runtime(format!("write send buffer: {e}")))?;

    let load = tmux(&[
        "load-buffer".into(),
        "-b".into(),
        buffer_name.clone(),
        tmp_str,
    ])
    .await;

    let paste = if load.is_ok() {
        tmux(&[
            "paste-buffer".into(),
            "-b".into(),
            buffer_name.clone(),
            "-t".into(),
            handle.into(),
            "-d".into(),
        ])
        .await
        .map(|_| ())
    } else {
        load.map(|_| ())
    };

    // Cleanup — best effort.
    let _ = tokio::fs::remove_file(&tmp_path).await;
    let _ = tmux(&["delete-buffer".into(), "-b".into(), buffer_name]).await;

    paste
}

/// Minimal POSIX shell escape. Wraps in single quotes if the value contains
/// anything outside a small safe set.
fn shell_escape(s: &str) -> String {
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '='));
    if safe {
        return s.to_string();
    }
    // Replace ' with '\'' (close-quote, escape, reopen-quote).
    let escaped = s.replace('\'', r#"'\''"#);
    format!("'{escaped}'")
}

fn assert_safe_session_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err(AoError::Runtime(format!(
            "invalid session id \"{id}\": must be [a-zA-Z0-9_-]+"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_keeps_safe_strings() {
        assert_eq!(shell_escape("hello"), "hello");
        assert_eq!(shell_escape("/tmp/foo.sh"), "/tmp/foo.sh");
        assert_eq!(shell_escape("FOO=bar"), "FOO=bar");
    }

    #[test]
    fn shell_escape_quotes_unsafe_strings() {
        assert_eq!(shell_escape("foo bar"), "'foo bar'");
        assert_eq!(shell_escape("hi $USER"), "'hi $USER'");
    }

    #[test]
    fn shell_escape_handles_single_quotes() {
        assert_eq!(shell_escape("it's"), r#"'it'\''s'"#);
    }

    #[test]
    fn safe_session_id_validation() {
        assert!(assert_safe_session_id("abc-123_x").is_ok());
        assert!(assert_safe_session_id("../bad").is_err());
        assert!(assert_safe_session_id("foo bar").is_err());
        assert!(assert_safe_session_id("").is_err());
    }
}
