import { describe, expect, it } from "vitest";

import type { ApiEvent } from "../api/client";
import { formatCiStatus, formatEvent, formatReviewDecision, getSessionTabLabel } from "./format";
import type { DashboardSession } from "./types";

describe("getSessionTabLabel", () => {
  it("formats `{project} - #{issue}: {status}` when issueId present", () => {
    const s: DashboardSession = {
      id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
      projectId: "ao-rs",
      status: "working",
      activity: null,
      agent: null,
      branch: null,
      summary: null,
      summaryIsFallback: false,
      issueTitle: null,
      issueId: "70",
      issueUrl: null,
      userPrompt: null,
      pr: null,
      attentionLevel: null,
      metadata: {},
    };

    expect(getSessionTabLabel(s)).toBe("ao-rs - #70: working");
  });
});

describe("formatCiStatus", () => {
  it("maps success to ok tone with check glyph", () => {
    expect(formatCiStatus("SUCCESS")).toEqual({ label: "CI ✓", tone: "ok" });
    expect(formatCiStatus("passing")).toEqual({ label: "CI ✓", tone: "ok" });
  });

  it("maps failure to bad tone with cross glyph", () => {
    expect(formatCiStatus("failure")).toEqual({ label: "CI ✗", tone: "bad" });
    expect(formatCiStatus("FAILING")).toEqual({ label: "CI ✗", tone: "bad" });
    expect(formatCiStatus("error")).toEqual({ label: "CI ✗", tone: "bad" });
  });

  it("maps in-flight states to neutral tone with ellipsis", () => {
    expect(formatCiStatus("pending")).toEqual({ label: "CI …", tone: "neutral" });
    expect(formatCiStatus("queued")).toEqual({ label: "CI …", tone: "neutral" });
    expect(formatCiStatus("RUNNING")).toEqual({ label: "CI …", tone: "neutral" });
    expect(formatCiStatus("in_progress")).toEqual({ label: "CI …", tone: "neutral" });
  });

  it("falls back to neutral tone with raw label for unknown states", () => {
    expect(formatCiStatus("stale")).toEqual({ label: "CI stale", tone: "neutral" });
  });
});

describe("formatReviewDecision", () => {
  it("maps approved to ok tone", () => {
    expect(formatReviewDecision("APPROVED")).toEqual({ label: "Approved", tone: "ok" });
  });

  it("maps changes_requested to bad tone", () => {
    expect(formatReviewDecision("CHANGES_REQUESTED")).toEqual({
      label: "Changes requested",
      tone: "bad",
    });
  });

  it("maps review_required / pending / none to neutral tone", () => {
    expect(formatReviewDecision("REVIEW_REQUIRED")).toEqual({
      label: "Review required",
      tone: "neutral",
    });
    expect(formatReviewDecision("pending")).toEqual({
      label: "Review required",
      tone: "neutral",
    });
    expect(formatReviewDecision("none")).toEqual({
      label: "Review required",
      tone: "neutral",
    });
  });

  it("falls back to neutral tone with raw label for unknown states", () => {
    expect(formatReviewDecision("dismissed")).toEqual({
      label: "Review dismissed",
      tone: "neutral",
    });
  });
});

