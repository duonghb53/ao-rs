# ao-rs parity + code-audit report

**Date:** 2026-04-18
**Scope:** 6 parallel audits. ao-rs (~45K LOC Rust, 115 files) vs ao-ts reference (~85K LOC TS, 281 files).
**Detail:** Per-slice reports in this directory (`01-lifecycle.md` … `06-rust-idiom.md`).

---

## TL;DR

| Area | Verdict |
|---|---|
| Core lifecycle + SCM transitions | Ladder clean ✓. **3 dispatch gaps** missing from TS (HIGH). |
| Session manager + orchestrator | Mostly clean ✓. Prompt template + rules fallback drift (MED). |
| GitHub plugins (scm + tracker) | Minor drift only. Rate-limit handling actually *stricter* than TS. |
| Config schema + types | Schema ~complete. **3 real bugs** (silent `defaultBranch`, missing validation, wrong macOS default). |
| Dead code | Clean: 0 unused deps, 1 dead pub fn, 4 duplicated helpers. |
| Rust idioms | Solid. **3 HIGH issues**: 1 panic path, 20-site mutex-expect pattern, O(n) LRU. |

**Total new findings (not in `docs/remaining-to-port.md`):** ~20, of which ~9 are HIGH-severity.

---

## 1. HIGH severity (real bugs, ship-blockers for parity)

### H1. `defaultBranch:` camelCase silently ignored → falls back to `"main"`
- **Location:** `crates/ao-core/src/config.rs` — `ProjectConfig.default_branch` field
- **Symptom:** `projects.foo.defaultBranch: develop` parses to `"main"` silently.
- **Root cause:** Field has `rename = "default_branch"` with only kebab-case alias, no camelCase alias.
- **Impact:** Any TS-migrated config breaks without warning. Orchestrator spawns on wrong base branch.
- **Fix:** Add `alias = "defaultBranch"` to the serde attribute.
- **Effort:** 1 LOC.
- **Source:** `04-config-and-gaps.md`

### H2. No review-backlog dispatch (TS `maybeDispatchReviewBacklog`)
- **Location:** `crates/ao-core/src/lifecycle.rs` has `last_review_backlog_check` field but never calls `pending_comments` / `automated_comments`.
- **TS reference:** `packages/core/src/lifecycle-manager.ts:758-932`.
- **Symptom:** After the initial `changes-requested` transition, agents stop receiving new reviewer feedback even though TS would keep fingerprinting + re-dispatching.
- **Impact:** Major UX regression on long PR review cycles.
- **Effort:** Medium (needs fingerprinting + de-dup logic port).
- **Source:** `01-lifecycle.md`

### H3. No detailed CI-failure dispatch + missing `all-complete` reaction
- **TS:** Formats failed check names/URLs into the dispatched message; fires a single `summary.all_complete` reaction once per drain.
- **Rust:** Only generic "CI failed" message; `"all-complete"` key is listed in defaults but never triggered anywhere.
- **Impact:** Agents can't tell which CI jobs failed. User never gets "batch done" notification.
- **Effort:** Medium.
- **Source:** `01-lifecycle.md`

### H4. Duplicate project / session-prefix validation missing
- **TS:** `validateProjectUniqueness` rejects two projects with the same basename or session prefix.
- **Rust:** Neither check exists. Multi-project configs can produce runtime session-ID collisions.
- **Effort:** Small (validate in `config.rs::load`).
- **Source:** `04-config-and-gaps.md`

### H5. `PowerConfig.preventIdleSleep` default is backwards on macOS
- **TS:** Defaults to `true` on darwin.
- **Rust:** Defaults to `false` via blanket `#[derive(Default)]`.
- **Impact:** macOS users running `ao-rs watch` unattended lose idle-sleep prevention silently.
- **Effort:** Small (platform-aware `Default` impl).
- **Source:** `04-config-and-gaps.md`

### H6. `panic!` on prompt template drift in hot path
- **Location:** `crates/ao-core/src/orchestrator_prompt.rs:282`
- **Issue:** If a new placeholder is added to `prompts/orchestrator.md` without matching `lookup_placeholder`, the render panics. Because this is called on the `ao-rs watch` spawn path, template drift crashes the daemon.
- **Fix:** Return `Result<String, AoError>`; propagate.
- **Effort:** Small (render fn already callers tolerate `Result`).
- **Source:** `06-rust-idiom.md`

