---
phase: requirements
title: Notifier routing — Slice 3 requirements
description: Turn ReactionAction::Notify into real fan-out to configurable notifier channels
---

# Notifier routing — Slice 3 requirements

## Problem Statement

Today, when a configured reaction fires with `action: notify`, the
reaction engine's `dispatch_notify` just emits a `ReactionTriggered`
event onto the `tokio::sync::broadcast` bus and returns success. No
message ever leaves the process. The only "subscriber" wired up is the
`ao-rs watch` CLI, which prints one row per event — useful for
observation, not useful as an actual notification channel.

This is a solo-learning port that mirrors TS `ao`'s `Notifier` plugin
contract. TS routes notifications through a priority table
(`notificationRouting: Record<EventPriority, NotifierName[]>`), and
ships stdout / desktop / ntfy / slack / email / discord plugins. Slice 3
ports the contract and enough plugins to make `notify` reactions
actually reach a human.

Affected: anyone running `ao-rs` with any reaction configured with
`action: notify` — currently only `approved-and-green` in typical
configs, but `ci-failed` / `agent-stuck` / `merge-conflict` escalate to
`Notify` after retries are exhausted, so the surface is larger than
"just one rule".

## Goals & Objectives

### Primary goals (must ship before Slice 3 is declared done)

1. **`Notifier` trait in `ao-core`** — single async method `send` that
   takes a typed `NotificationPayload` and returns `Result<(),
   NotifierError>`. Plugins are trait objects, loaded at compile time,
   same as the existing SCM/Tracker/Runtime/Agent/Workspace traits.
2. **`NotifierRegistry`** — map from notifier name (`"stdout"`,
   `"ntfy"`, …) to `Arc<dyn Notifier>`, owned by the lifecycle layer,
   attached to `ReactionEngine` via a `with_notifier_registry` builder.
3. **Priority-based routing** — config section
   `notification-routing:` that maps each `EventPriority`
   (`urgent | action | warning | info`) to an ordered list of notifier
   names. Missing priority → no notify sent, log a warn-once. Empty
   list → same.
4. **`ReactionEngine::dispatch_notify` wired through the registry** —
   on `Notify` dispatch, resolve priority → names → trait objects, call
   `send` on each, aggregate results into the existing `ReactionOutcome`
   shape. Existing `ReactionTriggered` event still emitted so the CLI
   subscriber keeps working.
5. **At least two notifier plugins shipped**: `stdout` (always present,
   the fallback when no routing is configured) and one HTTP-based
   plugin (`ntfy` is simplest — single HTTP POST, no auth setup).
6. **Integration test** that drives the full loop: reaction fires →
   engine routes → test notifier receives the payload.

### Secondary goals (nice-to-have, may slip)

- Desktop notification plugin via `notify-rust` (crate-based, not
  shell-out, single exception to principle #1 because `osascript` /
  `notify-send` divergence is painful).
- Slack webhook plugin.
- Notifier error backoff — today, if `send` returns `Err`, we log and
  move on. TS has a retry ladder. We can port it later if needed.

### Non-goals (explicitly out of scope)

- **Template engine.** TS has Handlebars for message bodies. We stick
  with plain `format!` / string interpolation inside each plugin; the
  message text a user writes in the reaction config is passed through
  verbatim.
- **Rate limiting / deduplication.** TS has per-notifier rate limits.
  We don't port them. At N≤30 sessions with per-reaction retry budgets
  already capping volume, the risk is negligible.
- **Dynamic plugin loading.** Notifier plugins are compile-time trait
  objects, consistent with the rest of ao-rs.
