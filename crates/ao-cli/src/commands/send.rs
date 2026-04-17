//! `ao-rs send` — message to a running session.

use std::path::PathBuf;
use std::time::Duration;

use ao_core::SessionManager;
use tokio::time::timeout;

use crate::cli::plugins::select_runtime;
use crate::cli::printing::short_id;

/// Assemble the final message string from optional inline words and an optional file.
fn assemble_message(parts: &[String], file: Option<&PathBuf>) -> Result<String, String> {
    let inline = parts.join(" ");
    let file_content = match file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read file {}: {e}", path.display()))?,
        ),
        None => None,
    };

    let message = match (inline.is_empty(), file_content) {
        (true, Some(content)) => content,
        (false, Some(content)) => format!("{inline}\n{content}"),
        (false, None) => inline,
        (true, None) => return Err("no message to send (provide message words or --file)".into()),
    };

    Ok(message)
}

pub async fn send(
    session_id_or_prefix: String,
    message_parts: Vec<String>,
    file: Option<PathBuf>,
    _no_wait: bool,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let message = assemble_message(&message_parts, file.as_ref())?;

    let sessions = SessionManager::with_default();
    let session = sessions.find_by_prefix(&session_id_or_prefix).await?;

    let handle = session.runtime_handle.as_deref().ok_or_else(|| {
        format!(
            "session {} has no runtime handle (status={}); nothing to send to",
            session.id,
            session.status.as_str()
        )
    })?;

    // Probe before send so a common failure mode — "the session crashed" —
    // produces an actionable error instead of a tmux stderr dump. Surface
    // probe-itself errors (tmux binary missing, spawn EMFILE, ...) directly
    // rather than collapsing them to "dead": restoring into the same broken
    // tmux would just fail again with less context.
    let runtime = select_runtime(&session.runtime);
    let alive = runtime
        .is_alive(handle)
        .await
        .map_err(|e| format!("failed to probe runtime {handle}: {e}"))?;
    if !alive {
        return Err(format!(
            "runtime handle {handle} is not alive. \
             try: ao-rs session restore {}",
            short_id(&session.id)
        )
        .into());
    }

    timeout(
        Duration::from_secs(timeout_secs),
        runtime.send_message(handle, &message),
    )
    .await
    .map_err(|_| format!("send timed out after {timeout_secs}s"))??;

    println!("→ sent {} bytes to {handle}", message.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_words_joined_with_space() {
        let parts = vec!["hello".into(), "world".into()];
        let msg = assemble_message(&parts, None).unwrap();
        assert_eq!(msg, "hello world");
    }

    #[test]
    fn file_content_used_when_no_inline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("msg.txt");
        std::fs::write(&path, "file body\n").unwrap();
        let msg = assemble_message(&[], Some(&path)).unwrap();
        assert_eq!(msg, "file body\n");
    }

    #[test]
    fn inline_and_file_combined() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("msg.txt");
        std::fs::write(&path, "from file").unwrap();
        let parts = vec!["prefix:".into()];
        let msg = assemble_message(&parts, Some(&path)).unwrap();
        assert_eq!(msg, "prefix:\nfrom file");
    }

    #[test]
    fn missing_file_returns_error() {
        let path = PathBuf::from("/tmp/ao-rs-nonexistent-file-xyz.txt");
        let err = assemble_message(&[], Some(&path)).unwrap_err();
        assert!(err.contains("cannot read file"), "got: {err}");
    }

    #[test]
    fn no_message_no_file_returns_error() {
        let err = assemble_message(&[], None).unwrap_err();
        assert!(err.contains("no message to send"), "got: {err}");
    }
}
