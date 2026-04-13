import { memo } from "react";
import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";
import { getSessionTitle } from "../lib/format";
import { cn } from "../lib/cn";

interface SessionCardProps {
  session: DashboardSession;
  onClick?: (session: DashboardSession) => void;
  onOpen?: (session: DashboardSession) => void;
}

function SessionCardView({ session, onClick, onOpen }: SessionCardProps) {
  const level = getAttentionLevel(session);
  const title = getSessionTitle(session);
  const secondary =
    session.branch ? session.branch : session.summary && session.summary !== title ? session.summary : null;

  return (
    <button
      type="button"
      className={cn("session-card", "w-full text-left")}
      onClick={() => onClick?.(session)}
      data-level={level}
    >
      <div className="session-card__strip" />
      <div className="session-card__top">
        <div className="session-card__id">{session.id.slice(0, 8)}</div>
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <div className="session-card__meta">
            {session.status ?? "-"} / {session.activity ?? "-"}
          </div>
          {onOpen ? (
            <span
              role="button"
              tabIndex={0}
              className="mini-pill"
              title="Open Session Detail in new tab"
              style={{ cursor: "pointer", userSelect: "none" }}
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onOpen(session);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  e.stopPropagation();
                  onOpen(session);
                }
              }}
            >
              ↗
            </span>
          ) : null}
        </div>
      </div>
      <div className="session-card__title">{title}</div>
      {secondary ? <div className="session-card__sub">{secondary}</div> : null}
    </button>
  );
}

export const SessionCard = memo(SessionCardView);

