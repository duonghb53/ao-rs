use ao_core::parity_feedback_tools::{
    generate_feedback_dedupe_key, validate_feedback_tool_input, FeedbackInput, FeedbackReportStore,
    FEEDBACK_TOOL_BUG_REPORT, FEEDBACK_TOOL_IMPROVEMENT_SUGGESTION,
};
use ao_core::parity_observability::{
    create_project_observer, read_observability_summary, RecordOperationInput, SetHealthInput,
    TsObservabilityConfig,
};

mod parity_test_utils;

#[test]
fn feedback_dedupe_key_is_stable_for_whitespace_case_and_evidence_order() {
    let payload = FeedbackInput {
        title: "Login failure for SSO users".into(),
        body: "Users with Google SSO are looped back to login.".into(),
        evidence: vec![
            "trace_id=abc123".into(),
            "Video capture from session".into(),
        ],
        session: "ao-22".into(),
        source: "agent".into(),
        confidence: 0.82,
    };
    let a = generate_feedback_dedupe_key(
        FEEDBACK_TOOL_BUG_REPORT,
        &FeedbackInput {
            title: " Login   failure FOR SSO users ".into(),
            body: payload.body.clone(),
            evidence: vec![
                "Video capture from session".into(),
                "trace_id=abc123".into(),
            ],
            session: payload.session.clone(),
            source: "Agent".into(),
            confidence: payload.confidence,
        },
    );
    let b = generate_feedback_dedupe_key(FEEDBACK_TOOL_BUG_REPORT, &payload);
    assert_eq!(a, b);
}

#[test]
fn feedback_validation_rejects_missing_required_fields() {
    let bad = FeedbackInput {
        title: "".into(),
        body: "x".into(),
        evidence: vec!["e".into()],
        session: "s".into(),
        source: "src".into(),
        confidence: 0.5,
    };
    assert!(validate_feedback_tool_input(FEEDBACK_TOOL_IMPROVEMENT_SUGGESTION, &bad).is_err());
}

#[test]
fn feedback_report_store_persists_and_lists() {
    let dir = parity_test_utils::unique_temp_dir("feedback");
    std::fs::create_dir_all(&dir).unwrap();
    let store = FeedbackReportStore::new(dir.join("reports"));
    let input = FeedbackInput {
        title: "T".into(),
        body: "B".into(),
        evidence: vec!["e1".into()],
        session: "ao-1".into(),
        source: "agent".into(),
        confidence: 0.9,
    };
    store.persist(FEEDBACK_TOOL_BUG_REPORT, input).unwrap();
    let list = store.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].tool, FEEDBACK_TOOL_BUG_REPORT);
}

#[test]
fn observability_records_counters_traces_sessions_and_health() {
    let root = parity_test_utils::unique_temp_dir("observability");
    std::fs::create_dir_all(&root).unwrap();
    let config = TsObservabilityConfig {
        config_path: root.join("agent-orchestrator.yaml"),
    };
    std::fs::write(&config.config_path, "projects: {}\n").unwrap();

    let observer = create_project_observer(config.clone(), "session-manager");
    observer.record_operation(RecordOperationInput {
        metric: "spawn".into(),
        operation: "session.spawn".into(),
        outcome: "success".into(),
        correlation_id: "corr-1".into(),
        project_id: Some("my-app".into()),
        session_id: Some("app-1".into()),
        reason: None,
        level: Some("info"),
    });
    observer.record_operation(RecordOperationInput {
        metric: "send".into(),
        operation: "session.send".into(),
        outcome: "failure".into(),
        correlation_id: "corr-2".into(),
        project_id: Some("my-app".into()),
        session_id: Some("app-1".into()),
        reason: Some("runtime unavailable".into()),
        level: Some("error"),
    });
    observer.set_health(SetHealthInput {
        surface: "lifecycle.worker".into(),
        status: "warn".into(),
        project_id: Some("my-app".into()),
        correlation_id: Some("corr-3".into()),
        reason: Some("poll delayed".into()),
    });

    let summary = read_observability_summary(config);
    let project = summary.projects.get("my-app").unwrap();
    assert_eq!(project.metrics.get("spawn").unwrap().total, 1);
    assert_eq!(project.metrics.get("spawn").unwrap().success, 1);
    assert_eq!(project.metrics.get("send").unwrap().failure, 1);
    assert_eq!(
        project.sessions.get("app-1").unwrap().operation,
        "session.send"
    );
    assert!(project
        .recent_traces
        .iter()
        .any(|t| t.operation == "session.spawn"));
    assert_eq!(
        project.health.get("lifecycle.worker").unwrap().status,
        "warn"
    );
    assert_eq!(summary.overall_status, "warn");
}
