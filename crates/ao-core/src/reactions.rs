//! Reaction engine types — Slice 2 Phase A (data only).
//!
//! This module defines the configuration shape the reaction engine will
//! consume. The engine itself (`ReactionEngine`, `ReactionTracker`, dispatch
//! logic) lands in Phase D; keeping Phase A to pure data types means the
//! types can be stabilized and reviewed before we wire them into
//! `LifecycleManager`.
//!
//! Mirrors `ReactionConfig`, `ReactionResult`, and `EventPriority` from
//! `packages/core/src/types.ts` (lines ~900–995 in the reference).
//!
//! ## Design choices worth calling out
//!
//! - **Kebab-case `action` and `priority`.** TS uses `"send-to-agent"`,
//!   `"auto-merge"`, `"urgent"`, `"warning"` as string literals. We match
//!   them in YAML so a user can drop a TS reaction config into our config
//!   file unmodified. Session status yaml still uses snake_case because
//!   that's a different file owned by a different subsystem.
//!
//! - **`EscalateAfter` is an untagged enum.** TS's `number | string` union
//!   becomes `Attempts(u32) | Duration(String)` with `#[serde(untagged)]`,
//!   so YAML can write either `escalate-after: 3` or `escalate-after: 10m`
//!   with no wrapper key.
//!
//! - **Durations stay as `String` in Phase A.** We don't parse `"10m"` →
//!   `Duration` here because the parser belongs next to the code that *uses*
//!   the duration (the engine, Phase D). Leaving them as strings keeps Phase
//!   A deserialization trivial and defers the "what units do we accept"
//!   question to when we have a concrete use site.

use crate::scm::MergeMethod;
use serde::{Deserialize, Serialize};

/// What a reaction should actually do when it fires. Matches the TS
/// union `"send-to-agent" | "notify" | "auto-merge"` — kebab-case on the
/// wire so TS config files round-trip unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReactionAction {
    /// Send a message to the agent, asking it to fix whatever broke.
    /// Uses `ReactionConfig::message` as the payload.
    SendToAgent,
    /// Fire a notification at a human (stdout, Slack, desktop, …).
    Notify,
    /// Merge the PR. Only makes sense for `approved-and-green`.
    AutoMerge,
}

impl ReactionAction {
    /// Kebab-case label matching the YAML wire form — used by CLI
    /// output (`ao-rs watch`) so log rows stay consistent with config
    /// file keys. Derived `Debug` would give PascalCase, which reads
    /// weirdly next to `ci_failed`/`status_changed` in the same row.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SendToAgent => "send-to-agent",
            Self::Notify => "notify",
            Self::AutoMerge => "auto-merge",
        }
    }
}

impl std::fmt::Display for ReactionAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Notification priority. Matches TS's four-value union verbatim so a
/// TS `notificationRouting` table could be ported later without a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventPriority {
    /// "Wake someone up." Paged/SMS-class.
    Urgent,
    /// "Needs human action soon." Default for send-to-agent failures.
    Action,
    /// "Something's off, check when you can." Default for stuck/conflict.
    Warning,
    /// "FYI." Default for `approved-and-green` notifications.
    Info,
}

impl EventPriority {
    /// Snake-case label matching the YAML wire form — used by the
    /// notifier registry (Slice 3 Phase A) for tracing fields and
    /// warn-once dedup keys so log rows stay consistent with config
    /// file keys. Mirror of `ReactionAction::as_str` a few lines up;
    /// derived `Debug` would give PascalCase, which reads weirdly
    /// next to `ci_failed` / `status_changed` in the same row.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Urgent => "urgent",
            Self::Action => "action",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// How long/how many attempts before a reaction escalates from
/// `SendToAgent` → `Notify`. Untagged so YAML can use a bare number *or*
/// a bare duration string:
///
/// ```yaml
/// ci-failed:
///   escalate-after: 3       # after 3 failed send attempts
/// agent-stuck:
///   escalate-after: 10m     # after 10 minutes of no progress
/// ```
///
/// Serde resolves the variants in order at parse time — `Attempts` is
/// listed first, so a bare YAML number always goes there. Anything else
/// falls through to `Duration`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EscalateAfter {
    /// Retry `send-to-agent` this many times, then escalate to `notify`.
    Attempts(u32),
    /// Wait this long after the first attempt before escalating. String
    /// form matching the TS regex `^\d+(s|m|h)$` — e.g. `"30s"`,
    /// `"10m"`, `"2h"`. Compound or fractional forms (`"1h30m"`,
    /// `"1.5m"`) are rejected. Parsed lazily by `parse_duration` on
    /// each dispatch so a misconfigured value only logs once and does
    /// not poison the engine.
    Duration(String),
}

