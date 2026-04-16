# Remaining: Features Not Yet Ported from ao-ts

Status: **Complete inventory** of what ao-ts has that ao-rs does not.

---

## Priority 1 — Functional gaps affecting runtime behavior

### 1.1 Project-level reaction resolution

- **ao-ts**: `getReactionConfigForSession()` merges `config.reactions[key]` with `project.reactions[key]` — project fields override global.
- **ao-rs**: `ProjectConfig.reactions` field exists and parses from YAML, but `ReactionEngine::dispatch` only reads global `config.reactions`. Project overrides are silently ignored.
- **Files to modify**: `crates/ao-core/src/reaction_engine.rs` (add `resolve_reaction_config` method), `crates/ao-core/src/lifecycle.rs` (thread `AoConfig` into engine)
- **Effort**: Small — types are already in place.

### 1.2 Review thread resolution (`is_resolved`)

- **ao-ts**: Uses GraphQL `reviewThreads` to detect resolved vs unresolved review comments.
- **ao-rs**: REST `pending_comments` always sets `is_resolved: false`. Cannot distinguish resolved threads. Risk: `changes-requested` reactions may spam agents with already-addressed comments.
- **Files to modify**: `crates/plugins/scm-github/src/lib.rs` (switch `pending_comments` to GraphQL), `crates/plugins/scm-github/src/parse.rs` (parse `reviewThreads` response)
- **Effort**: Medium — requires GraphQL query design and response parsing.

### 1.4 Session restore prompt redelivery

- **ao-ts**: `restore()` re-delivers the initial prompt after restarting the runtime.
- **ao-rs**: `restore.rs` restarts runtime but does not re-deliver the prompt. Left to manual `ao-rs send`.
- **Files to modify**: `crates/ao-core/src/restore.rs`
- **Effort**: Small — prompt builder already exists; wire it into restore flow.

---

## Priority 2 — Missing CLI commands

### 2.1 `stop`

- **ao-ts**: `ao stop [project]` with `--purge-session`, `--all`
- **ao-rs**: No equivalent. Closest: `ao-rs kill <session>` and `ao-rs cleanup` (session-scoped, not supervisor-scoped).
- **Effort**: Medium — needs lifecycle/pidfile integration.

### 2.2 `open`

- **ao-ts**: `ao open [target] [-w, --new-window]` — opens dashboard/session in browser/terminal.
- **ao-rs**: Not implemented.
- **Effort**: Small.

### 2.3 `verify`

- **ao-ts**: `ao verify [issue] [-p project] [--fail] [-c comment] [-l list]` — verify issue completion.
- **ao-rs**: Not implemented.
- **Effort**: Medium.

### 2.4 `update`

- **ao-ts**: `ao update [--skip-smoke] [--smoke-only] [--check]` — self-update.
- **ao-rs**: Not implemented.
- **Effort**: Medium — binary distribution story differs (cargo install vs npm).

### 2.5 `setup` umbrella

- **ao-ts**: `ao setup openclaw [--url] [--token] [--routing-preset] [--non-interactive]`
- **ao-rs**: Not implemented.
- **Effort**: Small per subcommand.

### 2.6 `plugin` umbrella

- **ao-ts**: `ao plugin <subcommand>` — marketplace install/list/update.
- **ao-rs**: Intentionally different architecture (workspace crates). No CLI surface.
- **Effort**: Large — design decision needed on whether to implement marketplace or stay crate-based.

### 2.7 `config-help`

- **ao-ts**: `ao config-help` — prints config guide.
- **ao-rs**: Has `docs/config.md` but no CLI command.
- **Effort**: Small.

---

## Priority 3 — Missing CLI flags on existing commands

### 3.1 `start`

- Missing: `--no-dashboard`, `--no-orchestrator`, `--rebuild`, `--dev`, `--interactive`

### 3.2 `status`

- Missing: `--json` output, `--watch`, `--interval`

### 3.3 `spawn`

- Missing: `--open`, `--claim-pr`, `--assign-on-github`, `--prompt <text>`

### 3.4 `send`

- Missing: `--file <path>`, `--no-wait`, `--timeout <seconds>`, variadic message args

### 3.5 `session kill`

- Missing: `--purge-session`

### 3.6 `session claim-pr`

