use super::*;

impl LifecycleManager {
    /// Probe the configured SCM plugin for this session and apply any
    /// status transition the observation implies.
    ///
    /// Structure:
    ///   1. `detect_pr` → `Option<PullRequest>`. `None` skips all field
    ///      probes and lets `derive_scm_status(current, None)` decide
    ///      whether the session should drop off the PR track.
    ///   2. `tokio::join!` fans out `pr_state` / `ci_status` /
    ///      `review_decision` / `mergeability` in parallel so the four
    ///      `gh` calls pay one RTT, not four. Matches `ao-rs pr`.
    ///   3. Failures in any field probe emit `TickError` and skip the
    ///      transition — we'd rather miss a tick than transition on a
    ///      partial observation. Next tick re-probes.
    ///   4. The observation is folded through the pure `derive_scm_status`
    ///      function (see `scm_transitions` module) which returns
    ///      `Some(next)` only when a real transition is warranted.
    pub(super) async fn poll_scm(&self, session: &mut Session) -> Result<()> {
        // Defense in depth: `tick()` already filters terminal sessions at
        // line ~199, and the activity path in `poll_one` can't currently
        // transition *into* a terminal status before reaching step 5. But
        // the invariant is implicit, not enforced by the type system, and
        // a future step 4 that ends in `Merged`/`Terminated` would bypass
        // the `tick()` filter for the current tick. Re-check here so the
        // SCM probe can never run — or worse, re-transition — a session
        // that some upstream step has already finalised.
        if session.is_terminal() {
            return Ok(());
        }

        let scm = self
            .scm
            .as_ref()
            .expect("poll_scm called without an SCM plugin");

        // ---- 1. Detect PR ----
        // Prefer the pre-detected PR from tick() Pass 1. Fall back to
        // a fresh detect_pr call for tests or edge cases where the
        // cache wasn't populated.
        let pr = {
            let mut cache = self.detected_prs_cache.lock().unwrap_or_else(|e| {
                tracing::error!("detected_prs_cache mutex poisoned; recovering inner state: {e}");
                e.into_inner()
            });
            cache.remove(&session.id)
        };
        let pr = match pr {
            Some(cached) => cached,
            None => match scm.detect_pr(session).await {
                Ok(pr) => pr,
                Err(e) => {
                    self.emit(OrchestratorEvent::TickError {
                        id: session.id.clone(),
                        message: format!("scm.detect_pr: {e}"),
                    });
                    return Ok(());
                }
            },
        };

        // Save a clone so later helpers (check_ci_failed, check_review_backlog)
        // can reference the PR after the observation-building block moves it.
        let pr_saved = pr.clone();

        // Persist the PR reference once it's discovered so it remains visible
        // after merge/cleanup even when `detect_pr` later returns None (e.g.
        // branch deleted, head ref no longer matches).
        if let Some(ref pr) = pr_saved {
            if session.claimed_pr_number.is_none() {
                session.claimed_pr_number = Some(pr.number);
            }
            if session.claimed_pr_url.is_none() {
                session.claimed_pr_url = Some(pr.url.clone());
            }
            // Best-effort persistence: failures shouldn't block status polling.
            if let Err(e) = self.sessions.save(session).await {
                tracing::warn!(
                    session = %session.id,
                    error = %e,
                    "failed to persist claimed PR reference"
                );
            }
        }

        // Build the optional observation.
        let observation = if let Some(pr) = pr {
            // ---- 2. Check batch enrichment cache ----
            let cache_key = format!("{}/{}#{}", pr.owner, pr.repo, pr.number);
            let cached = {
                let mut cache = self.pr_enrichment_cache.lock().unwrap_or_else(|e| {
                    tracing::error!(
                        "pr_enrichment_cache mutex poisoned; recovering inner state: {e}"
                    );
                    e.into_inner()
                });
                cache.remove(&cache_key)
            };

            if let Some(enrichment) = cached {
                tracing::trace!(
                    "poll_scm: using cached batch observation for PR #{}",
                    pr.number
                );
                Some(enrichment.observation)
            } else {
                // ---- Review backlog throttle ----
                // When there's no batch cache hit and the session is in a
                // review-related state, skip the expensive REST fallback
                // unless 2+ minutes have passed since the last check.
                if is_review_stable(session.status) {
                    let throttled = {
                        let map = self
                            .last_review_backlog_check
                            .lock()
                            .unwrap_or_else(|e| {
                                tracing::error!("last_review_backlog_check mutex poisoned; recovering inner state: {e}");
                                e.into_inner()
                            });
                        map.get(&session.id)
                            .map(|t| t.elapsed() < REVIEW_BACKLOG_THROTTLE)
                            .unwrap_or(false)
                    };
                    if throttled {
                        tracing::trace!(
                            "poll_scm: review backlog throttled for session {}",
                            session.id.0
                        );
                        return Ok(());
                    }
                }

                // ---- 3. Parallel fan-out (fallback) ----
                let (state_res, ci_res, review_res, readiness_res) = tokio::join!(
                    scm.pr_state(&pr),
                    scm.ci_status(&pr),
                    scm.review_decision(&pr),
                    scm.mergeability(&pr),
                );

                // Record the check timestamp for throttling
                {
                    let mut map = self.last_review_backlog_check.lock().unwrap_or_else(|e| {
                        tracing::error!(
                            "last_review_backlog_check mutex poisoned; recovering inner state: {e}"
                        );
                        e.into_inner()
                    });
                    map.insert(session.id.clone(), Instant::now());
                }

                match assemble_observation(state_res, ci_res, review_res, readiness_res) {
                    Ok(obs) => Some(obs),
                    Err(msg) => {
                        self.emit(OrchestratorEvent::TickError {
                            id: session.id.clone(),
                            message: format!("scm probes: {msg}"),
                        });
                        return Ok(());
                    }
                }
            }
        } else {
            None
        };

        // ---- 4. Pure decision + transition ----
        if let Some(mut next) = derive_scm_status(session.status, observation.as_ref()) {
            // TS stuck detection can override the fallback `pr_open` state when
            // the agent has been idle beyond threshold. To preserve the Rust
            // invariant of **one transition per tick**, we apply that override
            // here before persisting/emitting the transition.
            if next == SessionStatus::PrOpen && self.should_mark_stuck(session) {
                next = SessionStatus::Stuck;
            }
            self.transition(session, next).await?;
        }

        // ---- 5. Orthogonal merge-conflict check ----
        // Runs after the transition so we see the post-transition status
        // (in particular, `Merged` / `Killed` correctly enter the clear
        // branch). Skipped when the reaction engine is absent or no PR is
        // in hand — the helper is safe to call either way and returns
        // early, but checking here keeps the hot path allocation-free.
        if self.reaction_engine.is_some() {
            self.check_merge_conflicts(session, observation.as_ref())
                .await?;
        }

        // ---- 6. CI-failed detail dispatch (issue #195 H3) ----
        // When the session just landed in `CiFailed`, supplement the generic
        // status-driven reaction with check names + URLs from `ci_checks`.
        // Only runs when a PR is in hand and a reaction engine is wired in.
        if session.status == SessionStatus::CiFailed {
            if let Some(ref pr) = pr_saved {
                if self.reaction_engine.is_some() {
                    self.check_ci_failed(session, pr).await?;
                }
            }
        }

        // ---- 7. Review-backlog re-dispatch (issue #195 H2) ----
        // When fingerprint of pending comments changes, re-fire
        // `changes-requested` so the agent sees fresh reviewer feedback.
        // Only called when the throttle (managed by `poll_scm` above) allowed
        // the full REST fan-out this tick.
        if let Some(ref pr) = pr_saved {
            if self.reaction_engine.is_some() {
                self.check_review_backlog(session, pr).await?;
            }
        }

        // ---- 8. Bugbot comments dispatch (issue #212) ----
        // When new automated bot comments appear on the PR, dispatch a
        // detailed `bugbot-comments` reaction so the agent knows exactly
        // which checks flagged which lines.
        if let Some(ref pr) = pr_saved {
            if self.reaction_engine.is_some() {
                self.check_bugbot_comments(session, pr).await?;
            }
        }

        Ok(())
    }

