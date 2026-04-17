//! TS plugin registry (ported from `packages/core/src/plugin-registry.ts`).
//!
//! Parity status: test-only.
//!
//! The real ao-rs plugin wiring lives at the workspace level (per-slot
//! crates and explicit registration in `ao-cli`). This module only mirrors
//! the TS registry shape for parity comparison. See
//! `docs/ts-core-parity-report.md` → "Parity-only modules".

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginSlot(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManifest {
    pub name: String,
    pub slot: PluginSlot,
    pub description: String,
    pub version: String,
}

#[derive(Clone)]
pub struct PluginModule {
    pub manifest: PluginManifest,
    pub create: Arc<dyn Fn(Option<serde_json::Value>) -> serde_json::Value + Send + Sync>,
}

#[derive(Debug, Clone)]
pub struct PluginInstance {
    pub manifest: PluginManifest,
    pub instance: serde_json::Value,
    pub config: Option<serde_json::Value>,
}

#[derive(Default)]
pub struct PluginRegistry {
    by_slot: HashMap<PluginSlot, HashMap<String, PluginInstance>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: PluginModule, config: Option<serde_json::Value>) {
        let slot = plugin.manifest.slot.clone();
        let name = plugin.manifest.name.clone();
        let instance = (plugin.create)(config.clone());
        let entry = PluginInstance {
            manifest: plugin.manifest,
            instance,
            config,
        };
        self.by_slot.entry(slot).or_default().insert(name, entry);
    }

    pub fn get(&self, slot: &PluginSlot, name: &str) -> Option<&PluginInstance> {
        self.by_slot.get(slot)?.get(name)
    }

    pub fn list(&self, slot: &PluginSlot) -> Vec<String> {
        self.by_slot
            .get(slot)
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    }
}
