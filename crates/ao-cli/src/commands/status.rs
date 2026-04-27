//! `ao-rs status` — list sessions with optional PR column.

use std::io::Write;
use std::time::Duration;

use ao_core::{CiStatus, PrState, PullRequest, Scm, Session, SessionManager};
use serde::Serialize;
use tokio::time::sleep;

use crate::cli::auto_scm::AutoScm;
use crate::cli::printing::{session_display_title, truncate};
use crate::commands::pr::{ci_status_label, pr_state_label};

#[derive(Clone, Debug)]
pub struct StatusOptions {
    pub project_filter: Option<String>,
    pub with_pr: bool,
    pub with_cost: bool,
    pub show_all: bool,
    pub json: bool,
    pub watch: bool,
    pub interval_secs: u64,
}

pub async fn status(opts: StatusOptions) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout();
    status_with_writer(opts, &mut out, None).await
}

async fn status_with_writer(
    opts: StatusOptions,
    mut out: impl Write,
    #[allow(unused_variables)] max_iterations: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let manager = SessionManager::with_default();

    let interval = Duration::from_secs(opts.interval_secs.max(1));
    run_status_loop(
        &mut out,
        opts.watch,
        interval,
        max_iterations,
        !cfg!(test),
        || render_snapshot(&manager, &opts),
    )
    .await
}

