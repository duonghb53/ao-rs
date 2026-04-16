# 1.4 Session restore prompt redelivery

Status: planned

## Why

ao-ts restore re-delivers the initial prompt after restarting the runtime. ao-rs restore restarts the runtime but does not re-send the prompt, forcing manual `ao-rs send`.

## Current state (ao-rs)

- `crates/ao-core/src/restore.rs`
  - Restores runtime and persists new runtime handle
  - Does not build or send the initial prompt
- Prompt composition exists elsewhere:
  - `crates/ao-core/src/prompt_builder.rs`
  - `crates/ao-core/src/orchestrator_prompt.rs` (for orchestrator sessions)

## Target behavior (ao-ts parity)

- After runtime recreation, send the same initial prompt that would be sent at spawn-time so the agent can resume context.

## Proposed approach

1. In `restore.rs`, after `runtime.create(...)` succeeds:
   - Build the initial prompt from the restored `Session` context (and issue context if available).
2. Call `runtime.send_message(handle, prompt)` (or agent pathway used by spawn) to deliver it.
3. Decide failure semantics:
   - If prompt delivery fails, either treat restore as failed (strict) or restore runtime but emit a warning and allow manual resend (lenient). Match TS.

## Files to change

- `crates/ao-core/src/restore.rs`
  - Add prompt build + send step after runtime create, before final persist (or immediately after, with clear rollback strategy).
- (Optional) `crates/ao-core/src/prompt_builder.rs`
  - Expose helper for “spawn/restore initial prompt” if needed.

## Acceptance criteria

- `ao-rs session restore <id>` results in a live runtime **and** the agent receives the initial prompt automatically.
- Manual `ao-rs send` is no longer necessary for a restored session to resume work.

## Test plan

- Unit test with mock runtime that records `send_message` calls:
  - Verify restore calls `send_message` exactly once with a non-empty prompt.
- Ensure existing restore tests pass.

## Risks / notes

- Restored sessions may have stale issue context (if issue is fetched dynamically at spawn in TS). Decide whether to re-fetch issue or use persisted fields only.

