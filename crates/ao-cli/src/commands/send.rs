//! `ao-rs send` — message to a running session.

use ao_core::{SessionManager};

use crate::cli::plugins::select_runtime;
use crate::cli::printing::short_id;

pub async fn send(
    session_id_or_prefix: String,
    message: String,
) -> Result<(), Box<dyn std::error::Error>> {
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

    runtime.send_message(handle, &message).await?;
    println!("→ sent {} bytes to {handle}", message.len());
    Ok(())
}
