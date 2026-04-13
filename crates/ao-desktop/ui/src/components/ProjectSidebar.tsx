import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";
import { cn } from "../lib/cn";

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
}: {
  sessions: DashboardSession[];
  activeProjectId: string | null;
  activeSessionId: string | null;
  onSelectProject: (projectId: string | null) => void;
  onSelectSession: (sessionId: string) => void;
}) {
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
    <aside className="panel" style={{ height: "100%", overflow: "hidden" }}>
      <div className="panel__title">Projects</div>
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

      <div className="panel__title">Sessions</div>
      <div className="sessions" style={{ overflow: "auto", maxHeight: "calc(100% - 140px)" }}>
        {visibleSorted.map((s) => {
          const level = getAttentionLevel(s);
          const selected = activeSessionId === s.id;
          return (
            <button
              key={s.id}
              type="button"
              className="session-card"
              data-level={level}
              data-selected={String(selected)}
              onClick={() => onSelectSession(s.id)}
              style={{ textAlign: "left" }}
            >
              <div className="session-card__strip" />
              <div className="session-card__top">
                <div className="session-card__id">{s.id.slice(0, 8)}</div>
                <div className="session-card__meta">{level}</div>
              </div>
              <div className="session-card__title">{s.branch ?? s.id}</div>
              <div className="session-card__sub">{s.status ?? "-"}</div>
            </button>
          );
        })}
      </div>
    </aside>
  );
}

