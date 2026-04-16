# 3.4 send missing flags

Status: planned

## Why

ao-ts `send` supports sending file content and controlling wait/timeout. ao-rs `send` is currently a single message string.

## Current state (ao-rs)

- `crates/ao-cli/src/commands/send.rs` and args in `crates/ao-cli/src/cli/args.rs`
- Missing:
  - `--file <path>`
  - `--no-wait`
  - `--timeout <seconds>`
  - variadic message args

## Target behavior (ao-ts parity)

- `ao send <session> [message...] --file <path> --timeout 600 --no-wait`

## Proposed approach

1. Add `--file <path>`:
   - Read file contents and send as message body (with optional prefix header).
2. Add variadic message args:
   - Join args with spaces to match TS UX.
3. Add wait controls:
   - If runtime/agent supports acknowledgement, implement; otherwise `--no-wait` becomes a no-op documented flag until supported.
4. Add `--timeout`:
   - Bound runtime send operation.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/send.rs`
- (Optional) `crates/ao-core/src/traits.rs` runtime surface if acknowledgements are required

## Acceptance criteria

- `ao-rs send <id> --file README.md` sends the file contents.
- `ao-rs send <id> hello world` works with variadic args.
- `--timeout` is honored for the send operation.

## Test plan

- Unit tests for file read + message assembly.
- Mock runtime test to ensure timeout path triggers.

## Risks / notes

- Large files: define max size and truncation policy.

