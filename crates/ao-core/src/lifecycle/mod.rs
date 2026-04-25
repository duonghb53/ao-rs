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
    dashboard_payload::{attention_level, BatchedPrEnrichment, DashboardPr},
    error::Result,
    events::{OrchestratorEvent, TerminationReason},
    reaction_engine::{parse_duration, status_to_reaction_key, ReactionEngine},
    reactions::{ReactionAction, ReactionOutcome},
    scm::{CheckStatus, CiStatus, MergeReadiness, PrState, PullRequest, ReviewDecision},
    scm_transitions::{derive_scm_status, ScmObservation},
    session_manager::SessionManager,
    traits::{Agent, Runtime, Scm, Workspace},
    types::{ActivityState, Session, SessionId, SessionStatus},
};
use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

mod scm_poll;
mod stuck;
mod tick;
mod transition;

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
    pub(super) sessions: Arc<SessionManager>,
    pub(super) runtime: Arc<dyn Runtime>,
    pub(super) agent: Arc<dyn Agent>,
    pub(super) events_tx: broadcast::Sender<OrchestratorEvent>,
    poll_interval: Duration,
    /// Optional Slice 2 Phase D reaction engine. When set, every status
    /// transition into a reaction-triggering state (see
    /// `status_to_reaction_key`) calls `engine.dispatch(...)`. When unset,
    /// the lifecycle loop behaves exactly as it did in Phase C.
    pub(super) reaction_engine: Option<Arc<ReactionEngine>>,
    /// Optional Slice 2 Phase F SCM plugin. When set, every tick calls
    /// `detect_pr` on each non-terminal session; a fresh PR observation
    /// is folded through `derive_scm_status` to produce PR-driven status
    /// transitions (`Working → PrOpen/CiFailed/ChangesRequested/…`).
    /// When unset, the lifecycle loop is exactly the Phase C/D behaviour
    /// — SCM polling is off. This matches how tests and `ao-rs watch`
    /// without a configured plugin should behave.
    pub(super) scm: Option<Arc<dyn Scm>>,
    /// Optional workspace plugin. When set, sessions that transition to
    /// `Merged` automatically have their worktree destroyed so disk space
    /// is reclaimed without a manual `ao-rs cleanup`.
    pub(super) workspace: Option<Arc<dyn Workspace>>,
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
    pub(super) idle_since: Mutex<HashMap<SessionId, Instant>>,
    /// Per-tick cache of batch-enriched PR observations.
    ///
    /// Populated once at the start of each `tick()` call via
    /// `Scm::enrich_prs_full()`. Individual `poll_scm` calls check this
    /// cache first and skip the 4× REST fan-out when they find a hit.
    /// Cleared at the start of the next tick.
    ///
    /// Key format: `"{owner}/{repo}#{number}"`.
    pub(super) pr_enrichment_cache: Mutex<HashMap<String, BatchedPrEnrichment>>,
    /// Per-session previous-tick PR enrichment, keyed by session id.
    /// Used to diff against the current tick's enrichment so the lifecycle
    /// loop only emits `PrEnrichmentChanged` when something actually
    /// changed. Persists across ticks (not cleared like
    /// `pr_enrichment_cache`).
    pub(super) pr_enrichment_prev: Mutex<HashMap<SessionId, BatchedPrEnrichment>>,
    /// Latest `DashboardPr` payload per session, suitable for the SSE
    /// snapshot frame. Updated alongside `pr_enrichment_prev` whenever the
    /// lifecycle loop emits a `PrEnrichmentChanged`. Shared with the
    /// dashboard via `Arc` so the SSE handler can serialize the current
    /// state to a freshly connected client without an extra round-trip.
    pub(super) pr_enrichment_payload: Arc<Mutex<HashMap<SessionId, DashboardPr>>>,
    /// Per-session timestamp of the last review backlog API check.
    /// Throttles `pending_comments` calls to at most once per 2 minutes.
    pub(super) last_review_backlog_check: Mutex<HashMap<SessionId, Instant>>,
    /// Per-tick cache of detected PRs from `detect_pr`. Populated in
    /// `tick()` Pass 1 so `poll_scm` reuses the result instead of
    /// calling `detect_pr` a second time.
    pub(super) detected_prs_cache: Mutex<HashMap<SessionId, Option<PullRequest>>>,
    /// Unix-ms when `run_loop` started. `0` means "not yet started"
    /// (e.g. tests driving `tick` directly). Used by `tick` to
    /// classify first-seen sessions as `SessionRestored` (created
    /// before startup) vs. `Spawned` (created after).
    pub(super) startup_ms: AtomicU64,
    /// Set to `true` once `all-complete` has been dispatched for the
    /// current drain cycle (all active sessions reached terminal state).
    /// Reset to `false` on the first tick that observes a new non-terminal
    /// session, so a fresh batch of sessions gets its own `all-complete`.
    pub(super) all_complete_fired: AtomicBool,
}

