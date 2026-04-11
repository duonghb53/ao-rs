# CLI reference

Everything `ao-rs` knows how to do today, plus what Slice 2 will add.
Source of truth: `crates/ao-cli/src/main.rs`.

## Global

```
ao-rs <subcommand> [...]
```

Logging honors `RUST_LOG`; default is `warn,ao_core=info` — bump to
`RUST_LOG=ao_core=debug` while debugging the lifecycle loop.

## `ao-rs spawn` — create a new session

```
ao-rs spawn --task "<task>" [--repo PATH] [--default-branch BRANCH]
            [--project NAME] [--no-prompt]
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
| `--task` / `-t` | *required* | First message sent to the agent. |
| `--repo` | `cwd` | Path to the git repo to branch from. |
| `--default-branch` | `main` | Branch the worktree is created off of. |
| `--project` | `demo` | Namespaces sessions + worktrees on disk. |
| `--no-prompt` | off | Skip the initial `send_message` call. Handy when `claude` isn't installed — you still get a bootstrapped session you can attach to. |

## `ao-rs status` — list persisted sessions

```
ao-rs status [--project NAME] [--pr]
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

Off by default because it shells out to `gh` up to three times per session
(`detect_pr`, `pr_state`, `ci_status`) — only pay the latency when you want
it. One bad row never fails the whole table.

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

## Planned subcommands (Slice 2+)

These are **not implemented**. Tracking them here so the CLI has a
roadmap.

| Command | Slice | Purpose |
| --- | --- | --- |
| `ao-rs kill <id>` | 2 | `Runtime::destroy` + set status `killed`. Clean shutdown without losing the worktree. |
| `ao-rs merge <id>` | 2 | Call `Scm::merge` — usually fired by the reaction engine, but manual trigger is useful. |
| `ao-rs cleanup <id>` | 2 | Remove worktree + archive session file. Today you run `git worktree remove` by hand. |
| `ao-rs config show` | 2 | Dump the merged reaction config (global + project overrides). |
| `ao-rs daemon start/stop` | 3 | Long-running supervisor — same loop as `watch` but without a terminal attached. |

Everything past Slice 2 is speculative. See `reactions.md` for the
reaction-engine design that makes most of these useful.

## Divergences from the TS CLI

| TS | ao-rs | Why |
| --- | --- | --- |
| `ao` binary | `ao-rs` | Avoids shadowing a real install while you experiment. |
| `ao init` that writes a yaml config | (none) | No config file until Slice 2 needs one. |
| `ao plugins list` / `install` | (none) | Plugins are workspace members, not a registry. |
| `ao sessions list` | `ao-rs status` | Shorter name; single-verb style. |
| Interactive TUI picker | (none) | Out of scope for the port. |
| `--config PATH` global flag | (none) | There is no config file yet. |

When Slice 2 lands a config file the `--config` flag will come with it.
