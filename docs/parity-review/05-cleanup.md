# Code cleanup audit

## Verdict
Small cleanups — no major rot. The codebase is structurally sound. The main opportunities are `shell_escape` consolidation (4 duplicates), two `gh` subprocess runner functions that are nearly identical, a cluster of `#[allow(dead_code)]` structs in `tracker-linear`, one dead production function (`validate_symlink_entry`), and a handful of test-only modules that are correctly silenced but worth noting.

## Summary
- 1 dead public function (`validate_symlink_entry` — exported but never called outside its own file/tests)
- 4 duplicated `shell_escape` implementations across plugin crates + 1 in ao-cli
- 2 nearly-identical `gh` subprocess runner functions (`scm-github/src/lib.rs` vs `tracker-github/src/lib.rs`)
- 1 `#[allow(dead_code)]` method on a non-test struct (`LruCache::clear` in `scm-github`)
- 10 `#[allow(dead_code)]` fields in `tracker-linear` (intentional forward-compat serde shapes — low value to remove)
- 2 `#[allow(dead_code)]` items in `lifecycle.rs` test helpers (expected — test scaffolding)
- 0 unused cargo dependencies found
- `notifier_resolution` module is test-only in practice but not marked as such
- `opencode_session_id` is a single 18-line file that could fold into `parity_utils`

---

## Dead code

### `crates/ao-core/src/workspace_hooks.rs:66` — `pub fn validate_symlink_entry`
- Reason: Exported as `pub fn` but only called from within `workspace_hooks.rs` itself (at line 19) and its own `#[cfg(test)]` block. No caller outside the file — confirmed by `rg "validate_symlink_entry" -t rust`.
- LOC to remove: 0 (change visibility to `pub(crate)` or `fn` — not a deletion but a correction)
- Risk: low — no external crate depends on this symbol; it is not re-exported in `lib.rs`
- Verify: `rg "validate_symlink_entry" -t rust`

### `crates/plugins/scm-github/src/graphql_batch.rs:79` — `LruCache::clear`
- Reason: `#[allow(dead_code)]` on a private method that is never called — confirmed by `rg "\.clear\(\)" graphql_batch.rs`.
- LOC to remove: 3 (method + attribute)
- Risk: low — private method on a private struct
- Verify: `rg "\.clear\(\)" crates/plugins/scm-github/src/graphql_batch.rs`

### `crates/ao-core/src/notifier_resolution.rs` — entire module (production-side)
- Reason: `notifier_resolution` is exported in `lib.rs` at line 10 but has zero callers in `ao-core/src/`, `ao-cli/src/`, `ao-dashboard/src/`, or any plugin crate. Only consumer is the test file `tests/parity_utils_parity_test.rs`. It is a parity-only module with no `parity_` prefix to signal this.
- LOC to remove: 0 immediately, but the module should be moved to the parity-only set (rename to `parity_notifier_resolution.rs`, update `lib.rs`) and documented alongside the other six parity-only modules.
- Risk: low — adding the `parity_` prefix and updating `lib.rs` is a pure rename
- Verify: `rg "notifier_resolution\|resolve_notifier_target\|ResolvedNotifierTarget\|NotifierConfig" crates --include="*.rs" | grep -v "tests/\|notifier_resolution.rs"`

---

## Duplicated logic

### `shell_escape` — 4 copies in plugin crates + 1 variant in ao-cli

Locations:
- `crates/ao-core/src/parity_utils.rs:14` — always wraps in single quotes (parity-only, not suitable as canonical)
- `crates/plugins/runtime-tmux/src/lib.rs:253` — safe-set skip, then single-quote wrapping (most complete)
- `crates/plugins/agent-codex/src/lib.rs:228` — safe-set skip with `:` in safe set; empty string → `''`
- `crates/plugins/agent-aider/src/lib.rs:178` — always wraps (same as `parity_utils` variant)
- `crates/ao-cli/src/cli/spawn_helpers.rs:3` — always wraps, named `shell_escape_single_quotes`

The `runtime-tmux` variant is the most defensively correct (safe-set fast path, single-quote escape). The `agent-codex` variant adds `:` to the safe set (handles model strings like `gpt-4o:latest`).

Proposed extraction target: `crates/ao-core/src/shell_escape.rs` — expose `pub fn shell_escape(s: &str) -> String` with the `runtime-tmux` logic plus `:` in the safe set. Each plugin and `ao-cli` drops its local copy and uses `ao_core::shell_escape`.

LOC saved if consolidated: ~30 lines of duplicate + tests scattered across 4 files; the canonical version gains ~15 lines of tests. Net: ~15 lines removed.

Note: `parity_utils::shell_escape` should be kept as-is (it tests the TS-reference behaviour of always wrapping). Only the four production copies should be consolidated.

### `gh` subprocess runner — 2 copies

Locations:
- `crates/plugins/scm-github/src/lib.rs:~800` — named `run_cmd`, takes `bin` + `args` + optional `cwd`, does timeout + env hardening + rate-limit detection
- `crates/plugins/tracker-github/src/lib.rs:594` — named `gh`, takes `args` slice only, same env vars, same timeout constant, same rate-limit hook

The two functions are structurally identical except `scm-github`'s version generalises over `bin` (used for both `gh` and `git`). The `tracker-github` copy could be deleted and replaced with a direct call to `scm-github`'s helper, but that would create a crate dependency that doesn't currently exist. The cleaner path is to promote a shared `run_gh_subprocess(args, cwd)` helper into `ao-core::rate_limit` (which both plugins already import) or a new `ao-core::subprocess` module.

