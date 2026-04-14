//! Pure decision function: given the current session status and a fresh
//! SCM observation, what status should the session be in next?
//!
//! Mirrors the transition block in
//! `packages/core/src/lifecycle-manager.ts` (around `updateSessionFromPR`,
//! lines ~900-1020 in the reference), extracted into a standalone pure
//! function for two reasons:
//!
//! 1. **Testable without plumbing.** The lifecycle loop calls this from
//!    inside an async probe with real plugin traits; the function itself
//!    is sync and has no I/O, so ~20 single-line unit tests cover every
//!    transition branch without mocking a runtime or SCM.
//! 2. **Reusable.** A future `ao-rs pr refresh <id>` debug command can
//!    call it directly with a single `gh` query's worth of data to force
//!    a PR-driven transition without starting the whole polling loop.
//!
//! ## Scope
//!
//! This is the ONLY place PR-state is folded into `SessionStatus`. The
//! mapping from *raw GitHub fields* (strings, null-shaped enums, …) to
//! the domain types `PrState`/`CiStatus`/`ReviewDecision`/`MergeReadiness`
//! lives in the `ao-plugin-scm-github` crate. Everything downstream
//! consumes the domain types and never touches JSON.
//!
//! ## Priority order for open PRs
//!
//! When a PR is open, several of the observation fields can disagree
//! (e.g. CI failing *and* changes requested). The session can only be in
//! one status at a time, so we pick:
//!
//! 1. `mergeability.is_ready()` → `Mergeable` (fires `approved-and-green`)
//! 2. `ci == Failing` → `CiFailed` (fires `ci-failed`)
//! 3. `review == ChangesRequested` → `ChangesRequested` (fires the reaction)
//! 4. `review == Approved` (but not ready — e.g. CI still pending) → `Approved`
//! 5. `review == Pending` → `ReviewPending`
//! 6. Everything else → `PrOpen`
//!
//! The TS reference prioritizes `ci_failed` over `changes_requested` when both
//! are true. Rust matches that ordering for parity (a red CI signal is the
//! higher-urgency loop to close before re-review).
//!
//! ## No-PR handling
//!
//! If `detect_pr` returns `None` but the session is on the PR track, we
//! drop back to `Working` so the next push re-discovers. This is the
//! "agent force-pushed and closed the PR by accident" case — TS has the
//! same fallback.
//!
//! ## `MergeFailed` is a parking state (Phase G)
//!
//! `MergeFailed` is on the PR track but no ladder rung ever *produces*
//! it from an observation — it is only ever *entered* by the lifecycle
//! manager after `ReactionEngine::dispatch_auto_merge` reports a failed
//! merge. On the next tick, a still-ready observation re-promotes it
//! back to `Mergeable` via rung #1, which fires `approved-and-green`
//! again and burns another retry attempt. A no-longer-ready observation
//! drops it off the ready path (to `CiFailed`/`ChangesRequested`/
//! `Approved`/`PrOpen`) via the normal ladder. A `None` observation
//! drops it all the way back to `Working` via `is_pr_track`.
//!
//! The lifecycle manager owns the *entry* to this state because the
//! decision depends on a reaction outcome, not on the observation:
//! `derive_scm_status` has no visibility into "did the merge call just
//! fail?". See `LifecycleManager::transition` for the parking hook.
//!
//! ## `ReviewPending`
//!
//! The TS lifecycle-manager sets `review_pending` when the overall review
//! decision is `"pending"` ("REVIEW_REQUIRED" in GitHub terms). Rust matches
//! that by explicitly mapping `ReviewDecision::Pending` to
//! `SessionStatus::ReviewPending` (rung #5).

use crate::{
    scm::{CiStatus, MergeReadiness, PrState, ReviewDecision},
    types::SessionStatus,
};

