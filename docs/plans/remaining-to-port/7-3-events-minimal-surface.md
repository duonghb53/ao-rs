# 7.3 events minimal surface

Status: planned

## Why

`events.rs` is intentionally minimal (Phase C). If ao-ts has a richer event bus, new features may need additional event variants for observability and UI.

## Current state (ao-rs)

- `crates/ao-core/src/events.rs` defines:
  - lifecycle events (spawn/status/activity/terminated)
  - tick errors
  - reaction triggered/escalated
  - UI notifications
- No TS file path is referenced in this module.

## Target behavior (parity/maturity)

- Add event variants only as needed by new UI/CLI features (avoid speculative expansion).

## Proposed approach

1. Identify missing events by consumer:
   - dashboard UI needs X
   - `watch` needs Y
2. Add new variants with stable serde tags.
3. Ensure backwards compatibility for persisted logs (if any).

## Files to change

- `crates/ao-core/src/events.rs`
- Any producers (lifecycle, reaction engine)
- Any consumers (cli printing, dashboard API)

## Acceptance criteria

- New event variants appear in `ao-rs watch` and dashboard streams as expected.

## Test plan

- Unit tests for serde round-trip tags and for producer emission in key scenarios.

## Risks / notes

- `broadcast` drops events on lag; consumers should snapshot sessions on startup (already documented in lifecycle).

