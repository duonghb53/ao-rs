# 3.7 session remap command

Status: planned

## Why

ao-ts supports `session remap` to re-bind a session to a different workspace/runtime mapping. ao-rs lacks this recovery tool.

## Current state (ao-rs)

- Session restore exists (`ao-rs session restore`).
- No CLI command to remap metadata like workspace path or runtime handle (beyond manual editing).

## Target behavior (ao-ts parity)

`ao session remap <session> [-f, --force]`

## Proposed approach

1. Define “remap” scope in ao-rs:
   - Update workspace path and/or runtime handle fields on session.
2. Implement CLI flow:
   - Prompt for new values (or accept flags)
   - Validate paths exist unless `--force`
3. Persist updated session to disk.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/session/remap.rs` (new)
- `crates/ao-core/src/session_manager.rs` (update session)

## Acceptance criteria

- Remapped session can be attached/restored after remap.
- Without `--force`, invalid paths are rejected.

## Test plan

- Unit tests for remap validation and persistence.

## Risks / notes

- Remap is footgun-prone; ensure clear prompts and show diff before writing.

