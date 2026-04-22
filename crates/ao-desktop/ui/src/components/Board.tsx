import { useEffect, useMemo, useState } from "react";
import type { DashboardSession } from "../lib/types";
import { getDashboardLane } from "../lib/types";
import { SessionCard } from "./SessionCard";

const order = ["working", "pending", "review", "merge", "done"] as const;
type Lane = (typeof order)[number];

const labels: Record<Lane, string> = {
  working: "Working",
  pending: "Pending",
  review: "Attention",
  merge: "Merge Ready",
  done: "Done",
};

function formatRelative(ms: number): string {
  if (ms < 5_000) return "just now";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  return `${h}h ago`;
}

export function Board({
  title = "Kanban",
  sessions,
  onSelect,
  onOpen,
  onRestore,
  onSendMessage,
  onMerge,
  onClosePr,
  onDelete,
  leftSlot,
  rightSlot,
}: {
  title?: string;
  sessions: DashboardSession[];
  onSelect?: (s: DashboardSession) => void;
  onOpen?: (s: DashboardSession) => void;
  onRestore?: (s: DashboardSession) => Promise<void>;
  onSendMessage?: (s: DashboardSession, message: string) => Promise<void>;
  onMerge?: (s: DashboardSession) => Promise<void> | void;
  onClosePr?: (s: DashboardSession) => Promise<void> | void;
  onDelete?: (s: DashboardSession) => Promise<void> | void;
  leftSlot?: React.ReactNode;
  rightSlot?: React.ReactNode;
}) {
  const [collapsed, setCollapsed] = useState<Record<Lane, boolean>>({
    working: false,
    pending: false,
    review: false,
    merge: false,
    done: false,
  });
  const grouped: Record<Lane, DashboardSession[]> = {
    working: [],
    pending: [],
    review: [],
    merge: [],
    done: [],
  };
  for (const s of sessions) grouped[getDashboardLane(s)].push(s);

  const lastUpdate = useMemo(() => Date.now(), [sessions]);
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 5000);
    return () => window.clearInterval(id);
  }, []);

  return (
    <section className="board" style={{ "--col-count": order.length } as React.CSSProperties}>
      <div className="board__head">
        {leftSlot ? leftSlot : <h1>{title}</h1>}
        <div className="board__meta">
          <b>{sessions.length} sessions</b> · {formatRelative(now - lastUpdate)}
        </div>
        {rightSlot ? <div className="board__actions">{rightSlot}</div> : null}
      </div>
      <div className="board__cols" role="region" aria-label="Board columns">
        {order.map((lane) => {
          const col = grouped[lane];
          const isCollapsed = collapsed[lane];
          return (
            <section key={lane} className="col" data-tone={lane} data-col={lane}>
              <header className="col__head">
                <span className="col__title">{labels[lane]}</span>
                <span className="col__head-right">
                  <span className="col__count">{col.length}</span>
                  <button
                    type="button"
                    className="btn btn--icon col__toggle"
                    aria-label={isCollapsed ? `Expand ${labels[lane]}` : `Collapse ${labels[lane]}`}
                    title={isCollapsed ? "Expand" : "Collapse"}
                    onClick={() => setCollapsed((prev) => ({ ...prev, [lane]: !prev[lane] }))}
                  >
                    {isCollapsed ? "+" : "–"}
                  </button>
                </span>
              </header>
              <div className="col__body" hidden={isCollapsed}>
                {col.length === 0 ? (
                  <div className="col__empty">No sessions.</div>
                ) : (
                  col.map((s) => (
                    <SessionCard
                      key={s.id}
                      session={s}
                      onClick={onSelect}
                      onOpen={onOpen}
                      onRestore={onRestore}
                      onSendMessage={onSendMessage}
                      onMerge={onMerge}
                      onClosePr={onClosePr}
                      onDelete={onDelete}
                    />
                  ))
                )}
              </div>
            </section>
          );
        })}
      </div>
    </section>
  );
}
