---
phase: design
title: nextest adoption + per-module test scope — design
description: Where each piece of guidance lives, and how the docs cross-link so nothing drifts.
---

# Design — nextest adoption + per-module test scope

## Architecture Overview

This is a docs-only change. The "architecture" is which file owns
which piece of guidance and how they reference each other.

```
┌───────────────────────────────────────────────────────────────┐
│  CONTRIBUTING.md (repo root)   ← SOURCE OF TRUTH                    │
│  ─ How to run tests (cargo t + cargo test --doc)              │
│  ─ Per-module test scope rule (table)                         │
│  ─ What Rust already proves (don't test)                      │
│  ─ Target coverage by layer                                   │
│  ─ Inner dev loop command cheat-sheet                         │
└───────────────────────────────────────────────────────────────┘
        ▲                          ▲                   ▲
        │ link                     │ link              │ link
        │                          │                   │
┌───────┴────────┐  ┌──────────────┴────┐  ┌──────────┴───────┐
│ README.md      │  │ docs/RELEASE.md   │  │ docs/ai/impl…/   │
│ dev commands   │  │ release checklist │  │ README.md        │
│ (already uses  │  │ (already uses     │  │ test-scope       │
│  cargo t)      │  │  cargo t)         │  │ pointer          │
└────────────────┘  └───────────────────┘  └──────────────────┘
```

Only the root `CONTRIBUTING.md` holds the full rule. Everything else
links to it.

## Data Models

N/A — no code change.

## API Design

N/A — no code change.

## Component Breakdown

| Piece | File | Change type |
|---|---|---|
| Source-of-truth runner + scope doc | `CONTRIBUTING.md` (new, repo root) | Create |
| Pointer from dev-lifecycle implementation phase | `docs/ai/implementation/README.md` | Add "Testing conventions" section pointing at `CONTRIBUTING.md` |
| Dev-commands section alignment | `README.md` | Already correct (uses `cargo t`). No change needed. |
| Release checklist alignment | `docs/RELEASE.md` | Already correct (uses `cargo t` + `cargo test --doc`). No change needed. |
| CI workflow | `.github/workflows/*.yml` | **Does not exist** — issue criterion N/A. Record the canonical commands in `CONTRIBUTING.md` so the next workflow lands right. |

**Files added:** 1 (`CONTRIBUTING.md`).
**Files modified:** 1 (`docs/ai/implementation/README.md`).
**Files deleted:** 0.
**Rust code touched:** none.

## Design Decisions

### 1. `CONTRIBUTING.md` over `CLAUDE.md`

The issue offered both as candidates. `CLAUDE.md` at the repo root
is in `.gitignore` (treated as per-user Claude Code settings), so
committing one would be a dead letter. `CONTRIBUTING.md` is the
conventional GitHub contributor doc and is picked up by the
"Community Standards" sidebar. Ship `CONTRIBUTING.md`.

Agents spawned by Claude Code already get the essential test-runner
rules through `ao-rs.yaml`'s project `rules:` block (landed in
commit `e1cff9d`). `CONTRIBUTING.md` is the fuller, authoritative
version; `ao-rs.yaml` is the short agent-injection form.

Rejected alternatives:
- **Un-ignore `CLAUDE.md` and ship both.** The gitignore entry is
  there so users can keep their own CLAUDE.md customisations
  private. Removing it invites churn for zero benefit — the
  content has a good home in `CONTRIBUTING.md`.
- **Ship both with one pointing at the other.** Extra hop, extra
  drift surface. Re-open if a contributor specifically asks.

### 2. Copy the test-scope table verbatim from the issue

The issue body is the agreed spec. Re-phrasing it into a new doc
invites drift. The CONTRIBUTING.md section labels its source:
"source: [issue #168](...)". If the spec changes, the issue and
doc move together.

### 3. Don't add a `.config/nextest.toml` profile

The repo ships a minimal nextest setup:
- `.cargo/config.toml` aliases `cargo t` to `nextest run`.
- `.cargo/config.toml` sets `[build] jobs = 2` to keep laptop CPU
  reasonable.

No need for a `nextest.toml` profile yet:
- Default nextest settings (parallel-by-logical-CPU, per-test
  process isolation) are what we want.
- A profile file would add a place for values to drift without
  clear need.
- `--test-threads N` works on the command line for operators who
  want to throttle, and `README.md` already documents that.

Add one only when we hit a concrete need (a flaky integration
test that needs `retries = 2`, a slow-test bucket, CI-specific
reporter).

### 4. Don't modify `docs/RELEASE.md`

It already says `cargo t --workspace` and `cargo test --doc
--workspace`. The section "Automated CI (PRs + main)" still
references `.github/workflows/` but that section is forward-looking
("when we re-add CI"). Leaving it in place keeps the intent on
record.

### 5. Don't resurrect deleted CI workflows

Commit `e8e4c54` ("chore: remove obsolete CI and release artifact
workflows") deleted them on purpose. Re-adding them is out of
scope for this issue — **and** the issue's CI criterion can't be
met mechanically (no files to modify). Document the intended
command in `CONTRIBUTING.md` so the next workflow author starts right.

### 6. Keep `ao-rs.yaml` project rules

`ao-rs.yaml` already includes, for the `ao-rs` project:

```yaml
rules: |-
  - Testing: use `cargo t` (nextest alias) — NOT `cargo test`.
  - Run `cargo test --doc` separately for doctests (nextest skips them).
  - When implementing a new feature, add a `#[cfg(test)] mod tests`
    module alongside the code (or under `tests/` for integration) …
```

These lines came from commit `e1cff9d`. Don't duplicate them in
`CONTRIBUTING.md` — reference them and expand on the scope rule. The
agent-rules channel and the contributor doc channel cover the
same idea in different voices; `CONTRIBUTING.md` is the authoritative
source, `ao-rs.yaml` is the agent-injection shorthand.

## State Machine Deltas

N/A.

## Non-Functional Requirements

- **Freshness.** Update CONTRIBUTING.md whenever the workflow changes.
  The release-checklist commands in `docs/RELEASE.md` and the
  agent rules in `ao-rs.yaml` should re-link to CONTRIBUTING.md (not
  re-list commands) on next edit.

## Intentional Divergences from Issue #168

| Issue says | This design | Why |
|---|---|---|
| "CI `.github/workflows/*.yml` updated to use `cargo nextest run --workspace`" | No CI file edited | `.github/workflows/` was deleted in `e8e4c54`. The command is recorded in `CONTRIBUTING.md` for the next CI re-introduction. |
| "Install: `cargo install cargo-nextest`" | `CONTRIBUTING.md` documents install and links to the nextest site | Just covering the install path for first-time contributors. |

## Test Strategy preview

No Rust code changes — no new tests.

Verification steps (see planning doc):

1. `cargo fmt --all -- --check`.
2. `cargo clippy --workspace --all-targets -- -D warnings`.
3. `cargo t --workspace`.
4. `cargo test --doc --workspace`.
5. Manual: open `CONTRIBUTING.md` in the repo root and sanity-check
   that every command runs.
