use std::collections::BTreeMap;
use std::path::PathBuf;

use ao_core::AoConfig;

use crate::cli::project::resolve_repo_root;

const DEFAULT_NTFY_BASE_URL: &str = "https://ntfy.sh";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoutingPreset {
    UrgentOnly,
    UrgentAction,
    All,
}

impl RoutingPreset {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "urgent-only" => Ok(Self::UrgentOnly),
            "urgent-action" => Ok(Self::UrgentAction),
            "all" => Ok(Self::All),
            other => Err(format!(
                "invalid routing preset {other:?} (expected: urgent-only, urgent-action, all)"
            )),
        }
    }
}

fn env_first(keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn prompt_line(label: &str, default: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    use std::io::{self, Write};

    let mut out = io::stdout();
    if let Some(d) = default {
        write!(out, "{label} [{d}]: ")?;
    } else {
        write!(out, "{label}: ")?;
    }
    out.flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let v = line.trim().to_string();
    if v.is_empty() {
        Ok(default.unwrap_or_default().to_string())
    } else {
        Ok(v)
    }
}

fn ensure_mapping(v: &mut serde_yaml::Value) -> Result<&mut serde_yaml::Mapping, String> {
    match v {
        serde_yaml::Value::Mapping(m) => Ok(m),
        serde_yaml::Value::Null => {
            *v = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
            match v {
                serde_yaml::Value::Mapping(m) => Ok(m),
                _ => unreachable!(),
            }
        }
        other => Err(format!("expected YAML mapping at root, got {other:?}")),
    }
}

fn map_get_mut<'a>(map: &'a mut serde_yaml::Mapping, key: &str) -> &'a mut serde_yaml::Value {
    let k = serde_yaml::Value::String(key.to_string());
    map.entry(k).or_insert(serde_yaml::Value::Null)
}

fn set_string(map: &mut serde_yaml::Mapping, key: &str, value: impl Into<String>) {
    map.insert(
        serde_yaml::Value::String(key.to_string()),
        serde_yaml::Value::String(value.into()),
    );
}

fn set_seq(map: &mut serde_yaml::Mapping, key: &str, values: Vec<&str>) {
    map.insert(
        serde_yaml::Value::String(key.to_string()),
        serde_yaml::Value::Sequence(values.into_iter().map(|s| s.into()).collect()),
    );
}

fn desired_routing(preset: RoutingPreset) -> BTreeMap<&'static str, Vec<&'static str>> {
    // We always keep stdout in the route so users don't "lose" notifications if
    // ntfy isn't reachable or they haven't started the service yet.
    match preset {
        RoutingPreset::UrgentOnly => BTreeMap::from([
            ("urgent", vec!["stdout", "ntfy"]),
            ("action", vec!["stdout"]),
            ("warning", vec!["stdout"]),
            ("info", vec!["stdout"]),
        ]),
        RoutingPreset::UrgentAction => BTreeMap::from([
            ("urgent", vec!["stdout", "ntfy"]),
            ("action", vec!["stdout", "ntfy"]),
            ("warning", vec!["stdout"]),
            ("info", vec!["stdout"]),
        ]),
        RoutingPreset::All => BTreeMap::from([
            ("urgent", vec!["stdout", "ntfy"]),
            ("action", vec!["stdout", "ntfy"]),
            ("warning", vec!["stdout", "ntfy"]),
            ("info", vec!["stdout", "ntfy"]),
        ]),
    }
}

fn apply_openclaw_patch_to_yaml(
    existing_yaml: Option<&str>,
    url: &str,
    token: &str,
    preset: RoutingPreset,
) -> Result<String, String> {
    let mut root: serde_yaml::Value = match existing_yaml {
        Some(s) if !s.trim().is_empty() => {
            serde_yaml::from_str(s).map_err(|e| format!("failed to parse existing YAML: {e}"))?
        }
        _ => serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    };

    let root_map = ensure_mapping(&mut root)?;

    // --- notifiers.openclaw.{url,token} ---
    let notifiers = map_get_mut(root_map, "notifiers");
    let notifiers_map = ensure_mapping(notifiers)?;
    let openclaw = map_get_mut(notifiers_map, "openclaw");
    let openclaw_map = ensure_mapping(openclaw)?;
    set_string(openclaw_map, "url", url);
    set_string(openclaw_map, "token", token);
    // Keep a best-effort "plugin" hint for parity with TS configs.
    set_string(openclaw_map, "plugin", "openclaw");

    // --- notification-routing (canonical key is snake_case on write) ---
    let routing = map_get_mut(root_map, "notification_routing");
    let routing_map = ensure_mapping(routing)?;
    let desired = desired_routing(preset);
    for (priority, targets) in desired {
        set_seq(routing_map, priority, targets);
    }

    serde_yaml::to_string(&root).map_err(|e| format!("failed to render patched YAML: {e}"))
}

