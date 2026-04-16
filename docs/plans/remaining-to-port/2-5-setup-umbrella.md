# 2.5 setup umbrella

Status: planned

## Why

ao-ts offers guided setup commands (e.g. `setup openclaw`). ao-rs has config docs but lacks interactive helpers.

## Current state (ao-rs)

- Config documentation exists: `docs/config.md`
- No `ao-rs setup` subcommands.

## Target behavior (ao-ts parity)

`ao setup <subcommand>` (starting with an `openclaw`-like helper) supporting non-interactive mode and routing presets.

## Proposed approach

1. Add `ao-rs setup` command group with one initial subcommand:
   - `ao-rs setup openclaw` (or `setup notifier` if naming differs in Rust)
2. Implement as pure config writer:
   - Prompt for URL/token unless `--non-interactive`
   - Write/update relevant sections in `ao-rs.yaml` (or print patch to apply)
3. Keep scope minimal: no plugin marketplace work.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/setup/mod.rs` (new)
- `crates/ao-cli/src/commands/setup/openclaw.rs` (new)
- `crates/ao-core/src/config.rs` (only if new config fields required)

## Acceptance criteria

- Running `ao-rs setup <x>` produces a valid config update (or prints a patch) without breaking existing config.
- `--non-interactive` works with env vars / flags.

## Test plan

- Unit tests for config patching logic using in-memory YAML strings.

## Risks / notes

- Decide whether CLI should mutate files by default or print instructions. Prefer “write with backup” if mutating.

