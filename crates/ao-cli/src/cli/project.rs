//! Repo root and project id resolution for CLI commands.

use std::path::PathBuf;
use std::process::Command as StdCommand;

use ao_core::{detect_git_repo, AoConfig};

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_repo_root(repo: Option<PathBuf>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let base = repo.unwrap_or(cwd);
    // Prefer the real repo root even if called from a subdir.
    let out = StdCommand::new("git")
        .arg("-C")
        .arg(&base)
        .args(["rev-parse", "--show-toplevel"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                Ok(base)
            } else {
                Ok(PathBuf::from(s))
            }
        }
        _ => Ok(base),
    }
}

pub(crate) fn default_project_id(repo_root: &std::path::Path) -> String {
    repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("demo")
        .to_string()
}

pub(crate) fn resolve_project_id(
    repo_path: &std::path::Path,
    ao_config: &AoConfig,
    cli_project: Option<String>,
) -> String {
    let repo_canon = std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    let matched_project_id = ao_config.projects.iter().find_map(|(id, cfg)| {
        let p = std::path::Path::new(&cfg.path);
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        (canon == repo_canon).then(|| id.clone())
    });
    let matched_by_repo_slug =
        detect_git_repo(repo_path)
            .ok()
            .and_then(|(owner_repo, _repo_name, _branch)| {
                ao_config
                    .projects
                    .iter()
                    .find_map(|(id, cfg)| (cfg.repo == owner_repo).then(|| id.clone()))
            });
    let matched_single = (ao_config.projects.len() == 1)
        .then(|| ao_config.projects.keys().next().cloned())
        .flatten();
    cli_project
        .or(matched_project_id)
        .or(matched_by_repo_slug)
        .or(matched_single)
        .unwrap_or_else(|| default_project_id(repo_path))
}
