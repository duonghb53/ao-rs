//! `ao-rs` CLI.
//!
//! Subcommands:
//!   - `start`           — generate or load config file
//!   - `spawn`           — workspace-worktree → agent → runtime-tmux (`--task`, `--issue`, `--local-issue`)
//!   - `status`          — list persisted sessions; `--pr` adds PR/CI columns
//!   - `watch`           — run the LifecycleManager and stream events to stdout
//!   - `send`            — forward a message to a running session's agent
//!   - `pr`              — inspect GitHub PR state + CI + review for a session
//!   - `doctor`          — check environment: required tools, auth, config
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

use crate::cli::args::{Cli, Command, IssueAction, SessionAction};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Cheap tracing setup — honours RUST_LOG, defaults to warn for our crates.
    // Without this, tracing::warn! calls in the lifecycle loop would be silent.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,ao_core=info")),
        )
        .try_init();

    let cli = Cli::parse();

    match cli.command {
        Command::Start {
            repo,
            run,
            port,
            interval,
            open,
        } => commands::start::start(repo, run, port, interval.map(Duration::from_secs), open).await,
        Command::Spawn {
            task,
            issue,
            local_issue,
            repo,
            default_branch,
            project,
            no_prompt,
            force,
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
        } => commands::status::status(project, pr, cost, all).await,
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
        Command::Send { session, message } => commands::send::send(session, message).await,
        Command::Pr { session } => commands::pr::pr(session).await,
        Command::Kill { session } => commands::kill::kill(session).await,
        Command::Cleanup { project, dry_run } => commands::cleanup::cleanup(project, dry_run).await,
        Command::Doctor => commands::doctor::doctor().await,
        Command::ReviewCheck { project, dry_run } => {
            commands::review_check::review_check(project, dry_run).await
        }
        Command::Session { action } => match action {
            SessionAction::Restore { session } => session::restore::restore(session).await,
            SessionAction::Attach { session } => session::attach::attach(session).await,
        },
        Command::Issue { action } => match action {
            IssueAction::New { title, body, repo } => {
                cli::local_issue::issue_new(title, body, repo).await
            }
            IssueAction::List { repo } => cli::local_issue::issue_list(repo),
            IssueAction::Show { target, repo } => cli::local_issue::issue_show(target, repo),
        },
    }
}

#[cfg(test)]
mod tests;