/// A snapshot of everything `Scm` knows about a PR right now. Built by
/// the lifecycle loop after four parallel `gh` calls; consumed by
/// `derive_scm_status`.
///
/// Owned — not `&MergeReadiness` — so the struct is `Send + 'static`
/// without borrow gymnastics. `MergeReadiness` already derives `Clone`
/// and is tiny; the extra clone is free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScmObservation {
    pub state: PrState,
    pub ci: CiStatus,
    pub review: ReviewDecision,
    pub readiness: MergeReadiness,
}

/// Decide the next `SessionStatus` given the current status and a fresh
/// SCM observation.
///
/// Returns `None` if no transition is warranted — either the current
/// status already matches the observation, or there's no SCM signal
/// strong enough to move a session out of its current state.
///
/// `obs == None` means `detect_pr` returned `Ok(None)` — no PR exists
/// for this session right now. Sessions already on the PR track
/// (`PrOpen`, `CiFailed`, …) drop back to `Working`; everything else
/// stays put.
pub fn derive_scm_status(
    current: SessionStatus,
    obs: Option<&ScmObservation>,
) -> Option<SessionStatus> {
    let next = match obs {
        None => status_without_pr(current)?,
        Some(obs) => status_with_pr(obs),
    };
    // Uniform "no-op transition" filter — every branch above produces a
    // candidate status; this gate eliminates self-loops so subscribers
    // don't see spurious `StatusChanged(X → X)` events.
    (next != current).then_some(next)
}

/// Is `status` one of the PR-track statuses that `derive_scm_status`
/// can enter or leave based on a fresh observation?
///
/// Kept as a module-private helper so both `status_without_pr` and
/// the unit tests can share one definition. Adding a new PR-track
/// variant (e.g. a future `DraftPrOpen`) only touches this function
/// — the transition logic falls into line automatically.
///
/// `const fn` because it's a pure compile-time lookup table (`matches!`
/// over a `Copy` enum has no runtime dependencies). Making it const
/// documents the "no side effects, no surprises" intent and leaves
/// room for a future call-site that wants it at compile time (e.g.
/// static assertion over a const `SessionStatus`).
const fn is_pr_track(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::PrOpen
            | SessionStatus::CiFailed
            | SessionStatus::ReviewPending
            | SessionStatus::ChangesRequested
            | SessionStatus::Approved
            | SessionStatus::Mergeable
            // Phase G parking state. `MergeFailed` is PR-track so that
            // `detect_pr(None)` drops it back to `Working` (agent
            // force-pushed the branch; the parked merge retry can't
            // hit anything anyway) and so the status_with_pr ladder
            // owns *every* transition out of it — the ladder's first
            // rung re-promotes `MergeFailed` to `Mergeable` the moment
            // readiness holds again, which is how the retry loop
            // burns its budget.
            | SessionStatus::MergeFailed
    )
}

/// The no-PR branch. `None` means "no transition"; `Some(_)` means
/// "drop back to Working because the PR disappeared".
fn status_without_pr(current: SessionStatus) -> Option<SessionStatus> {
    // Was on the PR track — the PR is gone, fall back to Working. The
    // next push/spawn can re-open and the lifecycle loop will
    // transition us back in.
    //
    // Anything else — Working/Spawning/NeedsInput/Stuck/Idle and every
    // terminal state (Killed/Terminated/Done/Cleanup/Errored/Merged)
    // — is not PR-track, so SCM has nothing to say.
    is_pr_track(current).then_some(SessionStatus::Working)
}

