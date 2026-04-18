# Lifecycle + SCM transitions parity

## Verdict
Significant drift — core ladder is correct but several reaction-dispatch features from ao-ts were never ported.

## Parity-confirmed
- **SCM status ladder priority order.** Rust `derive_scm_status` (`crates/ao-core/src/scm_transitions.rs:167-201`) walks the same rungs as TS `determineStatus` step 4 (`packages/core/src/lifecycle-manager.ts:495-564`): Merged/Closed → Mergeable (readiness) → CiFailed → ChangesRequested → Approved → ReviewPending → PrOpen. The internal comment even calls out the deliberate choice to match TS by putting `ci_failed` above `changes_requested`.
- **No-PR fallback to Working.** Both drop any PR-track status back to `Working` when `detect_pr` returns `None` (`scm_transitions.rs:154-163` vs. TS implicit via step 2 `working` fallback at `lifecycle-manager.ts:577-584`).
- **ReviewDecision::None "no reviewers required".** Both treat absent review decision as effectively approved for ready-check purposes (Rust via `MergeReadiness::is_ready()` collapsing to readiness rung; TS `lifecycle-manager.ts:512-518`).
- **Reaction tracker lifecycle.** Increment-before-dispatch, clear-on-status-exit, per-(session,key) keying, and escalate-after for both attempts and duration forms all match (`reaction_engine.rs:94-114, 455-494, 535-554` vs. TS `lifecycle-manager.ts:595-640`). `parse_duration` uses the same `^\d+(s|m|h)$` regex shape with `None` short-circuit equivalent to TS's `0` return.
- **Merge-conflicts dispatch path.** Rust has a full port (`lifecycle.rs:1086-1167`), including the eligibility gate (`pr_open` through `mergeable`) and the `last_merge_conflict_dispatched` flag that mirrors TS `lastMergeConflictDispatched` (`lifecycle-manager.ts:1093-1188`). Contrary to the task brief's "known dead code" aside, it is wired and called from `poll_scm` step 5.
- **Session pruning on termination.** Rust `terminate` clears `reaction_engine.clear_all_for_session`, `idle_since`, and `last_review_backlog_check` (`lifecycle.rs:832-844`), matching TS's `pollAll` prune-by-id pass (`lifecycle-manager.ts:1348-1366`).

## Drift (severity: HIGH/MED/LOW)

