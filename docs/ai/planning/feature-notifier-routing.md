---
phase: planning
title: Notifier routing — Slice 3 planning
description: Phase breakdown for Slice 3, task list for Phase A
---

# Notifier routing — Slice 3 planning

## Milestones

- [ ] **Slice 3 Phase A — types + registry + config.** `Notifier` trait,
      `NotificationPayload`, `NotifierError`, `NotifierRegistry`,
      `NotificationRouting` config parse. No engine wiring, no plugin
      crates. One PR, one focused commit.
- [ ] **Slice 3 Phase B — engine integration.**
      `ReactionEngine::dispatch_notify` resolves through the registry
      and calls `Notifier::send` for each target. No plugin crates yet.
      Uses a test-only `TestNotifier` from Phase A for coverage.
- [ ] **Slice 3 Phase C — stdout plugin.** First real plugin crate
      `ao-plugin-notifier-stdout`, wired in `ao-cli`. Zero-config
      default routing sends everything to stdout. First end-to-end
      notification path.
- [ ] **Slice 3 Phase D — ntfy plugin.** Second plugin crate
      `ao-plugin-notifier-ntfy`, HTTP POST, optional env-var topic.
- [ ] **Slice 3 Phase E+ — future plugins.** Desktop / slack / email /
      discord, scoped when we get there. Not planned here.

## Task Breakdown — Phase A only

### Task 1: Module scaffolding

- [ ] 1.1: Create `crates/ao-core/src/notifier.rs` with module
      docstring explaining the Phase A / B / C split and cross-
      referencing `docs/ai/design/feature-notifier-routing.md`.
- [ ] 1.2: Declare `pub mod notifier;` in `crates/ao-core/src/lib.rs`
      and add a re-export block for the public types.
- [ ] 1.3: Confirm `thiserror` and `async_trait` are already workspace
      dependencies. If not, add to `ao-core`'s `Cargo.toml` with the
      same pinned versions used elsewhere in the workspace.

### Task 2: Core types

- [ ] 2.1: Define `NotificationPayload` struct with the fields from
      the design doc. Derive `Debug`, `Clone`. No `Serialize` (not
      needed — payload never hits disk).
- [ ] 2.2: Define `NotifierError` enum with five variants (`Io`,
      `Config`, `Service`, `Timeout`, `Unavailable`). `thiserror`-
      derived.
- [ ] 2.3: Define `Notifier` trait with `fn name(&self) -> &str` and
      `async fn send(&self, &NotificationPayload) -> Result<(),
      NotifierError>`. `async_trait`, `Send + Sync`. Comprehensive
      doc comment covering: plugin-author responsibilities, timeout
      convention, "must never panic", and "errors are logged, never
      propagate to the lifecycle loop".

### Task 3: Routing config

- [ ] 3.1: Define `NotificationRouting` struct wrapping
      `HashMap<EventPriority, Vec<String>>` with
      `#[serde(transparent)]`. Derive `Debug`, `Clone`, `Default`,
      `Serialize`, `Deserialize`, `PartialEq`, `Eq`.
- [ ] 3.2: Write serde round-trip test using the YAML example from
      the design doc.
- [ ] 3.3: Write `NotificationRouting::default()` is-empty test.
- [ ] 3.4: Write test confirming unknown priority names in YAML
      produce a parse error (we want strict priority matching).
- [ ] 3.5: Extend `AoConfig` in `crates/ao-core/src/config.rs` with
      a `notification_routing` field. `#[serde(default, rename =
      "notification_routing", alias = "notification-routing")]`
      matching the `escalate_after` alias pattern from Phase H.
- [ ] 3.6: Add two `config.rs` tests: (a) a config with only
      `notification-routing:` parses; (b) a config with both
      `reactions:` and `notification-routing:` parses.

### Task 4: Registry

- [ ] 4.1: Define `NotifierRegistry` struct with `plugins`,
      `routing`, and `warned` fields.
- [ ] 4.2: `NotifierRegistry::new(routing)` constructor.
- [ ] 4.3: `NotifierRegistry::register(name, plugin)` method.
      Overwrites existing entries for the same name — document this
      behaviour (tests could rely on it).
- [ ] 4.4: `NotifierRegistry::resolve(priority)` returning
      `Vec<(String, Arc<dyn Notifier>)>`. Handles: priority absent
      from routing → warn-once + empty vec; priority present with
      empty vec → warn-once + empty vec; name present but plugin
      absent → warn-once per (priority, name) pair + skip that name.
- [ ] 4.5: Private `warn_once` helper acquiring `warned` lock in a
      narrow scope and releasing before any `tracing::warn!` macro
      expansion (match the lock discipline from `warn_once_parse_failure`).

