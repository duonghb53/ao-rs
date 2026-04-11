# Session state machine

Everything here is already in code — this doc is the picture-first view.

## Two orthogonal axes

A `Session` has two state fields that move independently:

| Field | Type | Source of change |
| --- | --- | --- |
| `status` | `SessionStatus` (17 variants) | Lifecycle transitions, SCM signals, user action |
| `activity` | `Option<ActivityState>` (6 variants) | `Agent::detect_activity`, polled every tick |

The two exist because one `status` can contain many `activity` states. A
session in `Working` can be `Active` (typing code), `Ready` (finished its
turn, idle), or `Idle` (stale scrollback) depending on what the agent is
doing *right now*. A terminal `status` like `Merged` overrides activity —
the session is dead no matter what the agent plugin says.

Both enums mirror `packages/core/src/types.ts` with identical snake_case
names so yaml files on disk are drop-in comparable.

## `SessionStatus` variants (from `types.rs`)

```
spawning → working → pr_open
                   ↓
              ci_failed, review_pending, changes_requested
                   ↓
              approved → mergeable → merged
                   ↓                    ↓
              needs_input, stuck     cleanup

off-path terminal states: errored, killed, terminated, done, idle
```

| Variant | Meaning | Terminal? | Restorable? |
| --- | --- | --- | --- |
| `spawning` | Workspace/runtime being materialized | no | no |
| `working` | Agent is actively working (or was, last poll) | no | no |
| `pr_open` | PR opened; waiting for CI / review | no | no |
| `ci_failed` | CI on PR failed — reaction target | no | no |
| `review_pending` | PR waiting on human review | no | no |
| `changes_requested` | Review requested changes — reaction target | no | no |
| `approved` | Approved but not yet mergeable | no | no |
| `mergeable` | Approved + green — ready to merge | no | no |
| `merged` | PR merged | **yes** | **no** |
| `cleanup` | Post-merge cleanup in progress | **yes** | no |
| `needs_input` | Agent is blocked on a question | no | no |
| `stuck` | Agent stopped making progress | no | no |
| `errored` | Unrecoverable failure | **yes** | yes |
| `killed` | User explicitly killed | **yes** | yes |
| `idle` | Ready but nothing to do | no | no |
| `done` | Completed successfully | **yes** | yes |
| `terminated` | Runtime exited; not yet reclassified | **yes** | yes |

Terminal set comes from `SessionStatus::is_terminal` in `types.rs`, which
matches `TERMINAL_STATUSES` in the TS reference verbatim. Restorability
excludes `merged` (nothing left to resume into) and `cleanup` (in-flight
cleanup would collide) — see `SessionStatus::is_restorable`.

## `ActivityState` variants

```
active ↔ ready ↔ idle      waiting_input     blocked     exited (terminal)
```

| Variant | Meaning |
| --- | --- |
| `active` | Processing (thinking, writing code) |
| `ready` | Finished its turn, alive, waiting |
| `idle` | Inactive for a while (stale scrollback) |
| `waiting_input` | Permission prompt / question for the human |
| `blocked` | Hit an error or is stuck |
| `exited` | Process is gone — terminal |

## Transitions currently implemented (through Phase F)

`lifecycle.rs::poll_one` handles these transitions per tick:

1. **Runtime probe.** `Runtime::is_alive(handle)` → if false, `terminate(RuntimeGone)`.
2. **No handle at all.** → `terminate(NoHandle)`.
3. **Activity probe.** `Agent::detect_activity(session)`.
   - If `Exited`, `terminate(AgentExited)`.
   - Else persist and emit `ActivityChanged` if it differs from the last-seen value.
4. **Happy-path status flip.** If `status == Spawning` **and**
   `activity ∈ {Active, Ready}`, transition to `Working`.
