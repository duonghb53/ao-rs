# 2.6 plugin umbrella

Status: planned

## Why

ao-ts supports marketplace-managed plugins via `ao plugin ...`. ao-rs uses workspace crates for plugins and currently has no CLI for listing/enabling/disabling plugins.

## Current state (ao-rs)

- Plugins are Rust crates under `crates/plugins/*`.
- Config contains a `plugins:` list “stored for parity only”:
  - `crates/ao-core/src/config.rs`
- No `ao-rs plugin` command.

## Target behavior (ao-ts parity)

`ao plugin <subcommand>` (install/list/update/enable/disable).

## Proposed approach

Two viable approaches (pick one before implementation):

1. **Crate-based (recommended for ao-rs)**
   - `ao-rs plugin list`: list compiled-in plugins (agents, runtimes, etc.)
   - `ao-rs plugin info <name>`: show config keys + env vars
   - No install/update (handled by cargo / release process)
2. **Marketplace-based (true parity)**
   - Implement plugin installer, registry, updates, signatures, etc.
   - Large effort and new security surface.

## Files to change (crate-based)

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/plugin.rs` (new)
- `crates/ao-cli/src/cli/plugins.rs` (if central plugin registry exists / needs adding)

## Acceptance criteria

- `ao-rs plugin list` prints available plugin names grouped by slot.
- Docs link from output to `docs/plugin-spec.md`.

## Test plan

- Unit test for plugin registry enumeration (pure data).

## Risks / notes

- This item is explicitly flagged as requiring a design decision in `docs/remaining-to-port.md`.

