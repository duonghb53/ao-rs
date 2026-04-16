# 5.6 scm-gitlab parity unknown

Status: planned

## Why

`scm-gitlab` exists but is not framed as an ao-ts parity port in comments. It’s unclear whether ao-ts has a GitLab SCM plugin and what parity target should be.

## Current state (ao-rs)

- `crates/plugins/scm-gitlab/src/lib.rs` is a REST-based implementation.
- No `packages/plugins/...` TS reference in the crate doc/comments.

## Target behavior (parity decision)

Decide one of:

1. Treat GitLab SCM as ao-rs-only feature (no parity goal).
2. Port ao-ts GitLab plugin (if it exists) and align behavior.

## Proposed approach

1. Find ao-ts GitLab plugin (if any) and document its surface:
   - methods supported, auth model, CI mapping, mergeability rules
2. Write a parity matrix: GitHub vs GitLab differences.
3. If parity is required, update ao-rs implementation to match.

## Files to change

- `docs/remaining-to-port.md` (if you want to record decision; optional)
- `crates/plugins/scm-gitlab/src/lib.rs` (if parity work proceeds)

## Acceptance criteria

- Clear documented decision: parity-targeted or ao-rs-only.
- If parity-targeted: behavior is specified and tested.

## Test plan

- Unit tests with fixture JSON for CI status mapping and merge readiness.

## Risks / notes

- GitLab API shapes differ substantially; parity may not be meaningful beyond the shared `Scm` trait.

