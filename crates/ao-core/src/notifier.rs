//! Notifier plugin contract + registry — Slice 3 Phase A (data only).
//!
//! Slice 3 turns `ReactionAction::Notify` from "emit a `ReactionTriggered`
//! event and hope a subscriber is listening" into real fan-out to
//! configurable channels (stdout, ntfy, desktop, slack, …).
//!
//! ## Phase split
//!
//! - **Phase A (this module)** — `Notifier` trait, `NotificationPayload`,
//!   `NotifierError`, `NotificationRouting` config type, `NotifierRegistry`.
//!   No engine integration, no plugin crates. The types land first so
//!   they can be reviewed before anything calls them.
//! - **Phase B** — `ReactionEngine::dispatch_notify` resolves a priority
//!   through the registry and calls `Notifier::send` on each target,
//!   aggregating results into `ReactionOutcome`. Uses the test-only
//!   `TestNotifier` below for coverage — still no plugin crates.
//! - **Phase C** — first real plugin crate `ao-plugin-notifier-stdout`,
//!   wired in `ao-cli` with a default-to-stdout policy when the routing
//!   table is empty.
//! - **Phase D+** — additional plugin crates (ntfy, desktop, slack, …).
//!
//! See `docs/ai/design/feature-notifier-routing.md` for the full Slice 3
//! arc and the rationale for each design choice.
//!
//! ## Why data-only for Phase A
//!
//! Landing the trait, payload, error, routing config, and registry as
//! one focused commit gives reviewers a stable contract to evaluate
//! before any call sites depend on it. Mirrors the Phase A commit for
//! Slice 2 (reaction config types only) that preceded the engine wiring
//! in Phase D.
//!
//! Mirrors the `Notifier` / `NotificationPayload` / `notificationRouting`
//! types in `packages/core/src/types.ts` (TS reference).

