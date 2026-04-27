import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import type { DashboardPR, DashboardSession } from "../lib/types";
import { SessionDetail } from "./SessionDetail";

function makeSession(partial: Partial<DashboardSession>): DashboardSession {
  return {
    id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    projectId: "ao-rs",
    status: "working",
    activity: null,
    agent: null,
    branch: "feat/fix-auth",
    summary: null,
    summaryIsFallback: false,
    issueTitle: null,
    issueId: null,
    issueUrl: null,
    userPrompt: null,
    pr: null,
    claimedPrNumber: null,
    claimedPrUrl: null,
    attentionLevel: null,
    metadata: {},
    spawnedBy: null,
    createdAt: null,
    ...partial,
  };
}

const conflictPr: DashboardPR = {
  number: 42,
  url: "https://github.com/acme/app/pull/42",
  title: "Fix auth bug",
  owner: "acme",
  repo: "app",
  branch: "feat/fix-auth",
  baseBranch: "main",
  mergeable: false,
};

describe("SessionDetail — merge conflict actions", () => {
  let writeTextMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    writeTextMock = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: writeTextMock },
      configurable: true,
      writable: true,
    });
  });

  it("shows conflict actions when mergeable is false and all fields present", () => {
    render(<SessionDetail session={makeSession({ pr: conflictPr })} />);
    expect(screen.getByText("Open compare")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy branch" })).toBeInTheDocument();
  });

  it("compare link has correct GitHub URL", () => {
    render(<SessionDetail session={makeSession({ pr: conflictPr })} />);
    const link = screen.getByText("Open compare").closest("a");
    expect(link).toHaveAttribute(
      "href",
      "https://github.com/acme/app/compare/main...feat/fix-auth"
    );
    expect(link).toHaveAttribute("target", "_blank");
  });

  it("does not show conflict actions when mergeable is true", () => {
    render(<SessionDetail session={makeSession({ pr: { ...conflictPr, mergeable: true } })} />);
    expect(screen.queryByText("Open compare")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Copy branch" })).not.toBeInTheDocument();
  });

  it("does not show conflict actions when owner is missing", () => {
    render(<SessionDetail session={makeSession({ pr: { ...conflictPr, owner: undefined } })} />);
    expect(screen.queryByText("Open compare")).not.toBeInTheDocument();
  });

  it("does not show conflict actions when no PR", () => {
    render(<SessionDetail session={makeSession({ pr: null })} />);
    expect(screen.queryByText("Open compare")).not.toBeInTheDocument();
  });

  it("copy button writes branch to clipboard and shows Copied! feedback", async () => {
    render(<SessionDetail session={makeSession({ pr: conflictPr })} />);
    const btn = screen.getByRole("button", { name: "Copy branch" });
    fireEvent.click(btn);
    await act(async () => {});
    expect(writeTextMock).toHaveBeenCalledWith("feat/fix-auth");
    expect(screen.getByRole("button", { name: "Copied!" })).toBeInTheDocument();
  });
});
