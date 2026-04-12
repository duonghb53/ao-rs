//! End-to-end integration test: lifecycle → reaction engine → notifier.
//!
//! Exercises the full notification pipeline using mock plugins and a
//! recording notifier. Proves that a status transition dispatches the
//! right reaction and the notifier registry delivers to the right
//! plugins.

use ao_core::{
    error::Result,
    events::OrchestratorEvent,
    lifecycle::LifecycleManager,
    notifier::{
        NotificationPayload, NotificationRouting, Notifier, NotifierError, NotifierRegistry,
    },
    reaction_engine::ReactionEngine,
    reactions::{EventPriority, ReactionAction, ReactionConfig},
    scm::{CiStatus, MergeReadiness, PrState, PullRequest, ReviewDecision},
    session_manager::SessionManager,
    traits::{Agent, Runtime, Scm},
    types::{ActivityState, Session, SessionId, SessionStatus},
};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

static DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_temp_dir(label: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("ao-e2e-{label}-{nanos}-{n}"))
}

fn fake_session(short: &str, project: &str) -> Session {
    Session {
        id: SessionId(format!("{short}-0000-0000-0000-000000000000")),
        project_id: project.to_string(),
        status: SessionStatus::Working,
        branch: format!("ao-{short}"),
        task: "test task".to_string(),
        workspace_path: Some(PathBuf::from("/tmp/fake-ws")),
        runtime_handle: Some(format!("tmux-{short}")),
        activity: Some(ActivityState::Ready),
        created_at: ao_core::now_ms(),
    }
}

fn fake_pr() -> PullRequest {
    PullRequest {
        number: 42,
        title: "fix tests".to_string(),
        branch: "ao-test".to_string(),
        base_branch: "main".to_string(),
        url: "https://github.com/test/test/pull/42".to_string(),
        owner: "test".to_string(),
        repo: "test".to_string(),
        is_draft: false,
    }
}

// ---------------------------------------------------------------------------
// Mock plugins
// ---------------------------------------------------------------------------

struct MockRuntime {
    alive: AtomicBool,
}

impl MockRuntime {
    fn new() -> Self {
        Self {
            alive: AtomicBool::new(true),
        }
    }
}

