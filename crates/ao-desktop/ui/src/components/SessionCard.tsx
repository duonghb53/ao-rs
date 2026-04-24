import { memo, useState, type CSSProperties, type KeyboardEvent, type MouseEvent } from "react";
import type { DashboardSession } from "../lib/types";
import { getDashboardLane, isTerminalSession } from "../lib/types";
import { formatCiStatus, formatReviewDecision, getSessionTitle } from "../lib/format";
import { cn } from "../lib/cn";
import { projectAccentStyle } from "../lib/projectColors";
import { ConfirmModal } from "./ConfirmModal";

interface SessionCardProps {
  session: DashboardSession;
  onClick?: (session: DashboardSession) => void;
  onOpen?: (session: DashboardSession) => void;
  onRestore?: (session: DashboardSession) => Promise<void>;
  onSendMessage?: (session: DashboardSession, message: string) => Promise<void>;
  onMerge?: (session: DashboardSession) => Promise<void> | void;
  onDelete?: (session: DashboardSession) => Promise<void> | void;
}

type CardTone = "working" | "pending" | "review" | "respond" | "merge" | "done";

function cardTone(session: DashboardSession): CardTone {
  if ((session.activity ?? "").toLowerCase() === "waiting_input") return "respond";
  return getDashboardLane(session) as CardTone;
}

function shortId(id: string): string {
  const trimmed = id.startsWith("ao-") ? id.slice(3) : id;
  return `ag-${trimmed.slice(0, 4)}`;
}

function linkKind(session: DashboardSession): { kind: "GH" | "LIN"; label: string; url: string } | null {
  const issueId = session.issueId ?? "";
  const issueUrl = session.issueUrl ?? "";

  if (issueId.toUpperCase().startsWith("LIN-") && issueUrl) {
    return { kind: "LIN", label: issueId, url: issueUrl };
  }
  if (issueId && issueUrl) {
    // PR number already shown as `#<pr>` chip; footer link reserved for issue.
    return { kind: "GH", label: `issue #${issueId}`, url: issueUrl };
  }
  return null;
}

