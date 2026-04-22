import { lazy, Suspense, useCallback, useEffect, useMemo, useState } from "react";
import {
  type ApiEvent,
  type BacklogIssue,
  killSession,
  mergePr,
  restoreSession,
  sendMessage,
  spawnSession,
} from "../api/client";
import { Board } from "../components/Board";
import { IssuesPanel } from "../components/IssuesPanel";
import { ProjectSidebar } from "../components/ProjectSidebar";
import { SessionDetail } from "../components/SessionDetail";
import { useSessions } from "../hooks/useSessions";
import { useToasts } from "../hooks/useToasts";
import { formatEvent, getSessionTabLabel } from "../lib/format";
import { type DashboardSession, isOrchestratorSessionId, isTerminalSession } from "../lib/types";

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

function ActiveDetail({
  session,
  terminalSlot,
}: {
  session: DashboardSession | null;
  terminalSlot?: React.ReactNode;
}) {
  if (!session) return <div className="hint">select a session</div>;
  return <SessionDetail session={session} terminalSlot={terminalSlot} />;
}

export function App() {
  const [baseUrl, setBaseUrl] = useState("http://127.0.0.1:3000");
  const [events, setEvents] = useState<Array<{ key: string; at: number; evt: ApiEvent }>>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [detailOnly, setDetailOnly] = useState(false);
  const [activeTab, setActiveTab] = useState<"dashboard" | "backlog" | { sessionId: string }>("dashboard");
  const [sessionTabs, setSessionTabs] = useState<string[]>([]);
  const [theme, setTheme] = useState<"light" | "dark">(() => {
    const saved = window.localStorage.getItem("ao-ui-theme");
    return saved === "dark" || saved === "light" ? saved : "dark";
  });

  const { toasts, pushToast, dismissToast } = useToasts();

  const logEvent = useCallback((evt: ApiEvent) => {
    setEvents((prev) => {
      const at = Date.now();
      const key =
        typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `${at}-${Math.random()}`;
      return [{ key, at, evt }, ...prev].slice(0, 200);
    });
  }, []);

  const {
    sessions,
    setSessions,
    conn,
    refreshSessionsWithPr,
    retryConnection,
  } = useSessions(baseUrl, {
    onNotification: pushToast,
    onEvent: logEvent,
  });

  const connState: "connected" | "error" | "idle" =
    conn.kind === "connected" ? "connected" : conn.kind === "error" ? "error" : "idle";

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const sid = params.get("session");
    const view = params.get("view");
    if (sid) setSelectedSessionId(sid);
    if (view === "detail") setDetailOnly(true);
    if (sid && view === "detail") {
      setActiveTab({ sessionId: sid });
      setSessionTabs([sid]);
    }
  }, []);

  useEffect(() => {
    document.body.dataset.theme = theme;
    window.localStorage.setItem("ao-ui-theme", theme);
  }, [theme]);

  const dashboardSessions: DashboardSession[] = useMemo(
    () =>
      sessions.map((s) => ({
        id: s.id,
        projectId: s.project_id,
        status: s.status,
        activity: s.activity ?? null,
        agent: s.agent ?? null,
        branch: s.branch ?? null,
        summary: s.task ?? null,
        summaryIsFallback: false,
        issueTitle: s.issue_id ? (s.task ?? null) : null,
        issueId: s.issue_id ?? null,
        issueUrl: s.issue_url ?? null,
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
        spawnedBy: s.spawned_by ?? null,
        createdAt: typeof s.created_at === "number" ? s.created_at : null,
        claimedPrNumber: typeof s.claimed_pr_number === "number" ? s.claimed_pr_number : null,
        claimedPrUrl: s.claimed_pr_url ?? null,
      })),
    [sessions],
  );

  const workerSessions = useMemo(
    () => dashboardSessions.filter((s) => !isOrchestratorSessionId(s.id)),
    [dashboardSessions],
  );

  const visibleSessions = useMemo(
    () =>
      selectedProjectId === null
        ? workerSessions
        : workerSessions.filter((s) => s.projectId === selectedProjectId),
    [workerSessions, selectedProjectId],
  );

  const activeCount = useMemo(
    () => dashboardSessions.filter((s) => !isTerminalSession(s)).length,
    [dashboardSessions],
  );

  const selectedSession = useMemo(() => {
    if (!selectedSessionId) return null;
    return dashboardSessions.find((s) => s.id === selectedSessionId) ?? null;
  }, [dashboardSessions, selectedSessionId]);

  const activeSessionId = useMemo(() => {
    if (activeTab === "dashboard" || activeTab === "backlog") return selectedSessionId;
    return activeTab.sessionId;
  }, [activeTab, selectedSessionId]);

  const activeSession = useMemo(() => {
    if (!activeSessionId) return null;
    return dashboardSessions.find((s) => s.id === activeSessionId) ?? null;
  }, [dashboardSessions, activeSessionId]);

  useEffect(() => {
    if (activeTab === "dashboard" || activeTab === "backlog" || !activeSession) {
      document.title = "Ao Dashboard";
      return;
    }
    document.title = `Ao — ${getSessionTabLabel(activeSession)}`;
  }, [activeTab, activeSession]);

  const openSessionDetail = (id: string) => {
    setSelectedSessionId(id);
    setActiveTab({ sessionId: id });
    setSessionTabs((prev) => (prev.includes(id) ? prev : [id, ...prev].slice(0, 12)));
  };

  const closeSessionTab = (id: string) => {
    setSessionTabs((prev) => prev.filter((t) => t !== id));
    setActiveTab((prev) => {
      if (prev !== "dashboard" && prev !== "backlog" && prev.sessionId === id) return "dashboard";
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

  useEffect(() => {
    setSessionTabs((prev) => prev.filter((sid) => sessionById.has(sid)));
    setSelectedSessionId((prev) => (prev && sessionById.has(prev) ? prev : null));
    setActiveTab((prev) => {
      if (prev === "dashboard" || prev === "backlog") return prev;
      return sessionById.has(prev.sessionId) ? prev : "dashboard";
    });
  }, [sessionById]);

  const spawnFromIssue = useCallback(
    async (issue: BacklogIssue) => {
      const created = await spawnSession(baseUrl, {
        project_id: issue.project_id,
        task: issue.title,
        issue_id: String(issue.number),
        issue_url: issue.url,
      });
      setSessions((prev) => {
        const existing = prev.findIndex((s) => s.id === created.id);
        if (existing === -1) return [created, ...prev];
        const copy = prev.slice();
        copy[existing] = created;
        return copy;
      });
      await refreshSessionsWithPr();
    },
    [baseUrl, refreshSessionsWithPr, setSessions],
  );

  const tabsBar = (
    <div className="board__tabs" role="tablist">
      <button
        type="button"
        role="tab"
        className="tab-btn"
        aria-current={activeTab === "dashboard" ? "true" : "false"}
        onClick={() => setActiveTab("dashboard")}
      >
        dashboard
      </button>
      <button
        type="button"
        role="tab"
        className="tab-btn"
        aria-current={activeTab === "backlog" ? "true" : "false"}
        onClick={() => setActiveTab("backlog")}
      >
        backlog
      </button>
      {sessionTabs.map((sid) => {
        const s = sessionById.get(sid);
        const label = s ? getSessionTabLabel(s) : sid.slice(0, 8);
        const current = activeTab !== "dashboard" && activeTab !== "backlog" && activeTab.sessionId === sid;
        return (
          <span key={sid} className="tab-btn-wrap" style={{ display: "inline-flex", alignItems: "center" }}>
            <button
              type="button"
              role="tab"
              className="tab-btn"
              aria-current={current ? "true" : "false"}
              onClick={() => {
                setActiveTab({ sessionId: sid });
                setSelectedSessionId(sid);
              }}
              title={sid}
            >
              {label}
            </button>
            <button
              type="button"
              className="tab-btn__close"
              aria-label="Close tab"
              onClick={() => closeSessionTab(sid)}
            >
              ×
            </button>
          </span>
        );
      })}
    </div>
  );

  const controls = (
    <div className="top-actions">
      <span className="hint" title="Non-terminal sessions" aria-label={`${activeCount} active sessions`}>
        {activeCount} active
      </span>
      <input
        className="btn"
        size={22}
        value={baseUrl}
        onChange={(e) => setBaseUrl(e.target.value)}
        aria-label="Dashboard base URL"
        title="Dashboard base URL"
      />
      <button
        type="button"
        className="btn"
        aria-label={theme === "light" ? "Switch to dark mode" : "Switch to light mode"}
        title={theme === "light" ? "Switch to dark mode" : "Switch to light mode"}
        onClick={() => setTheme((t) => (t === "light" ? "dark" : "light"))}
      >
        {theme === "light" ? "\u263e" : "\u2600"}
      </button>
      {conn.kind === "error" ? (
        <button type="button" className="btn" onClick={() => void retryConnection()}>
          retry
        </button>
      ) : null}
      <button type="button" className="btn" onClick={goToDashboard} title="Back to dashboard">
        home
      </button>
    </div>
  );

  return (
    <div className="app" data-sidebar={detailOnly ? "false" : "true"}>
      {toasts.length ? (
        <div className="toast-stack" aria-live="polite" aria-relevant="additions">
          {toasts.map((t) => {
            const openToast = () => {
              setSelectedSessionId(t.sessionId);
              setActiveTab({ sessionId: t.sessionId });
              setSessionTabs((prev) => (prev.includes(t.sessionId) ? prev : [t.sessionId, ...prev]));
            };
            return (
              <div
                key={t.key}
                role="button"
                tabIndex={0}
                className={`toast ${t.priority ? `toast--${t.priority}` : ""}`}
                onClick={openToast}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    openToast();
                  }
                }}
                title="Open session"
              >
                <div className="toast__title">
                  <span className="mono">{t.reactionKey}</span>
                  {t.action ? <span className="toast__meta">{t.action}</span> : null}
                </div>
                {t.message ? <div className="toast__body">{t.message}</div> : null}
                <button
                  type="button"
                  className="toast__close"
                  aria-label="Dismiss"
                  onClick={(e) => {
                    e.stopPropagation();
                    dismissToast(t.key);
                  }}
                >
                  ×
                </button>
              </div>
            );
          })}
        </div>
      ) : null}

      {detailOnly ? null : (
        <ProjectSidebar
          sessions={workerSessions}
          activeProjectId={selectedProjectId}
          activeSessionId={activeSessionId}
          onSelectProject={(pid) => {
            setSelectedProjectId(pid);
            if (pid !== null && selectedSessionId) {
              const exists = dashboardSessions.some((s) => s.projectId === pid && s.id === selectedSessionId);
              if (!exists) setSelectedSessionId(null);
            }
          }}
          onSelectSession={(sid) => setSelectedSessionId(sid)}
          onOpenSession={(s) => openSessionDetail(s.id)}
          baseUrl={baseUrl}
          connState={connState}
        />
      )}

      <div className="main-pane" style={{ display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0 }}>
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

        {detailOnly ? (
          <>
            <div className="topbar">
              <div className="crumb">
                <span className="cur mono">
                  {selectedSessionId ? selectedSessionId.slice(0, 8) : "(none)"}
                </span>
              </div>
              <div className="top-actions">{controls}</div>
            </div>
            <section className="panel" style={{ margin: "12px 16px 16px" }}>
              <ActiveDetail
                session={selectedSession}
                terminalSlot={<TerminalPanel baseUrl={baseUrl} sessionId={selectedSessionId} />}
              />
            </section>
          </>
        ) : activeTab === "dashboard" ? (
          <>
            <Board
              title="Kanban"
              sessions={visibleSessions}
              onSelect={(s) => setSelectedSessionId(s.id)}
              onOpen={(s) => openSessionDetail(s.id)}
              onRestore={async (s) => {
                const updated = await restoreSession(baseUrl, s.id);
                setSessions((prev) => prev.map((x) => (x.id === updated.id ? updated : x)));
                await refreshSessionsWithPr();
              }}
              onSendMessage={async (s, msg) => {
                await sendMessage(baseUrl, s.id, msg);
                await refreshSessionsWithPr();
              }}
              onMerge={async (s) => {
                const prNumber = s.pr?.number ?? s.claimedPrNumber;
                if (typeof prNumber !== "number") {
                  openSessionDetail(s.id);
                  return;
                }
                await mergePr(baseUrl, prNumber);
                await refreshSessionsWithPr();
              }}
              onDelete={async (s) => {
                await killSession(baseUrl, s.id);
                await refreshSessionsWithPr();
              }}
              leftSlot={tabsBar}
              rightSlot={controls}
            />

            {conn.kind === "connected" && visibleSessions.length === 0 ? (
              <section className="panel" style={{ margin: "0 16px 16px" }}>
                <div className="panel__title">Sessions</div>
                <div style={{ padding: 24 }} className="hint">
                  No sessions match this view. Spawn a session from the CLI or API, or clear the project filter in the sidebar.
                </div>
              </section>
            ) : null}

            <section className="panel" style={{ margin: "0 16px 16px" }}>
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
                      const evtRec = evt as unknown as Record<string, unknown>;
                      const id =
                        typeof evtRec.id === "string"
                          ? (evtRec.id as string)
                          : typeof evtRec.session_id === "string"
                            ? (evtRec.session_id as string)
                            : null;
                      return (
                        <div className="evt" key={key}>
                          <div className="evt__head" style={{ display: "flex", gap: 10, alignItems: "baseline" }}>
                            <div className="evt__type">{type}</div>
                            <div className="evt__time mono" style={{ color: "var(--text-3)", fontSize: 11 }}>
                              {time}
                            </div>
                            {id ? (
                              <div
                                className="evt__id mono"
                                style={{ marginLeft: "auto", color: "var(--text-3)", fontSize: 11 }}
                              >
                                {id.slice(0, 8)}
                              </div>
                            ) : null}
                          </div>
                          <div className="evt__meta">{formatEvent(evt)}</div>
                        </div>
                      );
                    })
                  )}
                </div>
              )}
            </section>
          </>
        ) : activeTab === "backlog" ? (
          <>
            <div className="board__head">
              <h1>Backlog</h1>
              <div className="board__meta" />
              <div className="board__actions">
                {tabsBar}
                {controls}
              </div>
            </div>
            <section className="panel" style={{ margin: "0 16px 16px" }}>
              <IssuesPanel baseUrl={baseUrl} projectId={selectedProjectId} onSpawn={spawnFromIssue} />
            </section>
          </>
        ) : (
          <>
            <div className="topbar">
              <div className="crumb">
                <button type="button" onClick={goToDashboard}>
                  ← orchestrator
                </button>
                <span className="sep">/</span>
                <span className="cur mono">{activeTab.sessionId.slice(0, 8)}</span>
              </div>
              <div className="top-actions">
                {tabsBar}
                {controls}
              </div>
            </div>
            <section className="panel" style={{ margin: "12px 16px 16px" }}>
              <ActiveDetail
                session={activeSession}
                terminalSlot={<TerminalPanel baseUrl={baseUrl} sessionId={activeTab.sessionId} />}
              />
            </section>
          </>
        )}
      </div>
    </div>
  );
}
