//! Agent/runtime plugin selection and multi-agent delegation.

use std::sync::Arc;

use ao_core::{ActivityState, Agent, AgentConfig, Runtime, Session};
use ao_plugin_agent_aider::AiderAgent;
use ao_plugin_agent_claude_code::ClaudeCodeAgent;
use ao_plugin_agent_codex::CodexAgent;
use ao_plugin_agent_cursor::CursorAgent;
use ao_plugin_runtime_process::ProcessRuntime;
use ao_plugin_runtime_tmux::TmuxRuntime;
use async_trait::async_trait;

/// Typed error for duplicate issue detection so `batch_spawn` can distinguish
/// "skipped duplicate" from "real failure" without string matching.
#[derive(Debug)]
pub(crate) struct DuplicateIssue {
    pub(crate) issue_id: String,
    pub(crate) session_short: String,
}

impl std::fmt::Display for DuplicateIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "active session {} is already working on issue #{}. use --force to spawn anyway",
            self.session_short, self.issue_id
        )
    }
}

impl std::error::Error for DuplicateIssue {}

/// Select an agent plugin by name, optionally reading rules from config.
///
/// Warns (but does not error) if the name is unknown — falls back to
/// `claude-code` so that older configs still work.
pub(crate) fn select_agent(name: &str, agent_config: Option<&AgentConfig>) -> Box<dyn Agent> {
    match name {
        "codex" => match agent_config {
            Some(cfg) => Box::new(CodexAgent::from_config(cfg)),
            None => Box::new(CodexAgent::new()),
        },
        "aider" => match agent_config {
            Some(cfg) => Box::new(AiderAgent::from_config(cfg)),
            None => Box::new(AiderAgent::new()),
        },
        "cursor" => match agent_config {
            Some(cfg) => Box::new(CursorAgent::from_config(cfg)),
            None => Box::new(CursorAgent::new()),
        },
        "claude-code" => match agent_config {
            Some(cfg) => Box::new(ClaudeCodeAgent::from_config(cfg)),
            None => Box::new(ClaudeCodeAgent::with_default_rules()),
        },
        _ => {
            eprintln!("warning: unknown agent '{name}', falling back to claude-code");
            match agent_config {
                Some(cfg) => Box::new(ClaudeCodeAgent::from_config(cfg)),
                None => Box::new(ClaudeCodeAgent::with_default_rules()),
            }
        }
    }
}

pub(crate) fn select_runtime(name: &str) -> Arc<dyn Runtime> {
    match name {
        "process" => Arc::new(ProcessRuntime::new()),
        "tmux" => Arc::new(TmuxRuntime::new()),
        _ => {
            eprintln!("warning: unknown runtime '{name}', falling back to tmux");
            Arc::new(TmuxRuntime::new())
        }
    }
}

pub(crate) struct MultiAgent;

#[async_trait]
impl Agent for MultiAgent {
    fn launch_command(&self, session: &Session) -> String {
        select_agent(&session.agent, session.agent_config.as_ref()).launch_command(session)
    }

    fn environment(&self, session: &Session) -> Vec<(String, String)> {
        select_agent(&session.agent, session.agent_config.as_ref()).environment(session)
    }

    fn initial_prompt(&self, session: &Session) -> String {
        select_agent(&session.agent, session.agent_config.as_ref()).initial_prompt(session)
    }

    async fn detect_activity(&self, session: &Session) -> ao_core::Result<ActivityState> {
        select_agent(&session.agent, session.agent_config.as_ref())
            .detect_activity(session)
            .await
    }

    async fn cost_estimate(
        &self,
        session: &Session,
    ) -> ao_core::Result<Option<ao_core::CostEstimate>> {
        select_agent(&session.agent, session.agent_config.as_ref())
            .cost_estimate(session)
            .await
    }
}

// ---------------------------------------------------------------------------
// Compile-time plugin registry (pure data)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginSlot {
    Agent,
    Runtime,
    Workspace,
    Tracker,
    Scm,
    Notifier,
}

impl PluginSlot {
    pub fn all() -> &'static [PluginSlot] {
        &[
            PluginSlot::Agent,
            PluginSlot::Runtime,
            PluginSlot::Workspace,
            PluginSlot::Tracker,
            PluginSlot::Scm,
            PluginSlot::Notifier,
        ]
    }
}

impl std::fmt::Display for PluginSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PluginSlot::Agent => "agent",
            PluginSlot::Runtime => "runtime",
            PluginSlot::Workspace => "workspace",
            PluginSlot::Tracker => "tracker",
            PluginSlot::Scm => "scm",
            PluginSlot::Notifier => "notifier",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDescriptor {
    pub slot: PluginSlot,
    pub name: &'static str,
    pub config_keys: &'static [&'static str],
    pub env_vars: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub struct PluginRegistry {
    plugins: Vec<PluginDescriptor>,
}

