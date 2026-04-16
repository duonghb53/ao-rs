# 7.4 default merge method

Status: planned

## Why

Default merge method differs: ao-rs defaults to `merge`, ao-ts defaults to `squash`. This affects PR history and could surprise users migrating configs.

## Current state (ao-rs)

- `crates/plugins/scm-github/src/lib.rs` documents the divergence.
- `crates/ao-core/src/scm.rs` defines `MergeMethod` enum; defaults are Rust-defined.

## Target behavior (parity decision)

Decide one of:

1. Keep ao-rs default as `merge` (current behavior) and document clearly.
2. Switch default to `squash` to match ao-ts.
3. Make default explicit in generated config examples to avoid ambiguity.

## Proposed approach

1. Choose policy (1/2/3).
2. If switching default, update:
   - default merge method in SCM plugin
   - documentation and config example
3. Add regression test verifying chosen default.

## Files to change

- `crates/plugins/scm-github/src/lib.rs`
- (Optional) `crates/ao-core/src/scm.rs`
- `docs/reactions.md` or config docs where merge behavior is discussed

## Acceptance criteria

- Default merge method is consistent and documented.
- Users can override via reaction config `merge_method` when using auto-merge.

## Test plan

- Unit test asserting default merge method used when config omits it.

## Risks / notes

- Changing defaults is behavior-breaking; prefer explicit config for production setups.

