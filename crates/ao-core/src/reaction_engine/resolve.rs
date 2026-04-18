//! Config resolution helpers: `status_to_reaction_key`, `merge_reaction_config`,
//! `resolve_priority`, and `build_payload`.

use crate::{
    notifier::NotificationPayload,
    reactions::{default_priority_for_reaction_key, EventPriority, ReactionAction, ReactionConfig},
    types::{Session, SessionStatus},
};

/// Map a `SessionStatus` to the reaction key that should fire on entry.
///
/// Returns `None` for statuses that don't map to a reaction today.
/// Wired reactions: `changes-requested`, `approved-and-green`, `agent-stuck`,
/// `agent-needs-input`, `agent-exited` (issue #195 M1 parity fix).
///
/// **`CiFailed` is intentionally absent.** The lifecycle manager dispatches
/// `ci-failed` directly via `check_ci_failed` so that failed check names and
/// URLs can be included in the message body. Routing through this function
/// would fire a duplicate, message-less dispatch on the same tracker entry.
///
/// Public so `LifecycleManager` can peek at the mapping without having
/// to duplicate it — both on entry (what reaction to fire) and on exit
/// (which tracker to clear via `clear_tracker_on_transition`).
///
/// Phase H note: `Stuck` is the first status whose entry is driven by
/// an auxiliary in-memory clock (`LifecycleManager::idle_since`) rather
/// than by `derive_scm_status`'s pure state-machine ladder. The mapping
/// is still a straightforward one-liner here; the "when does Stuck
/// become reachable" logic lives in `LifecycleManager::check_stuck`.
pub const fn status_to_reaction_key(status: SessionStatus) -> Option<&'static str> {
    match status {
        SessionStatus::ChangesRequested => Some("changes-requested"),
        SessionStatus::Mergeable => Some("approved-and-green"),
        SessionStatus::Approved => Some("approved-and-green"),
        SessionStatus::Stuck => Some("agent-stuck"),
        SessionStatus::NeedsInput => Some("agent-needs-input"),
        SessionStatus::Killed => Some("agent-exited"),
        _ => None,
    }
}

/// Merge a global reaction with a project override. Boolean fields always
/// come from `project`; [`Option`] fields only override when the project
/// sets `Some(_)`, preserving the global value when the project leaves
/// `None`.
pub(super) fn merge_reaction_config(
    global: ReactionConfig,
    project: ReactionConfig,
) -> ReactionConfig {
    let mut out = global;
    out.auto = project.auto;
    out.action = project.action;
    if project.message.is_some() {
        out.message = project.message;
    }
    if project.priority.is_some() {
        out.priority = project.priority;
    }
    if project.retries.is_some() {
        out.retries = project.retries;
    }
    if project.escalate_after.is_some() {
        out.escalate_after = project.escalate_after;
    }
    if project.threshold.is_some() {
        out.threshold = project.threshold;
    }
    out.include_summary = project.include_summary;
    if project.merge_method.is_some() {
        out.merge_method = project.merge_method;
    }
    out
}

/// Pick the `EventPriority` for a notification. Configured `priority:`
/// wins; otherwise [`default_priority_for_reaction_key`].
pub(super) fn resolve_priority(reaction_key: &str, cfg: &ReactionConfig) -> EventPriority {
    cfg.priority
        .unwrap_or_else(|| default_priority_for_reaction_key(reaction_key))
}

/// Construct a `NotificationPayload` from the reaction context.
pub(super) fn build_payload(
    session: &Session,
    reaction_key: &str,
    cfg: &ReactionConfig,
    priority: EventPriority,
    escalated: bool,
) -> NotificationPayload {
    let title = if escalated {
        format!("[escalated] {} on {}", reaction_key, session.id)
    } else {
        format!("{} on {}", reaction_key, session.id)
    };
    let body = cfg.message.clone().unwrap_or_else(|| {
        if escalated {
            format!(
                "{} escalated to notify after retries exhausted",
                reaction_key
            )
        } else {
            format!("Reaction {} fired for session {}", reaction_key, session.id)
        }
    });
    NotificationPayload {
        session_id: session.id.clone(),
        reaction_key: reaction_key.to_string(),
        action: ReactionAction::Notify,
        priority,
        title,
        body,
        escalated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactions::ReactionAction;
    use crate::types::SessionStatus;

    #[test]
    fn status_map_covers_reactions_through_phase_h() {
        // CiFailed is absent from this map (dispatched via check_ci_failed).
        assert_eq!(
            status_to_reaction_key(SessionStatus::CiFailed),
            None,
            "CiFailed dispatches via check_ci_failed, not status_to_reaction_key"
        );
        assert_eq!(
            status_to_reaction_key(SessionStatus::ChangesRequested),
            Some("changes-requested")
        );
        assert_eq!(
            status_to_reaction_key(SessionStatus::Mergeable),
            Some("approved-and-green")
        );
        assert_eq!(
            status_to_reaction_key(SessionStatus::Stuck),
            Some("agent-stuck")
        );
        // Issue #195 M1: previously missing mappings now wired.
        assert_eq!(
            status_to_reaction_key(SessionStatus::NeedsInput),
            Some("agent-needs-input"),
            "NeedsInput must map to agent-needs-input"
        );
        assert_eq!(
            status_to_reaction_key(SessionStatus::Killed),
            Some("agent-exited"),
            "Killed must map to agent-exited"
        );
        assert_eq!(
            status_to_reaction_key(SessionStatus::Approved),
            Some("approved-and-green"),
            "Approved must map to approved-and-green"
        );
        // Statuses that deliberately don't map.
        assert_eq!(status_to_reaction_key(SessionStatus::Working), None);
        assert_eq!(status_to_reaction_key(SessionStatus::MergeFailed), None);
        assert_eq!(status_to_reaction_key(SessionStatus::Errored), None);
    }

    #[test]
    fn resolve_priority_uses_config_override() {
        let mut cfg = ReactionConfig::new(ReactionAction::Notify);
        cfg.priority = Some(EventPriority::Urgent);
        assert_eq!(resolve_priority("ci-failed", &cfg), EventPriority::Urgent);
    }

    #[test]
    fn resolve_priority_falls_back_to_defaults() {
        let cfg = ReactionConfig::new(ReactionAction::Notify);
        assert_eq!(resolve_priority("ci-failed", &cfg), EventPriority::Warning);
        assert_eq!(
            resolve_priority("changes-requested", &cfg),
            EventPriority::Info
        );
        assert_eq!(
            resolve_priority("merge-conflicts", &cfg),
            EventPriority::Warning
        );
        assert_eq!(
            resolve_priority("approved-and-green", &cfg),
            EventPriority::Action
        );
        assert_eq!(resolve_priority("agent-idle", &cfg), EventPriority::Info);
        assert_eq!(resolve_priority("agent-stuck", &cfg), EventPriority::Urgent);
        assert_eq!(
            resolve_priority("agent-needs-input", &cfg),
            EventPriority::Urgent
        );
        assert_eq!(
            resolve_priority("agent-exited", &cfg),
            EventPriority::Urgent
        );
        assert_eq!(resolve_priority("all-complete", &cfg), EventPriority::Info);
        assert_eq!(
            resolve_priority("unknown-reaction", &cfg),
            EventPriority::Warning
        );
    }
}
