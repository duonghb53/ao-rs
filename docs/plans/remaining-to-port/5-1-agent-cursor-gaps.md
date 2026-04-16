# 5.1 agent-cursor gaps

Status: planned

## Why

Cursor agent behavior diverges from ao-ts and misses features (`--append-system-prompt`, cost estimation). This affects prompt correctness and observability.

## Current state (ao-rs)

- `crates/plugins/agent-cursor/src/lib.rs`
  - Uses post-launch `send_message` to deliver prompt (like Claude Code), unlike TS embedding prompt in launch args.
  - No `--append-system-prompt` support.
  - `cost_estimate` returns `None`.

## Target behavior (ao-ts parity)

- Prompt delivery parity with TS (either embed prompt at launch or match TS semantics of when prompt is delivered).
- Support system-prompt append behavior.
- Provide cost estimate if Cursor logs contain token/cost metadata.

## Proposed approach

1. Decide prompt delivery strategy:
   - Keep post-launch send (consistent across agents), but ensure Cursor agent receives equivalent initial context.
2. Add `--append-system-prompt` support:
   - Extend Cursor invocation or prompt composition in plugin to prepend system rules.
3. Add cost estimation:
   - Parse Cursor logs if they contain token usage; else document as unsupported.

## Files to change

- `crates/plugins/agent-cursor/src/lib.rs`
- (Optional) `crates/ao-core/src/prompt_builder.rs` / config if system prompt rules are centralized

## Acceptance criteria

- Cursor agent receives the same prompt content as other agents (including system rules when configured).
- `ao-rs status --cost` shows non-empty cost for Cursor sessions when data is available (or clearly reports unsupported).

## Test plan

- Unit tests for prompt composition ordering (rules + user prompt).
- Unit tests for cost parsing with fixture logs if implemented.

## Risks / notes

- Cursor CLI capabilities differ across versions; keep flags guarded and fail gracefully.

