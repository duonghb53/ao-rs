# Session + orchestrator parity

## Verdict
minor drift — mostly intentional (on-disk format divergence, no opencode plugin in Rust), with a few real but low-blast-radius bugs in the parity helper module and the orchestrator prompt template.

## Parity-confirmed
- Orchestrator N-reservation algorithm (smallest unused `N`, 10k cap): `crates/ao-core/src/orchestrator_spawn.rs:60-82` vs `packages/core/src/session-manager.ts:690-744`.
- Orchestrator classification by id pattern: `crates/ao-core/src/orchestrator_spawn.rs:89-99` vs `isOrchestratorSessionRecord` at `session-manager.ts:333-347`.
- Orchestrator prompt rendering algorithm (placeholder substitution + `{{NAME_START}}…{{NAME_END}}` block stripping): `crates/ao-core/src/orchestrator_prompt.rs:28-296` vs `packages/core/src/orchestrator-prompt.ts:70-198`.
- Automated-reactions section formatting (retries/escalate/priority labels): `orchestrator_prompt.rs:126-172` vs `orchestrator-prompt.ts:33-59`.
- Session find-by-prefix semantics (empty → `SessionNotFound`, ambiguous → `Runtime` error with match count): `session_manager.rs:133-153`.
- Restore gate (terminal + `is_restorable`, live-probe enrichment): `restore.rs:58-145` vs `session-manager.ts` `restore` path (line ~2254).
- `OrchestratorSessionStrategy` config deserialization accepts all 6 variants including kebab-case aliases: `config.rs:515-523`.

## Drift (severity: HIGH/MED/LOW)

### [MED] `as_valid_opencode_session_id("ses_")` accepts empty suffix; TS rejects
- Rust: `crates/ao-core/src/opencode_session_id.rs:10-17`
- TS:   `packages/core/src/opencode-session-id.ts:1`
- Difference: TS regex `/^ses_[A-Za-z0-9_-]+$/` requires ≥1 char after `ses_`. Rust uses `s.bytes().skip(4).all(...)`, which returns `true` for an empty iterator, so bare `"ses_"` is accepted and returned as `Some("ses_")`.
- Impact: if an opencode plugin ever consumes such a string it would call `opencode session delete ses_` or similar, producing a broken CLI invocation or spurious match. Today Rust has no opencode plugin so blast radius is zero — but the parity test suite (`tests/parity_utils_parity_test.rs:23-28`) passes because it doesn't cover this edge.
- Fix: add an explicit length check: `if s.len() <= 4 { return None; }` before the byte scan, or use `s.bytes().skip(4).peekable()` and require `.peek().is_some()`.

### [MED] Orchestrator prompt rules falls back to `defaults.orchestratorRules`; TS doesn't
- Rust: `crates/ao-core/src/orchestrator_prompt.rs:174-194` (`project_rules.or(default_rules)`)
- TS:   `packages/core/src/orchestrator-prompt.ts:61-68` (reads only `project.orchestratorRules`)
- Difference: Rust silently promotes `defaults.orchestratorRules` into the rendered "Project-Specific Rules" block when the project itself has no rules. TS never does — the block is stripped.
- Impact: users migrating a shared `ao-rs.yaml` with global orchestrator guidance see it in every orchestrator's prompt under Rust, which would not appear under ao-ts. Content/behavior drift, not a crash.
- Fix: drop the `default_rules` branch, or promote the fallback to `config.ts` so both sides go through the same `project.orchestrator_rules` field at read time.

### [MED] No cross-project `sessionPrefix` collision check in orchestrator reservation
- Rust: `crates/ao-core/src/orchestrator_spawn.rs:60-82`
- TS:   `packages/core/src/session-manager.ts:713-725`
- Difference: TS aborts with a loud "orchestrator prefix conflicts with project X" error when another project's `sessionPrefix === "<this-project>-orchestrator"` (which would make that project's worker ids collide with this project's orchestrator ids). Rust just starts handing out ids and silently collides.
- Impact: pathological yaml config — rare in practice — but the Rust failure mode is a silent duplicate id instead of a clear error.
- Fix: before the reservation loop, iterate `cfg.config.projects` and compare `other.session_prefix.as_deref().unwrap_or(&other_id)` to `format!("{project_prefix}-orchestrator")`; return `AoError::Runtime(...)` with the TS-style message.

