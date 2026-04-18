//! `ao-rs dashboard` — HTTP API + lifecycle loop.

use std::sync::Arc;
use std::time::Duration;

use ao_core::{
    paths, Agent, AoConfig, LifecycleManager, LoadedConfig, LockError, OrchestratorEvent, PidFile,
    ReactionEngine, Scm, SessionManager, Workspace,
};
use ao_plugin_workspace_worktree::WorktreeWorkspace;
use tokio::sync::broadcast;

use crate::cli::auto_scm::AutoScm;
use crate::cli::lifecycle_wiring::notifier_registry_from_config;
use crate::cli::plugins::{select_runtime, MultiAgent};
use crate::cli::printing::print_config_warnings;
use crate::commands::doctor::preemptive_rate_limit_guard;
use crate::commands::stop::stop as stop_lifecycle;

fn build_dashboard_state() -> Result<ao_dashboard::state::AppState, Box<dyn std::error::Error>> {
    let sessions = Arc::new(SessionManager::with_default());
    let agent: Arc<dyn Agent> = Arc::new(MultiAgent);
    let scm: Arc<dyn Scm> = Arc::new(AutoScm::new());
    let workspace: Arc<dyn Workspace> = Arc::new(WorktreeWorkspace::new());

    // Dashboard handlers expect a broadcast sender even if no lifecycle loop is running.
    // In "HTTP-only" mode this sender will never publish any events.
    let (events_tx, _events_rx) = broadcast::channel::<OrchestratorEvent>(256);

    let config_path = AoConfig::local_path();
    let LoadedConfig { config, warnings } =
        AoConfig::load_from_or_default_with_warnings(&config_path)
            .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;
    print_config_warnings(&config_path, &warnings);

    let runtime_name = config
        .defaults
        .as_ref()
        .map(|d| d.runtime.as_str())
        .unwrap_or("tmux")
        .to_string();
    let runtime = select_runtime(&runtime_name);

    Ok(ao_dashboard::state::AppState {
        sessions,
        events_tx,
        runtime,
        scm,
        agent,
        workspace,
        config_path: Some(config_path),
    })
}

/// Run just the dashboard HTTP server (no lifecycle loop).
pub async fn dashboard_only(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let dashboard_state = build_dashboard_state()?;

    println!(
        "→ dashboard API on http://127.0.0.1:{port}/ (no orchestrator)"
    );
    println!("  UI: cd crates/ao-desktop/ui && npm run dev  →  http://localhost:5173");

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    tokio::select! {
        _ = &mut ctrl_c => {
            println!();
            println!("→ shutdown requested");
        }
        result = ao_dashboard::run_server(dashboard_state, port) => {
            if let Err(e) = result {
                eprintln!("dashboard server error: {e}");
            }
        }
    }

    println!("→ stopped.");
    Ok(())
}

/// Run the dashboard API server alongside the lifecycle loop.
///
/// Reuses the same plugin wiring as `watch` and adds an axum HTTP server.
/// Both run concurrently under `tokio::select!` so Ctrl-C stops them
/// together.
///
/// When `rebuild` is true, any previously-running lifecycle instance
/// (`watch` / `dashboard`) is stopped first, and a stale
/// `~/.ao-rs/lifecycle.pid` is purged, so the new dashboard starts
/// from a clean lock state.
pub async fn dashboard(
    port: u16,
    interval_override: Option<Duration>,
    rebuild: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if rebuild {
        println!("→ rebuild: clearing stale lifecycle state...");
        stop_lifecycle(false, false).await?;
    }

    let pid_path = paths::lifecycle_pid_file();
    let _lock = match PidFile::acquire(&pid_path) {
        Ok(lock) => lock,
        Err(LockError::HeldBy { pid, path }) => {
            eprintln!(
                "ao-rs is already running (pid {pid}, lock {}).",
                path.display()
            );
            return Err(format!("lifecycle lock held by pid {pid}").into());
        }
        Err(LockError::Io(e)) => {
            return Err(format!(
                "failed to take lifecycle lock at {}: {e}",
                pid_path.display()
            )
            .into());
        }
    };

    // Preemptively check GitHub quota — if it's nearly exhausted, enter
    // cooldown before polling starts so the loop doesn't immediately burn
    // the last calls and trip a secondary-rate-limit penalty.
    preemptive_rate_limit_guard().await;

    let sessions = Arc::new(SessionManager::with_default());
    let agent: Arc<dyn Agent> = Arc::new(MultiAgent);
    let scm: Arc<dyn Scm> = Arc::new(AutoScm::new());

    let config_path = AoConfig::local_path();
    let LoadedConfig { config, warnings } =
        AoConfig::load_from_or_default_with_warnings(&config_path)
            .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;
    print_config_warnings(&config_path, &warnings);
    let config = Arc::new(config);
    let interval = interval_override.unwrap_or_else(|| Duration::from_secs(config.poll_interval));

    let runtime_name = config
        .defaults
        .as_ref()
        .map(|d| d.runtime.as_str())
        .unwrap_or("tmux")
        .to_string();
    let runtime = select_runtime(&runtime_name);

    let lifecycle_builder = LifecycleManager::new(sessions.clone(), runtime.clone(), agent.clone())
        .with_poll_interval(interval);
    let events_tx = lifecycle_builder.events_sender();

    let notifier_registry = notifier_registry_from_config(config.as_ref());

    let engine = Arc::new(
        ReactionEngine::new_with_config(Arc::clone(&config), runtime.clone(), events_tx.clone())
            .with_scm(scm.clone())
            .with_notifier_registry(notifier_registry),
    );

    let workspace: Arc<dyn Workspace> = Arc::new(WorktreeWorkspace::new());
    let lifecycle = Arc::new(
        lifecycle_builder
            .with_reaction_engine(engine)
            .with_scm(scm.clone())
            .with_workspace(workspace.clone()),
    );
    let lifecycle_handle = lifecycle.spawn();

    // Build dashboard state and start the HTTP server.
    let dashboard_state = ao_dashboard::state::AppState {
        sessions,
        events_tx,
        runtime,
        scm,
        agent,
        workspace,
        config_path: Some(config_path),
    };

    println!(
        "→ dashboard API on http://127.0.0.1:{port}/ (poll every {}s)",
        interval.as_secs()
    );
    println!("  UI: cd crates/ao-desktop/ui && npm run dev  →  http://localhost:5173");

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    tokio::select! {
        _ = &mut ctrl_c => {
            println!();
            println!("→ shutdown requested");
        }
        result = ao_dashboard::run_server(dashboard_state, port) => {
            if let Err(e) = result {
                eprintln!("dashboard server error: {e}");
            }
        }
    }

    lifecycle_handle.stop().await;
    println!("→ stopped.");
    Ok(())
}
