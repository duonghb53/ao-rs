---
phase: implementation
title: "Feature: tauri-ui-port"
description: "Implementation notes and progress log"
---

# Feature: tauri-ui-port — Implementation

## Status
- Current repo branch may not yet be `feature-tauri-ui-port` (see `docs/ai/planning/feature-tauri-ui-port.md` for tasks).

## Decisions
- Backend contract remains `ao-dashboard` (REST + SSE).
- Desktop app remains Tauri v2 under `crates/ao-desktop/`.
- UI stack decision: use **React + TypeScript + Vite** inside `crates/ao-desktop/ui` so TS components from `../agent-orchestrator/packages/web` can be ported with minimal rewrite.

## Progress log
- Initialized requirements/design/planning docs.

