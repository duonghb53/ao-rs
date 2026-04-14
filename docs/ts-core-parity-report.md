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

