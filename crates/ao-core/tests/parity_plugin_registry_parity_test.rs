use ao_core::parity_plugin_registry::{PluginManifest, PluginModule, PluginRegistry, PluginSlot};
use std::sync::Arc;

fn make_plugin(slot: &str, name: &str) -> PluginModule {
    let slot_s = slot.to_string();
    let name_s = name.to_string();
    PluginModule {
        manifest: PluginManifest {
            name: name_s.clone(),
            slot: PluginSlot(slot_s.clone()),
            description: format!("Test {slot_s} plugin: {name_s}"),
            version: "0.0.1".to_string(),
        },
        create: Arc::new(move |config| {
            serde_json::json!({
              "name": name_s,
              "_config": config,
            })
        }),
    }
}

#[test]
fn register_and_get() {
    let mut r = PluginRegistry::new();
    r.register(make_plugin("runtime", "tmux"), None);
    let inst = r.get(&PluginSlot("runtime".into()), "tmux").unwrap();
    assert_eq!(inst.instance["name"], "tmux");
}

#[test]
fn get_returns_none_for_unregistered() {
    let r = PluginRegistry::new();
    assert!(r.get(&PluginSlot("runtime".into()), "nonexistent").is_none());
}

#[test]
fn passes_config_to_create() {
    let mut r = PluginRegistry::new();
    r.register(
        make_plugin("workspace", "worktree"),
        Some(serde_json::json!({"worktreeDir": "/custom/path"})),
    );
    let inst = r.get(&PluginSlot("workspace".into()), "worktree").unwrap();
    assert_eq!(inst.instance["_config"]["worktreeDir"], "/custom/path");
}

#[test]
fn overwrites_previously_registered() {
    let mut r = PluginRegistry::new();
    r.register(make_plugin("runtime", "tmux"), None);
    r.register(make_plugin("runtime", "tmux"), Some(serde_json::json!({"x": 1})));
    let inst = r.get(&PluginSlot("runtime".into()), "tmux").unwrap();
    assert_eq!(inst.instance["_config"]["x"], 1);
}

#[test]
fn registers_different_slots_independently() {
    let mut r = PluginRegistry::new();
    r.register(make_plugin("runtime", "tmux"), None);
    r.register(make_plugin("workspace", "worktree"), None);
    assert!(r.get(&PluginSlot("runtime".into()), "tmux").is_some());
    assert!(r.get(&PluginSlot("workspace".into()), "worktree").is_some());
    assert!(r.get(&PluginSlot("runtime".into()), "worktree").is_none());
}

#[test]
fn list_plugins_in_slot() {
    let mut r = PluginRegistry::new();
    r.register(make_plugin("runtime", "tmux"), None);
    r.register(make_plugin("runtime", "process"), None);
    r.register(make_plugin("workspace", "worktree"), None);
    let mut runtimes = r.list(&PluginSlot("runtime".into()));
    runtimes.sort();
    assert_eq!(runtimes, vec!["process".to_string(), "tmux".to_string()]);
}

