# CLI reference

All implemented `ao-rs` subcommands (kept in sync with clap help).
Source of truth: `crates/ao-cli/src/main.rs`.

## Global

```
ao-rs <subcommand> [...]
```

Logging honors `RUST_LOG`; default is `warn,ao_core=info` — bump to
`RUST_LOG=ao_core=debug` while debugging the lifecycle loop.

## `ao-rs start` — initialize config

```
ao-rs start [--repo PATH] [--run] [--port N] [--interval SECS] [--open]
```

Creates or loads a project-local `ao-rs.yaml` config file.

- If the config file exists, prints a short summary and exits.
- Otherwise, auto-detects the current git repo (or `--repo`) and generates a default config.

Also installs ai-devkit skills (best-effort).

When `--run` is set, `start` will also launch the dashboard + lifecycle loop
(equivalent to running `ao-rs dashboard` after `ao-rs start`). `--open` opens
the dashboard URL in your default browser (requires `--run`).

## `ao-rs spawn` — create a new session

```
ao-rs spawn (--task "<task>" | --issue <N|#N> | --local-issue PATH)
            [--repo PATH] [--default-branch BRANCH] [--project NAME]
            [--no-prompt] [--force]
            [--agent <claude-code|cursor|aider|codex>]
            [--runtime <tmux|process>]
            [--template <bugfix|feature|refactor|docs|test>]
```

Wires up, in order:

1. Resolve the repo path (default: `cwd`). Refuses to run outside a git repo.
2. Mint a fresh `SessionId` + 8-char short id + branch `ao-<shortid>`.
3. `WorktreeWorkspace::create` carves a git worktree at
   `~/.worktrees/<project>/<shortid>/`.
4. Persist the `Session` to `~/.ao-rs/sessions/<project>/<uuid>.yaml` with
   `status: spawning`.
5. Ask the agent for `launch_command`, `environment`, `initial_prompt`.
6. `TmuxRuntime::create` spawns a tmux session running the launch command.
7. Flip status to `working` and save again.
8. Sleep 1.5 s, then `Runtime::send_message(initial_prompt)` (unless
   `--no-prompt`).
9. Print attach/kill/cleanup instructions.

Flags:

| Flag | Default | Notes |
| --- | --- | --- |
| `--task` / `-t` | *one-of required* | Free-form task (first message sent to the agent). Conflicts with `--issue` and `--local-issue`. |
| `--issue` / `-i` | *one-of required* | GitHub issue number (`42` or `#42`). Fetches issue title/body via the tracker and uses them as context for the prompt builder. |
| `--local-issue` | *one-of required* | Local markdown issue file (`docs/issues/NNNN-slug.md`, created via `ao-rs issue new`). |
| `--repo` | `cwd` | Path to the git repo to branch from. |
| `--default-branch` | `main` | Branch the worktree is created off of. |
| `--project` | repo directory name | Namespaces sessions + worktrees on disk. |
| `--no-prompt` | off | Skip the initial `send_message` call. Handy when `claude` isn't installed — you still get a bootstrapped session you can attach to. |
| `--force` | off | Allow duplicate spawns for the same issue/local-issue id (normally rejected). |
| `--agent` | config default (fallback `claude-code`) | Agent plugin to use. Supported: `claude-code`, `cursor`, `aider`, `codex`. |
| `--runtime` | config default (fallback `tmux`) | Runtime plugin to use. Supported: `tmux`, `process`. |
| `--template` | none | Append a built-in template to the initial prompt. Built-ins: `bugfix`, `feature`, `refactor`, `docs`, `test`. |

Notes:

- For issue/local-issue spawns, the branch name is derived from the tracker/filename and prefixed with `ao-<shortid>-...` to avoid collisions.

## `ao-rs status` — list persisted sessions

```
ao-rs status [--project NAME] [--pr] [--cost]
```