    /// Port of `maybeDispatchReviewBacklog` from
    /// `packages/core/src/lifecycle-manager.ts:758-932`.
    ///
    /// After the initial `changes-requested` reaction fires, this helper
    /// re-dispatches whenever reviewers leave *new* comments. It fingerprints
    /// the current `pending_comments` set and compares against the last-seen
    /// fingerprint stored on the session. When the fingerprint changes a fresh
    /// `changes-requested` dispatch is fired so the agent sees the new
    /// feedback. Same-fingerprint ticks are silent (de-dup).
    ///
    /// **Throttle**: the caller (`poll_scm`) manages the 2-minute throttle via
    /// `last_review_backlog_check`; this helper is only called when the
    /// throttle has already been cleared for this tick.
    ///
    /// Only runs when:
    /// - A reaction engine is wired in.
    /// - The session is in a review-backlog-eligible state (`is_review_stable`).
    /// - A PR is in hand (caller guarantees this via `pr` parameter).
    pub(super) async fn check_review_backlog(
        &self,
        session: &mut Session,
        pr: &PullRequest,
    ) -> Result<()> {
        let Some(engine) = self.reaction_engine.as_ref() else {
            return Ok(());
        };
        let Some(scm) = self.scm.as_ref() else {
            return Ok(());
        };

        if !is_review_stable(session.status) {
            return Ok(());
        }

        let comments = match scm.pending_comments(pr).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    session = %session.id,
                    error = %e,
                    "pending_comments failed; skipping review backlog check"
                );
                return Ok(());
            }
        };

        // Fingerprint: stable hash of sorted (author, body, url) triples.
        let fingerprint = fingerprint_comments(&comments);

        let prev = session.last_review_backlog_fingerprint;
        if prev == Some(fingerprint) {
            // Nothing changed — no dispatch.
            return Ok(());
        }

        // Only dispatch when there are actual comments to act on.
        if comments.is_empty() {
            // No comments yet; store the empty fingerprint so a future
            // non-empty set triggers a dispatch.
            if prev.is_none() {
                session.last_review_backlog_fingerprint = Some(fingerprint);
                self.sessions.save(session).await?;
            }
            return Ok(());
        }

        // Build a formatted message from the new comments.
        let mut msg = String::from("New review comments on your PR:\n");
        for c in &comments {
            if let Some(ref path) = c.path {
                msg.push_str(&format!("- {} ({}): {}\n", c.author, path, c.body));
            } else {
                msg.push_str(&format!("- {}: {}\n", c.author, c.body));
            }
        }
        msg.push_str("\nCheck with `gh pr view --comments`, address the feedback, and push.");

        // Dispatch. The engine tracks attempts; cleared on transition exit.
        engine
            .dispatch_with_message(session, "changes-requested", msg)
            .await?;

        session.last_review_backlog_fingerprint = Some(fingerprint);
        self.sessions.save(session).await?;

        Ok(())
    }

    /// Build a CI-failure detail message and dispatch the `ci-failed`
    /// reaction when the session just entered `CiFailed`.
    ///
    /// Unlike the status-driven path in `transition` (which dispatches
    /// through `status_to_reaction_key` using the static YAML message),
    /// this helper fetches the live `ci_checks` list and formats failing
    /// check names / run URLs into the message body so the agent knows
    /// *which* checks failed.
    ///
    /// Called from `poll_scm` after the transition to `CiFailed`.
    pub(super) async fn check_ci_failed(&self, session: &Session, pr: &PullRequest) -> Result<()> {
        let Some(engine) = self.reaction_engine.as_ref() else {
            return Ok(());
        };
        let Some(scm) = self.scm.as_ref() else {
            return Ok(());
        };

        let checks = match scm.ci_checks(pr).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    session = %session.id,
                    error = %e,
                    "ci_checks failed; using generic ci-failed message"
                );
                // Fall back to generic dispatch without detail.
                engine.dispatch(session, "ci-failed").await?;
                return Ok(());
            }
        };

        let failed: Vec<_> = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Failed)
            .collect();

        let msg = if failed.is_empty() {
            // No individual failures returned (e.g. provider didn't
            // populate per-check data) — fall through to the static YAML
            // message via normal dispatch.
            engine.dispatch(session, "ci-failed").await?;
            return Ok(());
        } else {
            let mut s = String::from("CI failed. Failing checks:\n");
            for check in &failed {
                if let Some(ref url) = check.url {
                    s.push_str(&format!("- {} — {}\n", check.name, url));
                } else {
                    s.push_str(&format!("- {}\n", check.name));
                }
            }
            s.push_str("\nFix the failures, push, and wait for CI to re-run.");
            s
        };

        engine
            .dispatch_with_message(session, "ci-failed", msg)
            .await?;

        Ok(())
    }

    /// Port of `maybeDispatchAutomatedReview` from the TS reference
    /// (`lifecycle-manager.ts:1487-1532`).
    ///
    /// Fetches the current `automated_comments` set from the SCM plugin,
    /// fingerprints the result, and dispatches a detailed `bugbot-comments`
    /// reaction when the set has changed since the last dispatch. Same-set
    /// ticks are silent (de-dup via `last_automated_review_dispatch_hash`).
    ///
    /// When the fingerprint changes from the previous tick the reaction
    /// tracker is cleared so a fresh attempt counter starts. This mirrors
    /// TS line 1493–1495.
    pub(super) async fn check_bugbot_comments(
        &self,
        session: &mut Session,
        pr: &PullRequest,
    ) -> Result<()> {
        let Some(engine) = self.reaction_engine.as_ref() else {
            return Ok(());
        };
        let Some(scm) = self.scm.as_ref() else {
            return Ok(());
        };

        let comments = match scm.automated_comments(pr).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    session = %session.id,
                    error = %e,
                    "automated_comments failed; skipping bugbot check"
                );
                return Ok(());
            }
        };

        let fingerprint = fingerprint_automated_comments(&comments);

        // When the fingerprint has changed, clear the tracker so the next
        // dispatch starts with a fresh attempt count (mirrors TS 1493-1495).
        if session.last_automated_review_fingerprint != Some(fingerprint) {
            engine.clear_tracker(&session.id, "bugbot-comments");
            session.last_automated_review_fingerprint = Some(fingerprint);
        }

        // No bot comments — nothing to dispatch; reset dispatch hash.
        if comments.is_empty() {
            if session.last_automated_review_dispatch_hash.is_some() {
                session.last_automated_review_dispatch_hash = None;
                self.sessions.save(session).await?;
            }
            return Ok(());
        }

        // Already dispatched this exact set — skip.
        if session.last_automated_review_dispatch_hash == Some(fingerprint) {
            return Ok(());
        }

        let msg = format_automated_comments_message(&comments);

        engine
            .dispatch_with_message(session, "bugbot-comments", msg)
            .await?;

        session.last_automated_review_dispatch_hash = Some(fingerprint);
        self.sessions.save(session).await?;

        Ok(())
    }
}

