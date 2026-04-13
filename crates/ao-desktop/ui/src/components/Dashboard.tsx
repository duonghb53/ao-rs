import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";
import { AttentionZone } from "./AttentionZone";

export function Dashboard({
  sessions,
  onSelect,
}: {
  sessions: DashboardSession[];
  onSelect?: (session: DashboardSession) => void;
}) {
  const grouped: Record<string, DashboardSession[]> = {
    merge: [],
    respond: [],
    review: [],
    pending: [],
    working: [],
    done: [],
  };

  for (const s of sessions) {
    grouped[getAttentionLevel(s)].push(s);
  }

  return (
    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 12 }}>
      <AttentionZone level="merge" sessions={grouped.merge} onSelect={onSelect} />
      <AttentionZone level="respond" sessions={grouped.respond} onSelect={onSelect} />
      <AttentionZone level="review" sessions={grouped.review} onSelect={onSelect} />
      <AttentionZone level="pending" sessions={grouped.pending} onSelect={onSelect} />
      <AttentionZone level="working" sessions={grouped.working} onSelect={onSelect} />
      <AttentionZone level="done" sessions={grouped.done} onSelect={onSelect} />
    </div>
  );
}

