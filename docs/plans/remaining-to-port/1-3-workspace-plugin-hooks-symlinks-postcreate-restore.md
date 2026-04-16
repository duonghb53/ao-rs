# 1.3 Workspace plugin hooks (symlinks, postCreate, restore)

Status: implemented (symlinks + postCreate); restore/list parity still open

## Why

`symlinks` and `postCreate` are accepted in config but not executed. Restore support is missing, which blocks parity with ao-ts workspace behavior and reduces UX for common project setup steps.

## Current state (ao-rs)

- Config supports:
  - `crates/ao-core/src/config.rs`: `ProjectConfig.symlinks`, `ProjectConfig.post_create`
- Workspace creation executes hooks:
  - `crates/ao-core/src/workspace_hooks.rs`: `apply_workspace_hooks()` (symlinks + `postCreate`)
  - `crates/plugins/workspace-worktree/src/lib.rs`: calls `apply_workspace_hooks()` during `create()`
  - `crates/plugins/workspace-clone/src/lib.rs`: calls `apply_workspace_hooks()` during `create()`
  - `crates/ao-cli/src/commands/spawn.rs`: threads `symlinks` + `post_create` into `WorkspaceCreateConfig`
- Integration coverage:
  - `crates/plugins/workspace-worktree/tests/integration.rs`: `create_symlinks_and_post_create`
  - `crates/plugins/workspace-clone/tests/integration.rs`: `create_symlinks_and_post_create`
- Remaining parity gap:
  - No `Workspace::list` / `Workspace::restore` surface in `crates/ao-core/src/traits.rs` today.

## Target behavior (ao-ts parity)

- Workspace creation should:
  - Create configured symlinks into the workspace (e.g. `.env`, `.claude`)
  - Run configured `postCreate` commands after workspace creation
- Optional: support `restore()` (and/or `exists()` / `list()` as needed) for session restore flows.

## Proposed approach

1. Implement symlink creation in workspace `create()`:
   - For each entry in `symlinks`, symlink from project root into workspace root.
   - Define behavior on missing sources (fail vs warn; mirror TS).
2. Implement `postCreate` execution:
   - Run commands with workspace root as cwd.
   - Capture stdout/stderr for error reporting.
3. Decide scope for restore:
   - Minimal: add `Workspace::exists(path)` (or reuse filesystem check) so restore can fail early.
   - Full: add `Workspace::restore(session)` hook.

## Files to change

- `crates/ao-core/src/traits.rs`
  - If needed, extend `Workspace` trait with `restore()` / `exists()`.
- `crates/plugins/workspace-worktree/src/lib.rs`
  - Implement symlinks + postCreate behavior during `create()`.
- `crates/plugins/workspace-clone/src/lib.rs`
  - Implement symlinks + postCreate behavior during `create()`.
- `crates/ao-cli/src/commands/spawn.rs`
  - Ensure symlinks/postCreate config is passed through to workspace plugin calls (if not already).

## Acceptance criteria

- When a project config defines:
  - `symlinks: [.env]` and `.env` exists in project root
  - `postCreate: ["echo ok"]`
  then a spawned workspace contains `.env` as a symlink and `postCreate` runs successfully.

## Test plan

- Unit tests for symlink path safety + behavior on missing source.
- Integration test using temp dirs to create a worktree workspace and verify symlink + command execution.

## Risks / notes

- Cross-platform symlink behavior (macOS/Linux/Windows) — decide whether to support Windows now.
- Security: ensure `symlinks` entries are safe path segments (no `..`) and `postCreate` commands are executed intentionally (documented as user-provided).

