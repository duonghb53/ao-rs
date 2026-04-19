//! Shared utilities for agent plugins: shell escaping, git helpers,
//! and the fallback initial-prompt builder.

/// Returns true if the git repo at `path` has any commits in the last 60 seconds.
///
/// Runs `git log --since=60 seconds ago --format=%H` synchronously. Intended
/// for use inside `tokio::task::spawn_blocking` closures where async is not
/// available.
pub fn has_recent_commits(path: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .args(["log", "--since=60 seconds ago", "--format=%H"])
        .current_dir(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// POSIX single-quote shell escape.
///
/// Wraps `s` in single quotes and replaces any embedded `'` with `'\''`.
/// Always produces a quoted result — the always-wrap strategy is the safest
/// default: it is correct for all strings including empty ones and avoids
/// "safe-char set" disputes between callers.
///
/// Mirrors `shellEscape` from `packages/core/src/utils.ts`.
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'\''"#))
}

/// Build the fallback initial prompt sent to an agent after launch.
///
/// Issue-first spawns get structured context (branch, issue ID, URL, PR
/// directive). Task-only spawns (`--task`) get the raw `session.task`.
/// When `rules` is `Some`, they are prepended before the task block —
/// for agents that deliver rules via the prompt rather than a system-prompt
/// flag (e.g. aider, codex, cursor).
pub fn build_initial_prompt(session: &crate::Session, rules: Option<&str>) -> String {
    if let Some(id) = &session.issue_id {
        let url_line = session
            .issue_url
            .as_deref()
            .map(|u| format!("\nIssue URL: {u}"))
            .unwrap_or_default();
        let task_part = format!(
            "You are working on issue #{id} on branch `{branch}`.{url_line}\n\n\
             Task:\n{task}\n\n\
             When complete, push your branch and open a pull request.",
            branch = session.branch,
            task = session.task,
        );
        match rules {
            Some(r) => format!("{r}\n\n---\n\n{task_part}"),
            None => task_part,
        }
    } else {
        match rules {
            Some(r) => format!("{r}\n\n---\n\n{}", session.task),
            None => session.task.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::shell_escape;

    #[test]
    fn plain_string_is_wrapped() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn empty_string_becomes_two_quotes() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn single_quote_is_escaped() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn spaces_and_metacharacters_are_quoted() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(shell_escape("$VAR"), "'$VAR'");
    }
}
