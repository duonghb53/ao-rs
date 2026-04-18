import { useCallback, useEffect, useRef, useState } from "react";
import { type BacklogIssue, listIssues } from "../api/client";

const POLL_INTERVAL_MS = 60_000;

type FetchState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ready"; issues: BacklogIssue[] }
  | { kind: "error"; message: string };

export function IssuesPanel({
  baseUrl,
  projectId,
  onSpawn,
  spawnDisabledReason,
}: {
  baseUrl: string;
  projectId: string | null;
  /** Called when the user clicks Spawn on an issue. The parent owns the
   *  actual `spawnSession` call (it's the one that knows `repo_path`). */
  onSpawn: (issue: BacklogIssue) => Promise<void> | void;
  /** If provided, the Spawn button is disabled and this message is shown
   *  as a hint (e.g. "open a session in this project first"). */
  spawnDisabledReason?: string | null;
}) {
  const [state, setState] = useState<FetchState>({ kind: "idle" });
  const [pending, setPending] = useState<number | null>(null);
  /** Ref instead of state so we don't re-trigger the effect on each tick. */
  const timerRef = useRef<number | null>(null);

  const refresh = useCallback(async () => {
    setState((prev) => (prev.kind === "ready" ? prev : { kind: "loading" }));
    try {
      const issues = await listIssues(baseUrl, { projectId });
      setState({ kind: "ready", issues });
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setState({ kind: "error", message });
    }
  }, [baseUrl, projectId]);

  useEffect(() => {
    let cancelled = false;

    void (async () => {
      if (cancelled) return;
      await refresh();
    })();

    timerRef.current = window.setInterval(() => {
      void refresh();
    }, POLL_INTERVAL_MS);

    return () => {
      cancelled = true;
      if (timerRef.current !== null) {
        window.clearInterval(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [refresh]);

  const handleSpawn = async (issue: BacklogIssue) => {
    setPending(issue.number);
    try {
      await onSpawn(issue);
    } finally {
      setPending((prev) => (prev === issue.number ? null : prev));
    }
  };

  return (
    <section className="panel" aria-label="Issues backlog">
      <div
        className="panel__title"
        style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 10 }}
      >
        <span>Backlog{projectId ? ` — ${projectId}` : ""}</span>
        <button
          type="button"
          className="hint"
          onClick={() => void refresh()}
          aria-label="Refresh issues"
          title="Refresh"
        >
          Refresh
        </button>
      </div>

      {state.kind === "loading" ? (
        <div className="hint" style={{ padding: 12 }}>
          Loading issues…
        </div>
      ) : null}

      {state.kind === "error" ? (
        <div className="error-banner" role="alert" style={{ margin: 8 }}>
          <span>Failed to load issues: {state.message}</span>
          <button type="button" onClick={() => void refresh()}>
            Retry
          </button>
        </div>
      ) : null}

      {state.kind === "ready" && state.issues.length === 0 ? (
        <div className="hint" style={{ padding: 16 }}>
          No open issues.
        </div>
      ) : null}

      {state.kind === "ready" && state.issues.length > 0 ? (
        <ul
          style={{
            listStyle: "none",
            margin: 0,
            padding: 0,
            display: "grid",
            gap: 0,
          }}
        >
          {state.issues.map((issue) => (
            <li
              key={`${issue.project_id}#${issue.number}`}
              style={{
                display: "grid",
                gridTemplateColumns: "1fr auto",
                alignItems: "center",
                gap: 10,
                padding: "10px 12px",
                borderTop: "1px solid var(--border-subtle)",
              }}
            >
              <div style={{ display: "grid", gap: 4, minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                  <span className="mini-pill" title={`Project: ${issue.project_id}`}>
                    {issue.project_id}
                  </span>
                  <span className="mini-pill" title={`Repo: ${issue.repo}`}>
                    {issue.repo}
                  </span>
                  <a
                    href={issue.url}
                    target="_blank"
                    rel="noreferrer"
                    className="hint mono"
                    style={{ fontSize: 11 }}
                    title={issue.url}
                  >
                    #{issue.number}
                  </a>
                </div>
                <div
                  style={{
                    fontWeight: 600,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                  title={issue.title}
                >
                  {issue.title}
                </div>
                {issue.labels.length > 0 ? (
                  <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                    {issue.labels.map((label) => (
                      <span
                        key={label}
                        className="mini-pill"
                        style={{ fontSize: 10 }}
                        title={`Label: ${label}`}
                      >
                        {label}
                      </span>
                    ))}
                  </div>
                ) : null}
              </div>
              <div style={{ display: "grid", gap: 4, justifyItems: "end" }}>
                <button
                  type="button"
                  className="primary"
                  disabled={pending === issue.number || Boolean(spawnDisabledReason)}
                  onClick={() => void handleSpawn(issue)}
                  title={spawnDisabledReason ?? "Spawn a session on this issue"}
                >
                  {pending === issue.number ? "Spawning…" : "Spawn"}
                </button>
                {spawnDisabledReason ? (
                  <span className="hint" style={{ fontSize: 10, maxWidth: 180, textAlign: "right" }}>
                    {spawnDisabledReason}
                  </span>
                ) : null}
              </div>
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
}
