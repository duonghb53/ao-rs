//! `ao-rs plugin` commands (crate-based plugin registry).

use crate::cli::plugins::{compiled_plugins, PluginDescriptor, PluginSlot};

pub async fn list() -> Result<(), Box<dyn std::error::Error>> {
    let registry = compiled_plugins();

    println!("compiled-in plugins:");
    println!();

    for slot in PluginSlot::all() {
        let names = registry.names_for_slot(*slot);
        if names.is_empty() {
            continue;
        }
        println!("{slot}:");
        for name in names {
            println!("  - {name}");
        }
        println!();
    }

    println!("docs: `docs/plugin-spec.md`");
    Ok(())
}

pub async fn info(name: String) -> Result<(), Box<dyn std::error::Error>> {
    let registry = compiled_plugins();
    let Some(plugin) = registry.by_name(name.trim()) else {
        return Err(format!("unknown plugin {:?} (try: `ao-rs plugin list`)", name).into());
    };

    print_plugin_descriptor(plugin);
    Ok(())
}

fn print_plugin_descriptor(p: &PluginDescriptor) {
    println!("plugin: {}", p.name);
    println!("slot:   {}", p.slot);

    if !p.config_keys.is_empty() {
        println!();
        println!("config keys:");
        for k in p.config_keys {
            println!("  - {k}");
        }
    }

    if !p.env_vars.is_empty() {
        println!();
        println!("env vars:");
        for k in p.env_vars {
            println!("  - {k}");
        }
    }
}
