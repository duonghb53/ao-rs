use ao_core::parity_metadata::{
    delete_metadata, list_metadata, read_archived_metadata_raw, read_metadata, read_metadata_raw,
    update_metadata, write_metadata, TsSessionMetadata,
};
use std::collections::HashMap;
use std::path::PathBuf;

fn tmp() -> PathBuf {
    crate::parity_test_utils::unique_temp_dir("metadata")
}

mod parity_test_utils;

#[test]
fn write_and_read_basic_metadata() {
    let dir = tmp();
    std::fs::create_dir_all(&dir).unwrap();
    write_metadata(
        &dir,
        "app-1",
        &TsSessionMetadata {
            worktree: "/tmp/worktree".into(),
            branch: "feat/test".into(),
            status: "working".into(),
            issue: None,
            pr: None,
            pr_auto_detect: None,
            summary: None,
            project: None,
            created_at: None,
            runtime_handle: None,
            pinned_summary: None,
        },
    )
    .unwrap();

    let meta = read_metadata(&dir, "app-1").unwrap().unwrap();
    assert_eq!(meta.worktree, "/tmp/worktree");
    assert_eq!(meta.branch, "feat/test");
    assert_eq!(meta.status, "working");
}

#[test]
fn writes_key_value_format_and_omits_undefined() {
    let dir = tmp();
    std::fs::create_dir_all(&dir).unwrap();
    write_metadata(
        &dir,
        "app-3",
        &TsSessionMetadata {
            worktree: "/tmp/w".into(),
            branch: "feat/INT-123".into(),
            status: "working".into(),
            issue: Some("https://linear.app/team/issue/INT-123".into()),
            pr: None,
            pr_auto_detect: None,
            summary: None,
            project: None,
            created_at: None,
            runtime_handle: None,
            pinned_summary: None,
        },
    )
    .unwrap();
    let content = std::fs::read_to_string(dir.join("app-3")).unwrap();
    assert!(content.contains("worktree=/tmp/w"));
    assert!(content.contains("branch=feat/INT-123"));
    assert!(content.contains("status=working"));
    assert!(content.contains("issue=https://linear.app/team/issue/INT-123"));
    assert!(!content.contains("pr="));
}

#[test]
fn read_metadata_raw_reads_arbitrary_pairs() {
    let dir = tmp();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("raw-1"),
        "worktree=/tmp/w\nbranch=main\ncustom_key=custom_value\n",
    )
    .unwrap();
    let raw = read_metadata_raw(&dir, "raw-1").unwrap().unwrap();
    assert_eq!(
        raw.get("custom_key").map(|s| s.as_str()),
        Some("custom_value")
    );
}

#[test]
fn update_and_delete_and_archive() {
    let dir = tmp();
    std::fs::create_dir_all(&dir).unwrap();
    write_metadata(
        &dir,
        "app-1",
        &TsSessionMetadata {
            worktree: "/tmp/w".into(),
            branch: "main".into(),
            status: "spawning".into(),
            issue: None,
            pr: None,
            pr_auto_detect: None,
            summary: None,
            project: None,
            created_at: None,
            runtime_handle: None,
            pinned_summary: None,
        },
    )
    .unwrap();

    let mut updates = HashMap::new();
    updates.insert("status".to_string(), "working".to_string());
    update_metadata(&dir, "app-1", &updates).unwrap();
    let meta = read_metadata(&dir, "app-1").unwrap().unwrap();
    assert_eq!(meta.status, "working");

    delete_metadata(&dir, "app-1", true).unwrap();
    assert!(read_metadata(&dir, "app-1").unwrap().is_none());
    assert!(read_archived_metadata_raw(&dir, "app-1").unwrap().is_some());
}

#[test]
fn list_metadata_lists_only_session_files() {
    let dir = tmp();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a-1"), "worktree=/tmp\n").unwrap();
    std::fs::write(dir.join("b-2"), "worktree=/tmp\n").unwrap();
    std::fs::create_dir_all(dir.join("archive")).unwrap();
    let list = list_metadata(&dir).unwrap();
    assert_eq!(list, vec!["a-1".to_string(), "b-2".to_string()]);
}
