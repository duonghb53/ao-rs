//! `ao-rs doctor` — environment checks.

use ao_core::{
    paths, AoConfig, EventPriority, LoadedConfig, NotificationPayload, NotifierRegistry,
    ReactionAction, SessionId, SessionManager,
};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cli::lifecycle_wiring::notifier_registry_from_config;
use crate::cli::printing::print_config_warnings;

pub async fn doctor(fix: bool, test_notify: bool) -> Result<(), Box<dyn std::error::Error>> {
    println!("ao-rs doctor");
    println!("────────────────────────────────────────");

    let mut failures = 0u32;

    // 1. Required CLI tools on PATH.
    for tool in ["git", "gh", "tmux", "claude"] {
        let status = which(tool).await;
        match status {
            ToolStatus::Found(path) => println!("  PASS  {tool:<10} {path}"),
            ToolStatus::NotFound => {
                println!("  FAIL  {tool:<10} not found on PATH");
                failures += 1;
            }
        }
    }

    // 2. gh auth status — verify GitHub authentication.
    let gh_auth = tokio::process::Command::new("gh")
        .args(["auth", "status"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    match gh_auth {
        Ok(s) if s.success() => println!("  PASS  {:<10} authenticated", "gh auth"),
        Ok(_) => {
            println!(
                "  FAIL  {:<10} not authenticated (run `gh auth login`)",
                "gh auth"
            );
            failures += 1;
        }
        Err(_) => {
            println!("  WARN  {:<10} could not run `gh auth status`", "gh auth");
        }
    }

    // 3. GitHub API rate-limit status. `gh api rate_limit` does NOT
    // consume quota itself, so it's always safe to call.
    match check_rate_limit().await {
        Ok(Some(status)) => {
            print_rate_limit_line("rate(REST)", &status.core);
            print_rate_limit_line("rate(GQL)", &status.graphql);
            if status.core.is_failure() || status.graphql.is_failure() {
                failures += 1;
            }
        }
        Ok(None) => {
            println!(
                "  WARN  {:<10} `gh api rate_limit` returned no data",
                "rate"
            );
        }
        Err(e) => {
            println!("  WARN  {:<10} could not query rate limit: {e}", "rate");
        }
    }

    // 4. Config file loads without error.
    let config_path = AoConfig::local_path();
    match AoConfig::load_from_or_default_with_warnings(&config_path) {
        Ok(LoadedConfig {
            config: cfg,
            warnings,
        }) => {
            let projects = cfg.projects.len();
            let reactions = cfg.reactions.len();
            if config_path.exists() {
                println!(
                    "  PASS  {:<10} {} ({projects} project(s), {reactions} reaction(s))",
                    "config",
                    config_path.display()
                );
                print_config_warnings(&config_path, &warnings);
            } else {
                println!(
                    "  WARN  {:<10} no config file (run `ao-rs start`)",
                    "config"
                );
            }
        }
        Err(e) => {
            println!("  FAIL  {:<10} {} — {e}", "config", config_path.display());
            failures += 1;
        }
    }

    // 5. Sessions directory exists.
    let sessions_dir = paths::default_sessions_dir();
    if sessions_dir.is_dir() {
        let count = SessionManager::with_default()
            .list()
            .await
            .map(|s| s.len())
            .unwrap_or(0);
        println!(
            "  PASS  {:<10} {} ({count} session(s))",
            "sessions",
            sessions_dir.display()
        );
    } else {
        println!(
            "  WARN  {:<10} {} does not exist yet",
            "sessions",
            sessions_dir.display()
        );
    }

    // 6. --fix: remediate common issues.
    if fix {
        println!();
        println!("Fixes:");
        failures += apply_fixes(&paths::data_dir(), &paths::default_sessions_dir(), &config_path);
    }

    // 7. --test-notify: route a synthetic payload through each priority.
    if test_notify {
        println!();
        println!("Test notifications:");
        match AoConfig::load_from_or_default_with_warnings(&config_path) {
            Ok(LoadedConfig { config: cfg, .. }) => {
                let registry = notifier_registry_from_config(&cfg);
                failures += run_test_notify(&registry).await;
            }
            Err(e) => {
                println!("  FAIL  {:<10} cannot load config: {e}", "test-notify");
                failures += 1;
            }
        }
    }

    println!("────────────────────────────────────────");
    if failures > 0 {
        println!("  {failures} check(s) FAILED");
        std::process::exit(1);
    } else {
        println!("  all checks passed");
    }

    Ok(())
}

/// Create missing `~/.ao-rs` directories and suggest `ao-rs start` when the
/// config is absent. Returns the number of fixes that failed (to fold into
/// the doctor failure counter). Idempotent.
fn apply_fixes(data_dir: &Path, sessions_dir: &Path, config_path: &Path) -> u32 {
    let mut failures = 0u32;

    for (label, dir) in [("data-dir", data_dir), ("sessions", sessions_dir)] {
        if dir.is_dir() {
            println!("  PASS  {label:<10} {} already exists", dir.display());
            continue;
        }
        match std::fs::create_dir_all(dir) {
            Ok(()) => println!("  FIX   {label:<10} created {}", dir.display()),
            Err(e) => {
                println!("  FAIL  {label:<10} could not create {}: {e}", dir.display());
                failures += 1;
            }
        }
    }

    if config_path.exists() {
        println!(
            "  PASS  {:<10} {} already exists",
            "config",
            config_path.display()
        );
    } else {
        println!(
            "  FIX   {:<10} no config at {} — run `ao-rs start` to generate one",
            "config",
            config_path.display()
        );
    }

    failures
}

/// Route a synthetic `NotificationPayload` through the registry for each
/// priority. Returns the number of send errors. Prints one PASS/FAIL per
/// `(priority, notifier)` pair.
async fn run_test_notify(registry: &NotifierRegistry) -> u32 {
    let mut failures = 0u32;

    for priority in [
        EventPriority::Urgent,
        EventPriority::Action,
        EventPriority::Warning,
        EventPriority::Info,
    ] {
        let targets = registry.resolve(priority);
        if targets.is_empty() {
            println!(
                "  WARN  {:<10} no notifiers routed for {}",
                "test-notify",
                priority.as_str()
            );
            continue;
        }

        let payload = NotificationPayload {
            session_id: SessionId("doctor-test".into()),
            reaction_key: "doctor-test".into(),
            action: ReactionAction::Notify,
            priority,
            title: format!("ao-rs doctor test ({})", priority.as_str()),
            body: "Test notification from `ao-rs doctor --test-notify`".into(),
            escalated: false,
        };

        for (name, notifier) in targets {
            match notifier.send(&payload).await {
                Ok(()) => println!(
                    "  PASS  {:<10} {} -> {name}",
                    "test-notify",
                    priority.as_str()
                ),
                Err(e) => {
                    println!(
                        "  FAIL  {:<10} {} -> {name}: {e}",
                        "test-notify",
                        priority.as_str()
                    );
                    failures += 1;
                }
            }
        }
    }

    failures
}

/// Check if a tool is on PATH.
pub(crate) enum ToolStatus {
    Found(String),
    NotFound,
}

pub(crate) async fn which(tool: &str) -> ToolStatus {
    let output = tokio::process::Command::new("which")
        .arg(tool)
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            ToolStatus::Found(path)
        }
        _ => ToolStatus::NotFound,
    }
}

