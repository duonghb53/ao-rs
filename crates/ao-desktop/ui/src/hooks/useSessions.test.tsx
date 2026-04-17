import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApiEvent, ApiSession } from "../api/client";

type Handlers = {
  onOpen?: () => void;
  onError?: (msg: string) => void;
  onEvent?: (evt: ApiEvent) => void;
};

const handlersRef: { current: Handlers | null } = { current: null };
const fakeEs = { close: vi.fn() };
const getSessionsMock = vi.fn<(baseUrl: string, opts?: { pr?: boolean }) => Promise<ApiSession[]>>();
const connectEventsMock = vi.fn<(baseUrl: string, h: Handlers) => typeof fakeEs>((_url, h) => {
  handlersRef.current = h;
  return fakeEs;
});

vi.mock("../api/client", () => ({
  getSessions: (...args: Parameters<typeof getSessionsMock>) => getSessionsMock(...args),
  connectEvents: (...args: Parameters<typeof connectEventsMock>) => connectEventsMock(...args),
}));

// Import under test AFTER vi.mock so the mocks are in place.
import { useSessions } from "./useSessions";

function session(id: string): ApiSession {
  return {
    id,
    project_id: "ao-rs",
    status: "working",
    activity: null,
    branch: "main",
    task: "do things",
    agent: "claude",
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  getSessionsMock.mockReset();
  connectEventsMock.mockReset();
  connectEventsMock.mockImplementation((_url, h) => {
    handlersRef.current = h;
    return fakeEs;
  });
  fakeEs.close.mockReset();
  handlersRef.current = null;
});

afterEach(() => {
  vi.useRealTimers();
});

/** Drain pending microtasks *and* timers until the hook settles. */
async function flush() {
  await act(async () => {
    await vi.runAllTimersAsync();
  });
}

