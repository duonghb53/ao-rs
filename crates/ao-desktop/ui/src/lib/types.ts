// Raw attention buckets as served by `ao-dashboard` (mirrors the TS dashboard).
// Note: UI may re-map these into fewer lanes.
export type AttentionLevel = "merge" | "respond" | "review" | "pending" | "working" | "done";

// Dashboard lanes (displayed columns/filters). Issue #59: simplify to 4 active
// lanes + a "killed" lane for terminal sessions that can be restored.
export type DashboardLane = "working" | "pending" | "review" | "merge" | "killed";

export type DashboardPR = {
  number: number;
  url: string;
  title: string;
  owner?: string;
  repo?: string;
  branch?: string;
  baseBranch?: string;
  isDraft?: boolean;
  state?: string;
  ciStatus?: string;
  reviewDecision?: string;
  mergeable?: boolean;
  blockers?: string[];
};

export type DashboardSession = {
  id: string;
  projectId: string;
  status: string;
  activity: string | null;
  /** Orchestrator agent id (e.g. claude-code, cursor) from spawn / session record. */
  agent: string | null;
  branch: string | null;
  summary: string | null;
  summaryIsFallback: boolean;
  issueTitle: string | null;
  issueId: string | null;
  issueUrl: string | null;
  userPrompt: string | null;
  pr: DashboardPR | null;
  /** PR number claimed by the session (persists after merge, even when `pr` enrichment returns null). */
  claimedPrNumber: number | null;
  claimedPrUrl: string | null;
  attentionLevel?: AttentionLevel | null;
  metadata: Record<string, string>;
  /** Session id of the orchestrator that spawned this session, if any. */
  spawnedBy: string | null;
  /** Unix timestamp (seconds) when the session was created, if known. */
  createdAt: number | null;
};

/**
 * Pure predicate mirroring `ao_core::orchestrator_spawn::is_orchestrator_session`
 * — true for session ids that end with `-orchestrator` or match
 * `<prefix>-orchestrator-<digits>`.
 */
export function isOrchestratorSessionId(id: string): boolean {
  if (id.endsWith("-orchestrator")) return true;
  const marker = "-orchestrator-";
  const pos = id.lastIndexOf(marker);
  if (pos < 0) return false;
  const suffix = id.slice(pos + marker.length);
  return suffix.length > 0 && /^\d+$/.test(suffix);
}

export const TERMINAL_STATUSES = new Set([
  "merged",
  "cleanup",
  "killed",
  "terminated",
  "done",
  "errored",
]);

export const TERMINAL_ACTIVITIES = new Set(["exited"]);

export function isTerminalSession(session: DashboardSession): boolean {
  const status = (session.status ?? "").toLowerCase();
  const activity = (session.activity ?? "").toLowerCase();
  return TERMINAL_STATUSES.has(status) || TERMINAL_ACTIVITIES.has(activity);
}

export function getAttentionLevel(session: DashboardSession): AttentionLevel {
  if (session.attentionLevel) return session.attentionLevel;
  const status = (session.status ?? "").toLowerCase();
  const activity = (session.activity ?? "").toLowerCase();

  if (TERMINAL_STATUSES.has(status) || TERMINAL_ACTIVITIES.has(activity)) return "done";
  if (status === "mergeable" || status === "approved") return "merge";
  if (status === "needs_input" || status === "stuck" || activity === "waiting_input" || activity === "blocked") {
    return "respond";
  }
  if (status === "ci_failed" || status === "changes_requested") return "review";
  // Align with ao-dashboard `attention_level` when PR data exists: open PR + pending
  // review → "review", not Backlog. Backlog ("pending") is for CI still running.
  if (status === "review_pending") return "review";
  if (status === "pr_open") {
    if (activity === "active") return "working";
    return "review";
  }
  return "working";
}

function laneFromLegacyAttention(session: DashboardSession, level: AttentionLevel): DashboardLane {
  if (level === "working" || level === "pending" || level === "review" || level === "merge") return level;

  // Legacy lanes folded into the simplified dashboard.
  const status = (session.status ?? "").toLowerCase();
  if (level === "respond") {
    if (status === "ci_failed" || status === "changes_requested") return "review";
    return "pending";
  }

  // level === "done" — terminal sessions go to the Killed lane (where they
  // can be restored), except merged which goes to Merge.
  if (status === "merged" || status === "cleanup" || status === "done") return "merge";
  return "killed";
}

export function getDashboardLane(session: DashboardSession): DashboardLane {
  // Source of truth is `ao-dashboard` attention_level when present; it already encodes PR + CI.
  if (session.attentionLevel) return laneFromLegacyAttention(session, session.attentionLevel);

  // Fall back to local derivation, then fold into display lanes.
  const level = getAttentionLevel(session);
  return laneFromLegacyAttention(session, level);
}

