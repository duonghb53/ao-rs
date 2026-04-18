//! Escalation bookkeeping: `TrackerState`, `parse_duration`, and the
//! one-shot parse-failure warning helper.

use std::time::{Duration, Instant};

/// Per-(session, reaction) attempt bookkeeping. Mirrors TS `ReactionTracker`.
#[derive(Debug, Clone, Copy)]
pub(super) struct TrackerState {
    /// How many times this reaction has been dispatched for this session.
    /// Incremented *before* the action runs, so a dispatch that errored
    /// still counts.
    pub(super) attempts: u32,
    /// Monotonic `Instant` at which this `(session, reaction_key)` pair
    /// was first observed. Populated on the `or_insert_with` path during
    /// the first dispatch and **never updated** on subsequent dispatches
    /// — that's deliberate, so duration-based escalation (`escalate_after:
    /// 10m`) measures wall-clock time since the first trigger of a given
    /// episode, not since the last attempt.
    ///
    /// Cleared-and-recreated semantics: `clear_tracker` removes the whole
    /// entry, so if a session leaves and re-enters a triggering status
    /// (e.g. `ci-failed` → `working` → `ci-failed`) the next dispatch
    /// gets a fresh `first_triggered_at`. That's correct: a second
    /// episode shouldn't inherit the first episode's elapsed clock.
    pub(super) first_triggered_at: Instant,
}

/// Parse a duration string matching the TS reference's `parseDuration`:
/// `^\d+(s|m|h)$`. Returns `None` on any other shape so callers can no-op.
///
/// This is the honest contract used by both the reaction engine's
/// `escalate_after` duration form and `LifecycleManager::check_stuck`'s
/// stuck-threshold comparison. Kept `pub(crate)` because neither caller
/// is outside `ao-core`.
///
/// Accepted: `"0s"`, `"1s"`, `"10m"`, `"24h"`, etc. Zero is allowed —
/// `threshold: "0s"` is a legitimate test fixture, matching the
/// requirements doc's "no clamping, no floor" decision.
///
/// Rejected (return `None`): compound forms like `"1m30s"`, non-digit
/// prefixes like `"fast"`, missing suffix (`"10"`), empty string, and
/// anything that would overflow `u64` seconds (`checked_mul`).
///
/// Mirrors `packages/core/src/lifecycle-manager.ts` `parseDuration`
/// which returns `0` on garbage — the Rust `None` short-circuits at
/// the callsite the same way.
pub fn parse_duration(s: &str) -> Option<Duration> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let suffix = *bytes.last()?;
    let multiplier_secs: u64 = match suffix {
        b's' => 1,
        b'm' => 60,
        b'h' => 3600,
        _ => return None,
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let total_secs = n.checked_mul(multiplier_secs)?;
    Some(Duration::from_secs(total_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- Phase H: parse_duration ---------- //

    #[test]
    fn parse_duration_accepts_seconds() {
        assert_eq!(parse_duration("1s"), Some(Duration::from_secs(1)));
        assert_eq!(parse_duration("10s"), Some(Duration::from_secs(10)));
        assert_eq!(parse_duration("300s"), Some(Duration::from_secs(300)));
    }

    #[test]
    fn parse_duration_accepts_minutes() {
        assert_eq!(parse_duration("1m"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
    }

    #[test]
    fn parse_duration_accepts_hours() {
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(24 * 3600)));
    }

    #[test]
    fn parse_duration_accepts_zero() {
        // Matches the "no clamping, no floor" decision in the requirements
        // doc — zero is a legitimate test-fixture value (fires on the first
        // idle tick the session observes).
        assert_eq!(parse_duration("0s"), Some(Duration::ZERO));
        assert_eq!(parse_duration("0m"), Some(Duration::ZERO));
        assert_eq!(parse_duration("0h"), Some(Duration::ZERO));
    }

    #[test]
    fn parse_duration_rejects_missing_suffix() {
        assert_eq!(parse_duration("10"), None);
    }

    #[test]
    fn parse_duration_rejects_empty() {
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn parse_duration_rejects_non_numeric() {
        assert_eq!(parse_duration("fast"), None);
        assert_eq!(parse_duration("ten seconds"), None);
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn parse_duration_rejects_compound_form() {
        // TS `parseDuration` doesn't accept `1m30s` — neither do we.
        // Matches the regex `^\d+(s|m|h)$` exactly.
        assert_eq!(parse_duration("1m30s"), None);
        assert_eq!(parse_duration("1h30m"), None);
        assert_eq!(parse_duration("2d"), None);
    }

    #[test]
    fn parse_duration_rejects_negative_and_decimals() {
        assert_eq!(parse_duration("-5m"), None);
        assert_eq!(parse_duration("1.5h"), None);
        assert_eq!(parse_duration("0.5s"), None);
    }

    #[test]
    fn parse_duration_rejects_suffix_only() {
        assert_eq!(parse_duration("s"), None);
        assert_eq!(parse_duration("m"), None);
        assert_eq!(parse_duration("h"), None);
    }

    #[test]
    fn parse_duration_rejects_overflow() {
        // `u64::MAX` seconds parsed fine, but multiplying the digits of
        // an unbounded hours string must short-circuit to None rather
        // than panic or wrap.
        assert_eq!(parse_duration("99999999999999999999h"), None);
    }
}
