//! `ao-rs session restore`.

use ao_core::{restore_session, SessionManager};

use crate::cli::agent_config::resolve_agent_config_for_restore;
use crate::cli::plugins::select_agent;
use crate::cli::plugins::select_runtime;

pub async fn restore(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    // Resolve the session first so we can reconstruct the correct agent plugin
    // (and its captured config) for the restore call.
    let mut session = sessions.find_by_prefix(&session_id_or_prefix).await?;
    let before = session.agent_config.clone();
    resolve_agent_config_for_restore(&mut session);
    if session.agent_config != before {
        sessions.save(&session).await?;
    }
    let runtime = select_runtime(&session.runtime);
    let agent_box = select_agent(&session.agent, session.agent_config.as_ref());

    println!("→ restoring session: {session_id_or_prefix}");
    let outcome = restore_session(
        &session_id_or_prefix,
        &sessions,
        &*runtime,
        agent_box.as_ref(),
    )
    .await?;

    let short: String = outcome.session.id.0.chars().take(8).collect();
    println!();
    println!("───────────────────────────────────────────────");
    println!("  ✓ session restored");
    println!();
    println!("  session: {} (short {short})", outcome.session.id);
    println!("  status:  {}", outcome.session.status.as_str());
    println!("  handle:  {}", outcome.runtime_handle);
    println!("  launch:  {}", outcome.launch_command);
    if let Some(ws) = &outcome.session.workspace_path {
        println!("  worktree: {}", ws.display());
    }
    println!();
    println!("  attach:  tmux attach -t {}", outcome.runtime_handle);
    println!("  status:  ao-rs status");
    println!("───────────────────────────────────────────────");

    Ok(())
}
