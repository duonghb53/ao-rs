# Reaction engine — Slice 2 plan

This is a **forward-looking** doc. Nothing here is implemented yet; Slice 1
stops at `Working`. Slice 2 is where the loop starts reacting to PR/CI/review
events and the `Scm` + `Tracker` plugin slots come into existence.

Read this alongside `packages/core/src/lifecycle-manager.ts` lines 130–1180
and `packages/core/src/types.ts` lines 960–1170 in the TS reference.

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

## Out of scope for Slice 2

- **GraphQL batching.** TS has `enrichWithPullRequestsBatched` that
  fetches multiple PRs in one query. We'll call `gh pr view` per session,
  which is fine at N≤30.
- **Fancy duration parsing.** Accept `10m`, `1h`, `30s`. Anything else
  errors out at config-parse time.
- **Reaction history persistence.** The tracker map lives in memory;
  a watcher restart resets retry counts. TS is the same.
- **Multi-notifier routing.** `notificationRouting: Record<EventPriority,
  string[]>` — one notifier (stdout or nothing) in Slice 2, fan-out in
  Slice 3 if ever.

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
