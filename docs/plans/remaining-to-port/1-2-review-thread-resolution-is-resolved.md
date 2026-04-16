# 1.2 Review thread resolution (is_resolved)

Status: planned

## Why

`pending_comments` currently cannot tell **resolved** vs **unresolved** review threads, so `changes-requested` reactions risk spamming agents with already-addressed comments.

## Current state (ao-rs)

- `crates/plugins/scm-github/src/lib.rs`: `pending_comments()` uses REST PR review comments pagination.
- `crates/plugins/scm-github/src/parse.rs`: REST comment parsing sets `is_resolved: false` always (explicit TODO note).

## Target behavior (ao-ts parity)

- Use GraphQL `reviewThreads` to retrieve thread resolution state and map it into the comment model (`is_resolved: true|false`).

## Proposed approach

1. Add a GraphQL query (via `gh api graphql`) to fetch review threads for a PR:
   - thread id, isResolved, and comment nodes (ids, body, author, createdAt, path/position if needed)
2. Update `Scm::pending_comments` implementation in `scm-github` to prefer GraphQL:
   - Fall back to current REST path on GraphQL failure (optional, but keeps resilience).
3. Update parsing in `parse.rs` to map GraphQL payload into the existing `ReviewComment` type, including `is_resolved`.

## Files to change

- `crates/plugins/scm-github/src/lib.rs`
  - Replace or augment `pending_comments()` implementation to call GraphQL.
- `crates/plugins/scm-github/src/parse.rs`
  - Add parser for GraphQL `reviewThreads` response.
- (Optional) `crates/plugins/scm-github/src/graphql_batch.rs`
  - Reuse patterns for GraphQL calls (chunking, query strings), if appropriate.

## Acceptance criteria

- For a PR with resolved threads, returned comments include `is_resolved: true` for those threads.
- For unresolved threads, `is_resolved: false`.
- Existing consumers can ignore `is_resolved` without breaking.

## Test plan

- Unit tests for GraphQL response parsing in `parse.rs` using fixture JSON.
- (Optional) Integration test gated behind env (requires `gh` auth) or keep to unit parse tests only.

## Risks / notes

- GitHub GraphQL pagination: large PRs may require pagination (`first:` + `after:` cursors).
- Decide whether to return only unresolved comments, or return all with flags (prefer returning all with `is_resolved` to match current behavior).

