# 4.1 Scm trait missing methods

Status: planned

## Why

`Scm` in ao-rs is intentionally trimmed versus ao-ts `SCM` (per `traits.rs`). Some TS capabilities are missing and block parity for webhook-driven workflows and richer automation.

## Current state (ao-rs)

- `crates/ao-core/src/traits.rs`: `Scm` trait (subset)
- Explicitly missing vs TS (documented):
  - Webhook verification
  - Automated bot-comment fetch
  - PR check-out helper
  - Per-session `ProjectConfig` plumbing on each method

## Target behavior (ao-ts parity)

Bring the `Scm` trait surface closer to TS where it materially improves workflows, without forcing a full rewrite.

## Proposed approach

1. Prioritize missing methods by impact:
   - Webhooks (enables event-driven updates)
   - Bot-comment fetch (improves review/CI automation)
   - Checkout helper (quality-of-life)
2. Extend `Scm` trait incrementally:
   - Add new methods behind default no-op implementations when possible.
3. Implement in `scm-github` first; treat other SCM plugins as optional.

## Files to change

- `crates/ao-core/src/traits.rs`
  - Add methods and default impl strategy
- `crates/plugins/scm-github/src/lib.rs`
  - Implement new methods using `gh` REST/GraphQL
- `crates/ao-core/src/lifecycle.rs` (only if lifecycle needs to call new methods)

## Acceptance criteria

- New methods exist and are documented.
- GitHub SCM implements them.
- Existing builds/tests still pass for other SCM plugins (default impl or compile-time feature gates).

## Test plan

- Unit tests for method behavior (parsing, URL verification).
- Mock-based tests for lifecycle integration if used.

## Risks / notes

- Expanding trait surface impacts all SCM plugins; prefer optional methods or default impls.

