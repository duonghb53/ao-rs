# 6.0 parity-only modules (meta)

Status: planned

## Why

Several `parity_*` modules exist to validate ao-ts parity in isolation but are not wired into the ao-rs runtime. This is valuable test infrastructure, but can also become confusing or stale without clear ownership.

## Current state (ao-rs)

These modules exist under `crates/ao-core/src/` and are used primarily by parity tests:

- `parity_utils.rs` (ported `packages/core/src/utils.ts` and friends; not used by runtime)
- `parity_session_strategy.rs` (enums used in production config; `decide_existing_session_action` is test-only)
- `parity_config_validation.rs`
- `parity_plugin_registry.rs`
- `parity_observability.rs`
- `parity_metadata.rs`
- `parity_feedback_tools.rs`

## Decision needed

Pick one direction for each parity module (not necessarily the same for all):

1. **Keep test-only**
   - Treat as fixtures/parity harness.
   - Ensure docs and naming make that obvious.
2. **Graduate into runtime**
   - Move code into non-parity modules and call it from production paths.
   - Keep parity tests pointed at the production implementation.

## Proposed approach

1. Classify modules:
   - **Candidates to graduate**: utilities that reduce duplication (`parity_utils` helpers), config validation rules.
   - **Keep test-only**: TS observability/feedback tools unless feature is planned.
2. Add documentation:
   - Add a short section to `crates/ao-core/src/lib.rs` or `docs/ts-core-parity-report.md` explaining parity modules.
3. Add “staleness guard”:
   - Tests that ensure parity modules track the relevant production paths (when applicable).

## Files to change

- Documentation (one of):
  - `docs/ts-core-parity-report.md`
  - `docs/validation-ported-code.md`
- If graduating code:
  - Move selected functions into the appropriate production modules.

## Acceptance criteria

- Every `parity_*` module is clearly labeled as either test-only or production-used.
- Parity tests still pass and don’t duplicate production logic unnecessarily.

## Test plan

- No new tests required beyond ensuring existing parity tests remain meaningful.

## Risks / notes

- Graduating parity code too early can cause churn; prefer incremental moves tied to real runtime needs.

