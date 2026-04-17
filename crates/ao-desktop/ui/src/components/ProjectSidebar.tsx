import { useState } from "react";
import type { DashboardSession } from "../lib/types";
import { getDashboardLane, isTerminalSession } from "../lib/types";
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
  onSelectProject: (projectId: string | null) => void;
  onSelectSession: (sessionId: string) => void;
  onOpenSession?: (session: DashboardSession) => void;
}) {
  const [projectsCollapsed, setProjectsCollapsed] = useState(false);
  /** Default collapsed so the main board stays primary; user can expand. */
  const [sessionsCollapsed, setSessionsCollapsed] = useState(true);

  const byProject = new Map<string, DashboardSession[]>();
  for (const s of sessions) {
    const list = byProject.get(s.projectId) ?? [];
    list.push(s);
    byProject.set(s.projectId, list);
  }

  const projects: ProjectInfo[] = Array.from(byProject.entries())
    .map(([id, list]) => {
      const activeCount = list.filter((s) => !isTerminalSession(s)).length;
      return {
        id,
        name: projectLabel(id),
        sessionCount: list.length,
        activeCount,
      };
    })
    .sort((a, b) => b.activeCount - a.activeCount || a.name.localeCompare(b.name));

  const visibleSessions =
    activeProjectId === null
      ? sessions
      : sessions.filter((s) => s.projectId === activeProjectId);

  const visibleSorted = [...visibleSessions].sort((a, b) => {
    const la = getDashboardLane(a);
    const lb = getDashboardLane(b);
    const order: Record<string, number> = {
      merge: 0,
      review: 1,
      pending: 2,
      working: 3,
    };
    return (order[la] ?? 99) - (order[lb] ?? 99);
  });

  /** Dense one-line rows when many sessions — easier to scan than full cards. */
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
      <button
        type="button"
        className="panel__title section-toggle"
        onClick={() => setProjectsCollapsed((v) => !v)}
        aria-expanded={!projectsCollapsed}
        title={projectsCollapsed ? "Expand Projects" : "Collapse Projects"}
        style={{ width: "100%", display: "flex", justifyContent: "space-between", alignItems: "center", border: "none", borderRadius: 0, cursor: "pointer" }}
      >
        <span>Projects</span>
        <span className="section-toggle__caret" data-collapsed={String(projectsCollapsed)} aria-hidden="true">
          ▾
        </span>
      </button>
      <div className="sessions" style={{ paddingTop: 8 }} hidden={projectsCollapsed}>
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

      <button
        type="button"
        className="panel__title section-toggle"
        onClick={() => setSessionsCollapsed((v) => !v)}
        aria-expanded={!sessionsCollapsed}
        title={sessionsCollapsed ? "Expand Sessions" : "Collapse Sessions"}
        style={{ width: "100%", display: "flex", justifyContent: "space-between", alignItems: "center", border: "none", borderRadius: 0, cursor: "pointer" }}
      >
        <span>
          Sessions
          {summarySessionList ? (
            <span className="hint" style={{ marginLeft: 8, fontWeight: 500, fontSize: 11 }}>
              (summary)
            </span>
          ) : null}
        </span>
        <span className="section-toggle__caret" data-collapsed={String(sessionsCollapsed)} aria-hidden="true">
          ▾
        </span>
      </button>
      <div
        className={cn("sessions", summarySessionList && "sessions--summary")}
        hidden={sessionsCollapsed}
        style={{
          overflow: "auto",
          flex: "1 1 auto",
          minHeight: 0,
        }}
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
                      onClick={(e) => {
                        e.stopPropagation();
                        onOpenSession(s);
                      }}
                    >
                      term
                    </button>
                  ) : null}
                </div>
              );
            }
            return (
              <div key={s.id} data-level={level} data-selected={String(selected)}>
                <SessionCard
                  session={s}
                  onClick={() => onSelectSession(s.id)}
                  onOpen={onOpenSession}
                />
              </div>
            );
          })}
      </div>
    </aside>
  );
}

