---
phase: requirements
title: agent-stuck detection & reaction (Slice 2 Phase H)
description: Detect idle-beyond-threshold sessions, flip them to `Stuck`, and fire the `agent-stuck` reaction.
---

# Requirements — agent-stuck detection & reaction

## Problem Statement

The Rust port's reaction engine can already fire `ci-failed`,
`changes-requested`, and `approved-and-green` reactions. What it
**cannot** do is notice a session that has stopped making progress on
its own.

Symptoms a human sees today without stuck detection:

- An agent writes code, hits an unexpected edge case, emits a
  "hmm, I'm not sure what to do here" message, and goes `Idle`. The
  lifecycle loop observes `ActivityChanged(active → idle)` and
  persists it. That's the end of the story — the session sits in
  `Working` / `Idle` forever. No notification fires, no `stuck`
  status, no escalation.
- Same thing with a `PrOpen` session: agent pushed a PR, CI is
  queued, reviewer hasn't looked, and the agent has gone home. The
  session looks fine-ish in `ao-rs status`, but no human is told
  that the PR has been sitting for hours with no forward motion.

Concretely, the TODO at `crates/ao-core/src/reaction_engine.rs:93`
has been flagging this gap since Phase D:

```rust
// TODO(PhaseE): add Stuck → "agent-stuck" and Errored → "agent-errored".
// agent-stuck needs auxiliary state (time entered Idle) that the
// engine doesn't track today — the pure status-to-key mapping will
// work, but the engine side needs a `status_entered_at` tracker.
```

Phase H closes that gap.

**Who is affected?** The single operator running `ao-rs watch` on
their laptop (learning-port scope). No "mass" users.

**Current workaround?** Manually `tmux attach` and eyeball each
session. Doesn't scale past 2-3 sessions, which is exactly the
count this project is designed for.

## Goals & Objectives

### Primary goals

