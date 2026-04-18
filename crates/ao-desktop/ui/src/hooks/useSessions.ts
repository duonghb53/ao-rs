import { type Dispatch, type SetStateAction, useCallback, useEffect, useRef, useState } from "react";
import {
  type ApiEvent,
  type ApiSession,
  type ConnectionStatus,
  connectEvents,
  getSessions,
  listOrchestrators,
} from "../api/client";
import type { DashboardOrchestrator } from "../lib/types";

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
  orchestrators: DashboardOrchestrator[];
  conn: ConnectionStatus;
  refreshSessionsFast: () => Promise<void>;
  refreshSessionsWithPr: () => Promise<void>;
  scheduleRefresh: () => void;
  retryConnection: () => Promise<void>;
};

const PR_REFRESH_INTERVAL_MS = 45_000;
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

export function useSessions(baseUrl: string, opts: UseSessionsOptions = {}): UseSessions {
  const [sessions, setSessions] = useState<ApiSession[]>([]);
  const [orchestrators, setOrchestrators] = useState<DashboardOrchestrator[]>([]);
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

  const refreshOrchestrators = useCallback(async () => {
    try {
      const o = await listOrchestrators(baseUrl);
      setOrchestrators(o);
    } catch {
      // Keep stale list on error; conn status reflects SSE errors separately.
    }
  }, [baseUrl]);

  /** Fast list — no `gh` / PR enrichment (cheap on every SSE tick). */
  const refreshSessionsFast = useCallback(async () => {
    const [s] = await Promise.all([getSessions(baseUrl), refreshOrchestrators()]);
    setSessions(s);
  }, [baseUrl, refreshOrchestrators]);

  /** Full list with PR + attention (heavier; use after actions or on a timer). */
  const refreshSessionsWithPr = useCallback(async () => {
    const [s] = await Promise.all([getSessions(baseUrl, { pr: true }), refreshOrchestrators()]);
    setSessions(s);
  }, [baseUrl, refreshOrchestrators]);

  const scheduleRefresh = useCallback(() => {
    if (refreshTimerRef.current !== null) return;
    refreshTimerRef.current = window.setTimeout(() => {
      refreshTimerRef.current = null;
      refreshSessionsFast().catch(() => {
        // ignore; conn status will reflect SSE errors separately
      });
    }, REFRESH_DEBOUNCE_MS);
  }, [refreshSessionsFast]);

  // Periodically refresh PR/CI signals without hammering the API on every event.
  useEffect(() => {
    if (conn.kind !== "connected") return;
    const id = window.setInterval(() => {
      void refreshSessionsWithPr().catch(() => {});
    }, PR_REFRESH_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, [conn.kind, refreshSessionsWithPr]);

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
          // SSE snapshot: update sessions immediately without polling.
          if (isSnapshotEvent(evt)) {
            setSessions(evt.sessions);
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
        // `?pr=true` is heavier (GitHub/`gh` per session). Load fast first, enrich in background.
        const [fast] = await Promise.all([getSessions(baseUrl), refreshOrchestrators()]);
        if (cancelled) return;
        setSessions(fast);
        connectEs();
        void getSessions(baseUrl, { pr: true })
          .then((enriched) => {
            if (cancelled) return;
            setSessions(enriched);
          })
          .catch(() => {
            /* keep fast list; throttled refresh may retry */
          });
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
  }, [baseUrl, scheduleRefresh, refreshOrchestrators]);

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
      const [fast] = await Promise.all([getSessions(baseUrl), refreshOrchestrators()]);
      setSessions(fast);
      wireSseRef.current?.();
      void getSessions(baseUrl, { pr: true })
        .then(setSessions)
        .catch(() => {});
    } catch (e) {
      const msg = e instanceof Error ? e.message : "unknown error";
      setConn({ kind: "error", message: msg });
    }
  }, [baseUrl, refreshOrchestrators]);

  return {
    sessions,
    setSessions,
    orchestrators,
    conn,
    refreshSessionsFast,
    refreshSessionsWithPr,
    scheduleRefresh,
    retryConnection,
  };
}
