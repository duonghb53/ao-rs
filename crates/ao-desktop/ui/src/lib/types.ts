export type AttentionLevel = "merge" | "respond" | "review" | "pending" | "working" | "done";

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
  branch: string | null;
  summary: string | null;
  summaryIsFallback: boolean;
  issueTitle: string | null;
  userPrompt: string | null;
  pr: DashboardPR | null;
  attentionLevel?: AttentionLevel | null;
  metadata: Record<string, string>;
};

export const TERMINAL_STATUSES = new Set([
  "merged",
  "cleanup",
  "killed",
  "terminated",
  "done",
  "errored",
]);

export const TERMINAL_ACTIVITIES = new Set(["exited"]);

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

