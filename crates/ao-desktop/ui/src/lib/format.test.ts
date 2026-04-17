import { describe, expect, it } from "vitest";

import { formatCiStatus, formatReviewDecision, getSessionTabLabel } from "./format";
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

