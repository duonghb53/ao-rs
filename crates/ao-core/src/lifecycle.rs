//! Background polling loop that keeps `Session` state in sync with reality.
//!
//! Corresponds to `packages/core/src/lifecycle-manager.ts` in the reference
//! repo, trimmed to what Slice 1 Phase C actually needs:
//!
//! 1. Every `poll_interval`, list all non-terminal sessions from disk.
//! 2. For each one, probe `Runtime::is_alive` and `Agent::detect_activity`.
//! 3. Apply state transitions and persist the new `Session` atomically.
//! 4. Broadcast `OrchestratorEvent`s so subscribers (CLI, reaction engine,
//!    notifiers, …) can react without polling themselves.
//!
//! Design notes:
//!
//! - **Trait objects, not generics.** The manager owns `Arc<dyn Runtime>`
//!   etc. so the same `LifecycleManager` type can be used in tests (with
//!   mocks) and in the real CLI (with tmux/claude-code). Generic parameters
//!   would have leaked through every consumer.
//!
//! - **Disk is the source of truth.** The loop re-reads from
//!   `SessionManager::list` each tick rather than holding state in memory.
//!   This matches the Slice 1 design principle established in Phase A, and
//!   means `ao-rs spawn` running in a separate process is immediately
//!   visible on the next tick. (A future Slice 2+ may add an in-memory
//!   cache + file-watcher for efficiency.)
//!
//! - **Per-session errors don't stop the loop.** If one session's runtime
//!   probe fails, we emit `TickError` and continue. Only fatal `SessionManager::list`
//!   errors bubble up (and even then we log and keep looping).
//!
//! - **Event channel lag.** We use `tokio::sync::broadcast`, which drops
//!   old events when a slow subscriber can't keep up. That's fine for
//!   observability — a reaction engine that misses a tick just picks up
//!   the next one. Anyone needing lossless delivery should snapshot via
//!   `SessionManager::list` on startup and then subscribe.

