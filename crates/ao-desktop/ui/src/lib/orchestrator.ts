import type { DashboardOrchestrator, DashboardSession } from "./types";

/**
 * Derive the full list of managed project IDs for an orchestrator.
 * Today the backend only gives `primaryProjectId`; this function also folds in
 * the project IDs of all worker sessions so the sidebar is ready for the future
 * multi-project case without any API changes.
 */
export function deriveManagedProjectIds(
  orchestrator: DashboardOrchestrator,
  workers: DashboardSession[],
): string[] {
  const ids = new Set([orchestrator.primaryProjectId]);
  for (const w of workers) ids.add(w.projectId);
  return Array.from(ids);
}

/**
 * Partition sessions into two groups:
 * - `byOrchestrator`: workers keyed by the orchestrator id they were spawned by.
 *   Orchestrator sessions themselves are excluded from both groups.
 * - `unaffiliated`: sessions with no `spawnedBy` that are also not orchestrators.
 *
 * Source of truth: `worker.spawnedBy === orchestrator.id`.
 */
export function groupWorkersByOrchestrator(
  sessions: DashboardSession[],
  orchestrators: DashboardOrchestrator[],
): {
  byOrchestrator: Map<string, DashboardSession[]>;
  unaffiliated: DashboardSession[];
} {
  const orchIds = new Set(orchestrators.map((o) => o.id));
  const byOrchestrator = new Map<string, DashboardSession[]>(
    orchestrators.map((o) => [o.id, []]),
  );
  const unaffiliated: DashboardSession[] = [];

  for (const s of sessions) {
    if (orchIds.has(s.id)) continue; // orchestrator session itself — not a worker
    if (s.spawnedBy && byOrchestrator.has(s.spawnedBy)) {
      byOrchestrator.get(s.spawnedBy)!.push(s);
    } else {
      unaffiliated.push(s);
    }
  }

  return { byOrchestrator, unaffiliated };
}

/** True when a session was not spawned by any known orchestrator and is not itself an orchestrator. */
export function isUnaffiliated(
  session: DashboardSession,
  orchestrators: DashboardOrchestrator[],
): boolean {
  const orchIds = new Set(orchestrators.map((o) => o.id));
  return !session.spawnedBy && !orchIds.has(session.id);
}
