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

## Transitions currently implemented (Slice 1 Phase C)

`lifecycle.rs::poll_one` handles these transitions per tick:

1. **Runtime probe.** `Runtime::is_alive(handle)` → if false, `terminate(RuntimeGone)`.
2. **No handle at all.** → `terminate(NoHandle)`.
3. **Activity probe.** `Agent::detect_activity(session)`.
   - If `Exited`, `terminate(AgentExited)`.
   - Else persist and emit `ActivityChanged` if it differs from the last-seen value.
4. **Happy-path status flip.** If `status == Spawning` **and**
   `activity ∈ {Active, Ready}`, transition to `Working`.

Terminal sessions are skipped on the same tick they're observed.

`terminate()` flips `status` to `Terminated` and emits both the
`StatusChanged` and `Terminated` events. It is the only way the loop
transitions to `Terminated` today.

## Transitions NOT yet implemented

Everything past `Working` — `pr_open`, `ci_failed`, `review_pending`, etc.
— requires an `Scm` plugin to surface PR/CI/review state. Slice 2 adds
that. See `docs/reactions.md`.

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

All transitions above have a dedicated test in
`crates/ao-core/src/lifecycle.rs::tests` using `MockRuntime` +
`MockAgent`. When Slice 2 lands SCM-driven transitions, each new
transition should get its own mock test in the same file.