### Task 5: Test-only mock notifier

- [ ] 5.1: In `#[cfg(test)] mod tests` inside `notifier.rs`, define
      `TestNotifier` that stores received payloads in an
      `Arc<Mutex<Vec<NotificationPayload>>>`. Expose a constructor
      returning the notifier and a handle to the shared vec.
- [ ] 5.2: One test drives `TestNotifier::send` directly and asserts
      the payload was recorded.
- [ ] 5.3: Keep `TestNotifier` `pub(crate)` so Phase B's
      `reaction_engine.rs` tests can import it via
      `use crate::notifier::tests::TestNotifier`. If cross-module
      test visibility turns out to be awkward in Rust, promote to
      `pub(crate) struct` at the module root under `#[cfg(test)]`.

### Task 6: Registry tests

- [ ] 6.1: Empty-routing `resolve` returns empty.
- [ ] 6.2: Populated-routing `resolve` returns only registered names.
- [ ] 6.3: Missing-plugin path emits exactly one warn per
      (priority, name) pair across multiple resolve calls.
- [ ] 6.4: Registering the same name twice keeps only the last
      instance.
- [ ] 6.5: Resolve for a priority with an empty vec warns once and
      returns empty.

### Task 7: Gate + commit

- [ ] 7.1: `cargo fmt --all` and commit any reformatting.
- [ ] 7.2: `cargo clippy --all-targets -- -D warnings` clean.
- [ ] 7.3: `cargo test -p ao-core` — all new tests plus pre-Phase-A
      tests still green (target: 157 + ~8 new = ~165 tests).
- [ ] 7.4: Launch `rust-reviewer` subagent for review. Address any
      nits before committing.
- [ ] 7.5: Update `docs/architecture.md` — add `notifier.rs` to the
      reading order list, open question for "notifier plugin
      lifecycle" if any arises from review.
- [ ] 7.6: Create commit `feat(core): Slice 3 Phase A — Notifier
      trait, registry, routing config`. Use HEREDOC for multi-line
      body.
- [ ] 7.7: Push branch to origin, create PR, merge into main after
      user confirmation.

## Dependencies

- **Task 3 depends on Task 2** (routing references `EventPriority`
  which the existing `reactions.rs` already exports — no blocker).
- **Task 4 depends on Tasks 2 and 3** (registry stores `Arc<dyn
  Notifier>` and holds a `NotificationRouting`).
- **Task 5 depends on Task 2** (test notifier implements `Notifier`).
- **Task 6 depends on Tasks 4 and 5** (registry tests use the test
  notifier).
- **Task 7 depends on all earlier tasks.**

No external dependencies. `async_trait` and `thiserror` are already
pulled by the workspace.

## Risks & Mitigation

- **Risk: `pub(crate) use` of `TestNotifier` across modules.** Rust's
  test visibility rules sometimes require the test helper at the
  crate root under `#[cfg(test)]`. Mitigation: if Phase B's tests
  can't see `TestNotifier` via the expected import path, move it to
  a `pub(crate)` item at crate root inside a `#[cfg(test)] mod
  test_util` block. Not a Phase A blocker — surfaces in Phase B if at all.

- **Risk: `NotifierRegistry::warned` lock held across `tracing::warn!`
  panic path.** Same risk the Phase H `warn_once_parse_failure` faced.
  Mitigation: same discipline — acquire the lock, insert into the set,
  drop the lock, *then* call the macro. Covered by reviewer gate.

- **Risk: `NotificationRouting`'s strict priority parsing rejects
  valid configs.** Serde's `Deserialize for HashMap<EventPriority, _>`
  rejects unknown keys because `EventPriority` is an exhaustive enum.
  Mitigation: add a `rejects_unknown_priority` test (Task 3.4) so
  Phase A locks in the behaviour and a future change can't silently
  loosen it.

- **Risk: Phase A ships unused code.** The whole point of Phase A is
  types without engine wiring, so the `NotifierRegistry` is not called
  from any production code in this PR. Mitigation: clippy's
  `dead_code` only fires on `pub(crate)`, and all Phase A public
  items are `pub` (exported). Verified by Task 7.2.

## Timeline & Estimates

Solo learning port — no external timeline. Ordered by implementation
dependency; each task is small enough to live in one focused commit
range (though the whole of Phase A commits as one).

## Resources Needed

- `ao-core`, its `Cargo.toml`, existing `config.rs`, existing
  `reactions.rs` (for `EventPriority`).
- `async_trait`, `thiserror`, `tracing`, `serde`, `serde_yaml` — all
  already in the workspace.
- `rust-reviewer` subagent for the Task 7.4 gate.
