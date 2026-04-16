//! Cross-module unit tests (argument parsing, pure helpers, CLI fixtures).

use clap::Parser;

use ao_core::{
    now_ms, AgentConfig, AoConfig, CiStatus, DefaultsConfig, MergeReadiness, PrState,
    ProjectConfig, PullRequest, ReviewDecision, Session, SessionId, SessionStatus,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::agent_config::{resolve_agent_config, resolve_agent_config_for_restore};
use crate::cli::args::{Cli, Command};
use crate::cli::local_issue::{
    collect_local_issue_entries, local_issue_id_from_filename, local_issue_ids_from_path,
    next_local_issue_number, parse_local_issue_id_token, parse_local_issue_markdown,
    resolve_local_issue_for_show, slugify_filename,
};
use crate::cli::printing::session_display_title;
use crate::cli::project::resolve_project_id;
use crate::commands::pr::{
    ci_status_label, format_pr_report, pr_state_label, review_decision_label,
};
use crate::commands::status::pr_column;

fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ao-rs-cli-{label}-{nanos}-{n}"))
}

#[test]
fn resolve_agent_config_inlines_rules_file_and_clears_path() {
    let repo_dir = unique_temp_dir("rules-inline");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let rules_path = repo_dir.join("rules.md");
    std::fs::write(&rules_path, "RULES: be nice").unwrap();

    let cfg = AgentConfig {
        permissions: "permissionless".into(),
        rules: None,
        rules_file: Some("rules.md".into()),
        model: None,
        orchestrator_model: None,
        opencode_session_id: None,
    };
    let resolved = resolve_agent_config(Some(&cfg), &repo_dir).unwrap();
    assert_eq!(resolved.rules.as_deref(), Some("RULES: be nice"));
    assert!(resolved.rules_file.is_none());

    let _ = std::fs::remove_dir_all(&repo_dir);
}

#[test]
fn resolve_agent_config_for_restore_inlines_rules_file_using_workspace_path() {
    let ws = unique_temp_dir("rules-restore");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("rules.txt"), "restored rules").unwrap();

    let mut s = fake_session();
    s.workspace_path = Some(ws.clone());
    s.agent_config = Some(AgentConfig {
        permissions: "permissionless".into(),
        rules: None,
        rules_file: Some("rules.txt".into()),
        model: None,
        orchestrator_model: None,
        opencode_session_id: None,
    });

    resolve_agent_config_for_restore(&mut s);
    let cfg = s.agent_config.unwrap();
    assert_eq!(cfg.rules.as_deref(), Some("restored rules"));
    assert!(cfg.rules_file.is_none());

    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn session_display_title_prefixes_issue_sessions() {
    let mut s = Session {
        id: SessionId("x".into()),
        project_id: "p".into(),
        status: SessionStatus::Working,
        agent: "claude-code".into(),
        agent_config: None,
        branch: "br".into(),
        task: "Phase 2: Port TS package plugins/agent-aider".into(),
        workspace_path: None,
        runtime_handle: None,
        runtime: "tmux".into(),
        activity: None,
        created_at: now_ms(),
        cost: None,
        issue_id: Some("22".into()),
        issue_url: Some("https://github.com/duonghb53/ao-rs/issues/22".into()),
    };
    assert_eq!(
        session_display_title(&s),
        "#22 Phase 2: Port TS package plugins/agent-aider"
    );

    s.issue_id = None;
    assert_eq!(
        session_display_title(&s),
        "Phase 2: Port TS package plugins/agent-aider"
    );
}

#[test]
fn start_parses_run_flags() {
    let cli = Cli::try_parse_from([
        "ao-rs",
        "start",
        "--run",
        "--port",
        "4321",
        "--interval",
        "9",
        "--open",
    ])
    .unwrap();
    match cli.command {
        Command::Start {
            run,
            port,
            interval,
            open,
            ..
        } => {
            assert!(run);
            assert_eq!(port, 4321);
            assert_eq!(interval, Some(9));
            assert!(open);
        }
        _ => panic!("expected Start command"),
    }
}

