import { useMemo, useState, type CSSProperties } from "react";
import type { DashboardSession } from "../lib/types";
import { getDashboardLane } from "../lib/types";
import { getSessionTitle } from "../lib/format";
import { projectAccentStyle } from "../lib/projectColors";

type ProjectInfo = {
  id: string;
  sessions: DashboardSession[];
};

type ChildLevel = "" | "review" | "respond" | "merge" | "selected";

function childLevel(session: DashboardSession, selected: boolean): ChildLevel {
  if (selected) return "selected";
  if ((session.activity ?? "").toLowerCase() === "waiting_input") return "respond";
  const lane = getDashboardLane(session);
  if (lane === "review") return "review";
  if (lane === "merge") return "merge";
  return "";
}

export function ProjectSidebar({
  sessions,
  activeProjectId,
  activeSessionId,
  onSelectProject,
  onSelectSession,
  baseUrl,
  version,
  connState = "connected",
}: {
  sessions: DashboardSession[];
  activeProjectId: string | null;
  activeSessionId: string | null;
  onSelectProject: (pid: string | null) => void;
  onSelectSession: (sid: string) => void;
  onOpenSession?: (session: DashboardSession) => void;
  baseUrl?: string;
  version?: string;
  connState?: "connected" | "error" | "idle";
}) {
  const [collapsed, setCollapsed] = useState(false);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  const projects: ProjectInfo[] = useMemo(() => {
    const map = new Map<string, DashboardSession[]>();
    for (const s of sessions) {
      const list = map.get(s.projectId) ?? [];
      list.push(s);
      map.set(s.projectId, list);
    }
    return Array.from(map.entries())
      .map(([id, list]) => ({ id, sessions: list }))
      .sort((a, b) => b.sessions.length - a.sessions.length || a.id.localeCompare(b.id));
  }, [sessions]);

  const toggleProject = (pid: string) => {
    setExpanded((prev) => ({ ...prev, [pid]: !prev[pid] }));
  };

  const footLabel = (() => {
    if (!baseUrl) return "orch.local";
    try {
      const url = new URL(baseUrl);
      return `${url.hostname}${url.port ? `:${url.port}` : ""}`;
    } catch {
      return baseUrl;
    }
  })();

  return (
    <aside className="sidebar">
      <div className="sidebar__head">
        <span>Projects</span>
        <button
          type="button"
          className="icon-btn"
          aria-label={collapsed ? "Expand projects" : "Collapse projects"}
          onClick={() => setCollapsed((v) => !v)}
        >
          {collapsed ? "\u25b8" : "\u25be"}
        </button>
      </div>

      {collapsed ? null : (
        <div className="sidebar__list">
          <div
            className="proj-row"
            data-selected={String(activeProjectId === null)}
            onClick={() => onSelectProject(null)}
          >
            <span className="proj-row__name">
              <span aria-hidden="true">&nbsp;</span>
              All
            </span>
            <span className="proj-row__count">{sessions.length}</span>
          </div>

          {projects.map((p) => {
            const isOpen = expanded[p.id] ?? false;
            const selected = activeProjectId === p.id;
            const style = projectAccentStyle(p.id) as CSSProperties;
            return (
              <div key={p.id}>
                <div
                  className="proj-row"
                  data-selected={String(selected)}
                  style={style}
                  onClick={() => {
                    onSelectProject(p.id);
                    toggleProject(p.id);
                  }}
                >
                  <span className="proj-row__name">
                    <span aria-hidden="true">{isOpen ? "\u25be" : "\u25b8"}</span>
                    {p.id}
                  </span>
                  <span className="proj-row__count">{p.sessions.length}</span>
                </div>
                {isOpen ? (
                  <div className="proj-children">
                    {p.sessions.map((s) => {
                      const level = childLevel(s, activeSessionId === s.id);
                      return (
                        <div
                          key={s.id}
                          className="proj-child"
                          data-level={level}
                          onClick={(e) => {
                            e.stopPropagation();
                            onSelectSession(s.id);
                          }}
                          title={`${s.projectId} · ${s.status}${s.activity ? ` / ${s.activity}` : ""}`}
                        >
                          {getSessionTitle(s)}
                        </div>
                      );
                    })}
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>
      )}

      <div className="sidebar__foot">
        <span className="dot" data-state={connState} aria-hidden="true" />
        <span>{footLabel}</span>
        {version ? <span className="sidebar__foot__spacer">{version}</span> : null}
      </div>
    </aside>
  );
}
