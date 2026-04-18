import { describe, it, expect } from "vitest";
import {
  deriveManagedProjectIds,
  groupWorkersByOrchestrator,
  isUnaffiliated,
} from "../orchestrator";
import type { DashboardOrchestrator, DashboardSession } from "../types";

// ─── test data builders ───────────────────────────────────────────────────────

function makeOrch(overrides: Partial<DashboardOrchestrator> = {}): DashboardOrchestrator {
  return {
    id: "ao-rs-orchestrator-1",
    status: "working",
    managedProjectIds: ["ao-rs"],
    primaryProjectId: "ao-rs",
    createdAt: null,
    ...overrides,
  };
}

function makeSession(overrides: Partial<DashboardSession> = {}): DashboardSession {
  return {
    id: "ao-rs-1",
    projectId: "ao-rs",
    status: "working",
    activity: null,
    agent: null,
    branch: null,
    summary: null,
    summaryIsFallback: false,
    issueTitle: null,
    issueId: null,
    issueUrl: null,
    userPrompt: null,
    pr: null,
    attentionLevel: null,
    metadata: {},
    spawnedBy: null,
    ...overrides,
  };
}

// ─── deriveManagedProjectIds ──────────────────────────────────────────────────

describe("deriveManagedProjectIds", () => {
  it("returns primaryProjectId when no workers", () => {
    const orch = makeOrch({ primaryProjectId: "ao-rs" });
    expect(deriveManagedProjectIds(orch, [])).toEqual(["ao-rs"]);
  });

  it("includes worker project ids beyond primaryProjectId", () => {
    const orch = makeOrch({ primaryProjectId: "ao-rs" });
    const workers = [
      makeSession({ projectId: "ao-rs" }),
      makeSession({ id: "golf-1", projectId: "golf-coach" }),
    ];
    const result = deriveManagedProjectIds(orch, workers);
    expect(result).toContain("ao-rs");
    expect(result).toContain("golf-coach");
    expect(result).toHaveLength(2);
  });

  it("deduplicates project ids", () => {
    const orch = makeOrch({ primaryProjectId: "ao-rs" });
    const workers = [
      makeSession({ projectId: "ao-rs" }),
      makeSession({ id: "ao-rs-2", projectId: "ao-rs" }),
    ];
    expect(deriveManagedProjectIds(orch, workers)).toEqual(["ao-rs"]);
  });
});

// ─── groupWorkersByOrchestrator ───────────────────────────────────────────────

