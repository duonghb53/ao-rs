//! Shell escaping, branch slugification, spawn templates, tmux helpers.

pub(crate) fn shell_escape_single_quotes(s: &str) -> String {
    // Wrap in single quotes and escape embedded single quotes for POSIX shells.
    // Example: abc'd -> 'abc'\''d'
    let escaped = s.replace('\'', r#"'\''"#);
    format!("'{escaped}'")
}

pub(crate) fn git_safe_branch_fragment(input: &str) -> String {
    // Conservative "safe for git refs" sanitization.
    //
    // - Only allow [a-z0-9_-]
    // - Convert path separators and punctuation to '-'
    // - Collapse repeated dashes
    // - Trim leading/trailing dashes/underscores
    //
    // This intentionally drops dots and other valid-but-footgun characters.
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for c in input.chars() {
        let lower = c.to_ascii_lowercase();
        let keep = lower.is_ascii_alphanumeric() || lower == '_' || lower == '-';
        if keep {
            if lower == '-' {
                if prev_dash {
                    continue;
                }
                prev_dash = true;
            } else {
                prev_dash = false;
            }
            out.push(lower);
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '_').to_string();
    if trimmed.is_empty() {
        "work".to_string()
    } else {
        trimmed
    }
}

pub(crate) fn spawn_template_by_name(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let n = name.trim().to_ascii_lowercase();
    let body = match n.as_str() {
        "bugfix" => {
            r#"## Template: Bugfix
- **Hypothesis**: What is broken? What is the suspected root cause?
- **Repro**: Steps to reproduce + expected vs actual behavior
- **Fix**: Minimal change that addresses the root cause
- **Regression tests**: Add/extend tests to prevent recurrence

## Test plan
- [ ] Run relevant unit/integration tests
- [ ] Exercise the failing scenario manually (if applicable)
"#
        }
        "feature" => {
            r#"## Template: Feature
- **Goal**: What outcome should exist after this change?
- **Scope**: What's in / out of scope?
- **UX/Behavior**: Any edge cases, error states, or backwards-compat constraints?

## Acceptance criteria
- [ ] Meets functional requirements
- [ ] Has tests (unit/integration as appropriate)
- [ ] Docs/CLI help updated if user-facing

## Test plan
- [ ] Run relevant tests
- [ ] Verify behavior end-to-end
"#
        }
        "refactor" => {
            r#"## Template: Refactor
- **Motivation**: Why refactor (maintainability, correctness, performance)?
- **Constraints**: Behavior must remain identical unless specified
- **Risks**: What could regress? How to mitigate?

## Test plan
- [ ] Run relevant tests
- [ ] Confirm no behavior change (or document intended changes)
"#
        }
        "docs" => {
            r#"## Template: Docs
- **Audience**: Who is this for?
- **Goal**: What should the reader be able to do after reading?

## Test plan
- [ ] Validate instructions from a clean checkout (if possible)
"#
        }
        "test" => {
            r#"## Template: Tests
- **Coverage goal**: What behavior should be locked in?
- **Test types**: Unit vs integration vs e2e (pick the smallest that’s meaningful)
- **Fixtures/mocks**: Keep them minimal and readable

## Test plan
- [ ] Run the added tests and the relevant suite
"#
        }
        _ => {
            return Err(format!(
                "unknown template '{name}'. supported: bugfix, feature, refactor, docs, test"
            )
            .into())
        }
    };
    Ok(body.to_string())
}

pub(crate) async fn tmux_send_keys_literal_no_enter(handle: &str, text: &str) {
    // Best-effort: used for UI keystrokes (Cursor trust prompt) where sending
    // Enter can be harmful. Ignore failures so spawn doesn't fail just because
    // tmux is missing or the session exited.
    let _ = tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", handle, "-l", text])
        .status()
        .await;
}

pub(crate) fn git_safe_branch_namespace(input: &str) -> String {
    // Sanitize a user-supplied namespace while preserving `/` separators.
    //
    // Example:
    // - "Ao/Agent" => "ao/agent"
    // - "ao agent//team" => "ao-agent/team"
    let parts: Vec<String> = input
        .split('/')
        .map(git_safe_branch_fragment)
        .filter(|p| p != "work")
        .collect();
    let joined = parts.join("/");
    if joined.is_empty() {
        "ao".to_string()
    } else {
        joined
    }
}

pub(crate) fn issue_branch_name(issue_id: &str, issue_title: &str) -> String {
    let slug = slugify_issue_title_for_branch(issue_title);
    let slug = if slug.is_empty() {
        issue_id.to_string()
    } else {
        slug
    };
    format!("feature/{}-{}", issue_id, slug)
}

fn slugify_issue_title_for_branch(input: &str) -> String {
    // Slug rules for issue-based branches:
    // - Lowercase
    // - Keep ASCII alphanumeric as-is
    // - Replace any run of non-alphanumeric with a single '-'
    // - Trim leading/trailing '-'
    // - If empty after cleaning, the caller falls back to `issue_id`
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;

    for c in input.chars() {
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }

    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod spawn_helpers_tests {
    use super::{git_safe_branch_fragment, git_safe_branch_namespace, issue_branch_name};

    #[test]
    fn git_safe_branch_fragment_is_stable_and_safe() {
        assert_eq!(git_safe_branch_fragment("feat/issue-42"), "feat-issue-42");
        assert_eq!(
            git_safe_branch_fragment("Feat/ISSUE 42!!!"),
            "feat-issue-42"
        );
        assert_eq!(git_safe_branch_fragment("..."), "work");
        assert_eq!(git_safe_branch_fragment("a--b"), "a-b");
    }

    #[test]
    fn git_safe_branch_namespace_preserves_slashes_and_sanitizes_segments() {
        assert_eq!(git_safe_branch_namespace("Ao/Agent"), "ao/agent");
        assert_eq!(git_safe_branch_namespace("ao agent//team"), "ao-agent/team");
    }

    #[test]
    fn issue_branch_name_slugifies_title_and_falls_back_to_issue() {
        assert_eq!(
            issue_branch_name("77", "My Feature Title"),
            "feature/77-my-feature-title"
        );
        assert_eq!(
            issue_branch_name("local-0007", "My Feature Title"),
            "feature/local-0007-my-feature-title"
        );

        // Runs of punctuation collapse into a single dash.
        assert_eq!(
            issue_branch_name("77", "Fix CI: core/lifecycle"),
            "feature/77-fix-ci-core-lifecycle"
        );

        // Leading/trailing punctuation is trimmed.
        assert_eq!(
            issue_branch_name("77", "---Hello---"),
            "feature/77-hello"
        );

        // Unicode isn't ASCII alphanumeric, so it becomes '-'.
        assert_eq!(
            issue_branch_name("77", "My Café Title"),
            "feature/77-my-caf-title"
        );

        // If the title cleans into an empty slug, fall back to issue_id.
        assert_eq!(issue_branch_name("77", "!!!"), "feature/77-77");
        assert_eq!(issue_branch_name("77", "   "), "feature/77-77");
    }
}
