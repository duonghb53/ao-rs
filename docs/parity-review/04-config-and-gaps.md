# Config + types parity + gap analysis

## Verdict

**Significant drift** — core config schema is nearly complete but diverges from TS on required fields, validation semantics, and top-level filename. Types layer has one new Rust-only state (`MergeFailed`). Several important TS core modules are only in Rust as test-only `parity_*` stubs, not wired into the runtime. CLI commands and a few plugins are missing.

## Part A — Config schema

### Parity-confirmed

- `ProjectConfig` field coverage: `name`, `repo`, `path`, `default_branch`, `session_prefix`, `runtime`, `agent`, `workspace`, `tracker`, `scm`, `symlinks`, `post_create`, `agent_config`, `orchestrator`, `worker`, `reactions`, `agent_rules`, `agent_rules_file`, `orchestrator_rules`, `orchestrator_session_strategy`, `opencode_issue_session_strategy` all present.
- `ReactionConfig`: `auto`, `action` (`send-to-agent`/`notify`/`auto-merge`), `message`, `priority`, `retries`, `escalate_after` (untagged number-or-string), `threshold`, `include_summary`. `EventPriority` matches (`urgent`/`action`/`warning`/`info`).
- `ScmWebhookConfig` fields match (`enabled`, `path`, `secretEnvVar`, `signatureHeader`, `eventHeader`, `deliveryHeader`, `maxBodyBytes`) with camelCase rename + snake_case alias.
- `DefaultsConfig`: `runtime`, `agent`, `workspace`, `notifiers`, `orchestrator`, `worker` present.
- Serde rename/alias coverage: most fields accept both camelCase (TS style) *and* snake_case/kebab-case, giving drop-in YAML compatibility in both directions.
- Default reactions: 9 keys match the TS set (`ci-failed`, `changes-requested`, `merge-conflicts`, `approved-and-green`, `agent-idle`, `agent-stuck`, `agent-needs-input`, `agent-exited`, `all-complete`).

### Drift

- **Config filename diverges.** Rust uses `ao-rs.yaml`; TS uses `agent-orchestrator.yaml`. Intentional branding choice but means configs are not drop-in copy-paste between the two.
- **`ProjectConfig.repo` required vs optional.** Rust: `pub repo: String` — required. TS: `repo?: string` — optional (validated in generator; omitted when no remote detected). `ao-rs` cannot represent a "local-only" project without a remote.
- **`ProjectConfig.path` validation strictness.** Rust rejects `~`-prefixed paths with an explicit error; TS accepts them and calls `expandHome()` during `expandPaths()`. TS also accepts relative paths silently. This is a deliberate safety choice but could break a user moving a TS config into ao-rs.
- **`ProjectConfig.session_prefix` optionality.** Rust: `Option<String>` (never auto-derived at load). TS: derives one via `generateSessionPrefix(basename(project.path))` if missing, then checks prefix collisions. Rust skips both steps → no prefix-collision detection.
- **`ProjectConfig.name` auto-derivation missing.** TS sets `project.name = id` when unset during `applyProjectDefaults`; Rust leaves it `None`.
- **`inferScmPlugin` missing.** TS infers `scm: { plugin: "github" | "gitlab" }` from `repo`/`tracker.host` when not set. Rust leaves it `None` — downstream code must hit a later fallback.
- **`default-branch` default.** Matches ("main") but `#[serde(alias = "default-branch", rename = "default_branch")]` means TS-style `defaultBranch:` will *not* parse in Rust (no camelCase alias on this field, only kebab- and snake-case). **Confirmed mini-bug.**
- **`AgentPermissionMode` drift.** TS is an enum (`permissionless|default|auto-edit|suggest`, with legacy `skip` → `permissionless`). Rust stores `permissions: String` with no enum/validation and no `skip` normalization — a typo like `"permisionless"` will not fail YAML parse.
- **Duplicate-project-ID / session-prefix-collision validation absent in Rust.** TS's `validateProjectUniqueness` throws on duplicate basenames or duplicate prefixes; Rust's `AoConfig::validate` only checks repo/path shape and notifier/reaction-key names.
- **`_externalPluginEntries` not modeled.** TS sets this on the config object for plugin-manifest validation; Rust has no equivalent. `plugins` in Rust is `Vec<HashMap<String, serde_yaml::Value>>` — essentially a typed-as-dynamic pass-through.
- **`PowerConfig.preventIdleSleep` default.** TS defaults to `process.platform === "darwin"`. Rust defaults to `false` via `#[derive(Default)]` — macOS users no longer get idle-sleep prevention for free.
- **`AgentConfig::rules` and `rules_file` fields Rust-only.** TS `AgentSpecificConfig` has no `rules`/`rulesFile`; those live on `ProjectConfig.agentRules`/`agentRulesFile` in TS. Rust has both on `ProjectConfig` *and* on `AgentConfig`, which is a duplicate channel. Low-risk but schema drift.
- **`poll_interval` Rust-only.** TS doesn't have a top-level `pollInterval`; lifecycle interval is derived elsewhere. Rust adds one with alias `pollInterval`/`poll-interval`.
- **`ReactionConfig.merge_method` Rust-only.** Rust adds a `merge_method` field on reactions (for `auto-merge`). TS has no equivalent on the reaction config — merge method is passed separately through the SCM plugin.
- **`applyDefaultReactions` semantics differ.** TS merges user-defined reactions *on top of* defaults (user wins), so a TS config without `reactions:` still has all 9 keys populated. Rust's `load_from` returns `reactions` exactly as YAML wrote it (empty by default); defaults only apply in `generate_config`.
- **Pure-YAML `plugins:` missing schema.** TS `InstalledPluginConfigSchema` validates `source: "registry"|"npm"|"local"`, requires `path` for local and `package` for registry/npm. Rust stores an untyped `HashMap` — any shape parses.

