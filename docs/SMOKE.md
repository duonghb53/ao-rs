# Manual smoke checklist

Run before a release or after large changes to `ao-cli`, `ao-dashboard`, or `ao-desktop` UI. Adjust paths and ports to your machine.

## Prerequisites

- [ ] `cargo build -p ao-cli` succeeds
- [ ] `npm run build` in `crates/ao-desktop/ui` succeeds
- [ ] `gh auth status` passes (for PR enrichment tests)
- [ ] `tmux` and `git` on `PATH`

## CLI + lifecycle

- [ ] `ao-rs doctor` — no unexpected FAIL
- [ ] `ao-rs status` — lists expected sessions or empty state without panic
- [ ] `ao-rs dashboard` — prints listening URL; Ctrl-C stops cleanly
- [ ] `ao-rs dashboard --open` — browser opens root page (`/`)

## HTTP API (`ao-rs dashboard`, default `http://127.0.0.1:3000`)

- [ ] `GET /health` — JSON with `"ok": true`
- [ ] `GET /` — HTML landing with links
- [ ] `GET /api/sessions` — JSON array (may be `[]`)
- [ ] `GET /api/sessions?pr=true` — JSON array; with real sessions, entries include `attention_level` / `pr` when applicable
- [ ] `GET /api/events` — SSE stream stays open; events appear when lifecycle emits (use `curl -N` or browser devtools)

## Desktop UI (Vite: `npm run dev` in `crates/ao-desktop/ui`)

- [ ] **Dashboard URL** matches the running dashboard origin
- [ ] Status pill shows **connected** after load (not stuck on **error**)
- [ ] Session list appears; empty state copy is sensible when there are no sessions
- [ ] Open a session tab; **Session detail** shows fields; **PR** block shows link/signals when `?pr=true` data exists
- [ ] **Send message** — succeeds; list refreshes without full-page reload
- [ ] **Kill** / **Restore** — confirm modals; action completes; status updates
- [ ] **Terminal** — connects; typing reaches tmux; resize does not break layout badly; disconnect shows reconnect message and recovers
- [ ] **Terminal (load)** — run a high-output command (see `docs/terminal.md`) and confirm:
  - [ ] UI stays responsive
  - [ ] reconnect does not wedge the terminal
  - [ ] drop notices may appear under load

## Regression triggers

If any of these change, re-run the full checklist:

- `crates/ao-dashboard/src/routes.rs` or `sse.rs`
- `crates/ao-desktop/ui/src/ui/App.tsx` or `TerminalView.tsx`
- SCM / `gh` integration in `ao-plugin-scm-github`
