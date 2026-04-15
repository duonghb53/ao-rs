export type ConnectionStatus =
  | { kind: "disconnected" }
  | { kind: "connecting" }
  | { kind: "connected" }
  | { kind: "error"; message: string };

// Matches ao-rs `Session` JSON fields (snake_case) as served by ao-dashboard.
export type ApiSession = {
  id: string;
  project_id: string;
  status: string;
  activity?: string | null;
  branch: string;
  task: string;
  agent?: string;
  created_at?: number;
  runtime_handle?: string | null;
  workspace_path?: string | null;
  issue_id?: string | null;
  issue_url?: string | null;
  // Optional enrichment when calling `/api/sessions?pr=true`
  attention_level?: string;
  pr?: ApiPr | null;
};

export type ApiPr = {
  number: number;
  url: string;
  title: string;
  owner: string;
  repo: string;
  branch: string;
  base_branch: string;
  is_draft: boolean;
  state: string;
  ci_status: string;
  review_decision: string;
  mergeable: boolean;
  blockers?: string[];
};

/**
 * SSE event schema contract (from `ao-dashboard` `GET /api/events`):
 * - First message is always a snapshot: `{ type: "snapshot", sessions: ApiSession[] }`
 * - Subsequent messages are deltas from the orchestrator lifecycle loop (tagged objects with a `type` field).
 * - Server keep-alives are SSE comments and are not surfaced as messages by `EventSource`.
 */
export type SnapshotEvent = { type: "snapshot"; sessions: ApiSession[] };
export type DeltaEvent = Record<string, unknown> & { type: string };
export type ApiEvent = SnapshotEvent | DeltaEvent;

function joinUrl(baseUrl: string, path: string): string {
  return `${baseUrl.replace(/\/+$/, "")}${path}`;
}

async function httpJson<T>(url: string, init?: RequestInit): Promise<T> {
  const resp = await fetch(url, init);
  if (!resp.ok) {
    let detail = "";
    try {
      const text = await resp.text();
      if (text) {
        try {
          const parsed = JSON.parse(text) as unknown;
          if (parsed && typeof parsed === "object" && "error" in parsed) {
            const msg = (parsed as { error?: unknown }).error;
            if (typeof msg === "string") detail = msg;
          } else {
            detail = text;
          }
        } catch {
          detail = text;
        }
      }
    } catch {
      // ignore
    }
    const suffix = detail ? ` (${detail})` : "";
    throw new Error(`${init?.method ?? "GET"} ${url} failed: ${resp.status}${suffix}`);
  }
  return (await resp.json()) as T;
}

export async function getSessions(baseUrl: string, opts?: { pr?: boolean }): Promise<ApiSession[]> {
  const params = new URLSearchParams();
  params.set("all", "true");
  if (opts?.pr) params.set("pr", "true");
  return await httpJson<ApiSession[]>(joinUrl(baseUrl, `/api/sessions?${params.toString()}`));
}

export async function getSession(baseUrl: string, id: string): Promise<ApiSession> {
  return await httpJson<ApiSession>(joinUrl(baseUrl, `/api/sessions/${encodeURIComponent(id)}`));
}

export async function sendMessage(baseUrl: string, id: string, message: string): Promise<void> {
  await httpJson(joinUrl(baseUrl, `/api/sessions/${encodeURIComponent(id)}/message`), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ message }),
  });
}

export async function killSession(baseUrl: string, id: string): Promise<void> {
  await httpJson(joinUrl(baseUrl, `/api/sessions/${encodeURIComponent(id)}/kill`), {
    method: "POST",
  });
}

export async function restoreSession(baseUrl: string, id: string): Promise<ApiSession> {
  return await httpJson<ApiSession>(joinUrl(baseUrl, `/api/sessions/${encodeURIComponent(id)}/restore`), {
    method: "POST",
  });
}

export type SpawnSessionRequest = {
  project_id: string;
  repo_path: string;
  task: string;
  agent?: string;
  default_branch?: string;
  no_prompt?: boolean;
};

export async function spawnSession(baseUrl: string, req: SpawnSessionRequest): Promise<ApiSession> {
  return await httpJson<ApiSession>(joinUrl(baseUrl, "/api/sessions/spawn"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(req),
  });
}

export function connectEvents(
  baseUrl: string,
  handlers: {
    onOpen?: () => void;
    onError?: (message: string) => void;
    onEvent?: (event: ApiEvent) => void;
  },
): EventSource {
  const es = new EventSource(joinUrl(baseUrl, "/api/events"));
  es.onopen = () => handlers.onOpen?.();
  es.onerror = () => handlers.onError?.("SSE connection error");
  es.onmessage = (msg) => {
    try {
      const parsed = JSON.parse(msg.data) as ApiEvent;
      handlers.onEvent?.(parsed);
    } catch {
      // ignore
    }
  };
  return es;
}

