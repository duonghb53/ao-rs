use ao_core::notifier_resolution::{resolve_notifier_target, NotifierConfig};
use ao_core::opencode_session_id::as_valid_opencode_session_id;
use ao_core::parity_utils::{
    is_git_branch_name_safe, is_retryable_http_status, normalize_retry_config, read_last_jsonl_entry,
    RetryConfig,
};
use std::collections::HashMap;

mod parity_test_utils;

#[test]
fn opencode_session_id_accepts_valid() {
    assert_eq!(
        as_valid_opencode_session_id("ses_abc123").as_deref(),
        Some("ses_abc123")
    );
    assert_eq!(
        as_valid_opencode_session_id(" ses_ABC-123_xyz ").as_deref(),
        Some("ses_ABC-123_xyz")
    );
}

#[test]
fn opencode_session_id_rejects_invalid() {
    assert_eq!(as_valid_opencode_session_id(""), None);
    assert_eq!(as_valid_opencode_session_id("ses bad"), None);
    assert_eq!(as_valid_opencode_session_id("abc123"), None);
}

#[test]
fn read_last_jsonl_entry_empty_and_nonexistent() {
    let dir = crate::parity_test_utils::unique_temp_dir("utils-jsonl");
    std::fs::create_dir_all(&dir).unwrap();

    let p = dir.join("empty.jsonl");
    std::fs::write(&p, "").unwrap();
    assert!(read_last_jsonl_entry(&p).is_none());

    let p2 = dir.join("missing.jsonl");
    assert!(read_last_jsonl_entry(&p2).is_none());
}

#[test]
fn read_last_jsonl_entry_single_and_multi_line() {
    let dir = crate::parity_test_utils::unique_temp_dir("utils-jsonl2");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("a.jsonl");
    std::fs::write(&p, "{\"type\":\"assistant\",\"message\":\"hello\"}\n").unwrap();
    let got = read_last_jsonl_entry(&p).unwrap();
    assert_eq!(got.last_type.as_deref(), Some("assistant"));

    let p2 = dir.join("b.jsonl");
    std::fs::write(
        &p2,
        "{\"type\":\"human\"}\n{\"type\":\"assistant\"}\n{\"type\":\"result\"}\n",
    )
    .unwrap();
    let got = read_last_jsonl_entry(&p2).unwrap();
    assert_eq!(got.last_type.as_deref(), Some("result"));
}

#[test]
fn read_last_jsonl_entry_trailing_newlines_and_no_type() {
    let dir = crate::parity_test_utils::unique_temp_dir("utils-jsonl3");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("a.jsonl");
    std::fs::write(&p, "{\"type\":\"done\"}\n\n\n").unwrap();
    let got = read_last_jsonl_entry(&p).unwrap();
    assert_eq!(got.last_type.as_deref(), Some("done"));

    let p2 = dir.join("b.jsonl");
    std::fs::write(&p2, "{\"message\":\"no type\"}\n").unwrap();
    let got = read_last_jsonl_entry(&p2).unwrap();
    assert_eq!(got.last_type, None);
}

#[test]
fn is_git_branch_name_safe_parity() {
    assert!(is_git_branch_name_safe("feature/foo-bar-123"));
    assert!(is_git_branch_name_safe("feat/INT-123"));

    assert!(!is_git_branch_name_safe(""));
    assert!(!is_git_branch_name_safe("@"));
    assert!(!is_git_branch_name_safe("foo.lock"));
    assert!(!is_git_branch_name_safe("a..b"));
    assert!(!is_git_branch_name_safe(".hidden"));

    assert!(!is_git_branch_name_safe("feat//bar"));
    assert!(!is_git_branch_name_safe("feat/.hidden"));

    assert!(!is_git_branch_name_safe("bad branch"));
    assert!(!is_git_branch_name_safe("x:y"));
    assert!(!is_git_branch_name_safe("x~y"));
    assert!(!is_git_branch_name_safe("x?y"));
    assert!(!is_git_branch_name_safe("x[y]"));
    assert!(!is_git_branch_name_safe("a\nb"));
}

#[test]
fn retry_helpers_parity() {
    assert!(is_retryable_http_status(429));
    assert!(is_retryable_http_status(500));
    assert!(!is_retryable_http_status(404));

    let defaults = RetryConfig {
        retries: 2,
        retry_delay_ms: 1000,
    };
    let mut cfg = HashMap::new();
    cfg.insert("retries".to_string(), serde_json::json!(3));
    cfg.insert("retryDelayMs".to_string(), serde_json::json!(250));
    let out = normalize_retry_config(Some(&cfg), defaults);
    assert_eq!(
        out,
        RetryConfig {
            retries: 3,
            retry_delay_ms: 250
        }
    );
}

#[test]
fn notifier_resolution_alias_or_passthrough() {
    let mut map = HashMap::new();
    map.insert(
        "alerts".to_string(),
        NotifierConfig {
            plugin: "slack".to_string(),
        },
    );
    let got = resolve_notifier_target(Some(&map), "alerts");
    assert_eq!(got.plugin_name, "slack");

    let got = resolve_notifier_target(Some(&map), "desktop");
    assert_eq!(got.plugin_name, "desktop");
}

