# Issue #69 — ao (TS) ⇄ ao-rs (Rust) CLI parity inventory

Source of truth for TS CLI: `agent-orchestrator/packages/cli` (Commander program entry `src/program.ts`, command modules in `src/commands/`).

Source of truth for Rust CLI: `crates/ao-cli/src/cli/args.rs` (Clap `derive` definitions), with behavior implemented under `crates/ao-cli/src/commands/`.

This doc is a **Phase 1 inventory**: enumerate everything the TS CLI exposes, and mark ao-rs parity as **done / partial / missing / intentionally different** (with reasons).

## Scope + assumptions

- TS CLI referenced below is from `ComposioHQ/agent-orchestrator` because `duonghb53/agent-orchestrator` is not publicly accessible (404 at time of audit).
- Line anchors point at `main` and may drift; use them as navigation, not permanent citations.

## TS CLI inventory (commands + options) with Rust parity

Legend:
- **done**: command/flag exists with equivalent behavior in `ao-rs`
- **partial**: command exists but flags/behavior differ meaningfully
- **missing**: not present in `ao-rs`
- **intentionally different**: omitted or renamed by design (reason stated)

### Root / global

- **TS**: binary name `ao`; global `--help`, `--version` (Commander defaults).
- **Rust parity**: **intentionally different**
  - **Rust binary**: `ao-rs` (see `docs/cli-reference.md` “Divergences from the TS CLI”).
  - **No global `--config PATH`** in `ao-rs` (TS ecosystem commonly supports explicit config path); `ao-rs` discovers project-local `ao-rs.yaml`.

### `init`

- **TS**: `ao init` (**deprecated**, points to `ao start`)
- **Rust parity**: **intentionally different**
  - Rust uses `ao-rs start` for initialization and does not provide `init`.

### `start`

- **TS**: `ao start [project]`
  - **Options**: `--no-dashboard`, `--no-orchestrator`, `--rebuild`, `--dev`, `--interactive`
  - Source: `packages/cli/src/commands/start.ts`
- **Rust parity**: **partial**
  - Rust: `ao-rs start [--repo] [--run] [--port] [--interval] [--open]`
  - Gaps:
    - Missing `--no-dashboard` / `--no-orchestrator` split (Rust uses `--run` to start dashboard+lifecycle together).
    - Missing `--rebuild`, `--dev`, `--interactive` options.

### `stop`

- **TS**: `ao stop [project]`
  - **Options**: `--purge-session`, `--all`
  - Source: `packages/cli/src/commands/start.ts`
- **Rust parity**: **missing**
  - Closest Rust equivalents: `ao-rs kill <session>` and `ao-rs cleanup` (session-scoped vs supervisor-scoped).

### `status`

- **TS**: `ao status`
  - **Options**: `-p, --project <id>`, `--json`, `-w, --watch`, `--interval <seconds>`
  - Source: `packages/cli/src/commands/status.ts`
- **Rust parity**: **partial**
  - Rust: `ao-rs status [--project] [--pr] [--cost]`
  - Gaps:
    - Missing `--json` output.
    - Missing `--watch` and `--interval` (Rust has `ao-rs watch` but it streams lifecycle events, not a live status table).

### `spawn`

- **TS**: `ao spawn [first] [second]` (legacy args)
  - **Options**: `--open`, `--agent <name>`, `--claim-pr <pr>`, `--assign-on-github`, `--prompt <text>`
  - Source: `packages/cli/src/commands/spawn.ts`
- **Rust parity**: **partial**
  - Rust: `ao-rs spawn (--task|-t | --issue|-i | --local-issue) [--repo] [--default-branch] [--project] [--no-prompt] [--force] [--agent] [--runtime] [--template]`
  - Gaps:
    - Missing `--open` (TS opens newly spawned session(s) / terminal UX; Rust currently focuses on tmux attach flow).
    - Missing `--claim-pr` and `--assign-on-github` (Rust doesn’t currently attach existing PRs to sessions at spawn-time).
    - Missing `--prompt <text>` (Rust builds prompt from task/issue/local-issue + template; no override flag).
  - Differences:
    - Rust adds `--runtime` and `--template`.
    - Rust requires exactly one of `--task`, `--issue`, `--local-issue` (TS supports legacy positional issue/branch flows).

### `batch-spawn`

- **TS**: `ao batch-spawn <issues...>`
  - **Options**: `--open`
  - Source: `packages/cli/src/commands/spawn.ts`
- **Rust parity**: **partial**
  - Rust: `ao-rs batch-spawn <issues...> [--repo] [--default-branch] [--project] [--no-prompt] [--force] [--agent] [--runtime] [--template]`
  - Gap: missing `--open`.

### `session` (umbrella)

- **TS**: `ao session <subcommand>`
  - Source: `packages/cli/src/commands/session.ts`
- **Rust parity**: **partial**
  - Rust has `ao-rs session restore|attach`, and uses top-level commands for others (`kill`, `cleanup`).

#### `session ls`

- **TS**: `ao session ls`
  - **Options**: `-p, --project`, `-a, --all`, `--json`
- **Rust parity**: **partial**
  - Rust equivalent: `ao-rs status [--project]` (no `--json`, no `--all` concept; Rust always lists persisted sessions).

#### `session attach`

- **TS**: `ao session attach <session>`
- **Rust parity**: **done**
  - Rust: `ao-rs session attach <session>`

#### `session kill`

- **TS**: `ao session kill <session> [--purge-session]`
- **Rust parity**: **partial**
  - Rust: `ao-rs kill <session>`
  - Gap: missing `--purge-session` semantics.

