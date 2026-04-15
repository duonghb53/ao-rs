//! `ao-rs session attach`.

use ao_core::SessionManager;

use crate::cli::printing::short_id;

pub async fn attach(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let session = sessions.find_by_prefix(&session_id_or_prefix).await?;

    let handle = session.runtime_handle.as_deref().ok_or_else(|| {
        format!(
            "session {} has no runtime handle (status={})",
            short_id(&session.id),
            session.status.as_str()
        )
    })?;

    // exec() replaces the current process image — user is dropped straight
    // into tmux. If it returns at all, the exec failed.
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new("tmux")
        .args(["attach-session", "-t", handle])
        .exec();
    Err(format!("failed to exec tmux: {err}").into())
}
