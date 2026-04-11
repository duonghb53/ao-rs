# ao-rs docs

This folder is a curated, Rust-flavored condensation of the key docs from the
TypeScript reference repo (`ComposioHQ/agent-orchestrator`), focused on what
matters for this port. It is **not** a 1:1 mirror — most of the TS docs are
aimed at npm users, the web dashboard, and 23 plugins we will never port.

## Index

| Doc | What it covers |
| --- | --- |
| [architecture.md](architecture.md) | Directory layout, disk format, how ao-rs differs from TS |
| [plugin-spec.md](plugin-spec.md) | Rust trait contracts, slot list, how plugins are wired |
| [state-machine.md](state-machine.md) | Full 17-state session lifecycle + transitions + events |
| [reactions.md](reactions.md) | Slice 2 plan: reaction engine design, config shape, event→key map |
| [cli-reference.md](cli-reference.md) | All `ao-rs` subcommands (current + planned) |

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
