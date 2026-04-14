import { useMemo, useState } from "react";
import type { DashboardSession } from "../lib/types";
import { getAttentionLevel, TERMINAL_STATUSES } from "../lib/types";
import { getSessionTitle } from "../lib/format";
import { ConfirmModal } from "./ConfirmModal";

function IssueLink({ id, url }: { id: string; url: string }) {
  return (
    <a className="issue-link" href={url} target="_blank" rel="noreferrer" title={url}>
      #{id}
    </a>
  );
}

export function SessionDetail({
  session,
  onSendMessage,
  onKill,
  onRestore,
}: {
  session: DashboardSession;
  onSendMessage: (message: string) => Promise<void>;
  onKill: () => Promise<void>;
  onRestore: () => Promise<void>;
}) {
  const level = getAttentionLevel(session);
  const title = getSessionTitle(session);
  const [message, setMessage] = useState("");
  const [sending, setSending] = useState(false);
  const [killing, setKilling] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [status, setStatus] = useState<string>("");
  const [confirmKillOpen, setConfirmKillOpen] = useState(false);
  const [confirmRestoreOpen, setConfirmRestoreOpen] = useState(false);

  const isKillable = useMemo(() => {
    const s = (session.status ?? "").toLowerCase();
    return !TERMINAL_STATUSES.has(s);
  }, [session.status]);

  const pills = useMemo(() => {
    const items: Array<{ label: string; tone?: "ok" | "bad" }> = [];
    items.push({ label: `level: ${level}` });
    if (session.activity) items.push({ label: `activity: ${session.activity}` });
    items.push({ label: `status: ${session.status}` });
    return items;
  }, [level, session.activity, session.status]);

  const isRestorable = useMemo(() => {
    const s = (session.status ?? "").toLowerCase();
    return TERMINAL_STATUSES.has(s) && s !== "merged";
  }, [session.status]);

  const send = async () => {
    const trimmed = message.trim();
    if (!trimmed || sending) return;
    setSending(true);
    setStatus("sending…");
    try {
      await onSendMessage(trimmed);
      setMessage("");
      setStatus("sent");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : "send failed");
    } finally {
      setSending(false);
      setTimeout(() => setStatus(""), 1500);
    }
  };

  const kill = async () => {
    if (killing) return;
    if (!isKillable) return;
    setConfirmKillOpen(false);
    setKilling(true);
    setStatus("killing…");
    try {
      await onKill();
      setStatus("killed");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : "kill failed");
    } finally {
      setKilling(false);
      setTimeout(() => setStatus(""), 1500);
    }
  };

  const restore = async () => {
    if (restoring) return;
    if (!isRestorable) return;
    setConfirmRestoreOpen(false);
    setRestoring(true);
    setStatus("restoring…");
    try {
      await onRestore();
      setStatus("restored");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : "restore failed");
    } finally {
      setRestoring(false);
      setTimeout(() => setStatus(""), 1500);
    }
  };

  return (
    <div className="detail" style={{ display: "grid", gap: 12 }}>
      <ConfirmModal
        open={confirmKillOpen}
        title="Kill session"
        message={`Kill session ${session.id.slice(0, 8)}?\n\nThis stops the runtime for this session.`}
        confirmText="Kill"
        danger
        onCancel={() => setConfirmKillOpen(false)}
        onConfirm={() => void kill()}
      />
      <ConfirmModal
        open={confirmRestoreOpen}
        title="Restore session"
        message={`Restore session ${session.id.slice(0, 8)}?\n\nThis will re-spawn the runtime for this session.`}
        confirmText="Restore"
        onCancel={() => setConfirmRestoreOpen(false)}
        onConfirm={() => void restore()}
      />
      <section className="detail-hero">
        <div className="detail-hero__top">
          <div className="detail-hero__title">
            {session.issueId && session.issueUrl && session.issueTitle ? (
              <>
                <IssueLink id={session.issueId} url={session.issueUrl} /> {session.issueTitle}
              </>
            ) : (
              title
            )}
          </div>
          <span className="mini-pill detail-hero__status" data-tone={level}>
            {level}
          </span>
        </div>
        <div className="detail-hero__sub">
          <span className="mono" title={session.id}>
            {session.id.slice(0, 8)}
          </span>
        </div>
        <div className="detail-tags">
          {session.projectId ? <span className="mini-pill">project: {session.projectId}</span> : null}
          {session.branch ? <span className="mini-pill">branch: {session.branch}</span> : null}
          {session.pr ? <span className="mini-pill">PR #{session.pr.number}</span> : null}
          <span className="mini-pill">status: {session.status}</span>
          {session.activity ? <span className="mini-pill">activity: {session.activity}</span> : null}
        </div>
        <div className="detail-meta">
          <div className="kv">
            <div className="kv__k">Level</div>
            <div className="kv__v">{level}</div>
          </div>
          <div className="kv">
            <div className="kv__k">Status</div>
            <div className="kv__v">{session.status ?? "-"}</div>
          </div>
          <div className="kv">
            <div className="kv__k">Activity</div>
            <div className="kv__v">{session.activity ?? "-"}</div>
          </div>
        </div>
      </section>

      <section className="detail-card">
        <div className="detail-card__title">Pull Request</div>
        {session.pr ? (
          <>
            <div className="pr-head">
              <a className="pr-head__title" href={session.pr.url} target="_blank" rel="noreferrer">
                PR #{session.pr.number}{session.pr.title ? `: ${session.pr.title}` : ""}
              </a>
              <div className="pr-head__pills">
                {session.pr.ciStatus ? <span className="mini-pill">CI {session.pr.ciStatus}</span> : null}
                {session.pr.reviewDecision ? <span className="mini-pill">Review {session.pr.reviewDecision}</span> : null}
                {typeof session.pr.mergeable === "boolean" ? (
                  <span className="mini-pill">{session.pr.mergeable ? "mergeable" : "not mergeable"}</span>
                ) : null}
                <a className="mini-pill" href={session.pr.url} target="_blank" rel="noreferrer">
                  Open
                </a>
              </div>
            </div>
            {session.pr.blockers && session.pr.blockers.length > 0 ? (
              <div className="pr-blockers">
                <div className="pr-blockers__title">Blockers</div>
                <ul className="pr-blockers__list">
                  {session.pr.blockers.map((b) => (
                    <li key={b}>{b}</li>
                  ))}
                </ul>
              </div>
            ) : null}
          </>
        ) : (
          <div className="hint">No PR linked for this session (load sessions with PR enrichment from ao-dashboard).</div>
        )}
      </section>

      <section className="detail-card">
        <div className="detail-card__title">Send message</div>
        <textarea
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          placeholder="Type a message to the agent…"
          style={{ width: "100%" }}
        />
        <div className="row">
          <button className="primary" onClick={send} disabled={sending || !message.trim()}>
            Send
          </button>
          <button
            onClick={() => setConfirmKillOpen(true)}
            disabled={!isKillable || killing}
            title={isKillable ? "Kill session runtime" : "Session is already terminal"}
            style={{ borderColor: "rgba(220,38,38,0.35)" }}
          >
            Kill
          </button>
          <button
            onClick={() => setConfirmRestoreOpen(true)}
            disabled={!isRestorable || restoring}
            title={isRestorable ? "Restore session runtime" : "Only terminal sessions can be restored"}
          >
            Restore
          </button>
          <span className="hint">{status}</span>
        </div>
      </section>
    </div>
  );
}

