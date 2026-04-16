# 3.8 doctor missing flags

Status: planned

## Why

ao-ts `doctor` supports `--fix` and `--test-notify` to help users self-diagnose and validate notification routing. ao-rs `doctor` is read-only.

## Current state (ao-rs)

- `ao-rs doctor` exists without flags.
- Notifier registry + routing exist in config and wiring.

## Target behavior (ao-ts parity)

- `ao doctor --fix`: attempt to remediate common issues (missing dirs, missing config)
- `ao doctor --test-notify`: send a test notification through routing config

## Proposed approach

1. Add `--test-notify`:
   - Build a `NotificationPayload` with known priority and message
   - Route via registry and send to each notifier (or selected ones)
2. Add `--fix`:
   - Create missing `~/.ao-rs` directories
   - Suggest config init if missing
   - Avoid destructive actions

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/doctor.rs`

## Acceptance criteria

- `ao-rs doctor --test-notify` triggers notifier plugins with a clear “test” message.
- `ao-rs doctor --fix` is safe and idempotent.

## Test plan

- Unit tests with `TestNotifier` registry to assert routing and send calls.

## Risks / notes

- Networked notifiers (Slack/Discord/ntfy) need opt-in to avoid accidental spam; require explicit flag + confirmation or target selection.

