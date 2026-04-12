# ao-rs Benchmarks

Performance and feature comparison between **ao-rs** (this project, Rust) and **ao-ts** — the original [Agent Orchestrator](https://github.com/ComposioHQ/agent-orchestrator) written in TypeScript/Node.js that ao-rs is ported from.

> **Reproduce:** `./scripts/benchmark.sh ~/path/to/agent-orchestrator`

---

## Environment

| | |
|--|--|
| **Machine** | Apple M3 Pro, 18 GB RAM, macOS Tahoe 26.3 |
| **ao-rs** | Rust 1.89.0, release build (`cargo build --release`) |
| **ao-ts** | Node.js 18.19.1, TypeScript 5.7, run via `npx ao` (no pre-warm) |
| **Methodology** | Startup/memory: avg of 5 runs; build: clean single-crate |
| **Date** | 2026-04-12 |

---

## Startup Time

`status` command — time from process launch to exit.

| | ao-rs | ao-ts | Difference |
|--|--|--|--|
| **Avg (5 runs)** | **28 ms** | 770 ms | **27× faster** |
| **`--help`** | 9 ms | 612 ms | 68× faster |

ao-rs starts before Node.js has even finished loading the runtime.

---

## Memory Usage

Peak RSS (`/usr/bin/time -l`) running `status`.

| | ao-rs | ao-ts | Difference |
|--|--|--|--|
| **Peak RSS** | **9.1 MB** | 86.9 MB | **9.5× less** |

ao-rs has no garbage collector, no V8 heap, no JIT warm-up.

---

## Binary / Install Size

| | ao-rs | ao-ts |
|--|--|--|
| **Distributable** | **7.1 MB** single binary | ~180 MB `node_modules` |
| **Runtime required** | None | Node.js 20+ |
| **Install** | `cargo install` | `npm install` + `npx` |

Copy the `ao-rs` binary anywhere and it runs. No Node.js, no npm.

---

## Build Time

| | ao-rs | ao-ts |
|--|--|--|
| **Release build** | 12 s (`cargo build --release -p ao-cli`) | ~30 s (`tsc` + bundler) |
| **Incremental** | **< 2 s** (single crate touch) | ~10–15 s |

Rust's incremental compilation keeps inner-loop iteration fast.

---

## Codebase Metrics

| Metric | ao-rs | ao-ts |
|--|--|--|
| **Source files** | 36 `.rs` files | 370 `.ts`/`.tsx` files |
| **Lines of code** | **16,453** | 12,788 |
| **Test files / suites** | 36 (inline) | 158 test files |
| **Tests passing** | **310** | — |
| **Dependencies** | `Cargo.lock` only | 180+ MB `node_modules` |
| **Runtime** | None | Node.js 20+ |

ao-rs has more lines because Rust is explicit about types and error handling — but far fewer files and zero runtime dependencies.

---

## Feature Comparison

### CLI Commands

| Command | ao-rs | ao-ts |
|--|--|--|
| `start` / `init` | ✅ | ✅ |
| `spawn` | ✅ | ✅ |
| `status [--pr] [--cost]` | ✅ | ✅ (no `--cost`) |
| `watch` | ✅ | ✅ (`lifecycle-worker`) |
| `dashboard` | ✅ REST + SSE API | ✅ Next.js web app |
| `send` | ✅ | ✅ |
| `pr` | ✅ | ✅ (`review-check`) |
| `session restore` | ✅ | ✅ |
| `batch spawn` | ❌ | ✅ |
| `open` | ❌ | ✅ |
| `doctor` / `verify` | ❌ | ✅ |
| `plugin` management | ❌ | ✅ |

### API Endpoints

| Endpoint | ao-rs | ao-ts |
|--|--|--|
| `GET /api/sessions` | ✅ | ✅ |
| `GET /api/sessions/:id` | ✅ | ✅ |
| `POST /api/sessions/:id/message` | ✅ | ✅ |
| `POST /api/sessions/:id/kill` | ✅ | ✅ |
| `GET /api/events` (SSE) | ✅ | ✅ (`/api/sessions/patches`) |
| `POST /api/sessions/:id/restore` | ❌ | ✅ |
| `GET /api/projects` | ❌ | ✅ |
| `GET /api/backlog` | ❌ | ✅ |
| `GET /api/observability` | ❌ | ✅ |
| `POST /api/prs/:id/merge` | ❌ | ✅ |
| `POST /ws/terminal/:id` | ❌ *(planned)* | ✅ |

### Plugins

| Slot | ao-rs | ao-ts |
|--|--|--|
| **Runtime** | tmux | tmux, process, docker, k8s, ssh, e2b |
| **Agent** | Claude Code | Claude Code, Codex, Aider, Cursor, OpenCode |
| **Workspace** | git worktree | worktree, clone |
| **Tracker** | GitHub Issues | GitHub, Linear, GitLab |
| **SCM** | GitHub | GitHub, GitLab |
| **Notifier** | stdout, ntfy, desktop, discord | desktop, slack, discord, webhook, composio, openclaw |
| **Terminal** | *(planned: Tauri)* | iterm2, web |

### Features Unique to ao-rs

| Feature | Description |
|--|--|
| **Per-session cost tracking** | Token counts + USD from Claude Code JSONL logs |
| **Monthly cost ledger** | `~/.ao-rs/cost-ledger/YYYY-MM.yaml` — survives session deletion |
| **`ao-rs status --cost`** | Cost column in the session table |
| **MergeFailed parking loop** | `Mergeable ↔ MergeFailed` retry with budget for flaky merges |
| **Duration-based escalation** | `escalate_after: 30m` alongside attempt-count escalation |
| **Agent rules injection** | `--append-system-prompt` with 6-step dev lifecycle |
| **Single binary** | No Node.js, no npm, no runtime dependency |

### Features Unique to ao-ts

| Feature | Description |
|--|--|
| **Web dashboard** | Full Next.js 15 UI with kanban, terminal, PR management |
| **Backlog poller** | Auto-discovers and claims issues labeled `agent:backlog` |
| **Batch spawn** | Spawn multiple agents from a task list |
| **Orchestrator linking** | Agent-spawned sub-agents with role distinction |
| **Plugin marketplace** | Runtime plugin loading and management |
| **Terminal UI** | xterm.js in browser + iterm2 native integration |
| **Observability** | Activity logs, metrics, lifecycle events dashboard |
| **Multi-agent runtimes** | Docker, k8s, SSH, e2b support |

---

## Reproduce

```bash
# Clone ao-rs
git clone https://github.com/duonghb53/ao-rs
cd ao-rs

# Build release binary
cargo build --release

# Run benchmark (point at your ao-ts checkout)
./scripts/benchmark.sh ~/path/to/agent-orchestrator
```

The script outputs startup time (avg 5 runs), peak RSS, binary size, build time, and codebase metrics for both projects side by side.
