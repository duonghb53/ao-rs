pub fn as_valid_opencode_session_id(value: impl AsRef<str>) -> Option<String> {
    let s = value.as_ref().trim();
    if s.is_empty() {
        return None;
    }
    if !s.starts_with("ses_") {
        return None;
    }
    // Allowed: A-Z a-z 0-9 _ -
    if s.bytes()
        .skip(4)
        .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-'))
    {
        Some(s.to_string())
    } else {
        None
    }
}