#### `session cleanup`

- **TS**: `ao session cleanup [-p <project>] [--dry-run]`
- **Rust parity**: **partial**
  - Rust: `ao-rs cleanup [--project] [--dry-run]`
  - Difference: TS is under `session`; Rust is top-level.

#### `session claim-pr`

- **TS**: `ao session claim-pr <pr> [session] [--assign-on-github]`
- **Rust parity**: **missing**

#### `session restore`

- **TS**: `ao session restore <session>`
- **Rust parity**: **done**
  - Rust: `ao-rs session restore <session>`

#### `session remap`

- **TS**: `ao session remap <session> [-f, --force]`
- **Rust parity**: **missing**

### `send`

- **TS**: `ao send <session> [message...]`
  - **Options**: `-f, --file <path>`, `--no-wait`, `--timeout <seconds>` (default `600`)
  - Source: `packages/cli/src/commands/send.ts`
- **Rust parity**: **partial**
  - Rust: `ao-rs send <session> <message>`
  - Gaps:
    - Missing `--file` (send file content).
    - Missing async/wait controls (`--no-wait`, `--timeout`).
    - TS supports variadic message args; Rust expects a single string.

### `review-check`

- **TS**: `ao review-check [project] [--dry-run]`
  - Source: `packages/cli/src/commands/review-check.ts`
- **Rust parity**: **partial**
  - Rust: `ao-rs review-check [--project] [--dry-run]`
  - Difference: TS accepts optional positional `[project]`; Rust uses `--project`.

### `dashboard`

- **TS**: `ao dashboard`
  - **Options**: `-p, --port <port>`, `--no-open`, `--rebuild`
  - Source: `packages/cli/src/commands/dashboard.ts`
- **Rust parity**: **partial**
  - Rust: `ao-rs dashboard [--port] [--interval] [--open]`
  - Gaps:
    - Missing `--rebuild`.
    - TS default is open; Rust default is closed unless `--open` (inverse polarity).

### `open`

- **TS**: `ao open [target] [-w, --new-window]`
  - Source: `packages/cli/src/commands/open.ts`
- **Rust parity**: **missing**

### `verify`

- **TS**: `ao verify [issue]`
  - **Options**: `-p, --project <id>`, `--fail`, `-c, --comment <msg>`, `-l, --list`
  - Source: `packages/cli/src/commands/verify.ts`
- **Rust parity**: **missing**

### `doctor`

- **TS**: `ao doctor [--fix] [--test-notify]`
  - Source: `packages/cli/src/commands/doctor.ts`
- **Rust parity**: **partial**
  - Rust: `ao-rs doctor` (no flags)
  - Gaps: missing `--fix`, missing `--test-notify`.

### `update`

- **TS**: `ao update [--skip-smoke] [--smoke-only] [--check]`
  - Source: `packages/cli/src/commands/update.ts`
- **Rust parity**: **missing**

### `setup` (umbrella)

- **TS**: `ao setup <subcommand>`
  - Source: `packages/cli/src/commands/setup.ts`
- **Rust parity**: **missing**

#### `setup openclaw`

- **TS**: `ao setup openclaw [--url] [--token] [--routing-preset <urgent-only|urgent-action|all>] [--non-interactive]`
- **Rust parity**: **missing**

### `plugin` (umbrella)

- **TS**: `ao plugin <subcommand>` for marketplace-managed plugins
  - Source: `packages/cli/src/commands/plugin.ts`
- **Rust parity**: **intentionally different**
  - Rust uses **workspace crates** for plugins rather than a marketplace installer; no CLI surface yet for listing/installing/updating.

### `config-help`

- **TS**: `ao config-help` prints a config guide for `agent-orchestrator.yaml`
  - Source: `packages/cli/src/lib/config-instruction.ts`
- **Rust parity**: **missing** (doc-only alternative exists)
  - Rust has `docs/config.md` and config validation warnings; no dedicated `ao-rs config-help` command.

## Config behavior differences (CLI-relevant)

### File names + discovery

- **TS**: `agent-orchestrator.yaml` (often project-local; documented by `config-help`)
- **Rust**: `ao-rs.yaml` discovered by walking up from cwd (see `docs/config.md`)

### CLI options that depend on config

- **Agent/runtime defaults**
  - **TS**: defaults in `agent-orchestrator.yaml` (e.g. `defaults.*`, per-project overrides)
  - **Rust**: supports a subset of TS-style defaults and per-project config (see `docs/config.md`)
- **Plugins + notifiers**
  - **TS**: plugin marketplace + notifier routing are first-class in CLI (`plugin`, `setup`, `doctor --test-notify`)
  - **Rust**: notifier env vars exist (`AO_NTFY_*`, slack/discord webhooks), but plugin management is not exposed as a CLI surface

## Summary of parity gaps (actionable backlog)

High-signal gaps to port or explicitly document as intentional differences:

- **Missing commands**
  - `stop`, `open`, `verify`, `update`, `setup ...`, `plugin ...`, `config-help`
- **Partial commands worth tightening**
  - `start` (missing `--rebuild/--dev/--interactive` and dashboard/orchestrator toggles)
  - `status` (missing `--json`, `--watch`, `--interval`)
  - `send` (missing `--file`, `--timeout`, `--no-wait`)
  - `doctor` (missing `--fix`, `--test-notify`)
  - `session kill` parity (`--purge-session`)

If/when these are implemented, update this doc by flipping the parity tags and linking the PR that changed them.

