---
phase: requirements
title: "Feature: tauri-ui-port"
description: "Port Agent Orchestrator TS dashboard UI into ao-rs Tauri desktop app"
---

# Feature: tauri-ui-port — Requirements

## Problem Statement
We want the **full Agent Orchestrator dashboard UI** (currently in `../agent-orchestrator/packages/web`) to run as a **desktop app** backed by this repo’s Rust services (`ao-cli` + `ao-dashboard`).

## Goals & Objectives
- **Parity UI/UX**: replicate all major screens, components, and interactions from the TS web UI.
- **Desktop-first distribution**: packaged Tauri app runs locally on macOS (initial target), with a path to Windows/Linux later.
- **Backend separation**: keep `ao-dashboard` as the API layer; desktop UI is a client of that API.
- **Good defaults**: connect to local dashboard URL (default `http://127.0.0.1:3000`) with clear connection status.

## Non-goals
- Rewriting orchestrator core logic from TS into the desktop app (keep orchestration in Rust).
- Supporting multi-user auth/roles (local tool).
- Perfect SSR/Next.js behaviors (desktop app is client-rendered).

## User Stories & Use Cases
- As an operator, I can **see all sessions** grouped by attention zone (working/pending/review/respond/merge/done).
- As an operator, I can **open a session detail view** with PR/CI/review info and terminal view (where supported).
- As an operator, I can **send a message** to a session’s agent.
- As an operator, I can **kill** (terminate) a session.
- As an operator, I can **watch live updates** via SSE without manual refresh.
- As an operator, I can **view/interact with a session terminal** inside the desktop app.

## Success Criteria
- Desktop app renders the same functional areas as TS dashboard:
  - Dashboard view (kanban/attention zones)
  - Projects sidebar and session navigation
  - Session detail view
  - Connection state indicator
- Uses `ao-dashboard` endpoints (REST + SSE) without introducing new backend requirements.
- Core workflows work against a real running `ao-rs dashboard`.

## Constraints & Assumptions
- Source UI is at `../agent-orchestrator/packages/web`.
- Rust repo already has a prototype desktop crate at `crates/ao-desktop/` (Tauri v2).
- API is currently local-dev oriented (no auth; permissive CORS in `ao-dashboard`).

## Risks & Open Questions
- **Parity scope**: TS dashboard uses Next.js app router, Tailwind v4 tokens, and some server-side wiring; desktop will need equivalents.
- **Terminal embed**: TS has `DirectTerminal`/terminal components; exact parity may require additional backend endpoints or a local pty approach.
- **PR enrichment**: TS dashboard expects rich PR fields (CI checks, unresolved comments, additions/deletions). Rust API may need more enrichment to match.

