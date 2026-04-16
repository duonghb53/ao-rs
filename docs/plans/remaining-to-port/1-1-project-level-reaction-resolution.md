# 1.1 Project-level reaction resolution

Status: planned

## Why

`projects.<id>.reactions` exists in config but is **silently ignored** at runtime. This makes per-project overrides impossible and breaks ao-ts parity (`getReactionConfigForSession`).

## Current state (ao-rs)

- Config shape supports per-project overrides:
  - `crates/ao-core/src/config.rs`: `ProjectConfig.reactions: HashMap<String, ReactionConfig>`
- Reaction dispatch uses global map only:
  - `crates/ao-core/src/reaction_engine.rs`: `dispatch()` reads `self.config.get(reaction_key)`
- Lifecycle triggers dispatch:
  - `crates/ao-core/src/lifecycle.rs`: on status transition → `engine.dispatch(session, key)`

## Target behavior (ao-ts parity)

- Merge global + project reaction config:
  - global `reactions[key]` provides defaults
  - project `projects.<id>.reactions[key]` overrides fields (project wins)

## Proposed approach

1. Add a resolver that returns the effective `ReactionConfig` for a given `(session, key)`:
   - Prefer project override when present, else fall back to global.
   - If both exist, merge: copy global then overlay project fields.
2. Thread `AoConfig` (or just per-project reactions) into `ReactionEngine` so it can look up the session’s project config.
3. Update `dispatch()` to call the resolver rather than using only the global map.

## Files to change

- `crates/ao-core/src/reaction_engine.rs`
  - Add `fn resolve_reaction_config(&self, session: &Session, key: &str) -> Option<ReactionConfig>`
  - Store config reference (e.g. `Arc<AoConfig>` or `Arc<HashMap<String, ProjectConfig>>`)
  - Update `dispatch()` to use resolved config
- `crates/ao-cli/src/commands/watch.rs` and `crates/ao-cli/src/commands/dashboard.rs`
  - When constructing engine, pass config reference in
- (Optional) `crates/ao-core/src/lifecycle.rs` test wiring
  - Add unit test to prove project overrides win

## Acceptance criteria

- If `projects.A.reactions.ci-failed.auto=false`, a `ci-failed` transition in project A does **not** run the global `ci-failed` action.
- If a project defines a reaction key not present globally, it still works.
- Existing behavior unchanged when `projects.*.reactions` is empty.

## Test plan

- Add a unit test in `reaction_engine.rs` (or `ao-core/tests/`) that:
  - Builds global reactions + a project override
  - Creates a `Session { project_id: ... }`
  - Asserts the resolved config equals expected merged config

## Risks / notes

- `ReactionConfig` has boolean fields (e.g. `auto`) where “unset vs default” cannot be distinguished; define merge rule as “project value always wins” for booleans.
- Keep merge minimal; don’t refactor unrelated reaction semantics.

