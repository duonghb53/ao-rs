# 3.3 spawn missing flags

Status: planned

## Why

ao-ts spawn supports convenience flows (`--open`, `--claim-pr`, `--assign-on-github`, `--prompt`). ao-rs spawn is more structured (task/issue/template) but lacks these flags.

## Current state (ao-rs)

- `crates/ao-cli/src/commands/spawn.rs` and args in `crates/ao-cli/src/cli/args.rs`
- Missing flags:
  - `--open`
  - `--claim-pr <pr>`
  - `--assign-on-github`
  - `--prompt <text>`

## Target behavior (ao-ts parity)

- `--open`: open/attach session after spawning
- `--claim-pr`: attach existing PR to session metadata
- `--assign-on-github`: assign issue/PR to current user
- `--prompt`: override generated prompt text

## Proposed approach

1. Implement `--open`:
   - After spawn success, run `ao-rs session attach <id>` (tmux) or `ao-rs open session <id>` if available.
2. Implement `--prompt`:
   - If provided, use as initial prompt (bypass template/issue composition).
3. Implement `--claim-pr`:
   - Persist PR number/url into session metadata fields (define where).
4. Implement `--assign-on-github`:
   - For GitHub tracker, call `gh issue edit --add-assignee @me` or PR assignee API.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/spawn.rs`
- (Optional) `crates/plugins/tracker-github/src/lib.rs` (assignment helper)
- (Optional) session type additions if PR metadata fields are missing

## Acceptance criteria

- `ao-rs spawn ... --open` opens/attaches automatically.
- `--prompt` replaces generated prompt.
- `--claim-pr 123` records PR linkage for the session.

## Test plan

- Unit tests for arg parsing + prompt override.
- Mock tracker/SCM tests for `--assign-on-github` and `--claim-pr` persistence.

## Risks / notes

- Attaching existing PRs requires a clear mapping from PR → branch/session; keep scope minimal (store reference only).

