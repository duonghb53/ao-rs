# 2.4 update command

Status: planned

## Why

ao-ts can self-update via `ao update`. ao-rs is typically installed via cargo (different distribution model) and has no update command.

## Current state (ao-rs)

- No `ao-rs update` command.
- Releases/build story is Rust-native (cargo install, homebrew, etc.).

## Target behavior (ao-ts parity)

`ao update [--skip-smoke] [--smoke-only] [--check]`

## Proposed approach

1. Start with `--check` only:
   - Compare local version vs latest Git tag / GitHub release (using `gh release view` or HTTP).
2. Implement upgrade mechanism based on chosen distribution:
   - If installed via `cargo install`, run `cargo install ao-cli --locked` (or instruct user).
   - If installed via Homebrew, run `brew upgrade ao-rs`.
3. Add optional smoke test runner that calls `docs/SMOKE.md` steps.

## Files to change

- `crates/ao-cli/src/cli/args.rs` and routing
- `crates/ao-cli/src/commands/update.rs` (new)
- `docs/SMOKE.md` (reference only; no changes required)

## Acceptance criteria

- `ao-rs update --check` reports whether an update is available.
- `ao-rs update` performs upgrade for at least one supported install method or prints a clear instruction.

## Test plan

- Unit tests for version parsing and “latest version” resolution logic (mocked).
- Keep actual upgrade execution behind a thin adapter to avoid tests mutating system state.

## Risks / notes

- Multi-install-method support can be tricky; document supported paths clearly.

