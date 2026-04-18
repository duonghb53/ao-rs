import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { IssuesPanel } from "./IssuesPanel";
import type { BacklogIssue } from "../api/client";

const BASE = "http://dash.test";

function okResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

const FIXTURE: BacklogIssue[] = [
  {
    project_id: "demo",
    number: 42,
    title: "Add dark mode",
    url: "https://github.com/acme/demo/issues/42",
    labels: ["enhancement", "ui"],
    repo: "acme/demo",
    state: "open",
  },
  {
    project_id: "demo",
    number: 7,
    title: "Fix flaky test",
    url: "https://github.com/acme/demo/issues/7",
    labels: [],
    repo: "acme/demo",
    state: "open",
  },
];

describe("IssuesPanel", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    cleanup();
  });

  it("renders issues returned by /api/issues and a Spawn button per row", async () => {
    fetchMock.mockResolvedValueOnce(okResponse(FIXTURE));
    render(<IssuesPanel baseUrl={BASE} projectId={null} onSpawn={vi.fn()} />);

    await waitFor(() => {
      expect(screen.getByText("Add dark mode")).toBeInTheDocument();
    });
    expect(screen.getByText("Fix flaky test")).toBeInTheDocument();
    const spawnButtons = screen.getAllByRole("button", { name: /Spawn/ });
    expect(spawnButtons).toHaveLength(2);

    // URL should be built without `?project_id=` when projectId is null.
    expect(fetchMock.mock.calls[0][0]).toBe(`${BASE}/api/issues`);
  });

  it("passes ?project_id= to the API when projectId is set", async () => {
    fetchMock.mockResolvedValueOnce(okResponse([]));
    render(<IssuesPanel baseUrl={BASE} projectId="demo" onSpawn={vi.fn()} />);

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalled();
    });
    expect(fetchMock.mock.calls[0][0]).toBe(`${BASE}/api/issues?project_id=demo`);
  });

  it("shows the empty state when the list is empty", async () => {
    fetchMock.mockResolvedValueOnce(okResponse([]));
    render(<IssuesPanel baseUrl={BASE} projectId={null} onSpawn={vi.fn()} />);

    await waitFor(() => {
      expect(screen.getByText("No open issues.")).toBeInTheDocument();
    });
  });

  it("invokes onSpawn with the clicked issue", async () => {
    fetchMock.mockResolvedValueOnce(okResponse([FIXTURE[0]]));
    const onSpawn = vi.fn().mockResolvedValue(undefined);
    render(<IssuesPanel baseUrl={BASE} projectId={null} onSpawn={onSpawn} />);

    await waitFor(() => {
      expect(screen.getByText("Add dark mode")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByRole("button", { name: /Spawn/ }));
    expect(onSpawn).toHaveBeenCalledTimes(1);
    expect(onSpawn.mock.calls[0][0]).toEqual(FIXTURE[0]);
  });

  it("disables Spawn and shows the reason when spawnDisabledReason is set", async () => {
    fetchMock.mockResolvedValueOnce(okResponse([FIXTURE[0]]));
    render(
      <IssuesPanel
        baseUrl={BASE}
        projectId={null}
        onSpawn={vi.fn()}
        spawnDisabledReason="Open a session in this project first"
      />,
    );

    await waitFor(() => {
      expect(screen.getByText("Add dark mode")).toBeInTheDocument();
    });
    const btn = screen.getByRole("button", { name: /Spawn/ });
    expect(btn).toBeDisabled();
    expect(screen.getByText("Open a session in this project first")).toBeInTheDocument();
  });

  it("renders a retry banner and refetches when fetch fails", async () => {
    fetchMock.mockRejectedValueOnce(new Error("boom"));
    render(<IssuesPanel baseUrl={BASE} projectId={null} onSpawn={vi.fn()} />);

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent(/Failed to load issues: boom/);
    });

    fetchMock.mockResolvedValueOnce(okResponse(FIXTURE));
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => {
      expect(screen.getByText("Add dark mode")).toBeInTheDocument();
    });
  });
});
