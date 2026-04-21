import { describe, expect, it } from "vitest";
import { render, within } from "@testing-library/react";

import type { DashboardSession } from "../lib/types";
import { Board } from "./Board";
import { SessionCard } from "./SessionCard";

function makeSession(partial: Partial<DashboardSession>): DashboardSession {
  return {
    id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    projectId: "ao-rs",
    status: "working",
    activity: "idle working",
    agent: null,
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
    spawnedBy: partial.spawnedBy ?? null,
  };
}

describe("project-accent propagation", () => {
  it("SessionCard root carries the --project-h custom property", () => {
    const session = makeSession({ projectId: "ao-rs" });
    const { container } = render(<SessionCard session={session} />);
    const card = container.querySelector(".card") as HTMLElement | null;
    expect(card).not.toBeNull();
    expect(card!.getAttribute("style") ?? "").toContain("--project-h");
  });

  it("SessionCard shows branch and issue link", () => {
    const session = makeSession({
      projectId: "ao-rs",
      branch: "feat/add-something",
      issueId: "42",
      issueUrl: "https://github.com/x/y/issues/42",
    });
    const { container } = render(<SessionCard session={session} />);
    const branch = within(container).getByText(session.branch!);
    expect(branch).toBeInTheDocument();
  });

  it("Board renders a column per lane with a data-col attribute", () => {
    const projectId = "ao-rs";
    const s1 = makeSession({ projectId, id: "s1" });
    const s2 = makeSession({ projectId, id: "s2" });

    const { container } = render(<Board sessions={[s1, s2]} title="Board" />);
    const workingCol = container.querySelector('section.col[data-col="working"]');
    expect(workingCol).not.toBeNull();
    const headerScope = within(workingCol as HTMLElement);
    expect(headerScope.getByText("Working")).toBeInTheDocument();
  });
});
