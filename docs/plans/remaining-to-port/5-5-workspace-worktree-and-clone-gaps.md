# 5.5 workspace-worktree and clone gaps

Status: planned

## Why

Workspace plugins are intentionally minimal (create/destroy only). This leaves config fields (`symlinks`, `postCreate`) unused and blocks restore/list flows.

## Current state (ao-rs)

- `crates/plugins/workspace-worktree/src/lib.rs`: create/destroy only
- `crates/plugins/workspace-clone/src/lib.rs`: create/destroy only
- Missing:
  - symlinks execution
  - postCreate execution
  - list/restore support

## Target behavior (ao-ts parity)

- Implement workspace hooks during `create`.
- Add minimal restore support required by session restore.

## Proposed approach

1. Implement symlinks + postCreate (see 1.3 plan).
2. Add `exists`/`restore` surface if needed to unblock restore (see 1.3/1.4).

## Files to change

- `crates/plugins/workspace-worktree/src/lib.rs`
- `crates/plugins/workspace-clone/src/lib.rs`
- `crates/ao-core/src/traits.rs` (if adding new workspace methods)

## Acceptance criteria

- Configured hooks are executed on workspace creation.
- Session restore can verify workspace existence cleanly.

## Test plan

- Integration test using temp dirs to validate hook behavior.

## Risks / notes

- Avoid expanding the workspace trait too early; add only what restore and spawn need.

