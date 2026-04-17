//! `ao-rs spawn` / `batch-spawn`.

use std::path::PathBuf;
use std::time::Duration;

use ao_core::{
    build_prompt, now_ms, Agent, AoConfig, LoadedConfig, Session, SessionId, SessionManager,
    SessionStatus, Tracker, Workspace, WorkspaceCreateConfig,
};
use std::env;
use ao_plugin_tracker_github::GitHubTracker;
use ao_plugin_tracker_linear::LinearTracker;
use ao_plugin_workspace_worktree::WorktreeWorkspace;

use crate::cli::agent_config::resolve_agent_config;
use crate::cli::local_issue::{
    format_local_issue_context, local_issue_ids_from_path, parse_local_issue_markdown,
    resolve_path_in_repo,
};
use crate::cli::plugins::{select_agent, select_runtime, DuplicateIssue};
use crate::cli::printing::{print_config_warnings, short_id};
use crate::cli::project::{resolve_project_id, resolve_repo_root};
use crate::cli::spawn_helpers::{
    git_safe_branch_namespace, issue_branch_name, shell_escape_single_quotes,
    spawn_template_by_name, tmux_send_keys_literal_no_enter,
};
#[allow(clippy::too_many_arguments)]
pub async fn spawn(
    task: Option<String>,
    issue: Option<String>,
    local_issue: Option<PathBuf>,
    repo: Option<PathBuf>,
    default_branch: String,
    project: Option<String>,
    no_prompt: bool,
    force: bool,
    prompt: Option<String>,
    claim_pr: Option<String>,
    assign_on_github: bool,
    open: bool,
    agent_name: Option<String>,
    runtime_name: Option<String>,
    template: Option<String>,
    spawned_by: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Auto-detect parent orchestrator from `AO_SESSION_ID` when the CLI
    // flag wasn't provided. Set by the agent plugins when they launch,
    // so `ao-rs spawn` invoked from inside an orchestrator's own shell
    // links workers back to it automatically.
    let resolved_spawned_by = spawned_by
        .or_else(|| env::var("AO_SESSION_ID").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(SessionId);
    // ---- 1. Resolve repo path ----
    let repo_path = resolve_repo_root(repo)?;
    if !repo_path.join(".git").exists() {
        return Err(format!("not a git repo: {}", repo_path.display()).into());
    }
    // Load config early so we can pick a stable project id.
    //
    // Missing config is silently empty; a broken YAML is a loud error.
    let config_path = AoConfig::path_in(&repo_path);
    let LoadedConfig {
        config: ao_config,
        warnings,
    } = AoConfig::load_from_or_default_with_warnings(&config_path)
        .map_err(|e| format!("failed to load {}: {e}", config_path.display()))?;
    print_config_warnings(&config_path, &warnings);

    // Resolve project id:
    // - explicit `--project` wins
    // - otherwise try to match by `projects.*.path == repo_root`
    // - otherwise default to repo directory name
    let project = resolve_project_id(&repo_path, &ao_config, project);
    let project_config = ao_config.projects.get(&project);

    // ---- 1b. Resolve task, branch, issue metadata, and project config ----
    // One of: --issue (GitHub), --local-issue (markdown file), --task (free-form).
    //
    // `resolved_task` is stored in `Session::task` for display in `ao-rs status`.
    // For issue-first, it's just the title (the full issue body is rendered
    // by the prompt builder via `Tracker::generate_prompt`).
    // `issue_context` is the pre-formatted issue section for the prompt builder.
    let template_context = template
        .as_deref()
        .map(spawn_template_by_name)
        .transpose()?;

    let (resolved_task, issue_based_branch, resolved_issue_id, resolved_issue_url, issue_context) =
        if let Some(ref id) = issue {
            // Normalize: strip leading `#` so `#42` and `42` match the stored
            // `issue_id` (which is always a bare number from `gh issue view`).
            let normalized = id.strip_prefix('#').unwrap_or(id);

            // Duplicate detection: reject if another active session is on this issue.
            if !force {
                let manager = SessionManager::with_default();
                let dupes = manager.find_by_issue_id(normalized).await?;
                if !dupes.is_empty() {
                    let short = short_id(&dupes[0].id);
                    return Err(DuplicateIssue {
                        issue_id: normalized.to_string(),
                        session_short: short,
                    }
                    .into());
                }
            }
            println!("→ fetching issue {}...", id);
            let tracker_name = project_config
                .and_then(|p| p.tracker.as_ref())
                .and_then(|t| t.plugin.as_deref())
                .or_else(|| ao_config.defaults.as_ref().map(|d| d.tracker.as_str()))
                .unwrap_or("github");

            let tracker: Box<dyn Tracker> = match tracker_name {
                "linear" => Box::new(LinearTracker::from_env()?),
                // Default + fallback: github
                _ => Box::new(GitHubTracker::from_repo(&repo_path).await?),
            };

            if assign_on_github {
                if let Err(e) = tracker.assign_to_me(normalized).await {
                    println!(
                        "note: --assign-on-github failed on tracker {}: {e}",
                        tracker.name()
                    );
                }
            }

            let fetched = tracker.get_issue(id).await?;
            // Generate structured issue context via the tracker plugin's
            // generate_prompt() — this is the extension point for custom
            // formatting (Linear cycle info, Jira sprint fields, etc.).
            let issue_branch = issue_branch_name(&fetched.id, &fetched.title, &fetched.labels);
            let ctx = tracker.generate_prompt(&fetched);
            println!("  issue:     #{} — {}", fetched.id, fetched.title);
            (
                fetched.title.clone(),
                Some(issue_branch),
                Some(fetched.id.clone()),
                Some(fetched.url.clone()),
                Some(ctx),
            )
        } else if let Some(ref li) = local_issue {
            let path = resolve_path_in_repo(&repo_path, li);
            if !path.is_file() {
                return Err(format!("local issue is not a file: {}", path.display()).into());
            }
            let (issue_id, _branch_suffix) = local_issue_ids_from_path(&path)?;
            if !force {
                let manager = SessionManager::with_default();
                let dupes = manager.find_by_issue_id(&issue_id).await?;
                if !dupes.is_empty() {
                    let short = short_id(&dupes[0].id);
                    return Err(DuplicateIssue {
                        issue_id: issue_id.clone(),
                        session_short: short,
                    }
                    .into());
                }
            }
            let text = std::fs::read_to_string(&path)?;
            let (title, body) = parse_local_issue_markdown(&text);
            let ctx = format_local_issue_context(&title, &path, &body);
            println!("→ local issue: {}", path.display());
            println!("  id:        {issue_id} — {title}");
            let issue_branch = issue_branch_name(&issue_id, &title, &[]);
            (title, Some(issue_branch), Some(issue_id), None, Some(ctx))
        } else {
            (task.unwrap(), None, None, None, None)
        };

    if assign_on_github && issue.is_none() {
        // Best-effort: if the user passed `--claim-pr` without `--issue`, try to assign
        // by PR number using the repo's GitHub origin scope.
        if let Some((Some(n), _url)) = claim_pr.as_deref().map(parse_claim_pr) {
            if let Ok(tracker) = GitHubTracker::from_repo(&repo_path).await {
                if let Err(e) = tracker.assign_to_me(&n.to_string()).await {
                    println!("note: --assign-on-github failed: {e}");
                }
            } else {
                println!("note: --assign-on-github requires a GitHub origin remote");
            }
        } else if claim_pr.is_some() {
            println!("note: --assign-on-github needs a numeric PR (or URL ending in /<number>)");
        }
    }

    // Worker spawn agent resolution (matches ao-ts `resolveAgentSelection` for role=worker):
    // CLI `--agent` → `projects.*.worker.agent` → `projects.*.agent` →
    // `defaults.worker.agent` → `defaults.agent` → claude-code.
    let agent_name = agent_name
        .or_else(|| {
            project_config.and_then(|p| {
                p.worker
                    .as_ref()
                    .and_then(|w| w.agent.clone())
                    .or_else(|| p.agent.clone())
            })
        })
        .or_else(|| {
            ao_config
                .defaults
                .as_ref()
                .and_then(|d| d.worker.as_ref().and_then(|w| w.agent.clone()))
        })
        .or_else(|| ao_config.defaults.as_ref().map(|d| d.agent.clone()))
        .unwrap_or_else(|| "claude-code".to_string());
    let runtime_name = runtime_name
        .or_else(|| project_config.and_then(|p| p.runtime.clone()))
        .or_else(|| ao_config.defaults.as_ref().map(|d| d.runtime.clone()))
        .unwrap_or_else(|| "tmux".to_string());

    // ---- 2. Allocate ids ----
    let session_id = SessionId::new();
    // Short id is what tmux + worktree dirs see — uuid is too long for a tmux name.
    let short_id: String = session_id.0.chars().take(8).collect();
    // short_id is used for session/worktree uniqueness; issue-based branches
    // are derived deterministically from issue_id + slugified title.
    let branch_namespace = project_config
        .and_then(|p| p.branch_namespace.clone())
        .or_else(|| {
            ao_config
                .defaults
                .as_ref()
                .and_then(|d| d.branch_namespace.clone())
        })
        .map(|s| git_safe_branch_namespace(&s));
    let branch = match issue_based_branch {
        Some(b) => b,
        None => match branch_namespace.as_deref() {
            Some(ns) => format!("{ns}/{short_id}"),
            None => format!("ao-{short_id}"),
        },
    };

    println!("→ project:   {project}");
    println!("→ session:   {session_id}");
    println!("→ short id:  {short_id}");
    println!("→ branch:    {branch}");
    println!();

    // ---- 3. Workspace: git worktree add ----
    let workspace = WorktreeWorkspace::new();
    let symlinks = project_config
        .map(|p| p.symlinks.clone())
        .unwrap_or_default();
    let post_create = project_config
        .map(|p| p.post_create.clone())
        .unwrap_or_default();
    let workspace_cfg = WorkspaceCreateConfig {
        project_id: project.clone(),
        session_id: short_id.clone(),
        branch: branch.clone(),
        repo_path: repo_path.clone(),
        default_branch,
        symlinks,
        post_create,
    };

    println!("→ creating worktree...");
    let workspace_path = workspace.create(&workspace_cfg).await?;
    println!("  worktree:  {}", workspace_path.display());

    // Guard: clean up the worktree on any subsequent error. Without this,
    // a failure in steps 4-6 (save, runtime create, send_message) leaves a
    // ghost worktree directory with no Session record pointing at it.
    let post_workspace_result: Result<Session, Box<dyn std::error::Error>> = async {
        let (claimed_pr_number, claimed_pr_url) = claim_pr
            .as_deref()
            .map(parse_claim_pr)
            .unwrap_or((None, None));

        // Build the Session and persist it. Slice 1 Phase A: disk-backed.
        let mut session = Session {
            id: session_id.clone(),
            project_id: project.clone(),
            status: SessionStatus::Spawning,
            agent: agent_name.clone(),
            agent_config: None,
            branch: branch.clone(),
            task: resolved_task,
            workspace_path: Some(workspace_path.clone()),
            runtime_handle: None,
            runtime: runtime_name.clone(),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: resolved_issue_id,
            issue_url: resolved_issue_url,
            claimed_pr_number,
            claimed_pr_url,
            initial_prompt_override: prompt.clone(),
            spawned_by: resolved_spawned_by.clone(),
        };

        let manager = SessionManager::with_default();
        manager.save(&session).await?;

        // ---- 4. Agent: get launch command + env ----
        let agent_config = project_config.and_then(|p| p.agent_config.as_ref());
        let resolved_agent_config = resolve_agent_config(agent_config, &repo_path);
        session.agent_config = resolved_agent_config.clone();
        let agent: Box<dyn Agent> = select_agent(&agent_name, resolved_agent_config.as_ref());
        let env = agent.environment(&session);
        let initial_prompt = build_prompt(
            &session,
            project_config,
            issue_context.as_deref(),
            template_context.as_deref(),
        );
        let initial_prompt = if agent_name == "cursor" {
            format!(
                "Execute the task now. Use tools (edit files, run commands) as needed.\n\
If you need clarification, ask one question; otherwise proceed.\n\n\
{initial_prompt}"
            )
        } else {
            initial_prompt
        };

        // Cursor: match TS behavior by embedding prompt in launch command (`agent ... -- '<prompt>'`)
        // so the agent starts working immediately after trust.
        let (launch_command, post_launch_prompt) = if agent_name == "cursor" && !no_prompt {
            let prompt_arg = shell_escape_single_quotes(&initial_prompt);
            (
                format!("{} -- {prompt_arg}", agent.launch_command(&session)),
                None,
            )
        } else {
            (agent.launch_command(&session), Some(initial_prompt))
        };

        // ---- 5. Runtime: spawn session running the agent ----
        let runtime = select_runtime(&runtime_name);
        println!("→ spawning runtime '{runtime_name}': `{launch_command}`");
        let handle = runtime
            .create(&short_id, &workspace_path, &launch_command, &env)
            .await?;

        // Persist the runtime handle + transition status — so `ao-rs status` shows
        // the spawned session as Working, not Spawning.
        session.runtime_handle = Some(handle.clone());
        session.status = SessionStatus::Working;
        manager.save(&session).await?;

        // ---- 6. Deliver initial prompt (post-launch) ----
        //
        // Cursor Agent shows an interactive "Workspace Trust Required" prompt
        // for new worktree paths; without accepting it, the agent never starts.
        // Worktrees are fresh per session, so it is safe to auto-trust here.
        if agent_name == "cursor" {
            tokio::time::sleep(Duration::from_millis(800)).await;
            tmux_send_keys_literal_no_enter(&handle, "a").await;
        }

        let Some(initial_prompt) = post_launch_prompt else {
            if no_prompt {
                println!("→ skipping initial prompt (--no-prompt)");
            }
            return Ok(session);
        };

        if no_prompt {
            println!("→ skipping initial prompt (--no-prompt)");
        } else {
            // Give the agent UI time to initialize before we paste the prompt.
            let delay_ms = if agent_name == "cursor" { 9000 } else { 2500 };
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            println!("→ sending initial prompt: {initial_prompt:?}");
            runtime.send_message(&handle, &initial_prompt).await?;
            // Cursor Agent can still be mid-transition from the trust prompt to
            // the main UI; a second send shortly after makes startup robust.
            if agent_name == "cursor" {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                let _ = runtime.send_message(&handle, &initial_prompt).await;
            }
        }

        Ok(session)
    }
    .await;

    let session = match post_workspace_result {
        Ok(s) => s,
        Err(e) => {
            // Best-effort cleanup — if this also fails, log and surface the
            // original error so the user knows *why* the spawn failed.
            if let Err(cleanup_err) = workspace.destroy(&workspace_path).await {
                eprintln!("warning: failed to clean up worktree after spawn error: {cleanup_err}");
            }
            return Err(e);
        }
    };

    let handle = session.runtime_handle.as_deref().unwrap_or(&short_id);
    println!();
    println!("───────────────────────────────────────────────");
    println!("  ✓ session spawned & persisted");
    println!();
    println!("  attach:  tmux attach -t {handle}");
    println!("  kill:    tmux kill-session -t {handle}");
    println!("  status:  ao-rs status");
    println!(
        "  cleanup: cd {} && git worktree remove --force {}",
        repo_path.display(),
        workspace_path.display(),
    );
    println!("───────────────────────────────────────────────");

    if open {
        if session.runtime != "tmux" {
            println!("note: --open is currently only supported for the tmux runtime");
            return Ok(());
        }
        let Some(handle) = session.runtime_handle.as_deref() else {
            println!("note: session has no runtime handle; cannot attach");
            return Ok(());
        };

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new("tmux")
                .args(["attach-session", "-t", handle])
                .exec();
            return Err(format!("failed to exec tmux: {err}").into());
        }

        #[cfg(not(unix))]
        {
            println!("note: --open is not supported on this platform");
        }
    }

    Ok(())
}