// ---------------------------------------------------------------------------
// Rate-limit visibility
// ---------------------------------------------------------------------------

/// Single resource's rate-limit snapshot (REST core or GraphQL).
#[derive(Debug, Clone)]
pub(crate) struct ResourceLimit {
    pub remaining: u64,
    pub limit: u64,
    /// Unix timestamp (seconds) when the quota resets.
    pub reset: u64,
}

impl ResourceLimit {
    /// Seconds until quota resets, saturating at 0 for reset times in the past.
    pub(crate) fn reset_in(&self) -> Duration {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Duration::from_secs(self.reset.saturating_sub(now))
    }

    fn ratio(&self) -> f32 {
        if self.limit == 0 {
            return 1.0;
        }
        self.remaining as f32 / self.limit as f32
    }

    pub(crate) fn is_warning(&self) -> bool {
        (0.05..0.20).contains(&self.ratio())
    }

    pub(crate) fn is_failure(&self) -> bool {
        self.ratio() < 0.05
    }
}

/// Combined REST + GraphQL rate-limit snapshot.
#[derive(Debug, Clone)]
pub(crate) struct RateLimitStatus {
    pub core: ResourceLimit,
    pub graphql: ResourceLimit,
}

/// Query `gh api rate_limit` and parse the response.
///
/// Returns `Ok(None)` if the response is well-formed but missing the
/// expected fields (defensive — shouldn't happen on GitHub.com).
pub(crate) async fn check_rate_limit() -> Result<Option<RateLimitStatus>, String> {
    let out = tokio::process::Command::new("gh")
        .args(["api", "rate_limit"])
        .output()
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("invalid JSON from gh api rate_limit: {e}"))?;
    let resources = json.get("resources").ok_or("missing resources field")?;
    let core = parse_resource(resources.get("core"));
    let graphql = parse_resource(resources.get("graphql"));
    match (core, graphql) {
        (Some(core), Some(graphql)) => Ok(Some(RateLimitStatus { core, graphql })),
        _ => Ok(None),
    }
}