use crate::{
    error::Result,
    events::{OrchestratorEvent, TerminationReason},
    reaction_engine::{parse_duration, status_to_reaction_key, ReactionEngine},
    reactions::{ReactionAction, ReactionOutcome},
    scm::{CiStatus, MergeReadiness, PrState, PullRequest, ReviewDecision},
    scm_transitions::{derive_scm_status, ScmObservation},
    session_manager::SessionManager,
    traits::{Agent, Runtime, Scm, Workspace},
    types::{ActivityState, Session, SessionId, SessionStatus},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// How many events the broadcast channel buffers before dropping the oldest.
/// 1024 is generous — a healthy loop emits at most ~N events per tick where
/// N is the session count, and slow subscribers will lag at most a handful
/// of ticks before catching up.
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Default poll interval. Increased from the TS reference's 5 s to 10 s
/// to further reduce GitHub API pressure now that batch enrichment +
/// ETag guards handle the hot path.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Minimum time between review-backlog API calls per session (2 min).
/// Mirrors `REVIEW_BACKLOG_THROTTLE_MS = 120_000` in the TS reference.
/// Applies when the batch enrichment cache has no hit and the session is
/// already in a review-related state — avoids hammering the REST API for
/// review data that rarely changes within a 2-minute window.
const REVIEW_BACKLOG_THROTTLE: Duration = Duration::from_secs(120);

pub struct LifecycleManager {
    sessions: Arc<SessionManager>,
    runtime: Arc<dyn Runtime>,
    agent: Arc<dyn Agent>,
    events_tx: broadcast::Sender<OrchestratorEvent>,
    poll_interval: Duration,
    /// Optional Slice 2 Phase D reaction engine. When set, every status
    /// transition into a reaction-triggering state (see
    /// `status_to_reaction_key`) calls `engine.dispatch(...)`. When unset,
    /// the lifecycle loop behaves exactly as it did in Phase C.
    reaction_engine: Option<Arc<ReactionEngine>>,
    /// Optional Slice 2 Phase F SCM plugin. When set, every tick calls
    /// `detect_pr` on each non-terminal session; a fresh PR observation
    /// is folded through `derive_scm_status` to produce PR-driven status
    /// transitions (`Working → PrOpen/CiFailed/ChangesRequested/…`).
    /// When unset, the lifecycle loop is exactly the Phase C/D behaviour
    /// — SCM polling is off. This matches how tests and `ao-rs watch`
    /// without a configured plugin should behave.
    scm: Option<Arc<dyn Scm>>,
    /// Optional workspace plugin. When set, sessions that transition to
    /// `Merged` automatically have their worktree destroyed so disk space
    /// is reclaimed without a manual `ao-rs cleanup`.
    workspace: Option<Arc<dyn Workspace>>,
    /// Slice 2 Phase H bookkeeping for agent-stuck detection.
    ///
    /// Records the `Instant` at which each session first entered an idle
    /// activity state (`Idle` / `Blocked`). `check_stuck` reads this map
    /// to decide whether the session has been idle longer than the
    /// configured `agent-stuck.threshold`.
    ///
    /// - An entry is **inserted** the first tick activity flips into
    ///   `Idle`/`Blocked` and **preserved** across subsequent idle ticks
    ///   so `elapsed()` grows monotonically.
    /// - An entry is **removed** as soon as activity flips back to any
    ///   non-idle state, so the next idle streak restarts the clock.
    /// - `terminate` also clears the entry to bound memory for long-
    ///   running watch loops (Task 2.7).
    ///
    /// `Mutex<HashMap>` mirrors how `ReactionEngine::trackers` is stored —
    /// short critical sections around pure read/modify/write, no nested
    /// locking.
    idle_since: Mutex<HashMap<SessionId, Instant>>,
    /// Per-tick cache of batch-enriched PR observations.
    ///
    /// Populated once at the start of each `tick()` call via
    /// `Scm::enrich_prs_batch()`. Individual `poll_scm` calls check this
    /// cache first and skip the 4× REST fan-out when they find a hit.
    /// Cleared at the start of the next tick.
    ///
    /// Key format: `"{owner}/{repo}#{number}"`.
    pr_enrichment_cache: Mutex<HashMap<String, ScmObservation>>,
    /// Per-session timestamp of the last review backlog API check.
    /// Throttles `pending_comments` calls to at most once per 2 minutes.
    last_review_backlog_check: Mutex<HashMap<SessionId, Instant>>,
    /// Per-tick cache of detected PRs from `detect_pr`. Populated in
    /// `tick()` Pass 1 so `poll_scm` reuses the result instead of
    /// calling `detect_pr` a second time.
    detected_prs_cache: Mutex<HashMap<SessionId, Option<PullRequest>>>,
}

impl LifecycleManager {
    /// Decide whether the session should transition to `Stuck` *right now*,
    /// based on the idle-since bookkeeping and the configured `agent-stuck`
    /// threshold.
    ///
    /// This is the shared predicate used by:
    /// - `check_stuck` (step 6), which runs when no other transition happened
    ///   this tick.
    /// - `poll_scm` (step 5), which may override a would-be `PrOpen` transition
    ///   to `Stuck` so the tick still performs **one** status transition while
    ///   matching the TS reference behavior (where stuck detection can win over
    ///   the fallback `pr_open` state).
    fn should_mark_stuck(&self, session: &Session) -> bool {
        if !is_stuck_eligible(session.status) {
            return false;
        }

        let idle_started = {
            let map = self
                .idle_since
                .lock()
                .expect("lifecycle idle_since mutex poisoned");
            map.get(&session.id).copied()
        };
        let Some(idle_started) = idle_started else {
            return false;
        };

        let Some(engine) = self.reaction_engine.as_ref() else {
            return false;
        };
        let Some(cfg) = engine.resolve_reaction_config(session, "agent-stuck") else {
            return false;
        };
        let Some(raw) = cfg.threshold.as_deref() else {
            return false;
        };
        let Some(threshold) = parse_duration(raw) else {
            engine.warn_once_parse_failure("agent-stuck", "threshold", raw);
            return false;
        };

        idle_started.elapsed() > threshold
    }

    pub fn new(
        sessions: Arc<SessionManager>,
        runtime: Arc<dyn Runtime>,
        agent: Arc<dyn Agent>,
    ) -> Self {
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            sessions,
            runtime,
            agent,
            events_tx,
            poll_interval: DEFAULT_POLL_INTERVAL,
            reaction_engine: None,
            scm: None,
            workspace: None,
            idle_since: Mutex::new(HashMap::new()),
            pr_enrichment_cache: Mutex::new(HashMap::new()),
            last_review_backlog_check: Mutex::new(HashMap::new()),
            detected_prs_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Attach a reaction engine. `ReactionEngine::new` should be given
    /// this `LifecycleManager`'s `events_tx` (via `events_sender()`) so
    /// reaction events land on the same broadcast channel as lifecycle
    /// events. Builder form for test ergonomics.
    pub fn with_reaction_engine(mut self, engine: Arc<ReactionEngine>) -> Self {
        self.reaction_engine = Some(engine);
        self
    }

    /// Attach an SCM plugin. When present, `poll_one` fans out a
    /// `detect_pr` + four parallel `pr_state`/`ci_status`/`review_decision`/
    /// `mergeability` probes per session per tick, then routes the result
    /// through `derive_scm_status` for PR-driven status transitions.
    ///
    /// Builder form mirrors `with_reaction_engine` — call sites that don't
    /// care about SCM polling leave it unset and get Phase C/D behaviour.
    pub fn with_scm(mut self, scm: Arc<dyn Scm>) -> Self {
        self.scm = Some(scm);
        self
    }

    /// Attach a workspace plugin. When present, sessions that transition to
    /// `Merged` automatically have their worktree destroyed within the same
    /// poll cycle. Sessions with `workspace_path: None` are unaffected.
    pub fn with_workspace(mut self, workspace: Arc<dyn Workspace>) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Borrow the underlying broadcast sender so a `ReactionEngine`
    /// constructed separately can publish events on the same channel
    /// `LifecycleManager` uses. Cheap clone — `broadcast::Sender` is
    /// internally ref-counted.
    pub fn events_sender(&self) -> broadcast::Sender<OrchestratorEvent> {
        self.events_tx.clone()
    }

    /// Get a fresh subscriber. Each `recv()` call sees events from the
    /// point of subscription onward — history is not replayed.
    pub fn subscribe(&self) -> broadcast::Receiver<OrchestratorEvent> {
        self.events_tx.subscribe()
    }

    /// Spawn the background polling loop. Returns a handle that can be
    /// used to stop it cleanly.
    ///
    /// We use `tokio_util::sync::CancellationToken` rather than a oneshot
    /// because cancellation tokens are cheap to clone and can be passed
    /// into future sub-tasks (e.g. a reaction engine that shares this
    /// manager's shutdown signal).
    pub fn spawn(self: Arc<Self>) -> LifecycleHandle {
        let token = CancellationToken::new();
        let child_token = token.clone();
        let this = self.clone();
        let join = tokio::spawn(async move {
            this.run_loop(child_token).await;
        });
        LifecycleHandle { join, token }
    }

    /// The loop body. Ticks on `poll_interval`, exits cleanly when the
    /// cancellation token fires.
    async fn run_loop(self: Arc<Self>, token: CancellationToken) {
        // Per-loop memory of which session IDs we've already announced via
        // `Spawned`, so we emit it exactly once per session observed.
        let mut seen: HashSet<SessionId> = HashSet::new();

        // Crash-recovery sweep: if the process restarted after a session was
        // persisted as `Merged` but before its worktree was destroyed (e.g.
        // the daemon was killed mid-tick), the transition tick will never fire
        // again because terminal sessions are skipped by `poll_one`. We scan
        // once at startup and clean up any leftover worktrees.
        self.sweep_merged_worktrees().await;

        let mut ticker = tokio::time::interval(self.poll_interval);
        // Skip the immediate-fire behaviour of `interval` — users expect
        // "start, wait, tick" not "start, tick, wait". (The TS loop
        // behaves the same way.)
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    tracing::debug!("lifecycle loop received cancel");
                    return;
                }
                _ = ticker.tick() => {
                    if let Err(e) = self.tick(&mut seen).await {
                        // Fatal disk read error — log and keep going.
                        // A transient `~/.ao-rs/sessions` permission issue
                        // shouldn't permanently kill the loop.
                        tracing::error!("lifecycle tick failed: {e}");
                    }
                }
            }
        }
    }

    /// One pass over every non-terminal session. Public so tests can
    /// drive the state machine deterministically without `sleep`ing.
    pub async fn tick(&self, seen: &mut HashSet<SessionId>) -> Result<()> {
        let sessions = self.sessions.list().await?;

        // ---- Batch PR enrichment (rate-limit optimization) ----
        // Two-pass approach:
        //   Pass 1: detect_pr for each non-terminal session → collect PRs
        //   Batch: enrich_prs_batch for all collected PRs → populate cache
        //   Pass 2: poll_one (which calls poll_scm) consumes from cache,
        //           skipping the 4× REST fan-out when a batch hit exists.
        //
        // detect_pr runs once per session per tick (same as before); the
        // savings come from replacing 4× REST per session with 1 GraphQL
        // batch for all sessions.

        // Clear the previous tick's cache.
        {
            let mut cache = self
                .pr_enrichment_cache
                .lock()
                .expect("pr_enrichment_cache mutex poisoned");
            cache.clear();
        }

        // Pass 1: detect PRs (only when SCM is configured).
        // Store detected PRs keyed by session ID so poll_scm can reuse them.
        let mut detected_prs: HashMap<SessionId, Option<PullRequest>> = HashMap::new();
        if let Some(scm) = self.scm.as_ref() {
            let mut prs_for_batch = Vec::new();
            for session in &sessions {
                if session.is_terminal() {
                    continue;
                }
                match scm.detect_pr(session).await {
                    Ok(pr) => {
                        if let Some(ref p) = pr {
                            prs_for_batch.push(p.clone());
                        }
                        detected_prs.insert(session.id.clone(), pr);
                    }
                    Err(e) => {
                        self.emit(OrchestratorEvent::TickError {
                            id: session.id.clone(),
                            message: format!("scm.detect_pr: {e}"),
                        });
                        detected_prs.insert(session.id.clone(), None);
                    }
                }
            }

            // Batch enrichment
            if !prs_for_batch.is_empty() {
                match scm.enrich_prs_batch(&prs_for_batch).await {
                    Ok(enrichment) => {
                        if !enrichment.is_empty() {
                            tracing::debug!(
                                "[batch enrichment] cached {} PR observations",
                                enrichment.len()
                            );
                            let mut cache = self
                                .pr_enrichment_cache
                                .lock()
                                .expect("pr_enrichment_cache mutex poisoned");
                            *cache = enrichment;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[batch enrichment] failed: {e}");
                    }
                }
            }
        }

        // Store detected PRs so poll_scm can consume them.
        {
            let mut cache = self
                .detected_prs_cache
                .lock()
                .expect("detected_prs_cache mutex poisoned");
            *cache = detected_prs;
        }

        // Pass 2: poll each session.
        for session in sessions {
            if seen.insert(session.id.clone()) {
                self.emit(OrchestratorEvent::Spawned {
                    id: session.id.clone(),
                    project_id: session.project_id.clone(),
                });
            }

            if session.is_terminal() {
                continue;
            }

            if let Err(e) = self.poll_one(session).await {
                tracing::warn!("poll_one failed: {e}");
            }
        }
        Ok(())
    }

    /// Probe one session and apply any resulting transitions.
    async fn poll_one(&self, mut session: Session) -> Result<()> {
        // ---- 1. Runtime liveness ----
        let alive = match &session.runtime_handle {
            Some(handle) => match self.runtime.is_alive(handle).await {
                Ok(a) => a,
                Err(e) => {
                    // Runtime probe itself errored — treat as unknown,
                    // emit TickError, and don't transition.
                    self.emit(OrchestratorEvent::TickError {
                        id: session.id.clone(),
                        message: format!("is_alive: {e}"),
                    });
                    return Ok(());
                }
            },
            // No handle means we never got far enough to spawn a runtime,
            // or it was intentionally cleared. Consider the session dead.
            None => {
                self.terminate(&mut session, TerminationReason::NoHandle)
                    .await?;
                return Ok(());
            }
        };

        if !alive {
            self.terminate(&mut session, TerminationReason::RuntimeGone)
                .await?;
            return Ok(());
        }

        // ---- 2. Agent activity detection ----
        let activity = match self.agent.detect_activity(&session).await {
            Ok(a) => a,
            Err(e) => {
                self.emit(OrchestratorEvent::TickError {
                    id: session.id.clone(),
                    message: format!("detect_activity: {e}"),
                });
                return Ok(());
            }
        };

        // Agent says the process exited — treat the same as runtime gone,
        // but attribute the reason to the agent so observers can distinguish.
        if activity.is_terminal() {
            self.terminate(&mut session, TerminationReason::AgentExited)
                .await?;
            return Ok(());
        }

        // ao-ts parity: `waiting_input` is a first-class lifecycle status.
        // This must win early so a session doesn't stay `Working` while the
        // agent is blocked on a prompt.
        if activity == ActivityState::WaitingInput && session.status != SessionStatus::NeedsInput {
            self.transition(&mut session, SessionStatus::NeedsInput)
                .await?;
        }

        // ---- 3. Persist any activity transition ----
        if session.activity != Some(activity) {
            let prev = session.activity;
            session.activity = Some(activity);
            self.sessions.save(&session).await?;
            self.emit(OrchestratorEvent::ActivityChanged {
                id: session.id.clone(),
                prev,
                next: activity,
            });
        }

        // Phase H: maintain the idle-since timestamp used by
        // `check_stuck`. Unconditional every tick — the helper itself
        // decides whether to insert, preserve, or remove.
        self.update_idle_since(&session.id, activity);

        // Snapshot the status BEFORE any transitioning step runs, so
        // step 6 (`check_stuck`) can yield the tick if an earlier step
        // already mutated `session.status`. Matches the TS reference's
        // `determineStatus` contract of one transition per call — see
        // Design Decision 8 in docs/ai/design/feature-agent-stuck-detection.md
        // and the "one transition per tick" entry in memory.
        let pre_transition_status = session.status;

        // ---- 4. Status transitions driven by activity ----
        // Slice 1 Phase C handles the happy-path Spawning → Working flip.
        // Phase F layers SCM-driven transitions on top (see step 5).
        // Phase H extends this with the symmetric `Stuck → Working`
        // recovery: a session that went idle long enough to park in
        // `Stuck` should exit the moment the agent starts producing
        // activity again. `transition` auto-clears the `agent-stuck`
        // tracker via `status_to_reaction_key(Stuck) = Some("agent-stuck")`
        // so there's no bespoke cleanup needed here.
        if matches!(
            session.status,
            SessionStatus::Spawning | SessionStatus::Stuck | SessionStatus::NeedsInput
        ) && matches!(activity, ActivityState::Active | ActivityState::Ready)
        {
            self.transition(&mut session, SessionStatus::Working)
                .await?;
        }

        // ---- 5. PR-driven status transitions (Phase F) ----
        // Only runs when a `Scm` plugin is wired in (via `with_scm`). A
        // failing probe inside `poll_scm` emits `TickError` on the shared
        // channel and returns `Ok(())` so one bad `gh` shell-out doesn't
        // kill the whole tick.
        if self.scm.is_some() {
            self.poll_scm(&mut session).await?;
        }

        // ---- 5b. Worktree cleanup on Merged ----
        // When a workspace plugin is wired in and the session just landed in
        // `Merged`, remove its worktree. The session YAML stays on disk for
        // history; only the working-directory folder is deleted.
        if session.status == SessionStatus::Merged {
            // Kill the runtime (tmux window) — best-effort.
            if let Some(ref handle) = session.runtime_handle {
                match self.runtime.destroy(handle).await {
                    Ok(()) => tracing::info!(session = %session.id, "→ killed runtime on merge"),
                    Err(e) => {
                        tracing::warn!(session = %session.id, error = %e, "runtime destroy on merge failed")
                    }
                }
            }

            if let Some(ref workspace) = self.workspace {
                if let Some(ref ws_path) = session.workspace_path {
                    match workspace.destroy(ws_path).await {
                        Ok(()) => {
                            tracing::info!(
                                session = %session.id,
                                path = %ws_path.display(),
                                "→ removed worktree"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                session = %session.id,
                                path = %ws_path.display(),
                                error = %e,
                                "worktree cleanup failed"
                            );
                        }
                    }
                }
            }
        }

        // ---- 6. Agent-stuck detection (Phase H) ----
        // Gated on the pre-transition snapshot: if step 4 or 5 already
        // mutated `session.status` this tick, we yield and let the next
        // tick decide whether stuck still applies. Also gated on a
        // reaction engine being configured — without one, there's no
        // `agent-stuck` config to read and no way to emit the tracker
        // event, so the early-return in `check_stuck` would fire anyway
        // but checking here keeps the happy path one branch shorter.
        if self.reaction_engine.is_some() && session.status == pre_transition_status {
            self.check_stuck(&mut session).await?;
        }

        Ok(())
    }

    /// Probe the configured SCM plugin for this session and apply any
    /// status transition the observation implies.
    ///
    /// Structure:
    ///   1. `detect_pr` → `Option<PullRequest>`. `None` skips all field
    ///      probes and lets `derive_scm_status(current, None)` decide
    ///      whether the session should drop off the PR track.
    ///   2. `tokio::join!` fans out `pr_state` / `ci_status` /
    ///      `review_decision` / `mergeability` in parallel so the four
    ///      `gh` calls pay one RTT, not four. Matches `ao-rs pr`.
    ///   3. Failures in any field probe emit `TickError` and skip the
    ///      transition — we'd rather miss a tick than transition on a
    ///      partial observation. Next tick re-probes.
    ///   4. The observation is folded through the pure `derive_scm_status`
    ///      function (see `scm_transitions` module) which returns
    ///      `Some(next)` only when a real transition is warranted.
    async fn poll_scm(&self, session: &mut Session) -> Result<()> {
        // Defense in depth: `tick()` already filters terminal sessions at
        // line ~199, and the activity path in `poll_one` can't currently
        // transition *into* a terminal status before reaching step 5. But
        // the invariant is implicit, not enforced by the type system, and
        // a future step 4 that ends in `Merged`/`Terminated` would bypass
        // the `tick()` filter for the current tick. Re-check here so the
        // SCM probe can never run — or worse, re-transition — a session
        // that some upstream step has already finalised.
        if session.is_terminal() {
            return Ok(());
        }

        let scm = self
            .scm
            .as_ref()
            .expect("poll_scm called without an SCM plugin");

        // ---- 1. Detect PR ----
        // Prefer the pre-detected PR from tick() Pass 1. Fall back to
        // a fresh detect_pr call for tests or edge cases where the
        // cache wasn't populated.
        let pr = {
            let mut cache = self
                .detected_prs_cache
                .lock()
                .expect("detected_prs_cache mutex poisoned");
            cache.remove(&session.id)
        };
        let pr = match pr {
            Some(cached) => cached,
            None => match scm.detect_pr(session).await {
                Ok(pr) => pr,
                Err(e) => {
                    self.emit(OrchestratorEvent::TickError {
                        id: session.id.clone(),
                        message: format!("scm.detect_pr: {e}"),
                    });
                    return Ok(());
                }
            },
        };

        // Build the optional observation.
        let observation = if let Some(pr) = pr {
            // ---- 2. Check batch enrichment cache ----
            let cache_key = format!("{}/{}#{}", pr.owner, pr.repo, pr.number);
            let cached = {
                let mut cache = self
                    .pr_enrichment_cache
                    .lock()
                    .expect("pr_enrichment_cache mutex poisoned");
                cache.remove(&cache_key)
            };

            if let Some(obs) = cached {
                tracing::trace!(
                    "poll_scm: using cached batch observation for PR #{}",
                    pr.number
                );
                Some(obs)
            } else {
                // ---- Review backlog throttle ----
                // When there's no batch cache hit and the session is in a
                // review-related state, skip the expensive REST fallback
                // unless 2+ minutes have passed since the last check.
                if is_review_stable(session.status) {
                    let throttled = {
                        let map = self
                            .last_review_backlog_check
                            .lock()
                            .expect("last_review_backlog_check mutex poisoned");
                        map.get(&session.id)
                            .map(|t| t.elapsed() < REVIEW_BACKLOG_THROTTLE)
                            .unwrap_or(false)
                    };
                    if throttled {
                        tracing::trace!(
                            "poll_scm: review backlog throttled for session {}",
                            session.id.0
                        );
                        return Ok(());
                    }
                }

                // ---- 3. Parallel fan-out (fallback) ----
                let (state_res, ci_res, review_res, readiness_res) = tokio::join!(
                    scm.pr_state(&pr),
                    scm.ci_status(&pr),
                    scm.review_decision(&pr),
                    scm.mergeability(&pr),
                );

                // Record the check timestamp for throttling
                {
                    let mut map = self
                        .last_review_backlog_check
                        .lock()
                        .expect("last_review_backlog_check mutex poisoned");
                    map.insert(session.id.clone(), Instant::now());
                }

                match assemble_observation(state_res, ci_res, review_res, readiness_res) {
                    Ok(obs) => Some(obs),
                    Err(msg) => {
                        self.emit(OrchestratorEvent::TickError {
                            id: session.id.clone(),
                            message: format!("scm probes: {msg}"),
                        });
                        return Ok(());
                    }
                }
            }
        } else {
            None
        };

        // ---- 4. Pure decision + transition ----
        if let Some(mut next) = derive_scm_status(session.status, observation.as_ref()) {
            // TS stuck detection can override the fallback `pr_open` state when
            // the agent has been idle beyond threshold. To preserve the Rust
            // invariant of **one transition per tick**, we apply that override
            // here before persisting/emitting the transition.
            if next == SessionStatus::PrOpen && self.should_mark_stuck(session) {
                next = SessionStatus::Stuck;
            }
            self.transition(session, next).await?;
        }
        Ok(())
    }

    /// Crash-recovery sweep run once at startup.
    ///
    /// Scans all sessions for those already in `Merged` status that still
    /// have a `workspace_path` on disk. This handles the race where the
    /// process was killed after persisting `Merged` but before the per-tick
    /// `destroy()` call completed — terminal sessions are skipped by
    /// `poll_one`, so without this sweep the worktree would live forever.
    async fn sweep_merged_worktrees(&self) {
        let Some(ref workspace) = self.workspace else {
            return;
        };

        let sessions = match self.sessions.list().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("startup worktree sweep: failed to list sessions: {e}");
                return;
            }
        };

        for session in sessions {
            if session.status != SessionStatus::Merged {
                continue;
            }
            let Some(ref ws_path) = session.workspace_path else {
                continue;
            };
            if !ws_path.exists() {
                continue;
            }
            match workspace.destroy(ws_path).await {
                Ok(()) => {
                    tracing::info!(
                        session = %session.id,
                        path = %ws_path.display(),
                        "→ removed worktree (startup sweep)"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        session = %session.id,
                        path = %ws_path.display(),
                        error = %e,
                        "startup worktree sweep: cleanup failed"
                    );
                }
            }
        }
    }

    /// Flip a session to a terminal state, persist, and emit both the
    /// `StatusChanged` (to the chosen terminal `SessionStatus`) and the
    /// `Terminated` event.
    ///
    /// Also clears any reaction trackers the engine was holding for this
    /// session. Without this, a long-running `ao-rs watch` would slowly
    /// leak tracker entries for every terminated session.
    async fn terminate(&self, session: &mut Session, reason: TerminationReason) -> Result<()> {
        // ao-ts parity: runtime/agent exit is `killed`. Rust keeps `terminated`
        // for "exited but not explicitly killed" elsewhere, but lifecycle
        // liveness probes map to `killed` just like ao-ts.
        let terminal_status = match reason {
            TerminationReason::RuntimeGone
            | TerminationReason::AgentExited
            | TerminationReason::NoHandle => SessionStatus::Killed,
        };
        if session.status != terminal_status {
            self.transition(session, terminal_status).await?;
        }
        if let Some(engine) = self.reaction_engine.as_ref() {
            engine.clear_all_for_session(&session.id);
        }
        // Purge per-session bookkeeping so a long-running watch loop
        // doesn't accumulate entries for every session it has ever seen.
        self.idle_since
            .lock()
            .expect("lifecycle idle_since mutex poisoned")
            .remove(&session.id);
        self.last_review_backlog_check
            .lock()
            .expect("last_review_backlog_check mutex poisoned")
            .remove(&session.id);
        self.emit(OrchestratorEvent::Terminated {
            id: session.id.clone(),
            reason,
        });
        Ok(())
    }

    /// Transition status, persist, emit `StatusChanged`, and (if a
    /// reaction engine is attached) dispatch any reaction associated
    /// with the new status.
    ///
    /// Ordering matters: normally we save + emit `StatusChanged` *before*
    /// calling the engine, so subscribers see the transition event in the
    /// right order and so a panicking engine doesn't lose the state change.
    ///
    /// **Phase G parking hook.** When the reaction is `auto-merge` and
    /// the engine reports a non-escalated failure, `transition` persists
    /// the session as `MergeFailed` (instead of `Mergeable`) so the next
    /// SCM tick's `derive_scm_status` can decide whether to retry
    /// (still-ready observation re-promotes to `Mergeable`) or abandon
    /// (flake / closed PR drops off the PR track). Escalated outcomes are
    /// left in `Mergeable` so the retry loop stops and the human
    /// notification stands — see the doc on `should_park_in_merge_failed`.
    async fn transition(&self, session: &mut Session, to: SessionStatus) -> Result<()> {
        if session.status == to {
            return Ok(());
        }
        let from = session.status;
        session.status = to;

        // Poll cost on every status change (not every tick). Only
        // overwrite when the agent returns Some — a None keeps the
        // existing cost intact so we never lose data.
        //
        // `cost_estimate` may do blocking file I/O (JSONL parsing),
        // and `record_cost` writes to disk — both are wrapped in
        // spawn_blocking to avoid starving the Tokio executor.
        match self.agent.cost_estimate(session).await {
            Ok(Some(cost)) => {
                // Best-effort ledger write — don't fail the transition.
                let sid = session.id.0.clone();
                let pid = session.project_id.clone();
                let br = session.branch.clone();
                let c = cost.clone();
                let ca = session.created_at;
                let ledger_result = tokio::task::spawn_blocking(move || {
                    crate::cost_ledger::record_cost(&sid, &pid, &br, &c, ca)
                })
                .await;
                match ledger_result {
                    Ok(Err(e)) => {
                        tracing::warn!(session = %session.id, "cost ledger write failed: {e}");
                    }
                    Err(e) => {
                        tracing::warn!(session = %session.id, "cost ledger task panicked: {e}");
                    }
                    Ok(Ok(())) => {}
                }
                session.cost = Some(cost);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(session = %session.id, "cost_estimate failed: {e}");
            }
        }

        // Phase 1 invariant: **one status transition per session per tick**.
        //
        // The Phase G auto-merge retry loop needs to "park" a just-entered
        // `Mergeable` session in `MergeFailed` when the auto-merge action
        // fails without escalating. Historically this produced two
        // transitions/events in one tick (`… → Mergeable` then
        // `Mergeable → MergeFailed`). To preserve the invariant while
        // keeping reaction dispatch semantics, we decide the *final*
        // persisted status before emitting `StatusChanged`.
        let mut persisted_to = to;
        if let Some(engine) = self.reaction_engine.as_ref() {
            if let Some(next_key) = status_to_reaction_key(to) {
                match engine.dispatch(session, next_key).await {
                    Ok(Some(outcome))
                        if should_park_in_merge_failed(engine, session, to, next_key, &outcome) =>
                    {
                        persisted_to = SessionStatus::MergeFailed;
                        session.status = persisted_to;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            session = %session.id,
                            reaction = next_key,
                            error = %e,
                            "reaction dispatch failed; lifecycle loop continues"
                        );
                    }
                }
            }
        }

        // If the "parking" rewrite lands us back in the original status,
        // this tick should not persist or emit a no-op transition. (The
        // reaction attempt has already been recorded by the engine.)
        if persisted_to == from {
            session.status = from;
            return Ok(());
        }

        self.sessions.save(session).await?;
        self.emit(OrchestratorEvent::StatusChanged {
            id: session.id.clone(),
            from,
            to: persisted_to,
        });

        // Issue #169: notify the parent orchestrator (if any) so it can
        // react to worker state changes without manual human prodding.
        // Only fires for transitions the orchestrator actually needs to
        // see; best-effort, never fails the transition.
        if is_orchestrator_notifiable(persisted_to) {
            self.notify_orchestrator(session, persisted_to).await;
        }

        if let Some(engine) = self.reaction_engine.as_ref() {
            // Leaving a reaction-triggering status? Clear its tracker so
            // the next entry (e.g. new CI failure after a fix) gets a
            // fresh retry budget. Parking-loop transitions
            // (`Mergeable ↔ MergeFailed`) are the exception — see
            // `clear_tracker_on_transition` for the rationale.
            clear_tracker_on_transition(engine, &session.id, from, persisted_to);
        }

        Ok(())
    }

    /// Best-effort delivery of a worker state-change notification to the
    /// parent orchestrator via `Runtime::send_message`. Silent when the
    /// worker has no `spawned_by`, the parent yaml is gone, or the
    /// parent has no live runtime handle — any of those mean "no one to
    /// tell", not "error".
    async fn notify_orchestrator(&self, worker: &Session, to: SessionStatus) {
        let Some(orch_id) = worker.spawned_by.as_ref() else {
            return;
        };
        let parent = match self.sessions.find_by_prefix(&orch_id.0).await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(
                    session = %worker.id,
                    parent = %orch_id.0,
                    "orchestrator session lookup failed: {e}"
                );
                return;
            }
        };
        let Some(handle) = parent.runtime_handle.as_deref() else {
            return;
        };
        let msg = format_orchestrator_notification(worker, to);
        if let Err(e) = self.runtime.send_message(handle, &msg).await {
            tracing::warn!(
                session = %worker.id,
                parent = %parent.id,
                "failed to deliver orchestrator notification: {e}"
            );
        }
    }

    /// Phase H agent-stuck detection. Called from `poll_one` as the
    /// final transitioning step, gated on the pre-transition snapshot
    /// and the presence of a reaction engine.
    ///
    /// Early-returns (without erroring) in every "nothing to do" case:
    ///
    /// 1. Status is not stuck-eligible (terminal, already `Stuck`,
    ///    `NeedsInput`, etc.). Keeps us from double-firing and from
    ///    stuck-classifying states where idleness is expected.
    /// 2. No `idle_since` entry exists — the session is actively doing
    ///    something, so the stuck clock isn't running.
    /// 3. No `agent-stuck` reaction is configured. Missing config is a
    ///    deliberate "disabled" signal, not an error.
    /// 4. The configured `threshold` is absent or unparseable.
    ///    Malformed strings log a one-shot `tracing::warn!` via
    ///    `ReactionEngine::warn_once_parse_failure`, but do not panic.
    /// 5. The idle elapsed time has not yet *strictly* exceeded the
    ///    threshold (`elapsed <= threshold` early-returns; the flip
    ///    fires only on `elapsed > threshold`). Exactly-equal holds
    ///    the clock for one more tick, matching the strict `>` that
    ///    `ReactionEngine::dispatch` uses on `first_triggered_at`.
    ///
    /// Only when all five guards pass do we call `transition` into
    /// `SessionStatus::Stuck`, which in turn dispatches the
    /// `agent-stuck` reaction. Subsequent ticks see `Stuck` (not
    /// stuck-eligible) and stay stable until activity flips back.
    async fn check_stuck(&self, session: &mut Session) -> Result<()> {
        if !self.should_mark_stuck(session) {
            return Ok(());
        }

        // All guards passed: park the session in `Stuck`. The
        // `transition` helper handles `agent-stuck` dispatch via the
        // existing `status_to_reaction_key` path and emits
        // `StatusChanged` on the event bus, so there is nothing
        // extra to do here.
        self.transition(session, SessionStatus::Stuck).await
    }

    /// Maintain the `idle_since` map in response to a fresh activity
    /// reading.
    ///
    /// - `Idle` or `Blocked` → insert `Instant::now()` **only if** the
    ///   session isn't already in the map. Preserving the older timestamp
    ///   means an entry that has been idle for three ticks is still
    ///   three-ticks-old on the fourth tick, so `elapsed()` grows
    ///   monotonically across the idle streak.
    /// - Any other activity state → remove the entry so the next idle
    ///   streak restarts the clock from zero.
    ///
    /// Called unconditionally from `poll_one` after the persist-activity
    /// block. Terminal activities (`Exited`) never reach this helper —
    /// `poll_one` short-circuits to `terminate` beforehand, which clears
    /// the entry via `idle_since.remove` (Task 2.7).
    fn update_idle_since(&self, session_id: &SessionId, activity: ActivityState) {
        let mut map = self
            .idle_since
            .lock()
            .expect("lifecycle idle_since mutex poisoned");
        match activity {
            ActivityState::Idle | ActivityState::Blocked => {
                map.entry(session_id.clone()).or_insert_with(Instant::now);
            }
            _ => {
                map.remove(session_id);
            }
        }
    }

    /// Fire an event into the broadcast channel. A send error only means
    /// there are currently zero subscribers — that's expected during CLI
    /// startup and not worth surfacing.
    fn emit(&self, event: OrchestratorEvent) {
        let _ = self.events_tx.send(event);
    }
}

