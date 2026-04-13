---
title: "Agent Orchestrator → ao-rs Full Port Plan"
owner: "ao-rs"
status: draft
---

# Agent Orchestrator → ao-rs Full Port Plan

This document describes an end-to-end plan to port the **Agent Orchestrator (TS)** product into this repo’s **Rust + Tauri** implementation.

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

## Milestones
- **M1: Desktop shell** — Tauri boots + connects to dashboard
- **M2: Dashboard parity** — sessions board + attention zones + actions
- **M3: Session detail parity** — detail page equivalent (actions, PR info, terminal)
- **M4: PR/CI parity** — enrich PR shape to match TS expectations (checks, decisions, comments)
- **M5: Terminal parity** — interactive terminal (input + streaming) with robust transport
- **M6: Packaging** — docs + build/release workflow + smoke tests

## Work breakdown (phased)

### Phase 1 — Scaffold (Desktop)
- [ ] Create/validate `crates/ao-desktop/` as Tauri v2 host
- [ ] Vite+React+TS build pipeline, Tailwind/token support
- [ ] Typed API client for `ao-dashboard` (REST + SSE + WS)

### Phase 2 — Cherry-pick core dashboard components (UI)
From `../agent-orchestrator/packages/web/src/components/`:
- [ ] `SessionCard`
- [ ] `AttentionZone`
- [ ] `Dashboard`
- [ ] Connection status UX (`ConnectionBar` equivalent)

### Phase 3 — Session detail + actions (UI)
From `SessionDetail` and related components:
- [ ] Detail layout parity (title, pills, PR linkouts)
- [ ] Actions parity: message, kill, restore (requires API)
- [ ] Event reconciliation: SSE deltas update board + detail

### Phase 4 — API parity for PR/CI/review (Backend + UI)
TS expects a richer `DashboardPR` shape (see `packages/web/src/lib/types.ts`):
- [ ] Extend `ao-dashboard` enrichment to include:
  - PR state, CI rollup, **CI check list**
  - review decision + unresolved comments/threads counts (where feasible)
  - additions/deletions/changed files (optional but useful)
  - explicit “enriched/unknown/rate-limited” semantics (avoid misleading defaults)
- [ ] Add handler tests for enrichment paths

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

## Key risks / open questions
- **Terminal**: interactive streaming vs snapshot polling; correctness and performance.
- **PR enrichment cost**: `gh` per session is OK at small N but needs opt-in + concurrency control.
- **Restore in UI**: requires an HTTP endpoint that invokes the existing restore logic.

