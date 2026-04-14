import type { DashboardSession } from "./types";

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

