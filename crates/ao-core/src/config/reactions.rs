//! Reaction-related config helpers: default reactions, default routing,
//! and the reaction-key / notifier-name allow-lists used for validation.

use crate::{
    notifier::NotificationRouting,
    reactions::{EscalateAfter, EventPriority, ReactionAction, ReactionConfig},
};
use std::collections::HashMap;

pub(super) fn supported_reaction_keys() -> [&'static str; 9] {
    [
        "ci-failed",
        "changes-requested",
        "merge-conflicts",
        "approved-and-green",
        "agent-idle",
        "agent-stuck",
        "agent-needs-input",
        "agent-exited",
        "all-complete",
    ]
}

pub(super) fn supported_notifier_names() -> [&'static str; 5] {
    // These are the notifier plugin names ao-cli may register.
    // Some are conditional on env vars (ntfy/discord/slack), but the *names*
    // are still supported; validation should catch typos.
    ["stdout", "desktop", "ntfy", "discord", "slack"]
}

/// Returns the nine default reactions matching the TS agent-orchestrator.
///
/// `priority` is left unset so dispatch uses
/// [`reactions::default_priority_for_reaction_key`](crate::reactions::default_priority_for_reaction_key)
/// — configured `priority:` in YAML always overrides.
pub fn default_reactions() -> HashMap<String, ReactionConfig> {
    let mut m = HashMap::new();
    m.insert(
        "ci-failed".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some(
                "CI is failing on your PR. Run `gh pr checks` to see the failures, fix them, and push.".into(),
            ),
            priority: None,
            retries: Some(2),
            escalate_after: Some(EscalateAfter::Attempts(2)),
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "changes-requested".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some(
                "There are review comments on your PR. Check with `gh pr view --comments`, address them, and push."
                    .into(),
            ),
            priority: None,
            retries: None,
            escalate_after: Some(EscalateAfter::Duration("30m".into())),
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "merge-conflicts".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some(
                "Your branch has merge conflicts. Rebase on the default branch and resolve them."
                    .into(),
            ),
            priority: None,
            retries: None,
            escalate_after: Some(EscalateAfter::Duration("15m".into())),
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "approved-and-green".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::AutoMerge,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "agent-idle".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some(
                "You appear to be idle. If your task is not complete, continue working or explain blockers."
                    .into(),
            ),
            priority: None,
            retries: Some(2),
            escalate_after: Some(EscalateAfter::Duration("15m".into())),
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "agent-stuck".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: Some("10m".into()),
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "agent-needs-input".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "agent-exited".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
            merge_method: None,
        },
    );
    m.insert(
        "all-complete".into(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: true,
            merge_method: None,
        },
    );
    m
}

/// Returns default notification routing: all priorities → stdout.
pub fn default_routing() -> NotificationRouting {
    let mut m = HashMap::new();
    for &p in &[
        EventPriority::Urgent,
        EventPriority::Action,
        EventPriority::Warning,
        EventPriority::Info,
    ] {
        m.insert(p, vec!["stdout".to_string()]);
    }
    NotificationRouting::from_map(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_reactions_has_nine_keys() {
        use crate::reactions::default_priority_for_reaction_key;

        let reactions = default_reactions();
        assert_eq!(reactions.len(), 9);
        assert!(reactions.contains_key("ci-failed"));
        assert!(reactions.contains_key("changes-requested"));
        assert!(reactions.contains_key("merge-conflicts"));
        assert!(reactions.contains_key("approved-and-green"));
        assert!(reactions.contains_key("agent-idle"));
        assert!(reactions.contains_key("agent-stuck"));
        assert!(reactions.contains_key("agent-needs-input"));
        assert!(reactions.contains_key("agent-exited"));
        assert!(reactions.contains_key("all-complete"));

        for (key, rc) in &reactions {
            assert!(
                rc.priority.is_none(),
                "{key}: omit priority so default_priority_for_reaction_key applies"
            );
            let _ = default_priority_for_reaction_key(key);
        }
    }

    #[test]
    fn default_routing_covers_all_priorities() {
        let routing = default_routing();
        assert_eq!(routing.len(), 4);
        assert!(routing.names_for(EventPriority::Urgent).is_some());
        assert!(routing.names_for(EventPriority::Action).is_some());
        assert!(routing.names_for(EventPriority::Warning).is_some());
        assert!(routing.names_for(EventPriority::Info).is_some());
    }
}
