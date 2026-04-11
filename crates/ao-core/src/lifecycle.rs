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
    session_manager::SessionManager,
    traits::{Agent, Runtime},
    types::{ActivityState, Session, SessionId, SessionStatus},
};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// How many events the broadcast channel buffers before dropping the oldest.
/// 1024 is generous — a healthy loop emits at most ~N events per tick where
/// N is the session count, and slow subscribers will lag at most a handful
/// of ticks before catching up.
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Default poll interval — matches the TS reference's 5 s.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

pub struct LifecycleManager {
    sessions: Arc<SessionManager>,
    runtime: Arc<dyn Runtime>,
    agent: Arc<dyn Agent>,
    events_tx: broadcast::Sender<OrchestratorEvent>,
    poll_interval: Duration,
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
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
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

        for session in sessions {
            // Newly observed? Announce it.
            if seen.insert(session.id.clone()) {
                self.emit(OrchestratorEvent::Spawned {
                    id: session.id.clone(),
                    project_id: session.project_id.clone(),
                });
            }

            if session.is_terminal() {
                // Already in a terminal state; nothing to poll.
                continue;
            }

            if let Err(e) = self.poll_one(session).await {
                // Per-session failure: surface via TickError and keep going.
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

        // ---- 4. Status transitions driven by activity ----
        // Slice 1 Phase C handles only the happy-path Spawning → Working
        // flip. Slice 2 will add PR-driven transitions (pr_open, ci_failed,
        // etc.) once the tracker/scm plugins exist.
        if session.status == SessionStatus::Spawning
            && matches!(activity, ActivityState::Active | ActivityState::Ready)
        {
            self.transition(&mut session, SessionStatus::Working).await?;
        }

        Ok(())
    }

    /// Flip a session to a terminal state, persist, and emit both the
    /// `StatusChanged` (to `Terminated`) and the `Terminated` event.
    async fn terminate(&self, session: &mut Session, reason: TerminationReason) -> Result<()> {
        if session.status != SessionStatus::Terminated {
            self.transition(session, SessionStatus::Terminated).await?;
        }
        self.emit(OrchestratorEvent::Terminated {
            id: session.id.clone(),
            reason,
        });
        Ok(())
    }

    /// Transition status, persist, and emit `StatusChanged`.
    async fn transition(&self, session: &mut Session, to: SessionStatus) -> Result<()> {
        if session.status == to {
            return Ok(());
        }
        let from = session.status;
        session.status = to;
        self.sessions.save(session).await?;
        self.emit(OrchestratorEvent::StatusChanged {
            id: session.id.clone(),
            from,
            to,
        });
        Ok(())
    }

    /// Fire an event into the broadcast channel. A send error only means
    /// there are currently zero subscribers — that's expected during CLI
    /// startup and not worth surfacing.
    fn emit(&self, event: OrchestratorEvent) {
        let _ = self.events_tx.send(event);
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
            branch: format!("ao-{id}"),
            task: "test task".into(),
            workspace_path: Some(PathBuf::from("/tmp/ws")),
            runtime_handle: Some(format!("runtime-{id}")),
            activity: None,
            created_at: now_ms(),
        }
    }

    // ---------- Mock plugins ---------- //

    /// Runtime mock with a toggleable `alive` flag.
    struct MockRuntime {
        alive: AtomicBool,
    }

    impl MockRuntime {
        fn new(alive: bool) -> Self {
            Self {
                alive: AtomicBool::new(alive),
            }
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
        async fn send_message(&self, _handle: &str, _msg: &str) -> Result<()> {
            Ok(())
        }
        async fn is_alive(&self, _handle: &str) -> Result<bool> {
            Ok(self.alive.load(Ordering::SeqCst))
        }
        async fn destroy(&self, _handle: &str) -> Result<()> {
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

    /// Unused workspace mock kept around for the day we want to drive
    /// cleanup through lifecycle (Slice 2+).
    #[allow(dead_code)]
    struct MockWorkspace;
    #[async_trait]
    impl Workspace for MockWorkspace {
        async fn create(&self, _cfg: &WorkspaceCreateConfig) -> Result<PathBuf> {
            Ok(PathBuf::from("/tmp/ws"))
        }
        async fn destroy(&self, _workspace_path: &Path) -> Result<()> {
            Ok(())
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
        let (lifecycle, sessions, _rt, _agent, base) =
            setup("spawned", ActivityState::Ready).await;
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
            events.iter().any(|e| matches!(e, OrchestratorEvent::Spawned { .. })),
            "expected Spawned event, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ActivityChanged { next: ActivityState::Ready, .. }
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
        let (lifecycle, sessions, rt, _agent, base) =
            setup("dead", ActivityState::Ready).await;
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
        assert_eq!(persisted[0].status, SessionStatus::Terminated);
        assert!(persisted[0].is_terminal());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn exited_activity_terminates_with_agent_reason() {
        let (lifecycle, sessions, _rt, agent, base) =
            setup("exited", ActivityState::Ready).await;
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
        let (lifecycle, sessions, _rt, _agent, base) =
            setup("skip", ActivityState::Ready).await;
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
        let (lifecycle, sessions, _rt, _agent, base) =
            setup("once", ActivityState::Ready).await;
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
        assert!(saw_status_change, "background loop never emitted StatusChanged");

        let _ = std::fs::remove_dir_all(&base);
    }
}
