# 5.4 tracker-github gaps

Status: planned

## Why

GitHub tracker is intentionally trimmed vs TS; some missing behaviors affect performance and workflows (e.g. issue completion checks, issue write APIs).

## Current state (ao-rs)

- `crates/plugins/tracker-github/src/lib.rs`
  - Missing TS APIs: list/update/create issues, generatePrompt (moved to trait default)
  - No older-`gh` `stateReason` retry dance (requires newer `gh`)
  - `TODO(perf)`: `is_completed` may re-fetch full issue

## Target behavior (ao-ts parity)

- Add missing issue operations (list/update/create) once `Tracker` trait expands.
- Improve `is_completed` to avoid full issue fetch when possible.

## Proposed approach

1. Implement performance improvement for `is_completed`:
   - Use lightweight `gh issue view --json state` / minimal fields.
2. After `Tracker` trait expansion (see 4.2), implement:
   - `list_issues`, `update_issue`, `create_issue`
3. Decide whether to support older gh versions:
   - Keep current minimum and document requirement (preferred), or add retry dance.

## Files to change

- `crates/plugins/tracker-github/src/lib.rs`
- `crates/ao-core/src/traits.rs` (for trait expansion)

## Acceptance criteria

- `is_completed` uses a minimal API call.
- Tracker supports list/update/create issues after trait expansion.

## Test plan

- Unit tests with fixture JSON parsing for minimal issue view.

## Risks / notes

- `gh` output schema changes; prefer `--json` fields with stable names.

