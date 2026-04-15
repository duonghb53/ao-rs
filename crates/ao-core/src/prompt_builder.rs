//! Structured prompt builder for agent sessions.
//!
//! Composes a three-layer initial prompt sent via `Runtime::send_message`
//! after the agent process launches:
//!
//! 1. **Session context** — branch, project, repo slug, default branch.
//! 2. **Issue context** (issue-first only) — title, description, labels,
//!    assignee, URL formatted by `Tracker::generate_prompt()`.
//! 3. **Task directive** — a closing instruction telling the agent what
//!    to do with the above context.
//!
//! For `--task` (prompt-first) spawns, layer 2 is omitted and the raw
//! task text becomes the body.
//!
//! ## Design: rules stay in the system prompt
//!
//! Agent workflow rules (dev-lifecycle phases, coding standards, testing
//! requirements) are injected via `--append-system-prompt` on the agent's
//! launch command — persistent system-level guidance, not per-task
//! instructions. The prompt builder only composes the *user message*.

use crate::config::ProjectConfig;
use crate::scm::Issue;
use crate::types::Session;

/// Build a structured initial prompt for a session.
///
/// All parameters except `session` are optional — the builder degrades
/// gracefully when context is missing:
///
/// - No `project`: omits repo/branch metadata (prompt-first without config).
/// - No `issue_context`: uses `session.task` as a plain task directive
///   (prompt-first).
/// - Both `project` + `issue_context`: full structured prompt (issue-first).
///
/// `issue_context` is a pre-formatted string describing the issue. Callers
/// produce it via `Tracker::generate_prompt(&issue)` (which lets tracker
/// plugins add platform-specific context) or via the standalone
/// [`format_issue_context`] helper.
pub fn build_prompt(
    session: &Session,
    project: Option<&ProjectConfig>,
    issue_context: Option<&str>,
    template_context: Option<&str>,
) -> String {
    let mut sections: Vec<String> = Vec::new();

    // Layer 1: session context
    sections.push(format_session_context(session, project));

    // Layer 2: issue context (issue-first only)
    if let Some(ctx) = issue_context {
        sections.push(ctx.to_string());
    }

    // Optional: template context (issue-first and task-first)
    if let Some(t) = template_context {
        if !t.trim().is_empty() {
            sections.push(t.to_string());
        }
    }

    // Layer 3: task directive
    // Issue-first is derived from session metadata, not from whether
    // issue_context was passed — a session spawned with --issue always
    // has issue_id set.
    let is_issue_first = session.issue_id.is_some();
    sections.push(format_task_directive(session, is_issue_first));

    sections.join("\n\n")
}

/// Format a standalone issue prompt from an `Issue`, suitable for use
/// outside the full `build_prompt` flow (e.g. `Tracker::generate_prompt`
/// default impl).
pub fn format_issue_context(issue: &Issue) -> String {
    let mut lines = vec![format!("## Issue: #{} — {}", issue.id, issue.title)];

    if !issue.url.is_empty() {
        lines.push(format!("URL: {}", issue.url));
    }
    lines.push(format!("State: {}", issue_state_str(issue.state)));

    if let Some(ref milestone) = issue.milestone {
        if !milestone.trim().is_empty() {
            lines.push(format!("Milestone: {milestone}"));
        }
    }

    let (priority, context_labels, other_labels) = classify_labels(&issue.labels);
    if let Some(p) = priority {
        lines.push(format!("Priority: {p}"));
    }
    if !context_labels.is_empty() {
        lines.push(format!("Context: {}", context_labels.join(", ")));
    }
    if !other_labels.is_empty() {
        lines.push(format!("Labels: {}", other_labels.join(", ")));
    }
    if let Some(ref assignee) = issue.assignee {
        lines.push(format!("Assignee: {assignee}"));
    }

    if !issue.description.is_empty() {
        lines.push(String::new());
        lines.push(issue.description.clone());
    }

    lines.join("\n")
}

fn issue_state_str(s: crate::scm::IssueState) -> &'static str {
    match s {
        crate::scm::IssueState::Open => "open",
        crate::scm::IssueState::InProgress => "in_progress",
        crate::scm::IssueState::Closed => "closed",
        crate::scm::IssueState::Cancelled => "cancelled",
    }
}

