import { useMemo, useState } from "react";
import type { DashboardSession } from "../lib/types";
import { getDashboardLane, isTerminalSession } from "../lib/types";
import { getSessionTitle } from "../lib/format";
import { cn } from "../lib/cn";
import { SessionCard } from "./SessionCard";

type ProjectInfo = {
  id: string;
  sessionCount: number;
  activeCount: number;
};

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

export function ProjectSidebar({
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
  onSelectProject: (pid: string | null) => void;
  onSelectSession: (sid: string) => void;
  onOpenSession?: (session: DashboardSession) => void;
}) {
  const [projectsCollapsed, setProjectsCollapsed] = useState(false);
  const [sessionsCollapsed, setSessionsCollapsed] = useState(true);

  const byProject = useMemo(() => {
    const map = new Map<string, DashboardSession[]>();
    for (const s of sessions) {
      const list = map.get(s.projectId) ?? [];
      list.push(s);
      map.set(s.projectId, list);
    }
    return map;
  }, [sessions]);

  const projects: ProjectInfo[] = useMemo(
    () =>
      Array.from(byProject.entries())
        .map(([id, list]) => ({
          id,
          sessionCount: list.length,
          activeCount: list.filter((s) => !isTerminalSession(s)).length,
        }))
        .sort((a, b) => b.activeCount - a.activeCount || a.id.localeCompare(b.id)),
    [byProject],
  );

  const visibleSorted = useMemo(() => {
    const visible =
      activeProjectId === null
        ? sessions
        : sessions.filter((s) => s.projectId === activeProjectId);
    const order: Record<string, number> = { merge: 0, review: 1, pending: 2, working: 3 };
    return [...visible].sort(
      (a, b) => (order[getDashboardLane(a)] ?? 99) - (order[getDashboardLane(b)] ?? 99),
    );
  }, [sessions, activeProjectId]);

  const summarySessionList = visibleSorted.length >= 10;

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
        label="Projects"
        collapsed={projectsCollapsed}
        onToggle={() => setProjectsCollapsed((v) => !v)}
      />

      {!projectsCollapsed && (
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
                <span className="project-pill__name">{p.id}</span>
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
      )}
    </aside>
  );
}