### [HIGH] No automated/bugbot review comment dispatch
- Rust: `reaction_engine.rs:132-140` — `status_to_reaction_key` has no entry; no `maybeDispatchReviewBacklog` analogue anywhere in the crate.
- TS: `packages/core/src/lifecycle-manager.ts:140-163` (`eventToReactionKey` maps `automated_review.found → "bugbot-comments"`); `lifecycle-manager.ts:758-932` implements the dispatch with fingerprint + dedup.
- Behavior difference: TS fetches `scm.getAutomatedComments` and `scm.getPendingComments` per tick (throttled 2 min), fingerprints by comment ID, and re-dispatches the `changes-requested` / `bugbot-comments` reactions whenever the fingerprint changes. Rust tracks only the time-throttle and never consumes the comment lists — new review comments after the initial `changes_requested` transition never trigger a follow-up send.
- Impact: Agents stop getting new review feedback after the first tick. A reviewer who leaves three comments across ten minutes only sees the first batch forwarded.
- Fix: Port `maybeDispatchReviewBacklog` into `lifecycle.rs`; add `"bugbot-comments"` to `status_to_reaction_key` is wrong (it's not status-driven), so the helper should call `ReactionEngine::dispatch` directly like the merge-conflicts helper already does, with `Session` flag fields `last_pending_review_*` and `last_automated_review_*` persisted by `SessionManager`.

### [HIGH] No `summary.all_complete` / `all-complete` reaction trigger
- Rust: `lifecycle.rs:327-439` (`tick`) — no global "are all sessions terminal?" check; no `all_complete_emitted` guard anywhere.
- TS: `lifecycle-manager.ts:1368-1383` — once per poll cycle, if every session is terminal, fires the `all-complete` reaction (`system`, `all` pseudo-IDs) gated by `allCompleteEmitted` so it only fires once.
- Behavior difference: Rust configures `"all-complete"` in `config.rs:1028` and even assigns it a default priority (`reactions.rs:115`), but no code path ever dispatches it.
- Impact: Users who configure `all-complete: action: notify` get no notification when their watch loop drains. Silent feature loss.
- Fix: After the session loop in `tick`, when `sessions.len() > 0 && sessions.iter().all(|s| s.is_terminal())`, call `engine.dispatch_by_key("all-complete")` and latch a `Mutex<bool>` that resets the moment any non-terminal session reappears.

### [HIGH] No detailed CI-failure message dispatch
- Rust: no equivalent of `maybeDispatchCIFailureDetails` / `formatCIFailureMessage`.
- TS: `lifecycle-manager.ts:938-1085` — on `ci_failed` ticks, fetches `scm.getCIChecks`, fingerprints the failed-check set by `name:status:conclusion`, and sends the agent a formatted `**check**: status — url` bulleted message. Tracks `lastCIFailureFingerprint` / `lastCIFailureDispatchHash` so it re-dispatches only when the failure set changes.
- Behavior difference: Rust's `ci-failed` reaction fires with whatever `message:` the user wrote in config (or defaults to a generic boilerplate). The agent never sees the failing check names or log links.
- Impact: Agents have to run `gh` themselves to find what failed. Reaction quality is materially worse than TS.
- Fix: Port the helper. Needs a new `Scm::ci_checks(&PullRequest) -> Result<Vec<CiCheck>>` trait method (the `CICheck` type already exists in `scm.rs`). Then call it from `poll_scm` after the derive-status transition when `next == CiFailed`.

### [MED] `agent-needs-input` and `agent-exited` reactions never fire
- Rust: `reaction_engine.rs:132-140` — only `CiFailed`, `ChangesRequested`, `Mergeable`, `Stuck` map to keys.
- TS: `lifecycle-manager.ts:140-163` — `session.needs_input → "agent-needs-input"`, `session.killed → "agent-exited"`.
- Behavior difference: Rust transitions the session to `NeedsInput` (`lifecycle.rs:495-498`) and emits `StatusChanged`, but because the status-to-key table has no entry, the configured reaction never runs. Same for `Killed` via `terminate`.
- Impact: Users can't auto-notify on `needs_input` or on runtime death, both of which are documented reaction keys with explicit defaults in `reactions.rs:116`.
- Fix: Add `SessionStatus::NeedsInput => Some("agent-needs-input")` and `SessionStatus::Killed => Some("agent-exited")` to `status_to_reaction_key`. Note that `Killed` dispatch must run from `terminate` before `clear_all_for_session`, or the tracker will be wiped before the reaction gets its first attempt.

### [MED] `approved` status has no `review.approved` / action-priority notification
- Rust: `reaction_engine.rs:132-140` — `Approved` is not in the table.
- TS: `lifecycle-manager.ts:108-137` maps `approved → review.approved` then `inferPriority` at `:74-77` elevates it to `action`. `checkSession` always calls `notifyHuman` for untrapped transitions (`lifecycle-manager.ts:1282-1291`), so `approved` triggers an `action`-priority notification even without a configured reaction.
- Behavior difference: Rust emits `StatusChanged(_, Approved)` on the broadcast channel but no notifier is invoked. Only statuses with a configured reaction + `Notify` action reach the registry.
- Impact: The "human action required" channel (default `desktop` in the recommended config) never fires on approvals. CI-green-plus-approved PRs sit silent until the next poll flips them to `Mergeable`.
- Fix: Optional design decision — either add `Approved → Some("review-approved")` to the table (introducing a new reaction key), or add a fallback path in `transition` that notifies on every `StatusChanged` whose priority ≥ `Action` when no reaction consumed the event. TS chose the latter; the Rust engine structure would prefer the former.

### [LOW] Stuck detection won't fire during `Spawning`
- Rust: `is_stuck_eligible` at `lifecycle.rs:1406-1448` excludes `Spawning`.
- TS: `lifecycle-manager.ts:577-584` — a `spawning` session whose activity is idle beyond threshold still reaches the `stuck` return via step 2's fallback branches.
- Behavior difference: A hung spawn (agent process started but never emits activity) is classified `Working` by Rust then stays there until `check_stuck` eligibility, whereas TS reports stuck sooner.
- Impact: Small; the transition `Spawning → Working` triggers on first activity anyway. Worth logging as a known parity gap.
- Fix: Decide whether a stuck-on-spawn should be `Stuck` or `Errored`. Current Rust comment in `is_stuck_eligible` calls the exclusion intentional.

## Missing (feature in TS, absent in Rust)
- `maybeDispatchReviewBacklog` (HIGH above) — pending + automated comment dispatch.
- `maybeDispatchCIFailureDetails` (HIGH above) — failed-check formatting.
- `summary.all_complete` reaction dispatch (HIGH above).
- `pinnedSummary` first-quality-summary pin at `lifecycle-manager.ts:1299-1312`. Rust has no equivalent; agent summaries aren't pinned for title stability.
- `reaction.escalated` event fired *and* notified — Rust emits `ReactionEscalated` but doesn't also send a notification with `urgent` priority the way TS does (`lifecycle-manager.ts:626-640`).

## Notes
- Poll interval: TS 30 s vs. Rust 10 s (`lifecycle.rs:67`), justified by batch enrichment. Not a parity bug.
- Re-entrancy: Rust uses `MissedTickBehavior::Skip` rather than TS's `polling` boolean; functionally equivalent.
- `MergeFailed` parking (Phase G) is Rust-only. Its carve-out in `clear_tracker_on_transition` is correctly isolated.