describe("useSessions", () => {
  it("fetches fast then enriches with PR on mount", async () => {
    const fast = [session("s1")];
    const enriched = [{ ...session("s1"), attention_level: "review" }];
    getSessionsMock.mockResolvedValueOnce(fast).mockResolvedValueOnce(enriched);

    const { result } = renderHook(() => useSessions("http://x"));

    await flush();

    expect(getSessionsMock).toHaveBeenNthCalledWith(1, "http://x");
    expect(getSessionsMock).toHaveBeenNthCalledWith(2, "http://x", { pr: true });
    expect(result.current.sessions).toEqual(enriched);
    expect(connectEventsMock).toHaveBeenCalledTimes(1);
    expect(result.current.conn.kind).toBe("connecting");
  });

  it("marks connection as connected when SSE opens", async () => {
    getSessionsMock.mockResolvedValue([]);
    const { result } = renderHook(() => useSessions("http://x"));
    await flush();

    act(() => handlersRef.current?.onOpen?.());
    expect(result.current.conn).toEqual({ kind: "connected" });
  });

  it("updates sessions from an SSE snapshot event", async () => {
    getSessionsMock.mockResolvedValue([]);
    const { result } = renderHook(() => useSessions("http://x"));
    await flush();

    const snapshot: ApiEvent = { type: "snapshot", sessions: [session("live")] };
    act(() => handlersRef.current?.onEvent?.(snapshot));

    expect(result.current.sessions).toEqual([session("live")]);
  });

  it("routes ui_notification events through onNotification", async () => {
    getSessionsMock.mockResolvedValue([]);
    const onNotification = vi.fn();
    renderHook(() => useSessions("http://x", { onNotification }));
    await flush();

    const notif: ApiEvent = {
      type: "ui_notification",
      notification: {
        id: "sess-123",
        reaction_key: "respond",
        action: "ack",
        priority: "high",
        message: "needs reply",
      },
    };
    act(() => handlersRef.current?.onEvent?.(notif));

    expect(onNotification).toHaveBeenCalledTimes(1);
    expect(onNotification).toHaveBeenCalledWith({
      sessionId: "sess-123",
      reactionKey: "respond",
      action: "ack",
      priority: "high",
      message: "needs reply",
    });
  });

  it("ignores ui_notification events that lack id or reaction_key", async () => {
    getSessionsMock.mockResolvedValue([]);
    const onNotification = vi.fn();
    renderHook(() => useSessions("http://x", { onNotification }));
    await flush();

    act(() =>
      handlersRef.current?.onEvent?.({
        type: "ui_notification",
        notification: { id: "only-id" },
      } as unknown as ApiEvent),
    );

    expect(onNotification).not.toHaveBeenCalled();
  });

  it("forwards non-snapshot events via onEvent", async () => {
    getSessionsMock.mockResolvedValue([]);
    const onEvent = vi.fn();
    renderHook(() => useSessions("http://x", { onEvent }));
    await flush();

    const evt: ApiEvent = { type: "custom", payload: 1 } as ApiEvent;
    act(() => handlersRef.current?.onEvent?.(evt));

    expect(onEvent).toHaveBeenCalledWith(evt);
  });

  it("debounces scheduleRefresh inside a 400ms window", async () => {
    getSessionsMock.mockResolvedValue([]);
    const { result } = renderHook(() => useSessions("http://x"));
    await flush();

    // Clear calls from the initial fast + PR enrich + any post-event refresh.
    getSessionsMock.mockClear();
    getSessionsMock.mockResolvedValue([]);

    act(() => {
      result.current.scheduleRefresh();
      result.current.scheduleRefresh();
      result.current.scheduleRefresh();
    });

    // Nothing fires before the debounce window elapses.
    expect(getSessionsMock).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(400);
    });

    expect(getSessionsMock).toHaveBeenCalledTimes(1);
    expect(getSessionsMock).toHaveBeenCalledWith("http://x");
  });

  it("schedules reconnect with exponential backoff on SSE error", async () => {
    getSessionsMock.mockResolvedValue([]);
    const { result } = renderHook(() => useSessions("http://x"));
    await flush();
    expect(connectEventsMock).toHaveBeenCalledTimes(1);

    // First error → 1s delay before reconnect attempt.
    act(() => handlersRef.current?.onError?.("boom"));
    expect(result.current.conn.kind).toBe("error");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(999);
    });
    expect(connectEventsMock).toHaveBeenCalledTimes(1); // not yet

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(connectEventsMock).toHaveBeenCalledTimes(2);

    // Second error → 2s delay.
    act(() => handlersRef.current?.onError?.("boom2"));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_999);
    });
    expect(connectEventsMock).toHaveBeenCalledTimes(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(connectEventsMock).toHaveBeenCalledTimes(3);
  });

  it("retryConnection resets retries and reconnects immediately", async () => {
    getSessionsMock.mockResolvedValue([]);
    const { result } = renderHook(() => useSessions("http://x"));
    await flush();

    act(() => handlersRef.current?.onError?.("down"));
    const prevCalls = connectEventsMock.mock.calls.length;

    getSessionsMock.mockClear();
    getSessionsMock.mockResolvedValue([]);

    await act(async () => {
      await result.current.retryConnection();
    });
    // After retry: fast fetch (1) + wireSse invocation (+1 connectEvents) + bg pr fetch (2)
    await flush();

    expect(connectEventsMock.mock.calls.length).toBeGreaterThan(prevCalls);
    expect(getSessionsMock).toHaveBeenCalledWith("http://x");
    expect(getSessionsMock).toHaveBeenCalledWith("http://x", { pr: true });
  });

  it("sets conn to error when the initial fetch fails", async () => {
    getSessionsMock.mockRejectedValueOnce(new Error("network down"));
    const { result } = renderHook(() => useSessions("http://x"));

    await flush();

    expect(result.current.conn).toEqual({ kind: "error", message: "network down" });
    expect(connectEventsMock).not.toHaveBeenCalled();
  });

  it("runs a PR refresh every 45 seconds while connected", async () => {
    getSessionsMock.mockResolvedValue([]);
    renderHook(() => useSessions("http://x"));
    await flush();

    act(() => handlersRef.current?.onOpen?.());
    getSessionsMock.mockClear();
    getSessionsMock.mockResolvedValue([]);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(45_000);
    });
    expect(getSessionsMock).toHaveBeenCalledWith("http://x", { pr: true });
  });

  it("closes SSE and clears timers on unmount", async () => {
    getSessionsMock.mockResolvedValue([]);
    const { unmount } = renderHook(() => useSessions("http://x"));
    await flush();

    unmount();
    expect(fakeEs.close).toHaveBeenCalled();

    // Advancing time after unmount must not throw or reconnect.
    const before = connectEventsMock.mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });
    expect(connectEventsMock).toHaveBeenCalledTimes(before);
  });
});
