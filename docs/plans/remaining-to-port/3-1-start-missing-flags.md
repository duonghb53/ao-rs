# 3.1 start missing flags

Status: planned

## Why

ao-ts `start` supports toggles for dashboard/orchestrator plus rebuild/dev/interactive flows. ao-rs `start` is currently simpler (`--run`, `--port`, `--interval`, `--open`).

## Current state (ao-rs)

- `crates/ao-cli/src/commands/start.rs` (and args in `crates/ao-cli/src/cli/args.rs`)
- No equivalents for:
  - `--no-dashboard`, `--no-orchestrator`
  - `--rebuild`, `--dev`, `--interactive`

## Target behavior (ao-ts parity)

- Support controlling components independently:
  - start dashboard only / orchestrator only / both
- Support “rebuild” (meaning depends on implementation: rebuild UI, re-generate config, etc.)
- Support dev/interactive (likely affects logging + prompts)

## Proposed approach

1. Define exact semantics for each flag in ao-rs:
   - `--no-dashboard`: skip starting HTTP server
   - `--no-orchestrator`: skip starting lifecycle loop
   - `--rebuild`: rebuild UI assets / reset caches (choose scope)
   - `--dev`: enable debug logging / hot reload (if applicable)
   - `--interactive`: prompt user for missing config fields (if applicable)
2. Implement flags in Clap args + start command wiring.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/start.rs`
- (Optional) `crates/ao-cli/src/commands/dashboard.rs` and `watch.rs` if start delegates to them

## Acceptance criteria

- `ao-rs start --no-dashboard` starts lifecycle only.
- `ao-rs start --no-orchestrator` starts dashboard only.
- Flags have clear help text and do not change defaults unexpectedly.

## Test plan

- CLI arg parsing tests for mutual exclusivity and defaults.
- Integration test verifying the chosen components start (can be smoke-level).

## Risks / notes

- `--rebuild`/`--dev`/`--interactive` need precise definitions in Rust context; avoid adding flags with ambiguous behavior.