impl LifecycleManager {
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
            pr_enrichment_prev: Mutex::new(HashMap::new()),
            pr_enrichment_payload: Arc::new(Mutex::new(HashMap::new())),
            last_review_backlog_check: Mutex::new(HashMap::new()),
            detected_prs_cache: Mutex::new(HashMap::new()),
            startup_ms: AtomicU64::new(0),
            all_complete_fired: AtomicBool::new(false),
        }
    }

    /// Borrow the SSE-snapshot PR enrichment cache so the dashboard SSE
    /// handler can include the current `DashboardPr` for every active
    /// session in the initial `snapshot` frame. Cheap clone — `Arc`.
    pub fn pr_enrichment_payload(&self) -> Arc<Mutex<HashMap<SessionId, DashboardPr>>> {
        self.pr_enrichment_payload.clone()
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

        // Record startup time so `tick` can distinguish sessions that
        // predate this loop (emitted as `SessionRestored`) from sessions
        // created after startup (emitted as `Spawned`). Set *before* the
        // sweep so any session whose `created_at` equals or exceeds this
        // moment is classified as new.
        let startup_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // `0` is the "not started" sentinel — guard against clock skew
        // that would stamp it as such.
        self.startup_ms.store(startup_ms.max(1), Ordering::Relaxed);

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
            let mut cache = self.pr_enrichment_cache.lock().unwrap_or_else(|e| {
                tracing::error!("pr_enrichment_cache mutex poisoned; recovering inner state: {e}");
                e.into_inner()
            });
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
                let id = session.id.clone();
                match scm.detect_pr(session).await {
                    Ok(pr) => {
                        if let Some(ref p) = pr {
                            prs_for_batch.push(p.clone());
                        }
                        detected_prs.insert(id, pr);
                    }
                    Err(e) => {
                        self.emit(OrchestratorEvent::TickError {
                            id: id.clone(),
                            message: format!("scm.detect_pr: {e}"),
                        });
                        detected_prs.insert(id, None);
                    }
                }
            }

            // Batch enrichment
            if !prs_for_batch.is_empty() {
                match scm.enrich_prs_full(&prs_for_batch).await {
                    Ok(enrichment) => {
                        if !enrichment.is_empty() {
                            tracing::debug!(
                                "[batch enrichment] cached {} PR observations",
                                enrichment.len()
                            );
                            // Diff per session and emit `PrEnrichmentChanged`
                            // for every session whose PR enrichment shifted
                            // since the previous tick (or first observation).
                            // Diff before writing the cache so we can pass the
                            // fresh map by reference and avoid cloning the
                            // whole `HashMap<_, BatchedPrEnrichment>` per tick.
                            self.diff_and_emit_pr_enrichment(&sessions, &detected_prs, &enrichment);
                            let mut cache =
                                self.pr_enrichment_cache.lock().unwrap_or_else(|e| {
                                    tracing::error!(
                                        "pr_enrichment_cache mutex poisoned; recovering inner state: {e}"
                                    );
                                    e.into_inner()
                                });
                            *cache = enrichment;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[batch enrichment] failed: {e}");
                    }
                }
            }
            // Sessions whose PR vanished since last tick: emit a clearing
            // `PrEnrichmentChanged { pr: None }` so the UI drops stale
            // enrichment without waiting for a refresh.
            self.clear_lost_pr_enrichment(&detected_prs);
        }

        // Store detected PRs so poll_scm can consume them.
        {
            let mut cache = self.detected_prs_cache.lock().unwrap_or_else(|e| {
                tracing::error!("detected_prs_cache mutex poisoned; recovering inner state: {e}");
                e.into_inner()
            });
            *cache = detected_prs;
        }

        // Pass 2: poll each session.
        let startup_ms = self.startup_ms.load(Ordering::Relaxed);
        let mut any_active = false;
        for session in sessions {
            let id = session.id.clone();
            if seen.insert(id.clone()) {
                // Sessions that predate loop startup are restored from disk,
                // not newly spawned. When `startup_ms == 0` (tests driving
                // `tick` directly, no `run_loop`), preserve the original
                // behaviour and emit `Spawned` for everything.
                if startup_ms != 0 && session.created_at < startup_ms {
                    self.emit(OrchestratorEvent::SessionRestored {
                        id: id.clone(),
                        project_id: session.project_id.clone(),
                        status: session.status,
                    });
                } else {
                    self.emit(OrchestratorEvent::Spawned {
                        id,
                        project_id: session.project_id.clone(),
                    });
                }
            }

            if session.is_terminal() {
                continue;
            }

            any_active = true;
            // A fresh non-terminal session re-arms the all-complete gate so
            // a subsequent drain fires a new `all-complete`.
            self.all_complete_fired.store(false, Ordering::Relaxed);

            if let Err(e) = self.poll_one(session).await {
                tracing::warn!("poll_one failed: {e}");
            }
        }

        // ---- all-complete (issue #195 H3) ----
        // When all seen sessions are terminal and we have seen at least one,
        // dispatch `all-complete` exactly once per drain cycle.
        if !any_active && !seen.is_empty() && !self.all_complete_fired.load(Ordering::Relaxed) {
            if let Some(engine) = self.reaction_engine.as_ref() {
                // `all-complete` has no session context — we use a synthetic
                // sentinel session so the engine can look up the reaction
                // config. This mirrors how TS fires a summary-level event.
                let sentinel = all_complete_sentinel();
                match engine.dispatch(&sentinel, "all-complete").await {
                    Ok(_) => {
                        self.all_complete_fired.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "all-complete dispatch failed");
                    }
                }
            }
        }

        Ok(())
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
    pub(super) fn update_idle_since(&self, session_id: &SessionId, activity: ActivityState) {
        let mut map = self.idle_since.lock().unwrap_or_else(|e| {
            tracing::error!("lifecycle idle_since mutex poisoned; recovering inner state: {e}");
            e.into_inner()
        });
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
    pub(super) fn emit(&self, event: OrchestratorEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Diff the latest batch enrichment against the previous tick's snapshot
    /// and emit `PrEnrichmentChanged` for every session whose enrichment
    /// shifted (or first observation). Updates `pr_enrichment_prev` and
    /// `pr_enrichment_payload` so the SSE snapshot stays in sync.
    fn diff_and_emit_pr_enrichment(
        &self,
        sessions: &[Session],
        detected_prs: &HashMap<SessionId, Option<PullRequest>>,
        enrichment_map: &HashMap<String, BatchedPrEnrichment>,
    ) {
        // Hold both `prev` and `payload` locks across the diff loop to avoid
        // double-acquisition per session and to close the TOCTOU window where a
        // future concurrent caller could read a stale `prev` and emit a
        // duplicate event. Drop both before broadcasting so emit() never runs
        // under a lock.
        let mut to_emit: Vec<OrchestratorEvent> = Vec::new();
        {
            let mut prev = self.pr_enrichment_prev.lock().unwrap_or_else(|e| {
                tracing::error!("pr_enrichment_prev mutex poisoned; recovering inner state: {e}");
                e.into_inner()
            });
            let mut payload = self.pr_enrichment_payload.lock().unwrap_or_else(|e| {
                tracing::error!(
                    "pr_enrichment_payload mutex poisoned; recovering inner state: {e}"
                );
                e.into_inner()
            });
            for session in sessions {
                let Some(Some(pr)) = detected_prs.get(&session.id) else {
                    continue;
                };
                let key = format!("{}/{}#{}", pr.owner, pr.repo, pr.number);
                let Some(enrichment) = enrichment_map.get(&key) else {
                    continue;
                };
                if prev.get(&session.id) == Some(enrichment) {
                    continue;
                }
                let dash_pr = DashboardPr::from_enrichment(pr, enrichment);
                let level = attention_level(session, Some(&dash_pr));
                prev.insert(session.id.clone(), enrichment.clone());
                payload.insert(session.id.clone(), dash_pr.clone());
                to_emit.push(OrchestratorEvent::PrEnrichmentChanged {
                    id: session.id.clone(),
                    pr: Some(dash_pr),
                    attention_level: level,
                });
            }
        }
        for event in to_emit {
            self.emit(event);
        }
    }

    /// Drop cached enrichment for sessions whose PR no longer exists, and
    /// emit `PrEnrichmentChanged { pr: None }` for each so SSE clients
    /// clear their stale state.
    fn clear_lost_pr_enrichment(&self, detected_prs: &HashMap<SessionId, Option<PullRequest>>) {
        let lost: Vec<SessionId> = {
            let mut prev = self.pr_enrichment_prev.lock().unwrap_or_else(|e| {
                tracing::error!("pr_enrichment_prev mutex poisoned; recovering inner state: {e}");
                e.into_inner()
            });
            let mut payload = self.pr_enrichment_payload.lock().unwrap_or_else(|e| {
                tracing::error!(
                    "pr_enrichment_payload mutex poisoned; recovering inner state: {e}"
                );
                e.into_inner()
            });
            let lost: Vec<SessionId> = prev
                .keys()
                .filter(|sid| matches!(detected_prs.get(*sid), Some(None) | None))
                .cloned()
                .collect();
            for sid in &lost {
                prev.remove(sid);
                payload.remove(sid);
            }
            lost
        };
        for sid in lost {
            // Without a PR the attention level falls back to lifecycle status,
            // but we don't carry the Session here — clients can recompute from
            // their snapshot. Pass an empty string so the wire form stays
            // present (the client treats empty as "no override").
            self.emit(OrchestratorEvent::PrEnrichmentChanged {
                id: sid,
                pr: None,
                attention_level: String::new(),
            });
        }
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
pub(super) fn assemble_observation(
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
pub(super) fn should_park_in_merge_failed(
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
pub(super) fn clear_tracker_on_transition(
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

    // `CiFailed` is excluded from `status_to_reaction_key` (issue #195 H3)
    // because its dispatch is handled by `check_ci_failed`, not `transition`.
    // Explicitly clear the tracker here so a second CI failure episode
    // after a fix starts with a fresh retry budget.
    if from == SessionStatus::CiFailed {
        engine.clear_tracker(session_id, "ci-failed");
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
pub(super) const fn is_review_stable(status: SessionStatus) -> bool {
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
pub(super) const fn is_orchestrator_notifiable(status: SessionStatus) -> bool {
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
pub(super) fn format_orchestrator_notification(worker: &Session, to: SessionStatus) -> String {
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

pub(super) const fn is_stuck_eligible(status: SessionStatus) -> bool {
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

/// Stable hash fingerprint of a `ReviewComment` slice.
///
/// Sorts by `(author, body, url)` for determinism (API order can vary),
/// then folds through `DefaultHasher`. Used by `check_review_backlog` to
/// detect when the pending-comments set has changed between ticks.
pub(super) fn fingerprint_comments(comments: &[crate::scm::ReviewComment]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut keys: Vec<(&str, &str, &str)> = comments
        .iter()
        .map(|c| (c.author.as_str(), c.body.as_str(), c.url.as_str()))
        .collect();
    keys.sort_unstable();
    let mut h = DefaultHasher::new();
    keys.hash(&mut h);
    h.finish()
}

/// A minimal dummy `Session` used as the dispatch target for `all-complete`.
///
/// `all-complete` is a drain-level event — it has no per-session context.
/// The engine needs a session to look up the project's reaction config, but
/// since `all-complete` is a global reaction, any project id works here. We
/// use an empty project id so config lookup falls back to the global entry.
pub(super) fn all_complete_sentinel() -> Session {
    use crate::types::{now_ms, SessionId};
    Session {
        id: SessionId("__all_complete__".into()),
        project_id: String::new(),
        status: SessionStatus::Done,
        agent: String::new(),
        agent_config: None,
        branch: String::new(),
        task: String::new(),
        workspace_path: None,
        runtime_handle: None,
        runtime: String::new(),
        activity: None,
        created_at: now_ms(),
        cost: None,
        issue_id: None,
        issue_url: None,
        claimed_pr_number: None,
        claimed_pr_url: None,
        initial_prompt_override: None,
        spawned_by: None,
        last_merge_conflict_dispatched: None,
        last_review_backlog_fingerprint: None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::scm::{
        CheckRun, CiStatus, MergeMethod, MergeReadiness, PrState, PullRequest, Review,
        ReviewComment, ReviewDecision,
    };
    use crate::types::{now_ms, SessionId};
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub(crate) fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ao-rs-lifecycle-{label}-{nanos}-{n}"))
    }

    pub(crate) fn fake_session(id: &str, project: &str) -> Session {
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
            last_merge_conflict_dispatched: None,
            last_review_backlog_fingerprint: None,
        }
    }

    // ---------- Mock plugins ---------- //

    /// Runtime mock with a toggleable `alive` flag and a recorder for
    /// `send_message` calls. Tests that care about delivery can read
    /// the recorder via `MockRuntime::sends()`; others ignore it.
    pub(crate) struct MockRuntime {
        pub(crate) alive: AtomicBool,
        pub(crate) sends: Mutex<Vec<(String, String)>>,
        pub(crate) destroys: Mutex<Vec<String>>,
    }

    impl MockRuntime {
        pub(crate) fn new(alive: bool) -> Self {
            Self {
                alive: AtomicBool::new(alive),
                sends: Mutex::new(Vec::new()),
                destroys: Mutex::new(Vec::new()),
            }
        }

        /// Snapshot of all `(handle, message)` pairs received by
        /// `send_message` in the order they were called.
        pub(crate) fn sends(&self) -> Vec<(String, String)> {
            self.sends.lock().unwrap().clone()
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
    pub(crate) struct MockAgent {
        next: Mutex<ActivityState>,
    }

    impl MockAgent {
        pub(crate) fn new(initial: ActivityState) -> Self {
            Self {
                next: Mutex::new(initial),
            }
        }
        pub(crate) fn set(&self, state: ActivityState) {
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

    /// Scriptable SCM mock. Every method returns a value pre-set by the
    /// test. `detect_pr` returns `None` until `set_pr(Some(_))`. Each field
    /// probe (`pr_state`, `ci_status`, …) can be toggled to emit an error
    /// so tests cover the `TickError` branch of `poll_scm`.
    ///
    /// Kept intentionally minimal — we only test the handful of methods
    /// `poll_scm` calls. `pending_comments`, `reviews`, `ci_checks`, `merge`
    /// return empty/default values (enough to satisfy the trait).
    pub(crate) struct MockScm {
        pub(crate) pr: Mutex<Option<PullRequest>>,
        pub(crate) state: Mutex<PrState>,
        pub(crate) ci: Mutex<CiStatus>,
        pub(crate) review: Mutex<ReviewDecision>,
        pub(crate) readiness: Mutex<MergeReadiness>,
        // Counter: incremented on every detect_pr call, so tests can assert
        // "the loop actually called the plugin".
        pub(crate) detect_calls: AtomicUsize,
        // Error toggles. One per probe that `poll_scm` fans out to, so
        // individual tests can force exactly one slot to fail and
        // verify the error-aggregation path reports that slot by name.
        pub(crate) detect_pr_errors: AtomicBool,
        pub(crate) pr_state_errors: AtomicBool,
        pub(crate) ci_status_errors: AtomicBool,
        pub(crate) review_decision_errors: AtomicBool,
        pub(crate) mergeability_errors: AtomicBool,
        // Phase G: `merge()` error toggle + call recorder so parking-loop
        // tests can script "fail first, succeed later" and assert that
        // the engine actually called the plugin the expected number of
        // times.
        pub(crate) merge_errors: AtomicBool,
        pub(crate) merge_calls: Mutex<Vec<(u32, Option<MergeMethod>)>>,
        // Issue #195: scriptable pending_comments and ci_checks for H2/H3 tests.
        pub(crate) pending_comments_result: Mutex<Vec<ReviewComment>>,
        pub(crate) ci_checks_result: Mutex<Vec<CheckRun>>,
    }

    impl MockScm {
        pub(crate) fn new() -> Self {
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
                pending_comments_result: Mutex::new(vec![]),
                ci_checks_result: Mutex::new(vec![]),
            }
        }
        pub(crate) fn merges(&self) -> Vec<(u32, Option<MergeMethod>)> {
            self.merge_calls.lock().unwrap().clone()
        }
        pub(crate) fn set_pending_comments(&self, comments: Vec<ReviewComment>) {
            *self.pending_comments_result.lock().unwrap() = comments;
        }
        pub(crate) fn set_ci_checks(&self, checks: Vec<CheckRun>) {
            *self.ci_checks_result.lock().unwrap() = checks;
        }
        pub(crate) fn set_pr(&self, pr: Option<PullRequest>) {
            *self.pr.lock().unwrap() = pr;
        }
        pub(crate) fn set_state(&self, s: PrState) {
            *self.state.lock().unwrap() = s;
        }
        pub(crate) fn set_ci(&self, c: CiStatus) {
            *self.ci.lock().unwrap() = c;
        }
        pub(crate) fn set_review(&self, r: ReviewDecision) {
            *self.review.lock().unwrap() = r;
        }
        pub(crate) fn set_readiness(&self, r: MergeReadiness) {
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
            Ok(self.ci_checks_result.lock().unwrap().clone())
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
            Ok(self.pending_comments_result.lock().unwrap().clone())
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

    pub(crate) fn fake_pr(number: u32, branch: &str) -> PullRequest {
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

    pub(crate) async fn setup(
        label: &str,
        initial_activity: ActivityState,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockRuntime>,
        Arc<MockAgent>,
        PathBuf,
    ) {
        use crate::session_manager::SessionManager;
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

    pub(crate) async fn recv_timeout(
        rx: &mut broadcast::Receiver<OrchestratorEvent>,
    ) -> Option<OrchestratorEvent> {
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .ok()
            .and_then(|r| r.ok())
    }

    pub(crate) async fn drain_events(
        rx: &mut broadcast::Receiver<OrchestratorEvent>,
    ) -> Vec<OrchestratorEvent> {
        let mut out = Vec::new();
        while let Some(e) = recv_timeout(rx).await {
            out.push(e);
        }
        out
    }

    pub(crate) fn rewind_idle_since(
        lifecycle: &LifecycleManager,
        session_id: &SessionId,
        by: Duration,
    ) {
        let mut map = lifecycle.idle_since.lock().unwrap_or_else(|e| {
            tracing::error!("idle_since mutex poisoned; recovering inner state: {e}");
            e.into_inner()
        });
        let rewound = Instant::now()
            .checked_sub(by)
            .expect("test clock rewind underflowed Instant");
        map.insert(session_id.clone(), rewound);
    }

    pub(crate) async fn setup_with_scm(
        label: &str,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockScm>,
        PathBuf,
    ) {
        use crate::session_manager::SessionManager;
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

    pub(crate) fn script_ready_pr(scm: &MockScm, pr_number: u32) {
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

    pub(crate) async fn setup_with_scm_and_auto_merge_engine(
        label: &str,
        retries: Option<u32>,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockScm>,
        Arc<ReactionEngine>,
        PathBuf,
    ) {
        use crate::reactions::ReactionConfig;
        use crate::session_manager::SessionManager;
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

    pub(crate) async fn setup_with_merge_conflicts_engine(
        label: &str,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockScm>,
        Arc<MockRuntime>,
        Arc<ReactionEngine>,
        PathBuf,
    ) {
        use crate::reactions::ReactionConfig;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir(label);
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle =
            LifecycleManager::new(sessions.clone(), runtime.clone() as Arc<dyn Runtime>, agent);

        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("please rebase".into());
        let mut map = std::collections::HashMap::new();
        map.insert("merge-conflicts".into(), cfg);

        let engine_runtime: Arc<dyn Runtime> = runtime.clone() as Arc<dyn Runtime>;
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime,
            lifecycle.events_sender(),
        ));

        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );
        (lifecycle, sessions, scm, runtime, engine, base)
    }

    pub(crate) async fn setup_stuck(
        label: &str,
        threshold: Option<&str>,
    ) -> (
        Arc<LifecycleManager>,
        Arc<SessionManager>,
        Arc<MockAgent>,
        PathBuf,
    ) {
        use crate::reactions::ReactionConfig;
        use crate::session_manager::SessionManager;
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

    pub(crate) fn build_engine_with_ci_failed(
        lifecycle: &LifecycleManager,
        message: &str,
    ) -> Arc<ReactionEngine> {
        use crate::reactions::ReactionConfig;
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some(message.into());
        let mut map = std::collections::HashMap::new();
        map.insert("ci-failed".into(), cfg);

        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        Arc::new(ReactionEngine::new(map, runtime, lifecycle.events_sender()))
    }

    // Actual test functions live in the per-submodule test modules:
    // - tick::tests (tick.rs)
    // - scm_poll::tests (scm_poll.rs)
    // - transition::tests (transition.rs)
    // - stuck::tests (stuck.rs)
    //
    // The helpers above (fake_session, setup, recv_timeout, etc.) are
    // shared via `pub(crate)` visibility and imported by those modules.
}
