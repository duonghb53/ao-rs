# 5.3 agent-codex gaps

Status: done

## Why

Codex agent cost semantics differ (USD fixed to 0.0) and the launch mode is interactive (`--full-auto`) rather than TS `codex exec` style. This affects automation and cost reporting.

## Current state (ao-rs)

- `crates/plugins/agent-codex/src/lib.rs`
  - Runs interactive `codex --full-auto`.
  - `cost_estimate` aggregates tokens best-effort but sets `cost_usd = 0.0`.

## Target behavior (ao-ts parity)

- Align launch mode and cost reporting as closely as practical.

## Proposed approach

1. Decide whether to support `codex exec` mode:
   - If supported, prefer exec for deterministic runs.
2. Improve cost reporting:
   - If codex logs provide reliable USD cost, parse it.
   - Otherwise leave USD as `None` rather than `0.0` to avoid misleading output.
3. Document supported modes and limitations clearly.

## Files to change

- `crates/plugins/agent-codex/src/lib.rs`
- (Optional) `crates/ao-core/src/types.rs` cost model if switching `0.0` → `Option<f64>`

## Acceptance criteria

- Cost output is not misleading (no fake 0.0 USD unless truly known).
- Codex sessions can run in a deterministic mode if supported by installed codex.

## Test plan

- Unit tests for cost parsing with fixture logs.
- Unit tests for command assembly for chosen mode.

## Risks / notes

- Codex CLI output format may change; parsing must be best-effort and resilient.