fn classify_labels(labels: &[String]) -> (Option<String>, Vec<String>, Vec<String>) {
    // Heuristic: extract priority + a small "context" subset to help the agent.
    // Everything else remains under Labels.
    let mut priority: Option<String> = None;
    let mut context: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();

    for raw in labels {
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        let norm = l.to_ascii_lowercase();

        // Priority labels
        if matches!(norm.as_str(), "p0" | "p1" | "p2" | "p3") {
            priority = Some(norm.clone());
            continue;
        }
        if norm == "priority:high" || norm == "priority/high" || norm == "high" {
            priority = Some("high".to_string());
            continue;
        }
        if norm == "priority:medium" || norm == "priority/medium" || norm == "medium" {
            if priority.is_none() {
                priority = Some("medium".to_string());
            }
            continue;
        }
        if norm == "priority:low" || norm == "priority/low" || norm == "low" {
            if priority.is_none() {
                priority = Some("low".to_string());
            }
            continue;
        }

        // Context labels (common conventions)
        if norm.starts_with("area/") || norm.starts_with("kind/") || norm.starts_with("type/") {
            context.push(l.to_string());
            continue;
        }
        if matches!(
            norm.as_str(),
            "bug" | "feature" | "enhancement" | "refactor" | "docs" | "test" | "chore"
        ) {
            context.push(l.to_string());
            continue;
        }

        other.push(l.to_string());
    }

    // Keep output stable for snapshots/tests.
    context.sort();
    other.sort();
    (priority, context, other)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn format_session_context(session: &Session, project: Option<&ProjectConfig>) -> String {
    let mut lines = vec![format!("You are working on branch `{}`.", session.branch)];

    if let Some(proj) = project {
        lines.push(format!("Repository: {}", proj.repo));
        lines.push(format!("Default branch: {}", proj.default_branch));
    }

    if let Some(ref id) = session.issue_id {
        let url_part = session
            .issue_url
            .as_deref()
            .map(|u| format!(" — {u}"))
            .unwrap_or_default();
        lines.push(format!("Issue: #{id}{url_part}"));
    }

    lines.join("\n")
}

fn format_task_directive(session: &Session, is_issue_first: bool) -> String {
    if is_issue_first {
        // Issue-first: the issue context (layer 2) already describes the work.
        // The directive tells the agent to implement it and open a PR.
        "Use the dev-lifecycle workflow to turn the issue above into \
         concrete requirements and a plan, then implement the required \
         changes, verify with tests and linting, push your branch, and \
         open a pull request."
            .to_string()
    } else {
        // Prompt-first: the raw task IS the directive.
        session.task.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SessionId, SessionStatus};

    fn base_session() -> Session {
        Session {
            id: SessionId("test-prompt-builder".into()),
            project_id: "my-app".into(),
            status: SessionStatus::Working,
            agent: "claude-code".into(),
            agent_config: None,
            branch: "ao-abc123-feat-issue-42".into(),
            task: "Fix the login bug".into(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: None,
            created_at: 0,
            cost: None,
            issue_id: None,
            issue_url: None,
        }
    }

    fn sample_project() -> ProjectConfig {
        ProjectConfig {
            name: None,
            repo: "acme/widgets".into(),
            path: "/home/user/widgets".into(),
            default_branch: "main".into(),
            session_prefix: None,
            runtime: None,
            agent: None,
            workspace: None,
            tracker: None,
            scm: None,
            symlinks: vec![],
            post_create: vec![],
            agent_config: None,
            orchestrator: None,
            worker: None,
            orchestrator_rules: None,
        }
    }

    fn sample_issue() -> Issue {
        Issue {
            id: "42".into(),
            title: "Add dark mode".into(),
            description: "Users keep asking for dark mode support.".into(),
            url: "https://github.com/acme/widgets/issues/42".into(),
            state: crate::scm::IssueState::Open,
            labels: vec!["feature".into(), "ui".into()],
            assignee: Some("bob".into()),
            milestone: Some("Q2".into()),
        }
    }

    // ---- build_prompt: task-first ----

    #[test]
    fn task_first_no_project_returns_branch_and_task() {
        let session = base_session();
        let prompt = build_prompt(&session, None, None, None);

        assert!(prompt.contains("branch `ao-abc123-feat-issue-42`"));
        assert!(prompt.contains("Fix the login bug"));
        // No issue section.
        assert!(!prompt.contains("## Issue"));
    }

    #[test]
    fn task_first_with_project_includes_repo_context() {
        let session = base_session();
        let proj = sample_project();
        let prompt = build_prompt(&session, Some(&proj), None, None);

        assert!(prompt.contains("acme/widgets"));
        assert!(prompt.contains("Default branch: main"));
        assert!(prompt.contains("Fix the login bug"));
    }

    // ---- build_prompt: issue-first ----

    #[test]
    fn issue_first_full_context() {
        let mut session = base_session();
        session.issue_id = Some("42".into());
        session.issue_url = Some("https://github.com/acme/widgets/issues/42".into());
        let proj = sample_project();
        let issue = sample_issue();
        let issue_ctx = format_issue_context(&issue);

        let prompt = build_prompt(&session, Some(&proj), Some(&issue_ctx), None);

        // Layer 1: session context
        assert!(prompt.contains("branch `ao-abc123-feat-issue-42`"));
        assert!(prompt.contains("acme/widgets"));
        assert!(prompt.contains("Issue: #42"));

        // Layer 2: issue context
        assert!(prompt.contains("## Issue: #42 — Add dark mode"));
        assert!(prompt.contains("State: open"));
        assert!(prompt.contains("Milestone: Q2"));
        assert!(prompt.contains("Context: feature"));
        assert!(prompt.contains("Labels: ui"));
        assert!(prompt.contains("Assignee: bob"));
        assert!(prompt.contains("Users keep asking"));

        // Layer 3: directive (not the raw task)
        assert!(prompt.contains("push your branch"));
        assert!(prompt.contains("open a pull request"));
        assert!(prompt.contains("dev-lifecycle"));
    }

    #[test]
    fn issue_first_without_project_still_works() {
        let mut session = base_session();
        session.issue_id = Some("42".into());
        let issue = sample_issue();
        let issue_ctx = format_issue_context(&issue);

        let prompt = build_prompt(&session, None, Some(&issue_ctx), None);

        assert!(prompt.contains("## Issue: #42 — Add dark mode"));
        assert!(prompt.contains("open a pull request"));
        // No repo context line (the issue URL still contains the repo slug
        // naturally, but there's no "Repository:" metadata line).
        assert!(!prompt.contains("Repository:"));
        assert!(!prompt.contains("Default branch:"));
    }

    // ---- format_issue_context ----

    #[test]
    fn issue_context_includes_all_fields() {
        let issue = sample_issue();
        let ctx = format_issue_context(&issue);

        assert!(ctx.contains("## Issue: #42 — Add dark mode"));
        assert!(ctx.contains("https://github.com/acme/widgets/issues/42"));
        assert!(ctx.contains("State: open"));
        assert!(ctx.contains("Milestone: Q2"));
        assert!(ctx.contains("Context: feature"));
        assert!(ctx.contains("Labels: ui"));
        assert!(ctx.contains("Assignee: bob"));
        assert!(ctx.contains("Users keep asking"));
    }

    #[test]
    fn issue_context_minimal_issue() {
        let issue = Issue {
            id: "7".into(),
            title: "Fix typo".into(),
            description: String::new(),
            url: String::new(),
            state: crate::scm::IssueState::Open,
            labels: vec![],
            assignee: None,
            milestone: None,
        };
        let ctx = format_issue_context(&issue);

        assert!(ctx.contains("## Issue: #7 — Fix typo"));
        assert!(!ctx.contains("URL:"));
        assert!(ctx.contains("State: open"));
        assert!(!ctx.contains("Milestone:"));
        assert!(!ctx.contains("Labels:"));
        assert!(!ctx.contains("Assignee:"));
    }

    // ---- format_session_context ----

    #[test]
    fn session_context_with_issue_url() {
        let mut session = base_session();
        session.issue_id = Some("42".into());
        session.issue_url = Some("https://github.com/acme/widgets/issues/42".into());

        let ctx = format_session_context(&session, None);
        assert!(ctx.contains("Issue: #42 — https://github.com/acme/widgets/issues/42"));
    }

    #[test]
    fn session_context_issue_without_url() {
        let mut session = base_session();
        session.issue_id = Some("7".into());

        let ctx = format_session_context(&session, None);
        assert!(ctx.contains("Issue: #7"));
        assert!(!ctx.contains(" — "));
    }

    // ---- format_task_directive ----

    #[test]
    fn task_directive_issue_first_instructs_pr() {
        let session = base_session();
        let directive = format_task_directive(&session, true);
        assert!(directive.contains("open a pull request"));
        // Should NOT contain the raw task text — issue context covers it.
        assert!(!directive.contains("Fix the login bug"));
    }

    #[test]
    fn task_directive_prompt_first_returns_raw_task() {
        let session = base_session();
        let directive = format_task_directive(&session, false);
        assert_eq!(directive, "Fix the login bug");
    }
}
