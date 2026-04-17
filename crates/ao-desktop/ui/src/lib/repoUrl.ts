import type { DashboardSession } from "./types";

function githubRepoFromIssueUrl(issueUrl: string): { owner: string; repo: string } | null {
  try {
    const u = new URL(issueUrl);
    if (u.hostname !== "github.com") return null;
    const parts = u.pathname.split("/").filter(Boolean);
    if (parts.length < 2) return null;
    const [owner, repo] = parts;
    if (!owner || !repo) return null;
    return { owner, repo };
  } catch {
    return null;
  }
}

export function getSessionRepoUrl(session: DashboardSession): string | null {
  const owner = session.pr?.owner;
  const repo = session.pr?.repo;
  if (owner && repo) return `https://github.com/${owner}/${repo}`;

  if (session.issueUrl) {
    const parsed = githubRepoFromIssueUrl(session.issueUrl);
    if (parsed) return `https://github.com/${parsed.owner}/${parsed.repo}`;
  }

  return null;
}

