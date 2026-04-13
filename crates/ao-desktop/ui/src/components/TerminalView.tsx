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
  const lastSnapshotRef = useRef<string>("");

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
      term.dispose();
      termRef.current = null;
    };
  }, []);

  useEffect(() => {
    const term = termRef.current;
    if (!term) return;

    wsRef.current?.close();
    lastSnapshotRef.current = "";

    term.reset();
    term.writeln("Terminal");
    term.writeln("");

    if (!sessionId) {
      term.writeln("select a session to view terminal output");
      return;
    }

    const url = wsUrl(baseUrl, `/api/sessions/${encodeURIComponent(sessionId)}/terminal`);
    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onopen = () => {
      term.writeln(`connected (${url})`);
      term.writeln("");
    };
    ws.onclose = (evt) => {
      const reason = evt.reason ? ` reason=${evt.reason}` : "";
      term.writeln(`\r\n[ws closed] code=${evt.code}${reason}`);
    };
    ws.onerror = () => {
      // Some runtimes fire onerror on abrupt close; onclose has the useful details.
      term.writeln("\r\n[ws error]");
    };
    ws.onmessage = (msg) => {
      const text = typeof msg.data === "string" ? msg.data : "";
      if (!text || text === lastSnapshotRef.current) return;
      lastSnapshotRef.current = text;
      term.reset();
      term.write(text);
    };

    return () => {
      ws.close();
    };
  }, [baseUrl, sessionId]);

  return <div ref={hostRef} style={{ height: 360, width: "100%" }} />;
}

