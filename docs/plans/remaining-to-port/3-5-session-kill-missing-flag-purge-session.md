# 3.5 session kill missing flag purge-session

Status: planned

## Why

ao-ts supports `--purge-session` to remove persisted session records. ao-rs `kill` stops runtime and archives, but does not expose an explicit purge option.

## Current state (ao-rs)

- `ao-rs kill <session>` exists.
- Session persistence is in `~/.ao-rs/sessions/` via `SessionManager`.
- Archive semantics exist (e.g. `.archive/`).

## Target behavior (ao-ts parity)

`ao session kill <session> --purge-session` removes session record entirely (no archive).

## Proposed approach

1. Add `--purge` (or `--purge-session`) to `kill`:
   - When set, delete persisted session YAML instead of archiving.
2. Keep default behavior unchanged: archive by default.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/kill.rs`
- `crates/ao-core/src/session_manager.rs` (if new delete API is needed)

## Acceptance criteria

- `ao-rs kill <id> --purge-session` removes the session record from disk.
- Without flag, existing archive behavior remains unchanged.

## Test plan

- Unit test using temp session directory to verify archive vs purge behavior.

## Risks / notes

- Purge is destructive; require an explicit flag and print a warning.

