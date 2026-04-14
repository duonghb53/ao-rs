# Issue #14 Phase 1 — State machine parity audit (TS → Rust)

Reference TS implementation (upstream):
- `packages/core/src/types.ts`
- `packages/core/src/lifecycle-manager.ts` (`determineStatus`, PR/CI/review ladder)

Rust implementation (this repo):
- `crates/ao-core/src/types.rs` (`SessionStatus`, `ActivityState`)
- `crates/ao-core/src/scm_transitions.rs` (`derive_scm_status`)
- `crates/ao-core/src/lifecycle.rs` (tick order + “one transition per tick” gate)
- `crates/plugins/scm-github/src/lib.rs` (`compose_merge_readiness`)

## Status mapping (TS ↔ Rust)

All TS `SessionStatus` variants map 1:1 to Rust **except**:

- **TS**: (no equivalent) → **Rust**: `merge_failed`
  - **Why**: Rust filters self-loop transitions (`X → X`) to avoid event spam. TS allows `mergeable → mergeable` and uses that to retry auto-merge on later ticks. Rust adds `merge_failed` as a parking state so the merge retry loop remains possible without reintroducing self-loops.

## PR/CI/Review transition semantics (TS vs Rust)

### Open PR priority ladder

TS (`determineStatus`, PR branch) order:
1. merged → `merged`
2. closed → `killed`
3. CI failing → `ci_failed`
4. review changes requested → `changes_requested`
5. review approved OR none:
   - mergeability true → `mergeable`
   - else (approved only) → `approved`
6. review pending → `review_pending`
7. else → `pr_open`

Rust (`derive_scm_status`) now matches this ordering with two deliberate differences:
- It computes mergeability via `MergeReadiness::is_ready()` (a composite of CI + approvals + conflicts + blockers) rather than TS’s separate `reviewDecision` + `mergeable` booleans.
- It retains the `merge_failed` parking loop (see above).

### Fixes applied (parity gaps)

- **Made `review_pending` reachable** in Rust PR ladder when `ReviewDecision::Pending`.
- **Matched TS priority** for “CI failing vs changes requested”: `ci_failed` wins.
- **Matched TS terminal mapping** for closed PRs: `PrState::Closed → killed`.
- **Matched TS “review none counts as approved for merge readiness”** by treating missing `reviewDecision` as `approved=true` in GitHub merge-readiness composition so CI-green/no-review-required PRs can reach `mergeable`.

## Tick order / one-transition-per-tick

Rust `lifecycle.rs` enforces: **at most one status transition per tick**. This mirrors TS’s “one `determineStatus()` result per poll cycle” semantics.

Added regression test: a session beyond stuck threshold + an SCM transition on the same tick must yield only the SCM transition (no immediate `… → stuck`).

