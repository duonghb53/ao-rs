import { useEffect, useMemo, useRef, useState } from "react";
import {
  type ApiEvent,
  type ApiSession,
  connectEvents,
  getSessions,
  killSession,
  restoreSession,
  sendMessage,
  type ConnectionStatus,
} from "../api/client";
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
  const esRef = useRef<EventSource | null>(null);
  const refreshTimerRef = useRef<number | null>(null);
  const [detailOnly, setDetailOnly] = useState(false);
  const [activeTab, setActiveTab] = useState<"dashboard" | { sessionId: string }>("dashboard");
  const [sessionTabs, setSessionTabs] = useState<string[]>([]);

  const connLabel = useMemo(() => {
    if (conn.kind === "connected") return "connected";
    if (conn.kind === "connecting") return "connecting…";
    if (conn.kind === "error") return `error: ${conn.message}`;
    return "disconnected";
  }, [conn]);

  useEffect(() => {
    // URL params: `?session=<id>&view=detail`
    const params = new URLSearchParams(window.location.search);
    const sid = params.get("session");
    const view = params.get("view");
    if (sid) setSelectedSessionId(sid);
    if (view === "detail") setDetailOnly(true);
    if (sid && view === "detail") {
      setActiveTab({ sessionId: sid });
      setSessionTabs([sid]);
    }

    return () => {
      esRef.current?.close();
      esRef.current = null;
      if (refreshTimerRef.current !== null) {
        window.clearTimeout(refreshTimerRef.current);
        refreshTimerRef.current = null;
      }
    };
  }, []);

  // Auto-connect on load and when baseUrl changes.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setConn({ kind: "connecting" });
      try {
        const s = await getSessions(baseUrl);
        if (cancelled) return;
        setSessions(s);

        esRef.current?.close();
        esRef.current = connectEvents(baseUrl, {
          onOpen: () => setConn({ kind: "connected" }),
          onError: (message) => setConn({ kind: "error", message }),
          onEvent: (evt) => {
            setEvents((prev) => [evt, ...prev].slice(0, 200));
            scheduleRefresh();
          },
        });
      } catch (e) {
        if (cancelled) return;
        const msg = e instanceof Error ? e.message : "unknown error";
        setConn({ kind: "error", message: msg });
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [baseUrl]);

  const refreshSessions = async () => {
    const s = await getSessions(baseUrl);
    setSessions(s);
  };

  const scheduleRefresh = () => {
    if (refreshTimerRef.current !== null) return;
    refreshTimerRef.current = window.setTimeout(() => {
      refreshTimerRef.current = null;
      refreshSessions().catch(() => {
        // ignore; conn status will reflect SSE errors separately
      });
    }, 400);
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

  const openSessionDetail = (id: string) => {
    setSelectedSessionId(id);
    setActiveTab({ sessionId: id });
    setSessionTabs((prev) => (prev.includes(id) ? prev : [id, ...prev].slice(0, 12)));
  };

  const closeSessionTab = (id: string) => {
    setSessionTabs((prev) => prev.filter((t) => t !== id));
    setActiveTab((prev) => {
      if (prev !== "dashboard" && prev.sessionId === id) return "dashboard";
      return prev;
    });
  };

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
        </div>
      </div>

      <div
        className="main"
        style={
          detailOnly
            ? { gridTemplateColumns: "1fr" }
            : { gridTemplateColumns: "260px 1fr" }
        }
      >
        {detailOnly ? null : (
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
            onOpenSession={(s) => openSessionDetail(s.id)}
          />
        )}

        {detailOnly ? null : (
          <div style={{ gridColumn: "2 / 3", display: "grid", gap: 12, alignContent: "start" }}>
            <section className="panel">
              <div className="panel__title" style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                <button type="button" className={activeTab === "dashboard" ? "mini-pill" : "hint"} onClick={() => setActiveTab("dashboard")}>
                  Dashboard
                </button>
                {sessionTabs.map((sid) => (
                  <span key={sid} style={{ display: "inline-flex", gap: 6, alignItems: "center" }}>
                    <button
                      type="button"
                      className={activeTab !== "dashboard" && activeTab.sessionId === sid ? "mini-pill" : "hint"}
                      onClick={() => setActiveTab({ sessionId: sid })}
                      title={sid}
                    >
                      {sid.slice(0, 8)}
                    </button>
                    <button type="button" className="hint" onClick={() => closeSessionTab(sid)} title="Close tab">
                      ×
                    </button>
                  </span>
                ))}
              </div>
            </section>

            {activeTab === "dashboard" ? (
              <>
                <Dashboard sessions={visibleSessions} onSelect={(s) => setSelectedSessionId(s.id)} onOpen={(s) => openSessionDetail(s.id)} />
                <section className="panel">
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
              </>
            ) : (
              <>
                <section className="panel">
                  <div className="panel__title">Session Detail</div>
                  <div style={{ padding: 10 }}>
                    {selectedSession ? (
                      <SessionDetail
                        session={selectedSession}
                        onSendMessage={(msg) => sendMessage(baseUrl, selectedSession.id, msg)}
                        onKill={() => killSession(baseUrl, selectedSession.id)}
                        onRestore={async () => {
                          const updated = await restoreSession(baseUrl, selectedSession.id);
                          setSessions((prev) => prev.map((s) => (s.id === updated.id ? updated : s)));
                          scheduleRefresh();
                        }}
                      />
                    ) : (
                      <div className="hint">select a session</div>
                    )}
                  </div>
                </section>

                <section className="panel">
                  <div className="panel__title">Terminal</div>
                  <div style={{ padding: 10 }}>
                    <div className="hint" style={{ marginBottom: 6 }}>
                      selected: {activeTab === "dashboard" ? "(none)" : activeTab.sessionId.slice(0, 8)}
                    </div>
                    <TerminalView baseUrl={baseUrl} sessionId={activeTab === "dashboard" ? null : activeTab.sessionId} />
                  </div>
                </section>
              </>
            )}
          </div>
        )}

        {detailOnly ? (
          <div style={{ gridColumn: "1 / -1", display: "grid", gap: 12, alignContent: "start" }}>
            <section className="panel">
              <div className="panel__title">Session Detail</div>
              <div style={{ padding: 10 }}>
                {selectedSession ? (
                  <SessionDetail
                    session={selectedSession}
                    onSendMessage={(msg) => sendMessage(baseUrl, selectedSession.id, msg)}
                    onKill={() => killSession(baseUrl, selectedSession.id)}
                    onRestore={async () => {
                      const updated = await restoreSession(baseUrl, selectedSession.id);
                      setSessions((prev) =>
                        prev.map((s) => (s.id === updated.id ? updated : s)),
                      );
                      scheduleRefresh();
                    }}
                  />
                ) : (
                  <div className="hint">select a session</div>
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
        ) : null}

        {/* Events now live under the Dashboard tab */}
      </div>
    </div>
  );
}

