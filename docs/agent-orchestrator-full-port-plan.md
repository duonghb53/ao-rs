---
title: "Agent Orchestrator → ao-rs Full Port Plan"
owner: "ao-rs"
status: draft
---

# Agent Orchestrator → ao-rs Full Port Plan (Referenced)

This document describes an end-to-end plan to port the **Agent Orchestrator (TS)** product into this repo’s **Rust + Tauri** implementation, **referencing the upstream implementation plan** in `ComposioHQ/agent-orchestrator` (`artifacts/implementation-plan.md`).

Source UI/codebase: `../agent-orchestrator/`.

## Goals
- Deliver a **desktop dashboard** (Tauri) with feature parity to `packages/web` for core workflows.
- Keep orchestration logic in Rust (`ao-core`, `ao-cli`, `ao-dashboard`) and treat the desktop app as a client of `ao-dashboard`.
- Port incrementally, keeping the system runnable at each milestone.

## Non-goals (for the first full port)
- Multi-user auth/permissions model (local single-user tool).
- Marketplace-style dynamic plugin discovery (Rust stays compile-time plugin wiring).
- Perfect SSR parity with Next.js (desktop UI is client-rendered).

## Current baseline in ao-rs (as of this plan)
- `ao-dashboard` provides REST + SSE and a read-only terminal snapshot WS endpoint:
  - `GET /api/sessions`
  - `GET /api/sessions/{id}`
  - `POST /api/sessions/{id}/message`
  - `POST /api/sessions/{id}/kill`
  - `GET /api/events` (SSE)
  - `GET /api/sessions/{id}/terminal` (WebSocket; tmux `capture-pane` snapshots)
- `ao-desktop` (Tauri v2) hosts a Vite+React UI.

## Upstream “full feature set” (TS) summary
Upstream breaks the product into:
- **Core services**: metadata/session manager/lifecycle/reactions/event bus
- **Plugins**: runtime/workspace/agent/SCM/tracker/notifier/terminal
- **CLI**: spawn/status/send/review-check/dashboard/open
- **Dashboard UI**: attention zones + session detail + terminal embed

The plan below maps those areas onto ao-rs crates and incremental milestones.

## Milestones
- **M1: Core parity** — lifecycle + state machine + reactions behave like TS for supported slices
- **M2: Plugin parity** — runtime/agent/workspace/scm/tracker/notifier slots cover the TS baseline set
- **M3: API parity** — `ao-dashboard` exposes the data needed by the dashboard UI (REST + SSE + WS)
- **M4: Desktop UI parity** — Tauri UI reaches feature parity with `packages/web` for core workflows
- **M5: Terminal parity** — interactive terminal (input + streaming) with robust transport
- **M6: Packaging** — docs + build/release workflow + smoke tests

## Work breakdown (phased)

### Phase 0 — Foundation (interfaces + config + registry)
Upstream (TS) starts by defining all interfaces/types/config/registry before parallel work.

ao-rs mapping:
- `crates/ao-core/src/types.rs` (Session/Status/Activity/Events)
- `crates/ao-core/src/traits.rs` (Runtime/Agent/Workspace/Scm/Tracker)
- `crates/ao-core/src/scm.rs` + notifier types
- `crates/ao-core/src/config.rs` (config parsing; TS uses Zod, Rust uses `serde_yaml`)
- Compile-time “registry” is `ao-cli` wiring (intentional divergence; document it)

### Phase 1 — Core (ao-core) parity
- [ ] **State machine**: ensure `SessionStatus`, `ActivityState`, terminal/restorable sets match TS semantics
- [ ] **Lifecycle polling**: runtime + agent activity + SCM polling + one-transition-per-tick invariant
- [ ] **Reaction engine**:
  - [ ] retry accounting + duration-based escalation
  - [ ] auto-merge and merge-failure retry loop
  - [ ] stuck/needs-input/exit/idleness handling parity where implemented
- [ ] **Persistence**: session disk format and discovery (source-of-truth on disk)
- [ ] **Config parity**: define supported subset of TS config fields and validation strategy

