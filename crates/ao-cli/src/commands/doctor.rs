//! `ao-rs doctor` — environment checks.

use ao_core::{paths, AoConfig, LoadedConfig, SessionManager};

use crate::cli::printing::print_config_warnings;

pub async fn doctor() -> Result<(), Box<dyn std::error::Error>> {
    println!("ao-rs doctor");
    println!("────────────────────────────────────────");

    let mut failures = 0u32;

    // 1. Required CLI tools on PATH.
    for tool in ["git", "gh", "tmux", "claude"] {
        let status = which(tool).await;
        match status {
            ToolStatus::Found(path) => println!("  PASS  {tool:<10} {path}"),
            ToolStatus::NotFound => {
                println!("  FAIL  {tool:<10} not found on PATH");
                failures += 1;
            }
        }
    }

    // 2. gh auth status — verify GitHub authentication.
    let gh_auth = tokio::process::Command::new("gh")
        .args(["auth", "status"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    match gh_auth {
        Ok(s) if s.success() => println!("  PASS  {:<10} authenticated", "gh auth"),
        Ok(_) => {
            println!(
                "  FAIL  {:<10} not authenticated (run `gh auth login`)",
                "gh auth"
            );
            failures += 1;
        }
        Err(_) => {
            println!("  WARN  {:<10} could not run `gh auth status`", "gh auth");
        }
    }

    // 3. Config file loads without error.
    let config_path = AoConfig::local_path();
    match AoConfig::load_from_or_default_with_warnings(&config_path) {
        Ok(LoadedConfig {
            config: cfg,
            warnings,
        }) => {
            let projects = cfg.projects.len();
            let reactions = cfg.reactions.len();
            if config_path.exists() {
                println!(
                    "  PASS  {:<10} {} ({projects} project(s), {reactions} reaction(s))",
                    "config",
                    config_path.display()
                );
                print_config_warnings(&config_path, &warnings);
            } else {
                println!(
                    "  WARN  {:<10} no config file (run `ao-rs start`)",
                    "config"
                );
            }
        }
        Err(e) => {
            println!("  FAIL  {:<10} {} — {e}", "config", config_path.display());
            failures += 1;
        }
    }

    // 4. Sessions directory exists.
    let sessions_dir = paths::default_sessions_dir();
    if sessions_dir.is_dir() {
        let count = SessionManager::with_default()
            .list()
            .await
            .map(|s| s.len())
            .unwrap_or(0);
        println!(
            "  PASS  {:<10} {} ({count} session(s))",
            "sessions",
            sessions_dir.display()
        );
    } else {
        println!(
            "  WARN  {:<10} {} does not exist yet",
            "sessions",
            sessions_dir.display()
        );
    }

    println!("────────────────────────────────────────");
    if failures > 0 {
        println!("  {failures} check(s) FAILED");
        std::process::exit(1);
    } else {
        println!("  all checks passed");
    }

    Ok(())
}

/// Check if a tool is on PATH.
pub(crate) enum ToolStatus {
    Found(String),
    NotFound,
}

pub(crate) async fn which(tool: &str) -> ToolStatus {
    let output = tokio::process::Command::new("which")
        .arg(tool)
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            ToolStatus::Found(path)
        }
        _ => ToolStatus::NotFound,
    }
}
