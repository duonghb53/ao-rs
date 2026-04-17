//! Shared GitHub API rate-limit state.
//!
//! Both `scm-github` and `tracker-github` plugins invoke `gh` subprocesses.
//! When either plugin observes a rate-limit error, all `gh` calls across
//! the process should back off — otherwise the other plugin keeps firing
//! requests into a 403 response, risking secondary-rate-limit penalties.
//!
//! This module owns the single cooldown instant used by both plugins.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default cooldown window when a plugin detects a rate-limit error.
///
/// Kept at 120s to match the previous per-plugin behavior. Callers that
/// know the precise reset time (e.g. via `gh api rate_limit`) can pass a
/// custom `Duration` to [`enter_cooldown_for`].
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(120);

/// Returns true if the error message looks like a GitHub rate-limit
/// response (REST or GraphQL, primary or secondary).
pub fn is_rate_limited_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("api rate limit")
        || m.contains("secondary rate limit")
        || m.contains("rate limit exceeded")
        || m.contains("graphql: api rate limit")
}

fn cooldown_until() -> &'static Mutex<Option<Instant>> {
    static COOLDOWN: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    COOLDOWN.get_or_init(|| Mutex::new(None))
}

/// Returns true if we are currently in a rate-limit cooldown window.
///
/// Clears the stored instant once it has elapsed so subsequent checks
/// are cheap and logs accurately reflect recovery.
pub fn in_cooldown_now() -> bool {
    let Ok(mut guard) = cooldown_until().lock() else {
        return false;
    };
    if let Some(until) = *guard {
        if Instant::now() < until {
            return true;
        }
        *guard = None;
    }
    false
}

/// Enter cooldown for the default 120s window.
pub fn enter_cooldown() {
    enter_cooldown_for(DEFAULT_COOLDOWN);
}

/// Enter cooldown for a specific duration.
///
/// If a cooldown is already active with a later expiry, this call is a
/// no-op — we never shorten an existing cooldown. If `duration` is too
/// large to represent as a future `Instant` (e.g. malformed upstream
/// timestamps), we silently skip instead of panicking.
pub fn enter_cooldown_for(duration: Duration) {
    let Ok(mut guard) = cooldown_until().lock() else {
        return;
    };
    let Some(new_until) = Instant::now().checked_add(duration) else {
        return;
    };
    match *guard {
        Some(existing) if existing >= new_until => {}
        _ => *guard = Some(new_until),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn detects_rate_limit_messages() {
        assert!(is_rate_limited_error("API rate limit exceeded"));
        assert!(is_rate_limited_error(
            "You have exceeded a secondary rate limit"
        ));
        assert!(is_rate_limited_error("GraphQL: API rate limit exceeded"));
        assert!(is_rate_limited_error("rate limit exceeded for app"));
        assert!(!is_rate_limited_error("404 not found"));
        assert!(!is_rate_limited_error(""));
    }

    // The cooldown state is process-global, so these scenarios share one
    // test to keep them sequential. Running them as separate #[test]
    // functions would race against each other under `cargo test`'s
    // default parallel execution.
    #[test]
    fn cooldown_lifecycle() {
        // Reset.
        if let Ok(mut g) = cooldown_until().lock() {
            *g = None;
        }

        // Short cooldown becomes active then expires.
        enter_cooldown_for(Duration::from_millis(30));
        assert!(in_cooldown_now());
        sleep(Duration::from_millis(60));
        assert!(!in_cooldown_now());

        // A long cooldown is not shortened by a subsequent short one.
        enter_cooldown_for(Duration::from_secs(60));
        let first = *cooldown_until().lock().unwrap();
        enter_cooldown_for(Duration::from_millis(10));
        let after_short = *cooldown_until().lock().unwrap();
        assert_eq!(
            first, after_short,
            "short cooldown must not shorten longer one"
        );

        // Clean up so other tests in the process start from a fresh slate.
        if let Ok(mut g) = cooldown_until().lock() {
            *g = None;
        }
    }

    #[test]
    fn enter_cooldown_for_ignores_overflowing_duration() {
        // An absurdly large duration (e.g. from a spoofed reset timestamp)
        // used to panic via `Instant::now() + duration`. After the fix it
        // silently skips.
        if let Ok(mut g) = cooldown_until().lock() {
            *g = None;
        }
        enter_cooldown_for(Duration::MAX);
        // No panic. Cooldown should NOT be set because the `Instant::now()
        // + Duration::MAX` overflows.
        assert!(
            !in_cooldown_now(),
            "overflowing duration must not activate cooldown"
        );
    }
}
