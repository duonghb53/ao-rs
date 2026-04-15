## Interactive terminal (dashboard WS + desktop UI)

This repo exposes an interactive terminal per session via a dashboard WebSocket endpoint and renders it in the desktop UI using `xterm.js`.

### Endpoint

- **URL**: `GET /api/sessions/:id/terminal` (WebSocket upgrade)
- **Server implementation**: `crates/ao-dashboard/src/routes.rs` (`terminal_ws`, `stream_tmux_pty`)
- **Client implementation**: `crates/ao-desktop/ui/src/components/TerminalView.tsx`

### Message types

- **Server → client**
  - **Binary frames**: raw PTY output bytes (best-effort stream).
  - **Text frames**:
    - Human-readable error lines during setup failures (e.g. PTY open/spawn issues).
    - Control JSON:
      - `{"type":"dropped","dropped_chunks":N,"policy":"drop_newest"}`

- **Client → server**
  - **Text frames**: UTF-8 input to PTY (keystrokes / pasted text).
  - **Binary frames**: raw input bytes (optional; supported).
  - **Resize control**: `{"type":"resize","cols":<u16>,"rows":<u16>}`

### Backpressure + drop policy (output)

The PTY reader must never block behind a slow WebSocket client (otherwise the `tmux attach` session can stall).

- **Policy**: bounded buffering + **drop newest** on overflow.
  - The PTY reader thread forwards output into a bounded channel (`WS_OUT_CAPACITY`).
  - If the channel is full, the newest chunk is dropped immediately.
- **User-visible signal**: the server periodically emits a control JSON message describing how many output chunks were dropped since the last notice.

**Guarantee**: terminal input remains responsive even if output is high-volume.  
**Non-guarantee**: output is lossless; under load, output can be dropped.

### Reconnect semantics

Reconnect is **stateless** and safe:

- **On each WebSocket connection**, the server creates a fresh PTY and runs `tmux attach -t <session>` inside it.
- **If the socket disconnects**, the PTY process is torn down. The underlying tmux session remains intact.
- **When the client reconnects**, it attaches again to the same tmux session.

Client behavior:

- `TerminalView` auto-reconnects with exponential backoff after unexpected disconnects.
- On reconnect, the UI re-sends an initial resize to match the current container size.
- The client uses a streaming UTF-8 decoder for binary output to avoid corrupting multibyte sequences that span frames.

### Limitations

- The output stream is best-effort; do not rely on it for complete logs. Use persisted logs/artifacts for correctness.
- Extremely slow clients may observe frequent drop notices and gaps in output.
- Output ordering is preserved for delivered chunks, but gaps can occur where chunks were dropped.

### Manual load / stress probe

To validate responsiveness, in a session terminal run a high-output command, for example:

```bash
yes "spam" | head -n 200000
```

Expected:

- typing still works (input is accepted)
- UI stays responsive (no wedged terminal)
- you may see `[output dropped] ...` notices during peak output

