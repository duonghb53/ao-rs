//! Action executor implementations: `dispatch_send_to_agent`,
//! `dispatch_notify`, `dispatch_auto_merge`.

use super::{resolve::build_payload, resolve::resolve_priority, ReactionEngine};
use crate::{
    events::{OrchestratorEvent, UiNotification},
    notifier::NotifierError,
    reactions::{EventPriority, ReactionAction, ReactionConfig, ReactionOutcome},
    types::Session,
};

impl ReactionEngine {
    pub(super) async fn dispatch_send_to_agent(
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
    pub(super) async fn dispatch_notify(
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
    pub(super) async fn dispatch_auto_merge(
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
}
