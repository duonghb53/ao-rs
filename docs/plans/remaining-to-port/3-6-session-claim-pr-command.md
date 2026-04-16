# 3.6 session claim-pr command

Status: planned

## Why

ao-ts can attach an existing PR to a session (`claim-pr`). ao-rs lacks a way to associate PRs created outside spawn flow.

## Current state (ao-rs)

- Session metadata includes branch and may include issue ids/urls.
- SCM plugin can detect PR by branch (`detect_pr`).
- No CLI subcommand to bind a PR to a session explicitly.

## Target behavior (ao-ts parity)

`ao session claim-pr <pr> [session] [--assign-on-github]`

## Proposed approach

1. Implement `ao-rs session claim-pr <pr> [session]`:
   - Resolve session (optional arg; default to most recent / prompt error).
   - Persist PR reference (owner/repo + number, url) into session.
2. Optional `--assign-on-github`:
   - Assign PR to current user (GitHub API via `gh`).

## Files to change

- `crates/ao-cli/src/cli/args.rs` (session subcommands)
- `crates/ao-cli/src/commands/session/claim_pr.rs` (new)
- `crates/ao-core/src/types.rs` (if new PR fields needed)
- `crates/ao-core/src/session_manager.rs` (persist updated session)

## Acceptance criteria

- After `claim-pr`, `ao-rs pr <session>` resolves to that PR even if branch detection fails.
- Stored PR reference survives restart.

## Test plan

- Unit test for PR reference parsing and session update persistence.

## Risks / notes

- Decide the single source of truth: stored PR ref vs branch detection fallback.

