# 2.1 stop command

Status: planned

## Why

ao-ts has a supervisor-style `stop` to stop orchestrator/dashboard processes. ao-rs has session-scoped `kill` and `cleanup`, but no lifecycle service stop command.

## Current state (ao-rs)

- `ao-rs watch` uses a PID file lock:
  - `crates/ao-core/src/lockfile.rs` (`~/.ao-rs/lifecycle.pid`)
- `ao-rs dashboard`/`start --run` also spawn lifecycle loops but do not expose a dedicated stop interface.

## Target behavior (ao-ts parity)

`ao stop [project]` with options like:
- `--all` stop all watchers/services
- `--purge-session` (if applicable) remove supervisor-managed state

## Proposed approach

1. Implement `ao-rs stop` CLI command that:
   - Reads the pidfile (`paths::lifecycle_pid_file()`).
   - Sends a termination signal to that process (SIGTERM) and waits briefly.
   - Removes stale lock if process is not alive.
2. Decide scope:
   - Stop only the lifecycle watcher (`watch`) and dashboard’s internal lifecycle (same pid lock).
   - Leave sessions intact; session cleanup remains via `cleanup`.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
  - Add `Stop` command + flags.
- `crates/ao-cli/src/commands/stop.rs` (new)
  - Implement stop logic.
- `crates/ao-cli/src/main.rs` or command router
  - Wire command handler.

## Acceptance criteria

- Running `ao-rs stop` terminates a running `ao-rs watch`/`ao-rs dashboard` service loop.
- When no service is running, `ao-rs stop` exits cleanly with a helpful message.

## Test plan

- Unit tests for pidfile parsing and “stale pidfile” behavior.
- Optional integration test that spawns a child process running a minimal lifecycle loop and verifies stop terminates it.

## Risks / notes

- Signal handling differs across OS; focus on macOS/Linux first.
- If multiple services exist in future (separate pidfiles), extend with `--all` and per-service keys.