### H7. `expect` on `Mutex` poison in 20 call sites
- **Locations:** `lifecycle.rs` (12 sites) + `reaction_engine.rs` (8 sites)
- **Issue:** Invariant "critical sections never panic" is undocumented. Any future `spawn_blocking` inside a lock would silently introduce poison risk, which would abort the poll tick on panic.
- **Fix:** `unwrap_or_else(|p| p.into_inner())` + `tracing::error!`. `rate_limit.rs` already models this pattern correctly — copy it.
- **Effort:** Small (mechanical).
- **Source:** `06-rust-idiom.md`

### H8. O(n) LRU cache + unnecessary clone per hit
- **Location:** `crates/plugins/scm-github/src/graphql_batch.rs:58-66`
- **Issue:** `Vec::remove(pos)` shifts all elements (O(n)), then clones the value purely to return it.
- **Fix:** Replace with `lru` crate or `indexmap::IndexMap`. Or at minimum return `Option<&V>`.
- **Effort:** Small.
- **Source:** `06-rust-idiom.md`

### H9. `paths.rs` hash-based directory layout not ported → cross-config collisions
- **Rust:** 108 LoC (flat `~/.ao-rs/`).
- **TS:** 211 LoC with `generateConfigHash`, `getProjectBaseDir`, `getSessionsDir`, `generateTmuxName`, `validateAndStoreOrigin`.
- **Impact:** Two ao-rs configs on one machine collide in `~/.ao-rs`. Known-divergent per `ts-core-port-map.md` (marked "ported (different layout)"), but the **collision risk is not documented in existing docs**.
- **Decision needed:** Keep flat (document + guard with single-config check) or port hash-based layout.
- **Source:** `04-config-and-gaps.md`

---

## 2. MEDIUM severity (behavior drift, low-to-medium risk)

### M1. `status_to_reaction_key` missing 3 mappings
- `SessionStatus::NeedsInput` → should key to `"agent-needs-input"`
- `SessionStatus::Killed` → should key to `"agent-exited"`
- `SessionStatus::Approved` → TS routes via `action` priority; Rust returns `None`
- All 3 are listed in `default_priority_for_reaction_key` + `ao-rs.yaml`, so the intent is clearly there — just never wired. Similar class of bug as the `merge-conflicts` one just fixed in #193.
- **Source:** `01-lifecycle.md`

### M2. Orchestrator prompt template CLI drift
- **Issue:** `prompts/orchestrator.md` tells agents to use `ao-rs status --project`, `--task`, `ao-<short>` branches. TS uses `ao status`, `--prompt`, `-p`, `session/<id>`.
- **Effect:** Renders instructions that won't work if the user copies the prompt between ports. Low blast radius today (each port uses its own), but if ao-rs docs embed the TS-style commands elsewhere there's cross-doc drift.
- **Fix:** Align prompt strings or parametrize the CLI name.
- **Source:** `02-session.md`

### M3. `orchestrator_rules` global-fallback drift
- **Rust:** `orchestrator_prompt.rs:174-194` falls back to `defaults.orchestrator_rules` when project-level rules are absent.
- **TS:** Reads only `project.orchestratorRules`; no fallback.
- **Effect:** Users with `defaults.orchestrator_rules` set see rules in Rust but not TS (or vice-versa if they port back).
- **Decision:** Rust's behavior is arguably better, but it's a silent divergence. Document or remove fallback.
- **Source:** `02-session.md`

### M4. `AgentConfig.permissions: String` accepts typos silently
- **TS:** `z.enum(["permissionless", "default", "auto-edit", "suggest"])` rejects typos at load.
- **Rust:** Accepts any string. `permisionless` (typo) loads silently and gets ignored downstream.
- **Fix:** Convert to enum + strict deserialize.
- **Source:** `04-config-and-gaps.md`

### M5. `gh` subprocess runner duplicated across plugins
- Identical ~25 lines in `scm-github/src/lib.rs` + `tracker-github/src/lib.rs` (env hardening, timeout, rate-limit hook).
- **Fix:** Extract to `ao-core::rate_limit` (or new `ao-core::gh_subprocess`).
- **Source:** `05-cleanup.md`

