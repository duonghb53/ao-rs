pub fn as_valid_opencode_session_id(value: impl AsRef<str>) -> Option<String> {
    let s = value.as_ref().trim();
    if s.is_empty() {
        return None;
    }
    if !s.starts_with("ses_") {
        return None;
    }
    // Allowed: A-Z a-z 0-9 _ - (at least one char required after the prefix)
    let tail: Vec<u8> = s.bytes().skip(4).collect();
    if !tail.is_empty()
        && tail
            .iter()
            .all(|&b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-'))
    {
        Some(s.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ids_are_accepted() {
        assert_eq!(
            as_valid_opencode_session_id("ses_abc123"),
            Some("ses_abc123".to_string())
        );
        assert_eq!(
            as_valid_opencode_session_id("ses_A-B_C"),
            Some("ses_A-B_C".to_string())
        );
    }

    #[test]
    fn prefix_only_is_rejected() {
        // TS regex requires at least one char after "ses_"
        assert_eq!(as_valid_opencode_session_id("ses_"), None);
    }

    #[test]
    fn empty_and_no_prefix_rejected() {
        assert_eq!(as_valid_opencode_session_id(""), None);
        assert_eq!(as_valid_opencode_session_id("abc123"), None);
    }

    #[test]
    fn invalid_chars_rejected() {
        assert_eq!(as_valid_opencode_session_id("ses_ab!c"), None);
        assert_eq!(as_valid_opencode_session_id("ses_ x"), None);
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(
            as_valid_opencode_session_id("  ses_ok  "),
            Some("ses_ok".to_string())
        );
    }
}
