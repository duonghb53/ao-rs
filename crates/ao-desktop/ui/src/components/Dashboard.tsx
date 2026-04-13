import type { DashboardSession } from "../lib/types";
import { getAttentionLevel } from "../lib/types";
import { AttentionZone } from "./AttentionZone";

export function Dashboard({
  sessions,
  onSelect,
  onOpen,
}: {
  sessions: DashboardSession[];
  onSelect?: (session: DashboardSession) => void;
  onOpen?: (session: DashboardSession) => void;
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
      <AttentionZone level="merge" sessions={grouped.merge} onSelect={onSelect} onOpen={onOpen} />
      <AttentionZone level="respond" sessions={grouped.respond} onSelect={onSelect} onOpen={onOpen} />
      <AttentionZone level="review" sessions={grouped.review} onSelect={onSelect} onOpen={onOpen} />
      <AttentionZone level="pending" sessions={grouped.pending} onSelect={onSelect} onOpen={onOpen} />
      <AttentionZone level="working" sessions={grouped.working} onSelect={onSelect} onOpen={onOpen} />
      <AttentionZone level="done" sessions={grouped.done} onSelect={onSelect} onOpen={onOpen} />
    </div>
  );
}

