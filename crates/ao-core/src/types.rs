use crate::config::AgentConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Full lifecycle status, mirroring `packages/core/src/types.ts` in the
/// reference repo. Slice 1 Phase B expands the earlier 4-state set to the
/// complete lifecycle so the reaction engine in Slice 2 has real targets.
///
/// Kept verbatim from TS (same snake_case names) so YAML files are drop-in
/// comparable against the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Just created; workspace + runtime still being materialized.
    Spawning,
    /// Agent is actively working (or was, last we checked).
    Working,
    /// Agent opened a pull request; waiting for CI / review.
    PrOpen,
    /// CI on the PR failed — a candidate for auto-fix reaction.
    CiFailed,
    /// PR waiting on human review.
    ReviewPending,
    /// Review left change requests — candidate for auto-address reaction.
    ChangesRequested,
    /// Review approved but not yet mergeable (e.g. CI still running).
    Approved,
    /// Approved + green CI — ready to merge.
    Mergeable,
    /// Auto-merge ran but the underlying SCM call failed (network,
    /// branch-protection conflict, mergeability flake between the
    /// observation and the merge call). Parking state for the
    /// `Mergeable ↔ MergeFailed` retry loop — `derive_scm_status`
    /// re-promotes it to `Mergeable` on the next still-ready
    /// observation so the reaction engine can attempt the merge again
    /// and burn its retry budget. Leaves back to `PrOpen`/`CiFailed`/
    /// etc. when the PR stops being mergeable.
    ///
    /// Introduced in Slice 2 Phase G (M1 fix). See
    /// `docs/state-machine.md#the-mergefailed-parking-loop-phase-g`
    /// for the full transition table.
    MergeFailed,
    /// PR merged; session can be cleaned up.
    Merged,
    /// Post-merge cleanup in progress (worktree removal, branch delete).
    Cleanup,
    /// Agent is blocked on a question and waiting for human input.
    NeedsInput,
    /// Agent stopped making progress — long idle, no recent activity.
    Stuck,
    /// Unrecoverable failure (runtime crashed, plugin error, etc.).
    Errored,
    /// User explicitly killed the session.
    Killed,
    /// Ready but nothing to do; waiting for the user.
    Idle,
    /// Completed successfully — terminal state.
    Done,
    /// Runtime process exited on its own; orchestrator hasn't reclassified yet.
    Terminated,
}

impl SessionStatus {
    /// Terminal (dead) states — the runtime should be gone and the session
    /// won't transition further without user action.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Killed
                | Self::Terminated
                | Self::Done
                | Self::Cleanup
                | Self::Errored
                | Self::Merged
        )
    }

    /// `merged` is the only permanently non-restorable state — once the PR
    /// is gone, there's nothing left to resume into.
    pub const fn is_restorable(self) -> bool {
        self.is_terminal() && !matches!(self, Self::Merged)
    }

    /// Short lowercase label for CLI output. Matches the TS snake_case names.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spawning => "spawning",
            Self::Working => "working",
            Self::PrOpen => "pr_open",
            Self::CiFailed => "ci_failed",
            Self::ReviewPending => "review_pending",
            Self::ChangesRequested => "changes_requested",
            Self::Approved => "approved",
            Self::Mergeable => "mergeable",
            Self::MergeFailed => "merge_failed",
            Self::Merged => "merged",
            Self::Cleanup => "cleanup",
            Self::NeedsInput => "needs_input",
            Self::Stuck => "stuck",
            Self::Errored => "errored",
            Self::Killed => "killed",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Terminated => "terminated",
        }
    }
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Activity state as detected by the Agent plugin.
///
/// Separate from `SessionStatus` because one status can span multiple
/// activity states (e.g. a `working` session can be `active`, `ready`, or
/// `idle` depending on how long since the last log line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    /// Agent is processing (thinking, writing code).
    Active,
    /// Agent finished its turn, alive and waiting for input.
    Ready,
    /// Agent has been inactive for a while (stale).
    Idle,
    /// Agent is asking a question / permission prompt.
    WaitingInput,
    /// Agent hit an error or is stuck.
    Blocked,
    /// Agent process is no longer running.
    Exited,
}

impl ActivityState {
    /// True if the agent process is no longer running. Mirrors the TS
    /// `TERMINAL_ACTIVITIES` set — which is exactly `{exited}` today.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Exited)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Ready => "ready",
            Self::Idle => "idle",
            Self::WaitingInput => "waiting_input",
            Self::Blocked => "blocked",
            Self::Exited => "exited",
        }
    }
}

