---
phase: planning
title: agent-stuck detection — project plan
description: Task breakdown, execution order, and risks for Slice 2 Phase H.
---

# Planning — agent-stuck detection & reaction

## Milestones

- [ ] **M1: parser + tracker shape.** `parse_duration` + `TrackerState.first_triggered_at` landed and unit-tested.
- [ ] **M2: duration-based escalation live.** The reaction engine honours `EscalateAfter::Duration` end-to-end. Proven by a dedicated test in `reaction_engine.rs::tests`.
- [ ] **M3: stuck detection wired.** `LifecycleManager` tracks idle_since, runs stuck detection in `poll_one`, and flips sessions to `Stuck`. `status_to_reaction_key(Stuck)` returns `"agent-stuck"`.
- [ ] **M4: stuck exit + tracker hygiene.** `Stuck → Working` recovers cleanly, tracker clears, `idle_since` clears on terminate.
- [ ] **M5: docs + review gate.** `docs/state-machine.md`, `docs/reactions.md`, `docs/architecture.md` updated. `rust-reviewer` agent clean. `cargo fmt --check` + `cargo clippy -- -D warnings` clean. Commit and push.

## Task Breakdown

### Phase 1: Foundation (M1)

- [ ] **Task 1.1 — `parse_duration` helper**
      Add to `crates/ao-core/src/reaction_engine.rs` as a `pub(crate) fn` (location locked in Phase 3 review — not a new `duration.rs`). Match TS regex `^\d+(s|m|h)$`, return `Option<Duration>`. Unit tests: `"10s"`, `"10m"`, `"10h"`, `"0s"` (Some(ZERO)), `"10"` (None), `""` (None), `"fast"` (None), `"10m10s"` (None — TS doesn't accept compound).
      Ref: TS `lifecycle-manager.ts:45-59`.

- [ ] **Task 1.2 — Extend `TrackerState`**
      Add `first_triggered_at: Instant`. Populate in the `entry().or_insert_with(...)` site so every tracker carries a creation timestamp. Unit test: `ReactionEngine::attempts` still returns 0 for a never-triggered key; first dispatch populates `first_triggered_at`; second dispatch does **not** reset it.
      Ref: `crates/ao-core/src/reaction_engine.rs:70-77, 200-209`.

### Phase 2: Core features (M2 + M3 + M4)

- [ ] **Task 2.1 — Duration-based escalation in `dispatch` + warn-once set**
      Replace the `tracing::trace!` no-op branch at `reaction_engine.rs:224-232` with a real check: `if let Some(d) = parse_duration(s) { if tracker.first_triggered_at.elapsed() > d { escalate = true; } } else { warn_once(key) }`. Preserve the existing attempts-form path unchanged.
      Add `warned_parse_failures: Mutex<HashSet<String>>` field to `ReactionEngine` and a small `warn_once(&self, reaction_key: &str, field: &str)` helper that inserts the key and emits `tracing::warn!` on first insertion only. Used here for `escalate_after` and by `check_stuck` (via `reaction_config` accessor) for `threshold`.
      Unit tests: (a) configure `escalate_after: "1s"` + `retries: None`, dispatch twice with a `sleep(1100ms)` between — the second dispatch escalates. (b) configure `escalate_after: "garbage"` + retries: 3, dispatch 5 times — only retries gate fires, no panic, warn emits once.
      Critical: `retries: None` alone without escalate_after still means "retry forever" — don't regress Phase D semantics.

- [ ] **Task 2.2 — `status_to_reaction_key(Stuck) = Some("agent-stuck")`**
      Delete the `TODO(PhaseE)` at `reaction_engine.rs:93-97`, add the match arm. Update the doc-comment on `status_to_reaction_key` to mention it. Unit test extending the existing exhaustive-match test.

- [ ] **Task 2.3 — `LifecycleManager::idle_since` field + update helper**
      Add `idle_since: Mutex<HashMap<SessionId, Instant>>`. Initialize in `new`. Add private helper `update_idle_since(session_id, activity)`:
      - `Idle`/`Blocked` → `or_insert(Instant::now())` (preserve an older timestamp if one exists!)
      - Any other activity → `remove`.
      Call unconditionally from `poll_one` right after the "persist activity transition" block.

- [ ] **Task 2.4 — `check_stuck` helper + `poll_one` step 6 + prior-transition snapshot**
      New private `async fn check_stuck(&self, session: &mut Session)`:
      1. Early-return if `!is_stuck_eligible(session.status)`.
      2. Early-return if no `idle_since` entry (not idle).
      3. Look up `agent-stuck` reaction in the engine's config (need a new `reaction_config(key)` accessor on `ReactionEngine` — **decision: add `fn reaction_config(&self, key: &str) -> Option<&ReactionConfig>` as `pub(crate)`**).
      4. Parse `threshold`; `None` → no-op (warn-once handled in reaction engine, see Task 2.1).
      5. Compare `idle_since.elapsed()` vs parsed threshold.
      6. If over → `self.transition(session, SessionStatus::Stuck).await?`.
      Wire into `poll_one` as step 6, after `poll_scm`. **Gate step 6 on a pre-transition snapshot** (Design Decision 8): capture `let pre_transition_status = session.status;` before step 4, then only call `check_stuck` if `session.status == pre_transition_status`. Guard on `self.reaction_engine.is_some()` so the Phase C/D tests without a reaction engine don't crash.

- [ ] **Task 2.5 — `is_stuck_eligible` const fn**
      Exhaustive match on `SessionStatus`. Unit test: every variant has an explicit answer, no wildcard `_ =>`. This makes the next new `SessionStatus` variant fail the build until stuck eligibility is decided for it — same pattern as `derive_scm_status`'s exhaustiveness test.

- [ ] **Task 2.6 — Stuck → Working exit**
      Extend the existing `Spawning → Working` branch in `poll_one` step 4:
      ```rust
      if matches!(session.status, SessionStatus::Spawning | SessionStatus::Stuck)
          && matches!(activity, ActivityState::Active | ActivityState::Ready)
      { transition(Working) }
      ```
      `clear_tracker_on_transition` handles the `agent-stuck` tracker clear automatically via `status_to_reaction_key(Stuck)`.

- [ ] **Task 2.7 — Clear `idle_since` in `terminate`**
      Alongside the existing `engine.clear_all_for_session` call. Prevents memory leaks on long-running watch loops.

### Phase 3: Integration & polish (M5)

- [ ] **Task 3.1 — Integration tests (lifecycle)**
      Add to `crates/ao-core/src/lifecycle.rs::tests`:
      - `stuck_detection_fires_on_working_after_threshold`
      - `stuck_detection_fires_on_pr_open_after_threshold`
      - `stuck_recovers_to_working_on_active_activity`
      - `stuck_not_triggered_without_agent_stuck_config`
      - `stuck_not_triggered_before_threshold_elapses`
      - `stuck_eligibility_excludes_needs_input`
      Use a `MockAgent` that returns `Idle` and a `threshold: "100ms"` config. `tokio::time::sleep(150ms)` between ticks.

- [ ] **Task 3.2 — Integration test (reaction engine duration escalation)**
      `crates/ao-core/src/reaction_engine.rs::tests::duration_escalate_after_fires_on_elapsed`. No `LifecycleManager` needed — direct `engine.dispatch` calls with a sleep between.

- [ ] **Task 3.3 — Update `docs/state-machine.md`**
      New "Stuck detection (Phase H)" section. Document: the idle_since map, stuck-eligible status set, the `Stuck → Working` exit, and which trackers clear where.

- [ ] **Task 3.4 — Update `docs/reactions.md`**
      Move `agent-stuck` from the "Proposed" column to "Implemented through Phase H". Add a subsection on duration-based escalation with a yaml example.

- [ ] **Task 3.5 — Update `docs/architecture.md`**
      Move the "Duration-based `escalate-after`" bullet from "Open architecture questions" to "Answered / no longer open". One-liner pointing at Phase H.

- [ ] **Task 3.6 — Run `cargo fmt` + `cargo clippy -- -D warnings`**
      Workspace-wide. Fix any clippy lints introduced.

- [ ] **Task 3.7 — Invoke `rust-reviewer` agent**
      Per the porting-workflow feedback memory. Address blockers, re-run if they land meaningful changes.

- [ ] **Task 3.8 — Focused commit + push**
      One commit: `feat(core): Slice 2 Phase H — agent-stuck detection + duration-based escalate_after`. Push to origin, mark Phase H done in memory.

## Dependencies

```
Task 1.1 (parse_duration) ─┬─> Task 2.1 (duration escalation)
Task 1.2 (TrackerState)    ─┘
Task 2.2 (reaction key)    ─> Task 2.4 (check_stuck)
Task 2.3 (idle_since)      ─> Task 2.4 (check_stuck)
Task 2.5 (is_stuck_eligible)─> Task 2.4 (check_stuck)
Task 2.4 (check_stuck)     ─> Task 3.1 (integration tests)
Task 2.6 (stuck exit)      ─> Task 3.1
Task 2.1                   ─> Task 3.2
All code + tests           ─> Task 3.3-3.8 (docs + review + commit)
```

Concrete execution order (serial):

1. Task 1.1 (parser)
2. Task 1.2 (tracker field)
3. Task 2.1 (escalation) — finishes M2
4. Task 2.2 (reaction key)
5. Task 2.5 (stuck_eligible)
6. Task 2.3 (idle_since + update)
7. Task 2.4 (check_stuck + step 6)
8. Task 2.6 (stuck exit)
9. Task 2.7 (terminate cleanup) — finishes M3+M4
10. Task 3.1, 3.2 (tests) — verify M2/M3/M4 with real fixtures
11. Task 3.3, 3.4, 3.5 (docs) — M5 docs
12. Task 3.6 (fmt/clippy), 3.7 (review), 3.8 (commit) — M5 close

## Timeline & Estimates

Per the porting-workflow memory, each task is a focused "one clear change + its tests" unit. Rough order-of-magnitude:

| Task | Effort |
|---|---|
| 1.1 parse_duration | ~15 min |
| 1.2 TrackerState field | ~15 min |
| 2.1 duration escalation | ~30 min (includes test) |
| 2.2 reaction key | ~5 min |
| 2.3 idle_since | ~20 min |
| 2.4 check_stuck | ~45 min (includes new `reaction_config` accessor) |
| 2.5 is_stuck_eligible | ~20 min |
| 2.6 stuck exit | ~10 min |
| 2.7 terminate cleanup | ~5 min |
| 3.1 lifecycle integration tests | ~1 hour |
| 3.2 reaction engine duration test | ~20 min |
| 3.3–3.5 docs | ~45 min |
| 3.6 fmt/clippy | ~10 min |
| 3.7 rust-reviewer | ~20 min (cycle time, not wall-clock) |
| 3.8 commit + push | ~5 min |

Total: roughly a half-day of focused work, in line with Phase D/F/G.

## Risks & Mitigation

| Risk | Impact | Mitigation |
|---|---|---|
| **Test flakiness from `tokio::time::sleep`** | Intermittent CI failures on integration tests | Use `tokio::time::pause()` + `tokio::time::advance()` where possible. Where a real sleep is required (to let `Instant::elapsed` advance), use short durations (100ms) and generous margins (50% buffer). |
| **`Instant::elapsed` in tests vs. mock clocks** | Can't inject a test clock without overhauling the engine | Accept real-wall-clock in integration tests. Keep individual test durations ≤200 ms so the suite stays fast. |
| **`Stuck → Working` recovery missed if activity flips back *during* the same tick** | Session gets stuck on one tick and recovers the next before anyone notices | Acceptable — the reaction still fires once, the transition event is visible in `ao-rs watch`, and a one-tick false positive is better than silent non-detection. |
| **Duration escalation conflicts with attempts escalation when both configured** | User sees `escalate_after: 10m` but retries run out after 3 attempts first | Documented behaviour: the first gate to trigger wins. Either can fire. Add a test covering both configured — the earlier-firing gate escalates, the later is a no-op because `should_escalate` is already true. |
| **Config churn if Task 2.4 needs a new accessor** | Shape of `ReactionEngine` public API changes | `reaction_config(key) -> Option<&ReactionConfig>` is purely additive; no existing callers break. Low-risk extension. |
| **`parse_duration` silently no-oping on malformed threshold** | User misconfigures `threshold: "ten minutes"` and nothing happens | Log a one-shot `tracing::warn!` on first use per (session, reaction) pair. Follows the same pattern the engine uses for "`send-to-agent` without a message". |
| **Regression of Phase G retry loop** | Accidentally parking `MergeFailed` sessions via the stuck path | `is_stuck_eligible` explicitly excludes `MergeFailed`. Add a test that ensures a `MergeFailed` session with idle activity does NOT transition to `Stuck`. |

## Resources Needed

- **TS reference.** `~/study/agent-orchestrator/packages/core/src/lifecycle-manager.ts` lines 45-59 (parseDuration), 340-351 (isIdleBeyondThreshold), 370-560 (determineStatus with stuck checks at 4b and 5).
- **Existing Rust code to study.** `reaction_engine.rs` tracker accounting path (lines 200-270), `lifecycle.rs::poll_one` (215-297), `lifecycle.rs::terminate` (392-404), `clear_tracker_on_transition` for the cross-transition rules.
- **rust-reviewer agent.** Per porting workflow — gates every phase commit.
- **Tools.** `cargo fmt`, `cargo clippy`, `cargo test -p ao-core` (single-crate fast iteration), full `cargo test` before commit.

## Exit Criteria

Phase H is done when:

1. All tasks in the breakdown are checked.
2. `cargo test` passes on the worktree.
3. `cargo fmt --check` + `cargo clippy -- -D warnings` clean.
4. `rust-reviewer` agent returns no blockers.
5. One focused commit exists on `feature-agent-stuck-detection`.
6. The `ao-rs port state` memory entry is updated to reflect Phase H done + Phase I plan stub (or "port is paused" — decide at commit time).
