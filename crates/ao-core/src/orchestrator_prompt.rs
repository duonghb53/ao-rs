//! Orchestrator prompt generator (TS `orchestrator-prompt.ts` equivalent).
//!
//! Renders `prompts/orchestrator.md` with `{{placeholder}}` substitution
//! and `{{SECTION_START}}…{{SECTION_END}}` conditional blocks so a single
//! template handles repo-configured / not-configured / rules-present /
//! reactions-present variants.
//!
//! Kept 1:1 with the TS rendering algorithm so ao-rs and ao-ts can share
//! the same template file going forward.

use crate::config::{AoConfig, ProjectConfig};
use crate::error::{AoError, Result};
use crate::reactions::ReactionAction;

const ORCHESTRATOR_TEMPLATE: &str = include_str!("prompts/orchestrator.md");

pub struct OrchestratorPromptConfig<'a> {
    pub config: &'a AoConfig,
    pub project_id: &'a str,
    pub project: &'a ProjectConfig,
    pub dashboard_port: u16,
}

/// Generate the orchestrator's system prompt from the bundled template.
///
/// Placeholder / block semantics mirror `packages/core/src/orchestrator-prompt.ts`:
/// - `{{name}}` is unconditionally substituted; an unresolved one aborts.
/// - `{{NAME_START}}...{{NAME_END}}` blocks are kept iff their section has content.
pub fn generate_orchestrator_prompt(opts: OrchestratorPromptConfig<'_>) -> Result<String> {
    let data = RenderData::from_opts(&opts);
    let stripped = apply_optional_blocks(ORCHESTRATOR_TEMPLATE.trim(), &data);
    let rendered = substitute_placeholders(&stripped, &data)?;
    Ok(rendered.trim().to_string())
}

// ---------------------------------------------------------------------------
// Render data
// ---------------------------------------------------------------------------

struct RenderData<'a> {
    project_id: &'a str,
    project_name: String,
    project_repo: String,
    project_default_branch: &'a str,
    project_session_prefix: String,
    project_path: &'a str,
    dashboard_port: String,
    automated_reactions_section: String,
    project_specific_rules_section: String,
    repo_configured: bool,
    repo_not_configured: bool,
    reactions_section_present: bool,
    rules_section_present: bool,
}

impl<'a> RenderData<'a> {
    fn from_opts(opts: &'a OrchestratorPromptConfig<'a>) -> Self {
        let has_repo = !opts.project.repo.trim().is_empty();
        let repo_display = if has_repo {
            opts.project.repo.clone()
        } else {
            "not configured".to_string()
        };

        let session_prefix = opts
            .project
            .session_prefix
            .clone()
            .unwrap_or_else(|| opts.project_id.to_string());

        let project_name = opts
            .project
            .name
            .clone()
            .unwrap_or_else(|| opts.project_id.to_string());

        let reactions = build_automated_reactions_section(opts);
        let rules = build_project_specific_rules_section(opts);

        Self {
            project_id: opts.project_id,
            project_name,
            project_repo: repo_display,
            project_default_branch: &opts.project.default_branch,
            project_session_prefix: session_prefix,
            project_path: &opts.project.path,
            dashboard_port: opts.dashboard_port.to_string(),
            reactions_section_present: !reactions.is_empty(),
            rules_section_present: !rules.is_empty(),
            automated_reactions_section: reactions,
            project_specific_rules_section: rules,
            repo_configured: has_repo,
            repo_not_configured: !has_repo,
        }
    }

    fn lookup_placeholder(&self, key: &str) -> Option<&str> {
        Some(match key {
            "projectId" => self.project_id,
            "projectName" => self.project_name.as_str(),
            "projectRepo" => self.project_repo.as_str(),
            "projectDefaultBranch" => self.project_default_branch,
            "projectSessionPrefix" => self.project_session_prefix.as_str(),
            "projectPath" => self.project_path,
            "dashboardPort" => self.dashboard_port.as_str(),
            "automatedReactionsSection" => self.automated_reactions_section.as_str(),
            "projectSpecificRulesSection" => self.project_specific_rules_section.as_str(),
            _ => return None,
        })
    }

