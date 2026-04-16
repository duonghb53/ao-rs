# 4.3 Agent trait stub defaults

Status: planned

## Why

Default `Agent` trait behavior is stubbed (`detect_activity` returns `Ready`, `cost_estimate` returns `None`). This limits observability and cost tracking unless every plugin implements these methods.

## Current state (ao-rs)

- `crates/ao-core/src/traits.rs`:
  - `detect_activity()` default returns `ActivityState::Ready`
  - `cost_estimate()` default returns `None`
- Some agent plugins implement richer behavior (e.g. Claude Code reads JSONL costs).

## Target behavior (ao-ts parity)

Better default activity detection and cost estimation behavior where possible, without forcing every plugin to implement it immediately.

## Proposed approach

1. Improve defaults safely:
   - Activity: if runtime indicates process is dead, surface `Exited` (or `Terminated`) even when agent plugin doesn’t implement activity logs.
   - Cost: allow optional parsing of a shared JSONL format if present in workspace (best-effort).
2. Provide helper utilities in ao-core that plugins can call:
   - `read_last_jsonl_entry`-style helpers already exist in tests/util modules; expose stable helpers if appropriate.
3. Keep plugin overrides authoritative.

## Files to change

- `crates/ao-core/src/traits.rs`
  - Update default implementations (carefully; avoid breaking semantics).
- (Optional) `crates/ao-core/src/activity_log.rs` or a new helper module
  - Shared best-effort JSONL parsing for activity/cost.

## Acceptance criteria

- A session with a dead runtime process is reported as terminal/exited even if agent plugin doesn’t implement activity detection.
- Cost remains optional but is populated when standard logs exist.

## Test plan

- Unit tests for default activity behavior with mock runtime alive/dead.
- Unit tests for best-effort cost parsing helper with fixture logs.

## Risks / notes

- Avoid false positives: do not infer activity from noisy signals unless confidence is high.