function SessionCardView({ session, onClick, onOpen, onRestore, onSendMessage, onMerge, onDelete }: SessionCardProps) {
  const lane = getDashboardLane(session);
  const tone = cardTone(session);
  const title = getSessionTitle(session);
  const pr = session.pr;
  const terminal = isTerminalSession(session);
  const restorable = terminal && (session.status ?? "").toLowerCase() !== "merged";
  const [restoring, setRestoring] = useState(false);
  const [respondReply, setRespondReply] = useState("");
  const [sending, setSending] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [confirmDeleteOpen, setConfirmDeleteOpen] = useState(false);
  const [merging, setMerging] = useState(false);
  const projectAccent = projectAccentStyle(session.projectId);

  const ci = pr?.ciStatus ? formatCiStatus(pr.ciStatus) : null;
  const review = pr?.reviewDecision ? formatReviewDecision(pr.reviewDecision) : null;
  const ciFailing = ci?.tone === "bad";
  const changesRequested = review?.tone === "bad";
  const needsAttention = lane === "review" && !ciFailing && !changesRequested;
  const waitingInput = tone === "respond";
  const canMerge = lane === "merge";
  const link = linkKind(session);

  const handleCardClick = () => onClick?.(session);
  const stopAndRun = (fn: () => void) => (event: MouseEvent | KeyboardEvent) => {
    event.preventDefault();
    event.stopPropagation();
    fn();
  };

  const doDelete = async () => {
    if (deleting || !onDelete) return;
    setDeleting(true);
    setConfirmDeleteOpen(false);
    try {
      await Promise.resolve(onDelete(session));
    } finally {
      setDeleting(false);
    }
  };

  const askToFix = async (kind: "ci" | "changes" | "attention") => {
    if (!onSendMessage) {
      onOpen?.(session);
      return;
    }
    const message =
      kind === "ci"
        ? "Please fix the failing CI checks on this PR and push an update."
        : kind === "changes"
          ? "Please address the requested review changes on this PR and push an update."
          : "Please review the PR status, unblock whatever is pending, and proceed.";
    try {
      await onSendMessage(session, message);
    } finally {
      onOpen?.(session);
    }
  };

  const respond = async (message: string) => {
    if (!onSendMessage || sending) return;
    setSending(true);
    try {
      await onSendMessage(session, message);
      if (message === respondReply) setRespondReply("");
    } finally {
      setSending(false);
    }
  };

  const doMerge = async () => {
    if (merging) return;
    setMerging(true);
    try {
      if (onMerge) {
        await Promise.resolve(onMerge(session));
      } else {
        onOpen?.(session);
      }
    } finally {
      setMerging(false);
    }
  };

  return (
    <>
      <ConfirmModal
        open={confirmDeleteOpen}
        title="Delete session?"
        message="This will kill the running agent session. You can restore it later from the Killed lane."
        confirmText={deleting ? "Deleting…" : "Delete"}
        cancelText="Cancel"
        danger={true}
        onCancel={() => setConfirmDeleteOpen(false)}
        onConfirm={() => void doDelete()}
      />
      <div
        className={cn("card")}
        data-tone={tone}
        data-level={lane}
        role="button"
        tabIndex={0}
        onClick={handleCardClick}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            handleCardClick();
          }
        }}
        style={projectAccent as CSSProperties}
        title={`${session.projectId} · ${session.status}${session.activity ? ` / ${session.activity}` : ""}`}
      >
      <div className="card__head">
        <span className="card__id" title={session.id}>
          {session.projectId}
        </span>
        <div className="card__head-right">
          {onOpen ? (
            <span
              role="button"
              tabIndex={0}
              className="btn"
              onClick={stopAndRun(() => onOpen(session))}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") stopAndRun(() => onOpen(session))(e);
              }}
            >
              terminal
            </span>
          ) : null}
        </div>
      </div>

      <div className="card__title">{title}</div>

      {session.branch || pr || session.claimedPrNumber ? (
        <div className="card__branch">
          {session.branch ? <span className="br">{session.branch}</span> : null}
          {(() => {
            const prNumber = pr?.number ?? session.claimedPrNumber ?? null;
            const prUrl = pr?.url ?? session.claimedPrUrl ?? null;
            if (!prNumber) return null;
            if (prUrl) {
              return (
                <a
                  className="pr"
                  href={prUrl}
                  target="_blank"
                  rel="noreferrer"
                  onClick={(e) => e.stopPropagation()}
                  onKeyDown={(e) => e.stopPropagation()}
                  title={prUrl}
                >
                  #{prNumber}
                </a>
              );
            }
            return <span className="pr">#{prNumber}</span>;
          })()}
          {typeof pr?.additions === "number" || typeof pr?.deletions === "number" ? (
            <span className="pr-diff" title="PR diff size">
              <span className="plus">+{pr?.additions ?? 0}</span>{" "}
              <span className="minus">-{pr?.deletions ?? 0}</span>
            </span>
          ) : null}
          {session.agent ? <span className="agent">{session.agent}</span> : null}
        </div>
      ) : null}

      {ciFailing ? (
        <div className="card__alert">
          <span
            className="ci-fail"
            role="button"
            tabIndex={0}
            onClick={stopAndRun(() => onOpen?.(session))}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") stopAndRun(() => onOpen?.(session))(e);
            }}
          >
            {(() => {
              const n = pr?.failingChecks;
              if (typeof n === "number" && n > 0) return `${n} CI checks failing`;
              return ci?.label ?? "CI failing";
            })()}
          </span>
          {onOpen ? (
            <button type="button" className="ask-fix" onClick={stopAndRun(() => void askToFix("ci"))}>
              Ask to fix
            </button>
          ) : null}
          {pr?.failingCheckNames && pr.failingCheckNames.length > 0 ? (
            <div className="ci-checks">
              {pr.failingCheckNames.slice(0, 5).map((name) => (
                <div key={name} className="ci-checks__row">
                  <span className="ci-checks__dot" aria-hidden="true">
                    ×
                  </span>
                  <span className="ci-checks__name">{name}</span>
                </div>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}

      {changesRequested ? (
        <div className="card__alert">
          <span className="chg-req">changes requested</span>
          {onOpen ? (
            <button type="button" className="ask-fix" onClick={stopAndRun(() => void askToFix("changes"))}>
              Ask to fix
            </button>
          ) : null}
        </div>
      ) : null}

      {needsAttention ? (
        <div className="card__alert">
          <span className="ci-fail">needs attention</span>
          {onOpen ? (
            <button type="button" className="ask-fix" onClick={stopAndRun(() => void askToFix("attention"))}>
              Ask to fix
            </button>
          ) : null}
        </div>
      ) : null}

      {waitingInput ? (
        <div className="respond" onClick={(e) => e.stopPropagation()}>
          <div>{session.userPrompt ?? "Waiting for your input."}</div>
          <div className="respond__actions">
            <button
              type="button"
              className="btn"
              disabled={!onSendMessage || sending}
              onClick={stopAndRun(() => void respond("continue"))}
            >
              Continue
            </button>
            <button
              type="button"
              className="btn"
              disabled={!onSendMessage || sending}
              onClick={stopAndRun(() => void respond("abort"))}
            >
              Abort
            </button>
            <button
              type="button"
              className="btn"
              disabled={!onSendMessage || sending}
              onClick={stopAndRun(() => void respond("skip"))}
            >
              Skip
            </button>
          </div>
          <input
            className="respond__reply"
            placeholder="Type a reply..."
            value={respondReply}
            onChange={(e) => setRespondReply(e.target.value)}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter" && respondReply.trim()) {
                e.preventDefault();
                void respond(respondReply.trim());
              }
            }}
          />
        </div>
      ) : null}

      {canMerge ? (
        <button
          type="button"
          className="merge-btn"
          disabled={merging}
          aria-busy={merging ? "true" : "false"}
          onClick={stopAndRun(() => void doMerge())}
        >
          {merging ? "⇡  merging…" : "\u21e1  merge"}
        </button>
      ) : null}

      {!session.branch && (session.activity ?? "").toLowerCase() === "active" ? (
        <div className="card__meta">active · {session.status ?? "-"}</div>
      ) : null}

      <div className="card__foot">
        {link ? (
          <a
            className="card__link"
            data-kind={link.kind}
            href={link.url}
            target="_blank"
            rel="noreferrer"
            onClick={(e) => e.stopPropagation()}
          >
            {link.label}
          </a>
        ) : (
          <span />
        )}
        <span className="card__foot-actions">
          {restorable && onRestore ? (
            <span
              role="button"
              tabIndex={0}
              className="btn"
              onClick={stopAndRun(() => {
                if (restoring) return;
                setRestoring(true);
                onRestore(session).finally(() => setRestoring(false));
              })}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  stopAndRun(() => {
                    if (restoring) return;
                    setRestoring(true);
                    onRestore(session).finally(() => setRestoring(false));
                  })(e);
                }
              }}
            >
              {restoring ? "restoring…" : "restore"}
            </span>
          ) : null}
          <span
            role="button"
            tabIndex={0}
            className="btn btn--icon btn--danger"
            title="delete"
            aria-label="Delete session"
            aria-busy={deleting ? "true" : "false"}
            onClick={stopAndRun(() => {
              if (deleting || !onDelete) return;
              setConfirmDeleteOpen(true);
            })}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                stopAndRun(() => {
                  if (deleting || !onDelete) return;
                  setConfirmDeleteOpen(true);
                })(e);
              }
            }}
          >
            {"\uD83D\uDDD1"}
          </span>
        </span>
      </div>
      </div>
    </>
  );
}

export const SessionCard = memo(SessionCardView);
