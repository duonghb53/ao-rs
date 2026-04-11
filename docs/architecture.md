# Architecture

This is a condensed, Rust-flavored port of `ARCHITECTURE.md` in the TS
reference, plus the pieces of `packages/core/README.md` that still apply.

## Scope of the port

The TS repo is an npm-publishable product with a web dashboard, a plugin
marketplace, a feedback-report pipeline, and 23 plugin packages. The Rust
port is a **learning project** — the goal is to deeply understand the
state machine, the reaction engine, and the plugin system by writing them
idiomatically in Rust. Feature parity is explicitly **not** a goal.

Not ported, not planned:

- Web dashboard (Next.js)
- Plugin marketplace / registry install flow
- `~/.agent-orchestrator/plugins/` external install store
- Zod / YAML config file
- Feedback tool contracts (`bug_report`, `improvement_suggestion`)
- GraphQL batch PR enrichment optimisation
- Multi-plugin slots (notifier, terminal, tracker, scm) — Slice 2 will do
  *one* implementation of tracker+scm, nothing else

## Disk layout

TS uses a hash-based, multi-project layout:

```
~/.agent-orchestrator/<sha256(configDir)[:12]>-<projectId>/
  sessions/
  worktrees/
  archive/
```

ao-rs uses a flat, single-home layout — no hashing, no config file:

```
~/.ao-rs/
  sessions/
    <projectId>/
      <session-uuid>.yaml      # one file per session
  lifecycle.pid                 # watch-daemon pidfile (Phase D)
  # future: worktrees/, reactions/
```

Worktrees themselves live under `~/.worktrees/<projectId>/<short-id>/`,
namespaced by the `workspace-worktree` plugin — see
`crates/ao-plugin-workspace-worktree/src/lib.rs`.

Sessions are **yaml, not key=value**, because serde_yaml is idiomatic and
lets the 17-variant `SessionStatus` enum round-trip cleanly. The TS
`metadata.ts` key=value format exists for bash-compat, which we don't
need.

## Crate structure

```
ao-rs/
├── crates/
│   ├── ao-core/                              # domain types + traits + state machine
│   │   ├── src/types.rs                      # Session, SessionStatus, ActivityState
│   │   ├── src/traits.rs                     # Runtime, Agent, Workspace, Scm, Tracker
│   │   ├── src/scm.rs                        # PR/CI/review domain types
│   │   ├── src/session_manager.rs            # disk CRUD + find_by_prefix (no cache)
│   │   ├── src/lifecycle.rs                  # polling loop + event bus
│   │   ├── src/events.rs                     # OrchestratorEvent enum
│   │   ├── src/restore.rs                    # session-restore helper
│   │   ├── src/config.rs                     # ~/.ao-rs/config.yaml loader (reactions only)
│   │   ├── src/reactions.rs                  # ReactionConfig/Action/Outcome data types
│   │   ├── src/reaction_engine.rs            # dispatch + retry + escalation (Slice 2 Phase D)
│   │   ├── src/lockfile.rs                   # PID-file RAII lock
│   │   ├── src/paths.rs                      # ~/.ao-rs/... path helpers
│   │   └── src/error.rs                      # AoError + Result
│   ├── ao-cli/                               # `ao-rs` binary (clap)
│   ├── ao-plugin-workspace-worktree/         # git worktree via shell-out
│   ├── ao-plugin-runtime-tmux/               # tmux via shell-out
│   ├── ao-plugin-agent-claude-code/          # claude-code adapter
│   ├── ao-plugin-scm-github/                 # gh-based GitHub SCM plugin (Slice 2 Phase B)
│   └── ao-plugin-tracker-github/             # gh-based GitHub Issues tracker (Slice 2 Phase C)
```

Plugin loading is **compile-time trait objects**, not dynamic discovery:
`ao-cli` imports each plugin crate and instantiates the concrete type
behind an `Arc<dyn Runtime>` / `Arc<dyn Agent>` / `Arc<dyn Workspace>` /
`Arc<dyn Scm>` / `Arc<dyn Tracker>`. This loses the plug-and-play story
from the TS marketplace but is a tiny fraction of the complexity.

## Design principles (repeated every commit)

1. **Shell-out over libraries.** `git`, `tmux`, `gh` are subprocesses, not
   crates. Easier to debug, mentally closer to the TS source, and dodges
   a whole class of FFI / build-time failures.
2. **Disk is the source of truth.** No in-memory session cache. Every
   `ao-rs status` does a fresh `read_dir` walk. The lifecycle loop
   re-reads on each tick. This is slow at N=1000 and perfect at N=30.