#[test]
fn stop_parses_flags() {
    let cli = Cli::try_parse_from(["ao-rs", "stop", "--all", "--purge-session"]).unwrap();
    match cli.command {
        Command::Stop { all, purge_session } => {
            assert!(all);
            assert!(purge_session);
        }
        _ => panic!("expected Stop command"),
    }
}

fn fake_pr(number: u32) -> PullRequest {
    PullRequest {
        number,
        url: format!("https://github.com/acme/widgets/pull/{number}"),
        title: "fix the widgets".into(),
        owner: "acme".into(),
        repo: "widgets".into(),
        branch: "ao-3a4b5c6d".into(),
        base_branch: "main".into(),
        is_draft: false,
    }
}

fn fake_session() -> Session {
    Session {
        id: SessionId("3a4b5c6d-aaaa-bbbb-cccc-dddd".into()),
        project_id: "demo".into(),
        status: SessionStatus::Working,
        agent: "claude-code".into(),
        agent_config: None,
        branch: "ao-3a4b5c6d".into(),
        task: "fix the widgets".into(),
        workspace_path: None,
        runtime_handle: Some("3a4b5c6d".into()),
        runtime: "tmux".into(),
        activity: None,
        created_at: now_ms(),
        cost: None,
        issue_id: None,
        issue_url: None,
    }
}

#[test]
fn spawn_resolves_project_id_from_ao_rs_yaml_by_matching_repo_path() {
    // Regression test for "project defaults to demo so config is ignored".
    // We should pick the project id whose `projects.*.path` matches the repo root.
    let repo_dir = unique_temp_dir("repo-root-match");
    std::fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let mut projects = HashMap::new();
    projects.insert(
        "ao-rs".to_string(),
        ProjectConfig {
            name: None,
            repo: "duonghb53/ao-rs".into(),
            path: repo_dir.to_string_lossy().to_string(),
            default_branch: "main".into(),
            session_prefix: None,
            branch_namespace: None,
            runtime: None,
            agent: None,
            workspace: None,
            tracker: None,
            scm: None,
            symlinks: vec![],
            post_create: vec![],
            agent_config: Some(AgentConfig {
                permissions: "permissionless".into(),
                rules: Some("rules from config".into()),
                rules_file: None,
                model: None,
                orchestrator_model: None,
                opencode_session_id: None,
            }),
            orchestrator: None,
            worker: None,
            reactions: HashMap::new(),
            agent_rules: None,
            agent_rules_file: None,
            orchestrator_rules: None,
            orchestrator_session_strategy: None,
            opencode_issue_session_strategy: None,
        },
    );
    let cfg = AoConfig {
        port: 3000,
        ready_threshold_ms: 300_000,
        poll_interval: 10,
        terminal_port: None,
        direct_terminal_port: None,
        power: None,
        defaults: Some(DefaultsConfig {
            runtime: "tmux".into(),
            agent: "cursor".into(),
            workspace: "worktree".into(),
            tracker: "github".into(),
            branch_namespace: None,
            notifiers: vec![],
            orchestrator: None,
            worker: None,
            orchestrator_rules: None,
        }),
        projects,
        reactions: HashMap::new(),
        notification_routing: Default::default(),
        notifiers: HashMap::new(),
        plugins: vec![],
    };

    let config_path = AoConfig::path_in(&repo_dir);
    cfg.save_to(&config_path).unwrap();

    let loaded = AoConfig::load_from_or_default_with_warnings(&config_path)
        .unwrap()
        .config;
    let project_id = resolve_project_id(&repo_dir, &loaded, None);
    assert_eq!(project_id, "ao-rs");

    // And that means spawn would see the right per-project config.
    let proj = loaded.projects.get(&project_id).unwrap();
    assert_eq!(
        proj.agent_config.as_ref().unwrap().permissions,
        "permissionless"
    );
    assert_eq!(
        proj.agent_config.as_ref().unwrap().rules.as_deref(),
        Some("rules from config")
    );

    // And the right defaults.
    assert_eq!(loaded.defaults.as_ref().unwrap().agent, "cursor");

    let _ = std::fs::remove_file(&config_path);
    let _ = std::fs::remove_dir_all(&repo_dir);
}

// ---- pr_column --------------------------------------------------------

