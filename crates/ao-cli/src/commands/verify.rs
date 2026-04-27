//! `ao-rs verify` — minimal parity verification workflow.

use std::collections::{BTreeMap, BTreeSet};

use ao_core::{detect_git_repo, Session, SessionManager, SessionStatus, Tracker};
use ao_plugin_tracker_github::GitHubTracker;

pub async fn verify(
    list: bool,
    fail: bool,
    comment: Option<String>,
    target: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let manager = SessionManager::with_default();
    let sessions = load_sessions_with_archives(&manager).await?;

    if list {
        let mut rows = list_verify_targets(&sessions);
        if rows.is_empty() {
            println!("no verify targets (no issue sessions in merged/done)");
            return Ok(());
        }

        println!(
            "{:<12} {:<10} {:<10} {:<14} TASK",
            "ISSUE", "PROJECT", "SESSION", "STATUS"
        );
        for row in rows.drain(..) {
            println!(
                "{:<12} {:<10} {:<10} {:<14} {}",
                row.issue_id,
                row.project_id,
                row.session_short_id,
                row.status.as_str(),
                row.task
            );
        }
        return Ok(());
    }

    let target = target.expect("clap enforces target unless --list");

    let outcome = if is_issue_token(&target) {
        verify_issue_id(&normalize_issue_id(&target), &sessions)
    } else {
        verify_session_or_prefix(&manager, &sessions, &target).await?
    };

    match outcome {
        VerifyOutcome::Pass { target, details } => {
            println!("PASS {target} — {details}");
            if let Some(msg) = comment {
                post_comment_best_effort(&target, &msg).await?;
            }
            Ok(())
        }
        VerifyOutcome::Fail { target, details } => {
            println!("FAIL {target} — {details}");
            if let Some(msg) = comment {
                post_comment_best_effort(&target, &msg).await?;
            }
            if fail {
                Err(format!("verification failed for {target}").into())
            } else {
                Ok(())
            }
        }
    }
}

async fn post_comment_best_effort(
    target_label: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // We only support GitHub issue comments for now:
    // - determine if the target looks like `issue N`
    // - detect the current repo's GitHub remote
    // - post via GitHubTracker (gh cli)
    let Some(issue_id) = target_label.strip_prefix("issue ").map(str::trim) else {
        println!("note: --comment currently supports issue targets only");
        return Ok(());
    };

    // `detect_git_repo` expects to run within a git repo.
    let cwd = std::env::current_dir()?;
    let (owner_repo, _repo_name, _default_branch) = detect_git_repo(&cwd)?;
    let (owner, repo) = owner_repo
        .split_once('/')
        .ok_or_else(|| format!("could not parse owner/repo from '{owner_repo}'"))?;
    let tracker = GitHubTracker::new(owner.to_string(), repo.to_string());
    tracker.comment_issue(issue_id, body).await?;
    println!("commented on issue {issue_id}");
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VerifyOutcome {
    Pass { target: String, details: String },
    Fail { target: String, details: String },
}

#[derive(Debug, Clone)]
struct VerifyListRow {
    issue_id: String,
    project_id: String,
    session_short_id: String,
    status: SessionStatus,
    task: String,
}

async fn load_sessions_with_archives(
    manager: &SessionManager,
) -> Result<Vec<Session>, Box<dyn std::error::Error>> {
    let active = manager.list().await?;
    let projects: BTreeSet<String> = active.iter().map(|s| s.project_id.clone()).collect();

    // Pull in archived sessions for the same projects. This keeps `verify`
    // useful right after cleanup, without doing any network calls.
    let mut all = active;
    for project_id in projects.iter() {
        if let Ok(mut archived) = manager.list_archived(project_id).await {
            all.append(&mut archived);
        }
    }

    // Newest first overall (mirrors `SessionManager::list`).
    all.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    Ok(all)
}

fn list_verify_targets(sessions: &[Session]) -> Vec<VerifyListRow> {
    let mut best_by_issue: BTreeMap<String, (u64, VerifyListRow)> = BTreeMap::new();
    for s in sessions {
        let Some(issue_id) = s.issue_id.as_ref() else {
            continue;
        };
        if !is_success_status(s.status) {
            continue;
        }

        let row = VerifyListRow {
            issue_id: issue_id.clone(),
            project_id: s.project_id.clone(),
            session_short_id: s.id.0.chars().take(8).collect(),
            status: s.status,
            task: truncate(&s.task, 60),
        };

        // Keep the newest success row per issue id.
        best_by_issue
            .entry(issue_id.clone())
            .and_modify(|(existing_created_at, existing_row)| {
                if s.created_at > *existing_created_at {
                    *existing_created_at = s.created_at;
                    *existing_row = row.clone();
                }
            })
            .or_insert((s.created_at, row));
    }
    best_by_issue.into_values().map(|(_, row)| row).collect()
}

fn verify_issue_id(issue_id: &str, sessions: &[Session]) -> VerifyOutcome {
    let matching: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.issue_id.as_deref() == Some(issue_id))
        .collect();

    let target = format!("issue {issue_id}");
    if matching.is_empty() {
        return VerifyOutcome::Fail {
            target,
            details: "no sessions found".to_string(),
        };
    }

    if let Some(success) = matching.iter().find(|s| is_success_status(s.status)) {
        return VerifyOutcome::Pass {
            target,
            details: format!(
                "session {} is {}",
                success.id.0.chars().take(8).collect::<String>(),
                success.status.as_str()
            ),
        };
    }

    let newest = matching
        .iter()
        .max_by_key(|s| s.created_at)
        .expect("matching non-empty");
    VerifyOutcome::Fail {
        target,
        details: format!(
            "no merged/done sessions (newest is {} {})",
            newest.id.0.chars().take(8).collect::<String>(),
            newest.status.as_str()
        ),
    }
}