3. **Trait objects, not generics, at the plugin boundary.** Keeps the CLI
   clean and lets tests wire in mocks without a generic-parameter cascade.
4. **One crate per plugin**, even while we only have three. Makes the
   dependency story obvious and stops `ao-core` from accidentally
   pulling in tmux / git / gh at compile time.
5. **Comments explain *why*, and always reference the TS file the logic
   mirrors.** When a reader asks "why is this weird?" the answer is in
   the comment; when a reader asks "what did the original do differently?"
   they know which file to diff against.
6. **Never port file-by-file.** Read TS for intent; write idiomatic Rust.
   If something feels like a literal translation, stop and rewrite it.

## Intentional divergences from TS

| TS | ao-rs | Reason |
| --- | --- | --- |
| Hash-prefixed project dirs | Flat `~/.ao-rs/sessions/<project>/` | No multi-checkout scenario in a learning port |
| key=value session files | yaml | `serde_yaml` + enum round-trip is free |
| `git2` worktree creation (then falls back to shell) | always `git` shell-out | Matches principle #1; simpler to diff against TS |
| `pnpm` workspace, `vitest` | `cargo` workspace, `#[tokio::test]` | Native toolchain |
| Dynamic plugin loading from `~/.ao/plugins/` | Compile-time trait objects | #3 + scope |
| `eventEmitter.emit()` per subscriber | `tokio::sync::broadcast` | Cancellable, lossy-ok, native |
| `restore()` lives on SessionManager | Free fn in `ao_core::restore` | SessionManager doesn't own plugins here |

## Reading order for the state machine

If you're trying to understand the orchestrator core from scratch, read in
this order (1-2 hours, longest):

1. `crates/ao-core/src/types.rs` — domain types, `SessionStatus` variants
2. `crates/ao-core/src/traits.rs` — the five plugin contracts
3. `crates/ao-core/src/session_manager.rs` — the disk format + `find_by_prefix`
4. `crates/ao-core/src/lifecycle.rs` — the polling loop, tick fn, transitions
5. `crates/ao-core/src/events.rs` — what the loop emits
6. `crates/ao-core/src/restore.rs` — how a crashed session comes back
7. `crates/ao-core/src/reactions.rs` — reaction config types
8. `crates/ao-core/src/reaction_engine.rs` — dispatch, retry, escalation
9. `crates/ao-core/src/config.rs` — `~/.ao-rs/config.yaml` loader
10. `docs/state-machine.md` + `docs/reactions.md` — the bigger picture

Then compare against:

- `packages/core/src/lifecycle-manager.ts` (the TS equivalent of #4)
- `packages/core/src/session-manager.ts::restore()` (line 2254)

## Open architecture questions

These are parked until a slice forces us to decide:

- **Workspace::restore hook?** TS has an optional plugin method that
  recreates a missing worktree. We don't, and `restore_session` errors if
  the worktree is gone. A future Phase could add it.
- **Subprocess-plugin API?** TS's marketplace model eventually wants
  JSON-RPC / LSP-style subprocess plugins. Out of scope for the port.
- **SCM-driven status transitions.** Slice 2 Phase D wired the reaction
  engine into `LifecycleManager::transition`, but status transitions
  themselves are still only driven by runtime liveness + agent activity.
  A follow-up phase wires `Arc<dyn Scm>` into the polling loop so
  `Working → PrOpen/CiFailed/ChangesRequested/Mergeable` fires from real
  PR state. Until that phase lands, users exercise reactions by scripting
  transitions (or waiting for a future `ao-rs transition` debug command).
- **AutoMerge wiring.** The `approved-and-green` reaction currently emits
  a `ReactionTriggered` event with `action=auto-merge` but doesn't call
  `Scm::merge`. Same follow-up phase as SCM polling — once the loop holds
  an `Arc<dyn Scm>`, the engine can dispatch through it.
- **Duration-based `escalate-after`.** Phase D parses `Duration("10m")`
  but the engine ignores it (`tracing::trace!` + fall through to attempt
  counting). The parser lands next to the first reaction that actually
  needs wall-clock escalation (`agent-stuck`).

Answered / no longer open:

- ~~**Reaction engine as a separate task?**~~ Same task — it's driven by
  `LifecycleManager::transition` calling `ReactionEngine::dispatch` after
  each save/emit. Shares the broadcast channel via a cloned `Sender`.
- ~~**Config file?**~~ Yes, `~/.ao-rs/config.yaml`, loaded by
  `AoConfig::load_default()`. Only the `reactions:` subtree is read.
  Missing file is treated as "no reactions configured".