### Missing fields

- `OrchestratorConfig.configPath` — set post-load in TS for hash-based path derivation. No equivalent in Rust (Rust uses cwd-discovery at call sites).
- `DefaultPlugins.orchestrator.agent` / `DefaultPlugins.worker.agent` — TS has this via `RoleAgentDefaultsSchema`. Rust has `RoleAgentConfig` which is fuller (includes `agent_config`), but that's not the same shape.
- `generateTempPluginName`, `collectExternalPluginConfigs`, `mergeExternalPlugins` — entire external-plugin-auto-registration subsystem absent. Rust's plugin list is declared manually.
- `findConfigFile` home-dir fallbacks — TS walks CWD → `AO_CONFIG_PATH` env → home (`~/.agent-orchestrator.yaml`, `~/.config/agent-orchestrator/config.yaml`). Rust walks CWD only; no env-var override, no home fallbacks.

## Part B — Types

### Parity-confirmed

- `SessionStatus`: 17 of 17 TS variants present (spawning, working, pr_open, ci_failed, review_pending, changes_requested, approved, mergeable, merged, cleanup, needs_input, stuck, errored, killed, idle, done, terminated). Snake_case serde form matches TS literals.
- `ActivityState`: all 6 (active, ready, idle, waiting_input, blocked, exited).
- `TerminationReason` (Rust-only helper): wire form snake_case; no TS equivalent but doesn't conflict.
- `PRState` enum values (`open`/`merged`/`closed`) and `ReactionAction` (`send-to-agent`/`notify`/`auto-merge`) match.
- Terminal/restorable set definitions match TS: `{killed, terminated, done, cleanup, errored, merged}` terminal, only `merged` non-restorable.

### Drift