impl PluginRegistry {
    pub fn new(plugins: Vec<PluginDescriptor>) -> Self {
        Self { plugins }
    }

    pub fn names_for_slot(&self, slot: PluginSlot) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .plugins
            .iter()
            .filter(|p| p.slot == slot)
            .map(|p| p.name)
            .collect();
        v.sort();
        v
    }

    pub fn by_name(&self, name: &str) -> Option<&PluginDescriptor> {
        self.plugins.iter().find(|p| p.name == name)
    }
}

pub fn compiled_plugins() -> PluginRegistry {
    // Keep this list in sync with the compile-time wiring across `ao-cli`.
    PluginRegistry::new(vec![
        // ---- agent ----
        PluginDescriptor {
            slot: PluginSlot::Agent,
            name: "claude-code",
            config_keys: &[
                "defaults.agent",
                "defaults.worker.agent",
                "projects.<id>.agent",
                "projects.<id>.worker.agent",
                "projects.<id>.agent_config",
            ],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Agent,
            name: "cursor",
            config_keys: &[
                "defaults.agent",
                "defaults.worker.agent",
                "projects.<id>.agent",
                "projects.<id>.worker.agent",
                "projects.<id>.agent_config",
            ],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Agent,
            name: "aider",
            config_keys: &[
                "defaults.agent",
                "defaults.worker.agent",
                "projects.<id>.agent",
                "projects.<id>.worker.agent",
                "projects.<id>.agent_config",
            ],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Agent,
            name: "codex",
            config_keys: &[
                "defaults.agent",
                "defaults.worker.agent",
                "projects.<id>.agent",
                "projects.<id>.worker.agent",
                "projects.<id>.agent_config",
            ],
            env_vars: &["CODEX_HOME"],
        },
        // ---- runtime ----
        PluginDescriptor {
            slot: PluginSlot::Runtime,
            name: "tmux",
            config_keys: &["defaults.runtime", "projects.<id>.runtime"],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Runtime,
            name: "process",
            config_keys: &["defaults.runtime", "projects.<id>.runtime"],
            env_vars: &[],
        },
        // ---- workspace ----
        PluginDescriptor {
            slot: PluginSlot::Workspace,
            name: "worktree",
            config_keys: &[
                "defaults.workspace",
                "projects.<id>.workspace",
                "projects.<id>.symlinks",
                "projects.<id>.postCreate",
            ],
            env_vars: &[],
        },
        // ---- tracker ----
        PluginDescriptor {
            slot: PluginSlot::Tracker,
            name: "github",
            config_keys: &["defaults.tracker", "projects.<id>.tracker.plugin"],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Tracker,
            name: "linear",
            config_keys: &["defaults.tracker", "projects.<id>.tracker.plugin"],
            env_vars: &["LINEAR_API_TOKEN", "LINEAR_API_KEY"],
        },
        // ---- scm ----
        PluginDescriptor {
            slot: PluginSlot::Scm,
            name: "auto",
            config_keys: &["projects.<id>.scm.plugin"],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Scm,
            name: "github",
            config_keys: &["projects.<id>.scm.plugin"],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Scm,
            name: "gitlab",
            config_keys: &["projects.<id>.scm.plugin"],
            env_vars: &[],
        },
        // ---- notifier ----
        PluginDescriptor {
            slot: PluginSlot::Notifier,
            name: "stdout",
            config_keys: &[
                "defaults.notifiers[]",
                "notification_routing.<priority>[]",
                "notifiers.<name>.plugin",
            ],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Notifier,
            name: "desktop",
            config_keys: &[
                "defaults.notifiers[]",
                "notification_routing.<priority>[]",
                "notifiers.<name>.plugin",
            ],
            env_vars: &[],
        },
        PluginDescriptor {
            slot: PluginSlot::Notifier,
            name: "ntfy",
            config_keys: &[
                "defaults.notifiers[]",
                "notification_routing.<priority>[]",
                "notifiers.<name>.plugin",
            ],
            env_vars: &["AO_NTFY_TOPIC", "AO_NTFY_URL"],
        },
        PluginDescriptor {
            slot: PluginSlot::Notifier,
            name: "discord",
            config_keys: &[
                "defaults.notifiers[]",
                "notification_routing.<priority>[]",
                "notifiers.<name>.plugin",
            ],
            env_vars: &["AO_DISCORD_WEBHOOK_URL"],
        },
        PluginDescriptor {
            slot: PluginSlot::Notifier,
            name: "slack",
            config_keys: &[
                "defaults.notifiers[]",
                "notification_routing.<priority>[]",
                "notifiers.<name>.plugin",
            ],
            env_vars: &["AO_SLACK_WEBHOOK_URL"],
        },
    ])
}
