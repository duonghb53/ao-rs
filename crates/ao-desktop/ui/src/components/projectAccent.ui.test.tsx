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

  it("SessionCard project pill links to repo when PR repo info exists", () => {
    const session = makeSession({
      projectId: "ao-rs",
      pr: {
        number: 1,
        url: "https://github.com/duonghb53/ao-rs/pull/1",
        title: "PR title",
        owner: "duonghb53",
        repo: "ao-rs",
      },
    });

    const { container } = render(<SessionCard session={session} />);

    const pills = Array.from(container.querySelectorAll('.mini-pill[data-project-accent="true"]'));
    const projectPill = pills.find((el) => el.textContent?.trim() === `project: ${session.projectId}`) as
      | HTMLElement
      | undefined;
    expect(projectPill).toBeTruthy();
    expect(projectPill).toHaveAttribute("role", "link");
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

  it("Board toolbar shows repo url when unambiguous", () => {
    const s1 = makeSession({
      id: "s1",
      projectId: "ao-rs",
      pr: {
        number: 1,
        url: "https://github.com/duonghb53/ao-rs/pull/1",
        title: "PR title",
        owner: "duonghb53",
        repo: "ao-rs",
      },
    });
    const { container } = render(<Board sessions={[s1]} title="Sessions" />);
    const link = container.querySelector(".board__toolbar a.hint");
    expect(link).not.toBeNull();
    expect(link).toHaveAttribute("href", "https://github.com/duonghb53/ao-rs");
    expect(link).toHaveTextContent("ao-rs");
  });
});

