import { memo } from "react";
import type { AttentionLevel, DashboardSession } from "../lib/types";
import { SessionCard } from "./SessionCard";

interface AttentionZoneProps {
  level: AttentionLevel;
  sessions: DashboardSession[];
  onSelect?: (session: DashboardSession) => void;
}

const zoneConfig: Record<AttentionLevel, { label: string; emptyMessage: string }> = {
  merge: { label: "Ready", emptyMessage: "Nothing cleared to land yet." },
  respond: { label: "Respond", emptyMessage: "No agents need your input." },
  review: { label: "Review", emptyMessage: "No code waiting for review." },
  pending: { label: "Pending", emptyMessage: "Nothing blocked." },
  working: { label: "Working", emptyMessage: "No agents running." },
  done: { label: "Done", emptyMessage: "No completed sessions." },
};

function AttentionZoneView({ level, sessions, onSelect }: AttentionZoneProps) {
  const config = zoneConfig[level];
  return (
    <section className="panel">
      <div className="panel__title">
        {config.label} <span className="hint">({sessions.length})</span>
      </div>
      <div className="sessions">
        {sessions.length === 0 ? (
          <div className="hint">{config.emptyMessage}</div>
        ) : (
          sessions.map((s) => <SessionCard key={s.id} session={s} onClick={onSelect} />)
        )}
      </div>
    </section>
  );
}

export const AttentionZone = memo(AttentionZoneView);