use crate::{
    reactions::{EventPriority, ReactionAction},
    types::SessionId,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

// ---------------------------------------------------------------------------
// NotificationPayload
// ---------------------------------------------------------------------------

/// Data handed to every `Notifier::send` call.
///
/// Constructed by `ReactionEngine::dispatch_notify` at Phase B and
/// later. Phase A only defines the shape so plugins can be written
/// against a stable target.
///
/// Not `Serialize` — the payload lives entirely in-process, never hits
/// disk, and never rides the event bus (the bus carries narrow
/// `OrchestratorEvent` variants for fan-out, not rich payloads).
/// Keeping it off serde means plugins are free to embed non-serde
/// types (handles, closures, Instants) later without breaking the
/// type boundary.
#[derive(Debug, Clone)]
pub struct NotificationPayload {
    /// Session the notification is about.
    pub session_id: SessionId,
    /// Reaction key that fired (e.g. `"ci-failed"`).
    pub reaction_key: String,
    /// Action the engine actually took — always `Notify` at the call
    /// site, but carried for plugins that want to log/format it.
    pub action: ReactionAction,
    /// Priority chosen by the engine for this fire. Decides routing.
    pub priority: EventPriority,
    /// One-line title. Synthesized by the engine from `reaction_key +
    /// session` in Phase B.
    pub title: String,
    /// Body text. Pulled from `ReactionConfig.message` when set,
    /// otherwise engine-supplied boilerplate.
    pub body: String,
    /// `true` if this notify is the escalation fallback after retries
    /// were exhausted (engine swapped `SendToAgent` → `Notify`).
    /// Plugins that want to badge "escalated" branch on this.
    pub escalated: bool,
}

// ---------------------------------------------------------------------------
// NotifierError
// ---------------------------------------------------------------------------

/// Plugin-returned error type.
///
/// Every variant is treated identically by the engine: logged via
/// `tracing::warn!`, recorded in `ReactionOutcome { success: false, .. }`,
/// and never propagated up to the polling loop. A flaky notifier must
/// not wedge the tick — matches the "never poison the engine" principle
/// used for malformed durations in Slice 2 Phase H.
///
/// The variant split exists so plugin authors have a reasonable place
/// to put their own errors without inventing a new enum per plugin.
/// HTTP plugins lean on `Service` + `Timeout`; desktop plugins lean on
/// `Unavailable`; anything that failed before the wire lean on `Config`
/// or `Io`.
#[derive(Debug, thiserror::Error)]
pub enum NotifierError {
    /// Local I/O failed — filesystem, stdout, named pipe, etc.
    #[error("notifier I/O failure: {0}")]
    Io(String),
    /// Plugin configuration is invalid or incomplete (missing token,
    /// unparseable URL, …).
    #[error("notifier configuration error: {0}")]
    Config(String),
    /// External service returned a non-success status.
    #[error("notifier external service error: {status}: {message}")]
    Service { status: u16, message: String },
    /// Plugin exceeded its own timeout budget before the service
    /// responded.
    #[error("notifier timed out after {elapsed_ms}ms")]
    Timeout { elapsed_ms: u64 },
    /// External service or local dependency is unreachable right now
    /// (connection refused, DNS failure, desktop daemon missing).
    #[error("notifier unavailable: {0}")]
    Unavailable(String),
}

// ---------------------------------------------------------------------------
// Notifier trait
// ---------------------------------------------------------------------------

/// Plugin contract for delivering notifications.
///
/// One method + one associated function. Plugins live in their own
/// crates under `ao-plugin-notifier-*` starting in Phase C; the first
/// real plugin is stdout.
///
/// ## Implementor responsibilities
///
/// - **Never panic.** Return a `NotifierError` variant instead. The
///   engine traps errors but panics would tear down the polling task.
/// - **Respect a bounded timeout.** HTTP plugins should default to 5s
///   and map overruns to `NotifierError::Timeout`. The trait signature
///   does not enforce this; it's a hard convention.
/// - **Don't hold locks across `.await`.** The engine calls `send`
///   inline during a poll tick and a deadlocked plugin would wedge the
///   whole loop.
/// - **Keep `send` side-effect-only.** Payload mutation is out of
///   scope — plugins receive `&NotificationPayload` precisely so they
///   can't rewrite history for downstream plugins in the same fan-out.
///
/// ## Concurrency
///
/// Implementors must be `Send + Sync` because the registry stores
/// `Arc<dyn Notifier>` and the engine runs inside a `tokio::spawn`
/// task. Matches the rest of the `ao-core` plugin traits.
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Canonical name used in the `notification-routing` table.
    /// Conventionally kebab-case (`"stdout"`, `"ntfy"`, `"slack"`).
    /// Must be stable across the plugin's lifetime.
    fn name(&self) -> &str;

    /// Deliver one notification.
    ///
    /// Returning `Err` does not crash the engine — the engine logs via
    /// `tracing::warn!`, marks the `ReactionOutcome` as `success =
    /// false`, and proceeds to the next plugin in the fan-out.
    async fn send(&self, payload: &NotificationPayload) -> Result<(), NotifierError>;
}

// ---------------------------------------------------------------------------
// NotificationRouting
// ---------------------------------------------------------------------------

/// Priority-based routing table read from the `notification-routing:`
/// section of `~/.ao-rs/config.yaml`.
///
/// On-disk YAML:
///
/// ```yaml
/// notification-routing:
///   urgent: [stdout, ntfy]
///   action: [stdout, ntfy]
///   warning: [stdout]
///   info:    [stdout]
/// ```
///
/// Stored as a newtype around `HashMap<EventPriority, Vec<String>>`
/// with `#[serde(transparent)]` so the on-disk form is just the map —
/// no wrapper key. Hiding the inner `HashMap` behind `names_for` keeps
/// the public API stable if we later want to change the container or
/// bolt on a per-reaction-key override layer.
///
/// Default: empty map. An empty table means "nothing configured for
/// any priority" — `NotifierRegistry::resolve` warn-onces per priority
/// on the first miss and drops the notification. The fallback policy
/// (default-to-stdout when the table is empty) belongs one layer up
/// at the `ao-cli` wiring site in Phase C, not inside the config
/// type itself, so this module stays pure data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NotificationRouting(HashMap<EventPriority, Vec<String>>);

