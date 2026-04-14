//! Orchestrator prompt generator (TS `orchestrator-prompt.ts` equivalent).
//!
//! This is intended for an "orchestrator session" whose job is to manage
//! worker sessions, not to implement code changes directly.

use crate::config::{AoConfig, ProjectConfig};

pub struct OrchestratorPromptConfig<'a> {
    pub config: &'a AoConfig,
    pub project_id: &'a str,
    pub project: &'a ProjectConfig,
    pub dashboard_port: u16,
}

pub fn generate_orchestrator_prompt(opts: OrchestratorPromptConfig<'_>) -> String {
    let mut sections: Vec<String> = Vec::new();

    sections.push(format!(
        "# {} Orchestrator\n\n\
You are the **orchestrator agent** for the `{}` project.\n\n\
Your role is to coordinate and manage worker agent sessions. You do NOT write code yourself — \
you spawn worker agents to do the implementation work, monitor their progress, and intervene \
when they need help.",
        opts.project_id, opts.project_id
    ));

    sections.push(
        "## Non-Negotiable Rules\n\n\
- Investigations from the orchestrator session are **read-only**. Inspect status, logs, metadata, PR state, and worker output, but do not edit repository files or implement fixes from the orchestrator session.\n\
- Any code change, test run tied to implementation, git branch work, or PR takeover must be delegated to a **worker session**.\n\
- The orchestrator session must never own a PR. Never claim a PR into the orchestrator session.\n\
- If an investigation discovers follow-up work, either spawn a worker session or direct an existing worker session with clear instructions.\n\
- **Always use `ao-rs send` to communicate with sessions** — never use raw `tmux send-keys` or `tmux capture-pane`.\n\
- When a session might be busy, prefer sending a concise instruction and let the lifecycle loop + reactions drive follow-ups."
            .to_string(),
    );

    let rules = opts
        .project
        .orchestrator_rules
        .as_deref()
        .filter(|r| !r.trim().is_empty())
        .or_else(|| {
            opts.config
                .defaults
                .as_ref()
                .and_then(|d| d.orchestrator_rules.as_deref())
                .filter(|r| !r.trim().is_empty())
        });
    if let Some(rules) = rules {
        sections.push(format!("## Orchestrator Rules (from config)\n\n{rules}"));
    }

    sections.push(format!(
        "## Project Info\n\n\
- **Project ID**: {}\n\
- **Repository**: {}\n\
- **Default Branch**: {}\n\
- **Local Path**: {}\n\
- **Dashboard URL**: http://127.0.0.1:{}",
        opts.project_id,
        opts.project.repo,
        opts.project.default_branch,
        opts.project.path,
        opts.dashboard_port
    ));

    sections.push(
        "## Quick Start\n\n\
```bash\n\
# See all sessions\n\
ao-rs status\n\
\n\
# Spawn a worker for a GitHub issue\n\
ao-rs spawn --issue 123\n\
\n\
# Spawn multiple issues\n\
ao-rs batch-spawn 1 2 3\n\
\n\
# Send instructions to a session\n\
ao-rs send <session> \"Your message here\"\n\
\n\
# Run dashboard + orchestrator loop\n\
ao-rs start --run --open\n\
```\n"
            .to_string(),
    );

    if !opts.config.reactions.is_empty() {
        sections.push(format!(
            "## Automated Reactions\n\n\
This project has {} reaction(s) configured. Use `ao-rs watch` (or the dashboard event log) to observe when they fire.",
            opts.config.reactions.len()
        ));
    }

    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, DefaultsConfig};
    use std::collections::HashMap;

    #[test]
    fn prompt_contains_read_only_rules_and_send_guidance() {
        let cfg = AoConfig {
            port: 3000,
            terminal_port: None,
            direct_terminal_port: None,
            power: None,
            defaults: Some(DefaultsConfig::default()),
            projects: HashMap::new(),
            reactions: HashMap::new(),
            notification_routing: Default::default(),
            notifiers: HashMap::new(),
            plugins: vec![],
        };
        let project = ProjectConfig {
            name: None,
            repo: "org/my-app".into(),
            path: "/tmp/my-app".into(),
            default_branch: "main".into(),
            session_prefix: None,
            runtime: None,
            agent: None,
            workspace: None,
            tracker: None,
            scm: None,
            symlinks: vec![],
            post_create: vec![],
            agent_config: Some(AgentConfig::default()),
            orchestrator: None,
            worker: None,
            orchestrator_rules: None,
        };
        let prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 3000,
        });
        assert!(prompt.contains("Investigations from the orchestrator session are **read-only**"));
        assert!(prompt.contains("delegated to a **worker session**"));
        assert!(prompt.contains("Always use `ao-rs send`"));
        assert!(prompt.contains("tmux send-keys"));
    }
}
