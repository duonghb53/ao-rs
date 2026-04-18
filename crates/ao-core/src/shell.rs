//! Shell escaping utilities.
//!
//! Single canonical implementation of POSIX single-quote shell escaping,
//! consolidating the per-plugin copies previously in `runtime-tmux`,
//! `agent-codex`, `agent-aider`, and `ao-cli`.

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
