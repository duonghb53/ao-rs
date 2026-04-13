---
phase: design
title: "Feature: tauri-ui-port"
description: "Architecture for porting TS dashboard UI into Tauri + Rust"
---

# Feature: tauri-ui-port — Design

## Architecture Overview

```mermaid
graph TD
  User[Operator] --> TauriUI[Tauri Window (UI)]
  TauriUI -->|HTTP JSON| DashboardAPI[ao-dashboard (Axum REST)]
  TauriUI -->|SSE| Events[GET /api/events]
  DashboardAPI --> Sessions[SessionManager (disk YAML)]
  DashboardAPI --> Runtime[Runtime trait (tmux)]
  DashboardAPI --> SCM[Scm trait (gh)]
  aoCLI[ao-cli: ao-rs dashboard] --> DashboardAPI
```

### Key decisions
- **Keep `ao-dashboard` as the backend contract** and replicate the TS UI as a client.
- **Port TS components incrementally** (starting from `packages/web/src/components/Dashboard.tsx` and core types in `packages/web/src/lib/types.ts`).
- **Prefer a “web UI in Tauri” approach**: ship assets and run client-side rendering.

## Component breakdown (TS → Rust/Tauri)

### TS sources to port
- **Routes/layout**: `../agent-orchestrator/packages/web/src/app/*`
- **Core components**: `../agent-orchestrator/packages/web/src/components/*`
  - `Dashboard.tsx`, `ProjectSidebar.tsx`, `SessionCard.tsx`, `SessionDetail.tsx`, etc.
- **Client domain/types**: `../agent-orchestrator/packages/web/src/lib/types.ts`
- **Design tokens**: `../agent-orchestrator/packages/web/src/app/globals.css`

### Proposed Tauri structure
- `crates/ao-desktop/ui/`: compiled/static UI assets
- `crates/ao-desktop/src-tauri/`: minimal Tauri host (launch, window config)

## API design / compatibility targets
To reach parity, the desktop UI should consume:
- `GET /api/sessions` (and enriched variants like `?pr=true` as needed)
- `GET /api/sessions/{id}`
- `POST /api/sessions/{id}/message`
- `POST /api/sessions/{id}/kill`
- `GET /api/events` (SSE)
- `WS /api/terminal` (new): xterm.js terminal bridge (Phase 3)

### Gap analysis (expected)
The TS dashboard’s `DashboardPR` includes fields not yet present in ao-rs API:
- CI checks list, unresolved comments/threads, additions/deletions, and explicit `enriched` markers.

Plan: keep UI scaffolding first, then extend `ao-dashboard` enrichment endpoints to match required fields.

## Non-functional requirements
- **Performance**: keep default dashboard calls fast; make expensive PR enrichment opt-in.
- **Correctness**: avoid “lying” states on partial SCM failures (represent unknown explicitly).
- **Security**: local-first; do not add auth in this feature; do not expose secrets via API payloads.

