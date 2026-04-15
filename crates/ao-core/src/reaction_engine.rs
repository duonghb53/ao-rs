//! Slice 2 Phase D — Reaction dispatch.
//!
//! The reaction engine sits between `LifecycleManager` and the side-effect
//! plugins (`Runtime::send_message`, `Scm::merge`). When the lifecycle loop
//! observes a status transition into a "trigger state" like `CiFailed`, it
//! asks the engine to fire the corresponding reaction. The engine looks the
//! reaction up in `AoConfig::reactions`, tracks attempts, and runs the
//! configured action — or escalates to `Notify` when retries are exhausted.
//!
//! Mirrors `executeReaction` / `ReactionTracker` from
//! `packages/core/src/lifecycle-manager.ts` (lines ~570-710 in the reference).
//!
//! ## Why the engine is separate from LifecycleManager
//!
//! TS bundled everything into one big `createLifecycleManager` closure. In
//! Rust we split them so:
//!
//! 1. Tests can exercise the engine without a polling loop — unit tests
//!    build a `ReactionEngine` directly, call `dispatch`, and assert events.
//! 2. Future CLI commands (`ao-rs react fire ci-failed <id>`) can reuse
//!    the engine directly without going through lifecycle ticks.
//! 3. The lifecycle loop stays a thin state machine; all "business logic"
//!    about what an action *means* lives here.
//!
//! ## Tracker semantics
//!
//! The tracker is keyed on `(SessionId, reaction_key)`. One tracker per
//! (session, reaction) pair, regardless of how many times the same status
//! transition fires. A tracker is:
//!
//! - **Incremented** on every dispatch attempt (including the one that
//!   ultimately escalates).
//! - **Cleared** by `LifecycleManager` via `clear_tracker` when a session
//!   *leaves* the triggering status — so a new CI failure after a fix
//!   doesn't inherit the old failure's retry budget. This matches the TS
//!   `clearReactionTracker` calls on transition reset.
//!
//! ## Phase F additions
//!
//! - `with_scm` attaches an `Arc<dyn Scm>` so `dispatch_auto_merge` can
//!   actually call `Scm::merge`. Before merging the engine re-probes
//!   `detect_pr` + `mergeability` — a stale-green observation (the PR
//!   was ready when the lifecycle tick saw it, but CI just flipped red)
//!   aborts without merging, and the next tick can retry.
//!
//! ## Phase H additions
//!
//! - Duration-based `escalate_after` is now honoured. `TrackerState`
//!   gained a `first_triggered_at: Instant` set on first dispatch; the
//!   duration gate compares `entry.first_triggered_at.elapsed()` against
//!   `parse_duration(escalate_after)` and flips `should_escalate` when
//!   over. Parsing uses the same `^\d+(s|m|h)$` contract as TS
//!   `parseDuration`, returning `None` on garbage.
//! - Garbage duration strings no longer panic and do not cause escalate
//!   to fire. They trigger a one-shot `tracing::warn!` per
//!   `(reaction_key, field)` pair via `warned_parse_failures` — a
//!   process-local `HashSet` that bounds log noise to a single warn per
//!   misconfigured field.
//!
//! ## Phase B additions (Slice 3)
//!
//! - `with_notifier_registry` attaches a `NotifierRegistry` so
//!   `dispatch_notify` can fan out to real plugins. Without a registry
//!   the engine falls back to Phase D behaviour (emit event, return
//!   success). With a registry, each `Notify` dispatch resolves the
//!   priority against the routing table and calls `Notifier::send` on
//!   every matching plugin; failures are logged and recorded in
//!   `ReactionOutcome { success: false, .. }` but never propagate.
//! - Escalation now also routes through the registry so a retry-
//!   exhausted `SendToAgent → Notify` fallback actually reaches
//!   configured notifiers.
//! - `resolve_priority` uses configured `priority:` when set, otherwise
//!   [`default_priority_for_reaction_key`](crate::reactions::default_priority_for_reaction_key).

use crate::{
    error::Result,
    events::{OrchestratorEvent, UiNotification},
    notifier::{NotificationPayload, NotifierError, NotifierRegistry},
    reactions::{
        default_priority_for_reaction_key, EscalateAfter, EventPriority, ReactionAction,
        ReactionConfig, ReactionOutcome,
    },
    traits::{Runtime, Scm},
    types::{Session, SessionId, SessionStatus},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::broadcast;

/// Per-(session, reaction) attempt bookkeeping. Mirrors TS `ReactionTracker`.
#[derive(Debug, Clone, Copy)]
struct TrackerState {
    /// How many times this reaction has been dispatched for this session.
    /// Incremented *before* the action runs, so a dispatch that errored
    /// still counts.
    attempts: u32,
    /// Monotonic `Instant` at which this `(session, reaction_key)` pair
    /// was first observed. Populated on the `or_insert_with` path during
    /// the first dispatch and **never updated** on subsequent dispatches
    /// — that's deliberate, so duration-based escalation (`escalate_after:
    /// 10m`) measures wall-clock time since the first trigger of a given
    /// episode, not since the last attempt.
    ///
    /// Cleared-and-recreated semantics: `clear_tracker` removes the whole
    /// entry, so if a session leaves and re-enters a triggering status
    /// (e.g. `ci-failed` → `working` → `ci-failed`) the next dispatch
    /// gets a fresh `first_triggered_at`. That's correct: a second
    /// episode shouldn't inherit the first episode's elapsed clock.
    first_triggered_at: Instant,
}

/// Map a `SessionStatus` to the reaction key that should fire on entry.
///
/// Returns `None` for statuses that don't map to a reaction today. The
/// four currently-wired reactions are `ci-failed`, `changes-requested`,
/// `approved-and-green`, and `agent-stuck` (Phase H); everything else
/// returns `None` so the engine is a no-op on unrelated transitions.
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
        SessionStatus::CiFailed => Some("ci-failed"),
        SessionStatus::ChangesRequested => Some("changes-requested"),
        SessionStatus::Mergeable => Some("approved-and-green"),
        SessionStatus::Stuck => Some("agent-stuck"),
        _ => None,
    }
}

/// Parse a duration string matching the TS reference's `parseDuration`:
/// `^\d+(s|m|h)$`. Returns `None` on any other shape so callers can no-op.
///
/// This is the honest contract used by both the reaction engine's
/// `escalate_after` duration form and `LifecycleManager::check_stuck`'s
/// stuck-threshold comparison. Kept `pub(crate)` because neither caller
/// is outside `ao-core`.
///
/// Accepted: `"0s"`, `"1s"`, `"10m"`, `"24h"`, etc. Zero is allowed —
/// `threshold: "0s"` is a legitimate test fixture, matching the
/// requirements doc's "no clamping, no floor" decision.
///
/// Rejected (return `None`): compound forms like `"1m30s"`, non-digit
/// prefixes like `"fast"`, missing suffix (`"10"`), empty string, and
/// anything that would overflow `u64` seconds (`checked_mul`).
///
/// Mirrors `packages/core/src/lifecycle-manager.ts` `parseDuration`
/// which returns `0` on garbage — the Rust `None` short-circuits at
/// the callsite the same way.
pub(crate) fn parse_duration(s: &str) -> Option<Duration> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let suffix = *bytes.last()?;
    let multiplier_secs: u64 = match suffix {
        b's' => 1,
        b'm' => 60,
        b'h' => 3600,
        _ => return None,
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let total_secs = n.checked_mul(multiplier_secs)?;
    Some(Duration::from_secs(total_secs))
}

/// Pick the `EventPriority` for a notification. Configured `priority:`
/// wins; otherwise [`default_priority_for_reaction_key`].
fn resolve_priority(reaction_key: &str, cfg: &ReactionConfig) -> EventPriority {
    cfg.priority
        .unwrap_or_else(|| default_priority_for_reaction_key(reaction_key))
}

