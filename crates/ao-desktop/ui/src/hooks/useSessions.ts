import { type Dispatch, type SetStateAction, useCallback, useEffect, useRef, useState } from "react";
import {
  type ApiEvent,
  type ApiPr,
  type ApiSession,
  type ConnectionStatus,
  connectEvents,
  getSessions,
} from "../api/client";

export type UiNotificationPayload = {
  sessionId: string;
  reactionKey: string;
  action: string;
  priority?: string;
  message?: string;
};

export type UseSessionsOptions = {
  onNotification?: (n: UiNotificationPayload) => void;
  onEvent?: (evt: ApiEvent) => void;
};

export type UseSessions = {
  sessions: ApiSession[];
  setSessions: Dispatch<SetStateAction<ApiSession[]>>;
  conn: ConnectionStatus;
  refreshSessionsFast: () => Promise<void>;
  refreshSessionsWithPr: () => Promise<void>;
  scheduleRefresh: () => void;
  retryConnection: () => Promise<void>;
};

const REFRESH_DEBOUNCE_MS = 400;
const MAX_BACKOFF_MS = 30_000;
const MAX_BACKOFF_EXPONENT = 5;

function parseUiNotification(evt: ApiEvent): UiNotificationPayload | null {
  if (!evt || typeof evt !== "object") return null;
  if ((evt as { type?: unknown }).type !== "ui_notification") return null;
  const n = (evt as unknown as Record<string, unknown>).notification;
  if (!n || typeof n !== "object") return null;
  const rec = n as Record<string, unknown>;
  const sessionId = typeof rec.id === "string" ? rec.id : "";
  const reactionKey = typeof rec.reaction_key === "string" ? rec.reaction_key : "";
  if (!sessionId || !reactionKey) return null;
  return {
    sessionId,
    reactionKey,
    action: typeof rec.action === "string" ? rec.action : "",
    priority: typeof rec.priority === "string" ? rec.priority : undefined,
    message: typeof rec.message === "string" ? rec.message : undefined,
  };
}

function isSnapshotEvent(evt: ApiEvent): evt is ApiEvent & { sessions: ApiSession[] } {
  return (
    evt != null &&
    typeof evt === "object" &&
    (evt as { type?: unknown }).type === "snapshot" &&
    Array.isArray((evt as { sessions?: unknown }).sessions)
  );
}

function isPrEnrichmentChangedEvent(
  evt: ApiEvent,
): evt is ApiEvent & { id: string; pr: ApiPr | null; attention_level: string } {
  if (evt == null || typeof evt !== "object") return false;
  if ((evt as { type?: unknown }).type !== "pr_enrichment_changed") return false;
  const id = (evt as { id?: unknown }).id;
  const attention = (evt as { attention_level?: unknown }).attention_level;
  return typeof id === "string" && typeof attention === "string";
}

