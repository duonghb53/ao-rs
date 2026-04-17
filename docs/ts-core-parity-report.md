## TS core parity report (WIP)

Source: `/Users/haduong/study/agent-orchestrator/packages/core/src`

Goal: Port all modules + unit tests into Rust. This document tracks current parity status.

### Test suite inventory (TS)

| TS test | Primary modules | Rust status |
| --- | --- | --- |
| `__tests__/orchestrator-prompt.test.ts` | `orchestrator-prompt.ts` | **ported** (`crates/ao-core/src/orchestrator_prompt.rs`) |
| `__tests__/activity-log.test.ts` | `activity-log.ts` | **partial** (Rust has minimal `activity_log.rs`; missing classify/record helpers + ISO parsing) |
| `__tests__/opencode-session-id.test.ts` | `opencode-session-id.ts` | **missing** |
| `__tests__/utils.test.ts` | `utils.ts` | **missing** |
| `__tests__/config-validation.test.ts` | `config.ts` | **missing** (Rust config parsing exists but not TS validation rules) |
| `__tests__/config-generator.test.ts` | `config-generator.ts` | **partial** (Rust `generate_config` differs) |
| `__tests__/plugin-registry.test.ts` | `plugin-registry.ts` | **missing** |
| `__tests__/plugin-integration.test.ts` | plugin registry + config | **missing** |
| `__tests__/metadata.test.ts` | `metadata.ts`, `key-value.ts`, `atomic-write.ts` | **missing** |
| `__tests__/paths.test.ts` | `paths.ts` | **partial** (layout differs) |
| `__tests__/tmux.test.ts` | `tmux.ts` | **partial** (Rust tmux is in plugin crate, not ao-core) |
| `__tests__/types.test.ts` | `types.ts` | **partial** (many types differ) |
| `__tests__/observability.test.ts` | `observability.ts` | **missing** |
| `__tests__/feedback-tools.test.ts` | `feedback-tools.ts` | **missing** |
| `__tests__/orchestrator-session-strategy.test.ts` | `orchestrator-session-strategy.ts` | **missing** |
| `__tests__/lifecycle-manager.test.ts` | `lifecycle-manager.ts` | **partial** (Rust lifecycle differs) |
| `__tests__/session-manager*.test.ts` | `session-manager.ts` | **partial** (Rust uses YAML sessions + different plugin wiring) |
| `__tests__/recovery-*.test.ts` | `recovery/*` | **partial** (Rust has restore/cleanup, not full TS recovery system) |

### Next port slices (planned)
- Add Rust test harness utilities mirroring TS `test-utils.ts`
- Port pure helpers first (opencode id, utils)
- Then config validation, plugin registry, metadata kv store
- Then observability + feedback tools
- Then session-manager/lifecycle/recovery parity

## Parity-only modules

Several modules under `crates/ao-core/src/` are named `parity_*`. They were
ported from `packages/core/src/` for behavioral parity testing and are **not**
uniformly wired into the ao-rs runtime. Each module carries a `Parity status:`
tag in its `//!` header and is tracked in the guard test
`crates/ao-core/tests/parity_modules_meta.rs`, which fails if:

- A new `parity_*.rs` appears without being listed below, or
- An existing one is removed/renamed without updating the list, or
- A file is missing the `Parity status:` header line, or
- The `mixed` module stops re-exporting its production-used items from
  `lib.rs`.

Policy: **prefer incremental graduation tied to real runtime needs.** Don't
move parity code into production modules unless a concrete runtime caller
needs it. When a runtime need arises, move the function, update the
classification, and keep the parity test pointed at the production impl.

| Module | Status | TS source | Notes |
| --- | --- | --- | --- |
| `parity_utils.rs` | test-only | `packages/core/src/utils.ts` | Standalone helpers (`shell_escape`, `validate_url`, `is_git_branch_name_safe`, retry-config, JSONL tailer). Duplicate `shell_escape` lives in `runtime-tmux`, `agent-codex`, `agent-aider` plugins; consolidation deferred. |
| `parity_session_strategy.rs` | mixed | `packages/core/src/orchestrator-session-strategy.ts` | Enums `OrchestratorSessionStrategy` and `OpencodeIssueSessionStrategy` are re-exported from `ao_core` and used by `config.rs`. `decide_existing_session_action` is test-only (the runtime lifecycle code does not call it). |
| `parity_config_validation.rs` | test-only | `packages/core/src/config.ts` (validation rules) | Rust config (`config.rs`) has its own stricter validator; parity module exists as a regression harness only. |
| `parity_plugin_registry.rs` | test-only | `packages/core/src/plugin-registry.ts` | Rust plugin wiring lives in the workspace-level crate structure; this module mirrors the TS registry shape for comparison. |
| `parity_observability.rs` | test-only | `packages/core/src/observability.ts` | No runtime consumer; depends on `parity_metadata::atomic_write_file`. |
| `parity_metadata.rs` | test-only | `packages/core/src/metadata.ts`, `key-value.ts`, `atomic-write.ts` | Consumed only by other parity modules (`parity_observability`, `parity_feedback_tools`) and their tests. |
| `parity_feedback_tools.rs` | test-only | `packages/core/src/feedback-tools.ts` | No runtime consumer. |