impl std::fmt::Display for ActivityState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub repo_path: PathBuf,
    pub default_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub project_id: String,
    pub status: SessionStatus,
    /// Agent plugin name used to spawn this session (e.g. "claude-code", "cursor").
    ///
    /// `#[serde(default)]` keeps older session YAML (written before multiple
    /// agents were supported) deserializable.
    #[serde(default = "default_agent_name")]
    pub agent: String,
    /// Agent config captured at spawn time (effective/inline).
    ///
    /// We persist this on the session so `restore` can relaunch with the same
    /// rules/permissions even when the original repo-level `ao-rs.yaml` is not
    /// available from the worktree path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_config: Option<AgentConfig>,
    pub branch: String,
    pub task: String,
    pub workspace_path: Option<PathBuf>,
    /// Opaque handle returned by the Runtime plugin (e.g. tmux session name).
    pub runtime_handle: Option<String>,
    /// Runtime plugin name used to spawn this session (e.g. "tmux", "process").
    ///
    /// `#[serde(default)]` keeps older session YAML (written before multiple
    /// runtimes were supported) deserializable — they default to "tmux".
    #[serde(default = "default_runtime_name")]
    pub runtime: String,
    /// Activity from the Agent plugin's last `detect_activity` call.
    /// `None` until the lifecycle loop has polled at least once —
    /// which also keeps old YAML files (written before Phase B added this
    /// field) deserializable.
    #[serde(default)]
    pub activity: Option<ActivityState>,
    /// Unix epoch milliseconds when this session was first persisted.
    /// Used for sorting newest-first in `ao-rs status`.
    pub created_at: u64,
    /// Aggregated token usage / cost from the agent plugin.
    /// `None` until the first successful `Agent::cost_estimate` poll.
    /// `#[serde(default)]` keeps old session YAML (written before cost
    /// tracking) deserializable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostEstimate>,
    /// Tracker issue id this session was spawned from (e.g. `"42"`).
    /// `None` when spawned with `--task` (prompt-only mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    /// Canonical issue URL (e.g. `https://github.com/owner/repo/issues/42`).
    /// `None` when spawned with `--task`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
}

impl Session {
    /// Combined terminal check: either the status *or* the activity says
    /// the session is dead. Mirrors `isTerminalSession` in the TS reference.
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal() || self.activity.is_some_and(ActivityState::is_terminal)
    }

    /// Can this session be restored by `ao-rs session restore`?
    ///
    /// Must be terminal first (nothing to restore if it's still running),
    /// and not in a permanently non-restorable state like `merged`.
    pub fn is_restorable(&self) -> bool {
        self.is_terminal() && self.status.is_restorable()
    }
}

/// Current Unix time in milliseconds. Helper for `Session::created_at`.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn default_agent_name() -> String {
    "claude-code".into()
}

fn default_runtime_name() -> String {
    "tmux".into()
}

/// Aggregated token usage and estimated dollar cost for a session.
///
/// Source of truth is the agent's JSONL log (Claude Code writes `usage`
/// blocks on every assistant turn). The lifecycle loop polls this on
/// status changes and persists it on the `Session` YAML. A monthly
/// cost ledger (`~/.ao-rs/cost-ledger/YYYY-MM.yaml`) keeps a permanent
/// backup so cost data survives JSONL deletion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// Estimated total cost in USD, computed from Anthropic's published
    /// pricing at the time the tokens were consumed.
    ///
    /// `f64` is sufficient for reporting precision. Avoid exact equality
    /// comparisons on this field — use the token counts for deterministic
    /// checks instead.
    pub cost_usd: f64,
}

/// Input to `Workspace::create`. Carries everything the plugin needs to
/// materialize an isolated working directory for a session.
#[derive(Debug, Clone)]
pub struct WorkspaceCreateConfig {
    pub project_id: String,
    pub session_id: String,
    pub branch: String,
    pub repo_path: PathBuf,
    pub default_branch: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_terminal_set_matches_ts_reference() {
        // Exact mirror of TERMINAL_STATUSES in packages/core/src/types.ts.
        let terminal = [
            SessionStatus::Killed,
            SessionStatus::Terminated,
            SessionStatus::Done,
            SessionStatus::Cleanup,
            SessionStatus::Errored,
            SessionStatus::Merged,
        ];
        for s in terminal {
            assert!(s.is_terminal(), "{s} should be terminal");
        }

        // A few non-terminal spot checks.
        for s in [
            SessionStatus::Spawning,
            SessionStatus::Working,
            SessionStatus::PrOpen,
            SessionStatus::Stuck,
            SessionStatus::Idle,
            // MergeFailed is a *parking* state in the auto-merge retry
            // loop, not a terminal state — the next SCM observation
            // re-promotes it to Mergeable so the engine can burn
            // another retry attempt. Phase G would regress if someone
            // mechanically matched it against `Merged` and treated it
            // as done.
            SessionStatus::MergeFailed,
        ] {
            assert!(!s.is_terminal(), "{s} should NOT be terminal");
        }
    }

    #[test]
    fn merge_failed_serializes_as_snake_case() {
        // Lock the on-disk label. A rogue rename to `mergeFailed` or
        // `merge-failed` would break the lifecycle state machine's
        // round-trip through the session yaml.
        let s = SessionStatus::MergeFailed;
        assert_eq!(s.as_str(), "merge_failed");
        assert_eq!(serde_yaml::to_string(&s).unwrap().trim(), "merge_failed");
        let parsed: SessionStatus = serde_yaml::from_str("merge_failed").unwrap();
        assert_eq!(parsed, SessionStatus::MergeFailed);
    }

