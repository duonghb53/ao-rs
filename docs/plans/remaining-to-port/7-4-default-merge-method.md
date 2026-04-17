# 7.4 default merge method

Status: done

## Decision

Option 3 (from the original plan): **keep ao-rs's safer `merge` default and
make the divergence explicit** in docs and the example config. Flipping to
`squash` would silently rewrite commit history for anyone already running
ao-rs; preserving the current default is less surprising, and ao-ts users
who want squash opt in with one line.

## What landed

- `crates/plugins/scm-github/src/lib.rs`
  - Extracted `merge_method_flag(Option<MergeMethod>) -> &'static str` as a
    pure helper so the default can be unit-tested without shelling out to
    `gh`. The `merge()` impl now just calls it.
  - Added `merge_method_flag_default_is_merge_commit` test locking in that
    `None → "--merge"`, plus the mapping for each explicit variant. The
    test carries a "do not flip without updating this plan" comment.
- `crates/ao-core/src/scm.rs`
  - Enhanced the `MergeMethod` doc comment to call out the deliberate
    divergence from ao-ts and point at this plan.
- `ao-rs.yaml.example`
  - Added a commented `approved-and-green` block with `merge_method: merge`
    so new configs pick up the explicit value and ao-ts migrators see the
    available override.
- `docs/reactions.md`
  - New "Default merge method (parity divergence, issue #109)" subsection
    explaining the resolution order and the safer-default rationale.

## Parity status

- Default now matches `MergeMethod::default()` in both the GitHub and
  GitLab plugins (`scm-gitlab/src/lib.rs::merge` already called
  `method.unwrap_or_default()`).
- `docs/validation-ported-code.md` already lists the `Merge` vs `squash`
  divergence; left as-is — it now has a documented rationale and a
  regression test backing it.

## Follow-ups

None. If a future port ever flips the default, update the test, the
`MergeMethod` doc, `ao-rs.yaml.example`, and `docs/reactions.md` together.