fn parse_resource(v: Option<&serde_json::Value>) -> Option<ResourceLimit> {
    let v = v?;
    Some(ResourceLimit {
        remaining: v.get("remaining")?.as_u64()?,
        limit: v.get("limit")?.as_u64()?,
        reset: v.get("reset")?.as_u64()?,
    })
}

fn print_rate_limit_line(label: &str, r: &ResourceLimit) {
    let verdict = if r.is_failure() {
        "FAIL"
    } else if r.is_warning() {
        "WARN"
    } else {
        "PASS"
    };
    let reset_min = r.reset_in().as_secs() / 60;
    println!(
        "  {verdict}  {label:<10} {}/{} (resets in {}m)",
        r.remaining, r.limit, reset_min
    );
}

/// If the user's GitHub quota is critically low, print a warning and
/// preemptively engage the shared cooldown so the lifecycle loop skips
/// `gh` calls until the quota resets.
///
/// Called from `watch` and `dashboard` before starting `LifecycleManager`.
/// Silent and non-fatal on any error — the loop still starts either way.
pub(crate) async fn preemptive_rate_limit_guard() {
    let Ok(Some(status)) = check_rate_limit().await else {
        return;
    };
    for (label, resource) in [("REST", &status.core), ("GraphQL", &status.graphql)] {
        if resource.is_failure() {
            let reset_in = resource.reset_in();
            let mins = reset_in.as_secs() / 60;
            eprintln!(
                "⚠ GitHub {label} rate limit low: {}/{} remaining (resets in {mins}m) — entering cooldown until reset.",
                resource.remaining, resource.limit,
            );
            // Add a small slack so we don't start polling the instant
            // the quota resets and re-trigger secondary limits.
            let cooldown = reset_in.saturating_add(Duration::from_secs(10));
            ao_core::rate_limit::enter_cooldown_for(cooldown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rl(remaining: u64, limit: u64) -> ResourceLimit {
        ResourceLimit {
            remaining,
            limit,
            reset: 0,
        }
    }

    #[test]
    fn thresholds_pass_warn_fail() {
        // ≥20% remaining → PASS
        let pass = rl(1000, 5000);
        assert!(!pass.is_failure());
        assert!(!pass.is_warning());

        // 5–20% remaining → WARN
        let warn = rl(500, 5000);
        assert!(!warn.is_failure());
        assert!(warn.is_warning());

        // <5% remaining → FAIL
        let fail = rl(100, 5000);
        assert!(fail.is_failure());
        assert!(!fail.is_warning());
    }

    #[test]
    fn zero_limit_does_not_panic_and_is_pass() {
        // Defensive: division-by-zero would be bad; ratio() returns 1.0.
        let zero = rl(0, 0);
        assert!(!zero.is_failure());
        assert!(!zero.is_warning());
    }

    #[test]
    fn reset_in_clamps_to_zero_for_past_reset() {
        // A reset timestamp far in the past saturates to zero, not a
        // backwards duration.
        let past = ResourceLimit {
            remaining: 1,
            limit: 5000,
            reset: 1,
        };
        assert_eq!(past.reset_in(), Duration::ZERO);
    }

    #[test]
    fn parse_resource_handles_missing_fields() {
        let full = serde_json::json!({"remaining": 42, "limit": 100, "reset": 1700000000});
        let got = parse_resource(Some(&full)).unwrap();
        assert_eq!(got.remaining, 42);
        assert_eq!(got.limit, 100);
        assert_eq!(got.reset, 1700000000);

        // Missing any field → None, not panic.
        let missing_reset = serde_json::json!({"remaining": 1, "limit": 2});
        assert!(parse_resource(Some(&missing_reset)).is_none());
        assert!(parse_resource(None).is_none());
    }

    // ---- --fix tests ----

    #[test]
    fn apply_fixes_creates_missing_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join(".ao-rs");
        let sessions_dir = data_dir.join("sessions");
        let config_path = tmp.path().join("ao-rs.yaml");

        let failures = apply_fixes(&data_dir, &sessions_dir, &config_path);
        assert_eq!(failures, 0);
        assert!(data_dir.is_dir());
        assert!(sessions_dir.is_dir());
    }

    #[test]
    fn apply_fixes_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join(".ao-rs");
        let sessions_dir = data_dir.join("sessions");
        let config_path = tmp.path().join("ao-rs.yaml");

        assert_eq!(apply_fixes(&data_dir, &sessions_dir, &config_path), 0);
        // Second run must be a no-op with zero failures.
        assert_eq!(apply_fixes(&data_dir, &sessions_dir, &config_path), 0);
        assert!(data_dir.is_dir());
        assert!(sessions_dir.is_dir());
        assert!(!config_path.exists());
    }

    // ---- --test-notify tests ----

    use ao_core::{Notifier, NotifierError};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct CapturingNotifier {
        name: String,
        received: Arc<Mutex<Vec<NotificationPayload>>>,
    }

    impl CapturingNotifier {
        fn new(name: &str) -> (Self, Arc<Mutex<Vec<NotificationPayload>>>) {
            let received = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    name: name.into(),
                    received: Arc::clone(&received),
                },
                received,
            )
        }
    }

    #[async_trait]
    impl Notifier for CapturingNotifier {
        fn name(&self) -> &str {
            &self.name
        }
        async fn send(&self, payload: &NotificationPayload) -> Result<(), NotifierError> {
            self.received.lock().unwrap().push(payload.clone());
            Ok(())
        }
    }

    struct FailingNotifier;

    #[async_trait]
    impl Notifier for FailingNotifier {
        fn name(&self) -> &str {
            "failing"
        }
        async fn send(&self, _payload: &NotificationPayload) -> Result<(), NotifierError> {
            Err(NotifierError::Unavailable("test forced failure".into()))
        }
    }

    fn routing_for_all_priorities(names: Vec<String>) -> ao_core::NotificationRouting {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        for p in [
            EventPriority::Urgent,
            EventPriority::Action,
            EventPriority::Warning,
            EventPriority::Info,
        ] {
            map.insert(p, names.clone());
        }
        ao_core::NotificationRouting::from_map(map)
    }

    #[tokio::test]
    async fn test_notify_dispatches_payload_for_each_priority() {
        let (notifier, received) = CapturingNotifier::new("capture");
        let mut registry = NotifierRegistry::new(routing_for_all_priorities(vec![
            "capture".to_string()
        ]));
        registry.register("capture", Arc::new(notifier));

        let failures = run_test_notify(&registry).await;
        assert_eq!(failures, 0);

        let got = received.lock().unwrap();
        assert_eq!(got.len(), 4, "one payload per priority");
        let priorities: Vec<EventPriority> = got.iter().map(|p| p.priority).collect();
        assert_eq!(
            priorities,
            vec![
                EventPriority::Urgent,
                EventPriority::Action,
                EventPriority::Warning,
                EventPriority::Info,
            ]
        );
        for payload in got.iter() {
            assert_eq!(payload.reaction_key, "doctor-test");
            assert_eq!(payload.session_id.0, "doctor-test");
            assert!(payload.title.contains(payload.priority.as_str()));
        }
    }

    #[tokio::test]
    async fn test_notify_reports_empty_routing_as_warn_not_fail() {
        // Empty registry → resolve returns empty → warn printed, zero failures.
        let registry = NotifierRegistry::new(ao_core::NotificationRouting::default());
        let failures = run_test_notify(&registry).await;
        assert_eq!(failures, 0);
    }

    #[tokio::test]
    async fn test_notify_counts_send_errors_as_failures() {
        let mut registry = NotifierRegistry::new(routing_for_all_priorities(vec![
            "failing".to_string()
        ]));
        registry.register("failing", Arc::new(FailingNotifier));

        let failures = run_test_notify(&registry).await;
        // One failing send per priority × 4 priorities.
        assert_eq!(failures, 4);
    }
}
