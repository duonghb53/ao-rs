use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsProjectConfig {
    pub repo: String,
    pub path: String,
    pub default_branch: String,
    pub session_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsOrchestratorConfig {
    pub projects: HashMap<String, TsProjectConfig>,
}

pub fn generate_session_prefix(project_id: &str) -> String {
    if project_id.len() <= 4 {
        return project_id.to_lowercase();
    }

    let uppercase: Vec<char> = project_id.chars().filter(|c| c.is_ascii_uppercase()).collect();
    if uppercase.len() > 1 {
        return uppercase.into_iter().collect::<String>().to_lowercase();
    }

    if project_id.contains('-') || project_id.contains('_') {
        let sep = if project_id.contains('-') { '-' } else { '_' };
        return project_id
            .split(sep)
            .filter(|w| !w.is_empty())
            .filter_map(|w| w.chars().next())
            .collect::<String>()
            .to_lowercase();
    }

    project_id.chars().take(3).collect::<String>().to_lowercase()
}

pub fn validate_project_uniqueness(config: &TsOrchestratorConfig) -> Result<(), String> {
    let mut basenames: HashSet<String> = HashSet::new();
    let mut basename_to_paths: HashMap<String, Vec<String>> = HashMap::new();

    for project in config.projects.values() {
        let basename = Path::new(&project.path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        basename_to_paths
            .entry(basename.clone())
            .or_default()
            .push(project.path.clone());

        if basenames.contains(&basename) {
            let paths = basename_to_paths
                .get(&basename)
                .cloned()
                .unwrap_or_default()
                .join(", ");
            return Err(format!(
                "Duplicate project ID detected: \"{basename}\"\nMultiple projects have the same directory basename:\n  {paths}\n\nTo fix this, ensure each project path has a unique directory name.\nAlternatively, you can use the config key as a unique identifier."
            ));
        }
        basenames.insert(basename);
    }

    let mut prefixes: HashSet<String> = HashSet::new();
    let mut prefix_to_project_key: HashMap<String, String> = HashMap::new();

    for (config_key, project) in config.projects.iter() {
        let basename = Path::new(&project.path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let prefix = project
            .session_prefix
            .clone()
            .unwrap_or_else(|| generate_session_prefix(&basename));

        if prefixes.contains(&prefix) {
            let first = prefix_to_project_key.get(&prefix).cloned().unwrap_or_default();
            let first_path = config
                .projects
                .get(&first)
                .map(|p| p.path.clone())
                .unwrap_or_default();
            return Err(format!(
                "Duplicate session prefix detected: \"{prefix}\"\nProjects \"{first}\" and \"{config_key}\" would generate the same prefix.\n\nTo fix this, add an explicit sessionPrefix to one of these projects:\n\nprojects:\n  {first}:\n    path: {first_path}\n    sessionPrefix: {prefix}1  # Add explicit prefix\n  {config_key}:\n    path: {path}\n    sessionPrefix: {prefix}2  # Add explicit prefix\n",
                path = project.path
            ));
        }

        prefixes.insert(prefix.clone());
        prefix_to_project_key.insert(prefix, config_key.clone());
    }

    Ok(())
}

