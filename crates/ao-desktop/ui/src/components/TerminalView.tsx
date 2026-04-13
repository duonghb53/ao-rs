import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

function wsUrl(baseUrl: string, path: string): string {
  const trimmed = baseUrl.replace(/\/+$/, "");
  if (trimmed.startsWith("https://")) return `wss://${trimmed.slice("https://".length)}${path}`;
  if (trimmed.startsWith("http://")) return `ws://${trimmed.slice("http://".length)}${path}`;
  // fallback
  return `ws://${trimmed}${path}`;
}

export function TerminalView({
  baseUrl,
  sessionId,
}: {
  baseUrl: string;
  sessionId: string | null;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const inputDisposableRef = useRef<{ dispose: () => void } | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);

  useEffect(() => {
    if (!hostRef.current) return;
    const term = new Terminal({
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, Liberation Mono, monospace",
      fontSize: 12,
      convertEol: true,
    });
    termRef.current = term;
    term.open(hostRef.current);
    term.writeln("Terminal");
    term.writeln("");
    term.writeln(sessionId ? `connecting to ${sessionId}…` : "select a session to view terminal output");

    return () => {
      wsRef.current?.close();
      inputDisposableRef.current?.dispose();
      inputDisposableRef.current = null;
      resizeObserverRef.current?.disconnect();
      resizeObserverRef.current = null;
      term.dispose();
      termRef.current = null;
    };
  }, []);

  useEffect(() => {
    const term = termRef.current;
    if (!term) return;

    wsRef.current?.close();

    term.reset();
    term.writeln("Terminal");
    term.writeln("");

    if (!sessionId) {
      term.writeln("select a session to view terminal output");
      return;
    }

    const url = wsUrl(baseUrl, `/api/sessions/${encodeURIComponent(sessionId)}/terminal`);
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    wsRef.current = ws;

    ws.onopen = () => {
      term.writeln(`connected (${url})`);
      term.writeln("");

      // Pipe local keystrokes to backend.
      inputDisposableRef.current?.dispose();
      inputDisposableRef.current = term.onData((data) => {
        try {
          if (ws.readyState !== WebSocket.OPEN) return;
          // Send UTF-8 bytes so backend PTY receives raw input.
          ws.send(new TextEncoder().encode(data));
        } catch {
          // ignore
        }
      });

      const sendResize = () => {
        if (ws.readyState !== WebSocket.OPEN) return;
        try {
          ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
        } catch {
          // ignore
        }
      };

      // Initial resize + observe container resizes.
      sendResize();
      resizeObserverRef.current?.disconnect();
      resizeObserverRef.current = new ResizeObserver(() => sendResize());
      if (hostRef.current) resizeObserverRef.current.observe(hostRef.current);
    };
    ws.onclose = (evt) => {
      const reason = evt.reason ? ` reason=${evt.reason}` : "";
      term.writeln(`\r\n[ws closed] code=${evt.code}${reason}`);
      inputDisposableRef.current?.dispose();
      inputDisposableRef.current = null;
    };
    ws.onerror = () => {
      // Some runtimes fire onerror on abrupt close; onclose has the useful details.
      term.writeln("\r\n[ws error]");
    };
    ws.onmessage = (msg) => {
      if (typeof msg.data === "string") {
        // Server-side diagnostics.
        const text = msg.data;
        if (!text) return;
        term.writeln(`\r\n${text}`);
        return;
      }

      if (msg.data instanceof ArrayBuffer) {
        // PTY output stream: append bytes (do NOT clear/reset).
        const text = new TextDecoder().decode(new Uint8Array(msg.data));
        if (!text) return;
        term.write(text);
      }
    };

    return () => {
      inputDisposableRef.current?.dispose();
      inputDisposableRef.current = null;
      ws.close();
    };
  }, [baseUrl, sessionId]);

  return <div ref={hostRef} style={{ height: 360, width: "100%" }} />;
}

