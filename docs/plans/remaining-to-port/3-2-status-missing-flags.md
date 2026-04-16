# 3.2 status missing flags

Status: planned

## Why

ao-ts `status` supports `--json`, `--watch`, and `--interval`. ao-rs has `status` (table output) and `watch` (event stream), but lacks JSON + watchable status snapshots.

## Current state (ao-rs)

- `ao-rs status` supports `--project`, `--pr`, `--cost`, `--all` (behavior varies by version).
- No `--json` output.
- No `status --watch` mode.

## Target behavior (ao-ts parity)

- `ao status --json`: machine-readable status list
- `ao status --watch --interval <secs>`: repeated snapshots

## Proposed approach

1. Add `--json` to `ao-rs status`:
   - Output an array of sessions (id, project, status, timestamps, pr fields if `--pr`, cost if `--cost`).
2. Add `--watch` + `--interval`:
   - Loop printing JSON (or human table) every N seconds.
   - Prefer stable JSON format; keep human output unchanged when not `--json`.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/status.rs`

## Acceptance criteria

- `ao-rs status --json` produces valid JSON to stdout.
- `ao-rs status --watch --interval 2` refreshes until interrupted.

## Test plan

- Unit test for JSON serialization (parse output as JSON).
- CLI test for `--interval` parsing and that `--watch` loops at least twice (can be time-bounded).

## Risks / notes

- Avoid breaking existing `status` output formatting.

