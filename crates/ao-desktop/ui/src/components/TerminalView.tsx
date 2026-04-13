import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

function wsUrl(baseUrl: string, path: string): string {
  const trimmed = baseUrl.replace(/\/+$/, "");
  if (trimmed.startsWith("https://")) return `wss://${trimmed.slice("https://".length)}${path}`;
  if (trimmed.startsWith("http://")) return `ws://${trimmed.slice("http://".length)}${path}`;
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
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const inputDisposableRef = useRef<{ dispose: () => void } | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const reconnectTimerRef = useRef<number | null>(null);
  const reconnectAttemptRef = useRef(0);
  const connectRef = useRef<() => void>(() => {});

  const forceFocus = () => {
    const term = termRef.current;
    if (!term) return;
    term.focus();
    const textarea = hostRef.current?.querySelector("textarea") as HTMLTextAreaElement | null;
    textarea?.focus();
  };

  useEffect(() => {
    if (!hostRef.current) return;
    const term = new Terminal({
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, Liberation Mono, monospace",
      fontSize: 12,
      convertEol: true,
      disableStdin: false,
      cursorBlink: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    termRef.current = term;
    fitRef.current = fit;
    term.open(hostRef.current);
    fit.fit();
    setTimeout(() => forceFocus(), 0);
    term.writeln("Terminal");
    term.writeln("");
    term.writeln(sessionId ? `connecting to ${sessionId}…` : "select a session to view terminal output");

    return () => {
      if (reconnectTimerRef.current !== null) {
        window.clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      const w = wsRef.current;
      wsRef.current = null;
      w?.close();
      inputDisposableRef.current?.dispose();
      inputDisposableRef.current = null;
      resizeObserverRef.current?.disconnect();
      resizeObserverRef.current = null;
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, []);

  useEffect(() => {
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term) return;

    let cancelled = false;

    const clearReconnect = () => {
      if (reconnectTimerRef.current !== null) {
        window.clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
    };

    clearReconnect();
    {
      const prev = wsRef.current;
      wsRef.current = null;
      prev?.close();
    }
    reconnectAttemptRef.current = 0;

    term.reset();
    term.writeln("Terminal");
    term.writeln("");

    if (!sessionId) {
      term.writeln("select a session to view terminal output");
      return;
    }

    const url = wsUrl(baseUrl, `/api/sessions/${encodeURIComponent(sessionId)}/terminal`);

    const connect = () => {
      clearReconnect();
      {
        const prev = wsRef.current;
        wsRef.current = null;
        prev?.close();
      }
      const ws = new WebSocket(url);
      ws.binaryType = "arraybuffer";
      wsRef.current = ws;

      ws.onopen = () => {
        reconnectAttemptRef.current = 0;
        term.writeln(`connected (${url})`);
        term.writeln("");
        forceFocus();

        inputDisposableRef.current?.dispose();
        inputDisposableRef.current = term.onData((data) => {
          try {
            if (ws.readyState !== WebSocket.OPEN) return;
            // Text frames work reliably in Tauri + browsers; backend writes UTF-8 to PTY.
            ws.send(data);
          } catch {
            // ignore
          }
        });

        const sendResize = () => {
          if (ws.readyState !== WebSocket.OPEN) return;
          try {
            fit?.fit();
            ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
          } catch {
            // ignore
          }
        };

        sendResize();
        resizeObserverRef.current?.disconnect();
        resizeObserverRef.current = new ResizeObserver(() => sendResize());
        if (hostRef.current) resizeObserverRef.current.observe(hostRef.current);
      };

      ws.onclose = (evt) => {
        if (wsRef.current !== ws) return;

        const reason = evt.reason ? ` reason=${evt.reason}` : "";
        term.writeln(`\r\n[ws closed] code=${evt.code}${reason}`);
        inputDisposableRef.current?.dispose();
        inputDisposableRef.current = null;
        resizeObserverRef.current?.disconnect();
        resizeObserverRef.current = null;

        const attempt = reconnectAttemptRef.current + 1;
        reconnectAttemptRef.current = attempt;
        const delay = Math.min(30_000, 500 * Math.pow(2, Math.min(attempt, 6)));
        term.writeln(`\r\n[reconnecting…] attempt ${attempt} in ${delay}ms`);
        reconnectTimerRef.current = window.setTimeout(() => {
          reconnectTimerRef.current = null;
          if (cancelled) return;
          connectRef.current();
        }, delay);
      };

      ws.onerror = () => {
        term.writeln("\r\n[ws error]");
      };

      ws.onmessage = (msg) => {
        if (typeof msg.data === "string") {
          const text = msg.data;
          if (!text) return;
          term.writeln(`\r\n${text}`);
          return;
        }
        if (msg.data instanceof ArrayBuffer) {
          const text = new TextDecoder().decode(new Uint8Array(msg.data));
          if (!text) return;
          term.write(text);
        }
      };
    };

    connectRef.current = connect;
    connect();

    return () => {
      cancelled = true;
      clearReconnect();
      inputDisposableRef.current?.dispose();
      inputDisposableRef.current = null;
      resizeObserverRef.current?.disconnect();
      resizeObserverRef.current = null;
      const w = wsRef.current;
      wsRef.current = null;
      w?.close();
    };
  }, [baseUrl, sessionId]);

  return (
    <div
      ref={hostRef}
      tabIndex={0}
      style={{ height: 360, width: "100%", outline: "none" }}
      onMouseDown={() => forceFocus()}
      onFocus={() => forceFocus()}
    />
  );
}
