# 2.2 open command

Status: planned

## Why

ao-ts provides `ao open` to open dashboard/session targets in browser or terminal. ao-rs currently requires manual navigation.

## Current state (ao-rs)

- `ao-rs dashboard` has `--open` to open the dashboard URL (limited scope).
- No general-purpose open command exists.

## Target behavior (ao-ts parity)

`ao open [target] [-w, --new-window]` where targets could include:
- dashboard URL
- PR URL for a session (if known)
- workspace path for a session

## Proposed approach

1. Add `ao-rs open` with a small set of targets:
   - `dashboard` (default): open `http://localhost:<port>/`
   - `session <id>`: open session detail URL if dashboard running; else open workspace directory
2. Implement OS open:
   - macOS: `open`
   - Linux: `xdg-open`
3. Add `--new-window` if meaningful (macOS: `open -n`, browser-specific behavior varies).

## Files to change

- `crates/ao-cli/src/cli/args.rs`
  - Add `Open` command and target options.
- `crates/ao-cli/src/commands/open.rs` (new)
  - Implement `open` execution.

## Acceptance criteria

- `ao-rs open` opens the dashboard in the default browser.
- `ao-rs open session <id>` opens either dashboard detail URL (if available) or the workspace folder.

## Test plan

- Unit test for URL/target resolution (no actual OS open call).
- Keep OS open call behind a small adapter so tests can stub it.

## Risks / notes

- Cross-platform differences (Windows) can be deferred if not needed.