Does a fresh `read_dir` of `~/.ao-rs/sessions/` — there's no in-memory
cache (see `architecture.md` principle #2). Prints one row per session
sorted newest first:

```
ID         PROJECT        STATUS             ACTIVITY       BRANCH             TASK
3a4b5c6d   demo           working            ready          ao-3a4b5c6d        fix the tests
```

`--project` filters to a single project directory — useful at N>10 and
nothing else.

`--cost` adds a cost column showing token counts and USD estimate from the
Claude Code JSONL logs. Reads `~/.ao-rs/sessions/<project>/<uuid>.yaml`
which caches the last polled value. Shows `-` when cost data is not yet
available (e.g. the agent hasn't written any JSONL yet).

`--pr` adds a compact PR column populated by the GitHub SCM plugin.
Example row:

```
ID         PROJECT        STATUS             ACTIVITY       BRANCH             PR                       TASK
3a4b5c6d   demo           working            ready          ao-3a4b5c6d        #42 open/passing         fix the tests
```

Cell shapes:

- `-` — no PR (or no github origin, or `gh pr list` errored).
- `#42 open/passing`, `#42 open/failing`, `#42 open/pending` — normal open PR.
- `#42 merged` / `#42 closed` — merged/closed PRs drop the CI suffix because
  GitHub stops serving check data for them.
- `#42 ?/?`, `#42 open/?`, `#42 ?/passing` — `detect_pr` succeeded but a
  follow-up call flaked; the `?` marks which half is unknown so the row
  still carries information. Distinct from `-` (which means "no PR at all").

Off by default because it fans out to multiple concurrent `gh` subprocesses
per session (`detect_pr`, `pr_state`, `ci_status`, `review_decision`,
`mergeability`) via `tokio::join!` — only pay the latency when you want it.
One bad row never fails the whole table.

## `ao-rs watch` — run the lifecycle loop

```
ao-rs watch [--interval SECS]
```

Starts `LifecycleManager` against the real tmux runtime + claude-code
agent and streams events to stdout. Ctrl-C cancels cleanly (cancels the
tokio task, drains the event channel, persists any in-flight transition).

Guarded by a pidfile at `~/.ao-rs/lifecycle.pid` — a second `ao-rs watch`
while one is already running will print the holding PID and exit, rather
than racing two polling loops over the same sessions. The `PidFile` is
RAII; the lock is released when the function returns.

Output is a streaming table:

```
SESSION    EVENT                DETAIL
3a4b5c6d   spawned              project=demo
3a4b5c6d   status_changed       spawning → working
3a4b5c6d   activity_changed     - → ready
3a4b5c6d   terminated           runtime_gone
```

| Flag | Default | Notes |
| --- | --- | --- |
| `--interval` | `5` (sec) | Polling period. Matches the TS reference's default. Faster polls cost tmux pipe-pane probes; slower polls delay status transitions. |

See `state-machine.md` for which transitions fire today.

## `ao-rs dashboard` — REST API + SSE server

```
ao-rs dashboard [--port PORT] [--interval SECS] [--open]
```

Starts an axum HTTP server exposing REST endpoints and a Server-Sent Events stream.

- `--open` opens the dashboard root URL in the default browser.

## `ao-rs send <session> <message>` — nudge a running agent

```
ao-rs send <session> "<message>"
```

Resolve the session by uuid or short-id prefix, probe the runtime for
liveness, then call `Runtime::send_message(handle, msg)`. Thin wrapper —
the interesting bits are the error messages:

- Unknown/ambiguous session → `find_by_prefix` error from `SessionManager`.
- Session has no runtime handle → "nothing to send to" with the stored
  status, because that's almost always a terminated/pending session the
  user forgot about.
- `Runtime::is_alive` returned false → "runtime handle is not alive. try:
  `ao-rs session restore <short>`". Saves users from staring at a raw
  `tmux send-keys: no such session` message.

No confirmation prompt, no echo of the prior conversation — the CLI's job
is to deliver the bytes and get out of the way. Use `tmux attach` if you
want to see the agent's reaction.

## `ao-rs pr <session>` — summarize the GitHub PR for a session

```
ao-rs pr <session>
```

Uses the GitHub SCM plugin to derive `(owner, repo)` from the session's
workspace `origin` remote, then fans out `pr_state`, `ci_status`,
`review_decision`, `mergeability` concurrently via `tokio::join!` after
`detect_pr` resolves. `mergeability` internally re-invokes `pr_state` +
`ci_status` + one extra `gh pr view` probe, so a single `ao-rs pr` is
~7 gh subprocesses total (1 for detect, 4 parallel, 2 duplicated inside
`mergeability`). Accepted duplication — keeping the `Scm` trait
self-contained is worth more than shaving two subprocesses off a manual
debug command.

Output is a fixed block matching the `spawn`/`restore` frame style:

```
───────────────────────────────────────────────
  session: 3a4b5c6d-…-…-…-… (short 3a4b5c6d)
  branch:  ao-3a4b5c6d
  PR:      #42 fix the widgets
  url:     https://github.com/acme/widgets/pull/42

  state:   open
  CI:      passing
  review:  approved

  mergeable: yes
───────────────────────────────────────────────
```

When the PR isn't mergeable, a `blockers:` list appears after `mergeable:
no`, one `- reason` per line, pulled directly from
`MergeReadiness::blockers`. When there's no PR at all (session never
pushed, no github origin) the command exits 0 with
`no PR found for session <uuid> (branch <name>)` — `ao-rs pr` is a
query, not a trigger.

## `ao-rs session restore <id>` — respawn a terminated session

```
ao-rs session restore <session>
```

Looks up a session by full uuid or any unambiguous prefix (the 8-char
short id works), verifies its worktree still exists, and respawns the
runtime. Mirrors `restore()` in `packages/core/src/session-manager.ts`.

Steps:

1. Load all sessions and resolve `<session>` to exactly one match — errors
   on no match or ambiguous match.
2. Enrich state: if `Runtime::is_alive(old_handle) == false` and the
   stored status isn't terminal, flip to `terminated` in-memory. Without
   this, a crashed `working` session wouldn't pass the restorable gate.
3. `SessionStatus::is_restorable` gate — rejects `merged` and `cleanup`.
4. Verify `workspace_path` exists on disk. No `Workspace::restore` hook
   yet, so a deleted worktree is terminal.
5. Best-effort `Runtime::destroy(old_handle)` to clear any stale tmux
   session that may still be around.
6. `Runtime::create(...)` reusing the old short id when possible, so
   users' `tmux attach -t <short>` muscle memory still works.
7. Persist `status: spawning`, `activity: None`, new runtime handle.

Prints the same attach/status hint block as `ao-rs spawn`. The next
`ao-rs watch` tick flips `spawning → working` once the agent reports
`Active` or `Ready`.

## `ao-rs session attach <id>` — attach to tmux

```
ao-rs session attach <session>
```

Execs `tmux attach-session -t <handle>` for the resolved session, replacing the current process.

## `ao-rs kill <id>` — stop runtime + remove worktree + archive

```
ao-rs kill <session>
```

Best-effort: safe to run even if the tmux session or worktree is already gone.

## `ao-rs cleanup` — archive terminal sessions

```
ao-rs cleanup [--project NAME] [--dry-run]
```

Scans terminal sessions (killed, terminated, errored, merged, etc.), removes any remaining worktrees, and moves session YAML files into `.archive/`.

## `ao-rs doctor` — verify environment

```
ao-rs doctor
```

Checks required tools on PATH (`git`, `gh`, `tmux`, `claude`), GitHub auth (`gh auth status`), config loadability + validation (unknown/unsupported fields are warned and ignored), and sessions dir presence.

## `ao-rs review-check` — forward new PR comments to agents

```
ao-rs review-check [--project NAME] [--dry-run]
```

For each active session with a PR, fetches unresolved review comments via the SCM plugin and (when new vs the last run) messages the agent with a summary.

## `ao-rs issue` — local markdown issues (non-GitHub workflows)

Creates markdown files under `docs/issues/` inside the repo.

```
ao-rs issue new --title "..." [--body "..."] [--repo PATH]
ao-rs issue list [--repo PATH]
ao-rs issue show <PATH|NNNN> [--repo PATH]
```

`issue show` accepts either a path or a numeric id (`1`, `01`, `0001`) matching `docs/issues/NNNN-*.md`.

## Environment variables

| Variable | Purpose |
| --- | --- |
| `RUST_LOG` | Log level. Default: `warn,ao_core=info`. Set `ao_core=debug` to trace the lifecycle loop. |
| `AO_NTFY_TOPIC` | [ntfy.sh](https://ntfy.sh) topic for push notifications. Required to activate the ntfy notifier. |
| `AO_NTFY_URL` | Custom ntfy server URL. Default: `https://ntfy.sh`. |
| `AO_DISCORD_WEBHOOK_URL` | Discord webhook URL for the discord notifier. |
| `AO_SLACK_WEBHOOK_URL` | Slack incoming webhook URL for the slack notifier. |

## Roadmap (not implemented)

These commands are **not implemented** today.

| Command | Purpose |
| --- | --- |
| `ao-rs merge <id>` | Manual trigger for `Scm::merge` (auto-merge exists via reactions; manual is still useful). |
| `ao-rs config show` | Dump the merged config + notification routing. |
| `ao-rs config validate` | Validate config for typos/unknown keys/missing notifiers. |
| `ao-rs logs <id>` | Tail the agent terminal output for a session. |

## Divergences from the TS CLI

| TS | ao-rs | Why |
| --- | --- | --- |
| `ao` binary | `ao-rs` | Avoids shadowing a real install while you experiment. |
| `ao init` that writes a yaml config | `ao-rs start` | Generates `ao-rs.yaml` in the project directory. |
| `ao plugins list` / `install` | (none) | Plugins are workspace members, not a registry. |
| `ao sessions list` | `ao-rs status` | Shorter name; single-verb style. |
| `ao start` launches dashboard + orchestrator | `ao-rs watch` | No dashboard; the lifecycle loop is the whole supervisor. |
| `ao doctor` / `ao update` | (none) | Not needed for a learning project. |
| Interactive TUI picker | (none) | Out of scope for the port. |
| `--config PATH` global flag | (none) | Always reads `ao-rs.yaml` from the current directory. |