1. Detect that a session has been "idle-ish" continuously for longer
   than a configurable threshold (default: the `agent-stuck`
   reaction's `threshold` field, e.g. `"10m"`).
2. When detected, transition the session's `SessionStatus` to
   `Stuck` and fire the `agent-stuck` reaction through the existing
   `ReactionEngine::dispatch` path.
3. Allow a stuck session to recover: when the agent starts acting
   again, flip `Stuck` back to `Working` (matching the TS
   reference). Stuck is not terminal.
4. Wire **duration-based `escalate_after`** (`escalate_after: 10m`)
   into the reaction engine — `agent-stuck` is the first reaction
   that actually needs wall-clock escalation, and the parser was
   deliberately deferred until this phase (see
   `docs/architecture.md#open-architecture-questions`).

### Secondary goals

- Document the new transition in `docs/state-machine.md` and the
  new reaction wiring in `docs/reactions.md`.
- Keep the reaction delivery as `tracing::warn!` + emit
  `ReactionTriggered` on the broadcast channel. No new `Notifier`
  trait — that's Slice 3.

### Non-goals

- **Real `Notifier` plugin.** Slack / desktop / email delivery
  stays out of scope. `Notify` fires on the broadcast channel and
  `ao-rs watch` prints it to stdout — same contract as every other
  reaction today.
- **JSONL introspection.** The TS reference reads the agent's own
  session files to get a precise "idle since" timestamp. The Rust
  port will instead use "first tick we observed Idle/Blocked
  activity" as an approximation. Intentional divergence — see
  design doc.
- **Persistent idle-since state.** The `idle_since` map lives in
  memory inside `LifecycleManager`. A `ao-rs watch` restart resets
  it. TS does the same (the tracker is rebuilt from disk on boot,
  but the per-session idle clock is in-process).
- **Per-project stuck thresholds.** Slice 2 has no project-level
  reaction overrides yet (the global `reactions:` map is the only
  source). This phase uses whatever the global `agent-stuck`
  threshold is; per-project merge lands later if ever.
- **`agent-errored` reaction.** The TODO mentioned both. This
  phase only does `agent-stuck`.

## User Stories & Use Cases

1. **Idle background coder.** *As the operator*, I want a session
   that has gone idle for 10 minutes with no PR opened to flip to
   `stuck` and print a warning row in `ao-rs watch`, so I can
   attach and unblock it.
2. **PR that lost its reviewer.** *As the operator*, I want a
   session sitting at `pr_open` with idle activity for 30 minutes
   to also flip to `stuck`, so I don't have to spot-check every
   open PR.
3. **Recovery.** *As the operator*, when I attach and the agent
   starts working again, the session flips back to `working` on
   the next tick without me having to `ao-rs session restore`.
4. **Escalation.** *As the operator*, if I don't respond to a
   `Notify` for the configured `escalate_after: 10m`, the engine
   re-fires with escalation marker so I see it twice (still via
   `tracing::warn!` / broadcast event, but marked `escalated`).
5. **No-config means no check.** *As the operator*, if I haven't
   put `agent-stuck` in my `~/.ao-rs/config.yaml`, the feature is a
   no-op. No accidental status flips from a default threshold.

### What the operator sees in `ao-rs watch`

When a session goes stuck, the existing event printer (see
`crates/ao-cli/src/main.rs:700-750`) emits two rows in order, no
CLI changes needed:

```
3a4b5c6d   status_changed       working → stuck
3a4b5c6d   reaction_fired       agent-stuck → notify
```

On recovery:

```
3a4b5c6d   activity_changed     idle → active
3a4b5c6d   status_changed       stuck → working
```

If duration-based `escalate_after` trips (reaction was `notify`,
not responded to, wall-clock elapsed), a third row appears:

```
3a4b5c6d   reaction_escalated   agent-stuck (2 attempts)
3a4b5c6d   reaction_fired       agent-stuck → notify
```

Phase H ships zero new row shapes — the existing `reaction_fired`
and `reaction_escalated` variants already print cleanly.

## Success Criteria

Acceptance:

- [ ] `status_to_reaction_key(Stuck)` returns `Some("agent-stuck")`
      — the TODO at `reaction_engine.rs:93` is gone.
- [ ] A `LifecycleManager` integration test with a `MockAgent` that
      reports `Idle` and a 1-second `agent-stuck` threshold flips
      the session to `Stuck` on the tick after threshold expiry and
      emits `ReactionTriggered { key: "agent-stuck", action: notify }`.
- [ ] Same test but activity returns to `Active` — session flips
      back to `Working` on the next tick.
- [ ] Stuck detection fires for both non-PR sessions (`Working`)
      and PR-track sessions (`PrOpen`, `CiFailed`, etc.). A table
      test enumerates which statuses are "stuck-eligible".
- [ ] Duration-based `escalate_after: "1s"` triggers escalation in
      a unit test (wall-clock elapsed > parsed duration on the
      second attempt).
- [ ] Threshold parser accepts `Ns` / `Nm` / `Nh` for any
      non-negative `N` (including `0s` — see "Constraints" below).
      Garbage input (`"fast"`, `"10"`, empty, `"1m30s"` compound)
      becomes a no-op with a one-shot `tracing::warn!` on first
      use per reaction — matching how TS's `parseDuration` returns
      0 and short-circuits.
- [ ] No `agent-stuck` config → no behaviour change vs. Phase G.
      Covered by keeping all Phase G tests green.
- [ ] `cargo fmt --check` clean, `cargo clippy -- -D warnings`
      clean in the whole workspace.
- [ ] `rust-reviewer` gate passes.

Performance:

- No measurable overhead on ticks when no session is stuck.
  Insert/lookup on `HashMap<SessionId, Instant>` is O(1) and the
  per-tick cost is bounded by session count (dozens, not
  thousands). Matches the existing tracker map cost profile.

## Constraints & Assumptions

- **Disk is the source of truth** (architecture principle #2).
  Stuck is derived from runtime state; the transition is persisted
  via `SessionManager::save` like every other status change.
- **Trait objects at plugin boundaries** (#3). The `Agent` trait
  is not touched — we do not add a "return idle timestamp" method.
  `LifecycleManager` tracks idle entry in its own state.
- **Shell-out over libraries** (#1). Irrelevant here — no new
  subprocess calls.
- The TS reference uses a trivial regex for duration parsing
  (`^\d+(s|m|h)$`). Rust matches it — no need for `humantime` as
  a dep just to parse `"10m"`.
- `Stuck` is a non-terminal, non-PR-track status. `tick()` will
  still poll it. Exit is via the activity path in `poll_one`, not
  `derive_scm_status`.
- **`ActivityState::Blocked` counts as idle for stuck detection,
  not only `Idle`.** Semantically `Blocked` means "agent hit an
  error or got stuck" — exactly the condition the reaction is
  for. Matches TS (`state === "idle" || state === "blocked"`
  triggers `detectedIdleTimestamp` in
  `lifecycle-manager.ts:414`). Listed here rather than in the
  design doc because it affects which sessions fire user-visible
  reactions — not a hidden implementation detail.
- **Minimum `threshold` is whatever the parser accepts — no
  clamping, no floor.** `threshold: "0s"` will fire on the first
  idle-activity tick the session observes. `threshold: "1s"` with
  the default 5-second poll interval effectively fires on the
  next tick after entering idle. We do **not** clamp to poll
  interval because (a) the parser is the honest contract, and
  (b) users configuring sub-poll-interval thresholds will see
  surprising behaviour either way — clamping hides it, rejection
  helps catch typos but also catches legitimate test fixtures.
- **`ao-rs watch` restart resets the idle clock.** This is the
  user-observable consequence of the non-goal "persistent
  idle-since state". A session that was idle for 9 minutes under
  a `threshold: 10m` reaction, when the operator stops and
  restarts `ao-rs watch`, will take another full 10 minutes of
  continuous idle activity after restart before stuck fires.
  Accepted trade-off — matches TS (which rebuilds its tracker
  from disk on boot, but the idle timestamp itself is
  in-process). Documented here so the operator isn't surprised.

## Questions & Open Items

All originally-open questions have been resolved. Summary here so a
reader who only looks at this file knows what was decided:

- **`ActivityState::Blocked` counts as idle.** → Constraint
  section above.
- **Minimum stuck-eligible status set.** → Design doc. Excludes
  `Spawning`, `NeedsInput`, `Stuck` itself, `MergeFailed`, and
  all terminal states.
- **Clock source.** → Design doc. `std::time::Instant`
  (monotonic, immune to wall-clock skew).
- **Minimum `threshold` value / parser strictness.** →
  Constraint section. No clamping, no floor; the parser is the
  contract.
- **Restart behaviour.** → Constraint section. Idle clock
  resets; documented as known limitation.

Phase 2 review added three constraint items, one CLI-observability
user-story subsection, and sharpened the parser acceptance
criterion. No re-scope.
