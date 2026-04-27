//! Issue #24 Phase 1: targeted regression tests for retry/escalation + merge_failed loop.
//!
//! These are *integration* tests (under `crates/ao-core/tests`) to ensure the
//! public surface area stays TS-aligned for the in-scope reactions:
//! `ci-failed`, `changes-requested`, `approved-and-green`, `agent-stuck`.

use ao_core::{
    error::Result,
    events::OrchestratorEvent,
    lifecycle::LifecycleManager,
    reaction_engine::ReactionEngine,
    reactions::{EscalateAfter, ReactionAction, ReactionConfig},
    scm::{
        CheckRun, CiStatus, MergeMethod, MergeReadiness, PrState, PullRequest, Review,
        ReviewComment, ReviewDecision,
    },
    session_manager::SessionManager,
    traits::{Agent, Runtime, Scm},
    types::{ActivityState, Session, SessionId, SessionStatus},
};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::broadcast;

static DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_temp_dir(label: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("ao-issue24-{label}-{nanos}-{n}"))
}

fn fake_session(id: &str, project: &str) -> Session {
    Session {
        id: SessionId(format!("{id}-0000-0000-0000-000000000000")),
        project_id: project.to_string(),
        status: SessionStatus::Working,
        agent: "claude-code".to_string(),
        agent_config: None,
        branch: format!("ao-{id}"),
        task: "test task".to_string(),
        workspace_path: Some(PathBuf::from("/tmp/fake-ws")),
        runtime_handle: Some(format!("tmux-{id}")),
        runtime: "tmux".into(),
        activity: Some(ActivityState::Ready),
        created_at: ao_core::now_ms(),
        cost: None,
        issue_id: None,
        issue_url: None,
        claimed_pr_number: None,
        claimed_pr_url: None,
        initial_prompt_override: None,
        spawned_by: None,
        last_merge_conflict_dispatched: None,
        last_review_backlog_fingerprint: None,
        last_automated_review_fingerprint: None,
        last_automated_review_dispatch_hash: None,
    }
}

fn fake_pr() -> PullRequest {
    PullRequest {
        number: 24,
        title: "issue24".to_string(),
        branch: "ao-issue24".to_string(),
        base_branch: "main".to_string(),
        url: "https://github.com/test/test/pull/24".to_string(),
        owner: "test".to_string(),
        repo: "test".to_string(),
        is_draft: false,
    }
}

// ---------------------------------------------------------------------------
// Mock plugins
// ---------------------------------------------------------------------------

struct AlwaysAliveRuntime;

#[async_trait]
impl Runtime for AlwaysAliveRuntime {
    async fn create(
        &self,
        _session_id: &str,
        _cwd: &std::path::Path,
        _launch_command: &str,
        _env: &[(String, String)],
    ) -> Result<String> {
        Ok("tmux-handle".into())
    }

    async fn is_alive(&self, _handle: &str) -> Result<bool> {
        Ok(true)
    }

    async fn send_message(&self, _handle: &str, _message: &str) -> Result<()> {
        Ok(())
    }

    async fn destroy(&self, _handle: &str) -> Result<()> {
        Ok(())
    }
}

struct ReadyAgent;

#[async_trait]
impl Agent for ReadyAgent {
    fn launch_command(&self, _session: &Session) -> String {
        "true".into()
    }

    fn environment(&self, _session: &Session) -> Vec<(String, String)> {
        vec![]
    }

    fn initial_prompt(&self, _session: &Session) -> String {
        "hi".into()
    }

    async fn detect_activity(&self, _session: &Session) -> Result<ActivityState> {
        Ok(ActivityState::Ready)
    }
}

#[derive(Default)]
struct MergeableButMergeFailsScm {
    merge_calls: Mutex<usize>,
}

impl MergeableButMergeFailsScm {
    fn merge_calls(&self) -> usize {
        *self.merge_calls.lock().unwrap()
    }
}

#[async_trait]
impl Scm for MergeableButMergeFailsScm {
    fn name(&self) -> &str {
        "mock"
    }

    async fn detect_pr(&self, _session: &Session) -> Result<Option<PullRequest>> {
        Ok(Some(fake_pr()))
    }

    async fn pr_state(&self, _pr: &PullRequest) -> Result<PrState> {
        Ok(PrState::Open)
    }

    async fn ci_checks(&self, _pr: &PullRequest) -> Result<Vec<CheckRun>> {
        Ok(vec![])
    }

    async fn ci_status(&self, _pr: &PullRequest) -> Result<CiStatus> {
        Ok(CiStatus::Passing)
    }

    async fn reviews(&self, _pr: &PullRequest) -> Result<Vec<Review>> {
        Ok(vec![])
    }

