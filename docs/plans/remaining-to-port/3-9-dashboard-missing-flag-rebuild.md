# 3.9 dashboard missing flag rebuild

Status: planned

## Why

ao-ts dashboard supports `--rebuild`. ao-rs dashboard does not. Additionally, ao-ts opens browser by default, while ao-rs requires `--open`.

## Current state (ao-rs)

- `crates/ao-cli/src/commands/dashboard.rs` supports `--port`, `--interval`, `--open`.
- No `--rebuild`.

## Target behavior (ao-ts parity)

- `ao dashboard --rebuild` triggers rebuilding UI assets or refreshing cached state (define exact semantics).
- Default open behavior aligns with ao-ts (optional).

## Proposed approach

1. Define `--rebuild` in ao-rs context:
   - If desktop UI assets are built at compile-time, `--rebuild` may mean “clear caches” or “rebuild frontend dev bundle” (choose one).
2. Implement flag wiring and behavior.
3. Decide whether to flip default `--open` polarity or keep as divergence.

## Files to change

- `crates/ao-cli/src/cli/args.rs`
- `crates/ao-cli/src/commands/dashboard.rs`

## Acceptance criteria

- `ao-rs dashboard --rebuild` performs the defined rebuild action and starts successfully.
- Help text explains what rebuild does.

## Test plan

- CLI arg parse test.
- Unit test for rebuild action (if it only clears files/state).

## Risks / notes

- “Rebuild” can become ambiguous; keep the implementation narrowly scoped and documented.

