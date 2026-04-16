import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";

import type { DashboardSession } from "../lib/types";
import { Board } from "./Board";
import { SessionCard } from "./SessionCard";

function makeSession(partial: Partial<DashboardSession>): DashboardSession {
  return {
    id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    projectId: "ao-rs",
    status: "working",
    activity: "idle working",
    branch: "ao-fb01ba28-feat-issue-77",
    summary: null,
    summaryIsFallback: false,
    issueTitle: "Test issue",
    issueId: "42",
    issueUrl: null,
    userPrompt: null,
    pr: null,
    attentionLevel: "working",
    metadata: {},
    ...partial,
  };
}

describe("project-accent mini-pills", () => {
  it("SessionCard renders project + branch mini-pills with data-project-accent", () => {
    const session = makeSession({
      projectId: "ao-rs",
      branch: "feat/add-something",
    });

    const { container } = render(<SessionCard session={session} />);

    const projectPill = screen.getByText(`project: ${session.projectId}`);
    expect(projectPill).toHaveAttribute("data-project-accent", "true");
    expect(projectPill.getAttribute("style") ?? "").toContain("--project-h");

    const branchPill = within(container).getByText(`branch: ${session.branch}`);
    expect(branchPill).toHaveAttribute("data-project-accent", "true");
    expect(branchPill.getAttribute("style") ?? "").toContain("--project-h");
  });

  it("Board column header shows project pill only when a single project exists in the lane", () => {
    const projectId = "ao-rs";
    const s1 = makeSession({ projectId, id: "s1" });
    const s2 = makeSession({ projectId, id: "s2" });

    const { container } = render(<Board sessions={[s1, s2]} title="Board" />);

    const header = container.querySelector('section.board-col[data-col="working"] .board-col__header');
    expect(header).not.toBeNull();

    const headerScope = within(header as HTMLElement);
    const projectPill = headerScope.getByText(`project: ${projectId}`);
    expect(projectPill).toHaveAttribute("data-project-accent", "true");
  });
});

