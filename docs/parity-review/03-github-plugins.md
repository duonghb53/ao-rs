# GitHub plugins parity (scm + tracker)

## Verdict

**Minor drift** — core PR/merge/CI logic is faithful; the two batch paths
(individual `getMergeability` vs GraphQL batch) already disagree inside TS
itself, and Rust picks the internally-consistent option. A few genuine
gaps: webhook `timestamp` is dropped, `ScmObservation` drops several TS
batch fields (title, additions, deletions, ciChecks), and one blocker
message differs in wording. No critical bugs.

---

## scm-github

### Parity-confirmed

- `BOT_AUTHORS` — identical 10-login set
  ([lib.rs:65](/Users/haduong/study/ao-rs/crates/plugins/scm-github/src/lib.rs);
  [index.ts:45](/Users/haduong/study/agent-orchestrator/packages/plugins/scm-github/src/index.ts)).
- `map_check_state` vs `mapRawCheckStateToStatus` — same 5 buckets
  (IN_PROGRESS→running, PENDING/QUEUED/…→pending, SUCCESS→passed,
  FAILURE/TIMED_OUT/…→failed, else→skipped).
- `summarize_ci` fold precedence (failing → pending → passing → none)
  matches.
- `classify_bot_severity` substring list matches TS verbatim
  ([parse.rs:447](/Users/haduong/study/ao-rs/crates/plugins/scm-github/src/parse.rs)).
- Webhook HMAC verification, constant-time comparison, `sha256=` prefix,
  and event dispatch (pull_request, review, issue_comment with PR-only
  gating, check_run/suite, status, push) are equivalent.
- Automated-comments pagination (per_page=100, stop on short page) is
  equivalent; Rust adds `MAX_PAGES=100` safety guard.
- `mergeStateStatus` CLEAN / BEHIND / BLOCKED / UNSTABLE and `mergeable`
  MERGEABLE / CONFLICTING / UNKNOWN handling aligned in both paths.
- Merged-PR short-circuit returns all-green `MergeReadiness` in both
  ports.

### Drift

1. **`approved` computation for "no review required"** (minor /
   annotated).
   - TS `getMergeability` (`index.ts:993`): `approved = reviewDecision === "APPROVED"`
     — empty/NONE collapses to `false`.
   - TS GraphQL batch (`graphql-batch.ts:840`):
     `reviewDecision === "approved" || reviewDecision === "none"` —
     empty collapses to `true`.
   - **TS is internally inconsistent.** Rust `compose_merge_readiness`
     picks the batch semantics for both paths (`approved = empty ||
     APPROVED`), matched by a deliberate comment in
     [lib.rs:580-582](/Users/haduong/study/ao-rs/crates/plugins/scm-github/src/lib.rs).
     Principled, but any reaction that hinges on `approved == false`
     for unreviewed PRs will behave differently from ao-ts.

2. **BLOCKED blocker message wording** (cosmetic but user-visible).
   - TS: `"Merge is blocked by branch protection"` (index.ts:1012).
   - Rust: `"Branch protection requirements not satisfied"`
     (lib.rs:601).
   Downstream fingerprint/de-dup code that keys off the exact string
   will diverge.

3. **Rust-only `DIRTY` branch** in `compose_merge_readiness`
   (lib.rs:603): adds an extra blocker `"Merge is blocked (conflicts
   or failing requirements)"`. TS `getMergeability` has **no** DIRTY
   branch — GitHub treats DIRTY and CONFLICTING as overlapping, so
   Rust can emit both strings for the same PR.

4. **`ci_status` on subprocess error** (annotated, deliberate).
   - TS `getCISummary` (index.ts:689-721): if `getCIChecks` throws and
     the PR isn't merged/closed, return `"failing"` (fail-closed).
   - Rust `ci_status` (lib.rs:228-260): same guard for merged/closed,
     else **return `CiStatus::Pending`**. Comment says "a transient
     API hiccup shouldn't flip a session into the ci-failed reaction
     path and spam the agent". This silences ci-failed reactions when
     `gh` hiccups; symmetric safety vs cost tradeoff.