impl NotificationRouting {
    /// Return the list of notifier names registered for this priority,
    /// or `None` if the priority has no entry.
    ///
    /// An empty list (priority present but points at `[]`) is returned
    /// as `Some(&[])` — distinct from a missing entry. The registry's
    /// `resolve` method folds both cases together (warn-once + empty
    /// result) so callers don't need to branch on the difference, but
    /// they CAN if they ever want to.
    pub fn names_for(&self, priority: EventPriority) -> Option<&[String]> {
        self.0.get(&priority).map(Vec::as_slice)
    }

    /// True if the routing table has no priorities configured at all.
    /// The `ao-cli` wiring uses this in Phase C to decide whether to
    /// apply the "default to stdout for everything" fallback.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of priorities that have at least one entry.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Construct a routing table from a pre-built map. Used by
    /// `ao-cli` to build the default-to-stdout routing when the user's
    /// config has no `notification-routing:` section, and by unit tests
    /// that want an inline table without going through serde.
    pub fn from_map(map: HashMap<EventPriority, Vec<String>>) -> Self {
        Self(map)
    }
}

// ---------------------------------------------------------------------------
// NotifierRegistry
// ---------------------------------------------------------------------------

/// Runtime-side registry of notifier plugins keyed by name, plus the
/// routing table that decides which plugins receive each priority.
///
/// Constructed in `ao-cli` (Phase C) after plugin instantiation,
/// attached to `ReactionEngine` via `with_notifier_registry` (Phase B).
/// Existing call sites that don't attach one keep working — identical
/// opt-in pattern to `ReactionEngine::with_scm`.
///
/// ## Warn-once policy
///
/// `resolve` logs exactly one `tracing::warn!` per distinct
/// `(priority, notifier_name)` pair across the process lifetime, so a
/// typo in the routing table can't spam the log on every poll tick.
/// Matches the dedup pattern used by
/// `reaction_engine::warn_once_parse_failure` for malformed durations.
pub struct NotifierRegistry {
    plugins: HashMap<String, Arc<dyn Notifier>>,
    routing: NotificationRouting,
    /// Dedup set for `resolve`'s warn-once emits. Keys are one of:
    /// - `"priority.{priority}"` for missing or empty priority entries
    /// - `"{priority}.{notifier_name}"` for names with no registered
    ///   matching plugin
    ///
    /// `Mutex` (not `RwLock`) because the set is write-mostly: every
    /// miss either inserts a new key or short-circuits on an existing
    /// one. Lock is held narrowly — acquire, check-and-insert, drop,
    /// *then* call `tracing::warn!`.
    warned: Mutex<HashSet<String>>,
}