### [MED] Orchestrator prompt template diverges substantially from ao-ts
- Rust: `crates/ao-core/src/prompts/orchestrator.md`
- TS:   `packages/core/src/prompts/orchestrator.md`
- Difference: the Rust template has been rewritten with `ao-rs` CLI invocations and a different surface (e.g. `ao-rs spawn --task "..."`, `ao-rs status --project <id>`, `ao-rs cleanup --project <id>`, branch example `ao-<short-id>`) whereas TS uses `ao spawn --prompt "..."`, `ao session ls -p <project>`, `ao session cleanup -p <project>`, `session/<id>`. The TS template also includes `ao open <project>` (no Rust equivalent yet).
- Impact: the rendered orchestrator prompt instructs the agent to run non-existent commands if ao-rs adopts ao-ts naming (or vice versa). This is the single biggest source of "orchestrator output doesn't match docs" drift.
- Fix: either (a) document the rename explicitly in `README` and keep templates separate, or (b) resync the Rust template to the ao-ts wording and let the rust binary publish an `ao` alias. Also re-add the `ao open` section behind a feature flag until the Rust port has that command.

### [LOW] `decide_existing_session_action(DeleteNew, true)` returns `Abort`; TS normalizes it to `delete`
- Rust: `crates/ao-core/src/parity_session_strategy.rs:57`
- TS:   `packages/core/src/orchestrator-session-strategy.ts:8` (`"delete-new" → "delete"`)
- Difference: TS normalizer collapses `delete-new` into `delete`. In the Rust helper `DeleteNew` maps to `Abort` instead of `DeleteExistingAndReuseName`.
- Impact: low — the module is test-only per its doc comment, and the production runtime doesn't call `decide_existing_session_action`. But the helper is exported from `ao_core` and labelled as the parity port, so anyone reading it would get the wrong mental model.
- Fix: map `DeleteNew => DeleteExistingAndReuseName` (match TS normalization) or rename the helper to make its intent explicit.

### [LOW] Orchestrator persists `spawning` status before flipping to `working`; TS writes `working` once
- Rust: `crates/ao-core/src/orchestrator_spawn.rs:155, 174, 195-196`
- TS:   `packages/core/src/session-manager.ts:1473, 1489-1500`
- Difference: Rust saves the session twice (first as `Spawning`, then `Working` after `runtime.create`). TS writes metadata once, with `status: "working"`, after `runtime.create` succeeds.
- Impact: a narrow race window where a concurrent `ao-rs status` could observe the orchestrator as `spawning` with no `runtime_handle`. Cosmetic only; doesn't affect lifecycle loop logic.
- Fix: defer the first save to after `runtime.create` (mirror TS ordering), or accept the minor UX difference and document it.

## Missing
- **Orchestrator opencode-session reuse/delete**: TS `spawnOrchestrator` wires `resolveOpenCodeSessionReuse` + `discoverOpenCodeSessionIdByTitle` based on `orchestratorSessionStrategy`. Rust skips this entirely (no opencode plugin in the port yet). Acceptable but should be tracked as a follow-up when opencode lands.
- **System-prompt-file mode**: TS writes the orchestrator system prompt to a file under `<base>/orchestrator-prompt-<sessionId>.md` (for agents that accept `systemPromptFile`). Rust delivers via `runtime.send_message` after a 2.5s sleep. The Rust approach is simpler but fragile for very long prompts; for claude-code it works because CC has no `--system-prompt-file` flag.
- **Workspace hooks**: TS calls `agent.setupWorkspaceHooks(workspacePath, {dataDir})` on orchestrator spawn (for tracker/lifecycle webhooks etc.). Rust `spawn_orchestrator` does not. Hooks may be set up elsewhere, but the call site parity is gone.
- **Recovery/restore subsystem**: TS has a full `recovery/` package (scanner, validator, actions, manager, logger) for bulk session recovery (auto-detect crashed processes, auto-cleanup orphan worktrees, log-retention). Rust `restore.rs` is a single-session manual-invoke. Bulk recovery is absent. Not a parity bug per se, but a sizable functional gap.
- **Duplicate-PR repair on read / PR-ownership handoff**: TS `repairSessionMetadataOnRead` (session-manager.ts:444-495) de-dupes orchestrator metadata (clears stale `pr`/`prAutoDetect` fields) and picks a single PR owner when multiple sessions claim the same PR. Rust has no equivalent read-time repair — if bad YAML sneaks in, it stays.

## Notes
- The on-disk session format diverges intentionally: Rust uses YAML with `snake_case` Rust field names under `~/.ao-rs/sessions/<project>/<uuid>.yaml`, while TS uses per-session `key=value` metadata files under `<config>/<sha>/sessions/<sessionId>`. Round-trip parity between the two formats is neither implemented nor claimed.
- Session ids in Rust are UUIDs (with an 8-char short-id prefix-match for the CLI); TS uses `<sessionPrefix>-<N>`. The Rust orchestrator path alone uses `<prefix>-orchestrator-N` to stay compatible with TS orchestrator id assumptions.
- `SessionManager` in Rust is a pure on-disk CRUD type. The TS `createSessionManager` is the large facade that wires runtime/agent/tracker plugins and implements spawn/list/kill/cleanup/claim/restore. The corresponding Rust logic lives in `orchestrator_spawn.rs`, `restore.rs`, and `lifecycle.rs` — split rather than colocated.
