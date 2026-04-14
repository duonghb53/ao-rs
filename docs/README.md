# ao-rs docs

Curated, Rust-flavored documentation for the ao-rs port of
[agent-orchestrator](https://github.com/ComposioHQ/agent-orchestrator).
Not a 1:1 mirror of the TS docs — focused on what this port implements.

## Index

| Doc | What it covers |
| --- | --- |
| [DEV.md](DEV.md) | Local dev: `ao-rs dashboard`, Vite UI, Tauri pointer |
| [SMOKE.md](SMOKE.md) | Manual smoke checklist before releases |
| [RELEASE.md](RELEASE.md) | Local install + release workflow strategy |
| [architecture.md](architecture.md) | Crate structure, disk layout, design principles, TS divergences |
| [plugin-spec.md](plugin-spec.md) | All 6 plugin traits (Runtime, Agent, Workspace, Scm, Tracker, Notifier), session flow |
| [state-machine.md](state-machine.md) | 18-state session lifecycle, PR transitions, stuck detection, merge_failed loop |
| [reactions.md](reactions.md) | Reaction engine, config shape, notification routing, auto-merge, escalation |
| [cli-reference.md](cli-reference.md) | All `ao-rs` subcommands with flags and examples |

## Source-of-truth pointers

When implementing a new slice, the first thing to do is re-read the TS
equivalent. These are the files that actually encode the design:

| Concept | TS file (in `~/study/agent-orchestrator`) |
| --- | --- |
| All domain types (Runtime, Agent, Workspace, SCM, …) | `packages/core/src/types.ts` |
| State machine + polling loop + reaction engine | `packages/core/src/lifecycle-manager.ts` |
| Session CRUD, spawn, restore | `packages/core/src/session-manager.ts` |
| Plugin discovery + registration | `packages/core/src/plugin-registry.ts` |
| CLI command wiring | `packages/cli/src/program.ts`, `packages/cli/src/commands/*` |
| PID-file daemon lock | `packages/cli/src/lib/lifecycle-service.ts` |
| tmux runtime reference | `packages/plugins/runtime-tmux/src/index.ts` |
| claude-code agent reference | `packages/plugins/agent-claude-code/src/index.ts` |
| GitHub SCM plugin | `packages/plugins/scm-github/src/index.ts` |
| GitHub tracker plugin | `packages/plugins/tracker-github/src/index.ts` |

When the Rust code here diverges from the TS reference, the divergence is
intentional and (should be) commented inline. `docs/architecture.md` lists the
big ones at a glance.
