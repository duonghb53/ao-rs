//! `ao-rs` CLI.
//!
//! Subcommands:
//!   - `start`           — generate or load config file
//!   - `spawn`           — workspace-worktree → agent → runtime-tmux (`--task`, `--issue`, `--local-issue`)
//!   - `status`          — list persisted sessions; `--pr` adds PR/CI columns
//!   - `watch`           — run the LifecycleManager and stream events to stdout
//!   - `send`            — forward a message to a running session's agent
//!   - `pr`              — inspect GitHub PR state + CI + review for a session
//!   - `update`          — check for / perform CLI upgrade
//!   - `doctor`          — check environment: required tools, auth, config
//!   - `config-help`     — print a concise config guide
//!   - `review-check`    — scan PRs for new comments and forward to agents
//!   - `session restore` — respawn a terminated session in-place
//!   - `issue new` / `issue list` / `issue show` — markdown issues under `docs/issues/`
//!
//! `watch` is guarded by a pidfile at `~/.ao-rs/lifecycle.pid` so running
//! it twice concurrently fails fast instead of racing two polling loops.

mod cli;
mod commands;
mod session;

use std::time::Duration;

use clap::Parser;

use crate::cli::args::{
    Cli, Command, IssueAction, OpenTarget, PluginAction, SessionAction, SetupAction,
};
use crate::commands::start::StartOptions;

fn init_tracing(dev: bool) {
    // Cheap tracing setup — honours RUST_LOG. Without this, tracing calls in the
    // lifecycle loop would be silent.
    let _ = if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init()
    } else if dev {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "info,ao_cli=debug,ao_core=debug",
            ))
            .try_init()
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn,ao_core=info"))
            .try_init()
    };
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let dev = matches!(cli.command, Command::Start { dev: true, .. });
    init_tracing(dev);

    match cli.command {
        Command::Start {
            repo,
            run,
            no_dashboard,
            no_orchestrator,
            port,
            interval,
            open,
            rebuild,
            dev: _,
            interactive,
        } => {
            commands::start::start(StartOptions {
                repo,
                run,
                no_dashboard,
                no_orchestrator,
                port,
                interval_override: interval.map(Duration::from_secs),
                open,
                rebuild,
                interactive,
            })
            .await
        }
        Command::Spawn {
            task,
            issue,
            local_issue,
            repo,
            default_branch,
            project,
            no_prompt,
            force,
            prompt,
            claim_pr,
            assign_on_github,
            open,
            agent,
            runtime,
            template,
        } => {
            commands::spawn::spawn(
                task,
                issue,
                local_issue,
                repo,
                default_branch,
                project,
                no_prompt,
                force,
                prompt,
                claim_pr,
                assign_on_github,
                open,
                agent,
                runtime,
                template,
            )
            .await
        }
        Command::BatchSpawn {
            issues,
            repo,
            default_branch,
            project,
            no_prompt,
            force,
            agent,
            runtime,
            template,
        } => {
            commands::spawn::batch_spawn(
                issues,
                repo,
                default_branch,
                project,
                no_prompt,
                force,
                agent,
                runtime,
                template,
            )
            .await
        }
        Command::Status {
            project,
            pr,
            cost,
            all,
            json,
            watch,
            interval,
        } => {
            commands::status::status(commands::status::StatusOptions {
                project_filter: project,
                with_pr: pr,
                with_cost: cost,
                show_all: all,
                json,
                watch,
                interval_secs: interval,
            })
            .await
        }
        Command::Watch { interval } => {
            commands::watch::watch(interval.map(Duration::from_secs)).await
        }
        Command::Dashboard {
            port,
            interval,
            open,
        } => {
            if open {
                cli::browser::spawn_open_dashboard_browser(port);
            }
            commands::dashboard::dashboard(port, interval.map(Duration::from_secs)).await
        }
        Command::Open {
            port,
            new_window,
            target,
        } => commands::open::open(port, new_window, target.unwrap_or(OpenTarget::Dashboard)).await,
        Command::Stop { all, purge_session } => commands::stop::stop(all, purge_session).await,
        Command::Send {
            session,
            message,
            file,
            no_wait,
            timeout,
        } => commands::send::send(session, message, file, no_wait, timeout).await,
        Command::Pr { session } => commands::pr::pr(session).await,
        Command::Kill {
            session,
            purge_session,
        } => commands::kill::kill(session, purge_session).await,
        Command::Cleanup { project, dry_run } => commands::cleanup::cleanup(project, dry_run).await,
        Command::Update {
            check,
            skip_smoke,
            smoke_only,
        } => commands::update::update(check, skip_smoke, smoke_only).await,
        Command::Doctor => commands::doctor::doctor().await,
        Command::ConfigHelp => commands::config_help::config_help().await,
        Command::ReviewCheck { project, dry_run } => {
            commands::review_check::review_check(project, dry_run).await
        }
        Command::Verify {
            list,
            fail,
            comment,
            target,
        } => commands::verify::verify(list, fail, comment, target).await,
        Command::Plugin { action } => match action {
            PluginAction::List => commands::plugin::list().await,
            PluginAction::Info { name } => commands::plugin::info(name).await,
        },
        Command::Session { action } => match action {
            SessionAction::Restore { session } => session::restore::restore(session).await,
            SessionAction::Attach { session } => session::attach::attach(session).await,
            SessionAction::ClaimPr {
                pr,
                session,
                assign_on_github,
            } => session::claim_pr::claim_pr(pr, session, assign_on_github).await,
            SessionAction::Remap {
                session,
                workspace,
                runtime_handle,
                force,
            } => session::remap::remap(session, workspace, runtime_handle, force).await,
        },
        Command::Issue { action } => match action {
            IssueAction::New { title, body, repo } => {
                cli::local_issue::issue_new(title, body, repo).await
            }
            IssueAction::List { repo } => cli::local_issue::issue_list(repo),
            IssueAction::Show { target, repo } => cli::local_issue::issue_show(target, repo),
        },
        Command::Setup { action } => match action {
            SetupAction::Openclaw {
                repo,
                url,
                token,
                routing_preset,
                non_interactive,
                dry_run,
            } => {
                commands::setup::openclaw::openclaw(
                    repo,
                    url,
                    token,
                    routing_preset,
                    non_interactive,
                    dry_run,
                )
                .await
            }
        },
    }
}

#[cfg(test)]
mod tests;
