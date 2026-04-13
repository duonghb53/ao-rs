import { useEffect, useMemo, useState } from "react";
import {
  type ApiEvent,
  type ApiSession,
  connectEvents,
  getSessions,
  killSession,
  sendMessage,
  type ConnectionStatus,
} from "../api/client";
import { AttentionZone } from "../components/AttentionZone";
import { Dashboard } from "../components/Dashboard";
import { ProjectSidebar } from "../components/ProjectSidebar";
import { SessionDetail } from "../components/SessionDetail";
import { TerminalView } from "../components/TerminalView";
import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";

export function App() {
  const [baseUrl, setBaseUrl] = useState("http://127.0.0.1:3000");
  const [conn, setConn] = useState<ConnectionStatus>({ kind: "disconnected" });
  const [sessions, setSessions] = useState<ApiSession[]>([]);
  const [events, setEvents] = useState<ApiEvent[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);

  const connLabel = useMemo(() => {
    if (conn.kind === "connected") return "connected";
    if (conn.kind === "connecting") return "connecting…";
    if (conn.kind === "error") return `error: ${conn.message}`;
    return "disconnected";
  }, [conn]);

  useEffect(() => {
    // no-op on mount; user must click Connect (mirrors the prototype)
  }, []);

  const onConnect = async () => {
    setConn({ kind: "connecting" });
    try {
      const s = await getSessions(baseUrl);
      setSessions(s);
      const es = connectEvents(baseUrl, {
        onOpen: () => setConn({ kind: "connected" }),
        onError: (message) => setConn({ kind: "error", message }),
        onEvent: (evt) => {
          setEvents((prev) => [evt, ...prev].slice(0, 200));
        },
      });
      // If we reconnect, close prior stream by swapping handler ownership
      return () => es.close();
    } catch (e) {
      const msg = e instanceof Error ? e.message : "unknown error";
      setConn({ kind: "error", message: msg });
    }
  };

  const dashboardSessions: DashboardSession[] = useMemo(
    () =>
      sessions.map((s) => ({
        id: s.id,
        projectId: s.project_id,
        status: s.status,
        activity: s.activity ?? null,
        branch: s.branch ?? null,
        summary: s.task ?? null,
        summaryIsFallback: false,
        issueTitle: null,
        userPrompt: null,
        pr: null,
        metadata: {},
      })),
    [sessions],
  );

  const visibleSessions = useMemo(() => {
    if (selectedProjectId === null) return dashboardSessions;
    return dashboardSessions.filter((s) => s.projectId === selectedProjectId);
  }, [dashboardSessions, selectedProjectId]);

  const selectedSession = useMemo(() => {
    if (!selectedSessionId) return null;
    return dashboardSessions.find((s) => s.id === selectedSessionId) ?? null;
  }, [dashboardSessions, selectedSessionId]);

  return (
    <div className="app">
      <div className="topbar">
        <div className="brand">
          <div className="brand__title">ao-rs desktop</div>
          <span className={`pill ${conn.kind === "connected" ? "pill--ok" : conn.kind === "error" ? "pill--bad" : ""}`}>
            <span className="pill__dot" />
            {connLabel}
          </span>
        </div>
        <div className="controls">
          <span className="hint">Dashboard URL</span>
          <input
            size={28}
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
          />
          <button className="primary" onClick={onConnect}>
            Connect
          </button>
        </div>
      </div>

      <div className="main">
        <ProjectSidebar
          sessions={dashboardSessions}
          activeProjectId={selectedProjectId}
          activeSessionId={selectedSessionId}
          onSelectProject={(pid) => {
            setSelectedProjectId(pid);
            // Clear selection if it no longer exists in the filtered view
            if (pid !== null && selectedSessionId) {
              const exists = dashboardSessions.some((s) => s.projectId === pid && s.id === selectedSessionId);
              if (!exists) setSelectedSessionId(null);
            }
          }}
          onSelectSession={(sid) => setSelectedSessionId(sid)}
        />

        <div style={{ gridColumn: "2 / 3" }}>
          <Dashboard
            sessions={visibleSessions}
            onSelect={(s) => setSelectedSessionId(s.id)}
          />
        </div>

        <div style={{ gridColumn: "3 / 4", display: "grid", gap: 12, alignContent: "start" }}>
          <section className="panel">
            <div className="panel__title">Session Detail</div>
            <div style={{ padding: 10 }}>
              {selectedSession ? (
                <SessionDetail
                  session={selectedSession}
                  onSendMessage={(msg) => sendMessage(baseUrl, selectedSession.id, msg)}
                  onKill={() => killSession(baseUrl, selectedSession.id)}
                />
              ) : (
                <div className="hint">select a session to view details</div>
              )}
            </div>
          </section>

          <section className="panel">
            <div className="panel__title">Terminal</div>
            <div style={{ padding: 10 }}>
              <div className="hint" style={{ marginBottom: 6 }}>
                selected: {selectedSessionId ? selectedSessionId.slice(0, 8) : "(none)"}
              </div>
              <TerminalView baseUrl={baseUrl} sessionId={selectedSessionId} />
            </div>
          </section>
        </div>

        <section className="panel" style={{ gridColumn: "1 / -1" }}>
          <div className="panel__title">Events</div>
          <div className="events">
            {events.map((e, idx) => (
              <div className="evt" key={idx}>
                <div className="evt__type">{e.type ?? "event"}</div>
                <div className="evt__meta">{JSON.stringify(e)}</div>
              </div>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}

