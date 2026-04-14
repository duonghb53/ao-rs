import { useState } from "react";
import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";
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
  const [sessionsCollapsed, setSessionsCollapsed] = useState(false);

  const byProject = new Map<string, DashboardSession[]>();
  for (const s of sessions) {
    const list = byProject.get(s.projectId) ?? [];
    list.push(s);
    byProject.set(s.projectId, list);
  }

  const projects: ProjectInfo[] = Array.from(byProject.entries())
    .map(([id, list]) => {
      const activeCount = list.filter((s) => getAttentionLevel(s) !== "done").length;
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
    const la = getAttentionLevel(a);
    const lb = getAttentionLevel(b);
    const order: Record<string, number> = {
      respond: 0,
      merge: 1,
      review: 2,
      pending: 3,
      working: 4,
      done: 5,
    };
    return (order[la] ?? 99) - (order[lb] ?? 99);
  });

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
        <span>Sessions</span>
        <span className="section-toggle__caret" data-collapsed={String(sessionsCollapsed)} aria-hidden="true">
          ▾
        </span>
      </button>
      <div
        className="sessions"
        hidden={sessionsCollapsed}
        style={{
          overflow: "auto",
          flex: "1 1 auto",
          minHeight: 0,
        }}
      >
          {visibleSorted.map((s) => {
            const level = getAttentionLevel(s);
            const selected = activeSessionId === s.id;
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

