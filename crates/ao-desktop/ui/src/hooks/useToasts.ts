import { useCallback, useEffect, useRef, useState } from "react";

export type ToastItem = {
  key: string;
  at: number;
  sessionId: string;
  reactionKey: string;
  action: string;
  priority?: string;
  message?: string;
};

export type ToastInput = Omit<ToastItem, "key" | "at">;

const MAX_TOASTS = 6;
const AUTO_DISMISS_MS = 12_000;

function makeKey(at: number): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return `${at}-${Math.random()}`;
}

export type UseToasts = {
  toasts: ToastItem[];
  pushToast: (t: ToastInput) => void;
  dismissToast: (key: string) => void;
};

export function useToasts(): UseToasts {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const timersRef = useRef<Map<string, number>>(new Map());

  useEffect(() => {
    const timers = timersRef.current;
    return () => {
      for (const id of timers.values()) window.clearTimeout(id);
      timers.clear();
    };
  }, []);

  const dismissToast = useCallback((key: string) => {
    const timerId = timersRef.current.get(key);
    if (timerId !== undefined) {
      window.clearTimeout(timerId);
      timersRef.current.delete(key);
    }
    setToasts((prev) => prev.filter((x) => x.key !== key));
  }, []);

  const pushToast = useCallback(
    (t: ToastInput) => {
      const at = Date.now();
      const key = makeKey(at);
      setToasts((prev) => [{ key, at, ...t }, ...prev].slice(0, MAX_TOASTS));
      const timerId = window.setTimeout(() => {
        timersRef.current.delete(key);
        setToasts((prev) => prev.filter((x) => x.key !== key));
      }, AUTO_DISMISS_MS);
      timersRef.current.set(key, timerId);
    },
    [],
  );

  return { toasts, pushToast, dismissToast };
}
