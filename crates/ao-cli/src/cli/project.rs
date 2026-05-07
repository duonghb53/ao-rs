//! Repo root and project id resolution for CLI commands.

use std::path::PathBuf;
use std::process::Command as StdCommand;

use ao_core::{detect_git_repo, AoConfig, ProjectConfig};

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_repo_root(
    repo: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
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

/// CLI wrapper over `ProjectConfig::worktree_repo_path`: returns the
/// fallback when no project config is matched, otherwise delegates.
pub(crate) fn resolve_worktree_repo_path(
    project_config: Option<&ProjectConfig>,
    fallback: PathBuf,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match project_config {
        Some(p) => p.worktree_repo_path(&fallback).map_err(|e| e.into()),
        None => Ok(fallback),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn project_with_path(path: &str) -> ProjectConfig {
        ProjectConfig {
            name: None,
            repo: "owner/repo".into(),
            path: path.into(),
            default_branch: "main".into(),
            session_prefix: None,
            branch_namespace: None,
            runtime: None,
            agent: None,
            workspace: None,
            tracker: None,
            scm: None,
            symlinks: vec![],
            post_create: vec![],
            agent_config: None,
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
    fn worktree_repo_path_returns_fallback_when_no_project_config() {
        let fb = PathBuf::from("/tmp/fb");
        let got = resolve_worktree_repo_path(None, fb.clone()).unwrap();
        assert_eq!(got, fb);
    }

    #[test]
    fn worktree_repo_path_returns_fallback_when_path_empty() {
        let p = project_with_path("");
        let fb = PathBuf::from("/tmp/fb");
        let got = resolve_worktree_repo_path(Some(&p), fb.clone()).unwrap();
        assert_eq!(got, fb);
    }

    #[test]
    fn worktree_repo_path_returns_fallback_when_path_whitespace() {
        let p = project_with_path("   ");
        let fb = PathBuf::from("/tmp/fb");
        let got = resolve_worktree_repo_path(Some(&p), fb.clone()).unwrap();
        assert_eq!(got, fb);
    }

    #[test]
    fn worktree_repo_path_errors_when_path_set_but_not_git_repo() {
        let dir = TempDir::new().unwrap();
        let p = project_with_path(dir.path().to_str().unwrap());
        let res = resolve_worktree_repo_path(Some(&p), PathBuf::from("/tmp/fb"));
        assert!(res.is_err());
    }

    #[test]
    fn worktree_repo_path_returns_configured_when_path_is_git_repo() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let p = project_with_path(dir.path().to_str().unwrap());
        let got = resolve_worktree_repo_path(Some(&p), PathBuf::from("/tmp/fb")).unwrap();
        // canonicalize resolves symlinks (e.g. macOS /var → /private/var),
        // so compare against the canonicalized tempdir.
        assert_eq!(got, std::fs::canonicalize(dir.path()).unwrap());
    }
}
