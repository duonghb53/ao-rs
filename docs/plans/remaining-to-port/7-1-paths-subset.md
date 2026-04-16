# 7.1 paths subset

Status: planned

## Why

`paths.rs` implements only a subset of TS `paths.ts` (Slice 1 scope). If additional TS paths are needed (logs, caches, observability), the Rust path map must expand.

## Current state (ao-rs)

- `crates/ao-core/src/paths.rs`: implements `~/.ao-rs` base dir, sessions dir, pidfile path.
- Comment: “equivalent of `packages/core/src/paths.ts`, scoped down to Slice 1.”

## Target behavior (ao-ts parity)

- Add any missing path helpers required by newly ported features (not everything at once).

## Proposed approach

1. Inventory TS paths used by features you plan to port next (notifications, observability, plugin registry).
2. Add corresponding functions in `paths.rs` (pure, no side effects).
3. Update callers to use `paths` instead of hardcoding.

## Files to change

- `crates/ao-core/src/paths.rs`
- Call sites that currently hardcode paths (as found via grep).

## Acceptance criteria

- Newly required on-disk locations have a single source of truth in `paths.rs`.

## Test plan

- Unit tests for each new path helper (stable formatting).

## Risks / notes

- Keep `paths.rs` minimal; only add what’s needed by real features.