    #[test]
    fn only_merged_is_non_restorable_among_terminal() {
        assert!(!SessionStatus::Merged.is_restorable());
        assert!(SessionStatus::Done.is_restorable());
        assert!(SessionStatus::Killed.is_restorable());
        assert!(SessionStatus::Errored.is_restorable());
        // Non-terminal can't be restored either (nothing to restore from).
        assert!(!SessionStatus::Working.is_restorable());
    }

    #[test]
    fn activity_exited_is_terminal() {
        assert!(ActivityState::Exited.is_terminal());
        for a in [
            ActivityState::Active,
            ActivityState::Ready,
            ActivityState::Idle,
            ActivityState::WaitingInput,
            ActivityState::Blocked,
        ] {
            assert!(!a.is_terminal(), "{a} should NOT be terminal");
        }
    }

    #[test]
    fn session_is_terminal_combines_status_and_activity() {
        let base = Session {
            id: SessionId("x".into()),
            project_id: "demo".into(),
            status: SessionStatus::Working,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "feat-x".into(),
            task: "t".into(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: None,
            created_at: 0,
            cost: None,
            issue_id: None,
            issue_url: None,
        };
        assert!(!base.is_terminal());

        // Status alone can mark it terminal.
        let mut done = base.clone();
        done.status = SessionStatus::Done;
        assert!(done.is_terminal());

        // Activity alone can mark it terminal (status still says "working"
        // but the runtime process is gone).
        let mut exited = base.clone();
        exited.activity = Some(ActivityState::Exited);
        assert!(exited.is_terminal());
    }

    #[test]
    fn merged_session_is_terminal_but_not_restorable() {
        let merged = Session {
            id: SessionId("x".into()),
            project_id: "demo".into(),
            status: SessionStatus::Merged,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "feat-x".into(),
            task: "t".into(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: None,
            created_at: 0,
            cost: None,
            issue_id: None,
            issue_url: None,
        };
        assert!(merged.is_terminal());
        assert!(!merged.is_restorable());
    }

    #[test]
    fn serde_roundtrip_uses_snake_case() {
        let s = SessionStatus::ChangesRequested;
        let yaml = serde_yaml::to_string(&s).unwrap();
        assert_eq!(yaml.trim(), "changes_requested");
        let parsed: SessionStatus = serde_yaml::from_str("pr_open").unwrap();
        assert_eq!(parsed, SessionStatus::PrOpen);

        let a = ActivityState::WaitingInput;
        let ayaml = serde_yaml::to_string(&a).unwrap();
        assert_eq!(ayaml.trim(), "waiting_input");
    }

    #[test]
    fn old_yaml_without_activity_field_still_deserializes() {
        // Session yaml written in Phase A (before `activity` existed) must
        // still load — the `#[serde(default)]` on the field is what makes
        // this work. Regression guard for disk-format compat.
        let yaml = r#"
id: "abc"
project_id: demo
status: working
branch: feat-x
task: "an old task"
workspace_path: null
runtime_handle: null
created_at: 1700000000000
"#;
        let s: Session = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(s.id.0, "abc");
        assert_eq!(s.agent, "claude-code");
        assert!(s.agent_config.is_none());
        assert!(s.activity.is_none());
        assert!(s.cost.is_none());
    }

    #[test]
    fn cost_estimate_serde_roundtrip() {
        let cost = CostEstimate {
            input_tokens: 5000,
            output_tokens: 2000,
            cache_read_tokens: 1000,
            cache_creation_tokens: 500,
            cost_usd: 0.06,
        };
        let yaml = serde_yaml::to_string(&cost).unwrap();
        let parsed: CostEstimate = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, cost);
    }

    #[test]
    fn session_with_cost_roundtrips_through_yaml() {
        let session = Session {
            id: SessionId("cost-test".into()),
            project_id: "demo".into(),
            status: SessionStatus::Working,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "feat-cost".into(),
            task: "track tokens".into(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: None,
            created_at: 0,
            cost: Some(CostEstimate {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 10,
                cache_creation_tokens: 5,
                cost_usd: 0.001,
            }),
            issue_id: None,
            issue_url: None,
        };
        let yaml = serde_yaml::to_string(&session).unwrap();
        let parsed: Session = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.cost, session.cost);
    }

    #[test]
    fn session_without_cost_field_deserializes() {
        // Backward compat: YAML written before cost tracking.
        let yaml = r#"
id: "old"
project_id: demo
status: working
branch: feat-old
task: "old task"
workspace_path: null
runtime_handle: null
created_at: 0
"#;
        let s: Session = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(s.agent, "claude-code");
        assert!(s.agent_config.is_none());
        assert!(s.cost.is_none());
    }
}
