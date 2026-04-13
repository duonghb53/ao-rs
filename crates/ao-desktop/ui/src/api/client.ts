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
};

export type ApiEvent = Record<string, unknown> & { type?: string };

function joinUrl(baseUrl: string, path: string): string {
  return `${baseUrl.replace(/\/+$/, "")}${path}`;
}

async function httpJson<T>(url: string, init?: RequestInit): Promise<T> {
  const resp = await fetch(url, init);
  if (!resp.ok) {
    throw new Error(`${init?.method ?? "GET"} ${url} failed: ${resp.status}`);
  }
  return (await resp.json()) as T;
}

export async function getSessions(baseUrl: string): Promise<ApiSession[]> {
  return await httpJson<ApiSession[]>(joinUrl(baseUrl, "/api/sessions"));
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

