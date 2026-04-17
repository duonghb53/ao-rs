//! `ao-rs session claim-pr`.

use ao_core::{SessionManager, SessionStatus, Tracker};

use crate::cli::printing::short_id;

/// Parse a PR reference (plain number, `#123`, or full URL) into
/// `(pr_number, pr_url)`.
fn parse_pr_ref(input: &str) -> (Option<u32>, Option<String>) {
    let trimmed = input.trim();
    let number_str = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if let Ok(n) = number_str.parse::<u32>() {
        return (Some(n), None);
    }
    let url = trimmed.to_string();
    let last = trimmed
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    let num = last.parse::<u32>().ok();
    (num, Some(url))
}

/// Resolve the target session: explicit prefix arg → env vars → most recent active.
async fn resolve_session_id(
    sessions: &SessionManager,
    session_arg: Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(id) = session_arg {
        return Ok(id);
    }
    if let Ok(v) = std::env::var("AO_SESSION_NAME") {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if let Ok(v) = std::env::var("AO_SESSION") {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    // Fall back to most recently created active session.
    let mut all = sessions.list().await?;
    if all.is_empty() {
        return Err("no active sessions found; pass a session id explicitly".into());
    }
    all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(all[0].id.0.clone())
}

pub async fn claim_pr(
    pr_ref: String,
    session_arg: Option<String>,
    assign_on_github: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (pr_number, pr_url) = parse_pr_ref(&pr_ref);
    if pr_number.is_none() && pr_url.is_none() {
        return Err(format!("could not parse PR reference: {pr_ref}").into());
    }

    let sessions = SessionManager::with_default();
    let session_id = resolve_session_id(&sessions, session_arg).await?;
    let mut session = sessions.find_by_prefix(&session_id).await?;
    let short = short_id(&session.id);

    session.claimed_pr_number = pr_number;
    session.claimed_pr_url = pr_url.clone();
    if session.status == SessionStatus::Working || session.status == SessionStatus::Spawning {
        session.status = SessionStatus::PrOpen;
    }
    sessions.save(&session).await?;

    if assign_on_github {
        if let Some(n) = pr_number {
            if let Some(ref ws) = session.workspace_path {
                match ao_plugin_tracker_github::GitHubTracker::from_repo(ws).await {
                    Ok(tracker) => {
                        if let Err(e) = tracker.assign_to_me(&n.to_string()).await {
                            println!("note: --assign-on-github failed: {e}");
                        } else {
                            println!("→ assigned PR #{n} to current GitHub user");
                        }
                    }
                    Err(e) => println!("note: --assign-on-github requires a GitHub remote: {e}"),
                }
            } else {
                println!("note: --assign-on-github skipped (session has no workspace path)");
            }
        } else {
            println!("note: --assign-on-github needs a numeric PR reference (or URL ending in /<number>)");
        }
    }

    println!();
    println!("───────────────────────────────────────────────");
    println!("  ✓ PR claimed for session {short}");
    println!();
    if let Some(n) = pr_number {
        println!("  pr number: #{n}");
    }
    if let Some(ref url) = pr_url {
        println!("  pr url:    {url}");
    }
    println!("  status:    {}", session.status.as_str());
    println!();
    println!("  inspect:   ao-rs pr {short}");
    println!("───────────────────────────────────────────────");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_pr_ref;

    #[test]
    fn parse_plain_number() {
        assert_eq!(parse_pr_ref("42"), (Some(42), None));
    }

    #[test]
    fn parse_hash_number() {
        assert_eq!(parse_pr_ref("#42"), (Some(42), None));
    }

    #[test]
    fn parse_url_with_number() {
        let url = "https://github.com/owner/repo/pull/99";
        assert_eq!(parse_pr_ref(url), (Some(99), Some(url.to_string())));
    }

    #[test]
    fn parse_url_trailing_slash() {
        let url = "https://github.com/owner/repo/pull/7/";
        assert_eq!(parse_pr_ref(url), (Some(7), Some(url.to_string())));
    }

    #[test]
    fn parse_url_no_number() {
        let url = "https://github.com/owner/repo/pulls";
        assert_eq!(parse_pr_ref(url), (None, Some(url.to_string())));
    }
}
