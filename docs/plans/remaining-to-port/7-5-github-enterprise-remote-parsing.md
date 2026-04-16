# 7.5 github enterprise remote parsing

Status: planned

## Why

`parse_github_remote` is strict and only supports github.com-shaped remotes (`owner/repo`). This blocks GitHub Enterprise or nonstandard remote URL shapes.

## Current state (ao-rs)

- `crates/plugins/scm-github/src/lib.rs` (and parsing helpers) enforce strict `owner/repo`.
- Extra path segments are rejected to avoid silently using the wrong repo.

## Target behavior (parity/maturity)

- Support common GitHub Enterprise remote URL patterns without sacrificing safety.

## Proposed approach

1. Extend remote parsing to accept:
   - `https://ghe.example.com/owner/repo.git`
   - `git@ghe.example.com:owner/repo.git`
2. Keep safety rules:
   - Still reject URLs with extra segments beyond `owner/repo`.
   - Normalize `.git` suffix.
3. Add an optional config override:
   - Allow specifying `repo: owner/repo` explicitly in config to bypass remote parsing.

## Files to change

- `crates/plugins/scm-github/src/lib.rs` (remote parsing helper)
- `crates/ao-core/src/config.rs` (ensure explicit `repo` config is used first, if not already)

## Acceptance criteria

- Projects hosted on GitHub Enterprise can use SCM GitHub plugin successfully.
- Misformatted remotes still fail loudly with actionable error messages.

## Test plan

- Unit tests for remote parsing with multiple URL formats.

## Risks / notes

- Enterprise instances may have additional path prefixes; decide whether to support them or require explicit `repo` config.