impl NotifierRegistry {
    /// Construct an empty registry with the given routing table. Plugins
    /// are added via `register`.
    pub fn new(routing: NotificationRouting) -> Self {
        Self {
            plugins: HashMap::new(),
            routing,
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// Register a plugin under a name. Overwrites any existing entry
    /// for the same name — tests rely on this to stub plugins with
    /// replacements. Production wiring in `ao-cli` registers each
    /// plugin exactly once at startup.
    pub fn register(&mut self, name: impl Into<String>, plugin: Arc<dyn Notifier>) {
        self.plugins.insert(name.into(), plugin);
    }

    /// Look up a plugin by name without going through routing.
    /// Primarily useful for `ao-cli` smoke tests and for future phases
    /// that may want direct-addressed notifications.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Notifier>> {
        self.plugins.get(name).cloned()
    }

    /// Number of registered plugins.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// `true` if no plugins have been registered.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Resolve a priority against the routing table, returning the
    /// `(name, plugin)` pairs the engine should dispatch to.
    ///
    /// Empty return vec means "do nothing for this priority". That
    /// happens in three cases, all of which trigger a warn-once:
    ///
    /// 1. Priority missing from the routing table entirely.
    /// 2. Priority present but points at an empty list.
    /// 3. The routing table names one or more plugins that are not
    ///    registered — the registered subset (if any) is returned and
    ///    the missing names are each warned once.
    ///
    /// Case 3 can return a non-empty vec (the registered subset) even
    /// though some of the configured names were missing. That is
    /// deliberate: a partially-wired routing table should still deliver
    /// to the plugins that DO exist, not fail closed.
    pub fn resolve(&self, priority: EventPriority) -> Vec<(String, Arc<dyn Notifier>)> {
        let Some(names) = self.routing.names_for(priority) else {
            self.warn_once(format!("priority.{}", priority.as_str()), || {
                tracing::warn!(
                    priority = priority.as_str(),
                    "notification-routing has no entry for priority; notification dropped"
                );
            });
            return Vec::new();
        };

        if names.is_empty() {
            self.warn_once(format!("priority.{}", priority.as_str()), || {
                tracing::warn!(
                    priority = priority.as_str(),
                    "notification-routing has an empty list for priority; notification dropped"
                );
            });
            return Vec::new();
        }

        let mut out = Vec::with_capacity(names.len());
        for name in names {
            if let Some(plugin) = self.plugins.get(name) {
                out.push((name.clone(), plugin.clone()));
            } else {
                let key = format!("{}.{}", priority.as_str(), name);
                let missing_name = name.clone();
                self.warn_once(key, || {
                    tracing::warn!(
                        priority = priority.as_str(),
                        notifier = missing_name.as_str(),
                        "notification-routing references unregistered notifier; skipping"
                    );
                });
            }
        }
        out
    }

    /// Dedup helper. Acquires `warned` narrowly — insert, drop lock,
    /// then invoke `emit`. Matches the lock discipline used by
    /// `reaction_engine::warn_once_parse_failure` (Phase H) so a
    /// future `tracing::warn!` macro expansion that panics inside the
    /// formatter can never poison the mutex while it's held.
    fn warn_once<F: FnOnce()>(&self, key: String, emit: F) {
        let fire = {
            let mut set = self
                .warned
                .lock()
                .expect("notifier registry warned mutex poisoned");
            set.insert(key)
        };
        if fire {
            emit();
        }
    }

    /// Test-only accessor for the dedup set size. Production code
    /// must treat `warned` as opaque.
    #[cfg(test)]
    pub(crate) fn warned_count(&self) -> usize {
        self.warned
            .lock()
            .expect("notifier registry warned mutex poisoned")
            .len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Records every `send` call for inspection by tests. Lives in the
    /// `tests` module but is `pub(crate)` so Phase B's `reaction_engine`
    /// tests can import it: `use crate::notifier::tests::TestNotifier`.
    ///
    /// The inner mutex wraps a `Vec` of owned payloads. `send` is
    /// async but we never hold the lock across `.await` (we don't have
    /// an await point inside this impl at all), so `std::sync::Mutex`
    /// is fine — a `tokio::sync::Mutex` would be overkill.
    pub(crate) struct TestNotifier {
        name: String,
        received: Arc<StdMutex<Vec<NotificationPayload>>>,
    }

    impl TestNotifier {
        pub(crate) fn new(
            name: impl Into<String>,
        ) -> (Self, Arc<StdMutex<Vec<NotificationPayload>>>) {
            let received = Arc::new(StdMutex::new(Vec::new()));
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
    impl Notifier for TestNotifier {
        fn name(&self) -> &str {
            &self.name
        }

        async fn send(&self, payload: &NotificationPayload) -> Result<(), NotifierError> {
            self.received
                .lock()
                .expect("test notifier mutex poisoned")
                .push(payload.clone());
            Ok(())
        }
    }

    fn fake_payload(priority: EventPriority) -> NotificationPayload {
        NotificationPayload {
            session_id: SessionId("sess-test".into()),
            reaction_key: "ci-failed".into(),
            action: ReactionAction::Notify,
            priority,
            title: "CI broke on sess-test".into(),
            body: "tests failed on main".into(),
            escalated: false,
        }
    }

    // ---- NotificationRouting ----

    #[test]
    fn routing_default_is_empty() {
        let r = NotificationRouting::default();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.names_for(EventPriority::Urgent).is_none());
    }

    #[test]
    fn routing_yaml_round_trip() {
        let yaml = r#"
urgent: [stdout, ntfy]
action: [stdout, ntfy]
warning: [stdout]
info: [stdout]
"#;
        let parsed: NotificationRouting = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(
            parsed.names_for(EventPriority::Urgent).unwrap(),
            &["stdout".to_string(), "ntfy".to_string()]
        );
        assert_eq!(
            parsed.names_for(EventPriority::Info).unwrap(),
            &["stdout".to_string()]
        );

        // Round-trip through YAML: serialize back, re-parse, equals original.
        let back = serde_yaml::to_string(&parsed).unwrap();
        let again: NotificationRouting = serde_yaml::from_str(&back).unwrap();
        assert_eq!(parsed, again);
    }

    #[test]
    fn routing_rejects_unknown_priority_key() {
        // Strict priority matching: a typo ("critical") must fail the
        // parse, not be silently dropped. Locks in behaviour so a
        // future serde change (e.g. `#[serde(other)]`) can't flip it
        // without this test failing first.
        let yaml = "critical: [stdout]\n";
        let result: std::result::Result<NotificationRouting, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "expected parse error for unknown priority, got {result:?}"
        );
    }

    #[test]
    fn routing_preserves_empty_list_distinct_from_missing() {
        // `warning: []` is preserved as Some(&[]), NOT folded into
        // None. `resolve` folds them together for the engine, but the
        // distinction is visible at the config layer so tooling can
        // tell them apart if it ever wants to.
        let yaml = "warning: []\n";
        let parsed: NotificationRouting = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.names_for(EventPriority::Warning), Some(&[][..]));
        assert!(parsed.names_for(EventPriority::Urgent).is_none());
    }

    // ---- NotifierRegistry ----

    #[test]
    fn registry_new_is_empty() {
        let r = NotifierRegistry::new(NotificationRouting::default());
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.get("stdout").is_none());
    }

