# Reaction engine

Slices 2 and 3 are **feature-complete on `main`**. The "plan" sections
below document the original target shape; the "Implemented" section below
documents the actual landed surface — defer to the code (and the tests)
as the source of truth when they diverge.

Read this alongside `packages/core/src/lifecycle-manager.ts` lines 130–1180
and `packages/core/src/types.ts` lines 960–1170 in the TS reference.

## Implementation status

| Piece | Where | Status |
| --- | --- | --- |
| `Scm` trait | `ao_core::traits::Scm` | ✅ |
| `Scm` plugin (gh CLI) | `ao-plugin-scm-github` | ✅ |
| `ReactionEngine` | `ao_core::reaction_engine::ReactionEngine` | ✅ |
| `ReactionConfig` + `reactions:` yaml | `ao_core::{config, reactions}` | ✅ |
| SCM-driven `SessionStatus` transitions | `ao_core::{lifecycle::poll_scm, scm_transitions::derive_scm_status}` | ✅ |
| `approved-and-green` → real `gh pr merge` | `ReactionEngine::dispatch_auto_merge` | ✅ |
| `ci-failed` / `changes-requested` reactions | `ReactionEngine::dispatch` | ✅ |
| `merge_failed` parking loop (Phase G / M1) | `lifecycle::park_in_merge_failed` + `scm_transitions` | ✅ |
| `agent-stuck` detection + reaction (Phase H) | `lifecycle::check_stuck` + `ReactionEngine::dispatch` | ✅ |
| Duration-based `escalate_after: "10m"` | `ReactionEngine::dispatch` (parses via `parse_duration`) | ✅ |
| `Tracker` trait / GitHub impl | `ao_core::traits::Tracker`, `ao-plugin-tracker-github` | ✅ |
| `Notifier` trait + registry + routing | `ao_core::notifier` | ✅ |
| `stdout` notifier plugin | `ao-plugin-notifier-stdout` | ✅ |
| `ntfy` notifier plugin | `ao-plugin-notifier-ntfy` | ✅ |
| Priority-based notification fan-out | `NotifierRegistry::resolve` + `dispatch_notify` | ✅ |

### Phase F wiring (lifecycle ↔ reaction engine ↔ SCM)

Both engines share one `Arc<dyn Scm>` constructed once in `ao-cli::watch`:

```rust
let scm: Arc<dyn Scm> = Arc::new(GitHubScm::new());
let engine = ReactionEngine::new(config.reactions, runtime.clone(), events_tx)
    .with_scm(scm.clone());
let lifecycle = LifecycleManager::new(sessions, runtime, agent)
    .with_reaction_engine(Arc::new(engine))
    .with_scm(scm);
```

- **Lifecycle** uses `Scm` to poll PR state each tick. `poll_scm` fans
  out to `detect_pr` → (`pr_state`, `ci_status`, `review_decision`,
  `mergeability`) in parallel via `tokio::join!`, folds them into a
  `ScmObservation`, and hands them to the pure `derive_scm_status`
  decision function (see `docs/state-machine.md#pr-driven-transitions-phase-f`
  for the transition table).
- **Reaction engine** uses `Scm` to *execute* the `approved-and-green`
  reaction. `dispatch_auto_merge` is not a fire-and-forget intent event
  anymore — it re-probes `detect_pr` and `mergeability` at dispatch
  time and only calls `Scm::merge` if the PR still reads ready. This
  avoids stale-green merges when observation goes stale between tick
  and dispatch (e.g. a late-arriving CI failure after a reviewer
  approved).
- **`ReactionTriggered(AutoMerge)`** is emitted *after* the re-probes
  pass, so subscribers can rely on "triggered" meaning the merge was
  actually attempted. Skip paths (no PR, not ready, probe failure)
  emit no `ReactionTriggered`.
- The no-SCM fallback is preserved: if `ReactionEngine::with_scm` was
  never called (older tests, Phase D compatibility), `dispatch_auto_merge`
  emits the intent event without touching a plugin.

### Phase G wiring (merge-failure retry loop)