describe("groupWorkersByOrchestrator", () => {
  it("excludes orchestrator sessions from all groups", () => {
    const orch = makeOrch({ id: "ao-rs-orchestrator-1" });
    const orchSession = makeSession({ id: "ao-rs-orchestrator-1", spawnedBy: null });
    const { byOrchestrator, unaffiliated } = groupWorkersByOrchestrator([orchSession], [orch]);
    expect(byOrchestrator.get("ao-rs-orchestrator-1")).toEqual([]);
    expect(unaffiliated).toEqual([]);
  });

  it("places worker with spawnedBy into correct orchestrator bucket", () => {
    const orch = makeOrch({ id: "ao-rs-orchestrator-1" });
    const worker = makeSession({ id: "ao-rs-194", spawnedBy: "ao-rs-orchestrator-1" });
    const { byOrchestrator, unaffiliated } = groupWorkersByOrchestrator([worker], [orch]);
    expect(byOrchestrator.get("ao-rs-orchestrator-1")).toContain(worker);
    expect(unaffiliated).toEqual([]);
  });

  it("places session with no spawnedBy into unaffiliated", () => {
    const orch = makeOrch();
    const orphan = makeSession({ id: "manual-1", spawnedBy: null });
    const { byOrchestrator, unaffiliated } = groupWorkersByOrchestrator([orphan], [orch]);
    expect(byOrchestrator.get(orch.id)).toEqual([]);
    expect(unaffiliated).toContain(orphan);
  });

  it("correctly splits workers across two orchestrators", () => {
    const orch1 = makeOrch({ id: "ao-rs-orchestrator-1" });
    const orch2 = makeOrch({ id: "ao-rs-orchestrator-2" });
    const w1 = makeSession({ id: "w1", spawnedBy: "ao-rs-orchestrator-1" });
    const w2 = makeSession({ id: "w2", spawnedBy: "ao-rs-orchestrator-2" });
    const { byOrchestrator } = groupWorkersByOrchestrator([w1, w2], [orch1, orch2]);
    expect(byOrchestrator.get("ao-rs-orchestrator-1")).toContain(w1);
    expect(byOrchestrator.get("ao-rs-orchestrator-2")).toContain(w2);
  });

  it("(a) orchestrator with workers in 2 projects builds separate buckets", () => {
    const orch = makeOrch({ id: "ao-rs-orchestrator-4" });
    const w1 = makeSession({ id: "ao-rs-194", projectId: "ao-rs", spawnedBy: "ao-rs-orchestrator-4" });
    const w2 = makeSession({ id: "golf-7", projectId: "golf-coach", spawnedBy: "ao-rs-orchestrator-4" });
    const { byOrchestrator } = groupWorkersByOrchestrator([w1, w2], [orch]);
    const workers = byOrchestrator.get("ao-rs-orchestrator-4")!;
    const projectIds = [...new Set(workers.map((s) => s.projectId))];
    expect(projectIds).toContain("ao-rs");
    expect(projectIds).toContain("golf-coach");
  });

  it("(b) orchestrator with 0 workers still appears in map", () => {
    const orch = makeOrch({ id: "ao-rs-orchestrator-4" });
    const { byOrchestrator } = groupWorkersByOrchestrator([], [orch]);
    expect(byOrchestrator.has("ao-rs-orchestrator-4")).toBe(true);
    expect(byOrchestrator.get("ao-rs-orchestrator-4")).toEqual([]);
  });

  it("(c) unaffiliated-only: all sessions go to unaffiliated when no orchestrators", () => {
    const session = makeSession({ id: "s1", spawnedBy: null });
    const { byOrchestrator, unaffiliated } = groupWorkersByOrchestrator([session], []);
    expect(byOrchestrator.size).toBe(0);
    expect(unaffiliated).toContain(session);
  });

  it("(d) mixed: orchestrator workers and unaffiliated sessions are correctly split", () => {
    const orch = makeOrch({ id: "ao-rs-orchestrator-4" });
    const orchSelf = makeSession({ id: "ao-rs-orchestrator-4", spawnedBy: null });
    const worker = makeSession({ id: "ao-rs-194", spawnedBy: "ao-rs-orchestrator-4" });
    const manual = makeSession({ id: "ao-rs-168", spawnedBy: null });
    const { byOrchestrator, unaffiliated } = groupWorkersByOrchestrator(
      [orchSelf, worker, manual],
      [orch],
    );
    expect(byOrchestrator.get("ao-rs-orchestrator-4")).toContain(worker);
    expect(unaffiliated).toContain(manual);
    expect(unaffiliated).not.toContain(orchSelf);
    expect(unaffiliated).not.toContain(worker);
  });
});

// ─── isUnaffiliated ───────────────────────────────────────────────────────────

describe("isUnaffiliated", () => {
  it("returns true for session with no spawnedBy and not an orchestrator", () => {
    const orch = makeOrch();
    const s = makeSession({ id: "manual", spawnedBy: null });
    expect(isUnaffiliated(s, [orch])).toBe(true);
  });

  it("returns false for session that is itself an orchestrator", () => {
    const orch = makeOrch({ id: "ao-rs-orchestrator-1" });
    const s = makeSession({ id: "ao-rs-orchestrator-1", spawnedBy: null });
    expect(isUnaffiliated(s, [orch])).toBe(false);
  });

  it("returns false for session with spawnedBy set", () => {
    const orch = makeOrch();
    const s = makeSession({ spawnedBy: orch.id });
    expect(isUnaffiliated(s, [orch])).toBe(false);
  });
});