### Phase 2 — Plugins parity (crates/plugins/*)
For each plugin slot, target “TS baseline” parity first, then extensions:
- [ ] **Runtime**: `tmux` (create/send/is_alive/destroy) + terminal streaming helpers
- [ ] **Workspace**: `git worktree` creation/cleanup + restore story (decide: error vs recreate)
- [ ] **Agents**:
  - [ ] `claude-code` adapter (prompt delivery, activity detection, cost parsing)
  - [ ] `cursor` adapter parity where available
  - [ ] unify multi-agent session fleet handling
- [ ] **SCM**: GitHub via `gh` (detect PR, CI checks, review decisions, mergeability, merge)
- [ ] **Tracker**: GitHub issues (and optionally Linear) for issue-first spawning/backlog flows
- [ ] **Notifier**: stdout + desktop + discord + ntfy routing parity
- [ ] **Plugin-spec**: document supported slots and “compile-time wiring” divergence

### Phase 3 — API parity (ao-dashboard)
- [ ] **REST**: sessions list/detail, message, kill, restore
- [ ] **SSE**: event stream with snapshot/delta semantics needed by UI
- [ ] **WebSocket**:
  - [ ] terminal streaming endpoint(s): snapshots initially, interactive later
  - [ ] backpressure and reconnect behavior
- [ ] **Shape alignment**: align JSON to dashboard client expectations (TS `DashboardSession`/`DashboardPR`)
- [ ] **Testing**: handler tests for query params, enrichment, and websocket endpoints

### Phase 4 — Desktop UI parity (Tauri)
From `../agent-orchestrator/packages/web`:
- [ ] **Tokens/theme**: port `globals.css` tokens and component styles
- [ ] **Core components**: `Dashboard`, `AttentionZone`, `SessionCard`, `ProjectSidebar`
- [ ] **Session detail**: `SessionDetail` parity (actions + PR info + comment summaries)
- [ ] **Connection UX**: connection bar + offline states
- [ ] **State mgmt**: project/session selection + SSE reconciliation
- [ ] **Performance**: avoid expensive API calls by default; opt-in PR enrichment

### Phase 5 — Terminal parity (Transport + UI)
Current WS terminal is read-only snapshot streaming.
- [ ] Add a real terminal transport:
  - input support (keypress → runtime)
  - incremental output streaming (not full-screen snapshots)
  - backpressure + reconnect behavior
- [ ] Prefer a minimal initial bridge (tmux pipe/capture) before adding a full PTY.

### Phase 6 — Packaging + verification
- [ ] Document dev workflow (`dashboard` + `vite` + `tauri dev`)
- [ ] Add manual smoke checklist
- [ ] Decide release strategy (local build artifacts, signing later)

## Parallel work breakdown (upstream “7 agents” → ao-rs)
Upstream splits Phase 1 into 7 independent streams. ao-rs can mirror the *work areas* (even if done sequentially):

1. **Core services** → `crates/ao-core` (session manager, lifecycle, reactions, events)
2. **Runtime + workspace plugins** → `crates/plugins/runtime-tmux`, `crates/plugins/workspace-worktree`
3. **Agent plugins** → `crates/plugins/agent-claude-code`, `crates/plugins/agent-cursor`
4. **SCM + tracker** → `crates/plugins/scm-github`, `crates/plugins/tracker-github`
5. **CLI** → `crates/ao-cli` (spawn/status/send/review-check/dashboard/open equivalents)
6. **Dashboard UI** → `crates/ao-desktop/ui` (Tauri-hosted UI; TS uses Next.js web)
7. **Notifier + terminal** → `crates/plugins/notifier-*` + `ao-dashboard` WS terminal bridge

## Key risks / open questions
- **Terminal**: interactive streaming vs snapshot polling; correctness and performance.
- **PR enrichment cost**: `gh` per session is OK at small N but needs opt-in + concurrency control.
- **Restore in UI**: requires an HTTP endpoint that invokes the existing restore logic.
- **Config compat**: which TS config fields are in-scope; how strict to validate; migration story.
- **Plugin divergence**: compile-time plugin wiring vs TS runtime discovery; document clearly.

