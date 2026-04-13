import { memo, useState } from "react";
import type { AttentionLevel, DashboardSession } from "../lib/types";
import { SessionCard } from "./SessionCard";

interface AttentionZoneProps {
  level: AttentionLevel;
  sessions: DashboardSession[];
  onSelect?: (session: DashboardSession) => void;
  onOpen?: (session: DashboardSession) => void;
  defaultCollapsed?: boolean;
}

const zoneConfig: Record<AttentionLevel, { label: string; emptyMessage: string }> = {
  merge: { label: "Ready", emptyMessage: "Nothing cleared to land yet." },
  respond: { label: "Respond", emptyMessage: "No agents need your input." },
  review: { label: "Review", emptyMessage: "No code waiting for review." },
  pending: { label: "Pending", emptyMessage: "Nothing blocked." },
  working: { label: "Working", emptyMessage: "No agents running." },
  done: { label: "Done", emptyMessage: "No completed sessions." },
};

function AttentionZoneView({ level, sessions, onSelect, onOpen, defaultCollapsed }: AttentionZoneProps) {
  const config = zoneConfig[level];
  const [collapsed, setCollapsed] = useState(Boolean(defaultCollapsed));
  return (
    <section className="panel">
      <button
        type="button"
        className="panel__title"
        onClick={() => setCollapsed((v) => !v)}
        style={{
          width: "100%",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 10,
          background: "transparent",
          border: "none",
          borderRadius: 0,
          cursor: "pointer",
        }}
        title={collapsed ? "Expand" : "Collapse"}
      >
        <span>
          {config.label} <span className="hint">({sessions.length})</span>
        </span>
        <span className="hint" style={{ fontWeight: 800 }}>
          {collapsed ? "+" : "–"}
        </span>
      </button>
      {collapsed ? null : (
        <div className="sessions">
          {sessions.length === 0 ? (
            <div className="hint">{config.emptyMessage}</div>
          ) : (
            sessions.map((s) => (
              <SessionCard key={s.id} session={s} onClick={onSelect} onOpen={onOpen} />
            ))
          )}
        </div>
      )}
    </section>
  );
}

export const AttentionZone = memo(AttentionZoneView);

