//! TS observability helpers (ported from `packages/core/src/observability.ts`).
//!
//! Parity status: test-only.
//!
//! No runtime consumer. Depends on `parity_metadata::atomic_write_file` for
//! snapshot persistence during tests. See
//! `docs/ts-core-parity-report.md` → "Parity-only modules".

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type ObservabilityLevel = &'static str; // "debug" | "info" | "warn" | "error"
pub type ObservabilityOutcome = &'static str; // "success" | "failure"
pub type ObservabilityHealthStatus = &'static str; // "ok" | "warn" | "error"

#[derive(Debug, Clone)]
pub struct TsObservabilityConfig {
    pub config_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObservabilityMetricCounter {
    pub total: u64,
    pub success: u64,
    pub failure: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityTraceRecord {
    pub operation: String,
    pub outcome: String,
    pub correlation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilitySessionStatus {
    pub session_id: String,
    pub correlation_id: String,
    pub operation: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityHealthSurface {
    pub surface: String,
    pub status: String,
    pub component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProcessSnapshot {
    pub component: String,
    pub metrics: HashMap<String, ObservabilityMetricCounter>,
    pub traces: Vec<ObservabilityTraceRecord>,
    pub sessions: HashMap<String, ObservabilitySessionStatus>,
    pub health: HashMap<String, ObservabilityHealthSurface>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObservabilityProjectSnapshot {
    pub metrics: HashMap<String, ObservabilityMetricCounter>,
    pub recent_traces: Vec<ObservabilityTraceRecord>,
    pub sessions: HashMap<String, ObservabilitySessionStatus>,
    pub health: HashMap<String, ObservabilityHealthSurface>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObservabilitySummary {
    pub overall_status: String,
    pub projects: HashMap<String, ObservabilityProjectSnapshot>,
}

pub struct RecordOperationInput {
    pub metric: String,
    pub operation: String,
    pub outcome: String,
    pub correlation_id: String,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub reason: Option<String>,
    pub level: Option<ObservabilityLevel>,
}

pub struct SetHealthInput {
    pub surface: String,
    pub status: String,
    pub project_id: Option<String>,
    pub correlation_id: Option<String>,
    pub reason: Option<String>,
}

pub struct ProjectObserver {
    config: TsObservabilityConfig,
    component: String,
}

pub fn create_project_observer(config: TsObservabilityConfig, component: &str) -> ProjectObserver {
    ProjectObserver {
        config,
        component: component.to_string(),
    }
}

impl ProjectObserver {
    pub fn record_operation(&self, input: RecordOperationInput) {
        let mut snap = self.read_snapshot();
        let ctr = snap.metrics.entry(input.metric.clone()).or_default();
        ctr.total += 1;
        if input.outcome == "success" {
            ctr.success += 1;
        } else {
            ctr.failure += 1;
            ctr.last_failure_reason = input.reason.clone();
        }
        snap.traces.push(ObservabilityTraceRecord {
            operation: input.operation.clone(),
            outcome: input.outcome.clone(),
            correlation_id: input.correlation_id.clone(),
            project_id: input.project_id.clone(),
            session_id: input.session_id.clone(),
            reason: input.reason.clone(),
        });
        if let Some(session_id) = &input.session_id {
            snap.sessions.insert(
                session_id.clone(),
                ObservabilitySessionStatus {
                    session_id: session_id.clone(),
                    correlation_id: input.correlation_id,
                    operation: input.operation,
                    outcome: input.outcome,
                    reason: input.reason,
                },
            );
        }
        self.write_snapshot(&snap);
    }

    pub fn set_health(&self, input: SetHealthInput) {
        let mut snap = self.read_snapshot();
        snap.health.insert(
            input.surface.clone(),
            ObservabilityHealthSurface {
                surface: input.surface,
                status: input.status,
                component: self.component.clone(),
                reason: input.reason,
            },
        );
        self.write_snapshot(&snap);
    }

    fn snapshot_path(&self) -> PathBuf {
        let base = self
            .config
            .config_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(".ao-observability")
            .join("processes");
        let _ = std::fs::create_dir_all(&base);
        base.join(format!(
            "{}-{}.json",
            sanitize_component(&self.component),
            std::process::id()
        ))
    }

    fn read_snapshot(&self) -> ProcessSnapshot {
        let path = self.snapshot_path();
        let content = std::fs::read_to_string(&path).ok();
        if let Some(content) = content {
            serde_json::from_str(&content).unwrap_or_else(|_| ProcessSnapshot {
                component: self.component.clone(),
                ..Default::default()
            })
        } else {
            ProcessSnapshot {
                component: self.component.clone(),
                ..Default::default()
            }
        }
    }

    fn write_snapshot(&self, snap: &ProcessSnapshot) {
        let path = self.snapshot_path();
        let payload = serde_json::to_string_pretty(snap)
            .unwrap_or_else(|e| {
                tracing::warn!("observability snapshot serialization failed: {e}");
                "{}".into()
            })
            + "\n";
        let _ = crate::parity_metadata::atomic_write_file(&path, &payload);
    }
}

pub fn read_observability_summary(config: TsObservabilityConfig) -> ObservabilitySummary {
    let processes_dir = config
        .config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".ao-observability")
        .join("processes");
    let mut summary = ObservabilitySummary {
        overall_status: "ok".into(),
        ..Default::default()
    };
    let Ok(rd) = std::fs::read_dir(processes_dir) else {
        return summary;
    };
    for ent in rd.flatten() {
        let Ok(content) = std::fs::read_to_string(ent.path()) else {
            continue;
        };
        let Ok(snap) = serde_json::from_str::<ProcessSnapshot>(&content) else {
            continue;
        };
        for trace in &snap.traces {
            if let Some(project_id) = &trace.project_id {
                let p = summary.projects.entry(project_id.clone()).or_default();
                p.recent_traces.push(trace.clone());
            }
        }
        for (metric, ctr) in &snap.metrics {
            for project_id in snap
                .traces
                .iter()
                .filter_map(|t| t.project_id.clone())
                .collect::<std::collections::HashSet<_>>()
            {
                let p = summary.projects.entry(project_id).or_default();
                let c = p.metrics.entry(metric.clone()).or_default();
                c.total += ctr.total;
                c.success += ctr.success;
                c.failure += ctr.failure;
                if c.last_failure_reason.is_none() {
                    c.last_failure_reason = ctr.last_failure_reason.clone();
                }
            }
        }
        for (sid, s) in &snap.sessions {
            if let Some(project_id) = snap
                .traces
                .iter()
                .rev()
                .find(|t| t.session_id.as_deref() == Some(sid))
                .and_then(|t| t.project_id.clone())
            {
                let p = summary.projects.entry(project_id).or_default();
                p.sessions.insert(sid.clone(), s.clone());
            }
        }
        for (surface, h) in &snap.health {
            let status = h.status.as_str();
            if status_priority(status) > status_priority(&summary.overall_status) {
                summary.overall_status = status.to_string();
            }
            if let Some(project_id) = snap.traces.iter().rev().find_map(|t| t.project_id.clone()) {
                let p = summary.projects.entry(project_id).or_default();
                p.health.insert(surface.clone(), h.clone());
            }
        }
    }
    for p in summary.projects.values() {
        for h in p.health.values() {
            if status_priority(&h.status) > status_priority(&summary.overall_status) {
                summary.overall_status = h.status.clone();
            }
        }
    }
    summary
}

fn status_priority(s: &str) -> u8 {
    match s {
        "error" => 3,
        "warn" => 2,
        _ => 1,
    }
}

fn sanitize_component(component: &str) -> String {
    let cleaned = component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    cleaned.trim_matches('-').to_string()
}
