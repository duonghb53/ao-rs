import type { ApiEvent } from "../api/client";
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

function str(rec: Record<string, unknown>, key: string): string | null {
  const v = rec[key];
  return typeof v === "string" ? v : null;
}

function num(rec: Record<string, unknown>, key: string): number | null {
  const v = rec[key];
  return typeof v === "number" ? v : null;
}

export function formatEvent(evt: ApiEvent): string {
  const rec = evt as unknown as Record<string, unknown>;
  const fallback = () => JSON.stringify(evt);

  switch (evt.type) {
    case "snapshot": {
      const sessions = rec.sessions;
      const count = Array.isArray(sessions) ? sessions.length : 0;
      return `snapshot · ${count} session${count === 1 ? "" : "s"}`;
    }
    case "spawned": {
      const project = str(rec, "project_id");
      return project ? `spawned in ${project}` : "spawned";
    }
    case "session_restored": {
      const status = str(rec, "status");
      const project = str(rec, "project_id");
      const parts = ["restored", status, project].filter(Boolean) as string[];
      return parts.length > 1 ? parts.join(" · ") : fallback();
    }
    case "status_changed": {
      const from = str(rec, "from");
      const to = str(rec, "to");
      return from && to ? `${from} → ${to}` : fallback();
    }
    case "activity_changed": {
      const next = str(rec, "next");
      if (!next) return fallback();
      const prev = str(rec, "prev") ?? "∅";
      return `${prev} → ${next}`;
    }
    case "terminated": {
      const reason = str(rec, "reason");
      return reason ? `terminated · ${reason}` : "terminated";
    }
    case "tick_error": {
      const message = str(rec, "message");
      return message ? `tick error · ${message}` : "tick error";
    }
    case "reaction_triggered": {
      const key = str(rec, "reaction_key");
      const action = str(rec, "action");
      return key && action ? `reaction · ${key} → ${action}` : fallback();
    }
    case "reaction_escalated": {
      const key = str(rec, "reaction_key");
      const attempts = num(rec, "attempts");
      return key && attempts !== null
        ? `escalated · ${key} (attempts: ${attempts})`
        : fallback();
    }
    case "ui_notification": {
      const n = rec.notification;
      if (!n || typeof n !== "object") return fallback();
      const nr = n as Record<string, unknown>;
      const key = str(nr, "reaction_key");
      const action = str(nr, "action");
      if (!key || !action) return fallback();
      const message = str(nr, "message");
      return message
        ? `notify · ${key} → ${action} · ${message}`
        : `notify · ${key} → ${action}`;
    }
    default:
      return fallback();
  }
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

