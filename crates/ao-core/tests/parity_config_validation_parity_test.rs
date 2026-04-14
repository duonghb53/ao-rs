use ao_core::parity_config_validation::{
    validate_project_uniqueness, TsOrchestratorConfig, TsProjectConfig,
};
use std::collections::HashMap;

fn cfg(projects: Vec<(&str, &str, Option<&str>)>) -> TsOrchestratorConfig {
    let mut map = HashMap::new();
    for (key, path, prefix) in projects {
        map.insert(
            key.to_string(),
            TsProjectConfig {
                path: path.to_string(),
                repo: "org/repo".to_string(),
                default_branch: "main".to_string(),
                session_prefix: prefix.map(|s| s.to_string()),
            },
        );
    }
    TsOrchestratorConfig { projects: map }
}

#[test]
fn rejects_duplicate_project_ids_same_basename() {
    let c = cfg(vec![
        ("proj1", "/repos/integrator", None),
        ("proj2", "/other/integrator", None),
    ]);
    let err = validate_project_uniqueness(&c).unwrap_err();
    assert!(err.contains("Duplicate project ID"));
    assert!(err.contains("integrator"));
}

#[test]
fn error_message_shows_conflicting_paths() {
    let c = cfg(vec![
        ("proj1", "/repos/integrator", None),
        ("proj2", "/other/integrator", None),
    ]);
    let err = validate_project_uniqueness(&c).unwrap_err();
    assert!(err.contains("/repos/integrator"));
    assert!(err.contains("/other/integrator"));
}

#[test]
fn accepts_unique_basenames() {
    let c = cfg(vec![
        ("proj1", "/repos/integrator", None),
        ("proj2", "/repos/backend", None),
    ]);
    validate_project_uniqueness(&c).unwrap();
}

#[test]
fn rejects_duplicate_explicit_prefixes() {
    let c = cfg(vec![
        ("proj1", "/repos/integrator", Some("app")),
        ("proj2", "/repos/backend", Some("app")),
    ]);
    let err = validate_project_uniqueness(&c).unwrap_err();
    assert!(err.contains("Duplicate session prefix"));
    assert!(err.contains("\"app\""));
}

#[test]
fn rejects_duplicate_auto_generated_prefixes() {
    // integrator -> int; international -> int
    let c = cfg(vec![
        ("proj1", "/repos/integrator", None),
        ("proj2", "/repos/international", None),
    ]);
    let err = validate_project_uniqueness(&c).unwrap_err();
    assert!(err.contains("Duplicate session prefix"));
    assert!(err.contains("\"int\""));
}