/// A single reaction rule, typically read from `~/.ao-rs/config.yaml`
/// under `reactions.<key>`. See `docs/reactions.md` for the full list of
/// reaction keys and the matrix of which actions make sense for each.
///
/// All fields except `action` have sensible defaults, so the minimal
/// valid config is one line:
///
/// ```yaml
/// approved-and-green:
///   action: notify
/// ```
///
/// Everything else — retries, escalation, priority — falls back to a
/// value the engine considers "reasonable for hobby use".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionConfig {
    /// Master on/off. `false` means the engine sees the reaction key but
    /// does nothing; useful for disabling individual rules without
    /// deleting them. Defaults to `true` so newly-added rules are live.
    ///
    /// We skip serializing when `true` so a round-tripped config stays
    /// terse: the common case (enabled) doesn't clutter the output. Pair
    /// with `include_summary` below — both default-valued fields omit on
    /// write so a user who hand-edited a minimal config reads back a
    /// minimal config.
    #[serde(default = "default_auto", skip_serializing_if = "is_true")]
    pub auto: bool,

    /// What to do when the reaction fires. No default — you have to pick.
    pub action: ReactionAction,

    /// Body for `SendToAgent`, override text for `Notify`. Ignored by
    /// `AutoMerge`. Missing for `SendToAgent` falls back to an
    /// engine-supplied boilerplate (Phase D).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Priority for the resulting notification. Defaults to the
    /// reaction-key-specific default the engine picks (see
    /// `docs/reactions.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<EventPriority>,

    /// Max attempts of `SendToAgent` before escalating to `Notify`.
    /// `None` means "retry forever", matching TS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,

    /// Escalate after N attempts or after a wall-clock duration,
    /// whichever the user configured. Absent means "use `retries` as
    /// the only gate".
    #[serde(
        default,
        rename = "escalate_after",
        alias = "escalate-after",
        skip_serializing_if = "Option::is_none"
    )]
    pub escalate_after: Option<EscalateAfter>,

    /// Duration threshold for time-based triggers (e.g. `"10m"` for
    /// `agent-stuck`). Kept as an opaque string until Phase D adds a
    /// parser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<String>,

    /// Whether to attach a context summary to the notification.
    /// Defaults to `false` because the engine doesn't yet know how to
    /// produce one; Phase D might flip the default.
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_summary: bool,

    /// Merge method to use when `action: auto-merge`. If unset, the SCM
    /// plugin's default is used.
    #[serde(
        default,
        rename = "merge_method",
        alias = "merge-method",
        skip_serializing_if = "Option::is_none"
    )]
    pub merge_method: Option<MergeMethod>,
}

impl ReactionConfig {
    /// Convenience constructor for tests and Phase D wiring. Mirrors the
    /// minimum useful config (`auto: true`, action set, everything else
    /// default).
    pub fn new(action: ReactionAction) -> Self {
        Self {
            auto: true,
            action,
            message: None,
            priority: None,
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
            merge_method: None,
        }
    }
}

/// Outcome of a single reaction dispatch. Kept in Phase A so the engine
/// in Phase D has a stable return shape to target. Mirrors
/// `ReactionResult` in the TS reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionOutcome {
    /// Reaction key that fired (e.g. `"ci-failed"`).
    pub reaction_type: String,
    /// Did the configured action succeed? `false` means it either
    /// errored or was a no-op because `auto: false`.
    pub success: bool,
    /// Action that was *actually* taken — may differ from the configured
    /// action if the engine escalated mid-flight (e.g. `SendToAgent` →
    /// `Notify` after `retries` were exhausted).
    pub action: ReactionAction,
    /// Message delivered, if any. Useful for tests and for CLI echoing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// `true` if the engine decided to escalate rather than retry.
    pub escalated: bool,
}

