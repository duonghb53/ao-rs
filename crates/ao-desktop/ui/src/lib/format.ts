import type { DashboardSession } from "./types";

export type PillTone = "ok" | "bad" | "neutral";

export interface PillFormat {
  label: string;
  tone: PillTone;
}

export function formatCiStatus(raw: string): PillFormat {
  switch (raw.toLowerCase()) {
    case "success":
    case "passing":
      return { label: "CI ✓", tone: "ok" };
    case "failure":
    case "failing":
    case "error":
      return { label: "CI ✗", tone: "bad" };
    case "pending":
    case "queued":
    case "running":
    case "in_progress":
      return { label: "CI …", tone: "neutral" };
    default:
      return { label: `CI ${raw}`, tone: "neutral" };
  }
}

export function formatReviewDecision(raw: string): PillFormat {
  switch (raw.toLowerCase()) {
    case "approved":
      return { label: "Approved", tone: "ok" };
    case "changes_requested":
      return { label: "Changes requested", tone: "bad" };
    case "review_required":
    case "pending":
    case "none":
      return { label: "Review required", tone: "neutral" };
    default:
      return { label: `Review ${raw}`, tone: "neutral" };
  }
}

export function humanizeBranch(branch: string): string {
  const withoutPrefix = branch.replace(
    /^(?:feat|fix|chore|refactor|docs|test|ci|session|release|hotfix|feature|bugfix|build|wip|improvement)\//,
    "",
  );
  return withoutPrefix
    .replace(/[-_]/g, " ")
    .replace(/\b\w/g, (c) => c.toUpperCase())
    .trim();
}

export function getSessionTabLabel(session: DashboardSession): string {
  const project = session.projectId || "project";
  const issueOrPr =
    (session.issueId ? String(session.issueId) : null) ??
    (session.pr?.number ? String(session.pr.number) : null) ??
    session.id.slice(-4);
  const status = session.status || "unknown";
  return `${project} - #${issueOrPr}: ${status}`;
}

export function getSessionTitle(session: DashboardSession): string {
  if (session.pr?.title) return session.pr.title;
  if (session.issueId && session.issueTitle) return `#${session.issueId} ${session.issueTitle}`;
  if (session.issueTitle) return session.issueTitle;
  if (session.userPrompt) return session.userPrompt;
  if (session.branch) return humanizeBranch(session.branch);
  const pinned = session.metadata["pinnedSummary"];
  if (pinned) return pinned;
  if (session.summary && !session.summaryIsFallback) return session.summary;
  if (session.summary) return session.summary;
  return session.status;
}

