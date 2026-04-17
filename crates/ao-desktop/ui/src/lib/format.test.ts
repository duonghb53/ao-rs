import { describe, expect, it } from "vitest";

import { getSessionTabLabel } from "./format";
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