5. **SCM probe** *(Phase F, only when an `Scm` plugin is attached via
   `LifecycleManager::with_scm`)*. `poll_scm` calls `detect_pr` and, on
   hit, fans out in parallel (`tokio::join!`) to `pr_state`,
   `ci_status`, `review_decision`, and `mergeability`. Any probe
   failure is surfaced as `TickError` and the session stays put — we
   refuse to transition on a partial observation. On success the
   four-tuple is folded into `ScmObservation` and handed to the pure
   `derive_scm_status` decision function.

Terminal sessions are skipped on the same tick they're observed.

`terminate()` flips `status` to `Terminated` and emits both the
`StatusChanged` and `Terminated` events. It is the only way the loop
transitions to `Terminated` today.

## PR-driven transitions (Phase F)

The `scm_transitions::derive_scm_status` pure function owns the entire
mapping from `(SessionStatus, Option<ScmObservation>)` →
`Option<SessionStatus>`. Extracted as a free function so it's unit-
testable without plumbing (19 table tests in `scm_transitions.rs`) and
reusable from future debug commands like `ao-rs pr refresh <id>`.

### No-PR branch (`detect_pr` returned `Ok(None)`)

If the session is on the PR track (see `is_pr_track`: `pr_open`,
`ci_failed`, `review_pending`, `changes_requested`, `approved`,
`mergeable`) it drops back to `working` so the next push re-discovers.
Non-PR-track and terminal statuses stay put.

### Open-PR priority ladder

With an open PR, the decision walks this ladder in order — first match
wins:

| # | Condition | Next status |
| --- | --- | --- |
| 1 | `mergeability.is_ready()` | `mergeable` |
| 2 | `review == changes_requested` | `changes_requested` |
| 3 | `ci == failing` | `ci_failed` |
| 4 | `review == approved` (but not ready) | `approved` |
| 5 | default | `pr_open` |

Rationale for `changes_requested > ci_failed`: human feedback is
usually the higher-order bit (addressing it often re-runs CI anyway),
and the agent's reaction response is strictly more informative. The TS
reference folds the two into one reaction slot; we preserve the
priority explicitly.

### Terminal PR states

- `state == merged` → `merged` (terminal; fires post-merge cleanup).
- `state == closed` → `terminated` (TS has a dedicated `pr_closed`
  terminal state; we fold it into `terminated` because the session
  semantics are identical — runtime is gone, user decides what's next).

A `(next != current).then_some(next)` filter at the top of
`derive_scm_status` elides self-loops so subscribers never see
`StatusChanged(PrOpen → PrOpen)`.

## Events the loop emits

From `events.rs`:

- `Spawned { id, project_id }` — first time the loop sees a session on disk.
- `StatusChanged { id, from, to }` — `from != to` always.
- `ActivityChanged { id, prev, next }` — polled activity changed.
- `Terminated { id, reason }` — one of `RuntimeGone | AgentExited | NoHandle`.
- `TickError { id, message }` — per-session error, does not kill the loop.

All events ride on `tokio::sync::broadcast`, which means slow subscribers
get **lagged** and can miss events. That's fine for observability (the CLI
`ao-rs watch`) and future reaction code can snapshot via
`SessionManager::list` on startup and then subscribe for deltas.

## Test coverage

- **Activity/runtime-driven transitions** live in
  `crates/ao-core/src/lifecycle.rs::tests` using `MockRuntime` +
  `MockAgent`.
- **SCM decision function** has 19 table-driven unit tests in
  `scm_transitions.rs::tests` — every priority ladder branch, the
  no-PR fallback, merged/closed terminals, and a full `SessionStatus`
  exhaustiveness check so new variants break the build until they're
  classified.
- **SCM polling glue** has 9 integration tests in
  `lifecycle.rs::tests` using a `MockScm` fixture with per-probe
  error toggles (`detect_pr`, `pr_state`, `ci_status`,
  `review_decision`, `mergeability`). The `scm_poll_field_probe_error…`
  parameterized test forces each slot to fail and asserts that the
  emitted `TickError` message names the failing slot.
