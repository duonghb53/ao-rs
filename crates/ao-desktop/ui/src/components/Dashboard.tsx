import type { DashboardSession } from "../lib/types";
import { getDashboardLane } from "../lib/types";
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
  const grouped: Record<string, DashboardSession[]> = { working: [], pending: [], review: [], merge: [] };

  for (const s of sessions) {
    grouped[getDashboardLane(s)].push(s);
  }

  return (
    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 12 }}>
      <AttentionZone level="working" sessions={grouped.working} onSelect={onSelect} onOpen={onOpen} />
      <AttentionZone level="pending" sessions={grouped.pending} onSelect={onSelect} onOpen={onOpen} />
      <AttentionZone level="review" sessions={grouped.review} onSelect={onSelect} onOpen={onOpen} />
      <AttentionZone level="merge" sessions={grouped.merge} onSelect={onSelect} onOpen={onOpen} />
    </div>
  );
}

