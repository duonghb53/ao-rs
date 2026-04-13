## ao-desktop (Slice 6 prototype)

This is a **Tauri v2** desktop UI that connects to the existing `ao-rs dashboard`
REST + SSE (+ WebSocket terminal) API.

### Current status

- UI assets live in `crates/ao-desktop/ui/`
- Tauri backend lives in `crates/ao-desktop/src-tauri/`

### Run (dev)

1. Start the API server in another terminal:

```bash
ao-rs dashboard --port 3000
```

2. Install UI deps and start the UI dev server (Vite):

```bash
cd crates/ao-desktop/ui
npm install
npm run dev
```

3. Generate placeholder icons (required by Tauri at compile-time):

```bash
cd crates/ao-desktop/src-tauri
cargo tauri icon
```

4. Run the desktop app:

```bash
cargo tauri dev
```

### Notes

- The dashboard currently binds to `0.0.0.0`. For a local-only desktop app,
  we should consider binding to `127.0.0.1` by default in a later slice.
- Terminal streaming uses `tmux capture-pane -p` over a WebSocket endpoint:
  `GET /api/sessions/{id}/terminal` (read-only snapshots for now).

