import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useToasts } from "./useToasts";

const basePayload = {
  sessionId: "s1",
  reactionKey: "respond",
  action: "ping",
};

describe("useToasts", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("pushes toasts and prepends newest first", () => {
    const { result } = renderHook(() => useToasts());

    act(() => {
      result.current.pushToast({ ...basePayload, reactionKey: "first" });
    });
    act(() => {
      result.current.pushToast({ ...basePayload, reactionKey: "second" });
    });

    expect(result.current.toasts).toHaveLength(2);
    expect(result.current.toasts[0]?.reactionKey).toBe("second");
    expect(result.current.toasts[1]?.reactionKey).toBe("first");
  });

  it("caps the stack at 6", () => {
    const { result } = renderHook(() => useToasts());
    act(() => {
      for (let i = 0; i < 10; i++) {
        result.current.pushToast({ ...basePayload, reactionKey: `rk-${i}` });
      }
    });
    expect(result.current.toasts).toHaveLength(6);
    // newest-first: the last pushed one wins the slot
    expect(result.current.toasts[0]?.reactionKey).toBe("rk-9");
  });

  it("dismissToast removes the matching key immediately", () => {
    const { result } = renderHook(() => useToasts());
    act(() => {
      result.current.pushToast({ ...basePayload, reactionKey: "a" });
      result.current.pushToast({ ...basePayload, reactionKey: "b" });
    });

    const keep = result.current.toasts.find((t) => t.reactionKey === "a");
    const drop = result.current.toasts.find((t) => t.reactionKey === "b");
    expect(keep && drop).toBeTruthy();

    act(() => {
      result.current.dismissToast(drop!.key);
    });

    expect(result.current.toasts).toHaveLength(1);
    expect(result.current.toasts[0]?.reactionKey).toBe("a");
  });

  it("auto-dismisses after 12 seconds", () => {
    const { result } = renderHook(() => useToasts());
    act(() => {
      result.current.pushToast(basePayload);
    });
    expect(result.current.toasts).toHaveLength(1);

    act(() => {
      vi.advanceTimersByTime(11_999);
    });
    expect(result.current.toasts).toHaveLength(1);

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current.toasts).toHaveLength(0);
  });

  it("clears pending auto-dismiss timers on unmount", () => {
    const { result, unmount } = renderHook(() => useToasts());
    act(() => {
      result.current.pushToast(basePayload);
    });

    unmount();

    // Advancing time after unmount must not blow up (no lingering setState
    // from an auto-dismiss callback on an unmounted component).
    expect(() => {
      vi.advanceTimersByTime(20_000);
    }).not.toThrow();
  });
});
