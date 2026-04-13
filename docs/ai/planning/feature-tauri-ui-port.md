---
phase: planning
title: "Feature: tauri-ui-port"
description: "Task breakdown for porting agent-orchestrator TS dashboard UI into ao-rs Tauri desktop"
---

# Feature: tauri-ui-port — Planning

## Milestones
- [ ] **M1: Scaffold** — Tauri app (Vite + React) runs and can connect to `ao-dashboard`
- [ ] **M2: Core dashboard** — cherry-picked components working: `SessionCard`, `Dashboard`, `AttentionZone`
- [ ] **M3: Terminal** — terminal view via xterm.js with backend WebSocket bridge
- [ ] **M4: Sidebar + polish** — `ProjectSidebar` + navigation + UX polish

## Task Breakdown

### Phase 1: Scaffold (`crates/ao-desktop/` — previously called `ao-tauri`)
- [x] **Task 1.1**: Confirm UI source scope under `../agent-orchestrator/packages/web` (routes, components, styles, types)
- [x] **Task 1.2**: Decide UI stack in `ao-desktop` (vanilla vs React) and set up build pipeline accordingly
- [x] **Task 1.3**: Implement a typed API client for `ao-dashboard` (sessions + message + kill + SSE)
- [ ] **Task 1.4**: (Optional) Rename crate/path to `crates/ao-tauri/` for naming parity with earlier plan

### Phase 2: Cherry-pick core dashboard components (from ao-ts web)
- [x] **Task 2.1**: Port design tokens from `packages/web/src/app/globals.css` into `ao-desktop` styling system
- [x] **Task 2.2**: Cherry-pick `SessionCard` from `packages/web/src/components/SessionCard.tsx`
- [x] **Task 2.3**: Cherry-pick `AttentionZone` from `packages/web/src/components/AttentionZone.tsx`
- [x] **Task 2.4**: Cherry-pick `Dashboard` from `packages/web/src/components/Dashboard.tsx` (or minimal dashboard shell using the above)

### Phase 3: Terminal (xterm.js + WebSocket bridge)
- [x] **Task 3.1**: Add frontend terminal view using xterm.js (render-only first)
- [x] **Task 3.2**: Add backend WebSocket endpoint (in `ao-dashboard` or a sibling server) to proxy session terminal I/O
- [x] **Task 3.3**: Wire terminal view to WebSocket (attach/detach per session)

### Phase 4: `ProjectSidebar` + polish
- [x] **Task 4.1**: Cherry-pick `ProjectSidebar` from `packages/web/src/components/ProjectSidebar.tsx`
- [x] **Task 4.2**: Navigation/routing polish (sessions list → detail → back)
- [ ] **Task 4.3**: Packaging docs + smoke test checklist (manual) and document `cargo tauri dev/build` steps

## Dependencies
- Desktop UI needs `ao-rs dashboard` running (or embed it later as a separate feature).
- Terminal phase depends on deciding where the WebSocket bridge lives (likely `ao-dashboard`).

## Risks & Mitigation
- **Scope explosion**: keep parity tasks grouped; ship M1/M2 early.
- **Next.js-only patterns**: replace with client-only equivalents in desktop UI.

