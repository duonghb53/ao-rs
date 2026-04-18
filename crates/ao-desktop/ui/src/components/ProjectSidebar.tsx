import { useMemo, useState } from "react";
import type { DashboardOrchestrator, DashboardSession } from "../lib/types";
import { getDashboardLane, isTerminalSession } from "../lib/types";
import { deriveManagedProjectIds, groupWorkersByOrchestrator } from "../lib/orchestrator";
import { getSessionTitle } from "../lib/format";
import { cn } from "../lib/cn";
import { SessionCard } from "./SessionCard";

export type ProjectInfo = {
  id: string;
  name: string;
  sessionCount: number;
  activeCount: number;
};

function projectLabel(id: string): string {
  return id;
}

// ─── shared sub-components ────────────────────────────────────────────────────

function SectionToggle({
  label,
  collapsed,
  onToggle,
}: {
  label: string;
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      className="panel__title section-toggle"
      onClick={onToggle}
      aria-expanded={!collapsed}
      style={{
        width: "100%",
        display: "flex",
        justifyContent: "space-between",
        alignItems: "center",
        border: "none",
        borderRadius: 0,
        cursor: "pointer",
      }}
    >
      <span>{label}</span>
      <span className="section-toggle__caret" data-collapsed={String(collapsed)} aria-hidden="true">
        ▾
      </span>
    </button>
  );
}

// ─── project-first (fallback) tree ────────────────────────────────────────────

function ProjectFirstTree({
  sessions,
  activeProjectId,
  activeSessionId,
  onSelectProject,
  onSelectSession,
  onOpenSession,
}: {
  sessions: DashboardSession[];
  activeProjectId: string | null;
  activeSessionId: string | null;
  onSelectProject: (pid: string | null, orchId?: string) => void;
  onSelectSession: (sid: string) => void;
  onOpenSession?: (s: DashboardSession) => void;
}) {
  const [sessionsCollapsed, setSessionsCollapsed] = useState(true);

  const byProject = new Map<string, DashboardSession[]>();
  for (const s of sessions) {
    const list = byProject.get(s.projectId) ?? [];
    list.push(s);
    byProject.set(s.projectId, list);
  }

  const projects: ProjectInfo[] = Array.from(byProject.entries())
    .map(([id, list]) => ({
      id,
      name: projectLabel(id),
      sessionCount: list.length,
      activeCount: list.filter((s) => !isTerminalSession(s)).length,
    }))
    .sort((a, b) => b.activeCount - a.activeCount || a.name.localeCompare(b.name));

  const visibleSessions =
    activeProjectId === null
      ? sessions
      : sessions.filter((s) => s.projectId === activeProjectId);

  const visibleSorted = [...visibleSessions].sort((a, b) => {
    const order: Record<string, number> = { merge: 0, review: 1, pending: 2, working: 3 };
    return (order[getDashboardLane(a)] ?? 99) - (order[getDashboardLane(b)] ?? 99);
  });

  const summarySessionList = visibleSorted.length >= 10;

  return (
    <>
      <div className="sessions" style={{ paddingTop: 8 }}>
        <button
          type="button"
          className={cn("project-pill", activeProjectId === null && "project-pill--active")}
          data-selected={String(activeProjectId === null)}
          onClick={() => onSelectProject(null)}
        >
          <span className="project-pill__name">All</span>
          <span className="project-pill__count">{sessions.length}</span>
        </button>
        {projects.map((p) => (
          <button
            key={p.id}
            type="button"
            className={cn("project-pill", activeProjectId === p.id && "project-pill--active")}
            data-selected={String(activeProjectId === p.id)}
            onClick={() => onSelectProject(p.id)}
          >
            <span className="project-pill__name">{p.name}</span>
            <span className="project-pill__count">
              {p.activeCount}/{p.sessionCount}
            </span>
          </button>
        ))}
      </div>

      <SectionToggle
        label={summarySessionList ? "Sessions (summary)" : "Sessions"}
        collapsed={sessionsCollapsed}
        onToggle={() => setSessionsCollapsed((v) => !v)}
      />
      <div
        className={cn("sessions", summarySessionList && "sessions--summary")}
        hidden={sessionsCollapsed}
        style={{ overflow: "auto", flex: "1 1 auto", minHeight: 0 }}
      >
        {visibleSorted.map((s) => {
          const level = getDashboardLane(s);
          const selected = activeSessionId === s.id;
          if (summarySessionList) {
            const title = getSessionTitle(s);
            return (
              <div key={s.id} className="session-summary-row" data-level={level} data-selected={String(selected)}>
                <button
                  type="button"
                  className="session-summary-row__main"
                  onClick={() => onSelectSession(s.id)}
                  title={title}
                >
                  <span className="session-summary-row__strip" aria-hidden="true" />
                  <span className="session-summary-row__project">{s.projectId}</span>
                  <span className="session-summary-row__title">{title}</span>
                  <span className="session-summary-row__meta">
                    {s.status} / {s.activity ?? "-"}
                  </span>
                </button>
                {onOpenSession ? (
                  <button
                    type="button"
                    className="mini-pill mini-pill--terminal session-summary-row__term"
                    title="Open session terminal"
                    onClick={(e) => { e.stopPropagation(); onOpenSession(s); }}
                  >
                    term
                  </button>
                ) : null}
              </div>
            );
          }
          return (
            <div key={s.id} data-level={level} data-selected={String(selected)}>
              <SessionCard session={s} onClick={() => onSelectSession(s.id)} onOpen={onOpenSession} />
            </div>
          );
        })}
      </div>
    </>
  );
}

