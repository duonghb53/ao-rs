import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { SessionCard, _clearEnteredSessionIds } from "./SessionCard";
import type { DashboardSession } from "../lib/types";

function makeSession(id: string, overrides: Partial<DashboardSession> = {}): DashboardSession {
  return {
    id,
    projectId: "test-proj",
    status: "active",
    activity: "active",
    agent: null,
    branch: null,
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
    ...overrides,
  };
}

describe("SessionCard entrance animation", () => {
  let rafCallbacks: FrameRequestCallback[] = [];

  beforeEach(() => {
    _clearEnteredSessionIds();
    rafCallbacks = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
      rafCallbacks.push(cb);
      return rafCallbacks.length;
    });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("applies card-enter class on first mount", () => {
    const session = makeSession("sess-001");
    const { container } = render(<SessionCard session={session} />);
    const card = container.querySelector(".card");
    expect(card?.classList.contains("card-enter")).toBe(true);
  });

  it("omits card-enter class when session already entered (remount after column change)", () => {
    const session = makeSession("sess-002");

    // First mount — rAF fires, registering the session
    const { unmount } = render(<SessionCard session={session} />);
    rafCallbacks.forEach((cb) => cb(0));
    unmount();
    cleanup();

    // Remount (simulates column change causing React key to remount)
    const { container } = render(<SessionCard session={session} />);
    const card = container.querySelector(".card");
    expect(card?.classList.contains("card-enter")).toBe(false);
  });

  it("cancels rAF registration when component unmounts before first paint", () => {
    const session = makeSession("sess-003");
    const { unmount } = render(<SessionCard session={session} />);
    // Unmount before rAF fires — session should NOT be registered
    unmount();
    expect(window.cancelAnimationFrame).toHaveBeenCalled();

    // rAF callback never ran, so re-render should still get card-enter
    cleanup();
    const { container } = render(<SessionCard session={session} />);
    const card = container.querySelector(".card");
    expect(card?.classList.contains("card-enter")).toBe(true);
  });
});
