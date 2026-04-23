import { useMemo, useState, type CSSProperties, type ReactNode } from "react";
import type { DashboardSession } from "../lib/types";
import { getDashboardLane, isTerminalSession } from "../lib/types";
import { formatCiStatus, formatReviewDecision, getSessionTitle } from "../lib/format";
import { projectAccentStyle } from "../lib/projectColors";
import { getSessionRepoUrl } from "../lib/repoUrl";

function formatElapsed(unixSeconds: number | null | undefined): string {
  if (!unixSeconds || !Number.isFinite(unixSeconds)) return "-";
  const ms = Date.now() - unixSeconds * 1000;
  if (ms < 60_000) return "just now";
  const m = Math.floor(ms / 60_000);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  const rh = h - d * 24;
  return rh ? `${d}d ${String(rh).padStart(2, "0")}h` : `${d}d`;
}

function statusPill(session: DashboardSession): { className: string; label: string } {
  const activity = (session.activity ?? "").toLowerCase();
  const status = (session.status ?? "").toLowerCase();
  if (isTerminalSession(session)) return { className: "pill", label: status || "done" };
  if (activity === "active") return { className: "pill active", label: "active" };
  if (activity === "waiting_input") return { className: "pill", label: "waiting" };
  if (status === "errored") return { className: "pill error", label: "error" };
  return { className: "pill idle", label: status || activity || "idle" };
}