    async fn review_decision(&self, _pr: &PullRequest) -> Result<ReviewDecision> {
        Ok(ReviewDecision::Approved)
    }

    async fn pending_comments(&self, _pr: &PullRequest) -> Result<Vec<ReviewComment>> {
        Ok(vec![])
    }

    async fn mergeability(&self, _pr: &PullRequest) -> Result<MergeReadiness> {
        Ok(MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        })
    }

    async fn merge(&self, _pr: &PullRequest, _method: Option<MergeMethod>) -> Result<()> {
        let mut n = self.merge_calls.lock().unwrap();
        *n += 1;
        Err(ao_core::error::AoError::Runtime("merge failed".into()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retries_attempts_gate_escalates_deterministically() -> Result<()> {
    let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
    cfg.message = Some("fix".into());
    cfg.retries = Some(1);

    let mut map = HashMap::new();
    map.insert("ci-failed".to_string(), cfg);

    let (events_tx, _rx) = broadcast::channel(64);
    let engine = ReactionEngine::new(map, Arc::new(AlwaysAliveRuntime), events_tx);
    let session = fake_session("s1", "p1");

    let r1 = engine.dispatch(&session, "ci-failed").await?.unwrap();
    assert!(!r1.escalated);
    assert_eq!(r1.action, ReactionAction::SendToAgent);

    // attempts=2 > retries=1 => escalates on the second call.
    let r2 = engine.dispatch(&session, "ci-failed").await?.unwrap();
    assert!(r2.escalated);
    assert_eq!(r2.action, ReactionAction::Notify);
    Ok(())
}

#[tokio::test]
async fn escalate_after_duration_gate_fires_after_elapsed_exceeds_threshold() -> Result<()> {
    let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
    cfg.message = Some("fix".into());
    cfg.retries = None;
    cfg.escalate_after = Some(EscalateAfter::Duration("1s".into()));

    let mut map = HashMap::new();
    map.insert("ci-failed".to_string(), cfg);

    let (events_tx, _rx) = broadcast::channel(64);
    let engine = ReactionEngine::new(map, Arc::new(AlwaysAliveRuntime), events_tx);
    let session = fake_session("s1", "p1");

    let r1 = engine.dispatch(&session, "ci-failed").await?.unwrap();
    assert!(!r1.escalated);

    // Strict `elapsed > 1s` requires waiting past 1 second.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let r2 = engine.dispatch(&session, "ci-failed").await?.unwrap();
    assert!(r2.escalated);
    assert_eq!(r2.action, ReactionAction::Notify);
    Ok(())
}

#[tokio::test]
async fn merge_failed_parking_loop_preserves_approved_and_green_tracker() -> Result<()> {
    let base = unique_temp_dir("merge_failed_tracker");
    let sessions = Arc::new(SessionManager::new(base.clone()));

    let s = fake_session("s1", "p1");
    sessions.save(&s).await?;

    let mut cfg = ReactionConfig::new(ReactionAction::AutoMerge);
    cfg.retries = Some(10); // ensure no escalation; we want repeated parking.
    let mut map = HashMap::new();
    map.insert("approved-and-green".to_string(), cfg);

    let runtime = Arc::new(AlwaysAliveRuntime);
    let scm = Arc::new(MergeableButMergeFailsScm::default());
    let agent = Arc::new(ReadyAgent);

    let lifecycle =
        LifecycleManager::new(sessions.clone(), runtime.clone(), agent).with_scm(scm.clone());

    let mut events_rx = lifecycle.subscribe();

    let engine = Arc::new(
        ReactionEngine::new(map, runtime, lifecycle.events_sender()).with_scm(scm.clone()),
    );
    let lifecycle = lifecycle.with_reaction_engine(engine.clone());

    // Tick 1: Working -> Mergeable -> dispatch auto-merge (fails) -> park MergeFailed.
    lifecycle.tick(&mut HashSet::new()).await?;

    // Tick 2: MergeFailed -> Mergeable -> dispatch again -> park again.
    lifecycle.tick(&mut HashSet::new()).await?;

    // The retry budget must accumulate across Mergeable <-> MergeFailed.
    assert_eq!(
        engine.attempts(&s.id, "approved-and-green"),
        2,
        "expected attempts to persist across parking loop"
    );

    // Sanity: merge was actually attempted twice.
    assert_eq!(scm.merge_calls(), 2);

    // And we must not have spammed escalations on these two attempts.
    let mut escalations = 0usize;
    while let Ok(ev) = events_rx.try_recv() {
        if matches!(ev, OrchestratorEvent::ReactionEscalated { .. }) {
            escalations += 1;
        }
    }
    assert_eq!(escalations, 0);

    // Cleanup temp dir best-effort.
    let _ = tokio::fs::remove_dir_all(base).await;
    Ok(())
}