- **Feedback-report routing** (TS's `bug_report` / `improvement`).
- **Plugin-marketplace install flow.**
- **Hot-reload of routing table.** Config change needs lifecycle
  restart, same as the reaction table today.

## User Stories & Use Cases

**Primary user: the project author running `ao-rs watch` on their own
box, logged into Claude Code sessions.**

- As the user, when `ci-failed` exhausts its `SendToAgent` retries and
  escalates to `Notify`, I want a real notification (stdout at minimum,
  ntfy / desktop / slack when configured) so I can actually be pulled
  in. Currently the event is silently emitted to a channel I have to
  be actively subscribed to.
- As the user, when `approved-and-green` fires for a PR, I want a
  one-line notification with the session id + PR number + merge commit
  hash so I can verify the right thing got merged without polling.
- As the user, when I set `notification-routing: { urgent: [stdout,
  ntfy], warning: [stdout] }` in my config, I want urgent notifications
  to fan out to both channels and warning ones to just stdout.
- As the user running without a `notification-routing:` config at all,
  I want the engine to default to routing *everything* to `stdout`.
  Zero-config ≠ silent.
- As a plugin author (me, in a learning context), I want the
  `Notifier` trait small enough that writing a new plugin takes one
  file, one `impl`, one `send` method.

### Edge cases to cover in Slice 3

- **No registry attached.** Existing tests wire a `ReactionEngine`
  without notifier integration. `dispatch_notify` must still emit the
  event and return success. Attaching a registry is opt-in.
- **Plugin returns `Err`.** Log a warn, record `success = false` in
  the outcome, do NOT propagate the error to the lifecycle loop (a
  flaky notifier must not wedge the polling tick).
- **Multiple notifiers for one priority.** All of them are attempted;
  the outcome is `success = all_ok`. Partial success (one OK, one
  Err) is still recorded as `success = false` with a message listing
  which notifier failed.
- **Unknown notifier name in routing table.** `NotifierRegistry::get`
  returns `None`; the engine logs a warn-once (same `Mutex<HashSet>`
  pattern used by `warn_once_parse_failure` in Phase H) and skips
  that name.
- **Priority missing from routing table.** Warn-once with the
  priority name, drop the notification. No fallback to stdout in this
  case — the user configured a routing table deliberately, and silently
  routing elsewhere would be surprising.

## Success Criteria

- `cargo test -p ao-core` passes with new notifier-path tests.
- `cargo test --workspace` passes — every plugin crate builds and its
  own unit tests pass.
- `cargo fmt --all -- --check` + `cargo clippy --all-targets -- -D
  warnings` both clean.
- `rust-reviewer` subagent gate passes for each phase.
- Integration test: wire a test notifier (records received payloads),
  fire a reaction with `action: notify, priority: warning`, assert
  exactly one payload landed with the right fields.
- Running `ao-rs watch` against a session that crosses an escalation
  boundary prints a real notification to stdout (manual smoke test,
  not required for green CI but required for "done").

## Constraints & Assumptions

- **Shell-out vs. crate.** Principle #1 prefers shell-out. `stdout` is
  trivially "print to stdout via `println!` / `tracing`". `ntfy` is
  `POST https://ntfy.sh/<topic>` which we'll do with `reqwest`
  (already a workspace dep? — check in design). `desktop` wants
  `notify-rust` — explicit exception, documented in design.
- **`async_trait`.** `Notifier::send` is async because `reqwest` is,
  so the trait uses `#[async_trait::async_trait]` matching the
  existing Scm/Tracker/Runtime/Agent/Workspace pattern.
- **Event shape stability.** `OrchestratorEvent::ReactionTriggered`
  is already consumed by the CLI and by lifecycle tests. Its fields
  (`id`, `reaction_key`, `action`) must not change in Slice 3. New
  context goes into the `NotificationPayload` struct that the trait
  consumes — the event stays narrow.
- **Config compatibility.** The existing `reactions:` section must
  round-trip unchanged. Adding `notification-routing:` is additive.

## Questions & Open Items

- **Should plugins be `Send + Sync`?** Yes, because the registry
  holds `Arc<dyn Notifier>` and the engine runs inside a `tokio::spawn`
  task. Match existing trait bounds in `traits.rs`.
- **Should `NotifierError` be `thiserror`-backed or a plain struct?**
  Match the `AoError` convention in `ao_core::error`. Leaning thiserror.
- **Default routing fallback.** Zero-config → route everything to
  stdout, OR zero-config → emit event only, no stdout? Design picks
  stdout because the goal of Slice 3 is "stop being silent".
- **Where does `NotificationPayload` carry the message body from?**
  `ReactionConfig.message` for non-escalated notifies. For escalated
  ones, an engine-supplied string like `"ci-failed escalated after 3
  attempts"`. Decision in design.
