---
phase: planning
title: "Slice 6: Tauri desktop dashboard"
description: "Milestones and tasks for a first desktop UI"
---

# Slice 6: Tauri desktop dashboard — Planning

## Milestones

- [ ] **M1: App skeleton** — Tauri app boots, shows a window, config for dev builds
- [ ] **M2: Dashboard connectivity** — configure endpoint, fetch sessions list, show connection state
- [ ] **M3: Live events** — SSE subscribe, render event stream, update sessions incrementally
- [ ] **M4: Actions** — send message, open session details, basic error handling
- [ ] **M5: Polish** — filters, persistence of settings, minimal UX refinement

## Task Breakdown

### Phase 1: Foundation

- [ ] Add a new workspace crate/app for Tauri desktop (name TBD, e.g. `ao-desktop`)
- [ ] Choose UI stack (minimal: vanilla + fetch + SSE)
- [ ] Implement settings: dashboard base URL (default `http://127.0.0.1:3000`)

### Phase 2: Core features

- [ ] Sessions list view:
  - fetch `GET /api/sessions`
  - render table
  - periodic refresh or event-driven updates
- [ ] Event stream view:
  - connect SSE `GET /api/events`
  - append to bounded buffer
  - update affected sessions in memory

### Phase 3: Integration & polish

- [ ] Session detail panel:
  - fetch `GET /api/sessions/:id`
  - show full task/branch/status
- [ ] Send message action:
  - call `POST /api/sessions/:id/message`
  - show success/failure toast
- [ ] Resilience:
  - reconnect SSE with backoff
  - offline state UX

## Dependencies

- Tauri toolchain (Rust + Node tooling for UI build)
- A running local dashboard server (`ao-rs dashboard`)

## Timeline & Estimates

- M1–M2: 0.5–1 day
- M3–M4: 1–2 days
- M5: 0.5 day

## Risks & Mitigation

- **SSE handling in webview**: keep buffer bounded; throttle renders.
- **Cross-platform packaging**: start with macOS dev; package later.
- **API evolution**: keep UI tolerant to missing fields.

## Resources Needed

- Tauri docs/tooling
- Light UI components (table, input, toasts)

