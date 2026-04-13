---
phase: requirements
title: "Slice 6: Tauri desktop dashboard"
description: "Desktop UI for ao-rs sessions/events, backed by existing dashboard API"
---

# Slice 6: Tauri desktop dashboard — Requirements

## Problem Statement

`ao-rs watch` and `ao-rs dashboard` provide useful lifecycle visibility, but:

- It is **terminal-centric** (hard to monitor continuously).
- The dashboard is **API-only**; there is no first-party UI.
- Users want a **single, always-on** view of sessions, events, PR/CI state, and notifications.

## Goals & Objectives

### Primary goals

- Provide a **desktop application** that shows:
  - current sessions (`ao-rs status`-equivalent)
  - live event stream (SSE `OrchestratorEvent`)
  - per-session details (status/activity/PR/CI/review/cost where available)
- Make it easy to **start/stop** the local supervisor loop from the UI (or clearly indicate it is running elsewhere).

### Secondary goals

- Provide an ergonomic UI to:
  - send a message to a session (`ao-rs send`)
  - attach instructions for restore/attach/kill
- Ship a **local-only** app by default (no auth, no remote access).

### Non-goals (explicitly out of scope for Slice 6)

- Remote monitoring over the network / multi-host fleet view
- A full terminal emulator inside the app
- Replacing `tmux` runtime
- Plugin marketplace / dynamic plugin loading

## User Stories & Use Cases

- As a user, I want to see **all sessions** and their current state at a glance.
- As a user, I want to see a **live stream** of session events without leaving the app.
- As a user, I want to **message** an agent session from the UI.
- As a user, I want to quickly copy/paste the **tmux attach** command.
- As a user, I want to understand when the lifecycle loop is **not running**.

## Success Criteria

- Desktop app builds and launches on macOS (initial target).
- App can connect to a local `ao-rs dashboard` endpoint and:
  - list sessions
  - subscribe to events (SSE)
  - send a message to a session
- Clear UX for connection failures (dashboard not running, wrong port).

## Constraints & Assumptions

- Tauri is the chosen desktop framework (Rust backend + webview UI).
- `ao-rs dashboard` remains the primary API surface (REST + SSE).
- Local-only by default: bind to `127.0.0.1` unless explicitly configured otherwise.
- The repo already contains `crates/ao-dashboard` (server) and a stable event shape (`OrchestratorEvent`).

## Questions & Open Items

- Should Slice 6 embed the server (start dashboard internally) or connect to an externally started `ao-rs dashboard`?
- What UI stack to use inside Tauri (vanilla, React, Svelte)?
- How do we distribute (dev-only `cargo tauri dev` vs packaged app)?