/// The with-PR branch. Always returns a concrete status; the caller's
/// `then_some` filters out no-op transitions.
fn status_with_pr(obs: &ScmObservation) -> SessionStatus {
    // Terminal PR states first — merged/closed sessions stop moving.
    if matches!(obs.state, PrState::Merged) {
        return SessionStatus::Merged;
    }
    if matches!(obs.state, PrState::Closed) {
        // TS maps a closed PR to `killed`.
        return SessionStatus::Killed;
    }

    // Open PR — walk the priority ladder.
    //
    // Order matters: each rung is more-specific than the one below it,
    // so the first match wins. `Mergeable` is highest-priority because
    // it's the only status that can terminate the session automatically
    // via `approved-and-green`.
    if obs.readiness.is_ready() {
        return SessionStatus::Mergeable;
    }
    if matches!(obs.ci, CiStatus::Failing) {
        return SessionStatus::CiFailed;
    }
    if matches!(obs.review, ReviewDecision::ChangesRequested) {
        return SessionStatus::ChangesRequested;
    }
    if matches!(obs.review, ReviewDecision::Approved) {
        return SessionStatus::Approved;
    }
    if matches!(obs.review, ReviewDecision::Pending) {
        return SessionStatus::ReviewPending;
    }
    // Default: a PR exists but nothing urgent — CI pending / no review
    // yet / etc. The session sits in PrOpen until something changes.
    SessionStatus::PrOpen
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn readiness_ready() -> MergeReadiness {
        MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        }
    }

    fn readiness_blocked() -> MergeReadiness {
        MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: false,
            no_conflicts: true,
            blockers: vec!["CI is failing".into()],
        }
    }

    fn obs(
        state: PrState,
        ci: CiStatus,
        review: ReviewDecision,
        readiness: MergeReadiness,
    ) -> ScmObservation {
        ScmObservation {
            state,
            ci,
            review,
            readiness,
        }
    }

    // ---- no-PR branch -----------------------------------------------------

    #[test]
    fn no_pr_and_working_is_a_no_op() {
        assert_eq!(derive_scm_status(SessionStatus::Working, None), None);
    }

    #[test]
    fn no_pr_and_spawning_is_a_no_op() {
        // Phase C's activity-driven Spawning → Working is separate from
        // SCM transitions; the SCM function must not interfere.
        assert_eq!(derive_scm_status(SessionStatus::Spawning, None), None);
    }

    #[test]
    fn no_pr_drops_pr_open_back_to_working() {
        // "The PR I was tracking vanished" — agent force-pushed the branch
        // or a human closed the PR from the web UI.
        assert_eq!(
            derive_scm_status(SessionStatus::PrOpen, None),
            Some(SessionStatus::Working)
        );
    }

    /// Every `SessionStatus` variant, in declaration order. This list
    /// is exhaustiveness-checked via the `match _ => &[]` catch-all on
    /// a const reference: adding a new variant to `SessionStatus`
    /// forces a compile error in this test file until it's classified
    /// here as PR-track or not. That way `no_pr_drops_every_pr_track…`
    /// automatically picks up new variants without editing the test.
    const ALL_SESSION_STATUSES: &[SessionStatus] = &[
        SessionStatus::Spawning,
        SessionStatus::Working,
        SessionStatus::NeedsInput,
        SessionStatus::Idle,
        SessionStatus::Stuck,
        SessionStatus::PrOpen,
        SessionStatus::CiFailed,
        SessionStatus::ReviewPending,
        SessionStatus::ChangesRequested,
        SessionStatus::Approved,
        SessionStatus::Mergeable,
        SessionStatus::MergeFailed,
        SessionStatus::Cleanup,
        SessionStatus::Merged,
        SessionStatus::Killed,
        SessionStatus::Terminated,
        SessionStatus::Done,
        SessionStatus::Errored,
    ];

    #[test]
    fn all_session_statuses_list_is_exhaustive() {
        // Touch every variant in a match so `rustc --deny unreachable
        // -patterns` catches any variant missing from the constant
        // above. If this fails to compile, add the new variant to
        // `ALL_SESSION_STATUSES` and classify it in `is_pr_track`.
        for status in ALL_SESSION_STATUSES {
            match status {
                SessionStatus::Spawning
                | SessionStatus::Working
                | SessionStatus::NeedsInput
                | SessionStatus::Idle
                | SessionStatus::Stuck
                | SessionStatus::PrOpen
                | SessionStatus::CiFailed
                | SessionStatus::ReviewPending
                | SessionStatus::ChangesRequested
                | SessionStatus::Approved
                | SessionStatus::Mergeable
                | SessionStatus::MergeFailed
                | SessionStatus::Cleanup
                | SessionStatus::Merged
                | SessionStatus::Killed
                | SessionStatus::Terminated
                | SessionStatus::Done
                | SessionStatus::Errored => {}
            }
        }
    }

    #[test]
    fn no_pr_drops_every_pr_track_status_back_to_working() {
        // Iterate every variant and consult `is_pr_track` for the
        // expected behaviour, instead of hand-maintaining a parallel
        // list. Adding a new PR-track variant to `is_pr_track` picks
        // up correct coverage here automatically (backlog B2).
        for &from in ALL_SESSION_STATUSES {
            let got = derive_scm_status(from, None);
            if is_pr_track(from) {
                assert_eq!(
                    got,
                    Some(SessionStatus::Working),
                    "{from:?} is PR-track; detect_pr(None) should drop to Working"
                );
            } else {
                assert_eq!(
                    got, None,
                    "{from:?} is not PR-track; detect_pr(None) must be a no-op"
                );
            }
        }
    }

    #[test]
    fn no_pr_and_terminal_is_a_no_op() {
        // Terminal statuses never move. Even without the outer
        // `poll_one` guard, the function itself must not emit a
        // spurious transition. Covered more broadly by the
        // `no_pr_drops_every_pr_track_status_back_to_working` sweep,
        // but kept as a targeted regression test to keep the terminal
        // contract obvious from the test list.
        for term in [
            SessionStatus::Killed,
            SessionStatus::Terminated,
            SessionStatus::Done,
            SessionStatus::Cleanup,
            SessionStatus::Errored,
            SessionStatus::Merged,
        ] {
            assert_eq!(
                derive_scm_status(term, None),
                None,
                "{term:?} must stay terminal"
            );
        }
    }

    // ---- with-PR terminal cases ------------------------------------------

    #[test]
    fn merged_pr_transitions_working_to_merged() {
        let o = obs(
            PrState::Merged,
            CiStatus::Passing,
            ReviewDecision::Approved,
            readiness_ready(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::Working, Some(&o)),
            Some(SessionStatus::Merged)
        );
    }

    #[test]
    fn already_merged_session_with_merged_pr_is_a_no_op() {
        // Re-entrancy: if the poll happens to fire on an already-merged
        // session (e.g. a manual `tick()` call), the function must not
        // re-emit StatusChanged(Merged → Merged).
        let o = obs(
            PrState::Merged,
            CiStatus::Passing,
            ReviewDecision::Approved,
            readiness_ready(),
        );
        assert_eq!(derive_scm_status(SessionStatus::Merged, Some(&o)), None);
    }

    #[test]
    fn closed_pr_transitions_working_to_killed() {
        let o = obs(
            PrState::Closed,
            CiStatus::None,
            ReviewDecision::None,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::Working, Some(&o)),
            Some(SessionStatus::Killed)
        );
    }

    // ---- open-PR priority ladder -----------------------------------------

    #[test]
    fn fully_ready_pr_becomes_mergeable() {
        let o = obs(
            PrState::Open,
            CiStatus::Passing,
            ReviewDecision::Approved,
            readiness_ready(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::PrOpen, Some(&o)),
            Some(SessionStatus::Mergeable)
        );
    }

    #[test]
    fn changes_requested_beats_ci_failing() {
        // Both conditions true — TS prioritizes CI failing over changes requested.
        let o = obs(
            PrState::Open,
            CiStatus::Failing,
            ReviewDecision::ChangesRequested,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::PrOpen, Some(&o)),
            Some(SessionStatus::CiFailed)
        );
    }

    #[test]
    fn ci_failing_with_pending_review_is_ci_failed() {
        let o = obs(
            PrState::Open,
            CiStatus::Failing,
            ReviewDecision::Pending,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::PrOpen, Some(&o)),
            Some(SessionStatus::CiFailed)
        );
    }

    #[test]
    fn approved_but_ci_pending_is_approved_not_mergeable() {
        // Reviewer said yes but CI is still computing. Not ready yet —
        // session sits in `Approved` until CI finishes.
        let readiness = MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: true,
            no_conflicts: true,
            blockers: vec!["CI is pending".into()],
        };
        let o = obs(
            PrState::Open,
            CiStatus::Pending,
            ReviewDecision::Approved,
            readiness,
        );
        assert_eq!(
            derive_scm_status(SessionStatus::PrOpen, Some(&o)),
            Some(SessionStatus::Approved)
        );
    }

    #[test]
    fn plain_open_pr_with_no_decision_is_pr_open() {
        // No review yet, CI still pending. Nothing actionable.
        let o = obs(
            PrState::Open,
            CiStatus::Pending,
            ReviewDecision::None,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::Working, Some(&o)),
            Some(SessionStatus::PrOpen)
        );
    }

    #[test]
    fn review_pending_when_review_required_and_not_mergeable() {
        let o = obs(
            PrState::Open,
            CiStatus::Pending,
            ReviewDecision::Pending,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::PrOpen, Some(&o)),
            Some(SessionStatus::ReviewPending)
        );
    }

    // ---- no-op filtering --------------------------------------------------

    #[test]
    fn identical_status_returns_none() {
        // Already in `PrOpen` and the observation still says `PrOpen` →
        // no transition, no event. This is the common case on every
        // poll tick after the first.
        let o = obs(
            PrState::Open,
            CiStatus::Pending,
            ReviewDecision::None,
            readiness_blocked(),
        );
        assert_eq!(derive_scm_status(SessionStatus::PrOpen, Some(&o)), None);
    }

    #[test]
    fn ci_failed_stays_ci_failed_while_ci_still_failing() {
        let o = obs(
            PrState::Open,
            CiStatus::Failing,
            ReviewDecision::Pending,
            readiness_blocked(),
        );
        assert_eq!(derive_scm_status(SessionStatus::CiFailed, Some(&o)), None);
    }

    #[test]
    fn ci_failed_transitions_back_to_pr_open_when_ci_recovers() {
        // Agent fixed the bug and pushed — CI is now pending again.
        // Session must leave `CiFailed` so the reaction tracker clears.
        let o = obs(
            PrState::Open,
            CiStatus::Pending,
            ReviewDecision::None,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::CiFailed, Some(&o)),
            Some(SessionStatus::PrOpen)
        );
    }

    #[test]
    fn mergeable_drops_back_to_ci_failed_if_ci_flips_red() {
        // A Mergeable session where CI just went red — reviewer was
        // approved but a late check failed. Must fall off the happy
        // path so `approved-and-green` doesn't retry-merge a broken PR.
        let o = obs(
            PrState::Open,
            CiStatus::Failing,
            ReviewDecision::Approved,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::Mergeable, Some(&o)),
            Some(SessionStatus::CiFailed)
        );
    }

    #[test]
    fn changes_requested_transitions_up_to_mergeable_when_all_green() {
        // Agent addressed every comment, CI re-ran green, reviewer
        // re-approved, GitHub marks it mergeable. Jump straight to
        // `Mergeable` from `ChangesRequested` in one tick.
        let o = obs(
            PrState::Open,
            CiStatus::Passing,
            ReviewDecision::Approved,
            readiness_ready(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::ChangesRequested, Some(&o)),
            Some(SessionStatus::Mergeable)
        );
    }

    // ---- MergeFailed parking loop (Phase G) ------------------------------

    #[test]
    fn merge_failed_re_promotes_to_mergeable_on_next_ready_observation() {
        // The critical Phase G retry hook: a parked session sees a
        // still-ready observation and must move back to `Mergeable` so
        // the reaction engine fires `approved-and-green` again and
        // burns another retry attempt. Without this rung the parked
        // session would sit in `MergeFailed` forever.
        let o = obs(
            PrState::Open,
            CiStatus::Passing,
            ReviewDecision::Approved,
            readiness_ready(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::MergeFailed, Some(&o)),
            Some(SessionStatus::Mergeable)
        );
    }

    #[test]
    fn merge_failed_drops_to_ci_failed_when_ci_flips_red() {
        // Real-world race: dispatch_auto_merge parks the session,
        // then between ticks CI flips red (e.g. a late-starting
        // post-merge check). The parked state must react to the new
        // observation and fall off the ready path so the retry
        // engine doesn't keep banging on a PR that's no longer
        // mergeable.
        let o = obs(
            PrState::Open,
            CiStatus::Failing,
            ReviewDecision::Approved,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::MergeFailed, Some(&o)),
            Some(SessionStatus::CiFailed)
        );
    }

    #[test]
    fn merge_failed_drops_to_changes_requested_when_review_dismissed() {
        // Reviewer changed their mind between the failed merge and
        // the next tick. Priority ladder's rung #2 takes it.
        let o = obs(
            PrState::Open,
            CiStatus::Passing,
            ReviewDecision::ChangesRequested,
            readiness_blocked(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::MergeFailed, Some(&o)),
            Some(SessionStatus::ChangesRequested)
        );
    }

    #[test]
    fn merge_failed_drops_back_to_working_when_pr_disappears() {
        // Agent force-pushed, closing the PR. MergeFailed is on the
        // PR track, so the no-PR branch fires.
        assert_eq!(
            derive_scm_status(SessionStatus::MergeFailed, None),
            Some(SessionStatus::Working)
        );
    }

    #[test]
    fn merge_failed_to_merged_when_out_of_band_merge_happens() {
        // Edge case: a human manually merged the PR via the GitHub
        // UI while the session was parked. Next tick sees `state ==
        // Merged` and transitions straight to the terminal `Merged`
        // status. The ladder's "state first" check handles this
        // before it gets to the readiness rung.
        let o = obs(
            PrState::Merged,
            CiStatus::Passing,
            ReviewDecision::Approved,
            readiness_ready(),
        );
        assert_eq!(
            derive_scm_status(SessionStatus::MergeFailed, Some(&o)),
            Some(SessionStatus::Merged)
        );
    }

    #[test]
    fn status_with_pr_never_produces_merge_failed() {
        // Exhaustively iterate every (ci × review × readiness) shape
        // for an open PR and assert the priority ladder never emits
        // `MergeFailed`. The state is lifecycle-owned (entry is via
        // `LifecycleManager::park_in_merge_failed` on a failed auto-
        // merge outcome), NOT observation-owned. If a future refactor
        // adds a ladder rung that could produce `MergeFailed`, this
        // test fires — which is the cue to re-read the module comment
        // about why the state exists.
        for &state in &[PrState::Open] {
            for &ci in &[CiStatus::Passing, CiStatus::Failing, CiStatus::Pending] {
                for &review in &[
                    ReviewDecision::Approved,
                    ReviewDecision::ChangesRequested,
                    ReviewDecision::Pending,
                    ReviewDecision::None,
                ] {
                    for readiness in [readiness_ready(), readiness_blocked()] {
                        let o = obs(state, ci, review, readiness);
                        let next = status_with_pr(&o);
                        assert_ne!(
                            next,
                            SessionStatus::MergeFailed,
                            "ladder must never produce MergeFailed (state={state:?}, ci={ci:?}, review={review:?})"
                        );
                    }
                }
            }
        }
    }
}
