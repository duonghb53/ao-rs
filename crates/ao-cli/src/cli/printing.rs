//! Config warnings, session display helpers, and watch event printing.

use ao_core::{ConfigWarning, OrchestratorEvent, Session, SessionId};

pub(crate) fn print_config_warnings(config_path: &std::path::Path, warnings: &[ConfigWarning]) {
    if warnings.is_empty() {
        return;
    }
    eprintln!(
        "warning: {}: {} unsupported field(s) found (they will be ignored):",
        config_path.display(),
        warnings.len()
    );
    for w in warnings {
        if w.field.is_empty() {
            eprintln!("  - {}", w.message);
        } else {
            eprintln!("  - {}: {}", w.field, w.message);
        }
    }
}

pub(crate) fn short_id(id: &SessionId) -> String {
    id.0.chars().take(8).collect()
}

pub(crate) fn session_display_title(s: &Session) -> String {
    if let Some(issue_id) = s.issue_id.as_deref() {
        return format!("#{issue_id} {}", s.task);
    }
    s.task.clone()
}

/// Truncate a string to at most `max` characters, appending `…` if cut.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Pretty-print one `OrchestratorEvent` as a single table row.
pub(crate) fn print_event(event: &OrchestratorEvent) {
    let short = |id: &SessionId| -> String { id.0.chars().take(8).collect() };
    match event {
        OrchestratorEvent::Spawned { id, project_id } => {
            println!("{:<10} {:<20} project={project_id}", short(id), "spawned");
        }
        OrchestratorEvent::SessionRestored {
            id,
            project_id,
            status,
        } => {
            println!(
                "{:<10} {:<20} project={project_id} status={}",
                short(id),
                "session_restored",
                status.as_str()
            );
        }
        OrchestratorEvent::StatusChanged { id, from, to } => {
            println!(
                "{:<10} {:<20} {} → {}",
                short(id),
                "status_changed",
                from.as_str(),
                to.as_str()
            );
        }
        OrchestratorEvent::ActivityChanged { id, prev, next } => {
            let prev = prev.map(|a| a.as_str()).unwrap_or("-");
            println!(
                "{:<10} {:<20} {prev} → {}",
                short(id),
                "activity_changed",
                next.as_str()
            );
        }
        OrchestratorEvent::Terminated { id, reason } => {
            println!("{:<10} {:<20} {reason}", short(id), "terminated");
        }
        OrchestratorEvent::TickError { id, message } => {
            println!("{:<10} {:<20} {message}", short(id), "tick_error");
        }
        OrchestratorEvent::ReactionTriggered {
            id,
            reaction_key,
            action,
        } => {
            // Reaction events — Slice 2 Phase D. One line each, mirroring
            // the existing row shape so `ao-rs watch` stays grep-friendly.
            println!(
                "{:<10} {:<20} {reaction_key} → {action}",
                short(id),
                "reaction_fired"
            );
        }
        OrchestratorEvent::ReactionEscalated {
            id,
            reaction_key,
            attempts,
        } => {
            println!(
                "{:<10} {:<20} {reaction_key} ({attempts} attempts)",
                short(id),
                "reaction_escalated"
            );
        }
        OrchestratorEvent::UiNotification { notification } => {
            // UI-first event — keep `watch` output compact.
            let msg = notification.message.as_deref().unwrap_or("-");
            let prio = notification.priority.as_deref().unwrap_or("-");
            println!(
                "{:<10} {:<20} {} ({}) {msg}",
                short(&notification.id),
                "ui_notification",
                notification.reaction_key,
                prio
            );
        }
    }
}