export function SessionDetail({
  session,
  terminalSlot,
}: {
  session: DashboardSession;
  terminalSlot?: ReactNode;
}) {
  const lane = getDashboardLane(session);
  const title = getSessionTitle(session);
  const projectAccent = useMemo(() => projectAccentStyle(session.projectId), [session.projectId]);
  const repoUrl = useMemo(() => getSessionRepoUrl(session), [session]);

  const ci = session.pr?.ciStatus ? formatCiStatus(session.pr.ciStatus) : null;
  const review = session.pr?.reviewDecision ? formatReviewDecision(session.pr.reviewDecision) : null;
  const sp = statusPill(session);

  const blockers = session.pr?.blockers ?? [];
  const ciChecks = session.pr?.ciChecks ?? [];
  const passedChecks = ciChecks.filter((c) => c.status === "passed" || c.status === "skipped").length;
  const terminal = isTerminalSession(session);
  const [railCollapsed, setRailCollapsed] = useState<{ session: boolean; lifecycle: boolean; actions: boolean }>({
    session: false,
    lifecycle: false,
    actions: false,
  });

  return (
    <div
      className="page"
      style={{ height: "calc(100vh - 92px)", overflow: "hidden", alignItems: "stretch" } as CSSProperties}
    >
      <div
        className="main-col"
        style={{ minHeight: 0, height: "100%" } as CSSProperties}
      >
        <div
          className="main-col__scroll"
          style={{ overflowY: "auto", display: "flex", flexDirection: "column", gap: 14, flex: 1, minHeight: 0 }}
        >
        <section className="sess-head" data-tone={lane} style={projectAccent as CSSProperties}>
          <div className="row1">
            <h1 title={title}>{title}</h1>
            <span className={sp.className}>
              <span className="dot" aria-hidden="true" />
              {sp.label}
            </span>
            {session.pr ? (
              <span className="pill open">
                <span className="dot" aria-hidden="true" />
                PR #{session.pr.number}
              </span>
            ) : session.claimedPrNumber ? (
              <span className="pill">
                <span className="dot" aria-hidden="true" />
                PR #{session.claimedPrNumber}
              </span>
            ) : null}
            <span className="spacer" />
            {session.agent ? <span className="pill">{session.agent}</span> : null}
          </div>

          {session.summary && !session.summaryIsFallback ? (
            <p className="sess-desc">{session.summary}</p>
          ) : null}

          <div className="tags">
            {session.projectId ? (
              repoUrl ? (
                <a className="tag" href={repoUrl} target="_blank" rel="noreferrer" title={repoUrl}>
                  {session.projectId}
                </a>
              ) : (
                <span className="tag">{session.projectId}</span>
              )
            ) : null}
            {session.branch ? <span className="tag branch">{session.branch}</span> : null}
            {session.pr ? (
              <a className="tag pr" href={session.pr.url} target="_blank" rel="noreferrer">
                #{session.pr.number}
              </a>
            ) : session.claimedPrNumber ? (
              session.claimedPrUrl ? (
                <a className="tag pr" href={session.claimedPrUrl} target="_blank" rel="noreferrer">
                  #{session.claimedPrNumber}
                </a>
              ) : (
                <span className="tag pr">#{session.claimedPrNumber}</span>
              )
            ) : null}
            {session.issueId ? (
              session.issueUrl ? (
                <a className="tag" href={session.issueUrl} target="_blank" rel="noreferrer">
                  {session.issueId}
                </a>
              ) : (
                <span className="tag">{session.issueId}</span>
              )
            ) : null}
          </div>

          <div className="meta-row">
            <span>
              <b>lane</b> {lane}
            </span>
            <span>
              <b>status</b> {session.status ?? "-"}
            </span>
            {session.activity ? (
              <span>
                <b>activity</b> {session.activity}
              </span>
            ) : null}
            <span>
              <b>id</b> <span className="mono">{session.id.slice(0, 12)}</span>
            </span>
          </div>
        </section>

        {session.pr ? (
          <section className="pr-card">
            <div className="pr-head">
              <h3 className="pr-title">
                <a href={session.pr.url} target="_blank" rel="noreferrer">
                  {session.pr.title || `PR #${session.pr.number}`}
                </a>
              </h3>
              <div className="pr-diff">
                {typeof session.pr.additions === "number" && typeof session.pr.deletions === "number" ? (
                  <>
                    <span className="plus">+{session.pr.additions}</span>{" "}
                    <span className="minus">-{session.pr.deletions}</span>
                    <span className="sep-s">·</span>
                  </>
                ) : null}
                {session.pr.baseBranch ? (
                  <>
                    base <b>{session.pr.baseBranch}</b>
                  </>
                ) : null}
                {session.pr.branch ? (
                  <>
                    <span className="sep-s">·</span>branch <b>{session.pr.branch}</b>
                  </>
                ) : null}
                {typeof session.pr.isDraft === "boolean" ? (
                  <>
                    <span className="sep-s">·</span>
                    {session.pr.isDraft ? "draft" : "ready"}
                  </>
                ) : null}
              </div>
            </div>
            {blockers.length > 0 ? (
              <div className="pr-section">
                <div className="sec-label">Blockers</div>
                {blockers.map((b) => (
                  <div key={b} className="blocker-row">
                    {b}
                  </div>
                ))}
              </div>
            ) : null}
            {ci || review || typeof session.pr.mergeable === "boolean" ? (
              <div className="pr-section">
                <div className="sec-label">
                  Checks
                  {ciChecks.length > 0 ? (
                    <>
                      {" "}
                      — {passedChecks} passed
                    </>
                  ) : null}
                </div>
                <div className="checks">
                  {ci ? (
                    <span
                      className="check"
                      data-state={ci.tone === "ok" ? "pass" : ci.tone === "bad" ? "fail" : "pending"}
                    >
                      {ci.label}
                    </span>
                  ) : null}
                  {review ? (
                    <span
                      className="check"
                      data-state={review.tone === "ok" ? "pass" : review.tone === "bad" ? "fail" : "pending"}
                    >
                      {review.label}
                    </span>
                  ) : null}
                  {typeof session.pr.mergeable === "boolean" ? (
                    <span className="check" data-state={session.pr.mergeable ? "pass" : "fail"}>
                      {session.pr.mergeable ? "mergeable" : "not mergeable"}
                    </span>
                  ) : null}
                  {ciChecks.map((c) =>
                    c.url ? (
                      <a
                        key={c.name}
                        className="check"
                        data-state={c.status === "passed" || c.status === "skipped" ? "pass" : c.status === "failed" ? "fail" : "pending"}
                        href={c.url}
                        target="_blank"
                        rel="noreferrer"
                      >
                        {c.name}
                      </a>
                    ) : (
                      <span
                        key={c.name}
                        className="check"
                        data-state={c.status === "passed" || c.status === "skipped" ? "pass" : c.status === "failed" ? "fail" : "pending"}
                      >
                        {c.name}
                      </span>
                    )
                  )}
                </div>
              </div>
            ) : null}
          </section>
        ) : null}
        </div>

        {terminalSlot && !terminal ? (
          <div style={{ flexShrink: 0, display: "flex", flexDirection: "column", gap: 6 }}>
            <div className="term-label">│ terminal</div>
            {terminalSlot}
          </div>
        ) : null}
      </div>

      <aside className="rail">
        <section className="rail-card">
          <button
            type="button"
            className="rail-card__head"
            onClick={() => setRailCollapsed((p) => ({ ...p, session: !p.session }))}
            aria-expanded={!railCollapsed.session}
            title={railCollapsed.session ? "Expand" : "Collapse"}
          >
            <h4>session</h4>
            <span className="rail-card__caret" aria-hidden="true" data-collapsed={String(railCollapsed.session)}>
              ▾
            </span>
          </button>
          <div className="rail-list" hidden={railCollapsed.session}>
            <div className="kv">
              <span>agent</span>
              <b>{session.agent ?? "-"}</b>
            </div>
            {session.createdAt ? (
              <div className="kv">
                <span>started</span>
                <b>{formatElapsed(session.createdAt)} ago</b>
              </div>
            ) : null}
            <div className="kv">
              <span>project</span>
              <b>{session.projectId}</b>
            </div>
            <div className="kv">
              <span>status</span>
              <b>{session.status ?? "-"}</b>
            </div>
            <div className="kv">
              <span>activity</span>
              <b>{session.activity ?? "-"}</b>
            </div>
            {session.branch ? (
              <div className="kv">
                <span>branch</span>
                <b>{session.branch}</b>
              </div>
            ) : null}
            {session.issueId ? (
              <div className="kv">
                <span>issue</span>
                <b>{session.issueId}</b>
              </div>
            ) : null}
            <div className="kv">
              <span>id</span>
              <b title={session.id}>{session.id.slice(0, 12)}</b>
            </div>
          </div>
        </section>

        <section className="rail-card">
          <button
            type="button"
            className="rail-card__head"
            onClick={() => setRailCollapsed((p) => ({ ...p, lifecycle: !p.lifecycle }))}
            aria-expanded={!railCollapsed.lifecycle}
            title={railCollapsed.lifecycle ? "Expand" : "Collapse"}
          >
            <h4>lifecycle</h4>
            <span className="rail-card__caret" aria-hidden="true" data-collapsed={String(railCollapsed.lifecycle)}>
              ▾
            </span>
          </button>
          <div className="timeline" hidden={railCollapsed.lifecycle}>
            <div className="tl-item">
              <span className="t">{formatElapsed(session.createdAt)}</span>
              <span className="d">
                <b>spawn</b> · {session.projectId}
              </span>
            </div>
            {session.branch ? (
              <div className="tl-item">
                <span className="t">—</span>
                <span className="d">
                  <b>branch</b> {session.branch}
                </span>
              </div>
            ) : null}
            {session.pr ? (
              <div className="tl-item">
                <span className="t">—</span>
                <span className="d">
                  <b>pr</b> opened #{session.pr.number}
                </span>
              </div>
            ) : session.claimedPrNumber ? (
              <div className="tl-item">
                <span className="t">—</span>
                <span className="d">
                  <b>pr</b> #{session.claimedPrNumber}
                </span>
              </div>
            ) : null}
            {ci ? (
              <div className="tl-item">
                <span className="t">—</span>
                <span className="d">
                  <b>ci</b> {ci.label}
                </span>
              </div>
            ) : null}
            <div className={`tl-item${terminal ? "" : " active"}`}>
              <span className="t">now</span>
              <span className="d">
                <b>{session.status ?? "-"}</b>
                {session.activity ? ` · ${session.activity}` : ""}
              </span>
            </div>
          </div>
        </section>

        <section className="rail-card">
          <button
            type="button"
            className="rail-card__head"
            onClick={() => setRailCollapsed((p) => ({ ...p, actions: !p.actions }))}
            aria-expanded={!railCollapsed.actions}
            title={railCollapsed.actions ? "Expand" : "Collapse"}
          >
            <h4>quick actions</h4>
            <span className="rail-card__caret" aria-hidden="true" data-collapsed={String(railCollapsed.actions)}>
              ▾
            </span>
          </button>
          <div className="rail-list" style={{ gap: 6 }} hidden={railCollapsed.actions}>
            <button type="button" className="btn" disabled title="Not yet available">
              ↻ rebase on main
            </button>
            <button type="button" className="btn" disabled title="Not yet available">
              ◐ request review
            </button>
            <button type="button" className="btn" disabled title="Not yet available">
              ✎ post comment
            </button>
            <button type="button" className="btn" disabled title="Not yet available">
              ⏏ close session
            </button>
            {session.pr ? (
              <a className="btn" href={session.pr.url} target="_blank" rel="noreferrer">
                open PR
              </a>
            ) : session.claimedPrUrl ? (
              <a className="btn" href={session.claimedPrUrl} target="_blank" rel="noreferrer">
                open PR
              </a>
            ) : null}
            {session.issueUrl ? (
              <a className="btn" href={session.issueUrl} target="_blank" rel="noreferrer">
                open issue
              </a>
            ) : null}
          </div>
        </section>
      </aside>
    </div>
  );
}
