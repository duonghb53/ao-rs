# Issue #108 — 7.3 events minimal surface

Tracking: https://github.com/duonghb53/ao-rs/issues/108

## Decision

The `events.rs` surface is intentionally minimal. The issue warns against
speculative expansion: "Add event variants only as needed by new UI/CLI
features."

Research against ao-ts (`~/study/agent-orchestrator`) shows its richer
`EventType` (28 variants) is largely **dead code** — most variants are
defined but never emitted, and the web/CLI consumers do not match on raw
event types (they re-render from periodic session snapshots).

ao-rs already models the PR/CI/merge lifecycle via `SessionStatus`, which
flows through `OrchestratorEvent::StatusChanged`. Adding dedicated
`PrOpened`/`CiFailed`/`MergeReady`/… variants would duplicate that channel
without a consumer that needs the distinction.

**The one real consumer-visible gap** in the current surface: `Spawned`
fires on *first observation* of any session — including sessions that
already existed on disk when the lifecycle loop started. A user running
`ao-rs watch` sees a flood of "spawned" rows for pre-existing sessions,
which is misleading. Dashboard SSE subscribers have the same problem.

## Scope

Add one variant:

- `SessionRestored { id, project_id, status }` — emitted on the first
  observation of a session whose `created_at` predates the lifecycle
  loop's startup timestamp. Replaces `Spawned` for that case.

`Spawned` retains its meaning: a session that *appeared* while the loop
was already running (i.e. was created after startup).

## Implementation

### `crates/ao-core/src/events.rs`
- Add `SessionRestored` variant with stable serde tag `session_restored`
  (snake_case rename matches the existing enum convention).

### `crates/ao-core/src/lifecycle.rs`
- Add `startup_ms: AtomicU64` to `LifecycleManager`. Initialised to `0`
  (meaning "not yet started"). Set in `run_loop` before the first tick.
- In `tick()`, when `seen.insert()` returns true:
  - If `startup_ms != 0 && session.created_at < startup_ms` →
    emit `SessionRestored`.
  - Otherwise → emit `Spawned` (existing behaviour).
- Tests that call `tick(&mut seen)` directly without `run_loop` continue
  to see `Spawned` because `startup_ms` stays at `0` — backwards
  compatible.

### `crates/ao-cli/src/cli/printing.rs`
- Add match arm for `SessionRestored`, mirroring `Spawned`'s row shape
  but labelling the event `session_restored`.

### Consumers that auto-benefit
- Dashboard SSE (`crates/ao-dashboard/src/sse.rs`) — passes raw events
  through, no code change.
- Notifier registry / reaction engine — not interested in this variant;
  no change.

## Race condition note

A session created *between* `run_loop` startup and the first tick is
correctly classified by the `created_at < startup_ms` check — its
`created_at` will be greater than `startup_ms`, so it emits `Spawned`,
not `SessionRestored`.

## Tests

1. Serde round-trip for all `OrchestratorEvent` variants (including the
   new one) — covers stable tags.
2. Lifecycle producer test:
   - Seed two sessions: one with `created_at < startup_ms` (restored),
     one with `created_at > startup_ms` (new). Drive one tick. Assert
     one `SessionRestored` + one `Spawned`.

## Acceptance criteria

- `ao-rs watch` renders `session_restored` rows for pre-existing
  sessions instead of `spawned`.
- Dashboard SSE stream carries the new variant unchanged.
- `cargo t` green; `cargo clippy` clean; `cargo fmt` applied.