    fn section_flag(&self, marker: &str) -> Option<bool> {
        Some(match marker {
            "REPO_CONFIGURED_SECTION" => self.repo_configured,
            "REPO_NOT_CONFIGURED_SECTION" => self.repo_not_configured,
            "AUTOMATED_REACTIONS_SECTION" => self.reactions_section_present,
            "PROJECT_SPECIFIC_RULES_SECTION" => self.rules_section_present,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Section / placeholder helpers
// ---------------------------------------------------------------------------

fn build_automated_reactions_section(opts: &OrchestratorPromptConfig<'_>) -> String {
    let mut lines = Vec::new();

    // Project-level reactions win; fall back to global reactions for display.
    let reactions = if !opts.project.reactions.is_empty() {
        &opts.project.reactions
    } else {
        &opts.config.reactions
    };

    for (event, reaction) in reactions {
        if !reaction.auto {
            continue;
        }
        match &reaction.action {
            ReactionAction::SendToAgent => {
                let retries = reaction
                    .retries
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "none".into());
                let escalate = reaction
                    .escalate_after
                    .as_ref()
                    .map(|v| match v {
                        crate::reactions::EscalateAfter::Attempts(n) => n.to_string(),
                        crate::reactions::EscalateAfter::Duration(s) => s.clone(),
                    })
                    .unwrap_or_else(|| "never".into());
                lines.push(format!(
                    "- **{event}**: Auto-sends instruction to agent (retries: {retries}, escalates after: {escalate})"
                ));
            }
            ReactionAction::Notify => {
                let priority = reaction
                    .priority
                    .map(|p| p.as_str().to_string())
                    .unwrap_or_else(|| "info".into());
                lines.push(format!(
                    "- **{event}**: Notifies human (priority: {priority})"
                ));
            }
            _ => {}
        }
    }

    lines.join("\n")
}

fn build_project_specific_rules_section(opts: &OrchestratorPromptConfig<'_>) -> String {
    let project_rules = opts
        .project
        .orchestrator_rules
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let default_rules = opts
        .config
        .defaults
        .as_ref()
        .and_then(|d| d.orchestrator_rules.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    project_rules
        .or(default_rules)
        .map(str::to_string)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Optional blocks: {{NAME_START}}...{{NAME_END}}
// ---------------------------------------------------------------------------

fn apply_optional_blocks(template: &str, data: &RenderData<'_>) -> String {
    let mut out = template.to_string();

    let markers = [
        "REPO_CONFIGURED_SECTION",
        "REPO_NOT_CONFIGURED_SECTION",
        "AUTOMATED_REACTIONS_SECTION",
        "PROJECT_SPECIFIC_RULES_SECTION",
    ];

    for marker in markers {
        let start = format!("{{{{{marker}_START}}}}");
        let end = format!("{{{{{marker}_END}}}}");
        let keep = data.section_flag(marker).unwrap_or(false);
        out = process_blocks(&out, &start, &end, keep);
    }

    out
}

fn process_blocks(source: &str, start: &str, end: &str, keep: bool) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;

    while let Some(start_idx) = rest.find(start) {
        out.push_str(&rest[..start_idx]);
        let after_start = &rest[start_idx + start.len()..];
        let Some(end_rel) = after_start.find(end) else {
            // Malformed: missing end marker. Keep the literal text so tests
            // notice instead of silently swallowing the remainder.
            out.push_str(start);
            out.push_str(after_start);
            return out;
        };
        let inner = &after_start[..end_rel];
        if keep {
            out.push_str(inner);
        } else {
            collapse_gap(&mut out);
        }
        rest = &after_start[end_rel + end.len()..];
        if !keep {
            rest = rest.trim_start_matches('\n');
        }
    }
    out.push_str(rest);
    out
}

/// When removing a block, collapse any trailing blank lines from `out` and
/// the block's original context so we don't leave a double-blank scar.
fn collapse_gap(out: &mut String) {
    while out.ends_with("\n\n") {
        out.pop();
    }
}

// ---------------------------------------------------------------------------
// Placeholder substitution: {{name}}
// ---------------------------------------------------------------------------

fn substitute_placeholders(template: &str, data: &RenderData<'_>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open_idx) = rest.find("{{") {
        out.push_str(&rest[..open_idx]);
        let after_open = &rest[open_idx + 2..];
        let Some(close_rel) = after_open.find("}}") else {
            // No closing — emit literal and bail.
            out.push_str("{{");
            out.push_str(after_open);
            return Ok(out);
        };
        let key = &after_open[..close_rel];
        let after_close = &after_open[close_rel + 2..];

        if is_valid_placeholder_key(key) {
            match data.lookup_placeholder(key) {
                Some(value) => out.push_str(value),
                None => {
                    return Err(AoError::PromptTemplate {
                        key: key.to_string(),
                    });
                }
            }
        } else {
            // Preserve non-identifier braces (e.g. shell examples) verbatim.
            out.push_str("{{");
            out.push_str(key);
            out.push_str("}}");
        }

        rest = after_close;
    }
    out.push_str(rest);
    Ok(out)
}

fn is_valid_placeholder_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !key.ends_with("_START")
        && !key.ends_with("_END")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, DefaultsConfig};
    use crate::reactions::{EscalateAfter, EventPriority, ReactionAction, ReactionConfig};
    use std::collections::HashMap;

    fn base_config() -> AoConfig {
        AoConfig {
            port: 3000,
            ready_threshold_ms: 300_000,
            poll_interval: 10,
            terminal_port: None,
            direct_terminal_port: None,
            power: None,
            defaults: Some(DefaultsConfig::default()),
            projects: HashMap::new(),
            reactions: HashMap::new(),
            notification_routing: Default::default(),
            notifiers: HashMap::new(),
            plugins: vec![],
        }
    }

    fn base_project(repo: &str) -> ProjectConfig {
        ProjectConfig {
            name: None,
            repo: repo.into(),
            path: "/tmp/my-app".into(),
            default_branch: "main".into(),
            session_prefix: Some("my-app".into()),
            branch_namespace: None,
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
            reactions: HashMap::new(),
            agent_rules: None,
            agent_rules_file: None,
            orchestrator_rules: None,
            orchestrator_session_strategy: None,
            opencode_issue_session_strategy: None,
        }
    }

    #[test]
    fn repo_configured_template_substitutes_placeholders() {
        let cfg = base_config();
        let project = base_project("acme/my-app");
        let prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 4100,
        }).unwrap();