export function useSessions(baseUrl: string, opts: UseSessionsOptions = {}): UseSessions {
  const [sessions, setSessions] = useState<ApiSession[]>([]);
  const [conn, setConn] = useState<ConnectionStatus>({ kind: "disconnected" });
  const esRef = useRef<EventSource | null>(null);
  const refreshTimerRef = useRef<number | null>(null);
  const sseReconnectTimerRef = useRef<number | null>(null);
  const sseRetryRef = useRef(0);
  const wireSseRef = useRef<(() => void) | null>(null);

  // Keep callbacks in refs so hook consumers can change handler identity
  // without re-running the SSE effect.
  const onNotificationRef = useRef(opts.onNotification);
  const onEventRef = useRef(opts.onEvent);
  onNotificationRef.current = opts.onNotification;
  onEventRef.current = opts.onEvent;

  /** Fast list — no `gh` / PR enrichment (cheap on every SSE tick). */
  const refreshSessionsFast = useCallback(async () => {
    const s = await getSessions(baseUrl);
    setSessions(s);
  }, [baseUrl]);

  /** Full list with PR + attention (heavier; use after actions or on a timer). */
  const refreshSessionsWithPr = useCallback(async () => {
    const s = await getSessions(baseUrl, { pr: true });
    setSessions(s);
  }, [baseUrl]);

  const scheduleRefresh = useCallback(() => {
    if (refreshTimerRef.current !== null) return;
    refreshTimerRef.current = window.setTimeout(() => {
      refreshTimerRef.current = null;
      refreshSessionsFast().catch(() => {
        // ignore; conn status will reflect SSE errors separately
      });
    }, REFRESH_DEBOUNCE_MS);
  }, [refreshSessionsFast]);

  // PR/CI signals arrive via the `pr_enrichment_changed` SSE delta — no client-side polling needed.

  // Auto-connect on load and when baseUrl changes: sessions (with PR) + SSE with backoff reconnect.
  useEffect(() => {
    let cancelled = false;

    const clearSseReconnect = () => {
      if (sseReconnectTimerRef.current !== null) {
        window.clearTimeout(sseReconnectTimerRef.current);
        sseReconnectTimerRef.current = null;
      }
    };

    const connectEs = () => {
      if (cancelled) return;
      clearSseReconnect();
      esRef.current?.close();
      esRef.current = connectEvents(baseUrl, {
        onOpen: () => {
          if (cancelled) return;
          setConn({ kind: "connected" });
          sseRetryRef.current = 0;
        },
        onError: () => {
          if (cancelled) return;
          setConn({ kind: "error", message: "SSE connection error" });
          if (sseReconnectTimerRef.current !== null) return;
          const attempt = sseRetryRef.current++;
          const delay = Math.min(
            MAX_BACKOFF_MS,
            1000 * Math.pow(2, Math.min(attempt, MAX_BACKOFF_EXPONENT)),
          );
          sseReconnectTimerRef.current = window.setTimeout(() => {
            sseReconnectTimerRef.current = null;
            if (cancelled) return;
            connectEs();
          }, delay);
        },
        onEvent: (evt) => {
          if (cancelled) return;
          // SSE snapshot: update sessions immediately without polling. Snapshot
          // entries already carry `pr` + `attention_level` from the lifecycle's
          // shared enrichment cache.
          if (isSnapshotEvent(evt)) {
            setSessions(evt.sessions);
            return;
          }

          // PR enrichment delta: merge into one session in place. Source of truth
          // for `?pr=true` data — no follow-up HTTP poll needed.
          if (isPrEnrichmentChangedEvent(evt)) {
            setSessions((prev) =>
              prev.map((s) =>
                s.id === evt.id
                  ? { ...s, pr: evt.pr, attention_level: evt.attention_level }
                  : s,
              ),
            );
            onEventRef.current?.(evt);
            return;
          }

          const notification = parseUiNotification(evt);
          if (notification) onNotificationRef.current?.(notification);

          onEventRef.current?.(evt);
          scheduleRefresh();
        },
      });
    };

    wireSseRef.current = connectEs;

    (async () => {
      setConn({ kind: "connecting" });
      try {
        // Fast path: list sessions without PR enrichment (no per-session `gh` calls).
        // The SSE snapshot frame will replace this immediately with the lifecycle's
        // already-enriched view (`pr` + `attention_level`), and `pr_enrichment_changed`
        // deltas keep it fresh — so no follow-up `?pr=true` HTTP call is needed.
        const fast = await getSessions(baseUrl);
        if (cancelled) return;
        setSessions(fast);
        connectEs();
      } catch (e) {
        if (cancelled) return;
        const msg = e instanceof Error ? e.message : "unknown error";
        setConn({ kind: "error", message: msg });
      }
    })();

    return () => {
      cancelled = true;
      wireSseRef.current = null;
      clearSseReconnect();
      if (refreshTimerRef.current !== null) {
        window.clearTimeout(refreshTimerRef.current);
        refreshTimerRef.current = null;
      }
      esRef.current?.close();
      esRef.current = null;
    };
  }, [baseUrl, scheduleRefresh]);

  const retryConnection = useCallback(async () => {
    sseRetryRef.current = 0;
    if (sseReconnectTimerRef.current !== null) {
      window.clearTimeout(sseReconnectTimerRef.current);
      sseReconnectTimerRef.current = null;
    }
    esRef.current?.close();
    esRef.current = null;
    setConn({ kind: "connecting" });
    try {
      const fast = await getSessions(baseUrl);
      setSessions(fast);
      wireSseRef.current?.();
      // SSE snapshot will replace this with the enriched view shortly.
    } catch (e) {
      const msg = e instanceof Error ? e.message : "unknown error";
      setConn({ kind: "error", message: msg });
    }
  }, [baseUrl]);

  return {
    sessions,
    setSessions,
    conn,
    refreshSessionsFast,
    refreshSessionsWithPr,
    scheduleRefresh,
    retryConnection,
  };
}