fn parse_claim_pr(input: &str) -> (Option<u32>, Option<String>) {
    let trimmed = input.trim();
    let number_str = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if let Ok(n) = number_str.parse::<u32>() {
        return (Some(n), None);
    }

    // Keep it intentionally simple: store the URL verbatim, and if it ends with a numeric
    // path segment (e.g. .../pull/123), also store the number.
    let url = trimmed.to_string();
    let last = trimmed
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    let num = last.parse::<u32>().ok();
    (num, Some(url))
}

/// `ao-rs batch-spawn <issues...>` — spawn one session per issue.
///
/// Iterates the issue list sequentially, running the same spawn logic per
/// issue. Skips duplicates (another active session on the same issue) unless
/// `--force` is set. Prints a summary at the end.
#[allow(clippy::too_many_arguments)]
pub async fn batch_spawn(
    issues: Vec<String>,
    repo: Option<PathBuf>,
    default_branch: String,
    project: Option<String>,
    no_prompt: bool,
    force: bool,
    agent_name: Option<String>,
    runtime_name: Option<String>,
    template: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate the template once up front so we fail fast rather than after
    // spawning N-1 sessions.
    if let Some(ref name) = template {
        let _ = spawn_template_by_name(name)?;
    }

    let total = issues.len();
    let mut created = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;

    println!("→ batch-spawn: {} issue(s)", total);
    println!();

    for (i, issue_id) in issues.iter().enumerate() {
        println!("── [{}/{}] issue #{issue_id} ──", i + 1, total);

        match spawn(
            None,
            Some(issue_id.clone()),
            None,
            repo.clone(),
            default_branch.clone(),
            project.clone(),
            no_prompt,
            force,
            None,
            None,
            false,
            false,
            agent_name.clone(),
            runtime_name.clone(),
            template.clone(),
            None,
        )
        .await
        {
            Ok(()) => {
                created += 1;
            }
            Err(e) => {
                if e.downcast_ref::<DuplicateIssue>().is_some() {
                    println!("  ⊘ skipped: {e}");
                    skipped += 1;
                } else {
                    eprintln!("  ✗ failed: {e}");
                    failed += 1;
                }
            }
        }
        println!();
    }

    println!("───────────────────────────────────────────────");
    println!("  batch-spawn summary:");
    println!("    created: {created}");
    if skipped > 0 {
        println!("    skipped: {skipped} (duplicate)");
    }
    if failed > 0 {
        println!("    failed:  {failed}");
    }
    println!("───────────────────────────────────────────────");

    if failed > 0 {
        Err(format!("{failed} spawn(s) failed").into())
    } else {
        Ok(())
    }
}
