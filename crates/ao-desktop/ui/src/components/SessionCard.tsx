import { memo, useState } from "react";
import type { DashboardSession } from "../lib/types";
import { getDashboardLane, isTerminalSession } from "../lib/types";
import { formatCiStatus, formatReviewDecision, getSessionTitle } from "../lib/format";
import { cn } from "../lib/cn";
import { projectAccentStyle } from "../lib/projectColors";
import { getSessionRepoUrl } from "../lib/repoUrl";

interface SessionCardProps {
  session: DashboardSession;
  onClick?: (session: DashboardSession) => void;
  onOpen?: (session: DashboardSession) => void;
  onRestore?: (session: DashboardSession) => Promise<void>;
}

function SessionCardView({ session, onClick, onOpen, onRestore }: SessionCardProps) {
  const level = getDashboardLane(session);
  const title = getSessionTitle(session);
  const secondary =
    session.branch ? session.branch : session.summary && session.summary !== title ? session.summary : null;
  const pr = session.pr;
  const issueUrl = session.issueUrl;
  const issueId = session.issueId;
  const terminal = isTerminalSession(session);
  const restorable = terminal && (session.status ?? "").toLowerCase() !== "merged";
  const [restoring, setRestoring] = useState(false);
  const projectAccent = projectAccentStyle(session.projectId);
  const repoUrl = getSessionRepoUrl(session);
  const ci = pr?.ciStatus ? formatCiStatus(pr.ciStatus) : null;
  const review = pr?.reviewDecision ? formatReviewDecision(pr.reviewDecision) : null;

  return (
    <button
      type="button"
      className={cn("session-card", "w-full text-left")}
      onClick={() => onClick?.(session)}
      data-level={level}
    >
      <div className="session-card__strip" />
      <div className="session-card__top">
        <div className="session-card__id" title={session.projectId}>
          {session.projectId}
        </div>
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
            <a
              className="issue-link"
              href={issueUrl}
              target="_blank"
              rel="noreferrer"
              title={issueUrl}
              onClick={(e) => e.stopPropagation()}
              onKeyDown={(e) => e.stopPropagation()}
            >
              #{issueId}
            </a>{" "}
            <span>{session.issueTitle ?? title}</span>
          </>
        ) : (
          title
        )}
      </div>
      {secondary ? <div className="session-card__sub">{secondary}</div> : null}
      <div className="session-card__pills">
        {repoUrl ? (
          <span
            className="mini-pill"
            data-project-accent="true"
            style={{ ...projectAccent, cursor: "pointer", userSelect: "none" }}
            title={repoUrl}
            role="link"
            tabIndex={0}
            onClick={(e) => {
              e.stopPropagation();
              window.open(repoUrl, "_blank", "noopener,noreferrer");
            }}
            onMouseDown={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                window.open(repoUrl, "_blank", "noopener,noreferrer");
              }
            }}
          >
            project: {session.projectId}
          </span>
        ) : (
          <span className="mini-pill" data-project-accent="true" style={projectAccent}>
            project: {session.projectId}
          </span>
        )}
        {session.branch ? (
          <span className="mini-pill" data-project-accent="true" style={projectAccent} title={session.branch}>
            branch: {session.branch}
          </span>
        ) : null}
        {session.agent ? (
          <span className="mini-pill" title={`Agent: ${session.agent}`}>
            agent: {session.agent}
          </span>
        ) : null}
        {pr ? (
          <>
            <span className="mini-pill">PR #{pr.number}</span>
            {ci ? (
              <span className="mini-pill" data-tone={ci.tone}>
                {ci.label}
              </span>
            ) : null}
            {review ? (
              <span className="mini-pill" data-tone={review.tone}>
                {review.label}
              </span>
            ) : null}
            {typeof pr.mergeable === "boolean" ? (
              <span className="mini-pill">{pr.mergeable ? "mergeable" : "not mergeable"}</span>
            ) : null}
            {pr.blockers && pr.blockers.length > 0 ? (
              <span className="mini-pill">blockers: {pr.blockers.length}</span>
            ) : null}
          </>
        ) : null}
      </div>
      {restorable && onRestore ? (
        <div className="session-card__actions" style={{ marginTop: 6 }}>
          <span
            role="button"
            tabIndex={0}
            className="mini-pill mini-pill--restore"
            style={{ cursor: "pointer", userSelect: "none" }}
            title="Restore this session"
            onClick={(e) => {
              e.preventDefault();
              e.stopPropagation();
              if (restoring) return;
              setRestoring(true);
              onRestore(session).finally(() => setRestoring(false));
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                e.stopPropagation();
                if (restoring) return;
                setRestoring(true);
                onRestore(session).finally(() => setRestoring(false));
              }
            }}
          >
            {restoring ? "restoring…" : "restore"}
          </span>
        </div>
      ) : null}
    </button>
  );
}

export const SessionCard = memo(SessionCardView);

