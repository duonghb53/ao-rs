# 2.3 verify command

Status: planned

## Why

ao-ts has `ao verify` to verify issue completion and optionally comment/fail. ao-rs lacks this workflow.

## Current state (ao-rs)

- `ao-rs` can spawn from issues and track PR status, but has no “verify” pipeline.
- Tracker surfaces exist via `Tracker` trait, but missing issue update/comment methods.

## Target behavior (ao-ts parity)

`ao verify [issue]` with options:
- `--list` list verify targets
- `--comment <msg>` comment on issue/PR
- `--fail` exit non-zero if verification fails

## Proposed approach

1. Define verification rules (minimal parity):
   - Session exists for issue
   - PR merged OR session status terminal success (`Merged`/`Done`)
2. Implement CLI:
   - `ao-rs verify --list` shows issues/sessions eligible
   - `ao-rs verify <issue|session>` checks rules, prints result
3. Optional: integrate tracker commenting once `Tracker` adds `update_issue`/comment support.

## Files to change

- `crates/ao-cli/src/cli/args.rs` and command wiring
- `crates/ao-cli/src/commands/verify.rs` (new)
- (Optional) `crates/ao-core/src/traits.rs` / tracker plugins if comment support is required

## Acceptance criteria

- `ao-rs verify <id>` prints pass/fail status and exits 0/1 with `--fail`.
- `--list` works without network calls beyond session list.

## Test plan

- Unit tests for verification rule logic using in-memory sessions.
- CLI tests for exit codes with `--fail`.

## Risks / notes

- Full parity may require tracker write APIs (currently trimmed). Start read-only first.