    #[test]
    fn registry_register_and_get_round_trip() {
        let (tn, _received) = TestNotifier::new("stdout");
        let mut reg = NotifierRegistry::new(NotificationRouting::default());
        reg.register("stdout", Arc::new(tn));
        assert_eq!(reg.len(), 1);
        let got = reg.get("stdout").expect("plugin should be registered");
        assert_eq!(got.name(), "stdout");
    }

    #[test]
    fn registry_register_overwrites_existing() {
        // Two plugins registered under the same name — the second
        // replaces the first. Documented behaviour so tests can
        // reliably stub plugins with replacements.
        let (first, _) = TestNotifier::new("first");
        let (second, _) = TestNotifier::new("second");
        let mut reg = NotifierRegistry::new(NotificationRouting::default());
        reg.register("slot", Arc::new(first));
        reg.register("slot", Arc::new(second));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("slot").unwrap().name(), "second");
    }

    #[test]
    fn resolve_empty_routing_returns_empty_and_warns_once() {
        // Priority missing from the table → empty vec, one warn.
        // Resolving the same priority a second time → still empty
        // vec, warn is deduped.
        let reg = NotifierRegistry::new(NotificationRouting::default());
        assert!(reg.resolve(EventPriority::Urgent).is_empty());
        assert_eq!(reg.warned_count(), 1);
        assert!(reg.resolve(EventPriority::Urgent).is_empty());
        assert_eq!(reg.warned_count(), 1, "same-priority miss must dedup");

        // Different priority → second warn key.
        assert!(reg.resolve(EventPriority::Warning).is_empty());
        assert_eq!(reg.warned_count(), 2);
    }

    #[test]
    fn resolve_returns_only_registered_names() {
        // Routing table names two plugins; only one is registered.
        // Registered subset is returned; missing name fires a warn.
        let mut routing = HashMap::new();
        routing.insert(
            EventPriority::Urgent,
            vec!["stdout".to_string(), "ntfy".to_string()],
        );
        let (tn, _received) = TestNotifier::new("stdout");
        let mut reg = NotifierRegistry::new(NotificationRouting::from_map(routing));
        reg.register("stdout", Arc::new(tn));

        let resolved = reg.resolve(EventPriority::Urgent);
        assert_eq!(resolved.len(), 1, "should return only the registered one");
        assert_eq!(resolved[0].0, "stdout");
        assert_eq!(reg.warned_count(), 1, "one warn for missing 'ntfy'");

        // Second resolve of the same priority: same subset, same warn
        // set size (the missing-name dedup kicks in).
        let again = reg.resolve(EventPriority::Urgent);
        assert_eq!(again.len(), 1);
        assert_eq!(reg.warned_count(), 1);
    }

    #[test]
    fn resolve_distinct_missing_names_are_warned_separately() {
        // Two priorities each referencing a different missing plugin
        // → two distinct warn keys.
        let mut routing = HashMap::new();
        routing.insert(EventPriority::Urgent, vec!["missing-a".to_string()]);
        routing.insert(EventPriority::Warning, vec!["missing-b".to_string()]);
        let reg = NotifierRegistry::new(NotificationRouting::from_map(routing));

        assert!(reg.resolve(EventPriority::Urgent).is_empty());
        assert!(reg.resolve(EventPriority::Warning).is_empty());
        assert_eq!(reg.warned_count(), 2);
    }

    #[test]
    fn resolve_empty_list_warns_once() {
        // A priority configured with an empty list is the same as
        // "missing" from the engine's perspective — warn once, drop.
        let mut routing = HashMap::new();
        routing.insert(EventPriority::Warning, Vec::<String>::new());
        let reg = NotifierRegistry::new(NotificationRouting::from_map(routing));

        assert!(reg.resolve(EventPriority::Warning).is_empty());
        assert_eq!(reg.warned_count(), 1);
        assert!(reg.resolve(EventPriority::Warning).is_empty());
        assert_eq!(reg.warned_count(), 1);
    }

    #[test]
    fn resolve_returns_plugins_in_routing_order() {
        // The per-priority name list is a Vec — dispatch happens in
        // declared order. Locking this in so Phase B's failure
        // aggregation can rely on stable ordering.
        let mut routing = HashMap::new();
        routing.insert(
            EventPriority::Info,
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
        );
        let (a, _) = TestNotifier::new("a");
        let (b, _) = TestNotifier::new("b");
        let (c, _) = TestNotifier::new("c");
        let mut reg = NotifierRegistry::new(NotificationRouting::from_map(routing));
        reg.register("a", Arc::new(a));
        reg.register("b", Arc::new(b));
        reg.register("c", Arc::new(c));

        let resolved = reg.resolve(EventPriority::Info);
        let names: Vec<&str> = resolved.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    // ---- TestNotifier (directly) ----

    #[tokio::test]
    async fn test_notifier_records_payload() {
        // Sanity-check the mock: send one payload, assert it landed
        // in the shared vec. Phase B's engine tests will depend on
        // this mechanism.
        let (tn, received) = TestNotifier::new("test");
        let payload = fake_payload(EventPriority::Urgent);
        tn.send(&payload).await.unwrap();

        let got = received.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].reaction_key, "ci-failed");
        assert_eq!(got[0].priority, EventPriority::Urgent);
    }
}
