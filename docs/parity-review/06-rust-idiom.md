# Rust idiom review

## Verdict
Minor improvements — the codebase is solid and idiomatic in most areas.
No safety or correctness blockers. Several patterns repeat across the hot
path that are worth addressing before the codebase grows further.

---

## Top findings (ranked by impact)

### [HIGH] `panic!` in production prompt-rendering path
- Location: `crates/ao-core/src/orchestrator_prompt.rs:282`
- Current pattern:
  ```rust
  None => {
      panic!("unresolved orchestrator prompt placeholder: {{{{{key}}}}}");
  }
  ```
- Issue: This executes in the `build_orchestrator_prompt` render loop for any
  placeholder key that passes `is_valid_placeholder_key` but has no matching
  branch in `lookup_placeholder`. Any new placeholder added to the template
  without updating `lookup_placeholder` panics the whole process, not just the
  spawn call. Because `orchestrator_prompt` is called on the `ao-rs watch` hot
  path when spawning orchestrators, a template drift introduced in a commit
  would take down the daemon.
- Suggested: Convert the render function to return `Result<String, AoError>`
  and replace the `panic!` with `return Err(AoError::Runtime(format!(...)))`.
  Callers already propagate `Result`.
- Impact: correctness (process crash on template drift)

### [HIGH] `expect` on mutex locks — undocumented poison behaviour
- Location: `crates/ao-core/src/lifecycle.rs` lines 162, 346, 388, 404, 637,
  647, 672, 692, 719, 839, 843, 1188; `crates/ao-core/src/reaction_engine.rs`
  lines 459, 539, 552, 562, 578, 603
- Current pattern:
  ```rust
  let mut guard = self.idle_since
      .lock()
      .expect("lifecycle idle_since mutex poisoned");
  ```
- Issue: Mutex poison happens when a thread panics while holding the lock. The
  `expect` call panics the current async task on poison, which aborts the
  entire poll tick — a worse outcome than continuing with stale state. The
  critical sections are currently non-panicking, so poisoning is effectively
  impossible today, but that invariant is not documented anywhere. A future
  `spawn_blocking` inside a lock critical section would silently introduce
  the risk.
- Suggested: Use `unwrap_or_else(|p| p.into_inner())` (recover from poison
  by taking the inner data) with a `tracing::error!` for observability.
  Alternatively, document the invariant with a `// SAFETY:` comment at the
  struct definition.
- Impact: correctness / availability

### [HIGH] O(n) LRU cache with unconditional clone on every hit
- Location: `crates/plugins/scm-github/src/graphql_batch.rs:58-66`
- Current pattern:
  ```rust
  fn get(&mut self, key: &str) -> Option<V> {
      if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
          let entry = self.entries.remove(pos);  // O(n) shift
          let val = entry.1.clone();             // clone to return
          self.entries.push(entry);              // re-insert original
          Some(val)
      } else { None }
  }
  ```
- Issue: `Vec::remove` shifts all subsequent elements — O(n). For the
  `commit_status_etags` cache (up to 500 entries) this is a linear scan on
  every hit. The `entry.1.clone()` is also unnecessary: the value is cloned
  solely to satisfy the return, while the original is re-inserted.
- Suggested: Use `indexmap::IndexMap` with LRU eviction, or the `lru` crate.
  At minimum, change the return to `Option<&V>` where callers only need a
  borrow — this eliminates the clone entirely.
- Impact: perf (O(n) in a per-tick cache path; allocations per cache hit)

---

## Ownership & clones

### `session.id.clone()` repeated 10+ times per session per tick
- Location: `crates/ao-core/src/lifecycle.rs` — `tick()` and `poll_one()`
- `SessionId` wraps a `String`. Every `OrchestratorEvent` constructor call in
  the inner loop clones the ID. A `let id = &session.id;` binding used for
  all emit calls would reduce this to one move at event construction.

### Redundant `let c = cost.clone()` in `transition`
- Location: `crates/ao-core/src/lifecycle.rs:888`
- `cost` is not used after the `spawn_blocking` call, so `cost.clone()` can
  be removed in favour of moving `cost` directly into the closure.

### `PathBuf` clone in `Agent::cost_estimate` default
- Location: `crates/ao-core/src/traits.rs:131`
- `ws.clone()` is necessary for the `'static` closure bound. Using
  `ws.to_path_buf()` instead of `.clone()` communicates intent more clearly.

