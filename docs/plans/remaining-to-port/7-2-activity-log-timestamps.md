# 7.2 activity log timestamps

Status: planned

## Why

`activity_log.rs` staleness logic parses timestamps only when they are numeric milliseconds. If activity logs use ISO timestamps, staleness checks won’t work, reducing stuck/needs-input detection quality.

## Current state (ao-rs)

- `crates/ao-core/src/activity_log.rs`
  - “TS `activity-log.ts`-inspired”
  - `chrono_like_parse` only accepts `u128` ms strings

## Target behavior (ao-ts parity / robustness)

- Parse RFC3339 timestamps (or at least common ISO forms) for staleness calculations.

## Proposed approach

1. Decide dependency strategy:
   - Add `time` crate (preferred lightweight) or `chrono` (heavier) for parsing.
2. Update parser to accept:
   - numeric ms
   - RFC3339 strings
3. Ensure staleness math uses consistent timezone/epoch behavior.

## Files to change

- `crates/ao-core/src/activity_log.rs`
- `Cargo.toml` for dependency (if needed)

## Acceptance criteria

- Both numeric ms and RFC3339 timestamps are parsed successfully.
- Staleness check produces correct results on representative samples.

## Test plan

- Unit tests for timestamp parsing with multiple formats.

## Risks / notes

- Adding time parsing deps impacts compile times; keep scope narrow.

