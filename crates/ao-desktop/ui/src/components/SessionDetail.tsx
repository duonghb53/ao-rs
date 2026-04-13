import { useMemo, useState } from "react";
import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";
import { getSessionTitle } from "../lib/format";

export function SessionDetail({
  session,
  onSendMessage,
  onKill,
}: {
  session: DashboardSession;
  onSendMessage: (message: string) => Promise<void>;
  onKill: () => Promise<void>;
}) {
  const level = getAttentionLevel(session);
  const title = getSessionTitle(session);
  const [message, setMessage] = useState("");
  const [sending, setSending] = useState(false);
  const [killing, setKilling] = useState(false);
  const [status, setStatus] = useState<string>("");

  const pills = useMemo(() => {
    const items: Array<{ label: string; tone?: "ok" | "bad" }> = [];
    items.push({ label: `level: ${level}` });
    if (session.activity) items.push({ label: `activity: ${session.activity}` });
    items.push({ label: `status: ${session.status}` });
    return items;
  }, [level, session.activity, session.status]);

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

  return (
    <div style={{ display: "grid", gap: 10 }}>
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
          <button onClick={kill} disabled={killing} style={{ borderColor: "rgba(220,38,38,0.35)" }}>
            Kill
          </button>
          <span className="hint">{status}</span>
        </div>
      </div>
    </div>
  );
}

