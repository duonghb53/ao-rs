# 7.1 paths subset — requirements & plan

**Issue**: https://github.com/duonghb53/ao-rs/issues/106
**Status**: in progress

## Why

`crates/ao-core/src/paths.rs` is the Slice 1 scope of `packages/core/src/paths.ts`.
Since Slice 1, other features have landed (`cost_ledger`, `review_check`) that
hardcode joins against `paths::data_dir()` instead of centralising the on-disk
location. The acceptance criterion for #106 is: **newly required on-disk
locations have a single source of truth in `paths.rs`**.

## Inventory of hardcoded paths (2026-04)

Found by grep for `data_dir().join(`:

| Location (file:line)                                      | Path                                   | Already in `paths.rs`? |
|-----------------------------------------------------------|----------------------------------------|------------------------|
| `crates/ao-core/src/paths.rs:18`                          | `~/.ao-rs/sessions`                    | ✓ `default_sessions_dir()` |
| `crates/ao-core/src/paths.rs:25`                          | `~/.ao-rs/lifecycle.pid`               | ✓ `lifecycle_pid_file()` |
| `crates/ao-core/src/cost_ledger.rs:40` (`ledger_dir()`)   | `~/.ao-rs/cost-ledger/`                | ✗ — local helper only   |
| `crates/ao-cli/src/commands/review_check.rs:31,83`        | `~/.ao-rs/review-fingerprints/{session_id}.txt` | ✗ — hardcoded   |

**Out of scope** for this issue (no corresponding ao-rs feature exists yet, per the
"only add what’s needed by real features" risk note):

- `~/.agent-orchestrator/{hash}-observability/…` (TS `getObservabilityBaseDir`)
- plugin-registry on-disk layout
- hash-based project dirs (`generateConfigHash`, `getProjectBaseDir`, `.origin`)

`activity_log::activity_log_path()` is workspace-relative (`{ws}/.ao/activity.jsonl`),
not data-dir based; leaving its helper in `activity_log.rs` (parity with TS
`activity-log.ts`) is fine.

## Target state

Add to `crates/ao-core/src/paths.rs`:

1. `cost_ledger_dir() -> PathBuf` → `~/.ao-rs/cost-ledger/`
2. `review_fingerprint_dir() -> PathBuf` → `~/.ao-rs/review-fingerprints/`
3. `review_fingerprint_file(session_id: &str) -> PathBuf` → `~/.ao-rs/review-fingerprints/{session_id}.txt`

All are pure, side-effect-free functions returning `PathBuf`.

## Caller updates

- `cost_ledger.rs`: `ledger_dir()` delegates to `paths::cost_ledger_dir()` (keep
  the public re-export so existing callers don’t break).
- `review_check.rs`: replace `paths::data_dir().join("review-fingerprints")` and
  the per-session `.join(format!(…))` with `paths::review_fingerprint_dir()` and
  `paths::review_fingerprint_file(&session.id.0)`.

## Acceptance criteria

- [x] Inventory complete (table above).
- [ ] `paths.rs` exposes helpers for every hardcoded `~/.ao-rs/<something>`
      path used by a shipped feature.
- [ ] Existing callers use the helpers — no `data_dir().join("cost-ledger")` or
      `data_dir().join("review-fingerprints")` outside `paths.rs`.
- [ ] Unit tests in `paths.rs` verify stable formatting of each new helper.
- [ ] `cargo t` + `cargo test --doc` + `cargo clippy --all-targets` pass.

## Test plan

- Unit tests inside `paths.rs` for each new helper:
  - suffix matches (e.g. ends with `cost-ledger`),
  - parent is `data_dir()`,
  - `review_fingerprint_file("foo")` ends with `foo.txt` and lives under
    `review_fingerprint_dir()`.
- Existing `cost_ledger` and `review_check` test suites continue to pass.

## Risks / notes

- Keep `paths.rs` minimal — only helpers for paths that are actively used today.
- Don’t introduce a `Paths` struct; free functions match the TS module style.
- `cost_ledger::ledger_dir()` stays public (same signature) — it just delegates.
  This avoids churn in any tests/docs referencing it by name.