#[test]
fn pr_column_none_pr_is_dash() {
    assert_eq!(pr_column(None, None, None), "-");
    // Even if somehow state/ci were available, no PR still means dash.
    assert_eq!(
        pr_column(None, Some(PrState::Open), Some(CiStatus::Passing)),
        "-"
    );
}

#[test]
fn pr_column_open_pr_shows_state_and_ci() {
    let pr = fake_pr(42);
    assert_eq!(
        pr_column(Some(&pr), Some(PrState::Open), Some(CiStatus::Passing)),
        "#42 open/passing"
    );
    assert_eq!(
        pr_column(Some(&pr), Some(PrState::Open), Some(CiStatus::Failing)),
        "#42 open/failing"
    );
}

#[test]
fn pr_column_merged_drops_ci_suffix() {
    // GitHub stops serving check data for merged PRs; reporting "passing"
    // would be a lie. Collapse to just `#N merged`.
    let pr = fake_pr(7);
    assert_eq!(
        pr_column(Some(&pr), Some(PrState::Merged), Some(CiStatus::Passing)),
        "#7 merged"
    );
    // Closed gets the same treatment.
    assert_eq!(
        pr_column(Some(&pr), Some(PrState::Closed), None),
        "#7 closed"
    );
}

#[test]
fn pr_column_missing_state_or_ci_falls_back_to_question_mark() {
    // If `gh` flaked mid-row, show `?` for the unknown bit rather than
    // bailing the entire row. The `-` already means "no PR at all" — we
    // need a distinct cell for "PR exists but we couldn't read it".
    let pr = fake_pr(11);
    assert_eq!(pr_column(Some(&pr), None, None), "#11 ?/?");
    assert_eq!(
        pr_column(Some(&pr), Some(PrState::Open), None),
        "#11 open/?"
    );
    // And the symmetric case — state unknown but CI known. Fetches
    // are independent, either can succeed alone.
    assert_eq!(
        pr_column(Some(&pr), None, Some(CiStatus::Passing)),
        "#11 ?/passing"
    );
}

// ---- format_pr_report -------------------------------------------------

#[test]
fn format_pr_report_green_pr_has_no_blockers_section() {
    let pr = fake_pr(42);
    let session = fake_session();
    let readiness = MergeReadiness {
        mergeable: true,
        ci_passing: true,
        approved: true,
        no_conflicts: true,
        blockers: vec![],
    };
    let out = format_pr_report(
        &session,
        &pr,
        PrState::Open,
        CiStatus::Passing,
        ReviewDecision::Approved,
        &readiness,
    );
    assert!(out.contains("#42 fix the widgets"));
    assert!(out.contains("state:   open"));
    assert!(out.contains("CI:      passing"));
    assert!(out.contains("review:  approved"));
    assert!(out.contains("mergeable: yes"));
    // Blocker section is elided when the list is empty — keeps the
    // happy-path output compact.
    assert!(!out.contains("blockers:"), "got:\n{out}");
}

#[test]
fn format_pr_report_blocked_pr_lists_every_blocker() {
    let pr = fake_pr(42);
    let session = fake_session();
    let readiness = MergeReadiness {
        mergeable: false,
        ci_passing: false,
        approved: false,
        no_conflicts: false,
        blockers: vec![
            "CI is failing".into(),
            "Changes requested in review".into(),
            "Merge conflicts".into(),
        ],
    };
    let out = format_pr_report(
        &session,
        &pr,
        PrState::Open,
        CiStatus::Failing,
        ReviewDecision::ChangesRequested,
        &readiness,
    );
    assert!(out.contains("mergeable: no"));
    assert!(out.contains("blockers:"));
    assert!(out.contains("- CI is failing"));
    assert!(out.contains("- Changes requested in review"));
    assert!(out.contains("- Merge conflicts"));
    assert!(out.contains("review:  changes_requested"));
}

#[test]
fn format_pr_report_includes_short_id_and_full_uuid() {
    // Both forms are useful: short-id for copy-paste into subsequent
    // commands, full uuid so the user can disambiguate if they've got
    // colliding short prefixes.
    let pr = fake_pr(1);
    let session = fake_session();
    let readiness = MergeReadiness {
        mergeable: true,
        ci_passing: true,
        approved: true,
        no_conflicts: true,
        blockers: vec![],
    };
    let out = format_pr_report(
        &session,
        &pr,
        PrState::Open,
        CiStatus::Passing,
        ReviewDecision::Approved,
        &readiness,
    );
    assert!(out.contains("3a4b5c6d-aaaa-bbbb-cccc-dddd"));
    assert!(out.contains("short 3a4b5c6d"));
}

