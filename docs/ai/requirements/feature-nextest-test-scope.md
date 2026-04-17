---
phase: requirements
title: nextest adoption + per-module test scope guidelines (issue #168)
description: Formalize cargo-nextest as the standard runner and document the scope rule contributors should follow when adding or changing tests.
---

# Requirements — nextest adoption + per-module test scope

## Problem Statement

Two related pains from issue
[#168](https://github.com/duonghb53/ao-rs/issues/168):

1. **`cargo test --workspace` is slow.** Cold builds take 60–90 s for a
   one-line change and serial execution dominates the inner dev loop.
2. **No explicit test-scope rule.** Contributors (humans and spawned
   agents) lack clear guidance on *what* to test when adding or
   changing a module, leading to either overtesting (slow, brittle
   suites) or undertesting (gaps in business-logic coverage).

Current state in this repo:

- `cargo t` is already wired to `nextest run` via
  `.cargo/config.toml`. README, `docs/RELEASE.md`, and `ao-rs.yaml`
  already reference `cargo t` as the default. So **part A of the
  issue is mostly landed** — we still need a single contributor-
  facing page that states the convention plainly.
- There is **no `CONTRIBUTING.md` or `CONTRIBUTING.md`** at the repo root.
  Spawned agents rely on `ao-rs.yaml` project rules; human
  contributors have nothing.
- There is **no per-module scope rule** written down anywhere.
- `.github/workflows/` was deleted in commit `e8e4c54`; there is
  currently no CI to retarget. The issue's CI acceptance criterion
  is therefore N/A for this repo state — document the intended
  runner so the next CI workflow lands on nextest from the start.

## Goals & Objectives

### Primary goals

1. Ship a `CONTRIBUTING.md` at the repo root that documents, as the
   contributor-facing source of truth:
   - `cargo-nextest` as the standard test runner (plus the `cargo t`
     alias and the `cargo test --doc` carve-out).
   - The per-module test scope rule from the issue body.
   - The inner dev loop command cheat-sheet.
2. Make the test-scope rule discoverable from the dev-lifecycle docs
   (`docs/ai/implementation/`) so the existing `new-requirement` /
   `writing-test` skills pick it up.
3. Ensure the CI-command guidance in `docs/RELEASE.md` stays
   consistent with the CONTRIBUTING.md — so whoever re-adds the workflow
   copy-pastes the right commands.

### Secondary goals

- Spot-check that no new `tests/` module was added to an untouched
  module by this change (the issue's "no new test files for existing
  untouched modules" criterion) — this change is docs-only.

### Non-goals

- **Re-adding CI workflows.** `e8e4c54` removed them deliberately.
  Restoring them is a separate decision.
- **Rewriting the nextest config.** `.cargo/config.toml` is already
  a minimal, working setup (`jobs = 2` + `t = nextest run`). No
  `.config/nextest.toml` profile is added unless a concrete need
  shows up.
- **Retrofitting existing tests.** The scope rule applies going
  forward; existing tests are grandfathered.

## User Stories & Use Cases

1. **Human contributor clones the repo.** They expect a root-level
   `CONTRIBUTING.md` (or `CONTRIBUTING.md`) that tells them the canonical
   commands and the test-scope rule. They get it in one file.
2. **Agent spawned from issue #168 reads `CONTRIBUTING.md`.** The
   dev-lifecycle flow pulls the scope rule into its requirements
   drafting step — the agent doesn't invent ad-hoc coverage goals.
3. **Reviewer on a PR.** When someone adds a broad integration test
   for a refactor (behaviour unchanged), the reviewer can link to
   the "Refactor → existing tests must pass — do not add new ones"
   row in `CONTRIBUTING.md`.

## Success Criteria

Acceptance (mapped to the issue):

- [x] `cargo-nextest` documented in a root-level contributor file
      (`CONTRIBUTING.md`) as the standard runner.
- [~] CI workflows updated — **N/A**, repo currently has no
      `.github/workflows/*.yml` (removed in `e8e4c54`). The
      CONTRIBUTING.md records the intended command so the next workflow
      lands right.
- [x] Per-module test scope rule documented (copied from the issue
      body and cross-linked from `docs/ai/implementation/`).
- [x] No new test files added for existing untouched modules —
      enforced by this change being docs-only; verified in the PR
      diff.

Quality gates:

- `cargo fmt --all -- --check` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo t --workspace` clean.
- `cargo test --doc --workspace` clean.

## Constraints & Assumptions

- **Docs-only change.** No Rust code touched, no `Cargo.toml`
  touched.
- **Single source of truth = `CONTRIBUTING.md` at repo root.** Other docs
  (`README.md`, `docs/RELEASE.md`, `ao-rs.yaml`) cross-reference it
  rather than duplicate.
- **Assume the next CI re-introduction uses nextest.** The issue
  body is the spec; CONTRIBUTING.md records the exact command.

## Questions & Open Items

- Should `CONTRIBUTING.md` also exist as a GitHub-idiomatic pointer
  to `CONTRIBUTING.md`? **Decision:** ship `CONTRIBUTING.md` only for now.
  GitHub will surface a `CONTRIBUTING.md` in the "Contributing" sidebar
  if it's the only candidate, and duplicating content invites drift.
  Re-open if a contributor specifically asks.
