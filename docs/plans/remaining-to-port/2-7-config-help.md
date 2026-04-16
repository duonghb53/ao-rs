# 2.7 config-help

Status: planned

## Why

ao-ts offers `ao config-help` to print a config guide. ao-rs has `docs/config.md` but no CLI shortcut.

## Current state (ao-rs)

- Docs: `docs/config.md`
- No CLI command to print config instructions.

## Target behavior (ao-ts parity)

`ao config-help` prints a concise config guide and points to the example config file.

## Proposed approach

1. Add `ao-rs config-help` command that:
   - Prints a short guide (summary + common keys).
   - Prints path to `ao-rs.yaml.example`.
   - Links to `docs/config.md` and `docs/reactions.md`.
2. Keep output stable for copy/paste (no color by default).

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/config_help.rs` (new)
- `crates/ao-cli/src/main.rs` routing

## Acceptance criteria

- `ao-rs config-help` prints a useful guide in <200 lines.
- Mentions where config is discovered (`ao-rs.yaml` discovery).

## Test plan

- CLI snapshot test verifying output contains key sections and file names.

## Risks / notes

- Avoid duplicating `docs/config.md` entirely; keep it as a pointer + essentials.