/// Construct a `NotificationPayload` from the reaction context.
fn build_payload(
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

/// The reaction dispatcher. Holds config, attempt trackers, and the
/// Runtime handle needed to actually talk to the agent process.
///
/// `Arc<ReactionEngine>` is what gets wired into `LifecycleManager`.
pub struct ReactionEngine {
    /// Reaction-key → config. Sourced from `AoConfig::reactions` at build
    /// time. Hot-reload is deferred — a config change today needs a
    /// lifecycle restart, which matches how the TS reference behaves.
    config: HashMap<String, ReactionConfig>,
    /// Runtime used for `SendToAgent`. Required because every reaction
    /// configuration today could choose `send-to-agent` as its action.
    runtime: Arc<dyn Runtime>,
    /// Shared broadcast channel — cloned from `LifecycleManager::events_tx`.
    /// The engine emits `ReactionTriggered` / `ReactionEscalated` here so
    /// subscribers see them alongside lifecycle events.
    events_tx: broadcast::Sender<OrchestratorEvent>,
    /// Per-(session, reaction) attempt state. `Mutex` (not async) because
    /// the critical sections are tiny map mutations — no awaiting.
    trackers: Mutex<HashMap<(SessionId, String), TrackerState>>,
    /// Process-local set of `"{reaction_key}.{field}"` keys that have
    /// already emitted a one-shot `tracing::warn!` for a malformed
    /// duration string (`threshold` or `escalate_after`). Subsequent
    /// parse failures for the same key are silent. Reset on
    /// `ao-rs watch` restart — same non-persistence trade-off as
    /// `trackers` and `idle_since`. Size bounded by the number of
    /// reaction keys in the user's config (≤ 10 in practice).
    warned_parse_failures: Mutex<HashSet<String>>,
    /// Optional Phase F SCM plugin. When set, `dispatch_auto_merge`
    /// actually calls `Scm::merge` (after re-verifying readiness with a
    /// fresh `mergeability` probe). When unset, `auto-merge` degrades to
    /// the Phase D behaviour: emit intent, log, return success.
    scm: Option<Arc<dyn Scm>>,
    /// Optional Slice 3 Phase B notifier registry. When set,
    /// `dispatch_notify` resolves the reaction's priority against the
    /// routing table and calls `Notifier::send` on each target plugin.
    /// When unset, `dispatch_notify` falls back to Phase D behaviour
    /// (emit event, return success). Matches the `with_scm` opt-in
    /// pattern: existing call sites that don't attach a registry keep
    /// working unchanged.
    notifier_registry: Option<NotifierRegistry>,
}

impl ReactionEngine {
    /// Build an engine from a loaded config. The caller owns the runtime
    /// and the broadcast channel — typically `LifecycleManager` hands its
    /// own `events_tx` in via `clone()` so engine events share the channel.
    pub fn new(
        config: HashMap<String, ReactionConfig>,
        runtime: Arc<dyn Runtime>,
        events_tx: broadcast::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            config,
            runtime,
            events_tx,
            trackers: Mutex::new(HashMap::new()),
            warned_parse_failures: Mutex::new(HashSet::new()),
            scm: None,
            notifier_registry: None,
        }
    }

    /// Attach an SCM plugin so `auto-merge` can actually merge.
    ///
    /// Builder form to match `LifecycleManager::with_scm`. When unset,
    /// `dispatch_auto_merge` falls back to Phase D's "log and emit intent
    /// only" behaviour so existing callers that don't know about SCM
    /// keep working.
    pub fn with_scm(mut self, scm: Arc<dyn Scm>) -> Self {
        self.scm = Some(scm);
        self
    }

    /// Attach a notifier registry so `dispatch_notify` can fan out to
    /// real notifier plugins. Without a registry the engine falls back
    /// to Phase D behaviour (emit event, return success).
    pub fn with_notifier_registry(mut self, registry: NotifierRegistry) -> Self {
        self.notifier_registry = Some(registry);
        self
    }

    /// Read-only accessor so the lifecycle layer can peek at a single
    /// reaction config without taking ownership of the whole map.
    ///
    /// Phase H uses this in `LifecycleManager::check_stuck` to read the
    /// `agent-stuck` threshold; it returns `None` when the user has not
    /// configured that reaction, which `check_stuck` treats as "stuck
    /// detection is disabled for this session" and silently skips.
    ///
    /// Deliberately `pub(crate)` — this is an internal contract with
    /// the lifecycle manager, not a public extension point.
    pub(crate) fn reaction_config(&self, reaction_key: &str) -> Option<&ReactionConfig> {
        self.config.get(reaction_key)
    }

    /// Fire the reaction configured for `reaction_key` against `session`,
    /// if any. Returns `None` when there's no matching config — the
    /// caller (usually `LifecycleManager::transition`) treats that as
    /// "silently do nothing" rather than an error.
    ///
    /// `session` is borrowed (not cloned) because dispatch only needs
    /// the ID and runtime handle; nothing is persisted back.
    pub async fn dispatch(
        &self,
        session: &Session,
        reaction_key: &str,
    ) -> Result<Option<ReactionOutcome>> {
        let Some(cfg) = self.config.get(reaction_key).cloned() else {
            tracing::debug!(
                reaction = reaction_key,
                session = %session.id,
                "no reaction configured; skipping"
            );
            return Ok(None);
        };

        // `auto: false` means "the key exists so don't fall through to a
        // default, but don't actually do anything automatically". For
        // non-notify actions we skip entirely. For `Notify` we DO run it
        // (a disabled reaction still wants to surface to a human) but we
        // bypass the retry/escalation machinery — `auto: false` notify
        // has no budget, it just fires once per transition. Otherwise a
        // user who configured `auto: false` + `retries: 0` would see
        // spurious escalations on the first attempt.
        if !cfg.auto {
            if cfg.action == ReactionAction::Notify {
                let outcome = self
                    .dispatch_notify(session, reaction_key, &cfg, false)
                    .await;
                return Ok(Some(outcome));
            }
            tracing::debug!(
                reaction = reaction_key,
                session = %session.id,
                "reaction auto: false; skipping non-notify action"
            );
            return Ok(None);
        }

        // Resolve the duration-form `escalate_after` gate BEFORE the
        // tracker lock. `parse_duration` is pure and `warn_once_parse_failure`
        // takes its own independent lock — parsing outside avoids nested
        // locking. A `None` result here either means "no duration gate
        // configured" or "garbage string, already warned" — in both cases
        // the duration gate contributes nothing to escalation this dispatch.
        let duration_gate: Option<Duration> = match cfg.escalate_after {
            Some(EscalateAfter::Duration(ref s)) => match parse_duration(s) {
                Some(d) => Some(d),
                None => {
                    self.warn_once_parse_failure(reaction_key, "escalate_after", s);
                    None
                }
            },
            _ => None,
        };

        // Bump attempts under the lock and decide escalation inside the
        // same critical section so two concurrent dispatches can't both
        // escape the retry budget.
        let (attempts, should_escalate) = {
            let mut trackers = self
                .trackers
                .lock()
                .expect("reaction tracker mutex poisoned");
            let entry = trackers
                .entry((session.id.clone(), reaction_key.to_string()))
                .or_insert_with(|| TrackerState {
                    attempts: 0,
                    first_triggered_at: Instant::now(),
                });
            entry.attempts += 1;
            let attempts = entry.attempts;

            // Gate 1: `retries` budget. TS semantics — this is the MAX
            // number of attempts the engine will make before escalating.
            // Unset = infinite.
            let max_attempts = cfg.retries;
            let mut escalate = max_attempts.is_some_and(|n| attempts > n);

            // Gate 2: `escalate_after`. Either the attempts form (`N`)
            // with the same `>` comparison as retries, or the duration
            // form honoured via `first_triggered_at.elapsed()`. Only
            // one variant fires per dispatch because `escalate_after`
            // is a single `Option<enum>`.
            if let Some(EscalateAfter::Attempts(n)) = cfg.escalate_after {
                if attempts > n {
                    escalate = true;
                }
            } else if let Some(d) = duration_gate {
                // `>=` would fire on `0s` with zero elapsed too, but
                // that's not a sensible config and TS uses strict `>`.
                // We match TS: `elapsed > d` fires, `elapsed == d` doesn't.
                if entry.first_triggered_at.elapsed() > d {
                    escalate = true;
                }
            }

            (attempts, escalate)
        };

        if should_escalate {
            self.emit(OrchestratorEvent::ReactionEscalated {
                id: session.id.clone(),
                reaction_key: reaction_key.to_string(),
                attempts,
            });
            // Escalation ALWAYS reports as an executed `Notify`, regardless
            // of the originally configured action. Phase B routes through
            // the registry so a retry-exhausted escalation actually
            // reaches configured notifiers. `dispatch_notify` emits the
            // `ReactionTriggered(Notify)` event internally.
            let outcome = self
                .dispatch_notify(session, reaction_key, &cfg, true)
                .await;
            return Ok(Some(outcome));
        }

        let outcome = match cfg.action {
            ReactionAction::SendToAgent => {
                self.dispatch_send_to_agent(session, reaction_key, &cfg)
                    .await
            }
            ReactionAction::Notify => {
                self.dispatch_notify(session, reaction_key, &cfg, false)
                    .await
            }
            ReactionAction::AutoMerge => {
                self.dispatch_auto_merge(session, reaction_key, &cfg).await
            }
        };
        Ok(Some(outcome))
    }

    /// Forget any tracker state for `(session, reaction_key)`. Called by
    /// `LifecycleManager` on the tick that *leaves* a triggering status,
    /// so the next time the session re-enters it, retries start from zero.
    /// A lingering tracker would mean a session that failed CI, was fixed,
    /// and failed again would start already half-way through the retry
    /// budget — not what anyone wants.
    pub fn clear_tracker(&self, session_id: &SessionId, reaction_key: &str) {
        let mut trackers = self
            .trackers
            .lock()
            .expect("reaction tracker mutex poisoned");
        trackers.remove(&(session_id.clone(), reaction_key.to_string()));
    }

    /// Drop every tracker entry for `session_id`. Called by
    /// `LifecycleManager::terminate` — without this, the map would grow
    /// monotonically over a long-running `ao-rs watch` as terminated
    /// sessions leave orphan entries behind. Cheap: one full-map walk
    /// per termination, and the N is small (reaction-key count).
    pub fn clear_all_for_session(&self, session_id: &SessionId) {
        let mut trackers = self
            .trackers
            .lock()
            .expect("reaction tracker mutex poisoned");
        trackers.retain(|(sid, _), _| sid != session_id);
    }

    /// Current attempt count for `(session, reaction_key)`. Returns 0 if
    /// no tracker exists yet. Exposed for tests and for future CLI
    /// debugging (`ao-rs react status <id>`).
    pub fn attempts(&self, session_id: &SessionId, reaction_key: &str) -> u32 {
        self.trackers
            .lock()
            .expect("reaction tracker mutex poisoned")
            .get(&(session_id.clone(), reaction_key.to_string()))
            .map(|t| t.attempts)
            .unwrap_or(0)
    }

    /// Return the `Instant` at which this `(session, reaction_key)` pair
    /// was first triggered, or `None` if no tracker exists yet. Used by
    /// tests to assert the timestamp survives multiple dispatches — in
    /// production, `dispatch` reads the field inside the mutex-held
    /// critical section directly, so no external accessor is needed
    /// outside test code.
    #[cfg(test)]
    fn first_triggered_at(&self, session_id: &SessionId, reaction_key: &str) -> Option<Instant> {
        self.trackers
            .lock()
            .expect("reaction tracker mutex poisoned")
            .get(&(session_id.clone(), reaction_key.to_string()))
            .map(|t| t.first_triggered_at)
    }

    /// Emit a `tracing::warn!` exactly once per `(reaction_key, field)`
    /// pair for a duration-parse failure, then remember we've warned so
    /// subsequent parse failures for the same pair are silent.
    ///
    /// Used by two call sites:
    ///
    /// - `dispatch` for malformed `escalate_after` strings.
    /// - `LifecycleManager::check_stuck` (via the engine's config
    ///   accessor) for malformed `threshold` strings on the
    ///   `agent-stuck` reaction.
    ///
    /// See Design Decision 9 in
    /// `docs/ai/design/feature-agent-stuck-detection.md` and the
    /// warn-once observability note in the non-functional requirements
    /// section of the same doc.
    pub(crate) fn warn_once_parse_failure(&self, reaction_key: &str, field: &str, raw: &str) {
        let key = format!("{reaction_key}.{field}");
        let mut warned = self
            .warned_parse_failures
            .lock()
            .expect("reaction warned_parse_failures mutex poisoned");
        if warned.insert(key) {
            tracing::warn!(
                reaction = reaction_key,
                field = field,
                value = raw,
                "ignoring unparseable duration string; expected `^\\d+(s|m|h)$`"
            );
        }
    }

    // ---------- action implementations ---------- //

    async fn dispatch_send_to_agent(
        &self,
        session: &Session,
        reaction_key: &str,
        cfg: &ReactionConfig,
    ) -> ReactionOutcome {
        // `SendToAgent` requires a message body. A missing message is
        // recorded as a failure rather than falling through to a generic
        // boilerplate — Phase D keeps the config honest and surfaces bad
        // configs rather than silently sending noise to the agent.
        let Some(message) = cfg.message.clone() else {
            tracing::warn!(
                reaction = reaction_key,
                session = %session.id,
                "send-to-agent configured without a message; skipping"
            );
            return ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: false,
                action: ReactionAction::SendToAgent,
                message: None,
                escalated: false,
            };
        };

        // `send-to-agent` needs a live runtime handle. A session that's
        // still Spawning may not have one yet — count it as a soft failure
        // (no event emitted) so the next tick can retry.
        let Some(handle) = session.runtime_handle.as_deref() else {
            tracing::warn!(
                reaction = reaction_key,
                session = %session.id,
                "send-to-agent but session has no runtime_handle yet"
            );
            return ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: false,
                action: ReactionAction::SendToAgent,
                message: Some(message),
                escalated: false,
            };
        };

        match self.runtime.send_message(handle, &message).await {
            Ok(()) => {
                self.emit(OrchestratorEvent::ReactionTriggered {
                    id: session.id.clone(),
                    reaction_key: reaction_key.to_string(),
                    action: ReactionAction::SendToAgent,
                });
                let priority = resolve_priority(reaction_key, cfg);
                self.emit(OrchestratorEvent::UiNotification {
                    notification: UiNotification {
                        id: session.id.clone(),
                        reaction_key: reaction_key.to_string(),
                        action: ReactionAction::SendToAgent,
                        message: Some(message.clone()),
                        priority: Some(priority.as_str().to_string()),
                    },
                });
                ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: true,
                    action: ReactionAction::SendToAgent,
                    message: Some(message),
                    escalated: false,
                }
            }
            Err(e) => {
                // Don't emit a triggered event on send failure — subscribers
                // would misread it as "message delivered". The tracker has
                // already been incremented, so the next dispatch (from the
                // next tick) will count against the same retry budget.
                tracing::warn!(
                    reaction = reaction_key,
                    session = %session.id,
                    error = %e,
                    "runtime.send_message failed; retry next tick"
                );
                ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: false,
                    action: ReactionAction::SendToAgent,
                    message: Some(message),
                    escalated: false,
                }
            }
        }
    }

    /// Notify dispatcher. Phase B wires the `NotifierRegistry` so
    /// `Notify` actions fan out to real plugins instead of just emitting
    /// an event. The `ReactionTriggered` event is always emitted first
    /// (CLI `ao-rs watch` depends on it) — the plugin fan-out is
    /// additive.
    ///
    /// Without a registry (`notifier_registry: None`), returns
    /// `success = true` with no side effects beyond the event. This
    /// preserves Phase D compatibility for existing test fixtures that
    /// build an engine without notifiers.
    ///
    /// `escalated` is passed through into both the `NotificationPayload`
    /// and the returned `ReactionOutcome`. The escalation call site
    /// (`dispatch`) sets this to `true` after emitting
    /// `ReactionEscalated`; the normal Notify path always passes
    /// `false`.
    async fn dispatch_notify(
        &self,
        session: &Session,
        reaction_key: &str,
        cfg: &ReactionConfig,
        escalated: bool,
    ) -> ReactionOutcome {
        // Always emit — subscribers depend on seeing this event.
        self.emit(OrchestratorEvent::ReactionTriggered {
            id: session.id.clone(),
            reaction_key: reaction_key.to_string(),
            action: ReactionAction::Notify,
        });

        let priority = if escalated {
            cfg.priority.unwrap_or(EventPriority::Urgent)
        } else {
            resolve_priority(reaction_key, cfg)
        };

        let Some(registry) = &self.notifier_registry else {
            // No registry — Phase D behaviour.
            self.emit(OrchestratorEvent::UiNotification {
                notification: UiNotification {
                    id: session.id.clone(),
                    reaction_key: reaction_key.to_string(),
                    action: ReactionAction::Notify,
                    message: cfg.message.clone(),
                    priority: Some(priority.as_str().to_string()),
                },
            });
            return ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: true,
                action: ReactionAction::Notify,
                message: cfg.message.clone(),
                escalated,
            };
        };

        let payload = build_payload(session, reaction_key, cfg, priority, escalated);
        self.emit(OrchestratorEvent::UiNotification {
            notification: UiNotification {
                id: session.id.clone(),
                reaction_key: reaction_key.to_string(),
                action: ReactionAction::Notify,
                message: cfg.message.clone(),
                priority: Some(priority.as_str().to_string()),
            },
        });
        let targets = registry.resolve(priority);

        if targets.is_empty() {
            // Routing resolved to nothing — still success (no plugin
            // was expected to act, so nothing failed).
            return ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: true,
                action: ReactionAction::Notify,
                message: cfg.message.clone(),
                escalated,
            };
        }

        // Fan out to all notifiers concurrently. We still keep failure
        // reporting deterministic by sorting results back into routing order.
        let mut tasks = Vec::with_capacity(targets.len());
        for (idx, (name, plugin)) in targets.into_iter().enumerate() {
            let payload = payload.clone();
            let name_for_task = name.clone();
            tasks.push(tokio::spawn(async move {
                let res = plugin.send(&payload).await;
                (idx, name_for_task, res)
            }));
        }

        let mut results = Vec::with_capacity(tasks.len());
        for task in tasks {
            match task.await {
                Ok(tuple) => results.push(tuple),
                Err(join_err) => {
                    // A notifier task panicked or was cancelled. Treat as a failure
                    // but never take down the engine.
                    results.push((
                        usize::MAX,
                        "<join>".to_string(),
                        Err(NotifierError::Unavailable(format!(
                            "notifier task join failure: {join_err}"
                        ))),
                    ));
                }
            }
        }
        results.sort_by_key(|(idx, _, _)| *idx);

        let mut failed = Vec::new();
        for (_idx, name, res) in results {
            if let Err(e) = res {
                tracing::warn!(
                    notifier = name.as_str(),
                    reaction = reaction_key,
                    error = %e,
                    "notifier send failed"
                );
                failed.push(format!("{name}: {e}"));
            }
        }

        ReactionOutcome {
            reaction_type: reaction_key.to_string(),
            success: failed.is_empty(),
            action: ReactionAction::Notify,
            message: if failed.is_empty() {
                cfg.message.clone()
            } else {
                Some(format!("notifier failures: {}", failed.join("; ")))
            },
            escalated,
        }
    }

    /// Auto-merge dispatcher.
    ///
    /// Phase F finally wires the real merge. The flow is deliberately
    /// conservative because `approved-and-green` fires off an *older*
    /// observation — by the time the engine runs, CI may have flipped
    /// red, the reviewer may have dismissed, etc. So before actually
    /// calling `Scm::merge` we:
    ///
    /// 1. Re-probe `detect_pr` (the PR the session was tracking may be
    ///    gone if the agent force-pushed).
    /// 2. Re-probe `mergeability` — only proceed if `is_ready()` still
    ///    holds. A stale-green observation skips the merge and degrades
    ///    to an "intent only" event; the next tick can re-trigger if
    ///    the PR actually becomes mergeable again.
    /// 3. Call `Scm::merge(pr, None)` — `None` lets the plugin pick its
    ///    default merge method (configured at plugin-construction time).
    ///
    /// If no SCM plugin is attached (e.g. `with_scm` was never called),
    /// the engine falls back to the Phase D behaviour: emit intent,
    /// return success, don't actually merge. This keeps existing test
    /// fixtures that only wire a Runtime + events channel from breaking.
    ///
    /// ## Merge-failure recovery: parking loop (Phase G)
    ///
    /// When `Scm::merge` fails, the engine still reports the outcome
    /// as `ReactionOutcome { success: false, action: AutoMerge, .. }`
    /// — the engine's job is just to run the action once and report
    /// truthfully. The *retry* architecture lives one layer up in
    /// `LifecycleManager::transition`: it inspects the outcome and
    /// parks the session in `SessionStatus::MergeFailed`. On the next
    /// tick, a still-ready SCM observation re-promotes `MergeFailed`
    /// to `Mergeable` through the normal `derive_scm_status` ladder,
    /// which fires this dispatcher again and burns another attempt
    /// against the same `(session_id, "approved-and-green")` tracker.
    /// After the retry budget (`retries` / `escalate_after`) is
    /// exhausted the dispatcher's top-level escalation path flips to
    /// `Notify` and the lifecycle leaves the session in `Mergeable`
    /// (the parking check skips escalated outcomes), so the human is
    /// notified exactly once.
    ///
    /// The parking hook also respects the stale-green, no-PR, and
    /// `detect_pr` error branches above: they all report
    /// `success = false`, so the lifecycle parks them too. Either the
    /// next observation says "still ready" (retry) or "not ready"
    /// (drop off the ladder via `status_with_pr`). The session never
    /// gets stuck silently the way the pre-Phase-G flow did.
    ///
    /// See `LifecycleManager::transition`'s `should_park_in_merge_failed`
    /// / `park_in_merge_failed` helpers for the lifecycle side, and
    /// `docs/state-machine.md#the-mergefailed-parking-loop-phase-g`
    /// for the full transition table.
    ///
    /// The engine-side contract tested by
    /// `dispatch_auto_merge_propagates_merge_error_as_soft_failure`
    /// remains: the engine reports `success: false` and never tries
    /// to implement its own retry loop. Retry is a policy owned by
    /// the lifecycle, not the engine.
    ///
    /// `cfg.merge_method` is passed to `Scm::merge` when set; otherwise
    /// the SCM plugin uses its own default (GitHub: merge commit).
    async fn dispatch_auto_merge(
        &self,
        session: &Session,
        reaction_key: &str,
        cfg: &ReactionConfig,
    ) -> ReactionOutcome {
        // Phase D-compat path: no SCM attached → emit the intent event
        // and return success without merging. Existing Phase D tests and
        // downstream subscribers that predate Phase F see no change.
        let Some(scm) = self.scm.as_ref() else {
            tracing::info!(
                reaction = reaction_key,
                session = %session.id,
                "auto-merge requested but no SCM plugin attached; emitting intent only"
            );
            self.emit(OrchestratorEvent::ReactionTriggered {
                id: session.id.clone(),
                reaction_key: reaction_key.to_string(),
                action: ReactionAction::AutoMerge,
            });
            return ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: true,
                action: ReactionAction::AutoMerge,
                message: None,
                escalated: false,
            };
        };

        // Re-probe the PR. If `detect_pr` fails or returns `None`, we
        // don't have anything to merge — count as a soft failure so the
        // next tick can retry.
        //
        // Design note: we deliberately do NOT emit `ReactionTriggered`
        // on skip paths. A subscriber reading the event stream can rely
        // on "triggered(AutoMerge)" meaning an `Scm::merge` call was
        // actually attempted. The only difference between "attempted +
        // succeeded" and "attempted + failed" is the `success` flag on
        // the `ReactionOutcome` returned to the caller (usually the
        // lifecycle loop, which logs but does not re-emit).
        let pr = match scm.detect_pr(session).await {
            Ok(Some(pr)) => pr,
            Ok(None) => {
                tracing::warn!(
                    reaction = reaction_key,
                    session = %session.id,
                    "auto-merge: detect_pr returned None; nothing to merge"
                );
                return ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: false,
                    action: ReactionAction::AutoMerge,
                    message: None,
                    escalated: false,
                };
            }
            Err(e) => {
                tracing::warn!(
                    reaction = reaction_key,
                    session = %session.id,
                    error = %e,
                    "auto-merge: detect_pr errored; retry next tick"
                );
                return ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: false,
                    action: ReactionAction::AutoMerge,
                    message: None,
                    escalated: false,
                };
            }
        };

        // Re-verify readiness. The transition that got us here was based
        // on an observation that could be a few hundred ms old; a late
        // CI flake or a dismissed review must abort the merge.
        //
        // We deliberately do NOT re-probe `pr_state` on the theory that
        // `mergeability` subsumes it: a `Closed` or `Merged` PR reports
        // `is_ready() == false` with a blocker listing the terminal
        // state. The extra `gh pr view --state` round-trip would just
        // cost a second RTT for information already in the readiness
        // blob. If this assumption ever breaks (e.g. a plugin's
        // `mergeability` decouples from `state`), add the third probe
        // here and update the comment.
        let ready = match scm.mergeability(&pr).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    reaction = reaction_key,
                    session = %session.id,
                    error = %e,
                    "auto-merge: mergeability re-probe failed; skipping merge"
                );
                return ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: false,
                    action: ReactionAction::AutoMerge,
                    message: None,
                    escalated: false,
                };
            }
        };
        if !ready.is_ready() {
            tracing::info!(
                reaction = reaction_key,
                session = %session.id,
                blockers = ?ready.blockers,
                "auto-merge: readiness re-probe says not ready; skipping"
            );
            return ReactionOutcome {
                reaction_type: reaction_key.to_string(),
                success: false,
                action: ReactionAction::AutoMerge,
                message: None,
                escalated: false,
            };
        }

        // Commit point — we're about to call `Scm::merge`. Emit the
        // `ReactionTriggered` event here (not earlier) so subscribers
        // see it only when a real merge call is going to happen. All
        // the soft-failure paths above leave the event stream silent.
        self.emit(OrchestratorEvent::ReactionTriggered {
            id: session.id.clone(),
            reaction_key: reaction_key.to_string(),
            action: ReactionAction::AutoMerge,
        });

        // Actually merge. `None` = plugin default merge method.
        match scm.merge(&pr, cfg.merge_method).await {
            Ok(()) => {
                tracing::info!(
                    reaction = reaction_key,
                    session = %session.id,
                    pr = pr.number,
                    "auto-merge: merged successfully"
                );
                ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: true,
                    action: ReactionAction::AutoMerge,
                    message: Some(format!("merged PR #{}", pr.number)),
                    escalated: false,
                }
            }
            Err(e) => {
                tracing::warn!(
                    reaction = reaction_key,
                    session = %session.id,
                    pr = pr.number,
                    error = %e,
                    "auto-merge: Scm::merge failed"
                );
                ReactionOutcome {
                    reaction_type: reaction_key.to_string(),
                    success: false,
                    action: ReactionAction::AutoMerge,
                    message: Some(format!("merge failed: {e}")),
                    escalated: false,
                }
            }
        }
    }

    /// Broadcast an event. A send error means zero subscribers — the
    /// same "not worth surfacing" case as `LifecycleManager::emit`.
    fn emit(&self, event: OrchestratorEvent) {
        let _ = self.events_tx.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        reactions::{EscalateAfter, ReactionAction, ReactionConfig},
        traits::Runtime,
        types::{now_ms, ActivityState, Session, SessionId, SessionStatus},
    };
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex as StdMutex;

    // ---------- Helpers ---------- //

    /// Mock runtime that records every send_message for assertions.
    struct RecordingRuntime {
        sends: StdMutex<Vec<(String, String)>>,
        fail_send: std::sync::atomic::AtomicBool,
    }

    impl RecordingRuntime {
        fn new() -> Self {
            Self {
                sends: StdMutex::new(Vec::new()),
                fail_send: std::sync::atomic::AtomicBool::new(false),
            }
        }
        fn sends(&self) -> Vec<(String, String)> {
            self.sends.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Runtime for RecordingRuntime {
        async fn create(
            &self,
            _session_id: &str,
            _cwd: &Path,
            _launch_command: &str,
            _env: &[(String, String)],
        ) -> Result<String> {
            Ok("mock-handle".into())
        }
        async fn send_message(&self, handle: &str, msg: &str) -> Result<()> {
            if self.fail_send.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("mock send failed".into()));
            }
            self.sends
                .lock()
                .unwrap()
                .push((handle.to_string(), msg.to_string()));
            Ok(())
        }
        async fn is_alive(&self, _handle: &str) -> Result<bool> {
            Ok(true)
        }
        async fn destroy(&self, _handle: &str) -> Result<()> {
            Ok(())
        }
    }

    fn fake_session(id: &str) -> Session {
        Session {
            id: SessionId(id.into()),
            project_id: "demo".into(),
            status: SessionStatus::CiFailed,
            agent: "claude-code".into(),
            agent_config: None,
            branch: format!("ao-{id}"),
            task: "t".into(),
            workspace_path: Some(PathBuf::from("/tmp/ws")),
            runtime_handle: Some(format!("handle-{id}")),
            runtime: "tmux".into(),
            activity: Some(ActivityState::Ready),
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
        }
    }

    fn build(
        cfg_map: HashMap<String, ReactionConfig>,
    ) -> (
        Arc<ReactionEngine>,
        Arc<RecordingRuntime>,
        broadcast::Receiver<OrchestratorEvent>,
    ) {
        let runtime = Arc::new(RecordingRuntime::new());
        let (tx, rx) = broadcast::channel(32);
        let engine = Arc::new(ReactionEngine::new(
            cfg_map,
            runtime.clone() as Arc<dyn Runtime>,
            tx,
        ));
        (engine, runtime, rx)
    }

    fn drain(rx: &mut broadcast::Receiver<OrchestratorEvent>) -> Vec<OrchestratorEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    // ---------- Tests ---------- //

    #[test]
    fn status_map_covers_reactions_through_phase_h() {
        assert_eq!(
            status_to_reaction_key(SessionStatus::CiFailed),
            Some("ci-failed")
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
        // Negative cases — statuses that deliberately don't map to a
        // reaction today. If any of these ever gain a reaction, update
        // this test alongside the match arm above so the mapping stays
        // honest.
        assert_eq!(status_to_reaction_key(SessionStatus::Working), None);
        assert_eq!(status_to_reaction_key(SessionStatus::Approved), None);
        assert_eq!(status_to_reaction_key(SessionStatus::NeedsInput), None);
        assert_eq!(status_to_reaction_key(SessionStatus::MergeFailed), None);
        assert_eq!(status_to_reaction_key(SessionStatus::Errored), None);
    }

    // ---------- Phase H: parse_duration ---------- //

    #[test]
    fn parse_duration_accepts_seconds() {
        assert_eq!(parse_duration("1s"), Some(Duration::from_secs(1)));
        assert_eq!(parse_duration("10s"), Some(Duration::from_secs(10)));
        assert_eq!(parse_duration("300s"), Some(Duration::from_secs(300)));
    }

    #[test]
    fn parse_duration_accepts_minutes() {
        assert_eq!(parse_duration("1m"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
    }

    #[test]
    fn parse_duration_accepts_hours() {
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(24 * 3600)));
    }

    #[test]
    fn parse_duration_accepts_zero() {
        // Matches the "no clamping, no floor" decision in the requirements
        // doc — zero is a legitimate test-fixture value (fires on the first
        // idle tick the session observes).
        assert_eq!(parse_duration("0s"), Some(Duration::ZERO));
        assert_eq!(parse_duration("0m"), Some(Duration::ZERO));
        assert_eq!(parse_duration("0h"), Some(Duration::ZERO));
    }

    #[test]
    fn parse_duration_rejects_missing_suffix() {
        assert_eq!(parse_duration("10"), None);
    }

    #[test]
    fn parse_duration_rejects_empty() {
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn parse_duration_rejects_non_numeric() {
        assert_eq!(parse_duration("fast"), None);
        assert_eq!(parse_duration("ten seconds"), None);
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn parse_duration_rejects_compound_form() {
        // TS `parseDuration` doesn't accept `1m30s` — neither do we.
        // Matches the regex `^\d+(s|m|h)$` exactly.
        assert_eq!(parse_duration("1m30s"), None);
        assert_eq!(parse_duration("1h30m"), None);
        assert_eq!(parse_duration("2d"), None);
    }

    #[test]
    fn parse_duration_rejects_negative_and_decimals() {
        assert_eq!(parse_duration("-5m"), None);
        assert_eq!(parse_duration("1.5h"), None);
        assert_eq!(parse_duration("0.5s"), None);
    }

    #[test]
    fn parse_duration_rejects_suffix_only() {
        assert_eq!(parse_duration("s"), None);
        assert_eq!(parse_duration("m"), None);
        assert_eq!(parse_duration("h"), None);
    }

    #[test]
    fn parse_duration_rejects_overflow() {
        // `u64::MAX` seconds parsed fine, but multiplying the digits of
        // an unbounded hours string must short-circuit to None rather
        // than panic or wrap.
        assert_eq!(parse_duration("99999999999999999999h"), None);
    }

    #[tokio::test]
    async fn tracker_first_triggered_at_persists_across_dispatches() {
        // Invariant for Task 1.2: the first dispatch populates
        // `first_triggered_at`; subsequent dispatches bump `attempts`
        // but DO NOT reset the timestamp. This is what duration-based
        // escalation will rely on (Task 2.1) — escalate_after: 10m
        // must measure from the first trigger, not from the last
        // attempt.
        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("hi".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, _runtime, _rx) = build(map);
        let session = fake_session("s1");

        // Never-triggered: attempts = 0, no tracker entry.
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 0);
        assert!(engine
            .first_triggered_at(&session.id, "ci-failed")
            .is_none());

        // First dispatch populates both fields.
        engine.dispatch(&session, "ci-failed").await.unwrap();
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 1);
        let first = engine
            .first_triggered_at(&session.id, "ci-failed")
            .expect("first dispatch must populate first_triggered_at");

        // Tiny sleep so a resetting bug would be observable — the
        // second dispatch's `Instant::now()` is guaranteed later than
        // `first`.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Second dispatch increments attempts; timestamp unchanged.
        engine.dispatch(&session, "ci-failed").await.unwrap();
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 2);
        assert_eq!(
            engine.first_triggered_at(&session.id, "ci-failed"),
            Some(first),
            "first_triggered_at must survive subsequent dispatches"
        );
    }

    #[tokio::test]
    async fn tracker_first_triggered_at_resets_after_clear() {
        // Clearing the tracker (on status change away from the
        // triggering status) drops the whole entry, so the next
        // dispatch gets a fresh `first_triggered_at`. Protects the
        // "second episode starts a fresh clock" property the doc
        // comment promises.
        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("hi".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, _runtime, _rx) = build(map);
        let session = fake_session("s1");

        engine.dispatch(&session, "ci-failed").await.unwrap();
        let first = engine
            .first_triggered_at(&session.id, "ci-failed")
            .expect("populated");

        // Simulate the lifecycle's "left the triggering status" hook.
        engine.clear_tracker(&session.id, "ci-failed");
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 0);
        assert!(engine
            .first_triggered_at(&session.id, "ci-failed")
            .is_none());

        tokio::time::sleep(Duration::from_millis(5)).await;

        engine.dispatch(&session, "ci-failed").await.unwrap();
        let second = engine
            .first_triggered_at(&session.id, "ci-failed")
            .expect("repopulated");
        assert!(
            second > first,
            "second episode must start a fresh first_triggered_at"
        );
    }

    #[tokio::test]
    async fn dispatch_unconfigured_key_is_noop() {
        let (engine, runtime, mut rx) = build(HashMap::new());
        let session = fake_session("s1");
        let result = engine.dispatch(&session, "ci-failed").await.unwrap();
        assert!(result.is_none());
        assert!(runtime.sends().is_empty());
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn dispatch_send_to_agent_calls_runtime_and_emits_event() {
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("CI broke — please fix.".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, mut rx) = build(map);
        let session = fake_session("s1");

        let result = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();

        assert!(result.success);
        assert!(!result.escalated);
        assert_eq!(result.action, ReactionAction::SendToAgent);
        assert_eq!(runtime.sends().len(), 1);
        assert_eq!(runtime.sends()[0].0, "handle-s1");
        assert_eq!(runtime.sends()[0].1, "CI broke — please fix.");

        let events = drain(&mut rx);
        assert_eq!(events.len(), 2, "got {events:?}");
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::ReactionTriggered {
                reaction_key,
                action: ReactionAction::SendToAgent,
                ..
            } if reaction_key == "ci-failed"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::UiNotification { notification } if notification.reaction_key == "ci-failed"
        )));
    }

    #[tokio::test]
    async fn dispatch_send_to_agent_without_message_fails_softly() {
        let config = ReactionConfig::new(ReactionAction::SendToAgent); // no message
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, mut rx) = build(map);
        let session = fake_session("s1");
        let result = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();

        assert!(!result.success);
        assert!(runtime.sends().is_empty());
        // No event emitted on soft failure — subscribers shouldn't see a
        // "triggered" event for a dispatch that never left the engine.
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn dispatch_send_to_agent_propagates_runtime_send_failure_as_soft_failure() {
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix it".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, mut rx) = build(map);
        runtime.fail_send.store(true, Ordering::SeqCst);
        let session = fake_session("s1");

        let result = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(!result.success);
        // Attempt still counted — so the next tick's retry competes for
        // the same retry budget. Guards against a pathological case where
        // the engine would forget that it tried.
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 1);
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn dispatch_notify_emits_event_and_succeeds() {
        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("approved & green".into());
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let (engine, runtime, mut rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();

        assert!(result.success);
        assert_eq!(result.action, ReactionAction::Notify);
        assert!(runtime.sends().is_empty());

        let events = drain(&mut rx);
        assert_eq!(events.len(), 2, "got {events:?}");
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::Notify,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::UiNotification { notification } if notification.action == ReactionAction::Notify
        )));
    }

    #[tokio::test]
    async fn dispatch_auto_merge_without_scm_falls_back_to_phase_d_behaviour() {
        // Guard the backwards-compatible path: engines constructed
        // without `.with_scm(...)` (e.g. the existing Phase D fixtures)
        // must keep emitting intent + returning success without making
        // any SCM calls. Breaking this test would silently regress
        // every test that builds an engine the Phase D way.
        let config = ReactionConfig::new(ReactionAction::AutoMerge);
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let (engine, _runtime, mut rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();
        assert!(result.success);
        assert_eq!(result.action, ReactionAction::AutoMerge);

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::AutoMerge,
                ..
            }
        ));
    }

    // ---------- Phase F: auto-merge with SCM plugin ---------- //

    use crate::scm::{
        CheckRun, CiStatus, MergeMethod, MergeReadiness, PrState, PullRequest, Review,
        ReviewComment, ReviewDecision,
    };

    /// Scripted SCM plugin. Each method reads from `Mutex<_>` cells so
    /// tests can pre-configure the responses `dispatch_auto_merge` will
    /// see on its re-probe.
    struct MergeMockScm {
        pr: StdMutex<Option<PullRequest>>,
        readiness: StdMutex<MergeReadiness>,
        merge_calls: StdMutex<Vec<(u32, Option<MergeMethod>)>>,
        detect_pr_errors: std::sync::atomic::AtomicBool,
        merge_errors: std::sync::atomic::AtomicBool,
    }

    impl MergeMockScm {
        fn new(pr: Option<PullRequest>, readiness: MergeReadiness) -> Self {
            Self {
                pr: StdMutex::new(pr),
                readiness: StdMutex::new(readiness),
                merge_calls: StdMutex::new(Vec::new()),
                detect_pr_errors: std::sync::atomic::AtomicBool::new(false),
                merge_errors: std::sync::atomic::AtomicBool::new(false),
            }
        }
        fn merges(&self) -> Vec<(u32, Option<MergeMethod>)> {
            self.merge_calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Scm for MergeMockScm {
        fn name(&self) -> &str {
            "merge-mock"
        }
        async fn detect_pr(&self, _session: &Session) -> Result<Option<PullRequest>> {
            if self.detect_pr_errors.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("detect_pr".into()));
            }
            Ok(self.pr.lock().unwrap().clone())
        }
        async fn pr_state(&self, _pr: &PullRequest) -> Result<PrState> {
            Ok(PrState::Open)
        }
        async fn ci_checks(&self, _pr: &PullRequest) -> Result<Vec<CheckRun>> {
            Ok(vec![])
        }
        async fn ci_status(&self, _pr: &PullRequest) -> Result<CiStatus> {
            Ok(CiStatus::Passing)
        }
        async fn reviews(&self, _pr: &PullRequest) -> Result<Vec<Review>> {
            Ok(vec![])
        }
        async fn review_decision(&self, _pr: &PullRequest) -> Result<ReviewDecision> {
            Ok(ReviewDecision::Approved)
        }
        async fn pending_comments(&self, _pr: &PullRequest) -> Result<Vec<ReviewComment>> {
            Ok(vec![])
        }
        async fn mergeability(&self, _pr: &PullRequest) -> Result<MergeReadiness> {
            Ok(self.readiness.lock().unwrap().clone())
        }
        async fn merge(&self, pr: &PullRequest, method: Option<MergeMethod>) -> Result<()> {
            if self.merge_errors.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("merge failed".into()));
            }
            self.merge_calls.lock().unwrap().push((pr.number, method));
            Ok(())
        }
    }

    fn ready_readiness() -> MergeReadiness {
        MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        }
    }

    fn fake_pr(number: u32) -> PullRequest {
        PullRequest {
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: "fix the widgets".into(),
            owner: "acme".into(),
            repo: "widgets".into(),
            branch: "ao-s1".into(),
            base_branch: "main".into(),
            is_draft: false,
        }
    }

    fn build_with_scm(
        cfg_map: HashMap<String, ReactionConfig>,
        scm: Arc<dyn Scm>,
    ) -> (
        Arc<ReactionEngine>,
        Arc<RecordingRuntime>,
        broadcast::Receiver<OrchestratorEvent>,
    ) {
        let runtime = Arc::new(RecordingRuntime::new());
        let (tx, rx) = broadcast::channel(32);
        let engine = Arc::new(
            ReactionEngine::new(cfg_map, runtime.clone() as Arc<dyn Runtime>, tx).with_scm(scm),
        );
        (engine, runtime, rx)
    }

    #[tokio::test]
    async fn dispatch_auto_merge_with_ready_pr_calls_scm_merge() {
        // Happy path: observation still holds on re-probe, engine calls
        // `Scm::merge(pr, None)` with the default merge method.
        let config = ReactionConfig::new(ReactionAction::AutoMerge);
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let scm = Arc::new(MergeMockScm::new(Some(fake_pr(42)), ready_readiness()));
        let (engine, _runtime, mut rx) = build_with_scm(map, scm.clone() as Arc<dyn Scm>);

        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();

        assert!(result.success);
        assert_eq!(result.action, ReactionAction::AutoMerge);
        assert_eq!(scm.merges().len(), 1, "expected one merge call");
        assert_eq!(scm.merges()[0], (42, None));
        assert!(result.message.unwrap().contains("merged PR #42"));

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::AutoMerge,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn dispatch_auto_merge_with_stale_green_observation_does_not_merge() {
        // The lifecycle tick saw mergeable=true, but by the time the
        // engine ran (a few hundred ms later) CI flipped red. The
        // re-probe says not-ready → skip the merge, return soft failure,
        // and emit NO event (the commit-point emit happens only when
        // the engine actually calls `Scm::merge`).
        //
        // This is the whole reason for the re-probe: avoid merging on
        // observations that have gone stale since the transition fired.
        let config = ReactionConfig::new(ReactionAction::AutoMerge);
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let stale = MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: true,
            no_conflicts: true,
            blockers: vec!["CI is failing".into()],
        };
        let scm = Arc::new(MergeMockScm::new(Some(fake_pr(42)), stale));
        let (engine, _runtime, mut rx) = build_with_scm(map, scm.clone() as Arc<dyn Scm>);

        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();

        assert!(!result.success, "stale observation must not merge");
        assert!(scm.merges().is_empty(), "Scm::merge must not be called");

        // No event emitted — a subscriber reading `ReactionTriggered`
        // should be able to trust that "triggered" means a merge was
        // actually attempted. Skip paths leave the stream silent.
        let events = drain(&mut rx);
        assert!(
            events.is_empty(),
            "stale-green skip must not emit events, got {events:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_auto_merge_with_no_pr_returns_soft_failure() {
        // `detect_pr` returns None (agent force-pushed, PR was closed
        // out-of-band). Nothing to merge → soft failure, no events.
        let config = ReactionConfig::new(ReactionAction::AutoMerge);
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let scm = Arc::new(MergeMockScm::new(None, ready_readiness()));
        let (engine, _runtime, mut rx) = build_with_scm(map, scm.clone() as Arc<dyn Scm>);

        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();

        assert!(!result.success);
        assert!(scm.merges().is_empty());
        // Pin the semantics: no triggered event on soft failure so a
        // future refactor can't accidentally leak a "we tried" event.
        let events = drain(&mut rx);
        assert!(
            events.is_empty(),
            "no-PR skip must not emit events, got {events:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_auto_merge_with_detect_pr_error_returns_soft_failure() {
        let config = ReactionConfig::new(ReactionAction::AutoMerge);
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let scm = Arc::new(MergeMockScm::new(Some(fake_pr(42)), ready_readiness()));
        scm.detect_pr_errors.store(true, Ordering::SeqCst);
        let (engine, _runtime, mut rx) = build_with_scm(map, scm.clone() as Arc<dyn Scm>);

        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();

        assert!(!result.success);
        assert!(scm.merges().is_empty(), "merge must not run on detect err");
        let events = drain(&mut rx);
        assert!(
            events.is_empty(),
            "detect_pr error must not emit events, got {events:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_auto_merge_propagates_merge_error_as_soft_failure() {
        // Scm::merge itself fails (branch protection, network, whatever).
        // Engine surfaces the error message in the outcome so the CLI
        // can print it. Tracker has still incremented — retry logic
        // applies on the next tick if the transition re-fires.
        let config = ReactionConfig::new(ReactionAction::AutoMerge);
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let scm = Arc::new(MergeMockScm::new(Some(fake_pr(42)), ready_readiness()));
        scm.merge_errors.store(true, Ordering::SeqCst);
        let (engine, _runtime, _rx) = build_with_scm(map, scm.clone() as Arc<dyn Scm>);

        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();

        assert!(!result.success);
        assert!(
            result.message.unwrap().contains("merge failed"),
            "error message should surface"
        );
    }

    #[tokio::test]
    async fn dispatch_auto_false_skips_active_actions_but_allows_notify() {
        // `auto: false` on SendToAgent → no-op.
        let mut sta = ReactionConfig::new(ReactionAction::SendToAgent);
        sta.auto = false;
        sta.message = Some("noop".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), sta);

        // `auto: false` on Notify → still runs (notify is always a human
        // call, the disable flag doesn't gate it).
        let mut notify = ReactionConfig::new(ReactionAction::Notify);
        notify.auto = false;
        map.insert("approved-and-green".into(), notify);

        let (engine, runtime, mut rx) = build(map);

        // Active action should be skipped entirely — no outcome, no event.
        let s1 = fake_session("s1");
        assert!(engine.dispatch(&s1, "ci-failed").await.unwrap().is_none());
        assert!(runtime.sends().is_empty());
        assert!(drain(&mut rx).is_empty());

        // Notify must still fire.
        let mut s2 = fake_session("s2");
        s2.status = SessionStatus::Mergeable;
        let result = engine
            .dispatch(&s2, "approved-and-green")
            .await
            .unwrap()
            .unwrap();
        assert!(result.success);
        assert_eq!(result.action, ReactionAction::Notify);
    }

    #[tokio::test]
    async fn retries_exhausted_escalates_to_notify_and_emits_both_events() {
        // retries: 2 means the 3rd dispatch attempt is the one that escalates.
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.retries = Some(2);
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, mut rx) = build(map);
        let session = fake_session("s1");

        // Attempts 1 and 2: normal SendToAgent.
        let r1 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r1.success);
        assert!(!r1.escalated);
        let r2 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r2.success);
        assert!(!r2.escalated);
        assert_eq!(runtime.sends().len(), 2);

        // Attempt 3: escalate.
        let r3 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r3.escalated);
        assert_eq!(r3.action, ReactionAction::Notify);
        // Runtime NOT called on escalation — we only notify.
        assert_eq!(runtime.sends().len(), 2);

        // Events across all three dispatches:
        // triggered(send), triggered(send), escalated + triggered(notify).
        let events = drain(&mut rx);
        assert_eq!(events.len(), 7, "got {events:?}");
        let escalated_count = events
            .iter()
            .filter(|e| matches!(e, OrchestratorEvent::ReactionEscalated { .. }))
            .count();
        assert_eq!(escalated_count, 1);
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::Notify,
                ..
            }
        )));
        assert!(matches!(
            events.last().unwrap(),
            OrchestratorEvent::UiNotification { .. }
        ));
    }

    #[tokio::test]
    async fn escalate_after_attempts_escalates_independently_of_retries() {
        // No `retries` → infinite, but `escalate-after: 1` forces
        // escalation after the first attempt.
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.escalate_after = Some(EscalateAfter::Attempts(1));
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, _rx) = build(map);
        let session = fake_session("s1");

        // Attempt 1: normal send.
        let r1 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(!r1.escalated);
        assert_eq!(runtime.sends().len(), 1);

        // Attempt 2: escalated (attempts=2 > 1).
        let r2 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r2.escalated);
        assert_eq!(runtime.sends().len(), 1);
    }

    #[tokio::test]
    async fn escalate_after_duration_does_not_fire_before_elapsed() {
        // Phase H contract: duration gate is now honoured, but 5 rapid
        // back-to-back dispatches with a `10m` threshold are nowhere
        // near elapsed, so no escalation fires — the retries path is
        // unset, and the duration gate compares against `first_triggered_at
        // + 10m`, which is still in the future. Exercises the happy
        // "gate configured, not yet tripped" path.
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.escalate_after = Some(EscalateAfter::Duration("10m".into()));
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, _rx) = build(map);
        let session = fake_session("s1");

        for _ in 0..5 {
            let r = engine
                .dispatch(&session, "ci-failed")
                .await
                .unwrap()
                .unwrap();
            assert!(!r.escalated);
        }
        assert_eq!(runtime.sends().len(), 5);
    }

    #[tokio::test]
    async fn escalate_after_duration_fires_once_elapsed_exceeds_threshold() {
        // Phase H: duration-based escalation is real. We rewind
        // `first_triggered_at` by more than the configured threshold
        // instead of sleeping — the logic under test is a pure
        // comparison against `elapsed()`, and rewinding exercises the
        // same code path far faster than waiting.
        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("stuck".into());
        config.retries = None; // no attempts gate — only duration gate
        config.escalate_after = Some(EscalateAfter::Duration("1s".into()));
        let mut map = HashMap::new();
        map.insert("agent-stuck".into(), config);

        let (engine, _runtime, mut rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Working;

        // First dispatch: tracker created, first_triggered_at = now,
        // elapsed ≈ 0, duration gate NOT tripped.
        let first = engine
            .dispatch(&session, "agent-stuck")
            .await
            .unwrap()
            .unwrap();
        assert!(!first.escalated);

        // Rewind so elapsed() > 1s on the next read. We access the
        // private tracker map directly because this test lives inside
        // the same module.
        {
            let mut trackers = engine.trackers.lock().unwrap();
            let key = (session.id.clone(), "agent-stuck".to_string());
            let entry = trackers.get_mut(&key).expect("tracker populated");
            entry.first_triggered_at = Instant::now()
                .checked_sub(Duration::from_secs(2))
                .expect("monotonic clock has been running >2s");
        }

        // Second dispatch: elapsed ≈ 2s > threshold 1s → escalate.
        let second = engine
            .dispatch(&session, "agent-stuck")
            .await
            .unwrap()
            .unwrap();
        assert!(second.escalated, "duration gate should have fired");
        assert_eq!(second.action, ReactionAction::Notify);

        // Both a ReactionEscalated and a ReactionTriggered(Notify) are
        // emitted on escalation — matches the attempts-form path.
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::ReactionEscalated { .. })),
            "expected ReactionEscalated, got {events:?}"
        );
    }

    #[tokio::test]
    async fn escalate_after_duration_with_garbage_string_logs_once_and_retries_gate_still_fires() {
        // A malformed `escalate_after` string must not panic or flip
        // `escalate = true`. The retries gate is independent and must
        // still fire after the configured number of attempts.
        // `warned_parse_failures` must record the key exactly once.
        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("stuck".into());
        config.retries = Some(2);
        config.escalate_after = Some(EscalateAfter::Duration("ten minutes".into()));
        let mut map = HashMap::new();
        map.insert("agent-stuck".into(), config);

        let (engine, _runtime, _rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Working;

        // 3 dispatches: attempts 1, 2 no escalate; attempt 3 > retries=2 escalates.
        let r1 = engine
            .dispatch(&session, "agent-stuck")
            .await
            .unwrap()
            .unwrap();
        assert!(!r1.escalated);
        let r2 = engine
            .dispatch(&session, "agent-stuck")
            .await
            .unwrap()
            .unwrap();
        assert!(!r2.escalated);
        let r3 = engine
            .dispatch(&session, "agent-stuck")
            .await
            .unwrap()
            .unwrap();
        assert!(
            r3.escalated,
            "retries gate must still fire even when escalate_after is garbage"
        );

        // Warn-once set should contain the key exactly once (three
        // dispatches, but only the first parse failure emits a warn).
        let warned = engine.warned_parse_failures.lock().unwrap();
        assert!(warned.contains("agent-stuck.escalate_after"));
        assert_eq!(
            warned.len(),
            1,
            "only one warn should be recorded across 3 dispatches"
        );
    }

    #[tokio::test]
    async fn warn_once_parse_failure_is_idempotent_per_key() {
        // Direct helper test: calling warn_once_parse_failure multiple
        // times with the same (reaction_key, field) pair inserts once
        // and is silently idempotent on subsequent calls. Calling with
        // a different field inserts a second entry — the two warnings
        // are independent so a reaction with BOTH threshold and
        // escalate_after broken gets warned about each.
        let (engine, _runtime, _rx) = build(HashMap::new());

        engine.warn_once_parse_failure("agent-stuck", "threshold", "ten");
        engine.warn_once_parse_failure("agent-stuck", "threshold", "eleven");
        engine.warn_once_parse_failure("agent-stuck", "threshold", "twelve");
        engine.warn_once_parse_failure("agent-stuck", "escalate_after", "frob");

        let warned = engine.warned_parse_failures.lock().unwrap();
        assert_eq!(warned.len(), 2);
        assert!(warned.contains("agent-stuck.threshold"));
        assert!(warned.contains("agent-stuck.escalate_after"));
    }

    #[tokio::test]
    async fn clear_tracker_after_escalation_restores_real_action() {
        // Contract: once a session escalates and then *leaves* the
        // triggering status (lifecycle calls clear_tracker), re-entering
        // the same status must run the configured action again rather
        // than immediately re-escalating. This is the whole point of
        // clearing trackers on exit — without it, a session that
        // recovered and re-failed would see nothing but escalations.
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.retries = Some(1);
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, runtime, _rx) = build(map);
        let session = fake_session("s1");

        // 1st attempt: SendToAgent runs.
        let r1 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(!r1.escalated);
        // 2nd attempt: escalates (attempts=2 > retries=1).
        let r2 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r2.escalated);
        assert_eq!(runtime.sends().len(), 1);

        // Lifecycle clears the tracker on exit from CiFailed.
        engine.clear_tracker(&session.id, "ci-failed");

        // Re-entry: action runs again from a clean slate.
        let r3 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r3.success);
        assert!(!r3.escalated);
        assert_eq!(r3.action, ReactionAction::SendToAgent);
        assert_eq!(runtime.sends().len(), 2);
    }

    #[tokio::test]
    async fn clear_all_for_session_drops_every_reaction_tracker() {
        // Covers the leak-guard added for `LifecycleManager::terminate`:
        // terminating a session must drop all its trackers regardless
        // of which reaction keys it touched.
        let mut ci = ReactionConfig::new(ReactionAction::SendToAgent);
        ci.message = Some("fix".into());
        let mut cr = ReactionConfig::new(ReactionAction::SendToAgent);
        cr.message = Some("review".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), ci);
        map.insert("changes-requested".into(), cr);

        let (engine, _runtime, _rx) = build(map);
        let a = fake_session("a");
        let b = fake_session("b");

        // Seed three trackers across two sessions.
        engine.dispatch(&a, "ci-failed").await.unwrap();
        engine.dispatch(&a, "changes-requested").await.unwrap();
        engine.dispatch(&b, "ci-failed").await.unwrap();
        assert_eq!(engine.attempts(&a.id, "ci-failed"), 1);
        assert_eq!(engine.attempts(&a.id, "changes-requested"), 1);
        assert_eq!(engine.attempts(&b.id, "ci-failed"), 1);

        // Wipe session a only.
        engine.clear_all_for_session(&a.id);

        assert_eq!(engine.attempts(&a.id, "ci-failed"), 0);
        assert_eq!(engine.attempts(&a.id, "changes-requested"), 0);
        // Session b's trackers survive.
        assert_eq!(engine.attempts(&b.id, "ci-failed"), 1);
    }

    #[tokio::test]
    async fn auto_false_notify_fires_once_per_transition_and_does_not_escalate() {
        // Guards the `auto: false` + Notify edge case: a disabled
        // notify has no retry budget, so even `retries: Some(0)` cannot
        // trigger spurious escalations on it.
        let mut cfg = ReactionConfig::new(ReactionAction::Notify);
        cfg.auto = false;
        cfg.retries = Some(0); // would escalate if retry path ran
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), cfg);

        let (engine, _runtime, mut rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        // Two consecutive dispatches both return a normal (non-escalated)
        // Notify outcome — neither increments the tracker.
        for _ in 0..2 {
            let r = engine
                .dispatch(&session, "approved-and-green")
                .await
                .unwrap()
                .unwrap();
            assert!(r.success);
            assert!(!r.escalated);
            assert_eq!(r.action, ReactionAction::Notify);
        }
        assert_eq!(engine.attempts(&session.id, "approved-and-green"), 0);

        // No ReactionEscalated emitted on the channel.
        let events = drain(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::ReactionEscalated { .. })),
            "auto:false notify must not escalate, got {events:?}"
        );
    }

    #[tokio::test]
    async fn clear_tracker_resets_attempts_for_next_transition() {
        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix".into());
        config.retries = Some(1);
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, _runtime, _rx) = build(map);
        let session = fake_session("s1");

        // First attempt uses one retry.
        engine.dispatch(&session, "ci-failed").await.unwrap();
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 1);

        // CI goes green then red again → tracker cleared by lifecycle.
        engine.clear_tracker(&session.id, "ci-failed");
        assert_eq!(engine.attempts(&session.id, "ci-failed"), 0);

        // Fresh attempt sees a full budget.
        let r = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r.success);
        assert!(!r.escalated);
    }

    #[tokio::test]
    async fn trackers_are_scoped_per_reaction_key() {
        // An attempt on `ci-failed` must not consume budget for
        // `changes-requested` on the same session.
        let mut ci = ReactionConfig::new(ReactionAction::SendToAgent);
        ci.message = Some("fix ci".into());
        let mut cr = ReactionConfig::new(ReactionAction::SendToAgent);
        cr.message = Some("address review".into());

        let mut map = HashMap::new();
        map.insert("ci-failed".into(), ci);
        map.insert("changes-requested".into(), cr);

        let (engine, _runtime, _rx) = build(map);
        let session = fake_session("s1");

        engine.dispatch(&session, "ci-failed").await.unwrap();
        engine.dispatch(&session, "ci-failed").await.unwrap();
        engine
            .dispatch(&session, "changes-requested")
            .await
            .unwrap();

        assert_eq!(engine.attempts(&session.id, "ci-failed"), 2);
        assert_eq!(engine.attempts(&session.id, "changes-requested"), 1);
    }

    #[tokio::test]
    async fn trackers_are_scoped_per_session_id() {
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("fix".into());
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), cfg);

        let (engine, _runtime, _rx) = build(map);
        let a = fake_session("a");
        let b = fake_session("b");

        engine.dispatch(&a, "ci-failed").await.unwrap();
        engine.dispatch(&a, "ci-failed").await.unwrap();
        engine.dispatch(&b, "ci-failed").await.unwrap();

        assert_eq!(engine.attempts(&a.id, "ci-failed"), 2);
        assert_eq!(engine.attempts(&b.id, "ci-failed"), 1);
    }

    // ---------- Phase B: notifier registry integration ---------- //

    use crate::notifier::{tests::TestNotifier, NotificationRouting, NotifierRegistry};

    /// Build helper with a notifier registry attached. Same as `build()`
    /// but chains `.with_notifier_registry(...)`.
    fn build_with_notifier(
        cfg_map: HashMap<String, ReactionConfig>,
        registry: NotifierRegistry,
    ) -> (
        Arc<ReactionEngine>,
        Arc<RecordingRuntime>,
        broadcast::Receiver<OrchestratorEvent>,
    ) {
        let runtime = Arc::new(RecordingRuntime::new());
        let (tx, rx) = broadcast::channel(32);
        let engine = Arc::new(
            ReactionEngine::new(cfg_map, runtime.clone() as Arc<dyn Runtime>, tx)
                .with_notifier_registry(registry),
        );
        (engine, runtime, rx)
    }

    #[tokio::test]
    async fn dispatch_notify_without_registry_unchanged() {
        // Guard Phase D backwards compat: engines without a notifier
        // registry must keep emitting the event and returning success.
        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("approved".into());
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let (engine, _runtime, mut rx) = build(map);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();
        assert!(result.success);
        assert_eq!(result.action, ReactionAction::Notify);
        assert!(!result.escalated);
        assert_eq!(result.message.as_deref(), Some("approved"));

        let events = drain(&mut rx);
        assert_eq!(events.len(), 2, "got {events:?}");
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::Notify,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::UiNotification { notification } if notification.action == ReactionAction::Notify
        )));
    }

    #[tokio::test]
    async fn dispatch_notify_with_empty_routing_is_success() {
        // Registry attached but routing table empty → resolve returns
        // nothing → success true, no plugins called, event still emitted.
        let registry = NotifierRegistry::new(NotificationRouting::default());
        let config = ReactionConfig::new(ReactionAction::Notify);
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let (engine, _runtime, mut rx) = build_with_notifier(map, registry);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();
        assert!(result.success);
        assert!(!result.escalated);

        let events = drain(&mut rx);
        assert!(events
            .iter()
            .any(|e| matches!(e, OrchestratorEvent::ReactionTriggered { .. })));
    }

    #[tokio::test]
    async fn dispatch_notify_routes_to_single_plugin() {
        // One plugin registered for the priority, one notification
        // delivered. Assert the payload has the right fields.
        let mut routing = HashMap::new();
        routing.insert(EventPriority::Action, vec!["test".to_string()]);
        let (tn, received) = TestNotifier::new("test");
        let mut registry = NotifierRegistry::new(NotificationRouting::from_map(routing));
        registry.register("test", Arc::new(tn));

        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("PR merged".into());
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let (engine, _runtime, _rx) = build_with_notifier(map, registry);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();
        assert!(result.success);
        assert_eq!(result.message.as_deref(), Some("PR merged"));

        let payloads = received.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].reaction_key, "approved-and-green");
        assert_eq!(payloads[0].priority, EventPriority::Action);
        assert_eq!(payloads[0].body, "PR merged");
        assert!(!payloads[0].escalated);
    }

    #[tokio::test]
    async fn dispatch_notify_fan_out_reports_partial_failure() {
        // Two plugins: one succeeds, one fails. The outcome must be
        // success = false and message must name the failing plugin.
        use crate::notifier::NotifierError;

        struct FailNotifier;

        #[async_trait::async_trait]
        impl crate::notifier::Notifier for FailNotifier {
            fn name(&self) -> &str {
                "fail"
            }
            async fn send(
                &self,
                _payload: &NotificationPayload,
            ) -> std::result::Result<(), NotifierError> {
                Err(NotifierError::Unavailable("offline".into()))
            }
        }

        let mut routing = HashMap::new();
        routing.insert(
            EventPriority::Urgent,
            vec!["ok-plugin".to_string(), "fail".to_string()],
        );
        let (tn, received) = TestNotifier::new("ok-plugin");
        let mut registry = NotifierRegistry::new(NotificationRouting::from_map(routing));
        registry.register("ok-plugin", Arc::new(tn));
        registry.register("fail", Arc::new(FailNotifier));

        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.message = Some("something".into());
        let mut map = HashMap::new();
        // Default priority for `agent-stuck` is `urgent` (matches
        // `default_priority_for_reaction_key`).
        map.insert("agent-stuck".into(), config);

        let (engine, _runtime, _rx) = build_with_notifier(map, registry);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Stuck;

        let result = engine
            .dispatch(&session, "agent-stuck")
            .await
            .unwrap()
            .unwrap();
        assert!(!result.success);
        let msg = result.message.unwrap();
        assert!(
            msg.contains("fail"),
            "error message should name the failing notifier, got: {msg}"
        );

        // Successful plugin still received the payload.
        let payloads = received.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].reaction_key, "agent-stuck");
    }

    #[tokio::test]
    async fn escalation_routes_through_notifier_registry() {
        // When retries exhaust and the engine escalates to Notify, the
        // registry is used to fan out the escalated notification.
        let mut routing = HashMap::new();
        routing.insert(EventPriority::Urgent, vec!["test".to_string()]);
        let (tn, received) = TestNotifier::new("test");
        let mut registry = NotifierRegistry::new(NotificationRouting::from_map(routing));
        registry.register("test", Arc::new(tn));

        let mut config = ReactionConfig::new(ReactionAction::SendToAgent);
        config.message = Some("fix ci".into());
        config.retries = Some(1);
        let mut map = HashMap::new();
        map.insert("ci-failed".into(), config);

        let (engine, _runtime, mut rx) = build_with_notifier(map, registry);
        let session = fake_session("s1");

        // 1st attempt: SendToAgent (no escalation).
        let r1 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(!r1.escalated);

        // 2nd attempt: retries exhausted → escalation to Notify.
        let r2 = engine
            .dispatch(&session, "ci-failed")
            .await
            .unwrap()
            .unwrap();
        assert!(r2.escalated);
        assert_eq!(r2.action, ReactionAction::Notify);

        // Notifier received an escalated payload.
        let payloads = received.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert!(payloads[0].escalated);
        assert_eq!(payloads[0].reaction_key, "ci-failed");
        assert_eq!(payloads[0].priority, EventPriority::Urgent);

        // Events: SendToAgent trigger, then ReactionEscalated + ReactionTriggered(Notify).
        let events = drain(&mut rx);
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::ReactionEscalated {
                reaction_key,
                ..
            } if reaction_key == "ci-failed"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::Notify,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn auto_false_notify_still_routes_through_registry() {
        // `auto: false` + action: Notify → bypass retry budget but
        // still fan out through the registry.
        let mut routing = HashMap::new();
        routing.insert(EventPriority::Action, vec!["test".to_string()]);
        let (tn, received) = TestNotifier::new("test");
        let mut registry = NotifierRegistry::new(NotificationRouting::from_map(routing));
        registry.register("test", Arc::new(tn));

        let mut config = ReactionConfig::new(ReactionAction::Notify);
        config.auto = false;
        config.message = Some("fyi".into());
        let mut map = HashMap::new();
        map.insert("approved-and-green".into(), config);

        let (engine, _runtime, _rx) = build_with_notifier(map, registry);
        let mut session = fake_session("s1");
        session.status = SessionStatus::Mergeable;

        let result = engine
            .dispatch(&session, "approved-and-green")
            .await
            .unwrap()
            .unwrap();
        assert!(result.success);
        assert!(!result.escalated);

        let payloads = received.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].body, "fyi");
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
