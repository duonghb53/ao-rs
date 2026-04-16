# 4.2 Tracker trait missing methods

Status: planned

## Why

ao-ts tracker supports listing/updating/creating issues. ao-rs `Tracker` trait explicitly trimmed these methods, limiting CLI workflows like `verify --comment` and richer issue ops.

## Current state (ao-rs)

- `crates/ao-core/src/traits.rs`: `Tracker` trait is missing:
  - `list_issues`
  - `update_issue`
  - `create_issue`
- `tracker-github` also notes these are “deliberately not ported” from TS.

## Target behavior (ao-ts parity)

Expand `Tracker` trait to support basic read/write issue workflows.

## Proposed approach

1. Add new methods to `Tracker` trait with minimal inputs/outputs:
   - `list_issues(project, filters) -> Vec<Issue>`
   - `update_issue(id, patch) -> Issue`
   - `create_issue(fields) -> Issue`
2. Implement for GitHub tracker first (via `gh`).
3. Keep Linear tracker optional (implement if needed later).

## Files to change

- `crates/ao-core/src/traits.rs`
- `crates/plugins/tracker-github/src/lib.rs`
- (Optional) `crates/plugins/tracker-linear/src/lib.rs`
- CLI commands that will consume these methods (e.g. `verify`, future `issue` commands)

## Acceptance criteria

- GitHub tracker can list issues and update/create issues through the trait.
- Existing code using `Tracker` continues to compile and run.

## Test plan

- Unit tests for request construction/parsing (JSON fixtures).
- Keep integration tests optional behind env (requires `gh` auth).

## Risks / notes

- Trait expansion requires implementing or defaulting methods across plugins; prefer default “not supported” errors where appropriate.