fn write_snapshot(out: &mut impl Write, s: &str) -> Result<(), Box<dyn std::error::Error>> {
    out.write_all(s.as_bytes())?;
    if !s.ends_with('\n') {
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}

async fn run_status_loop<F, Fut>(
    out: &mut impl Write,
    watch: bool,
    interval: Duration,
    #[allow(unused_variables)] max_iterations: Option<usize>,
    enable_ctrl_c: bool,
    mut render: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<String, Box<dyn std::error::Error>>>,
{
    let mut iterations = 0usize;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        let snapshot = render().await?;
        write_snapshot(out, &snapshot)?;

        if !watch {
            break;
        }

        if cfg!(test) {
            if let Some(max) = max_iterations {
                iterations += 1;
                if iterations >= max {
                    break;
                }
            }
        }

        if enable_ctrl_c {
            tokio::select! {
                _ = &mut ctrl_c => break,
                _ = sleep(interval) => {},
            }
        } else {
            sleep(interval).await;
        }
    }

    Ok(())
}

async fn render_snapshot(
    manager: &SessionManager,
    opts: &StatusOptions,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut sessions = match &opts.project_filter {
        Some(p) => manager.list_for_project(p).await?,
        None => manager.list().await?,
    };

    if !opts.show_all {
        sessions.retain(|s| !s.is_terminal());
    }

    if sessions.is_empty() {
        return Ok(match &opts.project_filter {
            Some(p) => format!("no sessions in project '{p}'\n"),
            None => "no sessions (use --all to include killed)\n".to_string(),
        });
    }

    if opts.json {
        render_json_snapshot(&sessions, opts).await
    } else {
        render_table_snapshot(&sessions, opts).await
    }
}

async fn render_table_snapshot(
    sessions: &[Session],
    opts: &StatusOptions,
) -> Result<String, Box<dyn std::error::Error>> {
    // Columns wide enough for the longest status (`changes_requested` = 17
    // chars) and the longest activity (`waiting_input` = 13 chars). Trying
    // to autosize is not worth it for a tool that prints ~10 rows max.
    //
    // Header and row formatting adapt to the --pr and --cost flags.
    let cost_hdr = if opts.with_cost {
        format!("{:<10} ", "COST")
    } else {
        String::new()
    };

    let mut buf = String::new();
    if opts.with_pr {
        buf.push_str(&format!(
            "{:<10} {:<14} {:<18} {:<14} {:<18} {:<24} {}TASK\n",
            "ID", "PROJECT", "STATUS", "ACTIVITY", "BRANCH", "PR", cost_hdr
        ));
    } else {
        buf.push_str(&format!(
            "{:<10} {:<14} {:<18} {:<14} {:<18} {}TASK\n",
            "ID", "PROJECT", "STATUS", "ACTIVITY", "BRANCH", cost_hdr
        ));
    }

    // Build the SCM plugin once up front if `--pr` is on, rather than per-row.
    let scm = if opts.with_pr {
        Some(AutoScm::new())
    } else {
        None
    };

    for s in sessions {
        let short_id: String = s.id.0.chars().take(8).collect();
        let title = session_display_title(s);
        let task = truncate(&title, 60);
        let activity = s
            .activity
            .map(|a| a.as_str().to_string())
            .unwrap_or_else(|| "-".to_string());
        let cost_cell = if opts.with_cost {
            format!(
                "{:<10} ",
                s.cost
                    .as_ref()
                    .and_then(|c| c.cost_usd.map(|usd| format!("${usd:.2}")))
                    .unwrap_or_else(|| "-".to_string())
            )
        } else {
            String::new()
        };

        if let Some(scm) = scm.as_ref() {
            let pr_cell = fetch_pr_column(scm, s).await;
            buf.push_str(&format!(
                "{:<10} {:<14} {:<18} {:<14} {:<18} {:<24} {}{}\n",
                short_id,
                s.project_id,
                s.status.as_str(),
                activity,
                s.branch,
                pr_cell,
                cost_cell,
                task,
            ));
        } else {
            buf.push_str(&format!(
                "{:<10} {:<14} {:<18} {:<14} {:<18} {}{}\n",
                short_id,
                s.project_id,
                s.status.as_str(),
                activity,
                s.branch,
                cost_cell,
                task,
            ));
        }
    }

    Ok(buf)
}

#[derive(Debug, Clone, Serialize)]
struct StatusJsonCost {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    /// `null` when the agent reports tokens without reliable USD pricing
    /// (e.g. Codex). Emitting `null` rather than `0.0` keeps consumers
    /// from confusing "unknown" with "free".
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct StatusJsonPr {
    number: u32,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<PrState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ci_status: Option<CiStatus>,
}

#[derive(Debug, Clone, Serialize)]
struct StatusJsonSession {
    id: String,
    short_id: String,
    project: String,
    status: String,
    activity: Option<String>,
    branch: String,
    task: String,
    created_at: u64,
    agent: String,
    runtime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<StatusJsonPr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<StatusJsonCost>,
}

async fn render_json_snapshot(
    sessions: &[Session],
    opts: &StatusOptions,
) -> Result<String, Box<dyn std::error::Error>> {
    let scm = if opts.with_pr {
        Some(AutoScm::new())
    } else {
        None
    };

    let mut out: Vec<StatusJsonSession> = Vec::with_capacity(sessions.len());
    for s in sessions {
        let short_id: String = s.id.0.chars().take(8).collect();
        let pr = if let Some(scm) = scm.as_ref() {
            fetch_pr_json(scm, s).await
        } else {
            None
        };
        let cost = if opts.with_cost {
            s.cost.as_ref().map(|c| StatusJsonCost {
                input_tokens: c.input_tokens,
                output_tokens: c.output_tokens,
                cache_read_tokens: c.cache_read_tokens,
                cache_creation_tokens: c.cache_creation_tokens,
                cost_usd: c.cost_usd,
            })
        } else {
            None
        };
        out.push(StatusJsonSession {
            id: s.id.0.clone(),
            short_id,
            project: s.project_id.clone(),
            status: s.status.as_str().to_string(),
            activity: s.activity.map(|a| a.as_str().to_string()),
            branch: s.branch.clone(),
            task: s.task.clone(),
            created_at: s.created_at,
            agent: s.agent.clone(),
            runtime: s.runtime.clone(),
            workspace_path: s.workspace_path.as_ref().map(|p| p.display().to_string()),
            issue_id: s.issue_id.clone(),
            issue_url: s.issue_url.clone(),
            pr,
            cost,
        });
    }

    Ok(format!("{}\n", serde_json::to_string(&out)?))
}

async fn fetch_pr_json(scm: &dyn Scm, session: &Session) -> Option<StatusJsonPr> {
    let Ok(Some(pr)) = scm.detect_pr(session).await else {
        return None;
    };
    let (state, ci) = tokio::join!(scm.pr_state(&pr), scm.ci_status(&pr));
    Some(StatusJsonPr {
        number: pr.number,
        url: pr.url,
        state: state.ok(),
        ci_status: ci.ok(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_snapshot_is_valid_json_array() {
        let s = Session {
            id: ao_core::SessionId("abc".into()),
            project_id: "demo".into(),
            status: ao_core::SessionStatus::Working,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "feat-x".into(),
            task: "do work".into(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: Some(ao_core::ActivityState::Ready),
            created_at: 123,
            cost: None,
            issue_id: Some("87".into()),
            issue_url: Some("https://github.com/duonghb53/ao-rs/issues/87".into()),
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: None,
            spawned_by: None,
            last_merge_conflict_dispatched: None,
            last_review_backlog_fingerprint: None,
            last_automated_review_fingerprint: None,
            last_automated_review_dispatch_hash: None,
        };

        let opts = StatusOptions {
            project_filter: None,
            with_pr: false,
            with_cost: false,
            show_all: true,
            json: true,
            watch: false,
            interval_secs: 2,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let rendered = rt.block_on(async { render_json_snapshot(&[s], &opts).await.unwrap() });

        let v: serde_json::Value = serde_json::from_str(rendered.trim()).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[test]
    fn clap_watch_interval_is_required_by_watch_only() {
        use clap::Parser;

        // `--interval` requires `--watch`.
        let parsed = crate::cli::args::Cli::try_parse_from(["ao-rs", "status", "--interval", "2"]);
        assert!(parsed.is_err());

        // With `--watch`, default interval applies.
        let parsed = crate::cli::args::Cli::try_parse_from(["ao-rs", "status", "--watch"]);
        assert!(parsed.is_ok());
    }

    #[tokio::test]
    async fn watch_loops_at_least_twice_in_tests() {
        let opts = StatusOptions {
            project_filter: None,
            with_pr: false,
            with_cost: false,
            show_all: true,
            json: true,
            watch: true,
            interval_secs: 1,
        };

        let mut buf: Vec<u8> = Vec::new();
        let mut n = 0usize;
        run_status_loop(
            &mut buf,
            opts.watch,
            Duration::from_millis(1),
            Some(2),
            false,
            || {
                n += 1;
                async move { Ok(format!("[{{\"n\":{n}}}]\n")) }
            },
        )
        .await
        .unwrap();

        let s = String::from_utf8(buf).unwrap();
        // Two snapshots, each on its own line.
        assert!(s.lines().count() >= 2);
        // And each line is valid JSON.
        for line in s.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.is_array());
        }
    }
}
