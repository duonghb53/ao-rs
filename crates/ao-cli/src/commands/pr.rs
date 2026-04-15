//! `ao-rs pr` — PR / CI / review summary for a session.

use ao_core::{
    CiStatus, MergeReadiness, PrState, PullRequest, ReviewDecision, Scm, Session, SessionManager,
};

use crate::cli::auto_scm::AutoScm;
use crate::cli::printing::short_id;

pub async fn pr(session_id_or_prefix: String) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = SessionManager::with_default();
    let session = sessions.find_by_prefix(&session_id_or_prefix).await?;

    let auto = AutoScm::new();
    let scm: &dyn Scm = &auto;
    let Some(pr) = scm.detect_pr(&session).await? else {
        println!(
            "no PR found for session {} (branch {})",
            session.id, session.branch
        );
        return Ok(());
    };

    // Everything downstream is independent — fan out concurrently so `ao-rs
    // pr` doesn't pay 4× RTT. `mergeability` internally re-calls `pr_state`
    // and `ci_status`, so the total gh invocation count is ~7, not 4.
    // Accepted duplication — see the handler doc comment for rationale.
    let (state, ci, decision, readiness) = tokio::join!(
        scm.pr_state(&pr),
        scm.ci_status(&pr),
        scm.review_decision(&pr),
        scm.mergeability(&pr),
    );

    let report = format_pr_report(&session, &pr, state?, ci?, decision?, &readiness?);
    print!("{report}");
    Ok(())
}

/// Pretty-print a full PR report. Pulled out as a pure function — takes
/// everything already-fetched — so tests can exercise the blocker-list
/// formatting without shelling out to `gh`.
pub(crate) fn format_pr_report(
    session: &Session,
    pr: &PullRequest,
    state: PrState,
    ci: CiStatus,
    decision: ReviewDecision,
    readiness: &MergeReadiness,
) -> String {
    let mut out = String::new();
    out.push_str("───────────────────────────────────────────────\n");
    out.push_str(&format!(
        "  session: {} (short {})\n",
        session.id,
        short_id(&session.id)
    ));
    out.push_str(&format!("  branch:  {}\n", session.branch));
    out.push_str(&format!("  PR:      #{} {}\n", pr.number, pr.title));
    out.push_str(&format!("  url:     {}\n", pr.url));
    out.push('\n');
    out.push_str(&format!("  state:   {}\n", pr_state_label(state)));
    out.push_str(&format!("  CI:      {}\n", ci_status_label(ci)));
    out.push_str(&format!("  review:  {}\n", review_decision_label(decision)));
    out.push('\n');
    out.push_str(&format!(
        "  mergeable: {}\n",
        if readiness.is_ready() { "yes" } else { "no" }
    ));
    if !readiness.blockers.is_empty() {
        out.push_str("  blockers:\n");
        for b in &readiness.blockers {
            out.push_str(&format!("    - {b}\n"));
        }
    }
    out.push_str("───────────────────────────────────────────────\n");
    out
}

pub(crate) fn pr_state_label(s: PrState) -> &'static str {
    match s {
        PrState::Open => "open",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    }
}

pub(crate) fn ci_status_label(s: CiStatus) -> &'static str {
    match s {
        CiStatus::Pending => "pending",
        CiStatus::Passing => "passing",
        CiStatus::Failing => "failing",
        CiStatus::None => "none",
    }
}

pub(crate) fn review_decision_label(d: ReviewDecision) -> &'static str {
    match d {
        ReviewDecision::Approved => "approved",
        ReviewDecision::ChangesRequested => "changes_requested",
        ReviewDecision::Pending => "pending",
        ReviewDecision::None => "none",
    }
}