---

## Error handling

### `cost_estimate` default silently drops `JoinError`
- Location: `crates/ao-core/src/traits.rs:134`
  ```rust
  .await.unwrap_or(None);
  ```
- A panic inside `parse_usage_jsonl` (inside `spawn_blocking`) becomes a
  `JoinError` mapped silently to `None`. Cost tracking stalls without any log
  signal.
- Suggested: `.unwrap_or_else(|e| { tracing::warn!("cost_estimate task failed: {e}"); None })`

### Snapshot serialization failure is silent
- Location: `crates/ao-core/src/parity_observability.rs:199`
  ```rust
  serde_json::to_string_pretty(snap).unwrap_or_else(|_| "{}".into())
  ```
- Silent fallback to `"{}"` writes a corrupted snapshot without any log signal.
  Add `tracing::warn!` on the error branch.

### `parity_metadata` uses `String` as error type throughout
- Location: `crates/ao-core/src/parity_metadata.rs` — all public functions
- Returns `Result<_, String>` instead of `Result<_, AoError>`, breaking
  composability. Convert to `AoError::Other` to unify with the domain error
  hierarchy.

---

## Async

### `std::fs` blocking I/O called from async context
- Location: `crates/ao-core/src/activity_log.rs:32-40`
  ```rust
  // append_activity_entry — called from async detect_activity default
  std::fs::create_dir_all(parent)?;
  std::fs::OpenOptions::new().create(true).append(true).open(p)?...
  ```
- `append_activity_entry` does synchronous file I/O and is called from
  `Agent::detect_activity`, which is `async fn`. On a slow filesystem this
  blocks a tokio worker thread.
- Suggested: Wrap the call site in `tokio::task::spawn_blocking` (consistent
  with the `cost_estimate` pattern), or convert to `async fn` with `tokio::fs`.
- Impact: async correctness / perf

---

## Collections

### Ephemeral `HashSet` built on every `validate()` call
- Location: `crates/ao-core/src/config.rs:78, 118`
  ```rust
  let known: std::collections::HashSet<&'static str> =
      supported_reaction_keys().into_iter().collect();
  ```
- Not a hot path, but a 9-element array does not warrant a `HashSet`.
  Use `slice.contains(&key.as_str())` directly.

---

## Other

### `unsafe` in webhook tests missing `// SAFETY:` comment
- Location: `crates/plugins/scm-github/src/webhook.rs:633, 650, 658, 675`
  and `crates/plugins/scm-gitlab/src/webhook.rs:548, 565, 573, 591`
  ```rust
  unsafe { std::env::set_var(env_var, SECRET); }
  ```
- `std::env::set_var` is `unsafe` in Rust 2024. These tests are single-
  threaded today, but no `// SAFETY:` comment documents that invariant.
- Suggested: Add `// SAFETY: test is single-threaded; no concurrent env readers.`

### `cargo fmt --check` fails on three files
- `crates/ao-core/src/paths.rs:65`
- `crates/ao-core/tests/parity_modules_meta.rs:92`
- `crates/plugins/notifier-ntfy/src/lib.rs:150`
- Run `cargo fmt --all` to fix.

---

## Positive notes

- `ao-core/src/error.rs` is clean: `thiserror`-derived, sensible variants,
  no stringly-typed errors at the domain layer. `anyhow` is correctly absent
  from library crates.
- `rate_limit.rs` is an exemplary small module: `Mutex<Option<Instant>>` with
  `OnceLock`, consistent poison recovery, and a well-commented "never shorten
  an existing cooldown" invariant in `enter_cooldown_for`.
- `session_manager.rs` uses atomic rename for persistence and handles the
  `archive` TOCTOU correctly via `ErrorKind::NotFound` matching.
- All `unsafe` in production paths (`lockfile.rs:152`, `stop.rs:68,179`)
  carry accurate `// SAFETY:` comments.
- The `LifecycleManager` builder pattern is idiomatic and keeps the test
  harness clean.
- Clippy passes clean with `-D warnings`. No suppressed lints in production
  code paths.
- `spawn_blocking` is used correctly for disk-intensive operations
  (`cost_estimate`, `cost_ledger::record_cost`).
