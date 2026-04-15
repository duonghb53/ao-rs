//! `ao-rs watch` — lifecycle loop with event stream to stdout.

use std::sync::Arc;
use std::time::Duration;

use ao_core::{
    paths, Agent, AoConfig, LoadedConfig, LifecycleManager, LockError, PidFile, ReactionEngine, Scm,
    SessionManager,
};

use crate::cli::auto_scm::AutoScm;
use crate::cli::lifecycle_wiring::notifier_registry_from_config;
use crate::cli::plugins::{select_runtime, MultiAgent};
use crate::cli::printing::{print_config_warnings, print_event};

/// Run the lifecycle loop and pretty-print events as they arrive.
///
/// Wires up real plugins (tmux runtime, claude-code agent) and subscribes
/// to the broadcast channel. Exits cleanly on Ctrl-C or when the channel
/// is closed.
///
/// Phase D added the pidfile guard: we grab `~/.ao-rs/lifecycle.pid`
/// before starting the loop so a second `ao-rs watch` can detect it and
/// back off instead of double-polling every session. The `PidFile` is an
/// RAII handle — it removes itself when this function returns, even on
/// early error.
pub async fn watch(interval: Duration) -> Result<(), Box<dyn std::error::Error>> {
    // Acquire the singleton lock before touching any plugins so a rejected
    // second watcher exits before spawning tmux/claude probes.
    let pid_path = paths::lifecycle_pid_file();
    let _lock = match PidFile::acquire(&pid_path) {
        Ok(lock) => lock,
        Err(LockError::HeldBy { pid, path }) => {
            eprintln!(
                "ao-rs watch is already running (pid {pid}, lock {}).",
                path.display()
            );
            eprintln!("stop the running watcher first, or delete the lock if it's stale.");
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
    println!("→ acquired lifecycle lock at {}", pid_path.display());

    let sessions = Arc::new(SessionManager::with_default());
    let agent: Arc<dyn Agent> = Arc::new(MultiAgent);
    let scm: Arc<dyn Scm> = Arc::new(AutoScm::new());

    // Load config from the local project directory (ao-rs.yaml).
    // Missing config is silently empty; a broken YAML is a loud error.
    let config_path = AoConfig::local_path();
    let LoadedConfig { config, warnings } =
        AoConfig::load_from_or_default_with_warnings(&config_path)
            .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;
    print_config_warnings(&config_path, &warnings);
    if !config.reactions.is_empty() {
        println!(
            "→ loaded {} reaction(s) from {}",
            config.reactions.len(),
            config_path.display()
        );
    }
    let runtime_name = config
        .defaults
        .as_ref()
        .map(|d| d.runtime.as_str())
        .unwrap_or("tmux")
        .to_string();
    let runtime = select_runtime(&runtime_name);

    // Build lifecycle first so we can hand its broadcast channel to the
    // engine — engine events share the lifecycle channel so subscribers
    // see `ReactionTriggered` interleaved with `StatusChanged` etc.
    let lifecycle_builder = LifecycleManager::new(sessions.clone(), runtime.clone(), agent.clone())
        .with_poll_interval(interval);
    let events_tx = lifecycle_builder.events_sender();

    let notifier_registry = notifier_registry_from_config(&config);

    // Phase F wires SCM into both engines. `LifecycleManager` uses it to
    // drive PR-driven status transitions; `ReactionEngine` uses it to
    // re-probe + actually merge on `approved-and-green`. Same
    // `Arc<dyn Scm>` shared by both so we only pay for one plugin
    // instance.
    let engine = Arc::new(
        ReactionEngine::new(config.reactions, runtime.clone(), events_tx)
            .with_scm(scm.clone())
            .with_notifier_registry(notifier_registry),
    );

    let lifecycle = Arc::new(
        lifecycle_builder
            .with_reaction_engine(engine)
            .with_scm(scm.clone()),
    );

    let mut events = lifecycle.subscribe();
    let handle = lifecycle.spawn();

    println!(
        "→ watching sessions from {} (poll every {}s). ctrl-c to stop.",
        sessions.base_dir().display(),
        interval.as_secs(),
    );
    println!("{:<10} {:<20} DETAIL", "SESSION", "EVENT");

    // Shutdown path: forward ctrl-c to the lifecycle handle, then break the
    // recv loop.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                println!();
                println!("→ shutdown requested, stopping lifecycle loop...");
                break;
            }
            recv = events.recv() => {
                match recv {
                    Ok(event) => print_event(&event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("(warn) watcher lagged, dropped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    handle.stop().await;
    println!("→ stopped.");
    Ok(())
}
