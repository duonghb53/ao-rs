import { useMemo, useState } from "react";
import type { AttentionLevel, DashboardSession } from "../lib/types";
import { getDashboardLane } from "../lib/types";
import { SessionCard } from "./SessionCard";

const order = ["working", "pending", "review", "merge"] as const;
type Lane = (typeof order)[number];

const labels: Record<Lane, string> = {
  working: "Working",
  pending: "Pending",
  review: "Review",
  merge: "Merge",
};

export function Board({
  title = "Board",
  sessions,
  onSelect,
  onOpen,
  rightActionLabel,
  onRightAction,
}: {
  title?: string;
  sessions: DashboardSession[];
  onSelect?: (s: DashboardSession) => void;
  onOpen?: (s: DashboardSession) => void;
  rightActionLabel?: string;
  onRightAction?: () => void;
}) {
  const grouped: Record<Lane, DashboardSession[]> = { working: [], pending: [], review: [], merge: [] };

  for (const s of sessions) grouped[getDashboardLane(s)].push(s);

  const [collapsed, setCollapsed] = useState<Record<Lane, boolean>>({
    working: false,
    pending: false,
    review: false,
    merge: false,
  });

  const toggle = useMemo(
    () => (level: AttentionLevel) => {
      setCollapsed((prev) => ({ ...prev, [level]: !prev[level] }));
    },
    [],
  );

  return (
    <div className="board">
      <div className="board__toolbar">
        <div className="board__crumbs">
          <span className="board__crumb">ao-rs</span>
          <span className="board__sep">›</span>
          <span className="board__crumb board__crumb--strong">{title}</span>
        </div>
        <div className="board__tools">
          {rightActionLabel ? (
            <button type="button" className="primary" onClick={onRightAction}>
              {rightActionLabel}
            </button>
          ) : null}
        </div>
      </div>

      <div className="board__scroller" role="region" aria-label="Board columns">
        <div className="board__columns">
          {order.map((level) => {
            const col = grouped[level];
            const isCollapsed = collapsed[level];
            return (
              <section key={level} className="board-col" data-col={level}>
                <div className="board-col__header">
                  <div className="board-col__title">
                    <span className="status-chip" data-tone={level}>
                      <span className="status-chip__dot" aria-hidden="true" />
                      {labels[level]}
                    </span>
                    <span className="board-col__count">{col.length}</span>
                  </div>
                  <div className="board-col__actions">
                    <button
                      type="button"
                      className="icon-btn board-col__caret"
                      data-collapsed={String(isCollapsed)}
                      title={isCollapsed ? "Expand" : "Collapse"}
                      aria-label={isCollapsed ? "Expand column" : "Collapse column"}
                      onClick={() => toggle(level)}
                    >
                      ↓
                    </button>
                  </div>
                </div>
                {isCollapsed ? null : (
                  <div className="board-col__body">
                    {col.length === 0 ? (
                      <div className="hint">No sessions.</div>
                    ) : (
                      col.map((s) => (
                        <SessionCard key={s.id} session={s} onClick={onSelect} onOpen={onOpen} />
                      ))
                    )}
                  </div>
                )}
              </section>
            );
          })}
        </div>
      </div>
    </div>
  );
}