5. **`pending_comments` caching + fallback** (Rust-only improvement).
   Rust adds a 120 s TTL cache
   ([lib.rs:97-99](/Users/haduong/study/ao-rs/crates/plugins/scm-github/src/lib.rs)),
   a REST fallback when GraphQL fails, and GraphQL pagination (up to
   MAX_PAGES=10). TS paginates to only the first 100 threads with no
   fallback — Rust surfaces resolution status more reliably on PRs
   with >100 threads.

6. **Webhook `timestamp` field dropped.** TS `SCMWebhookEvent` carries a
   parsed timestamp (from pull_request.updated_at, review.submitted_at,
   etc.) for every event kind; Rust's `ScmWebhookEvent`
   ([scm.rs:262](/Users/haduong/study/ao-rs/crates/ao-core/src/scm.rs))
   has no timestamp field at all, and `webhook.rs` never parses one.
   Any consumer that relies on event ordering by event-claimed
   timestamp (rather than arrival) loses that signal.

7. **`MergeMethod` default** is a known, documented parity divergence:
   TS defaults to `--squash`, Rust defaults to `--merge`
   (lib.rs:644-650). Locked in by test
   `merge_method_flag_default_is_merge_commit`.

### Missing

1. **`ScmObservation` narrower than TS `PREnrichmentData`.** Rust
   batch drops `title`, `additions`, `deletions`, `hasConflicts`,
   `isBehind`, `ciChecks`, `isDraft`. Typed-schema narrowing forces
   extra REST calls for dashboard previews etc.
   [scm_transitions.rs:86](/Users/haduong/study/ao-rs/crates/ao-core/src/scm_transitions.rs).

2. **`BatchObserver` absent.** TS emits per-batch metrics via
   `observer.recordSuccess/recordFailure/log`. Rust uses
   `tracing::warn` / `debug`. Not a correctness issue, but
   downstream dashboards can't hook in the same way.

3. **Adaptive GraphQL timeout absent.** TS scales timeout by PR count
   (`30_000 + max(0, (n-10)*2000)`). Rust uses flat 30 s — large
   batches (close to `MAX_BATCH_SIZE=25`) have higher abort chance.

4. **`verifyGhCLI()` pre-flight** (TS `graphql-batch.ts:307`) absent
   in Rust. Minor — `gh`-missing error message is still actionable.

5. **TS GraphQL `ciChecks` truncation guard.** TS refuses to emit
   `ciChecks` when `contexts.pageInfo.hasNextPage` is true
   (`graphql-batch.ts:811-820`), forcing REST fallback. Rust's
   `extract_pr_enrichment` ignores `hasNextPage`, but since
   `ScmObservation` drops `ciChecks` anyway the downstream reaction
   engine re-fetches. Different architectural seam, not a regression.

---

## tracker-github

### Parity-confirmed

- `map_state` — CLOSED+NOT_PLANNED → cancelled, CLOSED → closed, else
  open — matches TS exactly.
- Parser accepts both REST `state_reason` and CLI `stateReason`
  (serde alias).
- `issue_url`/`branch_name` produce same output
  (`https://github.com/{owner}/{repo}/issues/N`, `feat/issue-N`).
- `list_issues`/`update_issue`/`create_issue` delegate to the same
  `gh` commands as TS.

### Drift

1. **`stateReason` fallback absent.** TS's `ghIssueViewJson` retries
   with a smaller field list when older `gh` doesn't know
   `stateReason` (index.ts:51-102). Rust hard-requires `gh >= 2.40`
   (documented choice); older `gh` fails where TS degrades silently.

2. **`list_issues` filter defaults.** Rust passes the filter string
   verbatim to REST (lib.rs:331-346). TS coerces `state` to
   `closed`/`all`/`open` (index.ts:224-230). Rust passes arbitrary
   values straight through — GitHub 422s on unknowns rather than
   quietly falling back to open.

3. **`updateIssue` ordering** — same semantics (state → edit →
   comment). Rust combines adds+removes into one `gh issue edit`,
   TS splits them.

4. **`is_completed` stale cache under cooldown** — Rust-only
   improvement. Keeps polling warm while `gh` is locked out.

