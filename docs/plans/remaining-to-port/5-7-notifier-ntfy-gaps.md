# 5.7 notifier-ntfy gaps

Status: planned

## Why

ntfy notifier lacks auth support for private servers and has limited automated test coverage.

## Current state (ao-rs)

- `crates/plugins/notifier-ntfy/src/lib.rs`
  - Webhook-like send to ntfy server.
  - Comment notes future auth support.
  - No HTTP unit tests in the crate.

## Target behavior (ao-ts parity / maturity)

- Support auth (token/basic) for private ntfy servers.
- Add test coverage for request construction.

## Proposed approach

1. Extend config/env support:
   - Add optional auth header (Bearer token) or basic auth fields.
2. Implement request header injection.
3. Add unit tests using a mock HTTP server or by testing request building logic without network.

## Files to change

- `crates/plugins/notifier-ntfy/src/lib.rs`
- `crates/ao-core/src/config.rs` (only if new config fields are added beyond env vars)

## Acceptance criteria

- Authenticated ntfy servers can receive notifications when configured.
- Unit tests validate URL, headers, payload formatting.

## Test plan

- Unit tests for header construction.
- Optional integration test using a local mock server in tests.

## Risks / notes

- Keep secrets out of logs; redact auth in tracing.

