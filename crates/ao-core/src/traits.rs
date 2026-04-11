use crate::{
    error::Result,
    types::{ActivityState, Session, WorkspaceCreateConfig},
};
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// How an agent process is executed (tmux, raw process, docker, ...).
///
/// The runtime returns an opaque `handle` string that the caller stores in
/// `Session::runtime_handle` and passes back to other methods.
#[async_trait]
pub trait Runtime: Send + Sync {
    /// Spawn a new isolated execution context (e.g. tmux session) and run the
    /// given launch command in it. `launch_command` is a single shell string
    /// — the runtime is responsible for any escaping/wrapping it needs.
    async fn create(
        &self,
        session_id: &str,
        cwd: &Path,
        launch_command: &str,
        env: &[(String, String)],
    ) -> Result<String>;

    async fn send_message(&self, handle: &str, msg: &str) -> Result<()>;
    async fn is_alive(&self, handle: &str) -> Result<bool>;
    async fn destroy(&self, handle: &str) -> Result<()>;
}

/// How a session's working directory is materialized (git worktree, full clone, ...).
#[async_trait]
pub trait Workspace: Send + Sync {
    /// Create an isolated copy of the repo on a new branch, returning its path.
    async fn create(&self, cfg: &WorkspaceCreateConfig) -> Result<PathBuf>;
    async fn destroy(&self, workspace_path: &Path) -> Result<()>;
}

/// A specific AI coding tool (Claude Code, Codex, Aider, Cursor, ...).
///
/// Mostly a metadata provider (launch command, env, prompt), plus one async
/// hook — `detect_activity` — which the lifecycle loop polls to learn what
/// the underlying agent process is currently doing. The TS reference does
/// this by tailing a JSONL log file the agent writes; Slice 1 Phase C's
/// stub just returns `Ready` so the polling loop has something to drive.
#[async_trait]
pub trait Agent: Send + Sync {
    /// Single shell string the runtime will run inside its execution context.
    fn launch_command(&self, session: &Session) -> String;
    fn environment(&self, session: &Session) -> Vec<(String, String)>;
    /// First prompt to deliver after the process is up.
    fn initial_prompt(&self, session: &Session) -> String;

    /// Inspect whatever evidence this agent leaves behind (log files,
    /// terminal scrollback, pid probes, ...) and report its current
    /// activity state. Called once per lifecycle tick.
    ///
    /// A default impl returns `Ready` so plugins can opt in gradually —
    /// matches the TS "no detection available" fallback.
    async fn detect_activity(&self, _session: &Session) -> Result<ActivityState> {
        Ok(ActivityState::Ready)
    }
}