async fn verify_session_or_prefix(
    manager: &SessionManager,
    all_sessions: &[Session],
    token: &str,
) -> Result<VerifyOutcome, Box<dyn std::error::Error>> {
    // Prefer the authoritative prefix resolver for active sessions.
    let session = match manager.find_by_prefix(token).await {
        Ok(s) => s,
        Err(_) => find_any_session_by_prefix(all_sessions, token)?,
    };

    let target = format!(
        "session {}",
        session.id.0.chars().take(8).collect::<String>()
    );
    if is_success_status(session.status) {
        Ok(VerifyOutcome::Pass {
            target,
            details: format!("status is {}", session.status.as_str()),
        })
    } else {
        Ok(VerifyOutcome::Fail {
            target,
            details: format!("status is {}", session.status.as_str()),
        })
    }
}

fn find_any_session_by_prefix(
    sessions: &[Session],
    token: &str,
) -> Result<Session, Box<dyn std::error::Error>> {
    if token.is_empty() {
        return Err("session id/prefix is empty".into());
    }
    let matches: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.id.0.starts_with(token))
        .collect();
    match matches.len() {
        0 => Err(format!("session not found: '{token}'").into()),
        1 => Ok(matches[0].clone()),
        n => Err(format!("session id prefix is ambiguous ('{token}' matched {n} sessions)").into()),
    }
}

fn is_issue_token(input: &str) -> bool {
    let s = input.trim();
    if s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    s.starts_with("local-")
}

fn normalize_issue_id(input: &str) -> String {
    let s = input.trim();
    s.strip_prefix('#').unwrap_or(s).to_string()
}

fn is_success_status(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Merged | SessionStatus::Done)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = s.chars().take(max.saturating_sub(1)).collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ao_core::{ActivityState, SessionId};

    fn fake_session(
        id: &str,
        issue_id: Option<&str>,
        status: SessionStatus,
        created_at: u64,
    ) -> Session {
        Session {
            id: SessionId(id.to_string()),
            project_id: "p".to_string(),
            status,
            agent: "claude-code".to_string(),
            agent_config: None,
            branch: "feat/x".to_string(),
            task: "do the thing".to_string(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".to_string(),
            activity: Some(ActivityState::Active),
            created_at,
            cost: None,
            issue_id: issue_id.map(|s| s.to_string()),
            issue_url: None,
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: None,
            spawned_by: None,
            last_merge_conflict_dispatched: None,
            last_review_backlog_fingerprint: None,
            last_automated_review_fingerprint: None,
            last_automated_review_dispatch_hash: None,
        }
    }

    #[test]
    fn is_issue_token_accepts_hash_digits_digits_and_local_prefix() {
        assert!(is_issue_token("#81"));
        assert!(is_issue_token("81"));
        assert!(is_issue_token("local-0001"));
        assert!(!is_issue_token("abcd"));
        assert!(!is_issue_token("81xyz"));
    }

    #[test]
    fn verify_issue_fails_when_no_sessions_found() {
        let sessions = vec![fake_session(
            "aaaaaaaa",
            Some("1"),
            SessionStatus::Working,
            1,
        )];
        let out = verify_issue_id("81", &sessions);
        assert!(matches!(out, VerifyOutcome::Fail { .. }));
    }

    #[test]
    fn verify_issue_passes_when_any_session_is_merged_or_done() {
        let sessions = vec![
            fake_session("aaaaaaaa", Some("81"), SessionStatus::Working, 1),
            fake_session("bbbbbbbb", Some("81"), SessionStatus::Merged, 2),
        ];
        let out = verify_issue_id("81", &sessions);
        assert!(matches!(out, VerifyOutcome::Pass { .. }));
    }

    #[test]
    fn verify_issue_fails_when_sessions_exist_but_none_are_success() {
        let sessions = vec![fake_session(
            "aaaaaaaa",
            Some("81"),
            SessionStatus::PrOpen,
            3,
        )];
        let out = verify_issue_id("81", &sessions);
        assert!(matches!(out, VerifyOutcome::Fail { .. }));
    }
}
