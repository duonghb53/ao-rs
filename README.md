# ao-rs

Rust port of [agent-orchestrator](https://github.com/ComposioHQ/agent-orchestrator) — spawn parallel AI coding agents, each in its own git worktree, with automatic CI-failure fixes, review-comment routing, and PR auto-merge.

This is a **learning project**: the goal is to deeply understand the state machine, reaction engine, and plugin system by rewriting them idiomatically in Rust. Feature parity with the TS original is explicitly not a goal.

## Features

- **Isolated sessions** — each agent gets its own git worktree and tmux session
- **18-state lifecycle** — `spawning → working → pr_open → ci_failed / changes_requested / approved → mergeable → merged`
- **Reaction engine** — auto-responds to CI failures, review comments, stuck agents, and approved PRs
- **Notification routing** — fan-out by priority to stdout, [ntfy.sh](https://ntfy.sh), or any custom `Notifier` plugin
- **SCM integration** — GitHub PR state, CI status, review decisions, and auto-merge via `gh` CLI
- **Agent-stuck detection** — configurable idle threshold flips sessions to `stuck` and notifies
- **Session restore** — revive crashed sessions without losing worktree state
- **268 tests** — unit + integration, clippy clean, fmt clean

## Prerequisites

| Tool | Version | Purpose |
|------|---------|---------|
| Rust | 1.80+ | Build |
| git | 2.25+ | Worktree creation |
| tmux | any | Agent runtime |
| gh | 2.x | GitHub SCM/tracker (must be `gh auth login`'d) |
| claude | any | Claude Code agent (optional with `--no-prompt`) |

## Quick start

```bash
# Build
cargo build --release

# Spawn a session
ao-rs spawn --task "fix the failing tests" --repo ~/my-project --project myapp

# Watch the lifecycle loop (another terminal)
ao-rs watch

# Check status
ao-rs status
ao-rs status --pr   # include GitHub PR info

# Send a follow-up message to an agent
ao-rs send 3a4b5c6d "also update the README"

# View PR details
ao-rs pr 3a4b5c6d

# Restore a crashed session
ao-rs session restore 3a4b5c6d
```

## Configuration

Optional config at `~/.ao-rs/config.yaml`. Missing file = sensible defaults (no reactions, stdout-only notifications).

```yaml
# Reaction engine — what to do when events fire
reactions:
  ci-failed:
    auto: true
    action: send-to-agent
    message: "CI failed. Read the logs, fix the issue, and push again."
    retries: 3
    escalate_after: 3        # escalate to notify after 3 attempts

  changes-requested:
    auto: true
    action: send-to-agent
    retries: 2
    escalate_after: 30m      # or use a duration

  approved-and-green:
    auto: true               # set to false for manual merge
    action: auto-merge
    priority: info

  agent-stuck:
    auto: true
    action: notify
    threshold: 10m           # idle time before flagging
    priority: warning

# Notification routing — which notifiers handle which priorities
notification_routing:
  urgent: [stdout, ntfy]
  action: [stdout, ntfy]
  warning: [stdout]
  info: [stdout]
```

### Environment variables

| Variable | Purpose |
|----------|---------|
| `AO_NTFY_TOPIC` | ntfy.sh topic name — enables the ntfy notifier |
| `AO_NTFY_URL` | Custom ntfy server URL (default: `https://ntfy.sh`) |
| `RUST_LOG` | Log level filter (default: `warn,ao_core=info`) |

## Architecture

```
ao-rs/
├── crates/
│   ├── ao-core/                          # Domain types, traits, state machine, reaction engine
│   ├── ao-cli/                           # `ao-rs` binary (clap)
│   ├── ao-plugin-workspace-worktree/     # git worktree via shell-out
│   ├── ao-plugin-runtime-tmux/           # tmux session management
│   ├── ao-plugin-agent-claude-code/      # Claude Code adapter
│   ├── ao-plugin-scm-github/            # GitHub SCM via gh CLI
│   ├── ao-plugin-tracker-github/        # GitHub Issues tracker via gh CLI
│   ├── ao-plugin-notifier-stdout/       # Terminal notification (always on)
│   └── ao-plugin-notifier-ntfy/         # ntfy.sh HTTP POST notifications
```

### Plugin slots

| Slot | Trait | Plugin | Status |
|------|-------|--------|--------|
| Runtime | `Runtime` | `ao-plugin-runtime-tmux` | Done |
| Agent | `Agent` | `ao-plugin-agent-claude-code` | Done |
| Workspace | `Workspace` | `ao-plugin-workspace-worktree` | Done |
| SCM | `Scm` | `ao-plugin-scm-github` | Done |
| Tracker | `Tracker` | `ao-plugin-tracker-github` | Done |
| Notifier | `Notifier` | `ao-plugin-notifier-stdout` | Done |
| Notifier | `Notifier` | `ao-plugin-notifier-ntfy` | Done |

Plugins are compile-time trait objects — no dynamic discovery, no registry. `ao-cli` imports each crate and wires them behind `Arc<dyn Trait>`.

### Design principles

1. **Shell-out over libraries** — `git`, `tmux`, `gh` are subprocesses, not crates
2. **Disk is the source of truth** — no in-memory session cache; every read walks `~/.ao-rs/sessions/`
3. **Trait objects at plugin boundaries** — keeps the CLI clean, lets tests use mocks
4. **One crate per plugin** — clear dependency boundaries
5. **Comments explain *why*** — and reference the TS file the logic mirrors
6. **Never port file-by-file** — read TS for intent, write idiomatic Rust

### Disk layout

```
~/.ao-rs/
  config.yaml                              # reactions + notification routing
  lifecycle.pid                            # watch-daemon lock file
  sessions/
    <project>/
      <session-uuid>.yaml                  # one file per session

~/.worktrees/
  <project>/
    <short-id>/                            # git worktree per session
```

## CLI reference

| Command | Description |
|---------|-------------|
| `ao-rs spawn --task "..." [--repo PATH] [--project NAME]` | Create a new agent session |
| `ao-rs status [--project NAME] [--pr]` | List all sessions |
| `ao-rs watch [--interval SECS]` | Run the lifecycle polling loop |
| `ao-rs send <session> "<message>"` | Send a message to a running agent |
| `ao-rs pr <session>` | Show PR state, CI, review, mergeability |
| `ao-rs session restore <session>` | Restore a terminated session |

See [docs/cli-reference.md](docs/cli-reference.md) for full details.

## How it works

1. **`ao-rs spawn`** creates a git worktree, starts a tmux session, launches `claude`, and sends the initial task prompt
2. **`ao-rs watch`** polls every 5s per session:
   - Probes the tmux runtime for liveness
   - Detects agent activity state (active, ready, idle, exited)
   - Polls GitHub for PR state, CI status, review decisions
   - Derives status transitions via a pure decision function
   - Dispatches reactions (send-to-agent, notify, auto-merge) with retry + escalation
   - Detects stuck agents via idle-time thresholds
3. **Reactions** close the loop — CI failure → agent gets the logs and retries; review comments → agent addresses them; approved + green → auto-merge fires

## State machine

```
spawning → working → pr_open
                   ↓
              ci_failed, review_pending, changes_requested
                   ↓
              approved → mergeable ↔ merge_failed → merged
                   ↓                    ↓
              needs_input, stuck     cleanup

terminal: errored, killed, terminated, done, idle, merged, cleanup
```

See [docs/state-machine.md](docs/state-machine.md) for the full transition table.

## What's not ported

This is intentionally scoped for learning, not production:

- Web dashboard
- Plugin marketplace / dynamic discovery
- GraphQL PR batching (uses `gh` CLI per session — fine at N≤30)
- Hot-reload of config
- Desktop / Slack / email notifiers
- Notification retry/backoff ladder
- Template engine for notification bodies
- Rate limiting / dedup
- `ao-rs stop` / `ao-rs kill` commands
- Observability / metrics / correlation IDs

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

## Docs

| Document | Content |
|----------|---------|
| [architecture.md](docs/architecture.md) | Crate structure, disk layout, design principles, TS divergences |
| [state-machine.md](docs/state-machine.md) | 18-state lifecycle, transitions, PR-driven logic, stuck detection |
| [reactions.md](docs/reactions.md) | Reaction engine design, config shape, event→key map |
| [cli-reference.md](docs/cli-reference.md) | All CLI subcommands with flags and examples |
| [plugin-spec.md](docs/plugin-spec.md) | Plugin trait contracts, slot list, how to add a plugin |

## License

MIT