- **`SessionStatus::MergeFailed` Rust-only.** Added for Phase G auto-merge retry loop. Not a bug — a deliberate Rust extension with snake_case wire form (`merge_failed`) and its own transition logic. TS represents the same scenario via metadata flags.
- **`OrchestratorEvent` wire form differs substantially.** TS defines a flat `EventType` union (29 dotted values like `session.spawned`, `pr.created`, `ci.passing`) plus an `OrchestratorEvent` with `type`/`priority`/`sessionId`/`data`. Rust uses a tagged Rust enum with *9 variants* (`spawned`, `session_restored`, `status_changed`, `activity_changed`, `terminated`, `tick_error`, `reaction_triggered`, `reaction_escalated`, `ui_notification`). Wire names don't overlap; subscribers to the TS SSE format cannot consume Rust events directly.
- **`Session` shape drift.** Rust stores `task: String` (the prompt), `branch: String`, `spawned_by`, `last_merge_conflict_dispatched`, `initial_prompt_override`, `claimed_pr_number`/`claimed_pr_url`, `agent`/`runtime` strings, `created_at: u64` ms-epoch. TS has `PRInfo` nested, `createdAt: Date`, `lastActivityAt`, `restoredAt`, `metadata: Record<string,string>` with conventions (`role`, `tmuxName`, `lastMergeConflictDispatched` sentinel). Both are valid; no YAML round-trip parity.
- **`CostEstimate` shape.** TS: `inputTokens`/`outputTokens`/`estimatedCostUsd`. Rust adds `cache_read_tokens`/`cache_creation_tokens` (parity for Claude's usage block) and renames `estimated_cost_usd` → `cost_usd: Option<f64>` (nullable for Codex).
- **`isOrchestratorSession` helper — Rust has no equivalent** of the TS logic that uses `metadata.role == "orchestrator"` or a prefix regex check. Rust identifies orchestrator sessions by separate conventions elsewhere.
- **Issue/CI/Review types.** TS has full `Issue`, `CICheck`, `Review`, `ReviewComment`, `AutomatedComment`, `MergeReadiness`, `PREnrichmentData`, `BatchObserver` interfaces. Rust has equivalent types in `scm.rs`/`traits.rs` but structural parity was not audited in this slice.
- **`SessionSpawnConfig.subagent` (OpenCode subagent override) parity**: TS has it. Rust — not audited here.

## Part C — Feature gap analysis

### Probably-intentional exclusions

- `agent-opencode` plugin — listed in known intentional exclusions.
- `terminal-iterm2`, `terminal-web` — listed in known intentional exclusions.
- `tracker-gitlab`, `notifier-composio`, `notifier-openclaw`, `notifier-webhook`, `web` package — listed.
- `atomic-write.ts` (11 lines), `key-value.ts` (18 lines) — folded into `parity_metadata.rs` as helpers.
- `scm-webhook-utils.ts` (35 lines) — small; parity/test utility. SCM plugins in Rust carry their own webhook parsing.
- `dashboard-rebuild.ts` (CLI lib) — ao-rs has `ao-dashboard` crate.

### Possible gaps (need triage)

| TS file | Purpose | Impact if missing | Recommended action |
|---|---|---|---|
| `packages/core/src/config-generator.ts` (317 LoC) | URL-driven `ao start <url>` with SCM platform detection (github/gitlab/bitbucket) | `ao-rs start <url>` cannot auto-detect remote SCM → users must hand-edit yaml | Port `isRepoUrl`, `parseRepoUrl`, `detectScmPlatform`; wire into `ao-rs start` |
| `packages/core/src/agent-selection.ts` (93 LoC) | Role-based agent resolution (orchestrator vs worker) | Role overrides may not be honored consistently; Rust logic is scattered | Audit `session_manager.rs` + `orchestrator_spawn.rs` for equivalent resolution |
| `packages/core/src/agent-workspace-hooks.ts` (339 LoC) | Per-agent workspace hook setup (Claude settings.json, Codex config, etc.) | Metadata auto-update via PostToolUse hooks may be absent → PRs created by agents don't surface in dashboard | High-priority: verify `workspace_hooks.rs` (249 LoC) covers all TS cases |
| `packages/core/src/opencode-agents-md.ts` (43 LoC) | Writes `AGENTS.md` for opencode subagents | Only relevant if `agent-opencode` is ported (intentional exclusion) | Defer with agent-opencode |
| `packages/core/src/tmux.ts` (200 LoC) | Shared tmux helpers | Rust has tmux logic in `runtime-tmux` plugin — functional equivalent exists | Confirm plugin coverage during Slice 7 |
| `packages/core/src/recovery/*` (7 files) | Structured recovery system with validator + actions | Rust has `restore.rs` (556 LoC) + `cleanup.rs` — partial coverage per ts-core-parity-report | Follow up: gap-analyze recovery vs restore |
| `packages/core/src/utils/pr.ts`, `session-from-metadata.ts`, `validation.ts` | Small util helpers | Individual audit — most likely absorbed into Rust call sites | Spot-check when a dependent feature ports |
| `packages/cli/src/commands/init.ts` | `ao init` — interactive wizard | `ao-rs` has no `init` command (only `setup/`) | Triage: is `setup` the replacement? |
| `packages/cli/src/commands/session.ts` | `ao session <subcmd>` umbrella | `ao-rs` exposes `kill`, `cleanup`, `stop`, `watch` as top-level commands | Likely equivalent via top-level commands |
| TS CLI libs: `caller-context`, `credential-resolver`, `detect-agent`, `detect-env`, `plugin-marketplace`, `plugin-scaffold`, `plugin-store`, `preflight`, `prevent-sleep`, `project-detection`, `project-resolution`, `update-check` | Supporting infra | Some duplicated in ao-cli, others missing. `plugin-marketplace`/`plugin-store`/`plugin-scaffold` absent | Audit per-command; `prevent-sleep` most likely to be a real gap on macOS |
| `packages/plugins/notifier-ntfy` (exists in ao-rs only) | Rust-added notifier | N/A | Note: Rust has notifier-ntfy + notifier-stdout that TS lacks |

### Confirmed bugs from missing logic

- **`defaultBranch:` in TS configs silently ignored by ao-rs.** Rust declares the field as `rename = "default_branch"` with only `alias = "default-branch"` — no camelCase alias. A TS config migrated verbatim will parse but the `defaultBranch` field falls into `default_runtime_name` (via `default_branch_name` serde default = `"main"`) silently. Fix: add `alias = "defaultBranch"`.
- **Session-prefix collision and project-ID collision detection absent.** Two projects with the same path basename will pass validation in ao-rs but throw in ao-ts. In multi-project configs this can cause session ID collisions at runtime.
- **`AgentConfig::permissions` accepts invalid strings.** The Rust type is `String` with no enum validation; YAML `permissions: permisionless` (typo) parses successfully. TS uses a Zod `z.enum` that rejects typos at load.
- **`PowerConfig.preventIdleSleep` is `false` on macOS by default in Rust.** TS defaults to `true` on darwin. Rust macOS users do not get idle-sleep prevention unless they opt in explicitly.
- **External inline plugin configs are not auto-registered in Rust.** TS config like `tracker: { package: "@acme/foo" }` automatically adds an entry to `plugins[]` via `collectExternalPluginConfigs`. Rust's `PluginConfig` accepts the fields but nothing downstream auto-loads them.

## Notes

- Rust has extra CLI commands (`verify`, `doctor`, `pr`, `orchestrator`, `stop`, `watch`, `kill`, `cleanup`, `update`, `config_help`) unique to ao-rs.
- `parity_*.rs` modules are **test-only** shims per `ts-core-parity-report.md` policy. No runtime consumers — downstream TS features depending on observability snapshots or feedback-tools JSON won't produce/consume equivalent ao-rs data.
- **`paths.rs` (108 LoC) is a major reduction from `paths.ts` (211 LoC)**. Missing: `generateConfigHash`, `generateInstanceId`, `generateSessionPrefix`, `getProjectBaseDir`, `getObservabilityBaseDir`, `getSessionsDir`, `getWorktreesDir`, `getFeedbackReportsDir`, `getArchiveDir`, `getOriginFilePath`, `generateTmuxName`, `parseTmuxName`, `validateAndStoreOrigin`. Instead of hash-based per-config dirs, Rust uses single global `~/.ao-rs/sessions` — two ao-rs configs side-by-side will collide.
- `OrchestratorEvent` wire-form divergence is the largest single-surface drift; TS SSE consumers cannot read Rust events unmodified.

Files audited: `crates/ao-core/src/{config,types,events,paths,activity_log,reactions,notifier_resolution,parity_*}.rs`, `docs/ts-core-parity-report.md`.
