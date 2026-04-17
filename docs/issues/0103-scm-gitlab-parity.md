# 5.6 scm-gitlab — parity with ao-ts

**Status**: Decision made — **parity-targeted**.

## Decision

The ao-ts repo has a first-class `packages/plugins/scm-gitlab` plugin that
implements the same `SCM` interface as `scm-github`. ao-rs targets parity with
that plugin (behavior, not transport — ao-ts shells out to `glab`, ao-rs uses
REST).

## Parity matrix (ao-ts → ao-rs)

Checked against `packages/plugins/scm-gitlab/src/index.ts` at HEAD.

| ao-ts method           | ao-rs method         | Port status | Note |
|------------------------|----------------------|-------------|------|
| `name`                 | `name`               | ✓           | returns `"gitlab"` |
| `verifyWebhook`        | `verify_webhook`     | **added**   | SHA-256 constant-time token compare on `x-gitlab-token` |
| `parseWebhook`         | `parse_webhook`      | **added**   | MR / push / tag_push / pipeline / note |
| `detectPR`             | `detect_pr`          | ✓           | REST MR list by source branch |
| `getPRState`           | `pr_state`           | ✓           | opened/merged/closed |
| `getPRSummary`         | `pr_summary`         | **added**   | state + title, additions/deletions always 0 (ao-ts parity) |
| `mergePR`              | `merge`              | **updated** | squash / rebase / merge |
| `closePR`              | `close_pr`           | **added**   | PUT merge_request with `state_event=close` |
| `getCIChecks`          | `ci_checks`          | **updated** | pipelines → jobs (per-job granularity, matches ao-ts) |
| `getCISummary`         | `ci_status`          | **updated** | "none" for merged/closed MRs when CI fetch fails |
| `getReviews`           | `reviews`            | **updated** | approvals + unresolved-discussion `changes_requested`, bot-filtered, deduped |
| `getReviewDecision`    | `review_decision`    | ✓           | approved / pending / none |
| `getPendingComments`   | `pending_comments`   | **updated** | bot authors filtered |
| `getAutomatedComments` | `automated_comments` | **added**   | bot-only notes with severity heuristic |
| `getMergeability`      | `mergeability`       | **updated** | early return for merged/closed; adds `blocking_discussions_resolved` + draft |

## GitHub vs GitLab API differences (worth recording)

- **Review decision**: GitHub exposes one `reviewDecision` enum; GitLab derives
  it from `approvals_required`/`approvals_left` + per-reviewer `approved_by`.
- **CI**: GitHub has `check_run`s (per-check granularity natively). GitLab
  pipelines roll up jobs — we list pipelines, pick the latest, then list its
  jobs to match GitHub's per-check shape.
- **Conflicts**: GitHub's `mergeable` bool bundles conflicts + branch
  protection + required reviews. GitLab splits these: `has_conflicts`,
  `blocking_discussions_resolved`, `merge_status` (`can_be_merged` /
  `cannot_be_merged` / `checking`).
- **Reviews**: GitLab doesn't have an explicit `changes_requested` review
  state. ao-ts synthesises one from unresolved resolvable discussions; we
  follow the same convention.
- **Webhook auth**: GitHub signs with HMAC-SHA256 over the raw body. GitLab
  sends a plain token in `x-gitlab-token` — we compare in constant time.

## Implementation notes

- Auth: ao-rs reads `GITLAB_TOKEN` (or `GITLAB_PRIVATE_TOKEN` / `PRIVATE_TOKEN`)
  from env. ao-ts delegates to `glab`'s config; the behaviors converge at the
  HTTP layer (both send `PRIVATE-TOKEN: <pat>`).
- Transport: REST `api/v4` via `reqwest`. Kept because (a) no native Rust
  analogue of `glab`; (b) wiremock fixtures give us hermetic unit tests.
- Bot list: mirrors ao-ts' GitLab-specific bot authors (`gitlab-bot`, `ghost`,
  `project_<N>_bot`, `*[bot]`).

## Test plan

Unit tests cover, via fixture JSON + wiremock:
- CI job mapping (all GitLab statuses including `canceled`, `manual`,
  `waiting_for_resource`, `preparing`, `scheduled`, `created`).
- Merge readiness composer (draft, CI, approvals, conflicts,
  `cannot_be_merged`, `checking`, unresolved discussions).
- Reviews (approvals + unresolved discussions, bot filtering, approved/
  changes-requested dedup).
- Pending/automated comments (resolvable+unresolved + bot split).
- Webhook verify (secret-env var, bad token, missing header, non-POST).
- Webhook parse (merge_request, push, tag_push branch=None, pipeline tag).
- Merge method mapping (squash/rebase/merge).
