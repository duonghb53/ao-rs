//! Shared payload types for dashboard/desktop UI.
//!
//! Lives in `ao-core` (not `ao-dashboard`) so the lifecycle loop can build
//! the same `DashboardPr` shape it pushes via SSE deltas, and so the SSE
//! event variant in `events.rs` can name the type.
//!
//! Two kinds of types here:
//!
//! - `BatchedPrEnrichment` — the PR-enrichment data the lifecycle loop
//!   caches per tick. Wraps `ScmObservation` (state+ci+review+readiness)
//!   plus the extra fields the dashboard wants (`additions`, `deletions`,
//!   `ci_checks`). Returned by `Scm::enrich_prs_full`.
//! - `DashboardPr` / `DashboardSession` — the JSON shapes served by
//!   `GET /api/sessions?pr=true` and pushed via the `pr_enrichment_changed`
//!   SSE event. Built from `BatchedPrEnrichment` + `PullRequest` + `Session`.

use serde::Serialize;

use crate::{
    scm::{CheckRun, CheckStatus, CiStatus, PrState, PullRequest, ReviewDecision},
    scm_transitions::ScmObservation,
    types::{Session, SessionStatus},
};

/// Per-PR enrichment data cached by the lifecycle loop and returned by
/// `Scm::enrich_prs_full`. Superset of `ScmObservation` — adds the
/// dashboard-only fields (`additions`, `deletions`, `ci_checks`) that the
/// reaction engine doesn't read.
///
/// `PartialEq` lets the lifecycle diff against the previous tick's
/// enrichment and only emit `PrEnrichmentChanged` when something actually
/// changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchedPrEnrichment {
    pub observation: ScmObservation,
    pub additions: u32,
    pub deletions: u32,
    pub ci_checks: Vec<CheckRun>,
}

/// PR fields rendered in the dashboard session card. Built by combining
/// `PullRequest` (from `detect_pr`) with `BatchedPrEnrichment` (from
/// `enrich_prs_full`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardPr {
    pub number: u32,
    pub url: String,
    pub title: String,
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub base_branch: String,
    pub is_draft: bool,

    pub state: PrState,
    pub ci_status: CiStatus,
    pub review_decision: ReviewDecision,
    pub mergeable: bool,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    #[serde(default)]
    pub failing_checks: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing_check_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ci_checks: Vec<CheckRun>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
}

/// Top-level row served by `GET /api/sessions?pr=true`. Wraps a `Session`
/// with optional PR enrichment + the derived `attention_level` bucket.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardSession {
    #[serde(flatten)]
    pub session: Session,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<DashboardPr>,
    pub attention_level: String,
}

impl DashboardPr {
    /// Build a `DashboardPr` from a freshly-detected `PullRequest` plus
    /// the batch enrichment for that PR. Mirrors the per-PR fan-out the
    /// dashboard used to do before Layer 1 of the rate-limit fix.
    pub fn from_enrichment(pr: &PullRequest, enrichment: &BatchedPrEnrichment) -> Self {
        let mut failing_check_names: Vec<String> = enrichment
            .ci_checks
            .iter()
            .filter(|c| c.status == CheckStatus::Failed)
            .map(|c| c.name.clone())
            .collect();
        failing_check_names.sort();
        let failing_checks = failing_check_names.len() as u32;
        failing_check_names.truncate(5);
        Self {
            number: pr.number,
            url: pr.url.clone(),
            title: pr.title.clone(),
            owner: pr.owner.clone(),
            repo: pr.repo.clone(),
            branch: pr.branch.clone(),
            base_branch: pr.base_branch.clone(),
            is_draft: pr.is_draft,
            state: enrichment.observation.state,
            ci_status: enrichment.observation.ci,
            review_decision: enrichment.observation.review,
            mergeable: enrichment.observation.readiness.mergeable,
            additions: enrichment.additions,
            deletions: enrichment.deletions,
            failing_checks,
            failing_check_names,
            ci_checks: enrichment.ci_checks.clone(),
            blockers: enrichment.observation.readiness.blockers.clone(),
        }
    }
}

/// Bucket a session into one of the dashboard board columns
/// (`working`/`pending`/`review`/`respond`/`merge`/`done`). Pure function
/// so dashboard HTTP handlers, SSE delta builders, and UI tests share one
/// implementation.
pub fn attention_level(session: &Session, pr: Option<&DashboardPr>) -> String {
    if session.is_terminal() {
        return "done".into();
    }

    if matches!(
        session.status,
        SessionStatus::Mergeable | SessionStatus::MergeFailed | SessionStatus::Approved
    ) {
        return "merge".into();
    }

    if let Some(pr) = pr {
        if pr.state == PrState::Open && pr.mergeable && pr.ci_status == CiStatus::Passing {
            return "merge".into();
        }
        if pr.review_decision == ReviewDecision::ChangesRequested
            || pr.ci_status == CiStatus::Failing
        {
            return "respond".into();
        }
        if pr.review_decision == ReviewDecision::Pending {
            return "review".into();
        }
        if pr.ci_status == CiStatus::Pending {
            return "pending".into();
        }
        if pr.state == PrState::Open {
            return "review".into();
        }
    }

    match session.status {
        SessionStatus::PrOpen | SessionStatus::ReviewPending | SessionStatus::Approved => {
            "review".into()
        }
        SessionStatus::CiFailed | SessionStatus::ChangesRequested => "respond".into(),
        SessionStatus::Mergeable | SessionStatus::MergeFailed => "merge".into(),
        SessionStatus::NeedsInput | SessionStatus::Stuck => "respond".into(),
        _ => "working".into(),
    }
}