### M6. `shell_escape` duplicated 4× across production crates
- `runtime-tmux`, `agent-codex`, `agent-aider`, `ao-cli/spawn_helpers`.
- Each has subtle semantic differences (always-wrap vs. skip-for-safe-strings).
- **Already tracked** in `ts-core-parity-report.md` as "Duplicate shell_escape lives in … consolidation deferred."
- **Status:** Reconfirmed by this audit — worth prioritizing now that all 4 call sites are stable.
- **Source:** `05-cleanup.md`

### M7. `parity_metadata` uses `String` as error type
- Public API returns `Result<_, String>` instead of `Result<_, AoError>`. Breaks composability with the rest of the core error hierarchy.
- Noted as test-only in `ts-core-parity-report.md`, but if it graduates to production it would drag the stringly-typed error with it.
- **Source:** `06-rust-idiom.md`

### M8. `std::fs` blocking I/O from async context (`activity_log.rs`)
- `append_activity_entry` does synchronous file I/O but is called from `Agent::detect_activity` (async fn). Blocks a tokio worker on slow disks.
- **Fix:** `spawn_blocking` wrap or switch to `tokio::fs`.
- **Source:** `06-rust-idiom.md`

---

## 3. LOW severity (cosmetics, edge cases, zero-blast-radius today)

- **L1.** `ScmWebhookEvent` missing `timestamp` field (TS parses from `updated_at`/`submitted_at`/`created_at`). Only bites event-reordering code that doesn't exist yet. *Source: 03*
- **L2.** `approved` semantics in `compose_merge_readiness` picks GraphQL path for both — principled fix of a TS inconsistency, but fingerprints keyed on `approved` bit will diverge for unreviewed PRs. *Source: 03*
- **L3.** Rust-only `DIRTY → "Merge is blocked (...)"` blocker string + `BLOCKED` wording drift (`"Branch protection requirements not satisfied"` vs TS `"Merge is blocked by branch protection"`). Any fingerprint keyed on the string diverges. *Source: 03*
- **L4.** `as_valid_opencode_session_id("ses_")` false-accepts the empty suffix due to vacuously-true iterator. Blast radius 0 (no opencode plugin in Rust). *Source: 02*
- **L5.** `decide_existing_session_action(DeleteNew)` → `Abort` in Rust vs TS delete-after-normalization. Test-only code path. *Source: 02*
- **L6.** `validate_symlink_entry` in `workspace_hooks.rs:66` is `pub` with zero external callers. Narrow to `pub(crate)` or delete. *Source: 05*
- **L7.** `LruCache::clear` in `graphql_batch.rs:79` explicitly `#[allow(dead_code)]` — safe to delete (3 LOC). *Source: 05*
- **L8.** `notifier_resolution.rs` is test-only but lacks the `parity_` prefix required by the meta-test guard. One-file rename. *Source: 05*
- **L9.** `cargo fmt --check` fails on 3 files: `paths.rs:65`, `parity_modules_meta.rs:92`, `notifier-ntfy/src/lib.rs:150`. *Source: 06*
- **L10.** `session.id.clone()` in the per-tick lifecycle loop — 10+ redundant clones per tick. Not a hot path yet, but will become one with N sessions. *Source: 06*
- **L11.** `unsafe { set_var(...) }` in webhook tests without `// SAFETY:` comment (Rust 2024 requires `unsafe`). *Source: 06*
- **L12.** `snapshot` + `cost_estimate` error paths silently drop errors (serde + join errors) without `tracing::warn!`. *Source: 06*
- **L13.** `HashSet<&'static str>` built on every `config.rs::validate()` call for a 9-element set. Replace with slice `.contains`. *Source: 06*

---

## 4. Already documented / deferred (not re-opened)

These came up in agent output but are already tracked:

