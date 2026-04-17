# 5.4 tracker-github gaps

Status: done

## Why

GitHub tracker is intentionally trimmed vs TS; some missing behaviors affect performance and workflows (e.g. issue completion checks, issue write APIs).

## Current state (ao-rs)

- `crates/plugins/tracker-github/src/lib.rs`
  - `list_issues`, `update_issue`, `create_issue` implemented via the
    expanded `Tracker` trait (4.2).
  - `is_completed` now hits `gh issue view <n> --repo <slug> --json
    state,stateReason` instead of the full REST API — minimal payload,
    same 30s cache, same rate-limit cooldown behavior.
  - `generatePrompt` lives on the trait default (unchanged).
  - No older-`gh` `stateReason` retry dance — we require `gh >= 2.40`
    and document it rather than growing the shell-out matrix.

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

