# Contributing to ao-rs

This is the contributor-facing source of truth for how we build,
test, and ship ao-rs. Agents spawned by Claude Code inherit these
conventions via the project rules in
[`ao-rs.yaml`](ao-rs.yaml) — keep the two in sync; this file is
authoritative.

Companion docs:

- [`README.md`](README.md) — what ao-rs is, quick start, CLI usage.
- [`docs/architecture.md`](docs/architecture.md) — crate layout and
  design principles.
- [`docs/DEV.md`](docs/DEV.md) — local dashboard + desktop UI setup.
- [`docs/RELEASE.md`](docs/RELEASE.md) — release + distribution flow.

## Running tests

We use [`cargo-nextest`](https://nexte.st) as the standard runner.
It runs each test in an isolated process and parallelises across
logical CPUs — typically 2–3× faster than `cargo test` on multi-core
machines.

`cargo t` is aliased to `cargo nextest run` in
[`.cargo/config.toml`](.cargo/config.toml), so:

```bash
# Fast workspace run (the default for the inner dev loop):
cargo t

# Scoped to one crate (fastest inner loop):
cargo t -p ao-core
cargo t -p ao-cli

# Filter by test name:
cargo t -p ao-core lifecycle

# Cap threads if the laptop is getting warm:
cargo t --workspace --test-threads=2
```

### Doctests

`cargo nextest` does **not** run doctests by design. Run them with
`cargo test --doc`:

```bash
cargo test --doc --workspace
```

This is the only thing plain `cargo test` is used for in this repo.

### Installing nextest (first time)

```bash
cargo install cargo-nextest --locked
```

After that, `cargo t` just works.

## Per-module test scope rule

Source: [issue #168](https://github.com/duonghb53/ao-rs/issues/168).

**Test scope = what changed, not the entire codebase.**

| Situation                      | What to test                                                                |
|--------------------------------|-----------------------------------------------------------------------------|
| New module added               | Public interface of that module only                                        |
| Bug fixed                      | Write a failing test that reproduces the bug first, then fix it             |
| Refactor (behaviour unchanged) | Existing tests must pass — do **not** add new ones                          |
| Port feature from ao-ts        | Business logic of the ported code; skip type-plumbing                       |
| Config field added             | One round-trip serde test for that field                                    |

When you touch an existing module, add tests **for what you
changed**, not for what was already there.

### What Rust already proves — do NOT write tests for these

- `Option<T>` / `Result<T, E>` handling — the compiler enforces.
- Exhaustive enum matching — compile error if missed.
- Type mismatches — caught at compile time.
- Borrow / mutation safety — borrow checker.

A test that "exercises" one of the above is test-theatre: it fails
at compile time if it's ever wrong, and passes without running
otherwise. Skip it.

### Target coverage by layer

| Layer                                                   | Target | Reason                                  |
|---------------------------------------------------------|--------|-----------------------------------------|
| Pure business logic (state machines, parsers, slugs)    | High   | High value, cheap to test               |
| I/O wrappers (file, YAML r/w)                           | Medium | One happy-path + one error case         |
| Plugin trait impls (agent launch, runtime)              | Low    | Integration tested via CLI              |
| API route handlers                                      | Low    | Tested via dashboard integration tests  |
| `serde` derive structs                                  | Skip   | `serde` itself is proven                |

### Where tests live

- **Unit tests** — co-located as `#[cfg(test)] mod tests { … }` at
  the bottom of the module they cover. Matches existing files in
  `crates/ao-core/src/`.
- **Integration tests** — under the crate's `tests/` directory, one
  file per scenario. Example: `crates/plugins/scm-github/tests/`.
- **Doctests** — in `///` comments on public items. Useful when the
  example is short and the code is part of the public contract.

## Inner dev loop

```bash
# Fast type-check only (no binary, 3-5x faster than a full build):
cargo check -p <crate>

# Watch + re-run tests for the crate you're working in:
cargo watch -x "nextest run -p ao-core"

# Full suite before opening a PR (same as the manual release check):
cargo t --workspace
cargo test --doc --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

`cargo check` is the right tool when you just want to know "does
this compile?". It skips codegen and linking, so it's 3–5× faster
than `cargo build` for the same feedback on type errors.

## Pre-PR checklist

Before pushing:

- [ ] `cargo fmt --all -- --check` clean.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `cargo t --workspace` clean.
- [ ] `cargo test --doc --workspace` clean.
- [ ] Tests for **what you changed** are present (see scope rule
      above). No new tests in modules you didn't touch.
- [ ] Manual smoke if touching UI — see
      [`docs/SMOKE.md`](docs/SMOKE.md).

## Working on a dev-lifecycle feature

When an issue is large enough to need a plan, scaffold the
dev-lifecycle docs under `docs/ai/` and work through the phases:

```
docs/ai/requirements/feature-<slug>.md
docs/ai/design/feature-<slug>.md
docs/ai/planning/feature-<slug>.md
docs/ai/implementation/feature-<slug>.md   # optional
docs/ai/testing/feature-<slug>.md           # optional
```

See `docs/ai/*/README.md` for the phase templates and existing
features (e.g. `feature-agent-stuck-detection`, `feature-notifier-
routing`) for worked examples.

## Agent-specific rules

Agents spawned via `ao-rs spawn` inherit the rules from
[`ao-rs.yaml`](ao-rs.yaml) (the `ao-rs` project block). Keep those
in sync with this file — the content here is authoritative.

Current agent rules worth calling out:

- Source-port reference: `~/study/agent-orchestrator`. Check its
  logic when implementing or porting a feature.
- When spawned from an issue, follow the dev-lifecycle flow
  (requirements → design → planning → implement → verify).
- If stuck for more than 5 minutes, explain what's blocking you.

## Where things live

- **`crates/ao-core/`** — types, traits, state machine, reaction
  engine. Business logic lives here. High test coverage target.
- **`crates/ao-cli/`** — `ao-rs` binary (clap). Low-coverage target;
  tested via integration tests where it matters.
- **`crates/ao-dashboard/`** — axum REST + SSE. Low-coverage target;
  route handlers tested via integration tests.
- **`crates/plugins/`** — one crate per plugin impl (runtime, agent,
  workspace, SCM, tracker, notifier). Trait-impl smoke tests only —
  plugins are integration-tested through `ao-cli`.
- **`docs/`** — architecture, state machine, reactions, CLI ref,
  plugin spec, dev-lifecycle feature docs.

## CI

There are currently no CI workflows in this repo (the previous
`.github/workflows/ci.yml` and `release-artifacts.yml` were removed
in commit `e8e4c54`). When CI is re-introduced, the Rust job should
run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
```

(Substitute `cargo nextest run` for `cargo test` — the `cargo t`
alias only exists locally via `.cargo/config.toml`.)