/// Fold four `Result<_, AoError>` probe results into a single
/// `ScmObservation`, or produce a `"probe_name: error; …"` diagnostic
/// string listing every probe that failed.
///
/// The `poll_scm` tick refuses to transition on a partial observation —
/// it's all or nothing — so the caller's path after this helper is a
/// single `match` between "we have all four fields" and "emit TickError".
///
/// Free function rather than a method because the decision has no
/// `&self` dependencies; extracting it keeps the async fan-out in
/// `poll_scm` readable at one level of abstraction.
fn assemble_observation(
    state: Result<PrState>,
    ci: Result<CiStatus>,
    review: Result<ReviewDecision>,
    readiness: Result<MergeReadiness>,
) -> std::result::Result<ScmObservation, String> {
    match (state, ci, review, readiness) {
        (Ok(state), Ok(ci), Ok(review), Ok(readiness)) => Ok(ScmObservation {
            state,
            ci,
            review,
            readiness,
        }),
        (state, ci, review, readiness) => {
            // Join whichever errors fired into one human-readable
            // message. Each slot contributes `"<slot>: <err>"` or
            // nothing; empty output is impossible because we only hit
            // this arm when at least one was Err.
            let parts: Vec<String> = [
                state.err().map(|e| format!("pr_state: {e}")),
                ci.err().map(|e| format!("ci_status: {e}")),
                review.err().map(|e| format!("review_decision: {e}")),
                readiness.err().map(|e| format!("mergeability: {e}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            Err(parts.join("; "))
        }
    }
}

/// Should the lifecycle park this session in `MergeFailed` after
/// dispatching `approved-and-green`?
///
/// Yes iff we *just* entered `Mergeable`, `reactions.approved-and-green`
/// is configured with `action: auto-merge`, and the dispatch soft-failed
/// *without* escalating. Parking is keyed off the **configured** action,
/// not `ReactionOutcome::action`, so a mismatched or escalated outcome
/// cannot trap a `notify` / `send-to-agent` rule in the merge retry
/// loop.
///
/// Escalated outcomes are deliberately **not** parked: once the retry
/// budget is exhausted the human has been notified, and bouncing the
/// session into `MergeFailed → Mergeable → escalate → Notify → ...`
/// on every tick would spam the notification channel. Leaving the
/// session in `Mergeable` visually says "ready, but auto-merge gave
/// up" — any subsequent observation change (CI flake, reviewer
/// dismissal, branch deletion) will naturally flip it off the ready
/// path via the normal ladder.
fn should_park_in_merge_failed(
    engine: &ReactionEngine,
    session: &Session,
    to: SessionStatus,
    reaction_key: &str,
    outcome: &ReactionOutcome,
) -> bool {
    to == SessionStatus::Mergeable
        && reaction_key == "approved-and-green"
        && engine
            .resolve_reaction_config(session, reaction_key)
            .is_some_and(|c| c.action == ReactionAction::AutoMerge)
        && !outcome.success
        && !outcome.escalated
}

/// Clear reaction trackers on status transitions, with a carve-out
/// for the `Mergeable ↔ MergeFailed` parking loop.
///
/// The default rule is simple: on exit from a reaction-triggering
/// status, clear that reaction's tracker so a future re-entry starts
/// with a full retry budget. Phase G needs a carve-out because the
/// parking loop repeatedly re-enters `Mergeable` on purpose, and the
/// retry budget is supposed to *accumulate* across those re-entries
/// — clearing the tracker on `Mergeable → MergeFailed` or
/// `MergeFailed → Mergeable` would reset attempts to zero and the
/// retry cap would never fire.
///
/// The exit case `MergeFailed → anything_but_Mergeable` (CI flipped
/// red, reviewer dismissed, PR closed) is subtle: the parking loop
/// is over, so a later re-entry from `PrOpen → Mergeable` should
/// start fresh. `status_to_reaction_key(MergeFailed) == None`, so
/// the default-rule branch below wouldn't clear anything — we need
/// an explicit `clear_tracker("approved-and-green")` for this case.
fn clear_tracker_on_transition(
    engine: &ReactionEngine,
    session_id: &SessionId,
    from: SessionStatus,
    to: SessionStatus,
) {
    // Parking-loop edges: preserve the `approved-and-green` tracker
    // so retry accounting accumulates across the loop.
    let parking_loop_edge = matches!(
        (from, to),
        (SessionStatus::Mergeable, SessionStatus::MergeFailed)
            | (SessionStatus::MergeFailed, SessionStatus::Mergeable)
    );
    if parking_loop_edge {
        return;
    }

    // Leaving `MergeFailed` to a non-`Mergeable` state: the retry
    // loop is over (observation moved off the ready path), so clear
    // the parked tracker. The default-rule branch below would miss
    // this because `status_to_reaction_key(MergeFailed) == None`.
    if from == SessionStatus::MergeFailed {
        engine.clear_tracker(session_id, "approved-and-green");
        return;
    }

    // Default rule: clear the `from` reaction's tracker on exit.
    if let Some(prev_key) = status_to_reaction_key(from) {
        engine.clear_tracker(session_id, prev_key);
    }
}

/// Is `status` eligible for stuck detection? I.e., if a session in
/// this status has been observed with `Idle`/`Blocked` activity for
/// longer than the `agent-stuck` reaction's `threshold`, should it be
/// flipped to `Stuck`?
///
/// Phase H. The set matches the "work in progress" statuses where a
/// silent agent is genuinely unexpected:
///
/// - `Working`: happy path coder lost its train of thought.
/// - `PrOpen` / `CiFailed` / `ReviewPending` / `ChangesRequested`
///   / `Approved` / `Mergeable`: PR-track states where the agent is
///   waiting on or reacting to CI / a reviewer, and should be
///   actively working (applying review comments, re-running tests,
///   responding to CI failures).
///
/// Excluded statuses — and why each is excluded — are enumerated
/// exhaustively in the match below. The match has **no wildcard**: a
/// future `SessionStatus` variant will fail the build here until
/// stuck-eligibility is decided for it. Same discipline as the
/// `ALL_SESSION_STATUSES` exhaustiveness test in `scm_transitions.rs`.
/// Returns `true` for session statuses where the review observation is
/// unlikely to change within a few seconds (the session is waiting on a
/// human review or has changes requested). Used by the review backlog
/// throttle to skip redundant REST API calls.
const fn is_review_stable(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::ChangesRequested | SessionStatus::ReviewPending | SessionStatus::Approved
    )
}

/// Transitions that warrant notifying the parent orchestrator (issue #169).
///
/// Chosen so the orchestrator only gets messages that require a decision
/// or at least a "status FYI" — not every intermediate tick. Noisy or
/// purely-informational states (e.g. `Working`, `Spawning`, intermediate
/// review flips) are intentionally excluded; the human/CLI can still see
/// them via `ao-rs status` or the SSE event stream.
const fn is_orchestrator_notifiable(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::PrOpen
            | SessionStatus::ReviewPending
            | SessionStatus::CiFailed
            | SessionStatus::ChangesRequested
            | SessionStatus::Approved
            | SessionStatus::Merged
            | SessionStatus::MergeFailed
            | SessionStatus::Killed
            | SessionStatus::Terminated
            | SessionStatus::Errored
            | SessionStatus::NeedsInput
            | SessionStatus::Stuck
    )
}

/// Render the notification message the orchestrator sees in its
/// terminal. Kept short and parseable — the orchestrator is usually an
/// LLM agent that re-reads its scrollback.
fn format_orchestrator_notification(worker: &Session, to: SessionStatus) -> String {
    let short: String = worker.id.0.chars().take(8).collect();
    let pr = worker
        .claimed_pr_url
        .as_deref()
        .or(worker.issue_url.as_deref())
        .unwrap_or("none");
    format!(
        "[ao-rs] worker {short} is now {to} — branch: {branch}, url: {pr}",
        branch = worker.branch,
    )
}

const fn is_stuck_eligible(status: SessionStatus) -> bool {
    match status {
        // Stuck-eligible: active work or PR-track where progress is expected.
        SessionStatus::Working
        | SessionStatus::PrOpen
        | SessionStatus::CiFailed
        | SessionStatus::ReviewPending
        | SessionStatus::ChangesRequested
        | SessionStatus::Approved
        | SessionStatus::Mergeable => true,

        // Not stuck-eligible:
        //
        // - Spawning: agent hasn't had its first activity poll yet;
        //   idle_since would never populate for this state anyway.
        // - Idle: the dedicated "no task assigned / waiting for work"
        //   status, distinct from "currently working and momentarily
        //   gone idle". A session in `Idle` is idle by design.
        // - NeedsInput: already a known-blocked-on-human state with
        //   its own (future) `agent-needs-input` reaction.
        // - Stuck: already stuck. Re-entry is handled by the
        //   `Stuck → Working` exit branch in `poll_one` step 4.
        // - MergeFailed: Phase G parking state with its own retry
        //   budget via the `approved-and-green` tracker. Conflating
        //   with stuck would double-charge retries and confuse the
        //   parking-loop accounting.
        // - Terminal states (`Killed`, `Terminated`, `Done`,
        //   `Cleanup`, `Errored`, `Merged`): filtered out by the
        //   `tick()` pre-filter long before `check_stuck` is called.
        //   Listed here so the exhaustive match stays exhaustive.
        SessionStatus::Spawning
        | SessionStatus::Idle
        | SessionStatus::NeedsInput
        | SessionStatus::Stuck
        | SessionStatus::MergeFailed
        | SessionStatus::Killed
        | SessionStatus::Terminated
        | SessionStatus::Done
        | SessionStatus::Cleanup
        | SessionStatus::Errored
        | SessionStatus::Merged => false,
    }
}

/// Handle returned by `LifecycleManager::spawn`. Dropping it does **not**
/// stop the loop — the caller must `.stop().await` explicitly, so a
/// CLI handler that accidentally drops the handle doesn't silently kill
/// the background worker.
pub struct LifecycleHandle {
    join: tokio::task::JoinHandle<()>,
    token: CancellationToken,
}

impl LifecycleHandle {
    /// Signal the loop to stop and wait for it to finish the current tick.
    pub async fn stop(self) {
        self.token.cancel();
        let _ = self.join.await;
    }

    /// Clone the cancellation token so sub-tasks can share shutdown.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scm::{
        CheckRun, CiStatus, MergeMethod, MergeReadiness, PrState, PullRequest, Review,
        ReviewComment, ReviewDecision,
    };
    use crate::traits::Workspace;
    use crate::types::{now_ms, SessionId, WorkspaceCreateConfig};
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ao-rs-lifecycle-{label}-{nanos}-{n}"))
    }

    fn fake_session(id: &str, project: &str) -> Session {
        Session {
            id: SessionId(id.into()),
            project_id: project.into(),
            status: SessionStatus::Spawning,
            agent: "claude-code".into(),
            agent_config: None,
            branch: format!("ao-{id}"),
            task: "test task".into(),
            workspace_path: Some(PathBuf::from("/tmp/ws")),
            runtime_handle: Some(format!("runtime-{id}")),
            runtime: "tmux".into(),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: None,
            spawned_by: None,
        }
    }

    // ---------- Mock plugins ---------- //

    /// Runtime mock with a toggleable `alive` flag and a recorder for
    /// `send_message` calls. Tests that care about delivery can read
    /// the recorder via `MockRuntime::sends()`; others ignore it.
    struct MockRuntime {
        alive: AtomicBool,
        sends: Mutex<Vec<(String, String)>>,
        destroys: Mutex<Vec<String>>,
    }

    impl MockRuntime {
        fn new(alive: bool) -> Self {
            Self {
                alive: AtomicBool::new(alive),
                sends: Mutex::new(Vec::new()),
                destroys: Mutex::new(Vec::new()),
            }
        }

        /// Snapshot of all `(handle, message)` pairs received by
        /// `send_message` in the order they were called.
        fn sends(&self) -> Vec<(String, String)> {
            self.sends.lock().unwrap().clone()
        }

        #[allow(dead_code)]
        fn destroyed_handles(&self) -> Vec<String> {
            self.destroys.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Runtime for MockRuntime {
        async fn create(
            &self,
            _session_id: &str,
            _cwd: &Path,
            _launch_command: &str,
            _env: &[(String, String)],
        ) -> Result<String> {
            Ok("mock-handle".into())
        }
        async fn send_message(&self, handle: &str, msg: &str) -> Result<()> {
            self.sends
                .lock()
                .unwrap()
                .push((handle.to_string(), msg.to_string()));
            Ok(())
        }
        async fn is_alive(&self, _handle: &str) -> Result<bool> {
            Ok(self.alive.load(Ordering::SeqCst))
        }
        async fn destroy(&self, handle: &str) -> Result<()> {
            self.destroys.lock().unwrap().push(handle.to_string());
            Ok(())
        }
    }

    /// Agent mock that returns a scripted activity state each call.
    struct MockAgent {
        next: Mutex<ActivityState>,
    }

    impl MockAgent {
        fn new(initial: ActivityState) -> Self {
            Self {
                next: Mutex::new(initial),
            }
        }
        fn set(&self, state: ActivityState) {
            *self.next.lock().unwrap() = state;
        }
    }

    #[async_trait]
    impl Agent for MockAgent {
        fn launch_command(&self, _session: &Session) -> String {
            "mock".into()
        }
        fn environment(&self, _session: &Session) -> Vec<(String, String)> {
            vec![]
        }
        fn initial_prompt(&self, _session: &Session) -> String {
            "".into()
        }
        async fn detect_activity(&self, _session: &Session) -> Result<ActivityState> {
            Ok(*self.next.lock().unwrap())
        }
    }

    #[allow(dead_code)]
    struct MockWorkspace {
        destroyed: Mutex<Vec<PathBuf>>,
    }

    #[allow(dead_code)]
    impl MockWorkspace {
        fn new() -> Self {
            Self {
                destroyed: Mutex::new(Vec::new()),
            }
        }

        fn destroyed_paths(&self) -> Vec<PathBuf> {
            self.destroyed.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Workspace for MockWorkspace {
        async fn create(&self, _cfg: &WorkspaceCreateConfig) -> Result<PathBuf> {
            Ok(PathBuf::from("/tmp/ws"))
        }
        async fn destroy(&self, workspace_path: &Path) -> Result<()> {
            self.destroyed
                .lock()
                .unwrap()
                .push(workspace_path.to_path_buf());
            Ok(())
        }
    }

    /// Scriptable SCM mock. Every method returns a value pre-set by the
    /// test. `detect_pr` returns `None` until `set_pr(Some(_))`. Each field
    /// probe (`pr_state`, `ci_status`, …) can be toggled to emit an error
    /// so tests cover the `TickError` branch of `poll_scm`.
    ///
    /// Kept intentionally minimal — we only test the handful of methods
    /// `poll_scm` calls. `pending_comments`, `reviews`, `ci_checks`, `merge`
    /// return empty/default values (enough to satisfy the trait).
    struct MockScm {
        pr: Mutex<Option<PullRequest>>,
        state: Mutex<PrState>,
        ci: Mutex<CiStatus>,
        review: Mutex<ReviewDecision>,
        readiness: Mutex<MergeReadiness>,
        // Counter: incremented on every detect_pr call, so tests can assert
        // "the loop actually called the plugin".
        detect_calls: AtomicUsize,
        // Error toggles. One per probe that `poll_scm` fans out to, so
        // individual tests can force exactly one slot to fail and
        // verify the error-aggregation path reports that slot by name.
        detect_pr_errors: AtomicBool,
        pr_state_errors: AtomicBool,
        ci_status_errors: AtomicBool,
        review_decision_errors: AtomicBool,
        mergeability_errors: AtomicBool,
        // Phase G: `merge()` error toggle + call recorder so parking-loop
        // tests can script "fail first, succeed later" and assert that
        // the engine actually called the plugin the expected number of
        // times.
        merge_errors: AtomicBool,
        merge_calls: Mutex<Vec<(u32, Option<MergeMethod>)>>,
    }

    impl MockScm {
        fn new() -> Self {
            Self {
                pr: Mutex::new(None),
                state: Mutex::new(PrState::Open),
                ci: Mutex::new(CiStatus::Pending),
                review: Mutex::new(ReviewDecision::None),
                readiness: Mutex::new(MergeReadiness {
                    mergeable: false,
                    ci_passing: false,
                    approved: false,
                    no_conflicts: true,
                    blockers: vec!["pending".into()],
                }),
                detect_calls: AtomicUsize::new(0),
                detect_pr_errors: AtomicBool::new(false),
                pr_state_errors: AtomicBool::new(false),
                ci_status_errors: AtomicBool::new(false),
                review_decision_errors: AtomicBool::new(false),
                mergeability_errors: AtomicBool::new(false),
                merge_errors: AtomicBool::new(false),
                merge_calls: Mutex::new(Vec::new()),
            }
        }
        fn merges(&self) -> Vec<(u32, Option<MergeMethod>)> {
            self.merge_calls.lock().unwrap().clone()
        }
        fn set_pr(&self, pr: Option<PullRequest>) {
            *self.pr.lock().unwrap() = pr;
        }
        fn set_state(&self, s: PrState) {
            *self.state.lock().unwrap() = s;
        }
        fn set_ci(&self, c: CiStatus) {
            *self.ci.lock().unwrap() = c;
        }
        fn set_review(&self, r: ReviewDecision) {
            *self.review.lock().unwrap() = r;
        }
        fn set_readiness(&self, r: MergeReadiness) {
            *self.readiness.lock().unwrap() = r;
        }
    }

    #[async_trait]
    impl Scm for MockScm {
        fn name(&self) -> &str {
            "mock"
        }
        async fn detect_pr(&self, _session: &Session) -> Result<Option<PullRequest>> {
            self.detect_calls.fetch_add(1, Ordering::SeqCst);
            if self.detect_pr_errors.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("mock detect_pr".into()));
            }
            Ok(self.pr.lock().unwrap().clone())
        }
        async fn pr_state(&self, _pr: &PullRequest) -> Result<PrState> {
            if self.pr_state_errors.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("mock pr_state".into()));
            }
            Ok(*self.state.lock().unwrap())
        }
        async fn ci_checks(&self, _pr: &PullRequest) -> Result<Vec<CheckRun>> {
            Ok(vec![])
        }
        async fn ci_status(&self, _pr: &PullRequest) -> Result<CiStatus> {
            if self.ci_status_errors.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("mock ci_status".into()));
            }
            Ok(*self.ci.lock().unwrap())
        }
        async fn reviews(&self, _pr: &PullRequest) -> Result<Vec<Review>> {
            Ok(vec![])
        }
        async fn review_decision(&self, _pr: &PullRequest) -> Result<ReviewDecision> {
            if self.review_decision_errors.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime(
                    "mock review_decision".into(),
                ));
            }
            Ok(*self.review.lock().unwrap())
        }
        async fn pending_comments(&self, _pr: &PullRequest) -> Result<Vec<ReviewComment>> {
            Ok(vec![])
        }
        async fn mergeability(&self, _pr: &PullRequest) -> Result<MergeReadiness> {
            if self.mergeability_errors.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("mock mergeability".into()));
            }
            Ok(self.readiness.lock().unwrap().clone())
        }
        async fn merge(&self, pr: &PullRequest, method: Option<MergeMethod>) -> Result<()> {
            if self.merge_errors.load(Ordering::SeqCst) {
                return Err(crate::error::AoError::Runtime("mock merge".into()));
            }
            self.merge_calls.lock().unwrap().push((pr.number, method));
            Ok(())
        }
    }

    fn fake_pr(number: u32, branch: &str) -> PullRequest {
        PullRequest {
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: "fix the widgets".into(),
            owner: "acme".into(),
            repo: "widgets".into(),
            branch: branch.into(),
            base_branch: "main".into(),
            is_draft: false,
        }
    }

    // ---------- Test helpers ---------- //

    async fn setup(
        label: &str,
        initial_activity: ActivityState,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockRuntime>,
        Arc<MockAgent>,
        PathBuf,
    ) {
        let base = unique_temp_dir(label);
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent = Arc::new(MockAgent::new(initial_activity));
        let lifecycle = Arc::new(LifecycleManager::new(
            sessions.clone(),
            runtime.clone() as Arc<dyn Runtime>,
            agent.clone() as Arc<dyn Agent>,
        ));
        (lifecycle, sessions, runtime, agent, base)
    }

    async fn recv_timeout(
        rx: &mut broadcast::Receiver<OrchestratorEvent>,
    ) -> Option<OrchestratorEvent> {
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .ok()
            .and_then(|r| r.ok())
    }

    // ---------- Tests ---------- //

    #[tokio::test]
    async fn first_tick_emits_spawned_and_transitions_to_working() {
        let (lifecycle, sessions, _rt, _agent, base) = setup("spawned", ActivityState::Ready).await;
        let mut rx = lifecycle.subscribe();
        sessions.save(&fake_session("s1", "demo")).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        // Drain events for this tick.
        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        // Must include: Spawned, ActivityChanged, StatusChanged(Spawning → Working).
        assert!(
            events
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::Spawned { .. })),
            "expected Spawned event, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ActivityChanged {
                    next: ActivityState::Ready,
                    ..
                }
            )),
            "expected ActivityChanged → Ready, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Spawning,
                    to: SessionStatus::Working,
                    ..
                }
            )),
            "expected StatusChanged Spawning → Working, got {events:?}"
        );

        // Persisted state must reflect the transition.
        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].status, SessionStatus::Working);
        assert_eq!(persisted[0].activity, Some(ActivityState::Ready));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn dead_runtime_terminates_session() {
        let (lifecycle, sessions, rt, _agent, base) = setup("dead", ActivityState::Ready).await;
        let mut rx = lifecycle.subscribe();
        sessions.save(&fake_session("s1", "demo")).await.unwrap();

        // Runtime is dead from the start.
        rt.alive.store(false, Ordering::SeqCst);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::Terminated {
                    reason: TerminationReason::RuntimeGone,
                    ..
                }
            )),
            "expected Terminated(RuntimeGone), got {events:?}"
        );

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Killed);
        assert!(persisted[0].is_terminal());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn exited_activity_terminates_with_agent_reason() {
        let (lifecycle, sessions, _rt, agent, base) = setup("exited", ActivityState::Ready).await;
        let mut rx = lifecycle.subscribe();
        sessions.save(&fake_session("s1", "demo")).await.unwrap();
        agent.set(ActivityState::Exited);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::Terminated {
                    reason: TerminationReason::AgentExited,
                    ..
                }
            )),
            "expected Terminated(AgentExited), got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn terminal_sessions_are_skipped_on_subsequent_ticks() {
        let (lifecycle, sessions, _rt, _agent, base) = setup("skip", ActivityState::Ready).await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Done; // already terminal
        sessions.save(&s).await.unwrap();

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        // Should emit Spawned (first sight) and nothing else — no
        // ActivityChanged, no StatusChanged.
        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert_eq!(events.len(), 1, "got {events:?}");
        assert!(matches!(&events[0], OrchestratorEvent::Spawned { .. }));

        // And the persisted status must be untouched.
        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Done);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn spawned_is_emitted_only_once_per_session() {
        let (lifecycle, sessions, _rt, _agent, base) = setup("once", ActivityState::Ready).await;
        sessions.save(&fake_session("s1", "demo")).await.unwrap();
        let mut rx = lifecycle.subscribe();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut spawned_count = 0;
        while let Some(e) = recv_timeout(&mut rx).await {
            if matches!(e, OrchestratorEvent::Spawned { .. }) {
                spawned_count += 1;
            }
        }
        assert_eq!(spawned_count, 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Reaction engine integration (Phase D) ---------- //

    use crate::reactions::{ReactionAction, ReactionConfig};

    fn build_engine_with_ci_failed(
        lifecycle: &LifecycleManager,
        message: &str,
    ) -> Arc<ReactionEngine> {
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some(message.into());
        let mut map = std::collections::HashMap::new();
        map.insert("ci-failed".into(), cfg);

        // Engine runs against its own MockRuntime — the integration tests
        // assert wiring via the shared broadcast channel (events), not via
        // runtime side-effects. `events_sender()` makes sure the engine's
        // events reach subscribers of `lifecycle.subscribe()`.
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        Arc::new(ReactionEngine::new(map, runtime, lifecycle.events_sender()))
    }

    #[tokio::test]
    async fn transition_into_ci_failed_dispatches_reaction_on_shared_channel() {
        // Build lifecycle, attach a reaction engine that shares the
        // lifecycle's broadcast channel, then push a session through
        // Working → CiFailed via the `transition` private helper. We're
        // asserting that the engine wiring fires on the same channel
        // that emits StatusChanged.
        let base = unique_temp_dir("reaction-transition");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);
        let engine = build_engine_with_ci_failed(&lifecycle, "fix CI please");
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine.clone()));

        let mut rx = lifecycle.subscribe();

        // Seed a session in Working, then transition into CiFailed.
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();
        lifecycle
            .transition(&mut s, SessionStatus::CiFailed)
            .await
            .unwrap();

        // Collect events synchronously from the broadcast channel.
        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::CiFailed,
                    ..
                }
            )),
            "expected StatusChanged to CiFailed, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ReactionTriggered {
                    action: ReactionAction::SendToAgent,
                    ..
                }
            )),
            "expected ReactionTriggered(SendToAgent) from engine, got {events:?}"
        );

        // Engine tracker should now hold one attempt for the reaction.
        assert_eq!(engine.attempts(&s.id, "ci-failed"), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn leaving_reaction_status_clears_tracker() {
        // Same setup as above, but after firing ci-failed we transition
        // the session back out to Working — lifecycle must ask the engine
        // to clear the tracker so a future CI failure starts fresh.
        let base = unique_temp_dir("reaction-clear");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);
        let engine = build_engine_with_ci_failed(&lifecycle, "fix");
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine.clone()));

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();
        lifecycle
            .transition(&mut s, SessionStatus::CiFailed)
            .await
            .unwrap();
        assert_eq!(engine.attempts(&s.id, "ci-failed"), 1);

        // CI goes green → back to PrOpen (not a reaction key).
        lifecycle
            .transition(&mut s, SessionStatus::PrOpen)
            .await
            .unwrap();
        assert_eq!(
            engine.attempts(&s.id, "ci-failed"),
            0,
            "tracker should be cleared on exit from CiFailed"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn unrelated_transition_does_not_touch_reaction_engine() {
        // Transitioning Spawning → Working (normal happy path) must not
        // fire a reaction — there's no reaction keyed to Working.
        let base = unique_temp_dir("no-react");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);
        let engine = build_engine_with_ci_failed(&lifecycle, "never fires");
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine.clone()));

        let mut rx = lifecycle.subscribe();
        sessions.save(&fake_session("s1", "demo")).await.unwrap();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        // Normal Spawning → Working happy path emits StatusChanged but
        // no ReactionTriggered / ReactionEscalated.
        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::StatusChanged {
                to: SessionStatus::Working,
                ..
            }
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::ReactionTriggered { .. })),
            "unexpected ReactionTriggered on Working transition: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Phase H: is_stuck_eligible classification ---------- //

    /// Every `SessionStatus` variant, in declaration order. Mirrors the
    /// same-named constant in `scm_transitions.rs` — we keep a local
    /// copy rather than re-exporting so each module's exhaustiveness
    /// test is self-contained. Adding a new variant breaks the
    /// `all_session_statuses_list_is_exhaustive_for_stuck_check` test
    /// below until classification is decided.
    const ALL_SESSION_STATUSES: &[SessionStatus] = &[
        SessionStatus::Spawning,
        SessionStatus::Working,
        SessionStatus::NeedsInput,
        SessionStatus::Idle,
        SessionStatus::Stuck,
        SessionStatus::PrOpen,
        SessionStatus::CiFailed,
        SessionStatus::ReviewPending,
        SessionStatus::ChangesRequested,
        SessionStatus::Approved,
        SessionStatus::Mergeable,
        SessionStatus::MergeFailed,
        SessionStatus::Cleanup,
        SessionStatus::Merged,
        SessionStatus::Killed,
        SessionStatus::Terminated,
        SessionStatus::Done,
        SessionStatus::Errored,
    ];

    #[test]
    fn all_session_statuses_list_is_exhaustive_for_stuck_check() {
        // Compile-time exhaustiveness: if a new `SessionStatus` variant
        // lands, this match fails to compile until it's added. That
        // forces a conscious classification decision for
        // `is_stuck_eligible` at the same time. Matches the pattern in
        // `scm_transitions.rs::all_session_statuses_list_is_exhaustive`.
        for status in ALL_SESSION_STATUSES {
            match status {
                SessionStatus::Spawning
                | SessionStatus::Working
                | SessionStatus::NeedsInput
                | SessionStatus::Idle
                | SessionStatus::Stuck
                | SessionStatus::PrOpen
                | SessionStatus::CiFailed
                | SessionStatus::ReviewPending
                | SessionStatus::ChangesRequested
                | SessionStatus::Approved
                | SessionStatus::Mergeable
                | SessionStatus::MergeFailed
                | SessionStatus::Cleanup
                | SessionStatus::Merged
                | SessionStatus::Killed
                | SessionStatus::Terminated
                | SessionStatus::Done
                | SessionStatus::Errored => {}
            }
        }
    }

    #[test]
    fn is_stuck_eligible_classifies_every_variant() {
        // Lock in the Phase H classification. Any change to
        // `is_stuck_eligible` that flips a variant's answer must update
        // this assertion table alongside — it's the contract for which
        // statuses can emit the `agent-stuck` reaction.
        for &status in ALL_SESSION_STATUSES {
            let expected = matches!(
                status,
                SessionStatus::Working
                    | SessionStatus::PrOpen
                    | SessionStatus::CiFailed
                    | SessionStatus::ReviewPending
                    | SessionStatus::ChangesRequested
                    | SessionStatus::Approved
                    | SessionStatus::Mergeable
            );
            assert_eq!(
                is_stuck_eligible(status),
                expected,
                "is_stuck_eligible({status:?}) classification mismatch"
            );
        }
    }

    #[test]
    fn is_stuck_eligible_excludes_merge_failed() {
        // Phase G parking state must not be stuck-eligible or the
        // parking loop's retry accounting would double-charge with
        // the stuck path. Explicit named test because the planning
        // doc lists this as a specific regression risk.
        assert!(!is_stuck_eligible(SessionStatus::MergeFailed));
    }

    #[test]
    fn is_stuck_eligible_excludes_needs_input() {
        // Needs-input is a known-blocked-on-human state and will get
        // its own `agent-needs-input` reaction in a later phase.
        // Conflating with stuck would fire two reactions on every
        // idle-NeedsInput session.
        assert!(!is_stuck_eligible(SessionStatus::NeedsInput));
    }

    // ---------- idle_since bookkeeping (Phase H) ---------- //

    #[tokio::test]
    async fn update_idle_since_inserts_preserves_and_clears() {
        let (lifecycle, _sessions, _rt, _agent, _base) =
            setup("idle_since_helper", ActivityState::Idle).await;
        let id = SessionId("sess-idle".into());

        let read_entry = |lm: &LifecycleManager| -> Option<Instant> {
            lm.idle_since
                .lock()
                .expect("idle_since mutex poisoned")
                .get(&id)
                .copied()
        };

        // Fresh lifecycle — nothing recorded yet.
        assert!(read_entry(&lifecycle).is_none());

        // First Idle flip inserts a timestamp.
        lifecycle.update_idle_since(&id, ActivityState::Idle);
        let t1 = read_entry(&lifecycle).expect("first idle should insert");

        // Second Idle call preserves the existing timestamp — we want
        // `elapsed()` to grow across a streak of idle ticks, not reset.
        lifecycle.update_idle_since(&id, ActivityState::Idle);
        let t2 = read_entry(&lifecycle).expect("second idle should keep entry");
        assert_eq!(t1, t2, "idle → idle must not reset the timestamp");

        // Blocked is also stuck-eligible and must not reset the clock.
        lifecycle.update_idle_since(&id, ActivityState::Blocked);
        let t3 = read_entry(&lifecycle).expect("blocked should keep entry");
        assert_eq!(t1, t3, "idle → blocked must not reset the timestamp");

        // Any non-idle activity clears the entry so the next streak
        // starts the clock over.
        lifecycle.update_idle_since(&id, ActivityState::Active);
        assert!(
            read_entry(&lifecycle).is_none(),
            "active activity must clear idle_since"
        );

        // A fresh Idle afterwards re-inserts (different timestamp — but
        // we only need to know an entry exists).
        lifecycle.update_idle_since(&id, ActivityState::Idle);
        assert!(
            read_entry(&lifecycle).is_some(),
            "idle after clear must re-insert"
        );

        // WaitingInput is NOT stuck-eligible — a session prompting the
        // human is not silently stuck, so it should reset the idle
        // clock just like Active. Ready behaves the same way.
        lifecycle.update_idle_since(&id, ActivityState::WaitingInput);
        assert!(
            read_entry(&lifecycle).is_none(),
            "waiting_input must clear idle_since"
        );

        lifecycle.update_idle_since(&id, ActivityState::Idle);
        lifecycle.update_idle_since(&id, ActivityState::Ready);
        assert!(
            read_entry(&lifecycle).is_none(),
            "ready must clear idle_since"
        );
    }

    #[tokio::test]
    async fn update_idle_since_tracks_sessions_independently() {
        let (lifecycle, _sessions, _rt, _agent, _base) =
            setup("idle_since_multi", ActivityState::Idle).await;
        let a = SessionId("sess-a".into());
        let b = SessionId("sess-b".into());

        lifecycle.update_idle_since(&a, ActivityState::Idle);
        lifecycle.update_idle_since(&b, ActivityState::Idle);

        // Clearing one does not touch the other — different sessions
        // have independent idle streaks.
        lifecycle.update_idle_since(&a, ActivityState::Active);

        let map = lifecycle
            .idle_since
            .lock()
            .expect("idle_since mutex poisoned");
        assert!(!map.contains_key(&a), "sess-a should have been cleared");
        assert!(map.contains_key(&b), "sess-b should still be idle");
    }

    // ---------- Phase H: agent-stuck detection integration ---------- //

    /// Build a `LifecycleManager` with a MockAgent in `Idle`, plus a
    /// reaction engine configured with `agent-stuck` → Notify and the
    /// given threshold string.
    ///
    /// `threshold` is accepted verbatim so tests can exercise malformed
    /// input; callers that want to disable stuck detection entirely
    /// should not call this helper.
    async fn setup_stuck(
        label: &str,
        threshold: Option<&str>,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockAgent>,
        PathBuf,
    ) {
        let base = unique_temp_dir(label);
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent = Arc::new(MockAgent::new(ActivityState::Idle));

        let lifecycle =
            LifecycleManager::new(sessions.clone(), runtime, agent.clone() as Arc<dyn Agent>);

        let mut cfg = ReactionConfig::new(ReactionAction::Notify);
        cfg.message = Some("stuck!".into());
        cfg.threshold = threshold.map(String::from);
        let mut map = std::collections::HashMap::new();
        map.insert("agent-stuck".into(), cfg);
        let engine_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine));
        (lifecycle, sessions, agent, base)
    }

    /// Build a lifecycle without an `agent-stuck` reaction configured
    /// at all. Used to prove `check_stuck` is a strict no-op when no
    /// config exists, independent of whether any other reactions are
    /// set up.
    async fn setup_stuck_no_config(
        label: &str,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockAgent>,
        PathBuf,
    ) {
        let base = unique_temp_dir(label);
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent = Arc::new(MockAgent::new(ActivityState::Idle));

        let lifecycle =
            LifecycleManager::new(sessions.clone(), runtime, agent.clone() as Arc<dyn Agent>);

        // Engine with an UNRELATED reaction keyed — not `agent-stuck`.
        let mut cfg = ReactionConfig::new(ReactionAction::Notify);
        cfg.message = Some("other".into());
        let mut map = std::collections::HashMap::new();
        map.insert("ci-failed".into(), cfg);
        let engine_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(lifecycle.with_reaction_engine(engine));
        (lifecycle, sessions, agent, base)
    }

    /// Collect every event currently buffered on `rx`, draining with a
    /// short timeout so the test doesn't hang on an empty channel.
    async fn drain_events(
        rx: &mut broadcast::Receiver<OrchestratorEvent>,
    ) -> Vec<OrchestratorEvent> {
        let mut out = Vec::new();
        while let Some(e) = recv_timeout(rx).await {
            out.push(e);
        }
        out
    }

    /// Rewind the idle_since entry for `session_id` by `by` so the
    /// next `check_stuck` sees an already-elapsed threshold without a
    /// real `tokio::time::sleep`. The helper inserts the entry if it
    /// doesn't exist yet. Tests call this AFTER tick 1 has already
    /// populated the map.
    fn rewind_idle_since(lifecycle: &LifecycleManager, session_id: &SessionId, by: Duration) {
        let mut map = lifecycle
            .idle_since
            .lock()
            .expect("idle_since mutex poisoned");
        let rewound = Instant::now()
            .checked_sub(by)
            .expect("test clock rewind underflowed Instant");
        map.insert(session_id.clone(), rewound);
    }

    #[tokio::test]
    async fn one_transition_per_tick_prefers_scm_transition_over_stuck() {
        // If a session is already beyond the stuck threshold *and* SCM
        // observes a PR transition (e.g. Working → CiFailed) on the same
        // tick, the lifecycle must perform only the SCM-driven transition.
        //
        // This mirrors the TS `determineStatus` behavior where PR/CI/review
        // semantics win over stuck detection in a single poll cycle.
        let base = unique_temp_dir("one_transition_per_tick_scm_over_stuck");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Idle));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);

        // Reaction engine includes both `agent-stuck` (threshold) and
        // `ci-failed` so both would be eligible if allowed.
        let mut stuck_cfg = ReactionConfig::new(ReactionAction::Notify);
        stuck_cfg.threshold = Some("1s".into());
        let ci_cfg = ReactionConfig::new(ReactionAction::Notify);
        let mut map = std::collections::HashMap::new();
        map.insert("agent-stuck".into(), stuck_cfg);
        map.insert("ci-failed".into(), ci_cfg);
        let engine_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime,
            lifecycle.events_sender(),
        ));

        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine)
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        // Pre-seed idle_since as if we've been idle for 2s (> 1s threshold).
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));

        // Script SCM to force a status transition on this tick.
        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::Pending);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::CiFailed,
                    ..
                }
            )),
            "expected Working → CiFailed transition, got {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "must NOT transition to Stuck on same tick as SCM transition: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_overrides_pr_open_in_same_tick_when_idle_beyond_threshold() {
        // TS determineStatus can return `stuck` instead of the fallback `pr_open`
        // when an agent has a PR open but has been idle beyond the configured
        // threshold. Rust matches this by overriding a would-be `PrOpen`
        // transition inside `poll_scm` so we still do only one transition.
        let base = unique_temp_dir("stuck_overrides_pr_open");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Idle));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);

        let mut stuck_cfg = ReactionConfig::new(ReactionAction::Notify);
        stuck_cfg.threshold = Some("1s".into());
        let mut map = std::collections::HashMap::new();
        map.insert("agent-stuck".into(), stuck_cfg);
        let engine_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime,
            lifecycle.events_sender(),
        ));

        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine)
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        // Pre-seed idle_since as if we've been idle for 2s (> 1s threshold).
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));

        // Script SCM to the "fallback pr_open" case: open PR, no failures, no approvals yet.
        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::None);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "expected Working → Stuck transition, got {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::PrOpen,
                    ..
                }
            )),
            "must not emit an intermediate PrOpen transition: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_detection_fires_on_working_after_threshold() {
        // Session starts in Working, MockAgent reports Idle. After the
        // threshold has elapsed we expect a transition to Stuck and
        // the agent-stuck reaction to fire on the shared event bus.
        //
        // Rather than sleep, we rewind `idle_since` to simulate a
        // long idle streak — deterministic and fast.
        let (lifecycle, sessions, _agent, base) =
            setup_stuck("stuck_from_working", Some("1s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        // First tick: activity flips to Idle, idle_since is set, but
        // elapsed is microseconds — no stuck transition yet.
        lifecycle.tick(&mut seen).await.unwrap();
        let early = drain_events(&mut rx).await;
        assert!(
            !early.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "stuck transition must not fire before threshold elapses: {early:?}"
        );

        // Simulate the session having been idle for 2s (> 1s threshold).
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let later = drain_events(&mut rx).await;
        assert!(
            later.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "expected Working → Stuck transition after threshold; got {later:?}"
        );
        assert!(
            later
                .iter()
                .any(|e| matches!(e, OrchestratorEvent::ReactionTriggered { .. })),
            "expected ReactionTriggered for agent-stuck; got {later:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_detection_fires_on_pr_open_after_threshold() {
        // PrOpen is stuck-eligible — an agent that opened a PR and
        // then went silent is just as stuck as one stuck in Working.
        let (lifecycle, sessions, _agent, base) =
            setup_stuck("stuck_from_pr_open", Some("1s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s2", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::PrOpen,
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "expected PrOpen → Stuck after threshold; got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_recovers_to_working_on_active_activity() {
        // After parking in Stuck, flipping the agent back to Active
        // should flip status back to Working on the next tick via
        // the Spawning|Stuck → Working branch in poll_one step 4.
        let (lifecycle, sessions, agent, base) = setup_stuck("stuck_recovery", Some("1s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s3", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        // Sanity check: session is now parked in Stuck.
        let reloaded = sessions.list().await.unwrap();
        assert_eq!(reloaded[0].status, SessionStatus::Stuck);

        // Drain events so we can isolate what the recovery tick emits.
        let _ = drain_events(&mut rx).await;

        // Flip activity → Active, tick once.
        agent.set(ActivityState::Active);
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Stuck,
                    to: SessionStatus::Working,
                    ..
                }
            )),
            "expected Stuck → Working recovery; got {events:?}"
        );

        // After recovery the idle_since entry should be gone — Active
        // is not an idle state, so update_idle_since removed it on
        // this tick. The next idle streak will restart the clock.
        let map = lifecycle
            .idle_since
            .lock()
            .expect("idle_since mutex poisoned");
        assert!(
            !map.contains_key(&s.id),
            "idle_since should be cleared after recovery"
        );
        drop(map);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_not_triggered_without_agent_stuck_config() {
        // No `agent-stuck` key in the engine → check_stuck is a no-op
        // even after the session has been Idle for a long time.
        let (lifecycle, sessions, _agent, base) = setup_stuck_no_config("stuck_no_config").await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s4", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "no agent-stuck config means no stuck transition; got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_not_triggered_before_threshold_elapses() {
        // Threshold is deliberately long — repeated ticks within the
        // window should never flip to Stuck because idle_since has
        // only aged by microseconds between ticks.
        let (lifecycle, sessions, _agent, base) =
            setup_stuck("stuck_before_threshold", Some("10s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s5", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "stuck transition must not fire before 10s threshold; got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_failed_does_not_become_stuck() {
        // Phase G parking state is NOT stuck-eligible: a session
        // parked in MergeFailed is waiting for the retry loop, not
        // stuck on the agent. Even with idle activity and an
        // elapsed threshold, check_stuck must stay a no-op.
        let (lifecycle, sessions, _agent, base) =
            setup_stuck("merge_failed_no_stuck", Some("1s")).await;
        let mut rx = lifecycle.subscribe();

        let mut s = fake_session("s6", "demo");
        s.status = SessionStatus::MergeFailed;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));
        lifecycle.tick(&mut seen).await.unwrap();

        let events = drain_events(&mut rx).await;
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "MergeFailed must not be stuck-eligible; got {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- SCM polling integration (Phase F) ---------- //

    /// Helper: build a lifecycle manager with both runtime/agent mocks AND
    /// a MockScm plugin attached via `with_scm`. Returns everything so
    /// tests can script state on each plugin side.
    async fn setup_with_scm(
        label: &str,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockScm>,
        PathBuf,
    ) {
        let base = unique_temp_dir(label);
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());
        let lifecycle = Arc::new(
            LifecycleManager::new(sessions.clone(), runtime, agent)
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );
        (lifecycle, sessions, scm, base)
    }

    #[tokio::test]
    async fn scm_poll_with_no_pr_leaves_working_untouched() {
        // Baseline: when the SCM plugin says "no PR", a `Working` session
        // must stay `Working`. The loop still *calls* detect_pr (proving
        // the wiring runs) but produces no status transition.
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-no-pr").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        assert_eq!(
            scm.detect_calls.load(Ordering::SeqCst),
            1,
            "detect_pr should be called exactly once"
        );
        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_opens_pr_transitions_working_to_pr_open() {
        // Working session + first-time PR detection → PrOpen transition.
        // This is the single most important PR-driven transition: the
        // moment the agent opens a PR, the lifecycle loop sees it and
        // moves the session onto the PR track.
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-open").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::None);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::PrOpen,
                    ..
                }
            )),
            "expected Working → PrOpen, got {events:?}"
        );

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::PrOpen);

        // Guard against a regression where the loop polls zero or two
        // times per tick (e.g. a stray extra `poll_scm` call in
        // `poll_one`, or a guard that short-circuits before step 5).
        assert_eq!(
            scm.detect_calls.load(Ordering::SeqCst),
            1,
            "expected exactly one detect_pr call per tick"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_ci_failing_transitions_pr_open_to_ci_failed() {
        // From PrOpen, a red CI check must flip us to CiFailed so the
        // ci-failed reaction (if configured) can fire.
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-ci-fail").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::Pending);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::CiFailed);
        assert_eq!(
            scm.detect_calls.load(Ordering::SeqCst),
            1,
            "expected exactly one detect_pr call per tick"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_full_green_transitions_through_to_mergeable() {
        // A brand-new Working session sees a PR that's already fully
        // approved + green. One tick should take it straight to
        // `Mergeable` — proving the priority ladder applies inside the
        // lifecycle, not just in the pure function's unit tests.
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-all-green").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Passing);
        scm.set_review(ReviewDecision::Approved);
        scm.set_readiness(MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        });

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Mergeable);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_pr_disappears_drops_pr_track_back_to_working() {
        // Session on `PrOpen`, then the plugin starts returning
        // `detect_pr == None` (e.g. agent force-pushed, closing the PR).
        // Lifecycle must fall back to Working so the next push can
        // re-open the PR track.
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-pr-gone").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(None); // no PR

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_detect_pr_error_emits_tick_error_and_skips() {
        // A failing `detect_pr` must not bring down the tick or transition
        // the session. It must emit TickError instead.
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-detect-err").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.detect_pr_errors.store(true, Ordering::SeqCst);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        // Must see a TickError carrying the detect_pr message.
        let mut saw_tick_error = false;
        while let Some(e) = recv_timeout(&mut rx).await {
            if let OrchestratorEvent::TickError { message, .. } = e {
                if message.contains("detect_pr") {
                    saw_tick_error = true;
                }
            }
        }
        assert!(saw_tick_error, "expected TickError from scm.detect_pr");

        // And the session is untouched.
        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_field_probe_error_emits_tick_error_and_skips_transition() {
        // detect_pr succeeds but one of the fan-out probes fails — we
        // refuse to transition on a partial observation. This locks in
        // the "next tick re-probes" contract documented in `poll_scm`.
        //
        // Parameterized over every probe slot. Each iteration spins up
        // a fresh fixture, flips exactly one error toggle, and asserts
        // that the TickError message names that probe by its slot
        // identifier (`pr_state` / `ci_status` / `review_decision` /
        // `mergeability`). If `assemble_observation` ever stops
        // reporting a slot, the matching iteration fails loudly.
        struct Case {
            label: &'static str,
            toggle: fn(&MockScm),
            expected_slot: &'static str,
        }
        let cases = [
            Case {
                label: "pr_state",
                toggle: |s| s.pr_state_errors.store(true, Ordering::SeqCst),
                expected_slot: "pr_state",
            },
            Case {
                label: "ci_status",
                toggle: |s| s.ci_status_errors.store(true, Ordering::SeqCst),
                expected_slot: "ci_status",
            },
            Case {
                label: "review_decision",
                toggle: |s| s.review_decision_errors.store(true, Ordering::SeqCst),
                expected_slot: "review_decision",
            },
            Case {
                label: "mergeability",
                toggle: |s| s.mergeability_errors.store(true, Ordering::SeqCst),
                expected_slot: "mergeability",
            },
        ];

        for case in cases {
            let (lifecycle, sessions, scm, base) =
                setup_with_scm(&format!("scm-field-err-{}", case.label)).await;
            let mut s = fake_session("s1", "demo");
            s.status = SessionStatus::Working;
            sessions.save(&s).await.unwrap();

            scm.set_pr(Some(fake_pr(42, "ao-s1")));
            (case.toggle)(&scm);

            let mut rx = lifecycle.subscribe();
            let mut seen = HashSet::new();
            lifecycle.tick(&mut seen).await.unwrap();

            let mut saw_probe_error = false;
            while let Some(e) = recv_timeout(&mut rx).await {
                if let OrchestratorEvent::TickError { message, .. } = e {
                    if message.contains(case.expected_slot) {
                        saw_probe_error = true;
                    }
                }
            }
            assert!(
                saw_probe_error,
                "expected TickError mentioning {} for case {}",
                case.expected_slot, case.label
            );

            // Session stays Working — no partial-observation transition.
            let persisted = sessions.list().await.unwrap();
            assert_eq!(persisted[0].status, SessionStatus::Working);

            let _ = std::fs::remove_dir_all(&base);
        }
    }

    #[tokio::test]
    async fn scm_poll_is_off_when_scm_is_not_configured() {
        // Proof that Phase C/D behaviour is preserved when no Scm plugin
        // is attached: a session that would be PR-track in Phase F stays
        // on its existing status forever.
        let base = unique_temp_dir("scm-absent");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let lifecycle = Arc::new(LifecycleManager::new(sessions.clone(), runtime, agent));

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        // No SCM plugin → no transition from Working.
        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_fires_reaction_when_transitioning_into_ci_failed() {
        // End-to-end check: with a reaction engine AND an scm plugin both
        // attached, a PR-driven `Working → CiFailed` transition triggers
        // the configured `ci-failed` reaction. This is the flow that
        // Phase D couldn't exercise because the transition source didn't
        // exist yet; Phase F makes it reachable.
        //
        // The engine gets its own `MockRuntime` (`engine_runtime`) so we
        // can read its `sends()` recorder independently of the lifecycle
        // runtime. Both stay as concrete `Arc<MockRuntime>` at the test
        // boundary — the engine/lifecycle constructors take
        // `Arc<dyn Runtime>`, but `Arc<MockRuntime>` coerces on the fly.
        let base = unique_temp_dir("scm-reaction");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);

        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("CI broke, please fix".into());
        let mut map = std::collections::HashMap::new();
        map.insert("ci-failed".into(), cfg);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));

        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        // Script the plugin: PR exists, CI is red, reviewer pending.
        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::Pending);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        // Three proofs:
        // 1. StatusChanged(Working → CiFailed) from the lifecycle loop.
        // 2. ReactionTriggered(SendToAgent) from the engine.
        // 3. The engine's runtime actually received the message — the
        //    event flag alone could mask a regression where the engine
        //    emits the event on a failing send.
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::CiFailed,
                    ..
                }
            )),
            "expected StatusChanged to CiFailed, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ReactionTriggered {
                    action: ReactionAction::SendToAgent,
                    ..
                }
            )),
            "expected ReactionTriggered(SendToAgent), got {events:?}"
        );
        let sends = engine_runtime.sends();
        assert_eq!(sends.len(), 1, "expected exactly one send, got {sends:?}");
        assert_eq!(sends[0].1, "CI broke, please fix");

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- MergeFailed parking loop (Phase G) ---------- //

    /// Build a lifecycle with both a reaction engine AND an SCM plugin,
    /// wired so that the engine shares the same SCM instance the
    /// lifecycle loop polls and uses the lifecycle's broadcast channel
    /// for reaction events. Mirrors `ao-cli::watch`'s production
    /// wiring: one `Arc<MockScm>` services two engines.
    ///
    /// Returns the fully-wired `Arc<LifecycleManager>`, the shared SCM
    /// so tests can script per-tick responses, the engine so tests
    /// can assert tracker attempts, and the base dir so the test can
    /// clean up.
    async fn setup_with_scm_and_auto_merge_engine(
        label: &str,
        retries: Option<u32>,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockScm>,
        Arc<ReactionEngine>,
        PathBuf,
    ) {
        let base = unique_temp_dir(label);
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);

        let mut cfg = ReactionConfig::new(ReactionAction::AutoMerge);
        cfg.retries = retries;
        let mut map = std::collections::HashMap::new();
        map.insert("approved-and-green".into(), cfg);

        // Engine gets its own MockRuntime (unused by auto-merge, but
        // `ReactionEngine::new` requires one) and the SAME Arc<MockScm>
        // the lifecycle polls. `.with_scm(...)` makes
        // `dispatch_auto_merge` call `Scm::merge` for real.
        let engine_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(
            ReactionEngine::new(map, engine_runtime, lifecycle.events_sender())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );
        (lifecycle, sessions, scm, engine, base)
    }

    /// Helper to script a "fully ready to merge" observation on the
    /// mock. Pair with the setup helper above.
    fn script_ready_pr(scm: &MockScm, pr_number: u32) {
        scm.set_pr(Some(fake_pr(pr_number, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Passing);
        scm.set_review(ReviewDecision::Approved);
        scm.set_readiness(MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        });
    }

    #[tokio::test]
    async fn auto_merge_failure_parks_in_merge_failed_then_retries_next_tick() {
        // The core Phase G M1 fix: a merge that fails on tick 1 must
        // land the session in `MergeFailed`, and a still-ready
        // observation on tick 2 must re-fire `approved-and-green`
        // (bumping the tracker to 2) and actually call `Scm::merge`
        // again. The pre-Phase-G behaviour was "stuck silently in
        // Mergeable forever" — this test would have hung there.
        let (lifecycle, sessions, scm, engine, base) =
            setup_with_scm_and_auto_merge_engine("park-retry", Some(5)).await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        script_ready_pr(&scm, 42);
        scm.merge_errors.store(true, Ordering::SeqCst);

        let mut rx = lifecycle.subscribe();

        // Tick 1: dispatch auto-merge and persist directly as MergeFailed.
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        // Must persist as MergeFailed, NOT Mergeable — pre-Phase-G
        // this would have been stuck at Mergeable.
        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].status,
            SessionStatus::MergeFailed,
            "tick 1 must park in MergeFailed after merge failure"
        );
        assert_eq!(
            engine.attempts(&s.id, "approved-and-green"),
            1,
            "tracker must increment on the failed merge"
        );
        assert_eq!(scm.merges().len(), 0, "failed merge does not record");

        // Flip the plugin: merge will succeed on retry.
        scm.merge_errors.store(false, Ordering::SeqCst);

        // Tick 2: MergeFailed → Mergeable (re-promotion), dispatch
        // runs again, merge succeeds, session stays in Mergeable. The
        // next SCM observation would flip PrState::Merged and then
        // transition to Merged — that's covered by Phase F tests, so
        // we stop at the successful merge call.
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].status,
            SessionStatus::Mergeable,
            "tick 2 must re-promote and stay in Mergeable after successful merge"
        );
        assert_eq!(
            engine.attempts(&s.id, "approved-and-green"),
            2,
            "tracker must accumulate across the parking loop"
        );
        assert_eq!(scm.merges().len(), 1, "second attempt must actually merge");
        assert_eq!(scm.merges()[0], (42, None));

        // Event stream proofs: exactly one status transition per tick:
        // tick 1 is `Working → MergeFailed`, tick 2 is
        // `MergeFailed → Mergeable`, plus two `ReactionTriggered(AutoMerge)`
        // events (one per tick).
        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }
        let park_seen = events.iter().any(|e| {
            matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::MergeFailed,
                    ..
                }
            )
        });
        let repromote_seen = events.iter().any(|e| {
            matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::MergeFailed,
                    to: SessionStatus::Mergeable,
                    ..
                }
            )
        });
        assert!(park_seen, "expected park event, got {events:?}");
        assert!(repromote_seen, "expected re-promote event, got {events:?}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn persistent_merge_failure_escalates_after_retries_exhausted() {
        // retries=2 means attempts 1 and 2 are real merge calls, and
        // attempt 3 escalates to Notify WITHOUT re-calling the SCM.
        // The critical assertion is that after escalation the session
        // is left in `Mergeable` (not parked), so a subsequent tick
        // with an unchanged observation is a no-op — no infinite
        // escalate-and-reparke spiral.
        let (lifecycle, sessions, scm, engine, base) =
            setup_with_scm_and_auto_merge_engine("park-escalate", Some(2)).await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        script_ready_pr(&scm, 42);
        scm.merge_errors.store(true, Ordering::SeqCst);

        let mut rx = lifecycle.subscribe();

        // Tick 1: first attempt.
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine.attempts(&s.id, "approved-and-green"), 1);
        assert_eq!(
            sessions.list().await.unwrap()[0].status,
            SessionStatus::MergeFailed
        );

        // Tick 2: attempts=2, still within budget (`attempts > retries`
        // is `2 > 2 = false`), so the engine dispatches again and parks
        // again on the second failure. Escalation only fires on tick 3.
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine.attempts(&s.id, "approved-and-green"), 2);
        assert_eq!(
            sessions.list().await.unwrap()[0].status,
            SessionStatus::MergeFailed
        );

        // Tick 3: attempts=3 > retries=2 → escalate. The engine's
        // escalation path short-circuits BEFORE `dispatch_auto_merge`
        // runs, so `Scm::merge` is not called on this tick and
        // `merges().len()` stays at 0 (the first two were rejected by
        // the error toggle, not recorded).
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine.attempts(&s.id, "approved-and-green"), 3);

        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].status,
            SessionStatus::Mergeable,
            "after escalation, session must stay in Mergeable (not parked) \
             so we don't re-dispatch on the next tick"
        );
        assert_eq!(
            scm.merges().len(),
            0,
            "both failed merges are rejected by the mock; no successful \
             records"
        );

        // Must have seen exactly one ReactionEscalated event.
        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }
        let escalated_count = events
            .iter()
            .filter(|e| matches!(e, OrchestratorEvent::ReactionEscalated { .. }))
            .count();
        assert_eq!(
            escalated_count, 1,
            "expected exactly one ReactionEscalated event, got {events:?}"
        );

        // Tick 4: unchanged observation → derive_scm_status returns
        // None → no transition → no dispatch → no double-escalation.
        // This is the key guard: escalated sessions must NOT bounce
        // through the parking loop on every subsequent tick.
        let attempts_before_tick4 = engine.attempts(&s.id, "approved-and-green");
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            engine.attempts(&s.id, "approved-and-green"),
            attempts_before_tick4,
            "tick 4 must not increment attempts — session is frozen"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_failed_exit_to_ci_failed_clears_approved_and_green_tracker() {
        // A parked session sees CI flip red on the next tick and
        // drops off the ready path to `CiFailed`. The approved-and-green
        // tracker must be cleared on that exit so a later re-entry to
        // Mergeable (after CI recovers) starts with a fresh retry
        // budget — otherwise the next merge attempt would inherit the
        // stale count and escalate prematurely.
        let (lifecycle, sessions, scm, engine, base) =
            setup_with_scm_and_auto_merge_engine("park-exit-clears", Some(5)).await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        script_ready_pr(&scm, 42);
        scm.merge_errors.store(true, Ordering::SeqCst);

        // Tick 1: Working → Mergeable → park. attempts=1.
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            sessions.list().await.unwrap()[0].status,
            SessionStatus::MergeFailed
        );
        assert_eq!(engine.attempts(&s.id, "approved-and-green"), 1);

        // Flip: CI just went red. Observation now says `CiFailed` is
        // the right status for this PR.
        scm.set_ci(CiStatus::Failing);
        scm.set_readiness(MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: true,
            no_conflicts: true,
            blockers: vec!["CI is failing".into()],
        });

        // Tick 2: MergeFailed → CiFailed. Exit-clear fires with the
        // hardcoded `"approved-and-green"` key because
        // status_to_reaction_key(MergeFailed) returns None.
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            sessions.list().await.unwrap()[0].status,
            SessionStatus::CiFailed
        );
        assert_eq!(
            engine.attempts(&s.id, "approved-and-green"),
            0,
            "approved-and-green tracker must be cleared on exit from MergeFailed"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_failed_drops_back_to_working_when_pr_disappears() {
        // End-to-end proof that `is_pr_track(MergeFailed) == true`:
        // the parked session force-pushes (simulating the agent) and
        // the lifecycle must drop back to Working, matching the
        // behaviour for every other PR-track status.
        let (lifecycle, sessions, scm, _engine, base) =
            setup_with_scm_and_auto_merge_engine("park-pr-gone", Some(5)).await;

        // Seed the session directly in MergeFailed so we don't have
        // to walk the full Working → Mergeable → fail → park path
        // just to reach the starting state.
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::MergeFailed;
        sessions.save(&s).await.unwrap();

        // No PR on the plugin side.
        scm.set_pr(None);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].status,
            SessionStatus::Working,
            "MergeFailed must be on the PR track so detect_pr(None) drops to Working"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn background_loop_starts_and_stops_cleanly() {
        // Manually assemble the manager with a tight poll interval so the
        // test runs in milliseconds, not the default 5 s.
        let base = unique_temp_dir("loop");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        sessions.save(&fake_session("s1", "demo")).await.unwrap();

        let lifecycle = Arc::new(
            LifecycleManager::new(
                sessions.clone(),
                Arc::new(MockRuntime::new(true)) as Arc<dyn Runtime>,
                Arc::new(MockAgent::new(ActivityState::Ready)) as Arc<dyn Agent>,
            )
            .with_poll_interval(Duration::from_millis(20)),
        );

        let mut rx = lifecycle.subscribe();
        let handle = lifecycle.spawn();

        // Wait for at least one StatusChanged to prove the loop ran.
        let mut saw_status_change = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                Ok(Ok(OrchestratorEvent::StatusChanged { .. })) => {
                    saw_status_change = true;
                    break;
                }
                Ok(Ok(_)) => {}
                _ => {}
            }
        }

        handle.stop().await;
        assert!(
            saw_status_change,
            "background loop never emitted StatusChanged"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Orchestrator notification (issue #169) ---------- //

    #[tokio::test]
    async fn transition_notifies_parent_orchestrator_via_runtime() {
        // Worker points at a parent session via `spawned_by`. Transitioning
        // the worker into `PrOpen` should deliver one runtime message to
        // the parent's handle — the mechanism the orchestrator relies on
        // to learn its worker changed state.
        let base = unique_temp_dir("orchestrator-notify");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let lifecycle = Arc::new(LifecycleManager::new(
            sessions.clone(),
            runtime.clone() as Arc<dyn Runtime>,
            agent,
        ));

        let mut parent = fake_session("orch1", "demo");
        parent.runtime_handle = Some("orch-handle".into());
        sessions.save(&parent).await.unwrap();

        let mut worker = fake_session("work1", "demo");
        worker.status = SessionStatus::Working;
        worker.spawned_by = Some(parent.id.clone());
        sessions.save(&worker).await.unwrap();

        lifecycle
            .transition(&mut worker, SessionStatus::PrOpen)
            .await
            .unwrap();

        let sends = runtime.sends();
        assert_eq!(
            sends.len(),
            1,
            "expected one notification to parent, got {sends:?}"
        );
        assert_eq!(sends[0].0, "orch-handle");
        assert!(
            sends[0].1.contains("pr_open"),
            "message should mention new status, got {:?}",
            sends[0].1
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn transition_without_spawned_by_sends_no_message() {
        let base = unique_temp_dir("orchestrator-notify-none");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let lifecycle = Arc::new(LifecycleManager::new(
            sessions.clone(),
            runtime.clone() as Arc<dyn Runtime>,
            agent,
        ));

        let mut worker = fake_session("lone1", "demo");
        worker.status = SessionStatus::Working;
        assert!(worker.spawned_by.is_none());
        sessions.save(&worker).await.unwrap();

        lifecycle
            .transition(&mut worker, SessionStatus::PrOpen)
            .await
            .unwrap();

        assert!(
            runtime.sends().is_empty(),
            "workers without spawned_by must not trigger a send"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn transition_into_non_notifiable_status_sends_no_message() {
        // Working is a non-notifiable status (noise-reduction policy);
        // even with a parent, no message should go out on Spawning→Working.
        let base = unique_temp_dir("orchestrator-notify-filter");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let lifecycle = Arc::new(LifecycleManager::new(
            sessions.clone(),
            runtime.clone() as Arc<dyn Runtime>,
            agent,
        ));

        let mut parent = fake_session("orch2", "demo");
        parent.runtime_handle = Some("orch-handle".into());
        sessions.save(&parent).await.unwrap();

        let mut worker = fake_session("work2", "demo");
        worker.status = SessionStatus::Spawning;
        worker.spawned_by = Some(parent.id.clone());
        sessions.save(&worker).await.unwrap();

        lifecycle
            .transition(&mut worker, SessionStatus::Working)
            .await
            .unwrap();

        assert!(
            runtime.sends().is_empty(),
            "transition to Working should not notify orchestrator"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