        assert!(prompt.contains("# my-app Orchestrator"));
        assert!(prompt.contains("**Repository**: acme/my-app"));
        assert!(prompt.contains("**Session Prefix**: my-app"));
        assert!(prompt.contains("ao-rs send my-app-1"));
        assert!(prompt.contains("http://localhost:4100"));
        // Repo-configured block stays.
        assert!(prompt.contains("batch-spawn"));
        // Repo-not-configured block is stripped.
        assert!(!prompt.contains("No repository remote is configured"));
    }

    #[test]
    fn repo_not_configured_template_strips_pr_sections() {
        let cfg = base_config();
        let project = base_project("");
        let prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 3000,
        }).unwrap();

        assert!(prompt.contains("**Repository**: not configured"));
        assert!(prompt.contains("No repository remote is configured"));
        // PR takeover section is repo-configured only.
        assert!(!prompt.contains("PR Takeover"));
        assert!(!prompt.contains("batch-spawn"));
    }

    #[test]
    fn rules_block_present_when_project_rules_set() {
        let cfg = base_config();
        let mut project = base_project("acme/my-app");
        project.orchestrator_rules = Some("Prefer small PRs.".into());
        let prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 3000,
        }).unwrap();
        assert!(prompt.contains("## Project-Specific Rules"));
        assert!(prompt.contains("Prefer small PRs."));
    }

    #[test]
    fn rules_block_stripped_when_no_rules() {
        let cfg = AoConfig {
            defaults: None,
            ..base_config()
        };
        let project = base_project("acme/my-app");
        let prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 3000,
        }).unwrap();
        assert!(!prompt.contains("## Project-Specific Rules"));
    }

    #[test]
    fn reactions_section_rendered_when_auto_reactions_configured() {
        let mut cfg = base_config();
        cfg.reactions.insert(
            "ci-failed".into(),
            ReactionConfig {
                auto: true,
                action: ReactionAction::SendToAgent,
                message: None,
                priority: None,
                retries: Some(3),
                escalate_after: Some(EscalateAfter::Attempts(5)),
                threshold: None,
                include_summary: false,
                merge_method: None,
            },
        );
        cfg.reactions.insert(
            "approved-and-green".into(),
            ReactionConfig {
                auto: true,
                action: ReactionAction::Notify,
                message: None,
                priority: Some(EventPriority::Action),
                retries: None,
                escalate_after: None,
                threshold: None,
                include_summary: false,
                merge_method: None,
            },
        );
        let project = base_project("acme/my-app");
        let prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 3000,
        }).unwrap();
        assert!(prompt.contains("## Automated Reactions"));
        assert!(prompt.contains("**ci-failed**"));
        assert!(prompt.contains("retries: 3"));
        assert!(prompt.contains("escalates after: 5"));
        assert!(prompt.contains("**approved-and-green**"));
        assert!(prompt.contains("priority: action"));
    }

    #[test]
    fn reactions_section_stripped_when_no_auto_reactions() {
        let cfg = base_config();
        let project = base_project("acme/my-app");
        let prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 3000,
        }).unwrap();
        assert!(!prompt.contains("## Automated Reactions"));
    }

    #[test]
    fn non_negotiable_rules_and_send_guidance_always_present() {
        let cfg = base_config();
        let project = base_project("acme/my-app");
        let prompt = generate_orchestrator_prompt(OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 3000,
        }).unwrap();
        assert!(prompt.contains("Investigations from the orchestrator session are **read-only**"));
        assert!(prompt.contains("delegated to a **worker session**"));
        assert!(prompt.contains("Always use `ao-rs send`"));
        assert!(prompt.contains("tmux send-keys"));
    }

    #[test]
    fn unknown_placeholder_returns_err_prompt_template() {
        let cfg = base_config();
        let project = base_project("acme/my-app");
        let opts = OrchestratorPromptConfig {
            config: &cfg,
            project_id: "my-app",
            project: &project,
            dashboard_port: 3000,
        };
        let data = RenderData::from_opts(&opts);
        let result = substitute_placeholders("Hello {{unknownKey}} world", &data);
        match result {
            Err(crate::error::AoError::PromptTemplate { key }) => {
                assert_eq!(key, "unknownKey");
            }
            other => panic!("expected PromptTemplate error, got {other:?}"),
        }
    }
}
