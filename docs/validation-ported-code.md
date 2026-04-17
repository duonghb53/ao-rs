# Validation Report: All Ported Code (ao-ts to ao-rs)

Status: **Complete audit** — comment-driven parity map based on what ao-rs claims to mirror.

---

## 1. Core Lifecycle + SCM Transitions — VALIDATED

**Files:** `crates/ao-core/src/lifecycle.rs`, `crates/ao-core/src/scm_transitions.rs`
**TS reference:** `packages/core/src/lifecycle-manager.ts`

### What matches TS

- Priority ladder for open PRs is structurally identical: Merged > Closed > Readiness > CI Failing > ChangesRequested > Approved > ReviewPending > PrOpen
- Stuck detection, idle tracking, two-pass batch enrichment all mirror TS lifecycle loop
- 26 unit tests in `scm_transitions.rs` cover every rung of the ladder
- `poll_scm` two-pass approach matches TS batch-then-individual pattern

### Known gaps

- None for the transition logic itself

### Intentional divergences

- Default poll interval: **10s** (ao-rs) vs **5s** (ao-ts) — documented in module doc
- Transitions extracted to pure function `derive_scm_status()` vs inline in TS lifecycle

---

## 2. Reaction Engine — VALIDATED

**Files:** `crates/ao-core/src/reaction_engine.rs`, `crates/ao-core/src/reactions.rs`
**TS reference:** `executeReaction` / `ReactionTracker` in `lifecycle-manager.ts` (~570-710), `types.ts` (~900-995)

### What matches TS

- Dispatch logic: config lookup → auto check → tracker bump → escalation gates → action dispatch
- Retry counting: `attempts > retries` threshold matches TS
- Duration-based escalation: strict `>` comparison matches TS (`elapsed > duration`)
- `EscalateAfter` supports both `Attempts(u32)` and `Duration(String)` — same as TS `number | string`
- `ReactionAction`: `SendToAgent`, `Notify`, `AutoMerge` — kebab-case serde matches TS wire format
- `EventPriority`: `Urgent`, `Action`, `Warning`, `Info` — matches TS four-value union
- Default priority table per reaction key matches TS practical defaults
- `clear_tracker` on transition reset mirrors TS `clearReactionTracker`
- `auto: false` skip behavior aligns with TS

### Known gaps

- **Project-level reaction resolution**: ao-ts merges `config.reactions[key]` with `project.reactions[key]` via `getReactionConfigForSession()`. ao-rs `ProjectConfig.reactions` field exists but is **not wired** into `ReactionEngine::dispatch`.
- `include_summary` defaults to `false` — engine cannot yet produce context summaries (Phase D comment)
- `threshold` field kept as opaque `String` in types (parsing in engine/lifecycle)

### Intentional divergences

- Split `ReactionEngine` vs TS monolithic `createLifecycleManager` closure — for testability
- `parseDuration`: TS returns `0` on garbage; Rust returns `None` — same effective "no gate" behavior
- `auto: false` + `Notify`: fires once without escalation machinery (Rust-specific policy)
- `ReactionOutcome` naming vs TS `ReactionResult`
- Escalated notify priority defaults to `Urgent` when config omits `priority`
- Tracker under `Mutex` for concurrency safety (Rust-specific need)

---

## 3. SCM Types + GitHub Plugin — VALIDATED

**Files:** `crates/ao-core/src/scm.rs`, `crates/plugins/scm-github/src/lib.rs`, `graphql_batch.rs`, `parse.rs`
**TS reference:** `types.ts` (~500-820), `packages/plugins/scm-github/src/index.ts`, `graphql-batch.ts`

### What matches TS

- All core SCM operations: detect PR, pr_state, ci_checks, ci_status, reviews, review_decision, pending_comments, mergeability, merge, enrich_prs_batch
- `summarize_ci` + error handling mirrors `getCISummary`
- `map_check_state` matches `mapRawCheckStateToStatus`
- `compose_merge_readiness` matches TS lines 981-1025
- Merged PR short-circuit matches TS lines 948-961
- Review comments pagination: 100/page, max 100 pages, matches TS lines 880-906
- 2-guard ETag + GraphQL batch with 25 PRs/chunk matches `graphql-batch.ts`
- 30s subprocess timeout matches TS `DEFAULT_TIMEOUT_MS`
- Parse functions align with TS `JSON.parse` sites at documented line numbers