fn default_auto() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaction_action_uses_kebab_case() {
        assert_eq!(
            serde_yaml::to_string(&ReactionAction::SendToAgent)
                .unwrap()
                .trim(),
            "send-to-agent"
        );
        assert_eq!(
            serde_yaml::to_string(&ReactionAction::AutoMerge)
                .unwrap()
                .trim(),
            "auto-merge"
        );

        let parsed: ReactionAction = serde_yaml::from_str("notify").unwrap();
        assert_eq!(parsed, ReactionAction::Notify);
    }

    #[test]
    fn event_priority_uses_snake_case() {
        let yaml = serde_yaml::to_string(&EventPriority::Urgent).unwrap();
        assert_eq!(yaml.trim(), "urgent");

        let parsed: EventPriority = serde_yaml::from_str("warning").unwrap();
        assert_eq!(parsed, EventPriority::Warning);
    }

    #[test]
    fn escalate_after_number_parses_as_attempts() {
        let parsed: EscalateAfter = serde_yaml::from_str("3").unwrap();
        assert_eq!(parsed, EscalateAfter::Attempts(3));
    }

    #[test]
    fn escalate_after_string_parses_as_duration() {
        let parsed: EscalateAfter = serde_yaml::from_str("10m").unwrap();
        assert_eq!(parsed, EscalateAfter::Duration("10m".into()));
    }

    #[test]
    fn escalate_after_attempts_roundtrips() {
        let e = EscalateAfter::Attempts(5);
        let yaml = serde_yaml::to_string(&e).unwrap();
        // Untagged enum means the raw number, no wrapper key.
        assert_eq!(yaml.trim(), "5");
        let back: EscalateAfter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn reaction_config_minimal_config_deserializes() {
        // Only `action` is required; everything else defaults.
        let yaml = "action: notify\n";
        let cfg: ReactionConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.action, ReactionAction::Notify);
        assert!(cfg.auto); // default_auto
        assert_eq!(cfg.retries, None);
        assert!(!cfg.include_summary);
    }

    #[test]
    fn reaction_config_full_config_roundtrips() {
        let yaml = r#"
auto: true
action: send-to-agent
message: "CI broke — logs attached, please fix."
priority: action
retries: 3
escalate_after: 3
threshold: 5m
include_summary: true
"#;
        let cfg: ReactionConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.action, ReactionAction::SendToAgent);
        assert_eq!(cfg.priority, Some(EventPriority::Action));
        assert_eq!(cfg.retries, Some(3));
        assert_eq!(cfg.escalate_after, Some(EscalateAfter::Attempts(3)));
        assert_eq!(cfg.threshold.as_deref(), Some("5m"));
        assert!(cfg.include_summary);

        // Re-serialize and re-parse — fields survive a round trip.
        let back: ReactionConfig =
            serde_yaml::from_str(&serde_yaml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn reaction_config_accepts_hyphenated_escalate_after_key() {
        // Config files in the wild will write `escalate-after:` more
        // often than `escalate_after:`. Serde `alias` makes both work,
        // but the canonical write-back form uses the underscore rename.
        let yaml = "action: notify\nescalate-after: 10m\n";
        let cfg: ReactionConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            cfg.escalate_after,
            Some(EscalateAfter::Duration("10m".into()))
        );
    }

    #[test]
    fn reaction_config_canonicalizes_escalate_after_on_write() {
        // The alias → rename contract: we accept `escalate-after:` on
        // read but always emit `escalate_after:` on write. This nails
        // it explicitly — without this test a stray `#[serde(alias)]`
        // change that flipped which form is canonical would go unnoticed.
        let yaml_in = "action: notify\nescalate-after: 10m\n";
        let cfg: ReactionConfig = serde_yaml::from_str(yaml_in).unwrap();
        let yaml_out = serde_yaml::to_string(&cfg).unwrap();
        assert!(
            yaml_out.contains("escalate_after:"),
            "expected canonical snake_case key in output, got:\n{yaml_out}"
        );
        assert!(
            !yaml_out.contains("escalate-after:"),
            "expected no kebab-case key in output, got:\n{yaml_out}"
        );
    }

    #[test]
    fn reaction_config_auto_true_is_omitted_on_write() {
        // Default-valued fields (`auto: true`, `include_summary: false`)
        // are elided on write so a minimal config round-trips to a
        // minimal config. Guard against a future field being added
        // without matching `skip_serializing_if` and silently bloating
        // every config write.
        let cfg = ReactionConfig::new(ReactionAction::Notify);
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert!(
            !yaml.contains("auto:"),
            "auto:true should be omitted, got:\n{yaml}"
        );
        assert!(
            !yaml.contains("include_summary"),
            "include_summary:false should be omitted, got:\n{yaml}"
        );
        // But `auto: false` must still serialize (it's a deliberate disable).
        let mut off = cfg;
        off.auto = false;
        let yaml = serde_yaml::to_string(&off).unwrap();
        assert!(
            yaml.contains("auto: false"),
            "auto:false must survive, got:\n{yaml}"
        );
    }

    #[test]
    fn escalate_after_duration_preserves_whitespace_verbatim() {
        // Phase D's duration parser will need to handle (or reject)
        // strings like "3 " with trailing whitespace. This test locks
        // in that Phase A's deserializer does NOT pre-trim — so the
        // parser later has a clear contract.
        let parsed: EscalateAfter = serde_yaml::from_str(r#""3 ""#).unwrap();
        assert_eq!(parsed, EscalateAfter::Duration("3 ".into()));
    }

    #[test]
    fn reaction_config_new_is_minimal() {
        let c = ReactionConfig::new(ReactionAction::AutoMerge);
        assert!(c.auto);
        assert_eq!(c.action, ReactionAction::AutoMerge);
        assert!(c.message.is_none());
        assert!(c.retries.is_none());
    }

    #[test]
    fn reaction_outcome_escalated_roundtrips() {
        let o = ReactionOutcome {
            reaction_type: "ci-failed".into(),
            success: true,
            action: ReactionAction::Notify,
            message: Some("escalated after 3 attempts".into()),
            escalated: true,
        };
        let back: ReactionOutcome =
            serde_yaml::from_str(&serde_yaml::to_string(&o).unwrap()).unwrap();
        assert_eq!(o, back);
    }
}