- **Project-level reaction resolution** (`remaining-to-port.md` §1.1)
- **Review thread `is_resolved`** (§1.2)
- **Restore prompt redelivery** (§1.4)
- **Missing CLI commands/flags** (§2.*, §3.*): `stop`, `--json`, `claim-pr`, `remap`, `--open`, `--prompt <text>`, `--file`, `--no-wait`, `--timeout`, `--watch`, `--rebuild`, `--fix`, `--test-notify`, `config-help`, `setup` umbrella, `plugin` marketplace.
- **Default merge method** divergence (§7.4 — documented, resolved decision).
- **GitHub Enterprise remote parsing** (§7.5 — resolved).
- **Agent trait stubs** (agent-cursor prompt-in-args, agent-aider `--yes`, agent-codex pricing).
- **Parity-only modules** (`parity_*` — classification policy in `ts-core-parity-report.md`).
- **`merge-conflicts` reaction** — resolved by #193 (issue #192 closed via `8169bfb`).

---

## 5. Recommended action order (PR-sized)

**Policy** (2026-04-18 clarification): Scope is *parity only* — fix where ao-rs diverges from ao-ts logic. Where ao-rs **adds** functionality or has Rust-specific quality issues, we only **report**, not fix.

### Must fix (parity drift — ao-rs behavior differs from ao-ts)

