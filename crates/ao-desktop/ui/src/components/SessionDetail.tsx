import { useMemo, useState } from "react";
import type { DashboardSession } from "../lib/types";
import { getAttentionLevel, TERMINAL_STATUSES } from "../lib/types";
import { getSessionTitle } from "../lib/format";
import { ConfirmModal } from "./ConfirmModal";

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
    <div style={{ display: "grid", gap: 10 }}>
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
      <div style={{ display: "flex", gap: 10, alignItems: "baseline", flexWrap: "wrap" }}>
        <div style={{ fontWeight: 800 }}>{title}</div>
        <div className="hint" style={{ fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, Liberation Mono, monospace" }}>
          {session.id}
        </div>
      </div>

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        {pills.map((p) => (
          <span key={p.label} className="mini-pill">
            {p.label}
          </span>
        ))}
        {session.branch ? <span className="mini-pill">branch: {session.branch}</span> : null}
        {session.projectId ? <span className="mini-pill">project: {session.projectId}</span> : null}
      </div>

      <div style={{ display: "grid", gap: 8 }}>
        <div className="hint">Pull Request</div>
        {session.pr ? (
          <>
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
              <span className="mini-pill">#{session.pr.number}</span>
              {session.pr.title ? (
                <span className="hint" style={{ flex: "1 1 200px" }}>
                  {session.pr.title}
                </span>
              ) : null}
              {session.pr.ciStatus ? <span className="mini-pill">CI: {session.pr.ciStatus}</span> : null}
              {session.pr.reviewDecision ? <span className="mini-pill">Review: {session.pr.reviewDecision}</span> : null}
              {typeof session.pr.mergeable === "boolean" ? (
                <span className="mini-pill">{session.pr.mergeable ? "mergeable" : "not mergeable"}</span>
              ) : null}
              <a className="mini-pill" href={session.pr.url} target="_blank" rel="noreferrer">
                Open PR
              </a>
            </div>
            {session.pr.blockers && session.pr.blockers.length > 0 ? (
              <div className="hint">Blockers: {session.pr.blockers.join(" · ")}</div>
            ) : null}
          </>
        ) : (
          <div className="hint">No PR linked for this session (load sessions with PR enrichment from ao-dashboard).</div>
        )}
      </div>

      <div style={{ display: "grid", gap: 8 }}>
        <div className="hint">Send message</div>
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
      </div>
    </div>
  );
}

