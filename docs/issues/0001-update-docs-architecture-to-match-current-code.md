# Update docs + architecture to match current code

## Goal

Bring documentation and architecture notes in sync with the current implementation (CLI commands, session lifecycle, plugins, dashboard/API, and batch-spawn).

## Scope

- Update CLI docs to reflect current subcommands + flags (incl. `batch-spawn`, issue workflows, `kill`/`cleanup`/`doctor`/`review-check`).
- Update architecture docs to reflect real module boundaries and data flow in `crates/ao-core`, `crates/ao-cli`, and plugins.
- Ensure docs mention current persistence paths and lifecycle lock behavior.

## Files to review/update

- `docs/cli-reference.md`
- `docs/architecture.md`
- `docs/state-machine.md`
- `docs/plugin-spec.md` (only if drifted)
- `README.md` (only if user-facing quickstart drifted)

## Acceptance criteria

- Docs match current clap help / implemented commands in `crates/ao-cli/src/main.rs`.
- Architecture sections reflect current crates and plugin wiring (agent/runtime/scm/tracker/workspace/notifiers).
- Roadmap vs implemented commands are clearly labeled (no ambiguity).
- Cross-links between docs are correct and not stale.

## Notes

- Prefer small, surgical doc edits; don’t refactor unrelated text.

## Notes

- 