LOC saved: ~25 lines (the tracker copy)
Risk: medium — requires adding a new public API to `ao-core` or restructuring crate deps

---

## Unused dependencies

No unused Cargo dependencies found. Every dep declared in `Cargo.toml` files is imported by at least one source file in that crate. (`reqwest` in `scm-gitlab` and `tracker-linear` is used; `semver` in `ao-cli` is used in `commands/update.rs`; `portable-pty` and `tokio-tungstenite` in `ao-dashboard` are used by the terminal WebSocket route.)

---

## Unused imports / mods

- `crates/ao-core/src/lib.rs:10` — `pub mod notifier_resolution` exports a module with no production callers (see dead code section above). This is a minor API surface issue, not a compile error.
- `crates/ao-core/src/lib.rs:11` — `pub mod opencode_session_id` exposes a single 18-line file. It is used in tests and config struct field names. It could be folded into `parity_utils` to reduce module count, but this is cosmetic.

---

## `#[allow(dead_code)]` suppressions — inventory

| File | Lines | Context | Action |
|---|---|---|---|
| `scm-github/src/graphql_batch.rs` | 79, 92 | `LruCache::clear` (dead method), `PrMetadata::ci_status` (field deserialized but not read) | Remove `clear`; keep `ci_status` allow (field read by serde) |
| `tracker-linear/src/lib.rs` | 283–346 | 10 fields across `LinearIssue`, `LinearState`, `LinearTeam`, `LinearProject`, `LinearCycle` structs — forward-compat serde shapes per comment | Keep — intentional; comment documents rationale |
| `ao-core/src/lifecycle.rs` | 1548, 1613, 1618 | `MockRuntime::destroyed_handles`, `MockWorkspace` struct and its impl — test helpers not used by any current test | Remove `destroyed_handles` allow and the method; remove `MockWorkspace` struct and impl (22 LOC) if no test exercises it |
| `ao-core/tests/parity_test_utils.rs` | 17 | Module-level allow on test helpers | Expected — keep |
| `ao-core/tests/parity_modules_meta.rs` | 17 | Single field on test struct | Expected — keep |

---

## Refactor opportunities (not strictly dead, but messy)

- **`parity_*` module naming inconsistency**: `notifier_resolution` is a parity-only module without the `parity_` prefix. The `parity_modules_meta.rs` test enforces the invariant for the six listed parity-only files but does not cover `notifier_resolution`. Either rename it or add it to the meta-test's `parity_only` list.

- **`opencode_session_id.rs` as a micro-module**: One 18-line pure function in its own file. The function is used by tests and config. Could live in `parity_utils` (where the test `parity_utils_parity_test.rs` already imports it from). Saves one module declaration.

- **`MockWorkspace` in `lifecycle.rs` tests**: `MockWorkspace` (lines 1613–1643) is annotated `#[allow(dead_code)]` with no test calling `destroyed_paths()`. The struct itself IS used (the `Workspace` impl), but the tracking field `destroyed` and its accessor are dead. Either write a test that uses it or remove the `destroyed` field and impl-side recording.

- **`shell_escape` variants differ semantically**: `agent-aider` always wraps (even safe strings like `gpt-4o`) while `runtime-tmux` and `agent-codex` skip wrapping for safe strings. This inconsistency is benign for correctness (single-quoted safe strings still execute correctly in POSIX shells) but it means the two variants produce different output for safe inputs. Any consolidation must pick one semantic — the skip-for-safe-strings variant is preferred (matches tmux/codex, produces cleaner `tmux new-session` invocations).

---

## Out-of-scope / deferred

- `merge-conflicts` reaction key dead code — tracked in issue #192.
- All `parity_*` test-only modules — intentionally ported scaffolding; removal deferred until parity tracking is complete.
- `ao-dashboard/src/bin/terminal_load.rs` — a load-testing CLI binary, not a dead file. No Cargo `[[bin]]` entry needed; Cargo auto-discovers `src/bin/*.rs`.
- `tracker-linear` forward-compat serde fields — intentional per code comment; not dead.

---

## Recommended next steps

1. **PR: restrict `validate_symlink_entry` visibility** — change `pub fn` to `pub(crate) fn` in `workspace_hooks.rs`. One-line change, zero risk. Verify: `cargo build --workspace`.

2. **PR: remove `LruCache::clear` and `MockWorkspace::destroyed` dead items** — delete `graphql_batch.rs:79-82` (4 lines) and `lifecycle.rs:1613-1629` `MockWorkspace` field + accessor (if no test is added for it). ~8 LOC removed. Run `cargo t -p ao-core -p ao-plugin-scm-github`.

3. **PR: rename `notifier_resolution` to `parity_notifier_resolution`** — update `lib.rs`, the one test import, and add it to the `parity_modules_meta.rs` invariant list. Zero runtime impact; enforces existing convention.

4. **PR: consolidate `shell_escape` into `ao-core`** — add `pub mod shell_escape` (or `pub fn shell_escape` directly in a suitable existing module), update `runtime-tmux`, `agent-codex`, `agent-aider`, and `ao-cli/spawn_helpers`. Run full `cargo t --workspace`. Medium effort, ~15 LOC net reduction, removes future drift risk.