fn backup_path_for(config_path: &std::path::Path) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let file_name = config_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(AoConfig::CONFIG_FILENAME);
    config_path.with_file_name(format!("{file_name}.bak.{ms}"))
}

pub async fn openclaw(
    repo: Option<PathBuf>,
    url: Option<String>,
    token: Option<String>,
    routing_preset: String,
    non_interactive: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo_root = resolve_repo_root(repo)?;
    let config_path = AoConfig::path_in(&repo_root);

    let preset =
        RoutingPreset::parse(&routing_preset).map_err(|e| format!("--routing-preset: {e}"))?;

    let url_default = url
        .or_else(|| env_first(&["AO_OPENCLAW_URL", "AO_NTFY_URL"]))
        .unwrap_or_else(|| DEFAULT_NTFY_BASE_URL.to_string());
    let token_default = token.or_else(|| env_first(&["AO_OPENCLAW_TOKEN", "AO_NTFY_TOPIC"]));

    let (final_url, final_token) = if non_interactive {
        let token = token_default.ok_or_else(|| {
            "missing --token (or env AO_OPENCLAW_TOKEN / AO_NTFY_TOPIC) in --non-interactive mode"
                .to_string()
        })?;
        (url_default, token)
    } else {
        let url = prompt_line("Openclaw URL", Some(&url_default))?;
        let token = prompt_line("Openclaw token (ntfy topic)", token_default.as_deref())?;
        if token.trim().is_empty() {
            return Err("token/topic is required".into());
        }
        (url, token)
    };

    let existing = std::fs::read_to_string(&config_path).ok();
    let patched =
        apply_openclaw_patch_to_yaml(existing.as_deref(), &final_url, &final_token, preset)
            .map_err(|e| format!("failed to patch config: {e}"))?;

    if dry_run {
        println!("{patched}");
        return Ok(());
    }

    if config_path.exists() {
        let backup = backup_path_for(&config_path);
        std::fs::copy(&config_path, &backup)
            .map_err(|e| format!("failed to create backup {}: {e}", backup.display()))?;
    } else if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&config_path, patched.as_bytes())
        .map_err(|e| format!("failed to write {}: {e}", config_path.display()))?;

    println!("Updated {}", config_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_preserves_unknown_top_level_keys() {
        let input = r#"
port: 9999
unknownTopLevel: 123
"#;
        let out = apply_openclaw_patch_to_yaml(
            Some(input),
            "https://ntfy.example",
            "topic-123",
            RoutingPreset::UrgentOnly,
        )
        .unwrap();
        let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        let m = v.as_mapping().unwrap();
        assert!(m.contains_key(serde_yaml::Value::String("unknownTopLevel".into())));
        assert_eq!(
            m.get(serde_yaml::Value::String("port".into()))
                .unwrap()
                .as_i64()
                .unwrap(),
            9999
        );
    }

    #[test]
    fn patch_sets_notifiers_openclaw_fields() {
        let out = apply_openclaw_patch_to_yaml(
            Some("reactions: {}\n"),
            "https://ntfy.example",
            "topic-123",
            RoutingPreset::UrgentAction,
        )
        .unwrap();
        let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        let notifiers = v.get("notifiers").and_then(|n| n.as_mapping()).unwrap();
        let openclaw = notifiers
            .get(serde_yaml::Value::String("openclaw".into()))
            .and_then(|n| n.as_mapping())
            .unwrap();
        assert_eq!(
            openclaw
                .get(serde_yaml::Value::String("url".into()))
                .unwrap()
                .as_str()
                .unwrap(),
            "https://ntfy.example"
        );
        assert_eq!(
            openclaw
                .get(serde_yaml::Value::String("token".into()))
                .unwrap()
                .as_str()
                .unwrap(),
            "topic-123"
        );
    }

    #[test]
    fn patch_is_idempotent() {
        let first = apply_openclaw_patch_to_yaml(
            Some("projects: {}\n"),
            "https://ntfy.example",
            "topic-123",
            RoutingPreset::All,
        )
        .unwrap();
        let second = apply_openclaw_patch_to_yaml(
            Some(&first),
            "https://ntfy.example",
            "topic-123",
            RoutingPreset::All,
        )
        .unwrap();
        assert_eq!(first, second);
    }
}