| # | Item | Issue | Severity | Reason it's parity |
|---|---|---|---|---|
| 1 | `defaultBranch` camelCase alias (H1) | [#194](https://github.com/duonghb53/ao-rs/issues/194) | HIGH | TS accepts; Rust silently falls back. Identical TS config breaks. |
| 2 | Duplicate project/session-prefix validation (H4) | #194 | HIGH | TS `validateProjectUniqueness` exists; Rust lets collisions through. |
| 3 | `preventIdleSleep` default on macOS (H5) | #194 | HIGH | TS: `true` on darwin; Rust: `false`. |
| 4 | `permissions` enum validation (M4) | #194 | MED | TS `z.enum` rejects typos; Rust accepts any string. |
| 5 | 3 missing `status_to_reaction_key` mappings (M1) | [#195](https://github.com/duonghb53/ao-rs/issues/195) | MED | TS dispatches on NeedsInput/Killed/Approved; Rust returns `None`. |
| 6 | `maybeDispatchReviewBacklog` port (H2) | #195 | HIGH | TS re-dispatches on new reviews; Rust stops after first transition. |
| 7 | CI-failure detail message + `all-complete` reaction (H3) | #195 | HIGH | TS formats failed checks + fires drain reaction; Rust does neither. |

### Report only — NOT parity bugs, no fix required

These are flagged for awareness but fall **outside** the parity-fix scope:

#### A. Rust-specific quality improvements (not parity)
- **H6** `panic!` on template drift in `orchestrator_prompt.rs:282` — TS would also abort on missing placeholder (functionally equivalent panic). Improvement valuable but Rust-only robustness concern.
- **H7** `expect` on Mutex poison (20 sites in `lifecycle.rs` + `reaction_engine.rs`) — TS has no mutex-poison concept (single-threaded). Latent; critical sections never panic today.
- **H8** O(n) LRU in `graphql_batch.rs` — Rust-specific perf; TS has no equivalent allocation concern.
- **L9** `cargo fmt --check` fails on 3 files — Rust-only tool hygiene.

#### B. ao-rs divergence (adds over TS, intentional or documented)
- **M3** `orchestrator_rules` falls back to `defaults.orchestrator_rules` — Rust adds a feature TS doesn't have. Document as divergence.
- **L2** `approved` semantics in `compose_merge_readiness` — Rust picks one consistent interpretation, fixing a TS internal inconsistency. Rust is more principled.
- **L3** `DIRTY` / `BLOCKED` blocker wording drift — ao-rs-specific strings; any fingerprinting keyed on exact strings would diverge, but no such consumer exists today.
- **Unified rate-limit cooldown** (`ao_core::rate_limit`) — Rust is *stricter* than TS (one shared cooldown across both GitHub plugins).

#### C. Code hygiene / consolidation (not parity)
- **M5** `gh` subprocess runner duplication in scm-github + tracker-github — Rust-only DRY improvement.
- **M6** `shell_escape` 4× duplication across plugins — already tracked in `ts-core-parity-report.md` as deferred.
- **L6** Dead `pub fn validate_symlink_entry` in `workspace_hooks.rs:66`.
- **L7** `LruCache::clear` with `#[allow(dead_code)]` in `graphql_batch.rs:79`.
- **L8** `notifier_resolution.rs` lacks required `parity_` prefix.
- **L10–L13** clone patterns, fmt nits, `HashSet` for small sets, snapshot error logging.

#### D. Documented divergences / decisions pending
- **H9** `paths.rs` hash-based layout reduction — Rust uses flat `~/.ao-rs/` by design; multi-config collision risk should be documented (see `docs/ts-core-port-map.md`).
- **M2** Orchestrator prompt template references `ao-rs`/`--task`/`ao-<short>` instead of `ao`/`--prompt`/`session/<id>` — Rust-native CLI naming, by design.
- **L1** `ScmWebhookEvent` missing `timestamp` field — no consumer needs it yet.
- **L4** `as_valid_opencode_session_id("ses_")` edge case — no opencode plugin in Rust.
- **L5** `DeleteNew → Abort` test-only path.

### Work queued

- [#194](https://github.com/duonghb53/ao-rs/issues/194) — Config hardening (4 parity fixes)
- [#195](https://github.com/duonghb53/ao-rs/issues/195) — Reaction dispatch gaps (3 parity fixes)

---

## 6. Positive notes (what's going well)

- **0 unused Cargo dependencies** — unusual for a 45K-LOC Rust workspace. Someone's been diligent.
- **`thiserror`-based error hierarchy** is clean; `anyhow` correctly absent from library crates.
- **`rate_limit.rs`** is an exemplar small module — other shared state should copy its pattern.
- **`session_manager.rs`** handles atomic rename + archive TOCTOU correctly.
- **`spawn_blocking`** is used correctly for disk I/O in `cost_estimate` + `cost_ledger::record_cost`.
- **Clippy passes with `-D warnings`**; no suppressed lints in production code.
- **Builder pattern** on `LifecycleManager` is idiomatic and keeps tests clean.
- **Core SCM transition ladder** (26 unit tests in `scm_transitions.rs`) is fully parity-validated.
- **Rate-limit handling is actually *stricter* than TS** — one shared cooldown across both plugins (post-#192-era refactor).

---

## 7. Cross-references

| Slice | File | Primary findings |
|---|---|---|
| 01 | `docs/parity-review/01-lifecycle.md` | Review-backlog, CI-detail, all-complete, 3 reaction-key mappings |
| 02 | `docs/parity-review/02-session.md` | Prompt template CLI drift, orchestrator_rules fallback, opencode ID edge |
| 03 | `docs/parity-review/03-github-plugins.md` | Webhook timestamp, approved semantics, DIRTY/BLOCKED wording |
| 04 | `docs/parity-review/04-config-and-gaps.md` | `defaultBranch`, dup validation, preventIdleSleep, paths reduction |
| 05 | `docs/parity-review/05-cleanup.md` | `shell_escape` x4, `gh` runner x2, `validate_symlink_entry` dead |
| 06 | `docs/parity-review/06-rust-idiom.md` | panic path, mutex expect, LRU cache, clone patterns |

Related existing docs:
- `docs/ts-core-port-map.md` — module-level port status
- `docs/ts-core-parity-report.md` — test suite parity
- `docs/remaining-to-port.md` — complete inventory of unported features (authoritative)
- `docs/validation-ported-code.md` — comment-driven parity map
- `docs/reactions.md` — reaction semantics reference

---

## 8. Methodology

Six agents ran in parallel (~6 min total):

- 4× `general-purpose` agents compared specific TS ↔ Rust file pairs for logic drift.
- 1× `refactor-cleaner` audited dead code, duplication, and unused deps (read-only; no modifications).
- 1× `rust-reviewer` audited ownership, error handling, async patterns, and unsafe usage (read-only).

Each agent wrote a capped report (<2000 words) to `docs/parity-review/NN-<slice>.md` and returned a short summary for synthesis. Agents had no knowledge of prior conversation context; prompts were self-contained with exact file paths.

**What this audit does NOT cover:**
- Notifier plugins (slack/discord/ntfy/desktop/stdout) — assumed low-risk plumbing.
- Agent plugins beyond the idiom review's incidental coverage.
- Runtime plugins (tmux/process) — assumed matching `ts-core-port-map.md` status.
- Web/dashboard UI — different architecture (Rust has `ao-dashboard` crate).
- Performance benchmarks — already in `scripts/benchmark.sh`.
- Security audit — out of scope.

A follow-up audit covering these areas could be spawned using the same pattern if the current findings are actioned first.
