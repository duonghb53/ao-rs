import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
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
import { Board } from "../components/Board";
import { ProjectSidebar } from "../components/ProjectSidebar";
import { SessionDetail } from "../components/SessionDetail";
import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";

const TerminalLazy = lazy(() => import("../components/TerminalView"));

function TerminalPanel({
  baseUrl,
  sessionId,
}: {
  baseUrl: string;
  sessionId: string | null;
}) {
  return (
    <Suspense
      fallback={
        <div className="hint" style={{ minHeight: 360, padding: 8 }}>
          Loading terminal…
        </div>
      }
    >
      <TerminalLazy baseUrl={baseUrl} sessionId={sessionId} />
    </Suspense>
  );
}

export function App() {
  const [baseUrl, setBaseUrl] = useState("http://127.0.0.1:3000");
  const [conn, setConn] = useState<ConnectionStatus>({ kind: "disconnected" });
  const [sessions, setSessions] = useState<ApiSession[]>([]);
  const [events, setEvents] = useState<Array<{ key: string; at: number; evt: ApiEvent }>>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const esRef = useRef<EventSource | null>(null);
  const refreshTimerRef = useRef<number | null>(null);
  const sseReconnectTimerRef = useRef<number | null>(null);
  const sseRetryRef = useRef(0);
  const wireSseRef = useRef<(() => void) | null>(null);
  const [detailOnly, setDetailOnly] = useState(false);
  const [activeTab, setActiveTab] = useState<"dashboard" | { sessionId: string }>("dashboard");
  const [sessionTabs, setSessionTabs] = useState<string[]>([]);
  const [theme, setTheme] = useState<"light" | "dark">(() => {
    const saved = window.localStorage.getItem("ao-ui-theme");
    return saved === "dark" || saved === "light" ? saved : "light";
  });

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
      if (sseReconnectTimerRef.current !== null) {
        window.clearTimeout(sseReconnectTimerRef.current);
        sseReconnectTimerRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    document.body.dataset.theme = theme;
    window.localStorage.setItem("ao-ui-theme", theme);
  }, [theme]);

  /** Fast list — no `gh` / PR enrichment (cheap on every SSE tick). */
  const refreshSessionsFast = useCallback(async () => {
    const s = await getSessions(baseUrl);
    setSessions(s);
  }, [baseUrl]);

  /** Full list with PR + attention (heavier; use after actions or on a timer). */
  const refreshSessionsWithPr = useCallback(async () => {
    const s = await getSessions(baseUrl, { pr: true });
    setSessions(s);
  }, [baseUrl]);

  const scheduleRefresh = useCallback(() => {
    if (refreshTimerRef.current !== null) return;
    refreshTimerRef.current = window.setTimeout(() => {
      refreshTimerRef.current = null;
      refreshSessionsFast().catch(() => {
        // ignore; conn status will reflect SSE errors separately
      });
    }, 400);
  }, [refreshSessionsFast]);

  // Periodically refresh PR/CI signals without hammering the API on every event.
  useEffect(() => {
    if (conn.kind !== "connected") return;
    const id = window.setInterval(() => {
      void refreshSessionsWithPr().catch(() => {});
    }, 45_000);
    return () => window.clearInterval(id);
  }, [conn.kind, baseUrl, refreshSessionsWithPr]);

  // Auto-connect on load and when baseUrl changes: sessions (with PR) + SSE with backoff reconnect.
  useEffect(() => {
    let cancelled = false;

    const clearSseReconnect = () => {
      if (sseReconnectTimerRef.current !== null) {
        window.clearTimeout(sseReconnectTimerRef.current);
        sseReconnectTimerRef.current = null;
      }
    };

    const connectEs = () => {
      if (cancelled) return;
      clearSseReconnect();
      esRef.current?.close();
      esRef.current = connectEvents(baseUrl, {
        onOpen: () => {
          if (cancelled) return;
          setConn({ kind: "connected" });
          sseRetryRef.current = 0;
        },
        onError: () => {
          if (cancelled) return;
          setConn({ kind: "error", message: "SSE connection error" });
          if (sseReconnectTimerRef.current !== null) return;
          const attempt = sseRetryRef.current++;
          const delay = Math.min(30_000, 1000 * Math.pow(2, Math.min(attempt, 5)));
          sseReconnectTimerRef.current = window.setTimeout(() => {
            sseReconnectTimerRef.current = null;
            if (cancelled) return;
            connectEs();
          }, delay);
        },
        onEvent: (evt) => {
          if (cancelled) return;
          // SSE snapshot: update sessions immediately without polling.
          if (
            evt &&
            typeof evt === "object" &&
            (evt as { type?: unknown }).type === "snapshot" &&
            Array.isArray((evt as { sessions?: unknown }).sessions)
          ) {
            setSessions((evt as { sessions: ApiSession[] }).sessions);
            return;
          }
          setEvents((prev) => {
            const at = Date.now();
            const key =
              typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `${at}-${Math.random()}`;
            return [{ key, at, evt }, ...prev].slice(0, 200);
          });
          scheduleRefresh();
        },
      });
    };

    wireSseRef.current = connectEs;

    (async () => {
      setConn({ kind: "connecting" });
      try {
        // Fast path: list sessions without PR enrichment (no per-session `gh` calls).
        // `?pr=true` is heavier (GitHub/`gh` per session). Load fast first, enrich in background.
        const fast = await getSessions(baseUrl);
        if (cancelled) return;
        setSessions(fast);
        connectEs();
        void getSessions(baseUrl, { pr: true })
          .then((enriched) => {
            if (cancelled) return;
            setSessions(enriched);
          })
          .catch(() => {
            /* keep fast list; throttled refresh may retry */
          });
      } catch (e) {
        if (cancelled) return;
        const msg = e instanceof Error ? e.message : "unknown error";
        setConn({ kind: "error", message: msg });
      }
    })();

    return () => {
      cancelled = true;
      wireSseRef.current = null;
      clearSseReconnect();
      esRef.current?.close();
      esRef.current = null;
    };
  }, [baseUrl, scheduleRefresh]);

  const retryConnection = async () => {
    sseRetryRef.current = 0;
    if (sseReconnectTimerRef.current !== null) {
      window.clearTimeout(sseReconnectTimerRef.current);
      sseReconnectTimerRef.current = null;
    }
    esRef.current?.close();
    esRef.current = null;
    setConn({ kind: "connecting" });
    try {
      const fast = await getSessions(baseUrl);
      setSessions(fast);
      wireSseRef.current?.();
      void getSessions(baseUrl, { pr: true })
        .then(setSessions)
        .catch(() => {});
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
        pr: s.pr
          ? {
              number: s.pr.number,
              url: s.pr.url,
              title: s.pr.title,
              owner: s.pr.owner,
              repo: s.pr.repo,
              branch: s.pr.branch,
              baseBranch: s.pr.base_branch,
              isDraft: s.pr.is_draft,
              state: s.pr.state,
              ciStatus: s.pr.ci_status,
              reviewDecision: s.pr.review_decision,
              mergeable: s.pr.mergeable,
              blockers: s.pr.blockers ?? [],
            }
          : null,
        attentionLevel:
          s.attention_level === "merge" ||
          s.attention_level === "respond" ||
          s.attention_level === "review" ||
          s.attention_level === "pending" ||
          s.attention_level === "working" ||
          s.attention_level === "done"
            ? s.attention_level
            : null,
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

  const goToDashboard = () => {
    setActiveTab("dashboard");
    setDetailOnly(false);
    setSelectedProjectId(null);
    setSelectedSessionId(null);
    const params = new URLSearchParams(window.location.search);
    params.delete("session");
    params.delete("view");
    const qs = params.toString();
    window.history.replaceState({}, "", `${window.location.pathname}${qs ? `?${qs}` : ""}`);
  };

  const sessionById = useMemo(() => {
    const m = new Map<string, DashboardSession>();
    for (const s of dashboardSessions) m.set(s.id, s);
    return m;
  }, [dashboardSessions]);

  const [eventsCollapsed, setEventsCollapsed] = useState(false);

  // If a session disappears (killed/archived) or becomes invalid for the current
  // filter/view, automatically close its tab and fall back to Dashboard.
  useEffect(() => {
    setSessionTabs((prev) => prev.filter((sid) => sessionById.has(sid)));
    setSelectedSessionId((prev) => (prev && sessionById.has(prev) ? prev : null));
    setActiveTab((prev) => {
      if (prev === "dashboard") return prev;
      return sessionById.has(prev.sessionId) ? prev : "dashboard";
    });
  }, [sessionById]);

  return (
    <div className="app">
      <div className="topbar">
        <button type="button" className="brand brand--home" onClick={goToDashboard} title="Back to Dashboard">
          <div className="brand__title">Ao Dashboard</div>
          <span className={`pill ${conn.kind === "connected" ? "pill--ok" : conn.kind === "error" ? "pill--bad" : ""}`}>
            <span className="pill__dot" />
            {connLabel}
          </span>
        </button>
        <div className="controls">
          <span className="hint">Dashboard URL</span>
          <input
            size={28}
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
          />
          <button
            type="button"
            className="icon-toggle"
            aria-label={theme === "light" ? "Switch to dark mode" : "Switch to light mode"}
            title={theme === "light" ? "Switch to dark mode" : "Switch to light mode"}
            onClick={() => setTheme((t) => (t === "light" ? "dark" : "light"))}
          >
            <span className="icon-toggle__icon" aria-hidden="true">
              {theme === "light" ? "☾" : "☀"}
            </span>
          </button>
          {conn.kind === "error" ? (
            <button type="button" className="primary" onClick={() => void retryConnection()}>
              Retry
            </button>
          ) : null}
        </div>
      </div>

      {conn.kind === "error" ? (
        <div className="error-banner">
          <span>
            {conn.message}. Sessions may be stale; live updates require SSE. Check the dashboard URL and that ao-dashboard is running.
          </span>
          <button type="button" onClick={() => void retryConnection()}>
            Retry
          </button>
        </div>
      ) : null}

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
                    {(() => {
                      const s = sessionById.get(sid);
                      const status = (s?.status ?? "").toLowerCase();
                      const activity = (s?.activity ?? "").toLowerCase();
                      const badge = status ? `${status}${activity ? `/${activity}` : ""}` : "";
                      return badge ? (
                        <span className="hint" style={{ fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, Liberation Mono, monospace", fontSize: 11 }}>
                          {badge}
                        </span>
                      ) : null;
                    })()}
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
                {conn.kind === "connected" && visibleSessions.length === 0 ? (
                  <section className="panel">
                    <div className="panel__title">Sessions</div>
                    <div style={{ padding: 24 }} className="hint">
                      No sessions match this view. Spawn a session from the CLI or API, or clear the project filter in the sidebar. The list refreshes automatically when the server emits events.
                    </div>
                  </section>
                ) : null}
                <Board
                  title="Sessions"
                  sessions={visibleSessions}
                  onSelect={(s) => setSelectedSessionId(s.id)}
                  onOpen={(s) => openSessionDetail(s.id)}
                />
                <section className="panel">
                  <div
                    className="panel__title"
                    style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 10 }}
                  >
                    <span>Events</span>
                    {events.length > 0 ? (
                      <button
                        type="button"
                        className="icon-btn"
                        aria-label={eventsCollapsed ? "Expand events" : "Collapse events"}
                        title={eventsCollapsed ? "Expand" : "Collapse"}
                        onClick={() => setEventsCollapsed((v) => !v)}
                        data-collapsed={String(eventsCollapsed)}
                      >
                        ↓
                      </button>
                    ) : null}
                  </div>
                  {eventsCollapsed ? null : (
                    <div className={`events ${events.length === 0 ? "events--empty" : ""}`}>
                      {events.length === 0 ? (
                        <div className="hint">No events yet. When SSE is connected, session updates appear here.</div>
                      ) : (
                        events.map(({ key, at, evt }) => {
                          const time = new Date(at).toLocaleString();
                          const type = evt.type ?? "event";
                          const id =
                            typeof evt.id === "string"
                              ? evt.id
                              : typeof (evt as { session_id?: unknown }).session_id === "string"
                                ? ((evt as { session_id: string }).session_id as string)
                                : null;
                          return (
                            <div className="evt" key={key}>
                              <div className="evt__head" style={{ display: "flex", gap: 10, alignItems: "baseline" }}>
                                <div className="evt__type">{type}</div>
                                <div className="evt__time mono" style={{ color: "var(--text-tertiary)", fontSize: 11 }}>
                                  {time}
                                </div>
                                {id ? (
                                  <div
                                    className="evt__id mono"
                                    style={{ marginLeft: "auto", color: "var(--text-tertiary)", fontSize: 11 }}
                                  >
                                    {id.slice(0, 8)}
                                  </div>
                                ) : null}
                              </div>
                              <div className="evt__meta">{JSON.stringify(evt)}</div>
                            </div>
                          );
                        })
                      )}
                    </div>
                  )}
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
                        onSendMessage={async (msg) => {
                          await sendMessage(baseUrl, selectedSession.id, msg);
                          await refreshSessionsWithPr();
                        }}
                        onKill={async () => {
                          await killSession(baseUrl, selectedSession.id);
                          await refreshSessionsWithPr();
                        }}
                        onRestore={async () => {
                          const updated = await restoreSession(baseUrl, selectedSession.id);
                          setSessions((prev) => prev.map((s) => (s.id === updated.id ? updated : s)));
                          await refreshSessionsWithPr();
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
                    <TerminalPanel baseUrl={baseUrl} sessionId={activeTab === "dashboard" ? null : activeTab.sessionId} />
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
                    onSendMessage={async (msg) => {
                      await sendMessage(baseUrl, selectedSession.id, msg);
                      await refreshSessionsWithPr();
                    }}
                    onKill={async () => {
                      await killSession(baseUrl, selectedSession.id);
                      await refreshSessionsWithPr();
                    }}
                    onRestore={async () => {
                      const updated = await restoreSession(baseUrl, selectedSession.id);
                      setSessions((prev) =>
                        prev.map((s) => (s.id === updated.id ? updated : s)),
                      );
                      await refreshSessionsWithPr();
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
                <TerminalPanel baseUrl={baseUrl} sessionId={selectedSessionId} />
              </div>
            </section>
          </div>
        ) : null}

        {/* Events now live under the Dashboard tab */}
      </div>
    </div>
  );
}

