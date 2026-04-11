# Plugin Spec

Condensed from `docs/PLUGIN_SPEC.md` in the TS reference, rewritten for the
Rust port's compile-time trait-object model.

## Runtime contract

A plugin is a regular Rust struct that implements **one** of the traits
defined in `ao_core::traits`. That's it — no manifests, no version metadata,
no dynamic discovery. The `ao-cli` crate imports the plugin crate and
instantiates it behind an `Arc<dyn Trait>` when wiring up commands.

```rust
// crates/ao-plugin-runtime-tmux/src/lib.rs
use ao_core::Runtime;
use async_trait::async_trait;

pub struct TmuxRuntime;

#[async_trait]
impl Runtime for TmuxRuntime {
    async fn create(&self, session_id: &str, cwd: &Path,
                    launch_command: &str, env: &[(String, String)]) -> Result<String> { ... }
    async fn send_message(&self, handle: &str, msg: &str) -> Result<()> { ... }
    async fn is_alive(&self, handle: &str) -> Result<bool> { ... }
    async fn destroy(&self, handle: &str) -> Result<()> { ... }
}
```

And on the CLI side:

```rust
let runtime: Arc<dyn Runtime> = Arc::new(TmuxRuntime::new());
let agent:   Arc<dyn Agent>   = Arc::new(ClaudeCodeAgent::new());
```

## Supported slots

The TS reference has seven plugin slots. Slice 0/1 implements three; Slice
2 adds two more; the rest are stubbed as "never going to port".

| Slot | Trait | Slice | Crate | Status |
| --- | --- | --- | --- | --- |
| runtime | `Runtime` | 0 | `ao-plugin-runtime-tmux` | ✓ done |
| agent | `Agent` | 0 | `ao-plugin-agent-claude-code` | ✓ done (stub activity detection) |
| workspace | `Workspace` | 0 | `ao-plugin-workspace-worktree` | ✓ done |
| tracker | `Tracker` | 2 | `ao-plugin-tracker-github` (planned) | 🚧 Slice 2 |
| scm | `Scm` | 2 | `ao-plugin-scm-github` (planned) | 🚧 Slice 2 |
| notifier | `Notifier` | 3 | — | 💭 stretch |
| terminal | — | — | — | ❌ not ported |

The `tracker` and `scm` traits do not exist yet — Slice 2 defines them.
See `docs/reactions.md` for the concrete method list we need.

## The three existing traits (Slice 0/1)

All defined in `crates/ao-core/src/traits.rs`. Full signatures live in the
file; the cheat-sheet below is intent-only.

### `Runtime`

*How an agent process is executed (tmux, raw process, docker, …).*

- `create(session_id, cwd, launch_command, env) -> handle` — spawn isolated
  execution context and return an opaque handle the caller will store
  in `Session::runtime_handle`.
- `send_message(handle, msg)` — deliver text to the running process.
- `is_alive(handle) -> bool` — polled by `LifecycleManager::tick`.
- `destroy(handle)` — best-effort teardown.

### `Workspace`

*How a session's working directory is materialized (git worktree, clone, …).*

- `create(WorkspaceCreateConfig) -> PathBuf` — carve out an isolated repo copy.
- `destroy(workspace_path)` — best-effort cleanup.

### `Agent`

*A specific AI coding tool (Claude Code, Codex, Aider, …).*

- `launch_command(&session) -> String` — single shell line the runtime runs.
- `environment(&session) -> Vec<(String, String)>` — env vars to merge.
- `initial_prompt(&session) -> String` — first thing to `send_message` after launch.
- `async detect_activity(&session) -> ActivityState` — polled per tick;
  default impl returns `Ready` so plugins opt in gradually.

## How a session flows through the plugins

This mirrors `spawn()` in `packages/core/src/session-manager.ts`.

1. CLI generates a fresh `SessionId` + short id + branch name.
2. `Workspace::create` → gets back a `PathBuf` for the worktree.
3. `Session` struct is built and persisted via `SessionManager::save`
   with `status: Spawning`.
4. `Agent::launch_command` + `Agent::environment` + `Agent::initial_prompt`
   are called on the CLI side — no I/O, pure metadata.
5. `Runtime::create` spawns tmux, returns a handle.
6. Session is updated to `status: Working`, `runtime_handle: Some(...)`,
   persisted again.
7. CLI sleeps briefly (tmux needs to finish drawing) then calls
   `Runtime::send_message` with the initial prompt.
8. `LifecycleManager` takes over polling on the next tick.

## Testing plugins

Mock plugins are the primary unit-test fixture. Pattern used in
`crates/ao-core/src/lifecycle.rs::tests` and `crates/ao-core/src/restore.rs::tests`:

```rust
struct MockRuntime { alive: AtomicBool }
#[async_trait]
impl Runtime for MockRuntime { ... }
```

Each plugin crate owns its own integration tests for real I/O (tmux,
git worktree). See `crates/ao-plugin-workspace-worktree/tests/integration.rs`.

## What we are explicitly not doing

- **No `PluginModule` trait with manifest metadata.** TS needs it because
  plugins are npm packages discovered at runtime. We don't.
- **No plugin install store (`~/.ao-rs/plugins/`).** Plugins are workspace
  members; `cargo add` handles installation.
- **No per-plugin config YAML.** Rust types take `&self`; if a plugin
  needs config, make it a `::new(...)` argument.
- **No `detect()` function.** TS uses this to see if a CLI (`claude`, `gh`)
  is on $PATH before offering the plugin. The Rust port crashes loudly at
  first invocation, which is fine for a learning project.

## Adding a new plugin (future slice)

1. Create `crates/ao-plugin-<slot>-<name>/` via `cargo new --lib`.
2. Add to workspace `Cargo.toml` members list.
3. Add `ao-core = { workspace = true }` and whatever crates the impl needs.
4. Implement the relevant trait. Put the business logic in the crate, not
   in `ao-cli`.
5. Wire it up in `ao-cli/src/main.rs` as an `Arc<dyn Trait>` — usually a
   one-line change.
6. Unit-test with a mock fixture mirroring the ones in `ao-core`.
7. Integration-test with real I/O only if the plugin does real I/O.
