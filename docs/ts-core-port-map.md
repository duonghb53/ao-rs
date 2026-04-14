## TS core → ao-rs core mapping (port checklist)

Source TS checkout: `/Users/haduong/study/agent-orchestrator/packages/core/src`

This repo’s Rust core lives in `crates/ao-core/src/` and intentionally diverges in a few places (see `docs/architecture.md`).

Legend:
- **ported**: equivalent exists in Rust today
- **partial**: some behavior exists; not full TS parity
- **missing**: no Rust equivalent yet
- **out-of-scope**: intentionally not planned for ao-rs unless explicitly requested

### High-signal files (TS “core services”)

| TS file | Purpose (TS) | Rust equivalent | Status |
| --- | --- | --- | --- |
| `types.ts` | domain types + enums | `crates/ao-core/src/types.rs`, `crates/ao-core/src/scm.rs`, `crates/ao-core/src/reactions.rs` | partial |
| `config.ts` | config find/load/validate (Zod) | `crates/ao-core/src/config.rs` (`serde_yaml`) | partial |
| `config-generator.ts` | generate config from repo/url | `crates/ao-core/src/config.rs` (`detect_git_repo`, `generate_config`) | partial |
| `paths.ts` | hash-based instance/project layout | `crates/ao-core/src/paths.rs` (flat `~/.ao-rs/`) | ported (different layout) |
| `session-manager.ts` | spawn/restore/kill/send/cleanup | `crates/ao-core/src/session_manager.rs` + `crates/ao-cli/src/main.rs` wiring | partial |
| `lifecycle-manager.ts` | polling loop + state machine + reactions | `crates/ao-core/src/lifecycle.rs` + `crates/ao-core/src/reaction_engine.rs` | partial |
| `prompt-builder.ts` | layered prompt composition | `crates/ao-core/src/prompt_builder.rs` | partial |
| `orchestrator-prompt.ts` | orchestrator “manager session” prompt | (planned) `crates/ao-core/src/orchestrator_prompt.rs` | missing |
| `plugin-registry.ts` | dynamic plugin resolution/marketplace | (compile-time wiring in `ao-cli`) | out-of-scope |

### Utilities / helpers

| TS file | Purpose (TS) | Rust equivalent | Status |
| --- | --- | --- | --- |
| `utils.ts` | misc helpers (shell escape, retry config, URL validation, etc.) | scattered (`ao-cli` helpers, `ao-core` small helpers) | partial |
| `utils/validation.ts` | safe parsing/validation helpers | `ao-core` uses strong types + serde; some ad-hoc parsing | partial |
| `utils/session-from-metadata.ts` | build Session from metadata | Rust reads Session YAML directly | out-of-scope |
| `utils/pr.ts` | PR helpers | `crates/ao-core/src/scm.rs` + SCM plugins | partial |
| `atomic-write.ts` | atomic writes | Rust uses `std::fs::write` (non-atomic in general) | missing (optional) |
| `key-value.ts` | parse `key=value` format | Rust uses YAML sessions | out-of-scope |
| `metadata.ts` | key=value session metadata store | Rust uses YAML sessions | out-of-scope |

### Runtime / tmux wrappers

| TS file | Purpose (TS) | Rust equivalent | Status |
| --- | --- | --- | --- |
| `tmux.ts` | tmux wrappers | `crates/plugins/runtime-tmux` | ported (plugin) |

### Notifications / routing / reactions

| TS file | Purpose (TS) | Rust equivalent | Status |
| --- | --- | --- | --- |
| `notifier-resolution.ts` | resolve notifier config keys → plugin targets | `crates/ao-core/src/notifier.rs` registry | partial |
| `observability.ts` | correlation + metrics summaries | none | out-of-scope |
| `feedback-tools.ts` | tool contracts + report store | none | out-of-scope |

### Activity + agent workspace hooks

| TS file | Purpose (TS) | Rust equivalent | Status |
| --- | --- | --- | --- |
| `activity-log.ts` | JSONL fallback activity detection for agents lacking native logs | none | missing (evaluate) |
| `agent-workspace-hooks.ts` | shared PATH wrapper | none (agent plugins embed their own env) | partial/out-of-scope |
| `agent-selection.ts` | resolve agent per session/role | `ao-cli` selects agent; sessions persist agent/config | partial |
| `orchestrator-session-strategy.ts` | orchestrator vs worker strategy | none | missing/out-of-scope (depends on orchestrator session feature) |

### Recovery subsystem

| TS file(s) | Purpose (TS) | Rust equivalent | Status |
| --- | --- | --- | --- |
| `recovery/*` | scan/validate/repair sessions after crashes | partial restore + cleanup in Rust (`restore.rs`, `cleanup`) | partial |

### Webhook utils

| TS file | Purpose (TS) | Rust equivalent | Status |
| --- | --- | --- | --- |
| `scm-webhook-utils.ts` | parse webhook headers/timestamps | none | out-of-scope (ao-rs uses polling via `gh`) |

### Test suite (TS)

TS has a rich unit test suite under `__tests__/`. Rust has coverage across `ao-core` and plugin crates, but does not mirror every TS test 1:1.