/// Stable hash fingerprint of an `AutomatedComment` slice.
///
/// Sorts by `(id, bot_name, url)` for determinism, then folds through
/// `DefaultHasher`. Used by `check_bugbot_comments` to detect when the
/// bot-comment set has changed between ticks.
pub(super) fn fingerprint_automated_comments(comments: &[crate::scm::AutomatedComment]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut keys: Vec<(&str, &str, &str)> = comments
        .iter()
        .map(|c| (c.id.as_str(), c.bot_name.as_str(), c.url.as_str()))
        .collect();
    keys.sort_unstable();
    let mut h = DefaultHasher::new();
    keys.hash(&mut h);
    h.finish()
}

/// Format a detailed dispatch message from a slice of `AutomatedComment`s.
///
/// Ports `formatAutomatedCommentsMessage` from
/// `packages/core/src/format-automated-comments.ts` (PR #1334). The
/// message includes:
/// - A preamble warning that the data came from a previous API call
/// - Per-comment: severity label, `path:line`, bot name, excerpt, URL
/// - Explicit API guidance for paginated re-fetching so the agent doesn't
///   rely on a stale first-page scan
pub(super) fn format_automated_comments_message(
    comments: &[crate::scm::AutomatedComment],
) -> String {
    use crate::scm::AutomatedCommentSeverity;

    let mut msg = String::from(
        "Automated bot review comments on your PR \
         (fetched at reaction time — verify via API before acting):\n\n",
    );

    for (i, c) in comments.iter().enumerate() {
        let severity = match c.severity {
            AutomatedCommentSeverity::Error => "ERROR",
            AutomatedCommentSeverity::Warning => "WARNING",
            AutomatedCommentSeverity::Info => "INFO",
        };
        let location = match (&c.path, c.line) {
            (Some(p), Some(l)) => format!(" {}:{}", p, l),
            (Some(p), None) => format!(" {}", p),
            _ => String::new(),
        };
        // Truncate long bodies to keep the message readable.
        let excerpt: String = c.body.chars().take(200).collect();
        let ellipsis = if c.body.len() > 200 { "…" } else { "" };

        msg.push_str(&format!(
            "{}. [{}] {}{}\n   {}{}\n   {}\n\n",
            i + 1,
            severity,
            c.bot_name,
            location,
            excerpt,
            ellipsis,
            c.url,
        ));
    }

    msg.push_str(
        "To verify this data is current and complete, use the GitHub API directly:\n\
         - GET /repos/{owner}/{repo}/pulls/{pr}/reviews\n\
         - GET /repos/{owner}/{repo}/pulls/{pr}/reviews/{review_id}/comments\n\
         - GET /repos/{owner}/{repo}/pulls/{pr}/comments?per_page=100&page=N \
           (paginate; check in_reply_to_id for reply threads)\n\
         Fix each issue, push, and confirm the bot re-checks pass.",
    );

    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::tests::{
        fake_pr, fake_session, recv_timeout, script_ready_pr, setup_with_scm,
        setup_with_scm_and_auto_merge_engine, MockAgent, MockRuntime, MockScm,
    };
    use crate::reactions::ReactionAction;
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;

    // ---------- SCM polling integration (Phase F) ---------- //

    #[tokio::test]
    async fn scm_poll_with_no_pr_leaves_working_untouched() {
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-no-pr").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        assert_eq!(
            scm.detect_calls.load(Ordering::SeqCst),
            1,
            "detect_pr should be called exactly once"
        );
        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_opens_pr_transitions_working_to_pr_open() {
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-open").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::None);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::PrOpen,
                    ..
                }
            )),
            "expected Working → PrOpen, got {events:?}"
        );

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::PrOpen);

        assert_eq!(
            scm.detect_calls.load(Ordering::SeqCst),
            1,
            "expected exactly one detect_pr call per tick"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_ci_failing_transitions_pr_open_to_ci_failed() {
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-ci-fail").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::Pending);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::CiFailed);
        assert_eq!(
            scm.detect_calls.load(Ordering::SeqCst),
            1,
            "expected exactly one detect_pr call per tick"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_full_green_transitions_through_to_mergeable() {
        use crate::scm::{CiStatus, MergeReadiness, PrState, ReviewDecision};
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-all-green").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Passing);
        scm.set_review(ReviewDecision::Approved);
        scm.set_readiness(MergeReadiness {
            mergeable: true,
            ci_passing: true,
            approved: true,
            no_conflicts: true,
            blockers: vec![],
        });

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Mergeable);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_pr_disappears_drops_pr_track_back_to_working() {
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-pr-gone").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(None);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_detect_pr_error_emits_tick_error_and_skips() {
        let (lifecycle, sessions, scm, base) = setup_with_scm("scm-detect-err").await;
        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.detect_pr_errors.store(true, Ordering::SeqCst);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut saw_tick_error = false;
        while let Some(e) = recv_timeout(&mut rx).await {
            if let OrchestratorEvent::TickError { message, .. } = e {
                if message.contains("detect_pr") {
                    saw_tick_error = true;
                }
            }
        }
        assert!(saw_tick_error, "expected TickError from scm.detect_pr");

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_field_probe_error_emits_tick_error_and_skips_transition() {
        struct Case {
            label: &'static str,
            toggle: fn(&MockScm),
            expected_slot: &'static str,
        }
        let cases = [
            Case {
                label: "pr_state",
                toggle: |s| s.pr_state_errors.store(true, Ordering::SeqCst),
                expected_slot: "pr_state",
            },
            Case {
                label: "ci_status",
                toggle: |s| s.ci_status_errors.store(true, Ordering::SeqCst),
                expected_slot: "ci_status",
            },
            Case {
                label: "review_decision",
                toggle: |s| s.review_decision_errors.store(true, Ordering::SeqCst),
                expected_slot: "review_decision",
            },
            Case {
                label: "mergeability",
                toggle: |s| s.mergeability_errors.store(true, Ordering::SeqCst),
                expected_slot: "mergeability",
            },
        ];

        for case in cases {
            let (lifecycle, sessions, scm, base) =
                setup_with_scm(&format!("scm-field-err-{}", case.label)).await;
            let mut s = fake_session("s1", "demo");
            s.status = SessionStatus::Working;
            sessions.save(&s).await.unwrap();

            scm.set_pr(Some(fake_pr(42, "ao-s1")));
            (case.toggle)(&scm);

            let mut rx = lifecycle.subscribe();
            let mut seen = HashSet::new();
            lifecycle.tick(&mut seen).await.unwrap();

            let mut saw_probe_error = false;
            while let Some(e) = recv_timeout(&mut rx).await {
                if let OrchestratorEvent::TickError { message, .. } = e {
                    if message.contains(case.expected_slot) {
                        saw_probe_error = true;
                    }
                }
            }
            assert!(
                saw_probe_error,
                "expected TickError mentioning {} for case {}",
                case.expected_slot, case.label
            );

            let persisted = sessions.list().await.unwrap();
            assert_eq!(persisted[0].status, SessionStatus::Working);

            let _ = std::fs::remove_dir_all(&base);
        }
    }

    #[tokio::test]
    async fn scm_poll_is_off_when_scm_is_not_configured() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("scm-absent");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let lifecycle = Arc::new(LifecycleManager::new(sessions.clone(), runtime, agent));

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].status, SessionStatus::Working);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn scm_poll_fires_reaction_when_transitioning_into_ci_failed() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::ReactionConfig;
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("scm-reaction");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);

        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("CI broke, please fix".into());
        let mut map = std::collections::HashMap::new();
        map.insert("ci-failed".into(), cfg);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));

        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::Pending);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }

        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::CiFailed,
                    ..
                }
            )),
            "expected StatusChanged to CiFailed, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::ReactionTriggered {
                    action: ReactionAction::SendToAgent,
                    ..
                }
            )),
            "expected ReactionTriggered(SendToAgent), got {events:?}"
        );
        let sends = engine_runtime.sends();
        assert_eq!(sends.len(), 1, "expected exactly one send, got {sends:?}");
        assert_eq!(sends[0].1, "CI broke, please fix");

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- MergeFailed parking loop (Phase G) ---------- //

    #[tokio::test]
    async fn auto_merge_failure_parks_in_merge_failed_then_retries_next_tick() {
        let (lifecycle, sessions, scm, engine, base) =
            setup_with_scm_and_auto_merge_engine("park-retry", Some(5)).await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        script_ready_pr(&scm, 42);
        scm.merge_errors.store(true, Ordering::SeqCst);

        let mut rx = lifecycle.subscribe();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].status,
            SessionStatus::MergeFailed,
            "tick 1 must park in MergeFailed after merge failure"
        );
        assert_eq!(
            engine.attempts(&s.id, "approved-and-green"),
            1,
            "tracker must increment on the failed merge"
        );
        assert_eq!(scm.merges().len(), 0, "failed merge does not record");

        scm.merge_errors.store(false, Ordering::SeqCst);

        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].status,
            SessionStatus::Mergeable,
            "tick 2 must re-promote and stay in Mergeable after successful merge"
        );
        assert_eq!(
            engine.attempts(&s.id, "approved-and-green"),
            2,
            "tracker must accumulate across the parking loop"
        );
        assert_eq!(scm.merges().len(), 1, "second attempt must actually merge");
        assert_eq!(scm.merges()[0], (42, None));

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }
        let park_seen = events.iter().any(|e| {
            matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::MergeFailed,
                    ..
                }
            )
        });
        let repromote_seen = events.iter().any(|e| {
            matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::MergeFailed,
                    to: SessionStatus::Mergeable,
                    ..
                }
            )
        });
        assert!(park_seen, "expected park event, got {events:?}");
        assert!(repromote_seen, "expected re-promote event, got {events:?}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn persistent_merge_failure_escalates_after_retries_exhausted() {
        let (lifecycle, sessions, scm, engine, base) =
            setup_with_scm_and_auto_merge_engine("park-escalate", Some(2)).await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        script_ready_pr(&scm, 42);
        scm.merge_errors.store(true, Ordering::SeqCst);

        let mut rx = lifecycle.subscribe();

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine.attempts(&s.id, "approved-and-green"), 1);
        assert_eq!(
            sessions.list().await.unwrap()[0].status,
            SessionStatus::MergeFailed
        );

        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine.attempts(&s.id, "approved-and-green"), 2);
        assert_eq!(
            sessions.list().await.unwrap()[0].status,
            SessionStatus::MergeFailed
        );

        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine.attempts(&s.id, "approved-and-green"), 3);

        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].status,
            SessionStatus::Mergeable,
            "after escalation, session must stay in Mergeable (not parked)"
        );
        assert_eq!(
            scm.merges().len(),
            0,
            "both failed merges are rejected by the mock; no successful records"
        );

        let mut events = Vec::new();
        while let Some(e) = recv_timeout(&mut rx).await {
            events.push(e);
        }
        let escalated_count = events
            .iter()
            .filter(|e| matches!(e, OrchestratorEvent::ReactionEscalated { .. }))
            .count();
        assert_eq!(
            escalated_count, 1,
            "expected exactly one ReactionEscalated event, got {events:?}"
        );

        let attempts_before_tick4 = engine.attempts(&s.id, "approved-and-green");
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            engine.attempts(&s.id, "approved-and-green"),
            attempts_before_tick4,
            "tick 4 must not increment attempts — session is frozen"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_failed_exit_to_ci_failed_clears_approved_and_green_tracker() {
        use crate::scm::{CiStatus, MergeReadiness};
        let (lifecycle, sessions, scm, engine, base) =
            setup_with_scm_and_auto_merge_engine("park-exit-clears", Some(5)).await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        script_ready_pr(&scm, 42);
        scm.merge_errors.store(true, Ordering::SeqCst);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            sessions.list().await.unwrap()[0].status,
            SessionStatus::MergeFailed
        );
        assert_eq!(engine.attempts(&s.id, "approved-and-green"), 1);

        scm.set_ci(CiStatus::Failing);
        scm.set_readiness(MergeReadiness {
            mergeable: false,
            ci_passing: false,
            approved: true,
            no_conflicts: true,
            blockers: vec!["CI is failing".into()],
        });

        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            sessions.list().await.unwrap()[0].status,
            SessionStatus::CiFailed
        );
        assert_eq!(
            engine.attempts(&s.id, "approved-and-green"),
            0,
            "approved-and-green tracker must be cleared on exit from MergeFailed"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_failed_drops_back_to_working_when_pr_disappears() {
        let (lifecycle, sessions, scm, _engine, base) =
            setup_with_scm_and_auto_merge_engine("park-pr-gone", Some(5)).await;

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::MergeFailed;
        sessions.save(&s).await.unwrap();

        scm.set_pr(None);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let persisted = sessions.list().await.unwrap();
        assert_eq!(
            persisted[0].status,
            SessionStatus::Working,
            "MergeFailed must be on the PR track so detect_pr(None) drops to Working"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stuck_overrides_pr_open_in_same_tick_when_idle_beyond_threshold() {
        use crate::lifecycle::tests::{fake_pr, rewind_idle_since, unique_temp_dir};
        use crate::reactions::ReactionConfig;
        use crate::scm::{CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;
        let base = unique_temp_dir("stuck_overrides_pr_open");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Idle));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), runtime, agent);

        let mut stuck_cfg = ReactionConfig::new(ReactionAction::Notify);
        stuck_cfg.threshold = Some("1s".into());
        let mut map = std::collections::HashMap::new();
        map.insert("agent-stuck".into(), stuck_cfg);
        let engine_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime,
            lifecycle.events_sender(),
        ));

        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine)
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        rewind_idle_since(&lifecycle, &s.id, Duration::from_secs(2));

        scm.set_pr(Some(fake_pr(42, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::None);

        let mut rx = lifecycle.subscribe();
        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let events: Vec<_> = {
            let mut v = Vec::new();
            while let Some(e) = recv_timeout(&mut rx).await {
                v.push(e);
            }
            v
        };
        assert!(
            events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    from: SessionStatus::Working,
                    to: SessionStatus::Stuck,
                    ..
                }
            )),
            "expected Working → Stuck transition, got {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                OrchestratorEvent::StatusChanged {
                    to: SessionStatus::PrOpen,
                    ..
                }
            )),
            "must not emit an intermediate PrOpen transition: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Issue #195 H3: CI-failed message includes check names ---------- //

    #[tokio::test]
    async fn ci_failed_message_includes_check_names() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::ReactionConfig;
        use crate::scm::{CheckRun, CheckStatus, CiStatus, PrState, ReviewDecision};
        use crate::session_manager::SessionManager;

        let base = unique_temp_dir("ci-detail");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("CI failed (generic)".into());
        let mut map = std::collections::HashMap::new();
        map.insert("ci-failed".into(), cfg);
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        scm.set_ci_checks(vec![
            CheckRun {
                name: "unit-tests".into(),
                status: CheckStatus::Failed,
                url: Some("https://ci.example.com/run/1".into()),
                conclusion: Some("failure".into()),
            },
            CheckRun {
                name: "lint".into(),
                status: CheckStatus::Failed,
                url: None,
                conclusion: Some("failure".into()),
            },
        ]);

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::Working;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(10, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Failing);
        scm.set_review(ReviewDecision::None);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let sends = engine_runtime.sends();
        assert_eq!(
            sends.len(),
            1,
            "expected exactly one ci-failed send, got {sends:?}"
        );
        let msg = &sends[0].1;
        assert!(
            msg.contains("unit-tests"),
            "message must include check name 'unit-tests', got: {msg}"
        );
        assert!(
            msg.contains("https://ci.example.com/run/1"),
            "message must include check URL, got: {msg}"
        );
        assert!(
            msg.contains("lint"),
            "message must include check name 'lint', got: {msg}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Issue #195 H2: review backlog dispatch + de-dup ---------- //

    #[tokio::test]
    async fn review_backlog_dispatches_once_on_new_comments() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::ReactionConfig;
        use crate::scm::{CiStatus, PrState, ReviewComment, ReviewDecision};
        use crate::session_manager::SessionManager;

        let base = unique_temp_dir("review-backlog-new");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("Address review comments".into());
        let mut map = std::collections::HashMap::new();
        map.insert("changes-requested".into(), cfg);
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::ChangesRequested;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(20, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::ChangesRequested);

        scm.set_pending_comments(vec![
            ReviewComment {
                id: "c1".into(),
                author: "alice".into(),
                body: "Please fix the indentation".into(),
                path: Some("src/main.rs".into()),
                line: Some(42),
                is_resolved: false,
                url: "https://github.com/a/b/pull/20#comment-1".into(),
            },
            ReviewComment {
                id: "c2".into(),
                author: "bob".into(),
                body: "Add a test for this".into(),
                path: None,
                line: None,
                is_resolved: false,
                url: "https://github.com/a/b/pull/20#comment-2".into(),
            },
        ]);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let sends = engine_runtime.sends();
        assert_eq!(
            sends.len(),
            1,
            "expected exactly 1 send for new comments, got {sends:?}"
        );
        let msg = &sends[0].1;
        assert!(
            msg.contains("alice") || msg.contains("New review"),
            "message should mention the comment author or be a review summary"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn review_backlog_no_redispatch_on_same_comments() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::ReactionConfig;
        use crate::scm::{CiStatus, PrState, ReviewComment, ReviewDecision};
        use crate::session_manager::SessionManager;

        let base = unique_temp_dir("review-backlog-dedup");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("Address review comments".into());
        let mut map = std::collections::HashMap::new();
        map.insert("changes-requested".into(), cfg);
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::ChangesRequested;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(21, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::ChangesRequested);

        let comments = vec![ReviewComment {
            id: "c1".into(),
            author: "alice".into(),
            body: "Fix this".into(),
            path: None,
            line: None,
            is_resolved: false,
            url: "https://github.com/a/b/pull/21#comment-1".into(),
        }];
        scm.set_pending_comments(comments.clone());

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(engine_runtime.sends().len(), 1, "tick 1 should dispatch");

        {
            let mut map = lifecycle.last_review_backlog_check.lock().unwrap();
            map.clear();
        }
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            engine_runtime.sends().len(),
            1,
            "tick 2 with same comments must NOT dispatch again"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---------- Issue #212: bugbot comments dispatch + de-dup ---------- //

    #[tokio::test]
    async fn bugbot_comments_dispatches_on_new_bot_comments() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::ReactionConfig;
        use crate::scm::{
            AutomatedComment, AutomatedCommentSeverity, CiStatus, PrState, ReviewDecision,
        };
        use crate::session_manager::SessionManager;

        let base = unique_temp_dir("bugbot-new");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("Fix bot comments".into());
        let mut map = std::collections::HashMap::new();
        map.insert("bugbot-comments".into(), cfg);
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(30, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::None);
        scm.set_automated_comments(vec![AutomatedComment {
            id: "bot-c1".into(),
            bot_name: "sonarcloud[bot]".into(),
            body: "Potential null pointer dereference".into(),
            path: Some("src/main.rs".into()),
            line: Some(42),
            severity: AutomatedCommentSeverity::Error,
            url: "https://github.com/a/b/pull/30#pullrequestreviewcomment-1".into(),
        }]);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();

        let sends = engine_runtime.sends();
        assert_eq!(
            sends.len(),
            1,
            "expected exactly 1 bugbot send, got {sends:?}"
        );
        let msg = &sends[0].1;
        assert!(
            msg.contains("sonarcloud[bot]"),
            "message must include bot name"
        );
        assert!(
            msg.contains("src/main.rs:42"),
            "message must include path:line"
        );
        assert!(msg.contains("ERROR"), "message must include severity");
        assert!(
            msg.contains("Potential null pointer"),
            "message must include excerpt"
        );
        assert!(msg.contains("reviews"), "message must include API guidance");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn bugbot_comments_no_redispatch_on_same_comment_set() {
        use crate::lifecycle::tests::unique_temp_dir;
        use crate::reactions::ReactionConfig;
        use crate::scm::{
            AutomatedComment, AutomatedCommentSeverity, CiStatus, PrState, ReviewDecision,
        };
        use crate::session_manager::SessionManager;

        let base = unique_temp_dir("bugbot-dedup");
        let sessions = Arc::new(SessionManager::new(base.clone()));
        let lifecycle_runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new(true));
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new(ActivityState::Ready));
        let scm = Arc::new(MockScm::new());

        let lifecycle = LifecycleManager::new(sessions.clone(), lifecycle_runtime, agent);
        let engine_runtime = Arc::new(MockRuntime::new(true));
        let mut cfg = ReactionConfig::new(ReactionAction::SendToAgent);
        cfg.message = Some("Fix bot comments".into());
        let mut map = std::collections::HashMap::new();
        map.insert("bugbot-comments".into(), cfg);
        let engine = Arc::new(ReactionEngine::new(
            map,
            engine_runtime.clone() as Arc<dyn Runtime>,
            lifecycle.events_sender(),
        ));
        let lifecycle = Arc::new(
            lifecycle
                .with_reaction_engine(engine.clone())
                .with_scm(scm.clone() as Arc<dyn Scm>),
        );

        let mut s = fake_session("s1", "demo");
        s.status = SessionStatus::PrOpen;
        sessions.save(&s).await.unwrap();

        scm.set_pr(Some(fake_pr(31, "ao-s1")));
        scm.set_state(PrState::Open);
        scm.set_ci(CiStatus::Pending);
        scm.set_review(ReviewDecision::None);
        scm.set_automated_comments(vec![AutomatedComment {
            id: "bot-c2".into(),
            bot_name: "codecov[bot]".into(),
            body: "Coverage dropped below threshold".into(),
            path: None,
            line: None,
            severity: AutomatedCommentSeverity::Warning,
            url: "https://github.com/a/b/pull/31#pullrequestreviewcomment-2".into(),
        }]);

        let mut seen = HashSet::new();
        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            engine_runtime.sends().len(),
            1,
            "tick 1 should dispatch once"
        );

        lifecycle.tick(&mut seen).await.unwrap();
        assert_eq!(
            engine_runtime.sends().len(),
            1,
            "tick 2 with same comments must NOT dispatch again"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn format_automated_comments_message_includes_all_fields() {
        use crate::scm::{AutomatedComment, AutomatedCommentSeverity};

        let comments = vec![
            AutomatedComment {
                id: "1".into(),
                bot_name: "sonarcloud[bot]".into(),
                body: "Null check missing".into(),
                path: Some("src/lib.rs".into()),
                line: Some(10),
                severity: AutomatedCommentSeverity::Error,
                url: "https://example.com/c/1".into(),
            },
            AutomatedComment {
                id: "2".into(),
                bot_name: "codecov[bot]".into(),
                body: "Coverage low".into(),
                path: None,
                line: None,
                severity: AutomatedCommentSeverity::Warning,
                url: "https://example.com/c/2".into(),
            },
        ];

        let msg = format_automated_comments_message(&comments);

        assert!(msg.contains("ERROR"), "must include ERROR severity");
        assert!(msg.contains("WARNING"), "must include WARNING severity");
        assert!(msg.contains("src/lib.rs:10"), "must include path:line");
        assert!(msg.contains("sonarcloud[bot]"), "must include bot name");
        assert!(
            msg.contains("Null check missing"),
            "must include body excerpt"
        );
        assert!(msg.contains("https://example.com/c/1"), "must include URL");
        assert!(
            msg.contains("per_page=100"),
            "must include pagination API hint"
        );
        assert!(
            msg.contains("in_reply_to_id"),
            "must include reply thread hint"
        );
    }
}