#[async_trait]
impl Runtime for MockRuntime {
    async fn create(
        &self,
        _id: &str,
        _cwd: &std::path::Path,
        _cmd: &str,
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

struct MockAgent;

#[async_trait]
impl Agent for MockAgent {
    fn launch_command(&self, _s: &Session) -> String {
        "echo mock".into()
    }
    fn environment(&self, _s: &Session) -> Vec<(String, String)> {
        vec![]
    }
    fn initial_prompt(&self, _s: &Session) -> String {
        "mock prompt".into()
    }
    async fn detect_activity(&self, _s: &Session) -> Result<ActivityState> {
        Ok(ActivityState::Ready)
    }
}

struct MockScm {
    pr: Mutex<Option<PullRequest>>,
    ci: Mutex<CiStatus>,
}

impl MockScm {
    fn new() -> Self {
        Self {
            pr: Mutex::new(None),
            ci: Mutex::new(CiStatus::Passing),
        }
    }

    fn set_pr(&self, pr: Option<PullRequest>) {
        *self.pr.lock().unwrap() = pr;
    }

    fn set_ci(&self, ci: CiStatus) {
        *self.ci.lock().unwrap() = ci;
    }
}

#[async_trait]
impl Scm for MockScm {
    fn name(&self) -> &str {
        "mock-scm"
    }
    async fn detect_pr(&self, _s: &Session) -> Result<Option<PullRequest>> {
        Ok(self.pr.lock().unwrap().clone())
    }
    async fn pr_state(&self, _pr: &PullRequest) -> Result<PrState> {
        Ok(PrState::Open)
    }
    async fn ci_checks(&self, _pr: &PullRequest) -> Result<Vec<ao_core::scm::CheckRun>> {
        Ok(vec![])
    }
    async fn ci_status(&self, _pr: &PullRequest) -> Result<CiStatus> {
        Ok(*self.ci.lock().unwrap())
    }
    async fn reviews(&self, _pr: &PullRequest) -> Result<Vec<ao_core::scm::Review>> {
        Ok(vec![])
    }
    async fn review_decision(&self, _pr: &PullRequest) -> Result<ReviewDecision> {
        Ok(ReviewDecision::None)
    }
    async fn pending_comments(
        &self,
        _pr: &PullRequest,
    ) -> Result<Vec<ao_core::scm::ReviewComment>> {
        Ok(vec![])
    }
    async fn mergeability(&self, _pr: &PullRequest) -> Result<MergeReadiness> {
        Ok(MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: false,
            no_conflicts: true,
            blockers: vec!["test".into()],
        })
    }
    async fn merge(
        &self,
        _pr: &PullRequest,
        _method: Option<ao_core::scm::MergeMethod>,
    ) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Recording + failing notifiers
// ---------------------------------------------------------------------------

struct RecordingNotifier {
    payloads: Mutex<Vec<NotificationPayload>>,
}

impl RecordingNotifier {
    fn new() -> Self {
        Self {
            payloads: Mutex::new(Vec::new()),
        }
    }

    fn recorded(&self) -> Vec<NotificationPayload> {
        self.payloads.lock().unwrap().clone()
    }
}

#[async_trait]
impl Notifier for RecordingNotifier {
    fn name(&self) -> &str {
        "recorder"
    }
    async fn send(&self, payload: &NotificationPayload) -> std::result::Result<(), NotifierError> {
        self.payloads.lock().unwrap().push(payload.clone());
        Ok(())
    }
}

struct FailingNotifier;

#[async_trait]
impl Notifier for FailingNotifier {
    fn name(&self) -> &str {
        "fail"
    }
    async fn send(&self, _payload: &NotificationPayload) -> std::result::Result<(), NotifierError> {
        Err(NotifierError::Io("intentional test failure".into()))
    }
}

// ---------------------------------------------------------------------------
// Setup helper
// ---------------------------------------------------------------------------

struct TestHarness {
    lifecycle: LifecycleManager,
    sessions: Arc<SessionManager>,
    scm: Arc<MockScm>,
    recorder: Arc<RecordingNotifier>,
    _base: PathBuf,
}

async fn setup(
    label: &str,
    reaction_config: HashMap<String, ReactionConfig>,
    routing: HashMap<EventPriority, Vec<String>>,
    extra_notifiers: Vec<(String, Arc<dyn Notifier>)>,
) -> TestHarness {
    let base = unique_temp_dir(label);
    std::fs::create_dir_all(base.join("sessions/test")).unwrap();

    let sessions = Arc::new(SessionManager::new(base.clone()));
    let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new());
    let agent: Arc<dyn Agent> = Arc::new(MockAgent);
    let scm: Arc<MockScm> = Arc::new(MockScm::new());

    let lifecycle = LifecycleManager::new(sessions.clone(), runtime.clone(), agent);

    // Build notifier registry with routing and recorder.
    let mut registry = NotifierRegistry::new(NotificationRouting::from_map(routing));
    let recorder = Arc::new(RecordingNotifier::new());
    registry.register("recorder", recorder.clone());
    for (name, notifier) in extra_notifiers {
        registry.register(&name, notifier);
    }

    let engine = Arc::new(
        ReactionEngine::new(reaction_config, runtime, lifecycle.events_sender())
            .with_scm(scm.clone() as Arc<dyn Scm>)
            .with_notifier_registry(registry),
    );

    let lifecycle = lifecycle
        .with_reaction_engine(engine)
        .with_scm(scm.clone() as Arc<dyn Scm>);

    TestHarness {
        lifecycle,
        sessions,
        scm,
        recorder,
        _base: base,
    }
}

fn drain_events(
    rx: &mut tokio::sync::broadcast::Receiver<OrchestratorEvent>,
) -> Vec<OrchestratorEvent> {
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    events
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Full chain: lifecycle tick → SCM detects CI failure → reaction engine
/// dispatches ci-failed → notifier registry resolves → recorder receives.
#[tokio::test]
async fn lifecycle_tick_triggers_notify_through_to_plugin() {
    let mut reactions = HashMap::new();
    reactions.insert(
        "ci-failed".to_string(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: Some("CI broke, please fix".into()),
            priority: Some(EventPriority::Action),
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
        },
    );

    let mut routing = HashMap::new();
    routing.insert(EventPriority::Action, vec!["recorder".to_string()]);

    let h = setup("notify-e2e", reactions, routing, vec![]).await;

    // Save a Working session, then set SCM to return a PR with failing CI.
    let session = fake_session("e2e1", "test");
    h.sessions.save(&session).await.unwrap();
    h.scm.set_pr(Some(fake_pr()));
    h.scm.set_ci(CiStatus::Failing);

    // Tick the lifecycle — should transition Working → CiFailed.
    let mut rx = h.lifecycle.subscribe();
    let mut seen = HashSet::new();
    h.lifecycle.tick(&mut seen).await.unwrap();

    // Verify the recorder received the notification.
    let recorded = h.recorder.recorded();
    assert_eq!(recorded.len(), 1, "expected exactly one notification");
    let p = &recorded[0];
    assert_eq!(p.reaction_key, "ci-failed");
    assert_eq!(p.priority, EventPriority::Action);
    assert_eq!(p.body, "CI broke, please fix");
    assert!(!p.escalated);
    assert_eq!(p.action, ReactionAction::Notify);

    // Verify events include StatusChanged and ReactionTriggered.
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::StatusChanged {
                from: SessionStatus::Working,
                to: SessionStatus::CiFailed,
                ..
            }
        )),
        "expected StatusChanged Working→CiFailed"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::ReactionTriggered {
                action: ReactionAction::Notify,
                ..
            }
        )),
        "expected ReactionTriggered with Notify"
    );
}

