use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn default_auto_terminate_on_merge() -> bool {
    true
}

/// Lifecycle automation settings (TS: `LifecycleConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LifecycleConfig {
    /// Auto-terminate sessions when their PR merges (default: `true`).
    ///
    /// When enabled, the runtime (tmux) and worktree are destroyed within
    /// one tick of the PR state transitioning to `merged`. Set to `false`
    /// to opt out and manage session lifetimes manually.
    #[serde(
        default = "default_auto_terminate_on_merge",
        alias = "autoTerminateOnMerge"
    )]
    pub auto_terminate_on_merge: bool,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            auto_terminate_on_merge: true,
        }
    }
}