Phase F left one known gap (the M1 backlog note that used to live in
`reaction_engine.rs`): when `Scm::merge` failed, the session stayed in
`mergeable`, `derive_scm_status(Mergeable, ready_obs)` returned `None`
(self-loop filter), and the engine was never invoked again — silently
eating the retry budget and the auto-merge never happened.

Phase G closes it with a parking state and a clean separation of
concerns:

- **Engine reports, lifecycle decides.** `dispatch_auto_merge` returns
  `ReactionOutcome { action: AutoMerge, success: false, escalated }`
  when the underlying `Scm::merge` call errors. The engine contract
  (tracker accounting, `should_escalate`) is unchanged — it just
  reports the failure truthfully.
- **Lifecycle parks on soft failure.** After
  `ReactionEngine::dispatch` returns, `LifecycleManager::transition`
  checks `should_park_in_merge_failed(to, &outcome)`: if the target
  status was `mergeable`, the action was `AutoMerge`, the outcome was
  *not* successful, and the engine did *not* escalate, it calls
  `park_in_merge_failed` which flips the status to `merge_failed`,
  persists, and emits `StatusChanged(Mergeable → MergeFailed)`.
- **Next tick re-promotes.** `derive_scm_status(MergeFailed,
  ready_obs)` returns `Some(Mergeable)`. The lifecycle transitions
  through `mergeable` again, which re-dispatches `approved-and-green`,
  which burns another attempt from the *same* `ReactionTracker` — so
  `retries` and `escalate_after` stay honest across the loop.
- **Escalation stops the spin.** Once the engine reports
  `escalated: true`, the lifecycle does **not** park. The session
  stays in `mergeable`, the self-loop filter kicks in, and the engine
  is never re-invoked. The human sees exactly one notification.
- **Tracker preservation is explicit.** `clear_tracker_on_transition`
  in `lifecycle.rs` hardcodes two rules for the parking loop:
  (a) parking edges (`mergeable ↔ merge_failed`) preserve the
  `approved-and-green` tracker so retry accounting survives, and
  (b) exit edges *out* of `merge_failed` (to `ci_failed`,
  `changes_requested`, `pr_open`, `working`, or `merged`) explicitly
  call `engine.clear_tracker(session_id, "approved-and-green")` —
  because `status_to_reaction_key(MergeFailed) == None`, the generic
  "clear the key for `from`" rule can't cover it.

See `docs/state-machine.md#the-merge_failed-parking-loop-phase-g` for
the transition table and the rationale for why the state had to exist.

### Phase H wiring (`agent-stuck` detection + duration escalation)

Phase H closes two gaps the engine carried from Phase D:

1. `agent-stuck` was listed as a valid reaction key but nothing ever
   fired it. `status_to_reaction_key(SessionStatus::Stuck)` returned
   `None`, and there was no machinery to flip a session *into* `Stuck`
   in the first place.
2. `escalate_after: "10m"` (the TS-compatible duration form) was
   accepted at parse time but silently ignored at dispatch — only
   `escalate_after: 3` (the attempts form) actually gated escalation.

The Phase H changes are scoped to `lifecycle.rs` + `reaction_engine.rs`
and do not touch plugin contracts:

- **Lifecycle owns detection.** `LifecycleManager::check_stuck` runs as
  the final transitioning step in `poll_one`, gated on both a
  reaction engine being attached AND a pre-step-4 status snapshot
  (so two transitions can never fire on the same tick — see
  [Stuck detection (Phase H)](../state-machine.md#stuck-detection-phase-h)).
  When every guard passes, `check_stuck` calls
  `transition(session, Stuck)`, which in turn dispatches the
  `agent-stuck` reaction through the normal
  `status_to_reaction_key(Stuck) = Some("agent-stuck")` path.
- **Engine owns duration parsing.** `reaction_engine::parse_duration`
  accepts the TS regex `^\d+(s|m|h)$` (e.g. `"10s"`, `"5m"`, `"2h"`).
  `dispatch` parses the duration form of `escalate_after` lazily on
  each call and compares against `TrackerState.first_triggered_at`
  (also new in Phase H). The attempts form (`Attempts(u32)`) still
  works unchanged — if both are configured, whichever gate trips
  first wins.
- **Warn-once on malformed strings.** Both `threshold` and duration
  `escalate_after` values that fail `parse_duration` trigger a single
  `tracing::warn!` per `"{reaction_key}.{field}"` key, deduplicated via
  a process-local `Mutex<HashSet<String>>` on the engine. Operators
  see the typo once in logs and then stop getting spammed every tick.
  The function silently no-ops afterwards — a broken threshold does
  not cause dispatch errors.
- **Status flip is decoupled from `auto`.** `check_stuck` transitions
  to `Stuck` whenever a `threshold` is configured, regardless of
  `auto: true|false`. The `auto` flag only gates the *action*
  (`Notify`, `SendToAgent`, `AutoMerge`) inside `ReactionEngine::dispatch`.
  This matches how `ci-failed`, `changes-requested`, and
  `approved-and-green` already behave — status flip is lifecycle
  bookkeeping, action dispatch is reaction bookkeeping.

A typical `agent-stuck` config:

```yaml
reactions:
  agent-stuck:
    auto: true
    action: notify
    priority: warning
    threshold: 10m          # Phase H: parsed lazily per tick
  ci-failed:
    auto: true
    action: send-to-agent
    message: "CI failed. Fix it and push."
    retries: 3
    escalate_after: 5m      # Phase H: duration form now honoured
```

With that config, a session that goes idle in `Working` for more than
10 minutes flips to `Stuck` on the next tick and fires the `Notify`
action. The moment the agent produces any `Active`/`Ready` activity
again, step 4 of `poll_one` flips `Stuck → Working`,
`clear_tracker_on_transition` wipes the `agent-stuck` tracker via
`status_to_reaction_key(Stuck)`, and the `idle_since` entry gets
removed — the next idle streak starts from a fresh clock.

### Notification routing (Slice 3)

Slice 3 adds a `Notifier` trait, a `NotifierRegistry`, and
priority-based routing. When a reaction dispatches a `Notify` action
(or escalates), `dispatch_notify` resolves notifiers by priority from
the `notification_routing` config table and fans out to all matched
plugins in parallel.

Key pieces:

- **`Notifier` trait** — `name() + async send(payload)`. Plugins:
  `StdoutNotifier` (always-on), `NtfyNotifier` (opt-in via
  `AO_NTFY_TOPIC` env var).
- **`NotificationRouting`** — `HashMap<EventPriority, Vec<String>>`
  mapping priority levels to notifier names. When the config table is
  empty, `ao-cli` defaults to routing all priorities through stdout.
- **`NotifierRegistry`** — holds `Arc<dyn Notifier>` instances and the
  routing table. `resolve(priority)` returns the matched notifiers;
  unknown names log a warn-once and are skipped.
- **Partial failure** — when one notifier in a fan-out fails, others
  are still attempted. The outcome reports `success = false` with a
  message listing the failed plugins.
- **Escalation** — routes through `dispatch_notify` with
  `escalated: true`, so escalated notifications reach all configured
  plugins (not just stdout).

Config example:

```yaml
notification_routing:
  urgent: [stdout, ntfy]
  action: [stdout, ntfy]
  warning: [stdout]
  info: [stdout]
```

## What is a "reaction"?

The TS lifecycle-manager does two jobs per poll tick:

1. **Observe.** Update `SessionStatus` from runtime/SCM probes.
2. **React.** When the observed state matches a known *reaction key* (e.g.
   `ci-failed`, `changes-requested`, `agent-stuck`), run the configured
   action: send a nudge to the agent, notify a human, or auto-merge.

Reactions are the glue between "we detected something went wrong" and
"someone or something does the work to fix it". Without them the loop is
an observer — with them it's a supervisor.

## Reaction keys (from `lifecycle-manager.ts::eventToReactionKey`)

| Key | Trigger | Typical action |
| --- | --- | --- |
| `ci-failed` | CI on PR failed | send CI logs summary to agent |
| `changes-requested` | Human reviewer requested changes | send review comments to agent |
| `bugbot-comments` | Automated review found issues | send bot comments to agent |
| `merge-conflicts` | PR can't merge cleanly | send "rebase please" to agent |
| `approved-and-green` | Mergeable PR | auto-merge or notify |
| `agent-stuck` | Session made no progress for `threshold` | notify human |
| `agent-needs-input` | Agent hit a permission prompt | notify human |
| `agent-exited` | Runtime gone | notify human |
| `all-complete` | All sessions in terminal state | notify "done for the day" |

Each key maps to a `ReactionConfig` in the project config.

## `ReactionConfig` shape (TS reference)

```ts
interface ReactionConfig {
  auto: boolean;                          // master on/off
  action: "send-to-agent" | "notify" | "auto-merge";
  message?: string;                       // body for send-to-agent
  priority?: EventPriority;               // urgent | warning | info
  retries?: number;                       // max send-to-agent attempts
  escalateAfter?: number | string;        // fall back to notify after N tries or "10m"
  threshold?: string;                     // "10m" — how long until we consider it stuck
  includeSummary?: boolean;               // attach a context blob to the notification
}
```

A `ReactionTracker` is kept per `(sessionId, reactionKey)` to count attempts
and remember when the reaction first fired — that's how `retries` and
`escalateAfter` stay honest across poll ticks.

## Proposed Rust shape

```rust
// ao_core::types
pub enum ReactionAction {
    SendToAgent,
    Notify,
    AutoMerge,
}

pub enum EventPriority { Urgent, Warning, Info }

pub struct ReactionConfig {
    pub auto: bool,
    pub action: ReactionAction,
    pub message: Option<String>,
    pub priority: Option<EventPriority>,
    pub retries: Option<u32>,
    pub escalate_after: Option<EscalateAfter>,   // Count(u32) | Duration(Duration)
    pub threshold: Option<Duration>,
    pub include_summary: bool,
}

pub enum EscalateAfter {
    Count(u32),
    Duration(std::time::Duration),
}

// ao_core::reactions (new module)
pub struct ReactionTracker {
    pub attempts: u32,
    pub first_triggered: std::time::Instant,
}

pub struct ReactionEngine {
    trackers: std::collections::HashMap<(SessionId, String), ReactionTracker>,
    config: ReactionMap,
}

pub type ReactionMap = std::collections::HashMap<String, ReactionConfig>;

impl ReactionEngine {
    pub async fn dispatch(
        &mut self,
        session: &Session,
        key: &str,
        runtime: &dyn Runtime,
        agent: &dyn Agent,
        notifier: Option<&dyn Notifier>,
    ) -> Result<ReactionOutcome> { ... }
}
```

`ReactionEngine::dispatch` owns the retry accounting and escalation; the
lifecycle loop just tells it "key X fired for session Y" and trusts the
engine to decide whether to attempt again, escalate, or no-op.

## New traits Slice 2 defines

### `Scm`

*GitHub, GitLab, Gitea, …*

```rust
#[async_trait]
pub trait Scm: Send + Sync {
    fn name(&self) -> &str;

    async fn pull_request(&self, session: &Session) -> Result<Option<PullRequest>>;
    async fn checks(&self, session: &Session) -> Result<Vec<CheckRun>>;
    async fn reviews(&self, session: &Session) -> Result<Vec<Review>>;
    async fn review_comments(&self, session: &Session) -> Result<Vec<ReviewComment>>;
    async fn merge(&self, session: &Session) -> Result<()>;
}
```

`PullRequest`, `CheckRun`, `Review`, `ReviewComment` all mirror
`types.ts` lines ~700-770 verbatim.

### `Tracker`

*Linear, GitHub Issues, Jira, …*

```rust
#[async_trait]
pub trait Tracker: Send + Sync {
    fn name(&self) -> &str;
    async fn get_issue(&self, identifier: &str, project: &ProjectConfig) -> Result<Issue>;
    async fn is_completed(&self, identifier: &str, project: &ProjectConfig) -> Result<bool>;
    fn issue_url(&self, identifier: &str, project: &ProjectConfig) -> String;
    fn branch_name(&self, identifier: &str) -> String;
}
```

### `Notifier` (stretch — maybe Slice 3)

*Slack, stdout, email, …*

Only needed if `ReactionAction::Notify` needs to reach out of the CLI. For
Slice 2 we can hard-code notifications to stdout/`tracing::warn!` and push
the plugin slot to Slice 3.

## Where reactions live in the loop

TS embeds reactions inside the same poll iteration. The Rust port should
do the same, for two reasons:

1. Reactions depend on the *transition*, not the absolute state, so they
   need to see `prev_status` / `next_status` together. That's already
   visible inside `LifecycleManager::poll_one`.
2. Running them in a second `tokio::spawn` adds a whole category of "did
   this tracker update land before the next poll?" races.

Concretely: `LifecycleManager` gains a `reaction_engine: Option<ReactionEngine>`
field. `poll_one` grows a new step between "status changed" and "emit event":

```
4. Status changed?
   - If a reaction key maps to (prev, next), call engine.dispatch().
   - If dispatch returns Escalated { .. }, also emit a Notified event.
5. Emit StatusChanged event.
```

The event bus should gain two new variants:

```rust
OrchestratorEvent::ReactionTriggered { id, key, action }
OrchestratorEvent::ReactionEscalated { id, key, attempts }
```

so `ao-rs watch` can show them on the same stream it already prints.

## Config file (the one thing Slice 2 forces on us)

Slice 1 has *zero* config — everything is defaults and CLI flags. Slice 2
has to read reactions from *somewhere*. Minimum shape:

```yaml
# ~/.ao-rs/config.yaml
reactions:
  ci-failed:
    auto: true
    action: send-to-agent
    message: "CI failed. Read the logs, fix the issue, and push again."
    retries: 3
    escalate_after: 3
  changes-requested:
    auto: true
    action: send-to-agent
    retries: 2
  agent-stuck:
    auto: true
    action: notify
    threshold: 10m
    priority: warning
  approved-and-green:
    auto: false    # never auto-merge by default
    action: notify
    priority: info

projects:
  demo:
    scm: github           # which scm plugin to use
    tracker: github       # which tracker plugin to use
    reactions:            # per-project overrides merge onto global
      approved-and-green:
        auto: true
        action: auto-merge
```

Load it with `serde_yaml` + defaults on missing fields. Reject unknown
reaction keys at parse time so typos fail loud.

## Testing strategy

Same pattern as Slice 1:

1. **Mock `Scm`** returning scripted PR/CI state. Drive the lifecycle loop
   tick-by-tick and assert that the right reaction keys fire.
2. **Mock `ReactionEngine::dispatch`** to record calls. Assert that the
   loop calls the engine with the expected `(session_id, key)` pairs
   across status transitions.
3. **Escalation test.** Configure `retries: 2, escalate_after: 2`; make
   the `send-to-agent` branch fail twice; assert escalation fires on the
   third attempt and `Notified` event is emitted.
4. **Retry bookkeeping test.** Same as above but make attempt 2 succeed;
   assert the tracker clears so a *later* recurrence starts fresh.

All of this belongs in `crates/ao-core/src/reactions.rs::tests` plus a
new integration test in `crates/ao-core/tests/reaction_flow.rs` that
walks a mock session through `working → ci_failed → working`.

## Not implemented

- **GraphQL batching.** TS has `enrichWithPullRequestsBatched` that
  fetches multiple PRs in one query. We call `gh pr view` per session,
  which is fine at N≤30.
- **Reaction history persistence.** The tracker map lives in memory;
  a watcher restart resets retry counts. TS is the same.
- **Notifier error backoff / retry ladder.** TS has it, we don't.
- **Template engine for notification bodies.** Using `format!`.
- **Desktop / Slack / email notifiers.** One crate each when needed.
- **`Errored` → `agent-errored` reaction.** Deferred — no signal to
  trigger it yet.

## Open question: reaction engine as a separate task?

TS runs reactions inline. The alternative is a `tokio::spawn` that
subscribes to the event bus and handles reactions out-of-band.

Pros of out-of-band:
- Reactions can be slow (network I/O) without stalling the poll loop.
- Natural fault isolation — a broken reaction can't wedge polling.

Cons:
- Event ordering becomes a concern; broadcast can lag.
- Two sources of truth for "what state did we just see?"

**Tentative call: inline in Slice 2**, matching TS. If reaction latency
becomes a measurable problem, revisit. See
`docs/architecture.md#open-architecture-questions`.
