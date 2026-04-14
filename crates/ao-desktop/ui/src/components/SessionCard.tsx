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

function openIssue(url: string) {
  window.open(url, "_blank", "noopener,noreferrer");
}

function SessionCardView({ session, onClick, onOpen }: SessionCardProps) {
  const level = getAttentionLevel(session);
  const title = getSessionTitle(session);
  const secondary =
    session.branch ? session.branch : session.summary && session.summary !== title ? session.summary : null;
  const pr = session.pr;
  const issueUrl = session.issueUrl;
  const issueId = session.issueId;

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
              className="mini-pill mini-pill--terminal"
              title="Open session terminal"
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
              terminal
            </span>
          ) : null}
        </div>
      </div>
      <div className="session-card__title">
        {issueUrl && issueId ? (
          <>
            <span
              role="link"
              tabIndex={0}
              title={issueUrl}
              style={{ cursor: "pointer", userSelect: "none" }}
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                openIssue(issueUrl);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  e.stopPropagation();
                  openIssue(issueUrl);
                }
              }}
            >
              #{issueId}
            </span>{" "}
            <span>{session.issueTitle ?? title}</span>
          </>
        ) : (
          title
        )}
      </div>
      {secondary ? <div className="session-card__sub">{secondary}</div> : null}
      {pr ? (
        <div className="session-card__pills">
          <span className="mini-pill">PR #{pr.number}</span>
          {pr.ciStatus ? <span className="mini-pill">CI: {pr.ciStatus}</span> : null}
          {pr.reviewDecision ? <span className="mini-pill">Review: {pr.reviewDecision}</span> : null}
          {typeof pr.mergeable === "boolean" ? (
            <span className="mini-pill">{pr.mergeable ? "mergeable" : "not mergeable"}</span>
          ) : null}
          {pr.blockers && pr.blockers.length > 0 ? <span className="mini-pill">blockers: {pr.blockers.length}</span> : null}
        </div>
      ) : null}
    </button>
  );
}

export const SessionCard = memo(SessionCardView);