describe("formatEvent", () => {
  it("summarises snapshot with session count (plural)", () => {
    const evt = { type: "snapshot", sessions: [{}, {}, {}] } as unknown as ApiEvent;
    expect(formatEvent(evt)).toBe("snapshot · 3 sessions");
  });

  it("summarises snapshot with session count (singular)", () => {
    const evt = { type: "snapshot", sessions: [{}] } as unknown as ApiEvent;
    expect(formatEvent(evt)).toBe("snapshot · 1 session");
  });

  it("summarises snapshot with zero sessions", () => {
    const evt = { type: "snapshot", sessions: [] } as unknown as ApiEvent;
    expect(formatEvent(evt)).toBe("snapshot · 0 sessions");
  });

  it("summarises spawned with project_id", () => {
    const evt = { type: "spawned", id: "s1", project_id: "ao-rs" } as ApiEvent;
    expect(formatEvent(evt)).toBe("spawned in ao-rs");
  });

  it("summarises spawned without project_id", () => {
    const evt = { type: "spawned", id: "s1" } as ApiEvent;
    expect(formatEvent(evt)).toBe("spawned");
  });

  it("summarises session_restored with status and project", () => {
    const evt = {
      type: "session_restored",
      id: "s1",
      project_id: "ao-rs",
      status: "working",
    } as ApiEvent;
    expect(formatEvent(evt)).toBe("restored · working · ao-rs");
  });

  it("summarises status_changed with from → to", () => {
    const evt = { type: "status_changed", id: "s1", from: "working", to: "pr_open" } as ApiEvent;
    expect(formatEvent(evt)).toBe("working → pr_open");
  });

  it("falls back when status_changed is missing to", () => {
    const evt = { type: "status_changed", id: "s1", from: "working" } as ApiEvent;
    expect(formatEvent(evt)).toBe(JSON.stringify(evt));
  });

  it("summarises activity_changed with prev → next", () => {
    const evt = { type: "activity_changed", id: "s1", prev: "idle", next: "active" } as ApiEvent;
    expect(formatEvent(evt)).toBe("idle → active");
  });

  it("summarises activity_changed with null prev using ∅", () => {
    const evt = { type: "activity_changed", id: "s1", prev: null, next: "active" } as ApiEvent;
    expect(formatEvent(evt)).toBe("∅ → active");
  });

  it("summarises terminated with reason", () => {
    const evt = { type: "terminated", id: "s1", reason: "runtime_gone" } as ApiEvent;
    expect(formatEvent(evt)).toBe("terminated · runtime_gone");
  });

  it("summarises tick_error with message", () => {
    const evt = { type: "tick_error", id: "s1", message: "poll failed" } as ApiEvent;
    expect(formatEvent(evt)).toBe("tick error · poll failed");
  });

  it("summarises reaction_triggered with key and action", () => {
    const evt = {
      type: "reaction_triggered",
      id: "s1",
      reaction_key: "stale-pr",
      action: "notify",
    } as ApiEvent;
    expect(formatEvent(evt)).toBe("reaction · stale-pr → notify");
  });

  it("summarises reaction_escalated with key and attempts", () => {
    const evt = {
      type: "reaction_escalated",
      id: "s1",
      reaction_key: "stale-pr",
      attempts: 3,
    } as ApiEvent;
    expect(formatEvent(evt)).toBe("escalated · stale-pr (attempts: 3)");
  });

  it("summarises ui_notification with message", () => {
    const evt = {
      type: "ui_notification",
      notification: {
        id: "s1",
        reaction_key: "stale-pr",
        action: "notify",
        message: "PR has been idle for 24h",
      },
    } as unknown as ApiEvent;
    expect(formatEvent(evt)).toBe("notify · stale-pr → notify · PR has been idle for 24h");
  });

  it("summarises ui_notification without message", () => {
    const evt = {
      type: "ui_notification",
      notification: { id: "s1", reaction_key: "stale-pr", action: "notify" },
    } as unknown as ApiEvent;
    expect(formatEvent(evt)).toBe("notify · stale-pr → notify");
  });

  it("falls back to JSON for unknown event types", () => {
    const evt = { type: "future_event", foo: "bar" } as unknown as ApiEvent;
    expect(formatEvent(evt)).toBe(JSON.stringify(evt));
  });

  it("falls back to JSON when required fields are wrong type", () => {
    const evt = {
      type: "reaction_triggered",
      id: "s1",
      reaction_key: 42,
      action: "notify",
    } as unknown as ApiEvent;
    expect(formatEvent(evt)).toBe(JSON.stringify(evt));
  });
});