// ─── orchestrator-first tree ──────────────────────────────────────────────────

function OrchestratorTree({
  sessions,
  orchestrators,
  activeOrchestratorId,
  activeProjectId,
  activeSessionId,
  onSelectOrchestrator,
  onSelectProject,
  onSelectSession,
  onOpenSession,
}: {
  sessions: DashboardSession[];
  orchestrators: DashboardOrchestrator[];
  activeOrchestratorId: string | null;
  activeProjectId: string | null;
  activeSessionId: string | null;
  onSelectOrchestrator: (id: string | null) => void;
  onSelectProject: (pid: string | null, orchId?: string) => void;
  onSelectSession: (sid: string) => void;
  onOpenSession?: (s: DashboardSession) => void;
}) {
  const [expandedOrchestrators, setExpandedOrchestrators] = useState<Set<string>>(
    () => new Set(activeOrchestratorId ? [activeOrchestratorId] : []),
  );
  const [expandedOrchestratorProjects, setExpandedOrchestratorProjects] = useState<Set<string>>(
    () =>
      activeOrchestratorId && activeProjectId
        ? new Set([`${activeOrchestratorId}:${activeProjectId}`])
        : new Set(),
  );
  const [unaffiliatedCollapsed, setUnaffiliatedCollapsed] = useState(true);
  const [expandedUnaffiliatedProjects, setExpandedUnaffiliatedProjects] = useState<Set<string>>(
    new Set(),
  );

  const { byOrchestrator, unaffiliated } = useMemo(
    () => groupWorkersByOrchestrator(sessions, orchestrators),
    [sessions, orchestrators],
  );

  const toggleOrchestrator = (id: string) => {
    setExpandedOrchestrators((prev) => {
      const next = new Set(prev);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });
  };

  const toggleOrchestratorProject = (key: string) => {
    setExpandedOrchestratorProjects((prev) => {
      const next = new Set(prev);
      next.has(key) ? next.delete(key) : next.add(key);
      return next;
    });
  };

  const toggleUnaffiliatedProject = (id: string) => {
    setExpandedUnaffiliatedProjects((prev) => {
      const next = new Set(prev);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });
  };

  // Build unaffiliated project groups
  const unaffiliatedByProject = useMemo(() => {
    const map = new Map<string, DashboardSession[]>();
    for (const s of unaffiliated) {
      const list = map.get(s.projectId) ?? [];
      list.push(s);
      map.set(s.projectId, list);
    }
    return map;
  }, [unaffiliated]);

  return (
    <div style={{ overflow: "auto", flex: "1 1 auto", minHeight: 0 }}>
      {/* Orchestrators */}
      {orchestrators.map((orch) => {
        const workers = byOrchestrator.get(orch.id) ?? [];
        const managedProjectIds = deriveManagedProjectIds(orch, workers);
        const isExpanded = expandedOrchestrators.has(orch.id);
        const isActive = activeOrchestratorId === orch.id && activeProjectId === null;
        const activeWorkerCount = workers.filter((s) => !isTerminalSession(s)).length;

        return (
          <div key={orch.id}>
            {/* Orchestrator row */}
            <button
              type="button"
              className={cn("project-pill", isActive && "project-pill--active")}
              data-selected={String(isActive)}
              style={{ width: "100%", textAlign: "left", display: "flex", alignItems: "center", gap: 6 }}
              onClick={() => {
                toggleOrchestrator(orch.id);
                onSelectOrchestrator(orch.id);
              }}
              aria-expanded={isExpanded}
            >
              <span style={{ fontSize: 10, opacity: 0.6 }}>{isExpanded ? "▾" : "▸"}</span>
              <span className="project-pill__name" style={{ fontFamily: "ui-monospace, monospace", fontSize: 12 }}>
                {orch.id}
              </span>
              <span className="project-pill__count">{activeWorkerCount}/{workers.length}</span>
            </button>

            {/* Project sub-nodes under orchestrator */}
            {isExpanded && managedProjectIds.map((projId) => {
              const projWorkers = workers.filter((s) => s.projectId === projId);
              const projKey = `${orch.id}:${projId}`;
              const isProjExpanded = expandedOrchestratorProjects.has(projKey);
              const isProjActive = activeOrchestratorId === orch.id && activeProjectId === projId;
              const activeProjWorkers = projWorkers.filter((s) => !isTerminalSession(s)).length;

              return (
                <div key={projKey} style={{ paddingLeft: 16 }}>
                  <button
                    type="button"
                    className={cn("project-pill", isProjActive && "project-pill--active")}
                    data-selected={String(isProjActive)}
                    style={{ width: "100%", textAlign: "left", display: "flex", alignItems: "center", gap: 6 }}
                    onClick={() => {
                      toggleOrchestratorProject(projKey);
                      onSelectProject(projId, orch.id);
                    }}
                    aria-expanded={isProjExpanded}
                  >
                    <span style={{ fontSize: 10, opacity: 0.6 }}>{isProjExpanded ? "▾" : "▸"}</span>
                    <span className="project-pill__name">{projectLabel(projId)}</span>
                    <span className="project-pill__count">{activeProjWorkers}/{projWorkers.length}</span>
                  </button>

                  {/* Worker sessions under project */}
                  {isProjExpanded && (
                    <div style={{ paddingLeft: 12 }}>
                      {projWorkers.length === 0 ? (
                        <div className="hint" style={{ padding: "4px 8px", fontSize: 11 }}>
                          No sessions
                        </div>
                      ) : (
                        projWorkers.map((s) => {
                          const level = getDashboardLane(s);
                          const isSessionActive = activeSessionId === s.id;
                          return (
                            <div
                              key={s.id}
                              className="session-summary-row"
                              data-level={level}
                              data-selected={String(isSessionActive)}
                            >
                              <button
                                type="button"
                                className="session-summary-row__main"
                                onClick={() => onSelectSession(s.id)}
                                title={getSessionTitle(s)}
                              >
                                <span className="session-summary-row__strip" aria-hidden="true" />
                                <span className="session-summary-row__title">{getSessionTitle(s)}</span>
                                <span className="session-summary-row__meta">{s.status}</span>
                              </button>
                              {onOpenSession ? (
                                <button
                                  type="button"
                                  className="mini-pill mini-pill--terminal session-summary-row__term"
                                  title="Open session terminal"
                                  onClick={(e) => { e.stopPropagation(); onOpenSession(s); }}
                                >
                                  term
                                </button>
                              ) : null}
                            </div>
                          );
                        })
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        );
      })}

      {/* Unaffiliated sessions */}
      {unaffiliated.length > 0 && (
        <div style={{ borderTop: "1px solid var(--border, rgba(0,0,0,0.1))", marginTop: 8 }}>
          <SectionToggle
            label={`Unaffiliated (${unaffiliated.length})`}
            collapsed={unaffiliatedCollapsed}
            onToggle={() => setUnaffiliatedCollapsed((v) => !v)}
          />
          {!unaffiliatedCollapsed && (
            <div style={{ paddingLeft: 0 }}>
              {Array.from(unaffiliatedByProject.entries()).map(([projId, projSessions]) => {
                const isExpanded = expandedUnaffiliatedProjects.has(projId);
                const isActive = activeOrchestratorId === null && activeProjectId === projId;
                const activeCount = projSessions.filter((s) => !isTerminalSession(s)).length;

                return (
                  <div key={projId}>
                    <button
                      type="button"
                      className={cn("project-pill", isActive && "project-pill--active")}
                      data-selected={String(isActive)}
                      style={{ width: "100%", textAlign: "left", display: "flex", alignItems: "center", gap: 6 }}
                      onClick={() => {
                        toggleUnaffiliatedProject(projId);
                        onSelectProject(projId, undefined);
                      }}
                      aria-expanded={isExpanded}
                    >
                      <span style={{ fontSize: 10, opacity: 0.6 }}>{isExpanded ? "▾" : "▸"}</span>
                      <span className="project-pill__name">{projectLabel(projId)}</span>
                      <span className="project-pill__count">{activeCount}/{projSessions.length}</span>
                    </button>

                    {isExpanded && (
                      <div style={{ paddingLeft: 16 }}>
                        {projSessions.map((s) => {
                          const level = getDashboardLane(s);
                          const isSessionActive = activeSessionId === s.id;
                          return (
                            <div
                              key={s.id}
                              className="session-summary-row"
                              data-level={level}
                              data-selected={String(isSessionActive)}
                            >
                              <button
                                type="button"
                                className="session-summary-row__main"
                                onClick={() => onSelectSession(s.id)}
                                title={getSessionTitle(s)}
                              >
                                <span className="session-summary-row__strip" aria-hidden="true" />
                                <span className="session-summary-row__title">{getSessionTitle(s)}</span>
                                <span className="session-summary-row__meta">{s.status}</span>
                              </button>
                              {onOpenSession ? (
                                <button
                                  type="button"
                                  className="mini-pill mini-pill--terminal session-summary-row__term"
                                  title="Open session terminal"
                                  onClick={(e) => { e.stopPropagation(); onOpenSession(s); }}
                                >
                                  term
                                </button>
                              ) : null}
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ─── public component ─────────────────────────────────────────────────────────

export function ProjectSidebar({
  sessions,
  orchestrators,
  activeProjectId,
  activeSessionId,
  activeOrchestratorId,
  onSelectOrchestrator,
  onSelectProject,
  onSelectSession,
  onOpenSession,
}: {
  sessions: DashboardSession[];
  orchestrators: DashboardOrchestrator[];
  activeProjectId: string | null;
  activeSessionId: string | null;
  activeOrchestratorId: string | null;
  onSelectOrchestrator: (id: string | null) => void;
  onSelectProject: (pid: string | null, orchId?: string) => void;
  onSelectSession: (sid: string) => void;
  onOpenSession?: (session: DashboardSession) => void;
}) {
  const [projectsCollapsed, setProjectsCollapsed] = useState(false);

  const useOrchestratorView = orchestrators.length > 0;

  return (
    <aside
      className="panel"
      style={{
        height: "100%",
        overflow: "hidden",
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
      }}
    >
      <SectionToggle
        label={useOrchestratorView ? "Orchestrators" : "Projects"}
        collapsed={projectsCollapsed}
        onToggle={() => setProjectsCollapsed((v) => !v)}
      />

      {!projectsCollapsed && (
        useOrchestratorView ? (
          <OrchestratorTree
            sessions={sessions}
            orchestrators={orchestrators}
            activeOrchestratorId={activeOrchestratorId}
            activeProjectId={activeProjectId}
            activeSessionId={activeSessionId}
            onSelectOrchestrator={onSelectOrchestrator}
            onSelectProject={onSelectProject}
            onSelectSession={onSelectSession}
            onOpenSession={onOpenSession}
          />
        ) : (
          <ProjectFirstTree
            sessions={sessions}
            activeProjectId={activeProjectId}
            activeSessionId={activeSessionId}
            onSelectProject={onSelectProject}
            onSelectSession={onSelectSession}
            onOpenSession={onOpenSession}
          />
        )
      )}
    </aside>
  );
}
