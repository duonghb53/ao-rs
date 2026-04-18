# ao-rs intentional divergences from ao-ts

**Date:** 2026-04-18
**Source:** `docs/parity-review/REPORT.md` §5 Report-only items (B) and (D).

This document records the places where ao-rs deliberately differs from the
TypeScript reference implementation (ao-ts / agent-orchestrator). Each entry
explains *why* the divergence exists and whether it is a quality improvement, an
intentional addition, a naming decision, or an accepted gap.

---

## 1. Behavior improvements (Rust > TS)

### 1.1 `approved` semantics in `compose_merge_readiness` (REPORT L2)

**Source:** `docs/parity-review/03-github-plugins.md`; REPORT §5 (B).

**What differs:** ao-rs evaluates the `approved` flag using GraphQL-batch
semantics consistently across all code paths. The TypeScript reference has an
internal inconsistency:

- `getMergeability` (non-batch path) treats an empty/NONE review state as
  `approved = false`.
- The batch path treats it as `approved = true`.

ao-rs unifies on the GraphQL-batch interpretation: a PR with no reviews yet is
not blocked by the `approved` gate (treating no reviews as implicitly not
blocking). This is more principled and avoids a class of spurious `Blocked`
states for freshly-opened PRs.

**Impact:** Reactions gating on `approved` for unreviewed PRs will behave
consistently in Rust. Any external system fingerprinting on the exact `approved`
boolean value for the no-review case would see a different result than TS, but
no such consumer exists in the current codebase.

**Decision:** Keep Rust behaviour; document here. Do not revert to TS
inconsistency.

---

### 1.2 Unified `ao_core::rate_limit` cooldown across both GitHub plugins (REPORT §5 B)

**Source:** `docs/parity-review/03-github-plugins.md`; REPORT §5 (B).

**What differs:** ao-rs shares a single `RateLimitCooldown` instant inside
`ao_core::rate_limit` across *both* `scm-github` and `tracker-github`. When
either plugin hits a GitHub rate-limit, both plugins back off together for the
remainder of the cooldown window.

TypeScript has separate rate-limit state per plugin (separate module-level
timestamps), so a rate-limit hit on the SCM plugin does not affect the tracker
plugin and vice versa.

**Impact:** ao-rs is *stricter* — a single rate-limit event causes both GitHub
plugins to pause, reducing the risk of compounding secondary rate-limit
penalties from concurrent retries. The downside is that an SCM rate-limit
briefly pauses tracker polling as well.

**Decision:** Rust behaviour is intentionally stricter. No change required.

---

## 2. Intentional additions (Rust has, TS lacks)

### 2.1 `orchestrator_rules` global fallback (REPORT M3)

**Source:** `docs/parity-review/02-session.md`; REPORT §5 (B).

**What differs:** `crates/ao-core/src/orchestrator_prompt.rs:174-194` falls
back to `defaults.orchestrator_rules` when the project-level
`project.orchestrator_rules` field is absent.

TypeScript reads only `project.orchestratorRules`; there is no fallback to a
global default.

**Impact:** Users who set `defaults.orchestrator_rules` in their ao-rs config
get those rules injected into every orchestrator prompt automatically, without
repeating them per project. TS users porting a config that relies on the global
fallback would need to copy the value into each project block.

**Decision:** ao-rs quality-of-life addition. Behaviour is documented here.
If the TS reference ever adds a global fallback, reconcile at that point.

---

## 3. Naming / schema deltas (intentional)

### 3.1 `paths.rs` flat directory layout (REPORT H9)

**Source:** `docs/parity-review/04-config-and-gaps.md`; REPORT §5 (D).
Also noted in `docs/ts-core-port-map.md` as "ported (different layout)".

**What differs:**

| | ao-rs | ao-ts |
|---|---|---|
| Sessions dir | `~/.ao-rs/sessions/` | `~/.ao-rs/<hash>/sessions/` |
| Worktrees dir | `~/.ao-rs/worktrees/` | `~/.ao-rs/<hash>/worktrees/` |

TypeScript computes a `generateConfigHash` from the config file path and uses it
as a per-config sub-directory (`getProjectBaseDir`). This lets multiple ao-ts
configs on the same machine maintain independent state.

ao-rs uses a flat `~/.ao-rs/` layout for simplicity (108 LoC vs 211 LoC).

**Known limitation:** Two ao-rs configs running on the same machine share the
same `~/.ao-rs/` directory. Sessions and worktrees from different configs will
interleave. This is only a problem when running multiple ao-rs instances with
different config files simultaneously, which is not a supported use-case today.

**Decision deferred:** Keep flat layout for now. A hash-based layout port can be
added later if multi-config support becomes a requirement. The collision risk is
documented here; no guard is implemented.

---

### 3.2 Orchestrator prompt CLI naming (REPORT M2)

**Source:** `docs/parity-review/02-session.md`; REPORT §5 (D).

**What differs:** `prompts/orchestrator.md` references:

| ao-rs | ao-ts |
|---|---|
| `ao-rs status --project` | `ao status -p` |
| `--task` | `--prompt` |
| `ao-<short>` branch names | `session/<id>` branch names |

These are intentional — each port uses its own CLI surface and branch
conventions. Copying the orchestrator prompt from one port to the other without
editing the CLI flag names would produce non-functional instructions.

**Decision:** By design. The prompt template is not shared between ports.

---

### 3.3 `BLOCKED` / `DIRTY` blocker message strings (REPORT L3)

**Source:** `docs/parity-review/03-github-plugins.md`; REPORT §5 (B).

**What differs:**

| State | ao-rs message | ao-ts message |
|---|---|---|
| `DIRTY` | `"Merge is blocked (conflicts or failing requirements)"` | `"Merge is blocked by branch protection"` |
| `BLOCKED` | `"Branch protection requirements not satisfied"` | `"Merge is blocked by branch protection"` |

ao-rs provides more descriptive, state-specific messages. The TS reference uses
the same string for both states.

**Impact:** Any downstream system performing exact-string matching on these
blocker messages would see different text. No such consumer exists in the
current codebase.

**Decision:** ao-rs strings are intentionally more descriptive. Document here;
no change required unless a consumer is added that depends on the exact text.

---

## 4. Accepted gaps (test-only paths, no runtime impact)

### 4.1 `ScmWebhookEvent` missing `timestamp` field (REPORT L1)

**Source:** `docs/parity-review/03-github-plugins.md`; REPORT §5 (D).

TypeScript parses `timestamp` from `updated_at` / `submitted_at` / `created_at`
fields in the webhook payload and attaches it to `ScmWebhookEvent`. ao-rs omits
this field.

**Impact:** Zero — no ao-rs consumer reads `timestamp` from webhook events. The
field would only be needed for event-reordering logic, which has not been ported.

**Decision:** Accepted gap. Add `timestamp` to `ScmWebhookEvent` when the first
consumer requires it.

---

### 4.2 `decide_existing_session_action(DeleteNew)` → `Abort` (REPORT L5)

**Source:** `docs/parity-review/02-session.md`; REPORT §5 (D).

**What differs:** When `decide_existing_session_action` receives `DeleteNew`,
ao-rs returns `Abort`. The TypeScript reference performs a delete-after-normalization
step and returns a different action.

**Impact:** This code path is exercised only in tests; no production caller
passes `DeleteNew` today.

**Decision:** Accepted gap for now. Revisit if `DeleteNew` semantics are needed
in production.

---

*For the authoritative list of unported features, see `docs/remaining-to-port.md`.*
*For the full audit findings, see `docs/parity-review/REPORT.md`.*
