---
phase: design
title: "Slice 6: Tauri desktop dashboard"
description: "Architecture for a desktop UI backed by ao-dashboard REST/SSE"
---

# Slice 6: Tauri desktop dashboard — Design

## Architecture Overview

Desktop app uses a web UI rendered in a Tauri window. Data comes from the existing
dashboard API (`crates/ao-dashboard`) over localhost REST + SSE.

```mermaid
graph TD
  UI[Web UI (Tauri WebView)] -->|HTTP JSON| API[ao-rs dashboard (axum)]
  UI -->|SSE /api/events| API
  UI -->|POST /api/sessions/:id/message| API

  API --> Sessions[~/.ao-rs/sessions/*]
  API --> Runtime[tmux runtime probes]
  API --> SCM[gh CLI probes]
```

### Key components

- **Tauri shell**: windows, native menus, local storage, optional autostart.
- **UI**: session table, session detail, event stream, actions (send message).
- **API server**: existing `ao-rs dashboard` command providing REST + SSE.

## Data Models

- `Session` (from `ao-core`): includes `status`, `activity`, `cost`, `agent`, `agent_config`.
- `OrchestratorEvent` (SSE): used for live updates.

UI will maintain:

- `sessions: Map<id, Session>`
- `events: ring buffer` (bounded list for live stream)
- derived views (filters by project/status)

## API Design

Consume existing endpoints (no new server API in Slice 6 unless required):

- `GET /api/sessions`
- `GET /api/sessions/:id`
- `POST /api/sessions/:id/message`
- `GET /api/events` (SSE)

## Component Breakdown

### UI (initial screens)

- **Sessions list**
  - columns: id(short), project, status, activity, branch, PR summary (optional), cost (optional)
  - actions: open details, copy attach/restore commands
- **Session detail**
  - full task, paths, timestamps
  - “Send message” input
  - link to PR if available
- **Events**
  - live list of event rows (filter by session)

### Backend (Tauri side)

Two modes (choose one in planning):

1. **Connect-only** (simpler): user runs `ao-rs dashboard`; app connects to `http://127.0.0.1:<port>`.
2. **Embedded supervisor** (more integrated): app starts/stops `ao-rs dashboard` as a child process and manages port selection.

## Design Decisions

- Prefer **connect-only first** to minimize coupling and keep slice scope tight.
- Avoid inventing new protocol: reuse existing REST/SSE server.
- Keep local-only defaults for security.

## Non-Functional Requirements

- UI remains responsive under high event rates (use bounded buffers, batch updates).
- Handle offline/unavailable dashboard gracefully (clear status + retry).
- Security: default to localhost; do not expose tokens/secrets; no remote binding without explicit opt-in.

