use ao_core::{now_ms, ActivityState, Session, SessionId, SessionStatus};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ao-rs-ts-parity-{label}-{nanos}-{n}"))
}

#[allow(dead_code)]
pub fn fake_session(id: &str) -> Session {
    Session {
        id: SessionId(id.into()),
        project_id: "my-app".into(),
        status: SessionStatus::Spawning,
        agent: "claude-code".into(),
        agent_config: None,
        branch: "feat/test".into(),
        task: "t".into(),
        workspace_path: Some(PathBuf::from("/tmp/ws")),
        runtime_handle: Some(format!("rt-{id}")),
        runtime: "tmux".into(),
        activity: Some(ActivityState::Active),
        created_at: now_ms(),
        cost: None,
        issue_id: None,
        issue_url: None,
    }
}
