use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrchestratorSessionStrategy {
    Reuse,
    Delete,
    Ignore,
    DeleteNew,
    IgnoreNew,
    KillPrevious,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpencodeIssueSessionStrategy {
    Reuse,
    Delete,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingSessionAction {
    ReuseExisting,
    DeleteExistingAndReuseName,
    IgnoreExistingAndSpawnNew,
    Abort,
}

/// Minimal parity port of TS orchestrator-session-strategy behavior.
///
/// This is currently used only by parity tests; the ao-rs runtime has its own
/// session lifecycle implementation.
pub fn decide_existing_session_action(
    strategy: OrchestratorSessionStrategy,
    existing_found: bool,
) -> ExistingSessionAction {
    if !existing_found {
        return ExistingSessionAction::IgnoreExistingAndSpawnNew;
    }
    match strategy {
        OrchestratorSessionStrategy::Reuse => ExistingSessionAction::ReuseExisting,
        OrchestratorSessionStrategy::Delete => ExistingSessionAction::DeleteExistingAndReuseName,
        OrchestratorSessionStrategy::Ignore => ExistingSessionAction::Abort,
        OrchestratorSessionStrategy::DeleteNew => ExistingSessionAction::Abort,
        OrchestratorSessionStrategy::IgnoreNew => ExistingSessionAction::Abort,
        OrchestratorSessionStrategy::KillPrevious => {
            ExistingSessionAction::DeleteExistingAndReuseName
        }
    }
}