/// Escalation path: send-to-agent with retries=0 immediately escalates
/// on first attempt, falling through to dispatch_notify with escalated=true.
#[tokio::test]
async fn escalation_reaches_notifier_with_escalated_flag() {
    let mut reactions = HashMap::new();
    reactions.insert(
        "ci-failed".to_string(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::SendToAgent,
            message: Some("fix CI".into()),
            priority: Some(EventPriority::Action),
            retries: Some(0),
            escalate_after: Some(ao_core::reactions::EscalateAfter::Attempts(0)),
            threshold: None,
            include_summary: false,
        },
    );

    let mut routing = HashMap::new();
    routing.insert(EventPriority::Action, vec!["recorder".to_string()]);

    let h = setup("escalation-e2e", reactions, routing, vec![]).await;

    let session = fake_session("esc1", "test");
    h.sessions.save(&session).await.unwrap();
    h.scm.set_pr(Some(fake_pr()));
    h.scm.set_ci(CiStatus::Failing);

    let mut rx = h.lifecycle.subscribe();
    let mut seen = HashSet::new();
    h.lifecycle.tick(&mut seen).await.unwrap();

    // Verify the recorder received an escalated notification.
    let recorded = h.recorder.recorded();
    assert_eq!(
        recorded.len(),
        1,
        "expected exactly one escalated notification"
    );
    let p = &recorded[0];
    assert!(p.escalated, "expected escalated=true");
    assert_eq!(p.reaction_key, "ci-failed");

    // Verify ReactionEscalated event was emitted.
    let events = drain_events(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OrchestratorEvent::ReactionEscalated { .. })),
        "expected ReactionEscalated event"
    );
}

/// Partial failure: one notifier fails, the other still receives the payload.
/// The lifecycle tick completes normally (no crash from the failing plugin).
#[tokio::test]
async fn partial_failure_one_plugin_fails_others_succeed() {
    let mut reactions = HashMap::new();
    reactions.insert(
        "ci-failed".to_string(),
        ReactionConfig {
            auto: true,
            action: ReactionAction::Notify,
            message: Some("CI broke".into()),
            priority: Some(EventPriority::Action),
            retries: None,
            escalate_after: None,
            threshold: None,
            include_summary: false,
        },
    );

    // Route to both recorder and fail.
    let mut routing = HashMap::new();
    routing.insert(
        EventPriority::Action,
        vec!["recorder".to_string(), "fail".to_string()],
    );

    let extra: Vec<(String, Arc<dyn Notifier>)> =
        vec![("fail".to_string(), Arc::new(FailingNotifier))];

    let h = setup("partial-e2e", reactions, routing, extra).await;

    let session = fake_session("pf1", "test");
    h.sessions.save(&session).await.unwrap();
    h.scm.set_pr(Some(fake_pr()));
    h.scm.set_ci(CiStatus::Failing);

    let mut seen = HashSet::new();
    // Tick should complete without panicking, even though FailingNotifier errors.
    h.lifecycle.tick(&mut seen).await.unwrap();

    // Recorder still received the notification despite the failing sibling.
    let recorded = h.recorder.recorded();
    assert_eq!(
        recorded.len(),
        1,
        "recorder should still receive notification"
    );
    assert_eq!(recorded[0].reaction_key, "ci-failed");
}