// ---- label helpers ----------------------------------------------------

#[test]
fn label_helpers_match_variant_shape() {
    // Cheap guard so a future variant addition doesn't silently get an
    // empty or wrong label. Pairs with the `#[non_exhaustive]`-free
    // nature of these enums — adding a variant forces the match to
    // update.
    assert_eq!(pr_state_label(PrState::Open), "open");
    assert_eq!(pr_state_label(PrState::Merged), "merged");
    assert_eq!(pr_state_label(PrState::Closed), "closed");
    assert_eq!(ci_status_label(CiStatus::Pending), "pending");
    assert_eq!(ci_status_label(CiStatus::None), "none");
    assert_eq!(
        review_decision_label(ReviewDecision::ChangesRequested),
        "changes_requested"
    );
    assert_eq!(review_decision_label(ReviewDecision::None), "none");
}

// ---- local issues ------------------------------------------------------

#[test]
fn slugify_filename_is_stable_and_non_empty() {
    assert_eq!(
        slugify_filename("Fix CI: core/lifecycle"),
        "fix-ci-core-lifecycle"
    );
    assert_eq!(slugify_filename("   "), "issue");
}

#[test]
fn next_local_issue_number_picks_max_plus_one() {
    let dir = std::env::temp_dir().join(format!("ao-cli-issue-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0001-foo.md"), "# a").unwrap();
    std::fs::write(dir.join("0007-bar.md"), "# b").unwrap();
    std::fs::write(dir.join("nope.md"), "# c").unwrap();

    let n = next_local_issue_number(&dir).unwrap();
    assert_eq!(n, 8);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_issue_id_from_filename_accepts_nnnn_slug_md() {
    assert_eq!(
        local_issue_id_from_filename("0001-test-local-issue.md"),
        Some(1)
    );
    assert_eq!(local_issue_id_from_filename("nope.md"), None);
    assert_eq!(local_issue_id_from_filename("1-bad.md"), None);
}

#[test]
fn collect_local_issue_entries_sorts_by_id() {
    let dir = std::env::temp_dir().join(format!("ao-cli-issue-collect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0003-c.md"), "# c").unwrap();
    std::fs::write(dir.join("0001-a.md"), "# a").unwrap();
    let v = collect_local_issue_entries(&dir).unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].0, 1);
    assert_eq!(v[1].0, 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parse_local_issue_markdown_reads_heading_and_body() {
    let md = "# Fix CI\n\nhello\n\n## Notes\n\n- x\n";
    let (t, b) = parse_local_issue_markdown(md);
    assert_eq!(t, "Fix CI");
    assert!(b.contains("hello"));
    assert!(b.contains("## Notes"));
}

#[test]
fn local_issue_ids_from_path_matches_nnnn_slug_md() {
    let p = std::path::PathBuf::from("/tmp/docs/issues/0007-my-task.md");
    let (id, branch) = local_issue_ids_from_path(&p).unwrap();
    assert_eq!(id, "local-0007");
    assert_eq!(branch, "feat/local-0007-my-task");
}

#[test]
fn parse_local_issue_id_token_accepts_padding() {
    assert_eq!(parse_local_issue_id_token("1"), Some(1));
    assert_eq!(parse_local_issue_id_token("0001"), Some(1));
    assert_eq!(parse_local_issue_id_token("12345"), None);
    assert_eq!(parse_local_issue_id_token("docs/foo.md"), None);
}

#[test]
fn resolve_local_issue_for_show_finds_by_id() {
    let root = std::env::temp_dir().join(format!("ao-issue-show-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let issues = root.join("docs/issues");
    std::fs::create_dir_all(&issues).unwrap();
    std::fs::write(issues.join("0002-b.md"), "# B\n").unwrap();
    let p = resolve_local_issue_for_show(&root, "2").unwrap();
    assert!(p.ends_with("0002-b.md"));
    let _ = std::fs::remove_dir_all(&root);
}