5. **`get_issue` endpoint.** Rust: `gh api repos/.../issues/{n}`
   (REST). TS: `gh issue view <n> --json`. Same `Issue` surface.

### Missing

1. **`generatePrompt`** (TS index.ts:190-212) absent in Rust.
   Documented: prompt composition moved up into `ao-cli`.

2. **`isCompleted` minimal-payload path.** TS fetches 1 field
   (`state`); Rust fetches 2 (`state,stateReason`) to pre-populate
   the shared `get_issue` cache. Rust net-win.

3. **Issue polling / `seen-set` dedup / `spawned-by-ao` tag** — not
   in either tracker plugin; lives in lifecycle/orchestrator layer
   (`ao-core/src/lifecycle.rs`). Out of scope for this slice.

---

## Cross-cutting (rate limits, auth, caching)

### Rate-limit handling

- **Parity-confirmed.** Both ports fold primary-rate-limit (403) and
  secondary-rate-limit (429) into the same cooldown path via
  stderr-substring matching. Rust centralises this in
  [`ao_core::rate_limit`](/Users/haduong/study/ao-rs/crates/ao-core/src/rate_limit.rs),
  so scm-github and tracker-github share one 120 s cooldown instant.
  TS replicates the check per-plugin — means a rate-limit hit by the
  tracker doesn't always back the SCM off in TS, while Rust backs
  both off atomically. Rust is stricter (safer).
- `is_rate_limited_error` matches the same three substrings
  ("api rate limit", "secondary rate limit", "rate limit exceeded")
  plus "graphql: api rate limit" — Rust covers the GraphQL variant,
  TS does not. Marginal improvement.
- Rust's `enter_cooldown_for(duration)` guards against `Duration::MAX`
  overflow (lib.rs:67); TS doesn't. Non-issue unless an attacker-
  controlled upstream timestamp feeds the call.
- **Drift:** Rust does **not** key off `Retry-After` / `x-ratelimit-reset`
  headers to choose a tighter cooldown; always uses the 120 s default.
  TS also uses a fixed cooldown, so parity on this point — both share
  the same gap vs an ideal implementation.

### GraphQL batch + ETag caching

- Guard 1 (PR list ETag) and Guard 2 (commit status ETag) are
  structurally identical. LRU cache sizes match (100/500/200/200 in
  Rust vs 100/500/200 in TS; Rust adds a 4th LRU for enrichment data
  beyond TS's single `prEnrichmentDataCache`, same effective
  behaviour).
- ETag header extraction regex is equivalent (case-insensitive
  `etag:` prefix match on each line).
- 304 detection: TS checks `output.includes("HTTP/1.1 304") ||
  output.includes("HTTP/2 304")`; Rust uses `output.contains("304")`
  — **slightly looser**: a 304 substring anywhere in the response
  (including in header values or body) would count as "not
  modified". In practice `gh api -i` output won't contain a stray
  "304" elsewhere, but it's a theoretical false-negative path.
- Error path: both ports conservatively treat ETag failures as
  "assume changed" — matches.

### Auth / webhook

- HMAC verification: both constant-time.
- `raw_body` preference: both ports prefer the raw bytes over UTF-8
  decoded body for signature calculation.
- Default header names (`x-hub-signature-256`, `x-github-event`,
  `x-github-delivery`) match.

---

## Notes

- **merge-conflicts reaction dead code (issue #192)** — noted per
  prompt. ao-ts uses `session.metadata.lastMergeConflictDispatched`
  orthogonal de-dup; Rust's `status_to_reaction_key` never emits
  `merge-conflicts`. Port tracked separately.
- **Biggest real-world risk** is #6 above: webhooks lose their
  `timestamp` field, so any consumer that re-orders events by the
  event's claimed time (rather than arrival order) can't work the
  same way in Rust. If ao-rs isn't sorting events by timestamp
  anywhere today, this is latent; if it starts to, the field has to
  come back.
- **No critical bugs.** All drift is either explicitly annotated
  in Rust ("deliberate deviation from TS"), cosmetic (blocker
  wording), or a Rust-only improvement (ETag GraphQL-variant
  detection, pending-comments cache, is_completed stale fallback).