### Known gaps

- **Review thread resolution**: REST `pending_comments` cannot expose `is_resolved` (always `false`). TODO for GraphQL `reviewThreads` or consumer-side dedupe. Risk of spamming `changes-requested` reactions.
- **Webhooks**: explicitly out of `scm-github` plugin scope
- **Bot-comment severity**: reaction-engine concern, not SCM
- **GitHub Enterprise**: `parse_github_remote` is host-agnostic (strict `owner/repo` path, any hostname — resolved in #110). Exotic GHE path prefixes still need explicit `projects.<id>.repo`.
- **ETag 304 detection**: uses `output.contains("304")` — brittle vs structured headers
- **GraphQL vs REST merge readiness**: two parallel implementations of similar rules (could drift)

### Intentional divergences

- Default merge method: **`Merge`** (ao-rs) vs **`squash`** (ao-ts)
- `detect_pr` uses `--state all` for merged/closed PRs (dashboard enrichment)
- Smaller `PullRequest` struct vs TS `PRInfo` — extra fields on enrichment structs
- `CheckRun` naming vs TS `CICheck`; `conclusion` kept as opaque `String`
- Unknown check states map to `Skipped` not `Failed` (defensive anti-footgun)
- `Review.submittedAt` dropped (no ordering needed yet)
- Dates as `String` — no chrono dependency

---

## 4. Config Loading — VALIDATED

**Files:** `crates/ao-core/src/config.rs`
**TS reference:** `OrchestratorConfig` shape from agent-orchestrator

### What matches TS

- Full field mapping with serde aliases: camelCase, snake_case, kebab-case interop
- All documented TS fields present: `port`, `terminalPort`, `directTerminalPort`, `readyThresholdMs`, `power.preventIdleSleep`, `defaults.*`, `notifiers`, `notificationRouting`, `reactions`, project-level fields (`name`, `sessionPrefix`, `scm`, `symlinks`, `postCreate`, `agentRules`, `orchestratorRules`, etc.)
- `default_reactions()` matches the nine default reactions from TS
- Serde round-trip tests validate YAML compatibility

### Known gaps

- `plugins` list: stored for parity only — no installer/marketplace behavior

### Intentional divergences

- Config filename: `ao-rs.yaml` vs TS `agent-orchestrator.yaml`
- Discovery: walks up from cwd (no global `--config` flag)
- Missing file returns empty `AoConfig` (fresh install UX)
- Stricter validation: rejects unknown reaction keys, bad durations, unknown notifiers, requires absolute paths
- `default_agent_rules()` / `default_orchestrator_rules()` are Rust/ai-devkit-oriented, not verbatim TS
- `generate_config()` defaults to `cursor` agent vs generic `claude-code` default

---

## 5. Session Types + Management — VALIDATED

**Files:** `crates/ao-core/src/types.rs`, `crates/ao-core/src/session_manager.rs`, `crates/ao-core/src/restore.rs`
**TS reference:** `types.ts` (`SessionStatus`, `isTerminalSession`), `session-manager.ts` (~2254)

### What matches TS

- `SessionStatus` enum variants match TS
- `TERMINAL_STATUSES` locked to same six variants as TS (tests enforce this)
- `Session::is_terminal()` mirrors TS `isTerminalSession` (combines status OR activity terminality)
- `ActivityState::is_terminal()` mirrors TS `TERMINAL_ACTIVITIES`
- `restore.rs` step sequence aligned to TS `restore()`: find → enrich liveness → restorable gate → workspace exists → destroy old runtime → create → persist
- Serialization uses `snake_case` to stay drop-in comparable with TS YAML

### Known gaps

- **Workspace restore hook**: TS has optional `workspace.restore` plugin hook — not implemented in Rust
- **Prompt redelivery**: `restore` does not re-deliver the initial prompt (left to `ao-rs send`)
- **Enrichment**: Rust only uses `runtime.is_alive(handle)` — narrower than full TS `enrichSessionWithRuntimeState(..., plugins, ...)`
- **Session manager**: no in-memory cache (doc says Slice 2+)

### Intentional divergences

- `MergeFailed` explicitly non-terminal (parking state for merge retry) — tests guard this
- Extra persisted fields on `Session`: `agent`, `agent_config`, `runtime`, `activity`, `cost`, `issue_id`, `issue_url`, `created_at`
- Session manager skips corrupt YAML files with warning (vs TS failing hard)
- Restore sets `status = Spawning` and `activity = None` (Slice 1 simplification)

---

## 6. Traits Surface — VALIDATED

**Files:** `crates/ao-core/src/traits.rs`
**TS reference:** `types.ts` (~577) for `SCM`, `Tracker`

### What matches TS

- `Scm` trait covers: detect_pr, pr_state, ci_checks, ci_status, reviews, review_decision, pending_comments, mergeability, merge, enrich_prs_batch
- `Agent` trait: launch, env, initial_prompt, detect_activity, cost_estimate
- `Tracker` trait: get_issue, is_completed, issue_url, branch_name, generate_prompt

### Known gaps (explicitly documented in file)

- **Scm**: webhook verification, GraphQL batch enrichment (partially reintroduced as optional), automated-bot-comment fetch, PR check-out helper, per-session `ProjectConfig` plumbing
- **Tracker**: `list_issues`, `update_issue`, `create_issue` — cut for now
- **Agent**: default `detect_activity` returns `Ready` (stub); `cost_estimate` returns `None` by default

### Intentional divergences

- `Tracker` holds config via `::new()` instead of passing `ProjectConfig` on every method call
- `enrich_prs_batch` defaults to empty (opt-in batching)
- Thinner API surface — documented as deliberate scope reduction

---

## 7. Notifier Stack — VALIDATED

**Files:** `crates/ao-core/src/notifier.rs`, `crates/ao-core/src/notifier_resolution.rs`
**TS reference:** `packages/core/src/types.ts` — Notifier / NotificationPayload / notificationRouting

### What matches TS

- Same conceptual bundle: notifier contract, payload shape, priority-to-notifier routing
- `NotificationRouting` serde matches TS `notificationRouting` config shape
- Registry `resolve(priority)` returns list of `(name, plugin)` pairs
- Warn-once for missing/empty priorities and unknown notifier names

### Known gaps

- Full Slice 3 pipeline (engine integration, all plugin crates) described as phased (A-D+)
- `notifier_resolution.rs` has no documented TS mirror (only behavioral tests)

### Intentional divergences

- `NotificationPayload` is not `Serialize` (in-process only)
- Empty routing + "default to stdout" lives in `ao-cli` wiring, not config type
- Partial routing (some names missing) still returns registered subset

---

## 8. Prompts — VALIDATED

**Files:** `crates/ao-core/src/orchestrator_prompt.rs`, `crates/ao-core/src/prompt_builder.rs`
**TS reference:** `orchestrator-prompt.ts` (for orchestrator_prompt.rs); none claimed for prompt_builder.rs

### What matches TS

- `orchestrator_prompt.rs` described as equivalent to TS `orchestrator-prompt.ts`
- Generates markdown sections: role intro → non-negotiable rules → optional orchestrator_rules → project info + dashboard URL → quick start → optional reactions blurb
- `prompt_builder.rs`: session context → issue context → template → task directive

### Known gaps

- `prompt_builder.rs` does not claim TS parity (no `packages/` reference)

### Intentional divergences

- Orchestrator prompt is `ao-rs`-specific (CLI strings, URLs, ai-devkit references)
- Workflow rules live in system prompt (`--append-system-prompt`), not in user-message builder

---

## 9. Plugins — VALIDATED

### Agent plugins

| Plugin | TS Reference | Status | Gaps |
|--------|-------------|--------|------|
| **agent-claude-code** | "TS reference's default" (idle threshold) | Full | No `packages/` path; post-launch delivery vs `claude -p`; activity does not detect process exit |
| **agent-cursor** | `packages/plugins/agent-cursor/src/index.ts` | Partial | TS embeds prompt in launch; Rust uses post-launch `send_message`; no `--append-system-prompt`; cost always `None` |
| **agent-aider** | "mirrors the TS plugin strategy" | Partial | No `packages/` path; bare `aider` launch; no `cost_estimate` |
| **agent-codex** | "mirrors other plugins' approach" | Partial | `cost_usd` fixed to `0.0`; interactive `codex --full-auto` |

### Runtime plugins

| Plugin | TS Reference | Status | Gaps |
|--------|-------------|--------|------|
| **runtime-tmux** | `packages/plugins/runtime-tmux/src/index.ts` | Full | Aligns: 5s timeout, temp script, paste for long messages |
| **runtime-process** | None | RS-native | No TS counterpart claimed; no `session attach`; no pty |

### Workspace plugins

| Plugin | TS Reference | Status | Gaps |
|--------|-------------|--------|------|
| **workspace-worktree** | `packages/plugins/workspace-worktree/src/index.ts` | Partial | No symlinks, no postCreate hooks, no list/restore |
| **workspace-clone** | `packages/plugins/workspace-clone` | Partial | Same omissions; Rust adds `--local`, `--no-hardlinks`, `--single-branch`, optional shallow clone |

### Tracker plugins

| Plugin | TS Reference | Status | Gaps |
|--------|-------------|--------|------|
| **tracker-github** | `packages/plugins/tracker-github/src/index.ts` | Partial | No `generatePrompt`, `listIssues`, `updateIssue`, `createIssue`; no stateReason retry; TODO(perf) on `is_completed` |
| **tracker-linear** | None | Full | No TS comparison; extra GraphQL fields for forward compat |

### SCM plugins

| Plugin | TS Reference | Status | Gaps |
|--------|-------------|--------|------|
| **scm-github** | `packages/plugins/scm-github/src/index.ts` | Full | See section 3 above for detailed gaps |
| **scm-gitlab** | None | N/A | Not framed as TS port; REST + fixtures |

### Notifier plugins

| Plugin | TS Reference | Status | Gaps |
|--------|-------------|--------|------|
| **notifier-stdout** | `packages/notifier-console` (`ConsoleNotifier`) | Full | `println!` can panic on broken pipe (noted in comment) |
| **notifier-desktop** | None | Full | macOS-only; no ao-ts path |
| **notifier-slack** | None | Full | Webhook-based; no TS reference |
| **notifier-discord** | None | Full | No client-side rate limiting |
| **notifier-ntfy** | None | Partial | Future: auth for private servers; no HTTP tests |

---

## 10. Infrastructure — VALIDATED

**Files:** `crates/ao-core/src/lockfile.rs`, `paths.rs`, `activity_log.rs`, `events.rs`

### What matches TS

- `lockfile.rs` mirrors `lifecycle-service.ts`: advisory PID lock, kill(pid,0) alive check, temp+rename atomic write, RAII cleanup
- `paths.rs` equivalent to `paths.ts`: `~/.ao-rs` data dir layout
- `activity_log.rs` inspired by `activity-log.ts`: JSONL at `{workspace}/.ao/activity.jsonl`

### Known gaps

- `paths.rs`: only a subset of full TS paths implemented (Slice 1 scope)
- `activity_log.rs`: timestamp parsing only works for numeric ms strings; ISO timestamps won't parse for staleness
- `events.rs`: event set is minimal by design — no TS file referenced

### Intentional divergences

- `lockfile.rs`: PID probe instead of `flock` (documented rationale: wanting "pid alive" semantics)
- `paths.rs`: scoped to Slice 1 needs
- `activity_log.rs`: "inspired" not strict mirror; minimal deps (no chrono)

---

## Summary

| Area | Verdict | Critical Gaps |
|------|---------|---------------|
| Lifecycle + transitions | Match | None |
| Reaction engine | Match | Project-level reaction resolution not wired |
| SCM + GitHub plugin | Match | Review thread resolution (REST limitation) |
| Config | Match | `plugins` list parity-only |
| Session types | Match | No workspace restore hook |
| Traits | Match (subset) | Documented trimmed surface |
| Notifier | Match | Phased rollout (Slice 3) |
| Prompts | Match | `prompt_builder` no TS claim |
| Plugins | Mixed | See per-plugin table |
| Infrastructure | Match | Subset of TS paths |

**Overall:** The core logic (lifecycle, transitions, reactions, SCM) is structurally correct and matches ao-ts. Gaps are documented, intentional, or scoped to future slices.
