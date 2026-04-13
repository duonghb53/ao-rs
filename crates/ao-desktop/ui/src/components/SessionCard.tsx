import { memo } from "react";
import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";
import { getSessionTitle } from "../lib/format";
import { cn } from "../lib/cn";

interface SessionCardProps {
  session: DashboardSession;
  onClick?: (session: DashboardSession) => void;
}

function SessionCardView({ session, onClick }: SessionCardProps) {
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
        <div className="session-card__meta">
          {session.status ?? "-"} / {session.activity ?? "-"}
        </div>
      </div>
      <div className="session-card__title">{title}</div>
      {secondary ? <div className="session-card__sub">{secondary}</div> : null}
    </button>
  );
}

export const SessionCard = memo(SessionCardView);

