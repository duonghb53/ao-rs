//! `ao-rs start` — generate or load project config.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use ao_core::{
    default_orchestrator_rules, generate_config, install_skills, AgentConfig, AoConfig,
    LoadedConfig, PermissionsMode, RoleAgentConfig,
};

use crate::cli::browser::spawn_open_dashboard_browser;
use crate::cli::printing::print_config_warnings;
use crate::cli::project::resolve_repo_root;
use crate::commands::dashboard::{dashboard, dashboard_only};
use crate::commands::watch::watch;

pub struct StartOptions {
    pub repo: Option<PathBuf>,
    pub run: bool,
    pub no_dashboard: bool,
    pub no_orchestrator: bool,
    pub port: u16,
    pub interval_override: Option<Duration>,
    pub open: bool,
    pub rebuild: bool,
    pub interactive: bool,
}

fn confirm_overwrite(path: &std::path::Path) -> io::Result<bool> {
    eprint!("{} already exists. Overwrite? [y/N] ", path.display());
    let _ = io::stderr().flush();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

pub async fn start(opts: StartOptions) -> Result<(), Box<dyn std::error::Error>> {
    let repo_root = resolve_repo_root(opts.repo)?;
    let config_path = AoConfig::path_in(&repo_root);

    if config_path.exists() && !opts.rebuild {
        // Load existing config and print summary.
        let LoadedConfig {
            mut config,
            warnings,
        } = AoConfig::load_from_with_warnings(&config_path)
            .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;
        print_config_warnings(&config_path, &warnings);

        // Backfill newly-added orchestrator fields so `ao-rs start` upgrades older configs.
        let mut changed = false;

        // Ensure defaults.orchestrator/worker exist (outer `defaults:` section).
        if let Some(defaults) = config.defaults.as_mut() {
            if defaults.orchestrator.is_none() {
                defaults.orchestrator = Some(RoleAgentConfig {
                    agent: Some("cursor".into()),
                    agent_config: Some(AgentConfig {
                        permissions: PermissionsMode::Permissionless,
                        rules: None,
                        rules_file: None,
                        model: None,
                        orchestrator_model: None,
                        opencode_session_id: None,
                    }),
                });
                changed = true;
            }
            if defaults.worker.is_none() {
                defaults.worker = Some(RoleAgentConfig {
                    agent: Some("cursor".into()),
                    agent_config: None,
                });
                changed = true;
            }
            if defaults
                .orchestrator_rules
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                defaults.orchestrator_rules = Some(default_orchestrator_rules().to_string());
                changed = true;
            }
        }

        for (_id, project) in config.projects.iter_mut() {
            // Migrate per-project orchestrator/worker blocks up into defaults when they match
            // the standard cursor setup. After migration we keep project-level overrides only
            // when they are truly custom.
            if let Some(defaults) = config.defaults.as_ref() {
                let default_orch_agent = defaults
                    .orchestrator
                    .as_ref()
                    .and_then(|o| o.agent.as_deref());
                let default_worker_agent =
                    defaults.worker.as_ref().and_then(|w| w.agent.as_deref());

                let is_default_orch = project
                    .orchestrator
                    .as_ref()
                    .and_then(|o| o.agent.as_deref())
                    .map(|a| Some(a) == default_orch_agent)
                    .unwrap_or(false);

                let is_default_worker = project
                    .worker
                    .as_ref()
                    .and_then(|w| w.agent.as_deref())
                    .map(|a| Some(a) == default_worker_agent)
                    .unwrap_or(false);

                if is_default_orch {
                    project.orchestrator = None;
                    changed = true;
                }
                if is_default_worker {
                    project.worker = None;
                    changed = true;
                }

                // If a project's orchestrator_rules matches the defaults, drop it so the project inherits.
                let default_rules = defaults.orchestrator_rules.as_deref().unwrap_or("").trim();
                if !default_rules.is_empty() {
                    let project_rules = project.orchestrator_rules.as_deref().unwrap_or("").trim();
                    if !project_rules.is_empty() && project_rules == default_rules {
                        project.orchestrator_rules = None;
                        changed = true;
                    }
                }
            }

            // If per-project orchestrator_rules is empty/missing, do nothing — it inherits defaults.orchestrator_rules.
            if project
                .orchestrator_rules
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
                && project.orchestrator_rules.is_some()
            {
                project.orchestrator_rules = None;
                changed = true;
            }
        }
        if changed {
            config
                .save_to(&config_path)
                .map_err(|e| format!("failed to write {}: {e}", config_path.display()))?;
        }

        println!("Config already exists: {}", config_path.display());
        println!();
        if let Some(ref defaults) = config.defaults {
            println!("  defaults:");
            println!("    runtime:   {}", defaults.runtime);
            println!("    agent:     {}", defaults.agent);
            println!("    workspace: {}", defaults.workspace);
            if !defaults.notifiers.is_empty() {
                println!("    notifiers: {}", defaults.notifiers.join(", "));
            }
        }
        if !config.projects.is_empty() {
            println!(
                "  projects:  {}",
                config
                    .projects
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("  reactions: {} configured", config.reactions.len());
        println!(
            "  routing:   {} priority level(s)",
            config.notification_routing.len()
        );
        println!();
        println!("Edit {} to customize.", config_path.display());
        let should_run = opts.run || opts.no_dashboard || opts.no_orchestrator;
        if !should_run {
            return Ok(());
        }

        if opts.no_dashboard {
            if opts.open {
                eprintln!("(warn) --open ignored because --no-dashboard was set");
            }
            return watch(opts.interval_override).await;
        }

        if opts.open {
            spawn_open_dashboard_browser(opts.port);
        }

        if opts.no_orchestrator {
            if opts.interval_override.is_some() {
                eprintln!("(warn) --interval ignored because --no-orchestrator was set");
            }
            return dashboard_only(opts.port).await;
        }

        return dashboard(opts.port, opts.interval_override, false).await;
    }

    if opts.rebuild && config_path.exists() && opts.interactive && !confirm_overwrite(&config_path)?
    {
        return Ok(());
    }

    // Generate new config by detecting the current git repo.
    let config =
        generate_config(&repo_root).map_err(|e| format!("failed to detect project: {e}"))?;

    config
        .save_to(&config_path)
        .map_err(|e| format!("failed to write {}: {e}", config_path.display()))?;

    // Install ai-devkit skills (non-fatal).
    println!("→ installing ai-devkit skills...");
    match install_skills(&repo_root) {
        Ok(()) => println!("  ✓ skills installed"),
        Err(e) => println!("  ⚠ skill installation skipped: {e}"),
    }

    println!();
    println!("Created {}", config_path.display());
    println!();
    if let Some(ref defaults) = config.defaults {
        println!("  defaults:");
        println!("    runtime:   {}", defaults.runtime);
        println!("    agent:     {}", defaults.agent);
        println!("    workspace: {}", defaults.workspace);
    }
    for (name, project) in &config.projects {
        println!("  project \"{}\":", name);
        println!("    repo:           {}", project.repo);
        println!("    path:           {}", project.path);
        println!("    default_branch: {}", project.default_branch);
        if let Some(ref ac) = project.agent_config {
            println!("    permissions:    {}", ac.permissions);
        }
    }
    println!("  reactions: {} configured", config.reactions.len());
    println!(
        "  routing:   {} priority level(s)",
        config.notification_routing.len()
    );
    println!();
    println!("Edit {} to customize.", config_path.display());
    let should_run = opts.run || opts.no_dashboard || opts.no_orchestrator;
    if !should_run {
        return Ok(());
    }

    if opts.no_dashboard {
        if opts.open {
            eprintln!("(warn) --open ignored because --no-dashboard was set");
        }
        return watch(opts.interval_override).await;
    }

    if opts.open {
        spawn_open_dashboard_browser(opts.port);
    }

    if opts.no_orchestrator {
        if opts.interval_override.is_some() {
            eprintln!("(warn) --interval ignored because --no-orchestrator was set");
        }
        dashboard_only(opts.port).await
    } else {
        dashboard(opts.port, opts.interval_override, false).await
    }
}
