//! `ao-rs status` — list sessions with optional PR column.

use ao_core::{CiStatus, PrState, PullRequest, Scm, Session, SessionManager};

use crate::cli::auto_scm::AutoScm;
use crate::cli::printing::{session_display_title, truncate};
use crate::commands::pr::{ci_status_label, pr_state_label};

pub async fn status(
    project_filter: Option<String>,
    with_pr: bool,
    with_cost: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let manager = SessionManager::with_default();
    let sessions = match &project_filter {
        Some(p) => manager.list_for_project(p).await?,
        None => manager.list().await?,
    };

    if sessions.is_empty() {
        match project_filter {
            Some(p) => println!("no sessions in project '{p}'"),
            None => println!("no sessions"),
        }
        return Ok(());
    }

    // Columns wide enough for the longest status (`changes_requested` = 17
    // chars) and the longest activity (`waiting_input` = 13 chars). Trying
    // to autosize is not worth it for a tool that prints ~10 rows max.
    //
    // Header and row formatting adapt to the --pr and --cost flags.
    let cost_hdr = if with_cost {
        format!("{:<10} ", "COST")
    } else {
        String::new()
    };
    if with_pr {
        println!(
            "{:<10} {:<14} {:<18} {:<14} {:<18} {:<24} {}TASK",
            "ID", "PROJECT", "STATUS", "ACTIVITY", "BRANCH", "PR", cost_hdr
        );
    } else {
        println!(
            "{:<10} {:<14} {:<18} {:<14} {:<18} {}TASK",
            "ID", "PROJECT", "STATUS", "ACTIVITY", "BRANCH", cost_hdr
        );
    }

    // Build the SCM plugin once up front if `--pr` is on, rather than
    // per-row. `AutoScm` delegates based on the detected PR URL shape.
    let scm = if with_pr { Some(AutoScm::new()) } else { None };

    for s in sessions {
        let short_id: String = s.id.0.chars().take(8).collect();
        let title = session_display_title(&s);
        let task = truncate(&title, 60);
        let activity = s
            .activity
            .map(|a| a.as_str().to_string())
            .unwrap_or_else(|| "-".to_string());
        let cost_cell = if with_cost {
            format!(
                "{:<10} ",
                s.cost
                    .as_ref()
                    .map(|c| format!("${:.2}", c.cost_usd))
                    .unwrap_or_else(|| "-".to_string())
            )
        } else {
            String::new()
        };

        if let Some(scm) = scm.as_ref() {
            let pr_cell = fetch_pr_column(scm, &s).await;
            println!(
                "{:<10} {:<14} {:<18} {:<14} {:<18} {:<24} {}{}",
                short_id,
                s.project_id,
                s.status.as_str(),
                activity,
                s.branch,
                pr_cell,
                cost_cell,
                task,
            );
        } else {
            println!(
                "{:<10} {:<14} {:<18} {:<14} {:<18} {}{}",
                short_id,
                s.project_id,
                s.status.as_str(),
                activity,
                s.branch,
                cost_cell,
                task,
            );
        }
    }
    Ok(())
}

/// Best-effort PR column for `ao-rs status --pr`.
///
/// Two failure tiers:
/// - `detect_pr` failure (or `Ok(None)`) → `-`, i.e. "this row has no PR
///   as far as we can tell". Mirrors the `detect_pr` tolerant contract.
/// - Post-detect failure (`pr_state`/`ci_status` err) → `pr_column`
///   renders `?` for the missing half, so the row still shows `#N ?/?`
///   or `#N open/?`. That's distinct from `-` on purpose: "there's a PR
///   here, we just couldn't read all of it this tick".
pub(crate) async fn fetch_pr_column(scm: &dyn Scm, session: &Session) -> String {
    let Ok(Some(pr)) = scm.detect_pr(session).await else {
        return "-".to_string();
    };
    // `pr_state` and `ci_status` are independent — run them concurrently
    // so `--pr` doesn't pay 2× RTT per session. Both results feed the
    // pure formatter so the column shape is testable.
    let (state, ci) = tokio::join!(scm.pr_state(&pr), scm.ci_status(&pr));
    pr_column(Some(&pr), state.ok(), ci.ok())
}

/// Compact PR column cell. Pulled out as a pure function so the width
/// and shape can be unit-tested without shelling out to `gh`.
///
/// Format:
///   `-`                 — no PR (or any upstream error)
///   `#42 open/passing`  — PR number, pr state, rolled-up CI
///   `#42 merged`        — merged PRs drop the CI suffix (GitHub discards it)
pub(crate) fn pr_column(
    pr: Option<&PullRequest>,
    state: Option<PrState>,
    ci: Option<CiStatus>,
) -> String {
    let Some(pr) = pr else {
        return "-".to_string();
    };
    let state_label = state.map(pr_state_label).unwrap_or("?");
    // Merged/closed PRs shouldn't advertise a CI column — GitHub drops the
    // check data for them and we want the table to read "it's done" rather
    // than "it's done but CI is also saying something".
    if matches!(state, Some(PrState::Merged) | Some(PrState::Closed)) {
        return format!("#{} {state_label}", pr.number);
    }
    let ci_label = ci.map(ci_status_label).unwrap_or("?");
    format!("#{} {state_label}/{ci_label}", pr.number)
}