- Missing entirely: `ao session claim-pr <pr> [session] [--assign-on-github]`

### 3.7 `session remap`

- Missing entirely: `ao session remap <session> [-f, --force]`

### 3.8 `doctor`

- Missing: `--fix`, `--test-notify`

### 3.9 `dashboard`

- Missing: `--rebuild`
- Divergence: TS default opens browser; Rust requires `--open`

---

## Priority 4 — Trait surface gaps (documented in `traits.rs`)

### 4.1 `Scm` trait — methods not ported from TS

- Webhook verification
- Automated bot-comment fetch
- PR check-out helper
- Per-session `ProjectConfig` plumbing on every method

### 4.2 `Tracker` trait — methods not ported

- `list_issues`
- `update_issue`
- `create_issue`

### 4.3 `Agent` trait — stub defaults

- `detect_activity` returns `Ready` by default (stub)
- `cost_estimate` returns `None` by default (no universal pricing API)

---

## Priority 5 — Plugin-level gaps

### 5.1 agent-cursor

- TS embeds full prompt in launch args; Rust uses post-launch `send_message`
- No `--append-system-prompt` support
- `cost_estimate` always returns `None`

### 5.2 agent-aider

- Bare `aider` launch with no `--yes` default
- No `cost_estimate` override

### 5.3 agent-codex

- `cost_usd` fixed to `0.0` (no stable pricing API)
- Interactive `codex --full-auto` vs TS `codex exec`

### 5.4 tracker-github

- No `generatePrompt` (moved to `Tracker` trait default)
- No `listIssues`, `updateIssue`, `createIssue`
- No older-`gh` `stateReason` retry dance
- `is_completed` re-fetches full issue (TODO for perf)

### 5.5 workspace-worktree / workspace-clone

- `list`/`restore` parity is not implemented (workspace plugins remain `create`/`destroy` only)

### 5.6 scm-gitlab

- Not framed as ao-ts port — independent REST implementation
- No parity claim

### 5.7 notifier-ntfy

- No auth for private servers (future)
- No HTTP unit tests

---

## Priority 6 — Parity-only modules (not wired into runtime)

These modules exist in `crates/ao-core/src/` for cross-port testing but are not used by the main ao-rs runtime:

| Module | TS Mapping | Status |
|--------|-----------|--------|
| `parity_utils.rs` | `packages/core/src/utils.ts` | Ported for parity tests only |
| `parity_session_strategy.rs` | TS orchestrator-session-strategy | Enums are production; `decide_existing_session_action` is test-only |
| `parity_config_validation.rs` | TS project/orchestrator config validation | Test-only |
| `parity_plugin_registry.rs` | TS plugin manifest/registry | Test-only |
| `parity_observability.rs` | TS observability snapshot/metrics | Test-only |
| `parity_metadata.rs` | TS session metadata on-disk format | Test-only |
| `parity_feedback_tools.rs` | TS feedback tools (bug_report, improvement) | Test-only |

Decision needed: integrate into runtime or keep as test infrastructure only.

---

## Priority 7 — Minor / cosmetic gaps

### 7.1 Paths

- `paths.rs` only implements a subset of TS `paths.ts` (Slice 1 scope)

### 7.2 Activity log

- Timestamp parsing only handles numeric ms strings; ISO timestamps won't parse for staleness checks
- "Inspired by" ao-ts, not a strict port

### 7.3 Events

- Event set is minimal (Phase C) — no TS file referenced; likely smaller than mature TS event bus

### 7.4 Default merge method

- ao-rs defaults to `Merge`; ao-ts defaults to `squash`

### 7.5 GitHub Enterprise

- `parse_github_remote` only handles github.com-shaped URLs (strict `owner/repo`)

---

## Quick reference: effort estimates

| Item | Effort | Impact |
|------|--------|--------|
| Project-level reaction resolution | Small | High — config feature silently broken |
| Review thread resolution | Medium | High — reaction accuracy |
| Restore prompt redelivery | Small | Medium — UX gap |
| `stop` command | Medium | Medium — supervisor management |
| `--json` on status | Small | Medium — scripting/CI integration |
| `claim-pr` / `remap` commands | Medium | Low — advanced workflows |
| Trait surface expansion | Large | Low — most trimmed methods not needed yet |
| Parity module integration | Large | Low — test infrastructure |
