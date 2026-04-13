---
phase: testing
title: "Feature: tauri-ui-port"
description: "Testing strategy and coverage notes"
---

# Feature: tauri-ui-port — Testing

## Test plan (high-level)
- **API contract tests**: `ao-dashboard` endpoints needed for UI parity (sessions, session detail, message, kill, SSE).
- **Desktop smoke tests** (manual): connect/disconnect, render sessions, open detail, send message, kill session, SSE updates.

## Coverage targets
- Add focused handler tests for any new enriched endpoint/query params.

