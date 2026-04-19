# ao-rs User Guide

ao-rs lets AI coding agents (Claude Code, Cursor, Aider, Codex) work autonomously on GitHub issues inside isolated git worktrees. You spawn sessions, the lifecycle loop watches them, and reactions handle CI failures, review comments, and auto-merges without manual intervention.

---

## 1. Prerequisites & Installation

**Required tools — all must be on `PATH`:**

| Tool | Why |
|------|-----|
| `git` | Worktree creation and branch management |
| `tmux` | Default runtime for agent sessions |
| `gh` | GitHub API — PRs, CI status, reviews (must be authenticated: `gh auth login`) |
| `claude` | Claude Code agent (skip if using Cursor/Aider/Codex) |

**Install ao-rs from source:**

```bash
git clone https://github.com/duonghb53/ao-rs
cd ao-rs
cargo install --path crates/ao-cli
```

Requires Rust 1.89+. Install via [rustup.rs](https://rustup.rs) if needed.

**Verify the environment:**

```bash
ao-rs doctor
```

`doctor` checks PATH tools, GitHub auth, and config validity. Fix any `FAIL` lines before going further.

---

## 2. Core Concepts (5 min read)

### Sessions, worktrees, the lifecycle loop

A **session** is the fundamental unit in ao-rs. When you spawn a session:

1. A fresh git worktree is carved at `~/.worktrees/<project>/<shortid>/`
2. A tmux session is started there, running the agent
3. A YAML file is persisted at `~/.ao-rs/sessions/<project>/<uuid>.yaml`

Sessions are independent — each has its own branch (`ao-<shortid>-...`), its own worktree, and its own agent process. You can run dozens in parallel without them interfering.

The **lifecycle loop** (`ao-rs watch`) polls every 5 seconds:

- Is the tmux session alive?
- What is the agent doing (active / ready / idle)?
- Has a PR been opened? What is CI doing? Has it been reviewed?

These observations drive **status transitions** — a session moves through `spawning → working → pr_open → ci_failed → approved → mergeable → merged`. See [state-machine.md](state-machine.md) for the full 18-state diagram.

### Orchestrators vs workers

A **worker** session works on a single task. It opens a PR, handles CI and review feedback, and merges.

An **orchestrator** session is a long-lived agent that manages worker sessions. It reads the issue backlog, calls `ao-rs spawn` to create workers, and monitors their progress via `ao-rs status`. Orchestrators are useful when you want the AI to decide what to work on next rather than you deciding manually.

### Reactions

When a transition fires, the lifecycle loop checks whether a **reaction** is configured for that event. Reactions are the automation layer:

- CI failed → send CI log summary to the agent
- Changes requested → send review comments to the agent
- PR approved + green → auto-merge
- Agent stuck for 10 minutes → notify you

Reactions run automatically. You configure them in `ao-rs.yaml` and then stay out of the way until something needs human judgment.

---

## 3. First Run Walkthrough

### Initialize the project

```bash
cd /path/to/your/project
ao-rs start
```

`start` detects the current git repo, generates `ao-rs.yaml` with sensible defaults, and installs ai-devkit skills. If the file already exists it prints a summary and exits — safe to re-run.

Pass `--run` to also start the dashboard immediately after init:

```bash
ao-rs start --run --open
```

### Spawn your first session

```bash
ao-rs spawn --issue 42
```

ao-rs fetches issue #42 from GitHub, builds a structured prompt for the agent, creates the worktree, and starts the agent. Output looks like:

```
─────────────────────────────────────────────────
  session: 3a4b5c6d-…  (short: 3a4b5c6d)
  branch:  fix/42-the-widget-bug
  task:    Fix the widget bug (#42)

  attach:   tmux attach-session -t 3a4b5c6d
  status:   ao-rs status
  kill:     ao-rs kill 3a4b5c6d
─────────────────────────────────────────────────
```

### Watch the lifecycle loop

In a separate terminal:

```bash
ao-rs watch
```

This streams a live event table:

```
SESSION    EVENT                DETAIL
3a4b5c6d   spawned              project=my-repo
3a4b5c6d   status_changed       spawning → working
3a4b5c6d   activity_changed     - → active
3a4b5c6d   status_changed       working → pr_open
```

The loop runs until you press Ctrl-C. Only one `watch` can run at a time — a second invocation prints the PID of the current owner and exits.

For a web dashboard instead:

```bash
ao-rs dashboard --open
```

This starts the axum API server at `http://localhost:3000` (configurable with `--port`) alongside the lifecycle loop, then opens it in your browser.

### Check status

```bash
ao-rs status
ao-rs status --pr --cost
```

With `--pr`, each row gains a PR column (`#42 open/passing`). With `--cost`, each row shows token count and USD estimate. Both flags are off by default because they make external calls per session.

---

## 4. Spawning Sessions

### From a GitHub issue

```bash
ao-rs spawn --issue 42
ao-rs spawn --issue "#42"    # hash prefix also accepted
```

ao-rs fetches the issue title and body via `gh`, derives the branch name from labels and title, and builds the initial agent prompt from that context.

### From a free-form task

```bash
ao-rs spawn --task "refactor the auth module to use JWTs"
```

Use this when there is no GitHub issue, or when the task is exploratory. The task string becomes the first message sent to the agent.

### From a local issue file

For non-GitHub workflows, create a markdown issue first:

```bash
ao-rs issue new --title "Improve error messages in the parser"
# creates docs/issues/0001-improve-error-messages-in-the-parser.md

ao-rs spawn --local-issue docs/issues/0001-improve-error-messages-in-the-parser.md
```

Local issues are stored in the repo under `docs/issues/`. Use `ao-rs issue list` and `ao-rs issue show <N>` to browse them.

### Batch spawn

Spawn multiple sessions from a list of issue numbers in one command:

```bash
ao-rs batch-spawn --issues 42,43,44 --project my-repo
```

Each issue gets its own session, worktree, and agent process. Useful for parallelizing a sprint milestone.

### Key flags

| Flag | What it does |
|------|-------------|
| `--agent` | Override the agent: `claude-code`, `cursor`, `aider`, `codex` |
| `--template` | Append a built-in prompt template: `bugfix`, `feature`, `refactor`, `docs`, `test` |
| `--no-prompt` | Create the session but skip sending the initial message (attach manually later) |
| `--force` | Allow spawning a second session for the same issue (normally rejected as a duplicate) |
| `--default-branch` | Branch to create the worktree from (default: `main`) |
| `--project` | Override the project name used to namespace sessions and worktrees on disk |
| `--open` | Immediately attach to the new tmux session after spawning |

See [cli-reference.md](cli-reference.md) for the full flag table.

---

## 5. Monitoring Sessions

### `ao-rs status`

```bash
ao-rs status                        # basic table, terminal sessions hidden
ao-rs status --all                  # include killed/terminated
ao-rs status --pr                   # add PR + CI column
ao-rs status --cost                 # add token count + USD column
ao-rs status --pr --cost            # both
ao-rs status --project my-repo      # filter to one project
ao-rs status --json                 # machine-readable JSON array
ao-rs status --watch                # re-print every 2s (Ctrl-C to stop)
ao-rs status --watch --interval 5   # re-print every 5s
```

PR column cell shapes:

| Cell | Meaning |
|------|---------|
| `-` | No PR found (or no GitHub origin) |
| `#42 open/passing` | Open PR, CI passing |
| `#42 open/failing` | Open PR, CI failing |
| `#42 open/pending` | Open PR, CI still running |
| `#42 merged` | PR merged |
| `#42 closed` | PR closed without merge |
| `#42 ?/passing` | PR state unknown but CI is known |

Cost shows `-` until Claude Code has written its first JSONL output. Once available, the value is cached in the session YAML and updated each poll.

### `ao-rs watch`

Streams lifecycle events to stdout. One event per line. Useful for a dedicated monitoring terminal or for piping into a log file:

```bash
ao-rs watch --interval 10   # poll every 10s (default: 5s)
ao-rs watch 2>&1 | tee lifecycle.log
```

Only one watch loop can run at a time (pidfile at `~/.ao-rs/lifecycle.pid`).

### `ao-rs dashboard`

```bash
ao-rs dashboard                   # start on port 3000 (from config)
ao-rs dashboard --port 4000       # custom port
ao-rs dashboard --open            # open browser automatically
ao-rs dashboard --interval 10     # slower poll for quieter machines
```

The dashboard provides a React UI with lanes (Working / Pending / Review / Merged) that update via SSE, plus the REST API:

```bash
curl http://localhost:3000/api/sessions | jq
curl -N http://localhost:3000/api/events       # SSE stream
```

### `ao-rs pr <id>`

Fetch a detailed PR summary for one session:

```bash
ao-rs pr 3a4b5c6d
```

Output:

```
───────────────────────────────────────────────
  session: 3a4b5c6d-…
  branch:  fix/42-the-widget-bug
  PR:      #42 Fix the widget bug
  url:     https://github.com/acme/widgets/pull/42

  state:   open
  CI:      passing
  review:  approved

  mergeable: yes
───────────────────────────────────────────────
```

When `mergeable: no`, a `blockers:` list appears explaining what is blocking the merge.

### `ao-rs review-check`

```bash
ao-rs review-check                    # check all projects
ao-rs review-check --project my-repo  # one project only
ao-rs review-check --dry-run          # print what would be sent, don't send
```

Fetches unresolved review comments from GitHub and forwards any new ones to the agent. Run this manually if you want immediate forwarding outside the normal watch cycle, or schedule it via cron.

---

## 6. Interacting with Running Sessions

### Send a message

```bash
ao-rs send 3a4b5c6d "the API changed, use v2 endpoints instead"
ao-rs send 3a4b5c6d please focus on the auth module only
```

Multiple words are joined with spaces — quotes are optional for simple messages.

Send the contents of a file (useful for long briefs or context dumps):

```bash
ao-rs send 3a4b5c6d --file docs/architecture-brief.md
ao-rs send 3a4b5c6d "context follows:" --file context.md
```

When `--file` is combined with inline words, the file content is appended after a newline.

The session must have a live runtime. If it doesn't:

```bash
ao-rs session restore 3a4b5c6d   # respawn the tmux session first
ao-rs send 3a4b5c6d "resume the task"
```

### Attach to the terminal

```bash
ao-rs session attach 3a4b5c6d
```

This execs `tmux attach-session` for the resolved session, replacing your current process. You can watch the agent type in real time, or type a message directly. Detach with `Ctrl-B D`.

Alternatively, the short id works directly with tmux:

```bash
tmux attach-session -t 3a4b5c6d
```

### Restore a terminated session

When a session shows `terminated` status (tmux session exited), restore it without losing the worktree or branch:

```bash
ao-rs session restore 3a4b5c6d
```

This respawns the agent in the same worktree under the same short id. The next `ao-rs watch` tick flips `spawning → working` once the agent is active.

Sessions in `merged` or `cleanup` status cannot be restored — the work is done.

---

## 7. Orchestrators

### What is an orchestrator

An orchestrator is a session whose job is to manage other sessions. Instead of working on a single task, the orchestrator reads the issue backlog, decides what to work on, calls `ao-rs spawn` to create worker sessions, and monitors their status in a loop.

You use an orchestrator when:
- You want the AI to decide task prioritization, not you
- You are running a large batch (a milestone or sprint) and want autonomous handoff

### Spawn an orchestrator

```bash
# From the project directory
ao-rs orchestrator spawn

# Override the agent or port
ao-rs orchestrator spawn --agent claude-code --port 3000

# Skip the auto-generated system prompt
ao-rs orchestrator spawn --no-prompt
```

The orchestrator is created in its own worktree (`orchestrator/<session-id>`), receives a system prompt that explains its role and the available `ao-rs` commands, and starts working. The `orchestrator_rules` key in `ao-rs.yaml` lets you inject custom instructions into that prompt.

A real `ao-rs.yaml` orchestrator config:

```yaml
defaults:
  orchestrator:
    agent: claude-code
    agent_config:
      permissions: permissionless
      model: opus                 # one orchestrator model for every project
  worker:
    agent: claude-code
    agent_config:
      model: sonnet               # one worker model for every project
  orchestrator_rules: |-
    After spawning a worker, do NOT stop. Run a monitoring loop:
    1. Immediately confirm spawn with: ao-rs status
    2. Every 5 minutes, check: ao-rs status --project <id>
    3. When worker reaches pr_open/review_pending/merged/ci_failed → act
    4. Only stop monitoring when all workers reach terminal state
```

### Choosing the model (orchestrator vs worker)

Orchestrators and workers can run on different models. The orchestrator is
usually worth a stronger model (e.g. `opus`) because it makes judgment calls
about what to spawn next. Workers can run on a cheaper/faster model (e.g.
`sonnet`) since most of their work is mechanical.

**Set it in one place** — `defaults.orchestrator.agent_config.model` applies
to every project's orchestrator. Same for `defaults.worker.agent_config.model`.
A project can still override either with its own `agent_config.model` or
`orchestrator.agent_config.model`.

See [config.md → Model selection](config.md#model-selection-orchestrator-vs-worker)
for the full fallback chain and per-project override recipes.

### How orchestrators manage workers

Workers spawned by an orchestrator pass `--spawned-by <orchestrator-session-id>` internally. This links the sessions so lifecycle events on the worker (PR opened, CI failed, merged) are delivered as messages to the orchestrator's agent. The orchestrator can then react — reassign, nudge, or spawn a replacement — without waiting for you.

### List orchestrators

```bash
ao-rs orchestrator list
```

Filters the session list to orchestrator sessions only. Orchestrators appear in the dashboard left sidebar, grouped above their worker sessions.

---

## 8. Reactions & Automation

Reactions are the automation backbone. Each reaction key maps to a trigger event; when that event fires, the reaction engine runs the configured action.

### Built-in reaction keys

| Key | Trigger |
|-----|---------|
| `ci-failed` | CI on the PR failed |
| `changes-requested` | A reviewer requested changes |
| `approved-and-green` | PR approved and CI passing (mergeable) |
| `merge-conflicts` | PR cannot merge cleanly |
| `agent-stuck` | Agent made no progress for `threshold` duration |
| `agent-idle` | Agent has been idle past the threshold |
| `agent-needs-input` | Agent hit a permission prompt |
| `agent-exited` | Runtime process exited |
| `all-complete` | All sessions reached terminal state |

### Configuring reactions

Each reaction key accepts:

```yaml
reactions:
  ci-failed:
    auto: true              # master on/off switch
    action: send-to-agent   # send-to-agent | notify | auto-merge
    message: "CI failed. Read the logs, fix the issue, and push again."
    retries: 3              # how many times to try before escalating
    escalate_after: 3       # escalate after N failed attempts (count form)
    # or:
    escalate_after: 30m     # escalate after 30 minutes (duration form)
    priority: warning       # urgent | action | warning | info

  agent-stuck:
    auto: true
    action: notify
    threshold: 10m          # how long idle before "stuck" triggers
    priority: warning

  approved-and-green:
    auto: true
    action: auto-merge
    merge_method: squash    # merge (default) | squash | rebase
```

### `escalate_after`: count vs duration

Both forms are supported and can be used independently:

- `escalate_after: 3` — escalate after 3 failed `send-to-agent` attempts
- `escalate_after: 30m` — escalate after the reaction has been firing for 30 minutes, regardless of attempt count

Duration format: `\d+(s|m|h)` — e.g. `"10s"`, `"5m"`, `"2h"`.

### `notification_routing` priorities

When a reaction escalates, or when `action: notify` fires, ao-rs routes the notification by priority level:

```yaml
notification_routing:
  urgent:  [stdout, ntfy, desktop, discord]
  action:  [stdout, ntfy]
  warning: [stdout, desktop]
  info:    [stdout]
```

Default priorities when `priority:` is omitted in the reaction config:

| Reaction | Default priority |
|----------|-----------------|
| `ci-failed`, `merge-conflicts` | `warning` |
| `changes-requested`, `agent-idle`, `all-complete` | `info` |
| `approved-and-green` | `action` |
| `agent-stuck`, `agent-needs-input`, `agent-exited` | `urgent` |

### Per-project reaction overrides

Global reactions apply to all projects. Override them for a specific project:

```yaml
projects:
  my-critical-repo:
    repo: acme/my-critical-repo
    path: /Users/me/work/my-critical-repo
    reactions:
      approved-and-green:
        auto: true
        action: auto-merge
        merge_method: squash
```

Per-project reactions merge onto the global config — you only need to specify the keys you want to change.

---

## 9. Notifications Setup

### ntfy.sh (push to phone or desktop)

```bash
export AO_NTFY_TOPIC=my-ao-alerts
# optional: point at a self-hosted server
export AO_NTFY_URL=https://ntfy.example.com
# optional: auth token for private topics
export AO_NTFY_TOKEN=tk_...
```

Add `ntfy` to the relevant priority routes in `notification_routing`. Subscribe on your phone via the ntfy app or at `https://ntfy.sh/my-ao-alerts`.

For HTTP Basic auth on private servers (if not using a token):

```bash
export AO_NTFY_USERNAME=alice
export AO_NTFY_PASSWORD=secret
```

### Discord

```bash
export AO_DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/...
```

Add `discord` to `notification_routing.urgent` (or whichever level you want Discord for).

### Desktop notifications

Add `desktop` to `notification_routing`. No extra configuration needed — ao-rs uses your OS notification system directly.

### Slack

```bash
export AO_SLACK_WEBHOOK_URL=https://hooks.slack.com/services/...
```

Add `slack` to the relevant routing levels.

Put environment variables in your shell profile (`~/.zshrc`, `~/.bashrc`) or in a `.env` file sourced before running ao-rs. ao-rs does not read `.env` files automatically.

---

## 10. Cleanup & Maintenance

### Kill a session

```bash
ao-rs kill 3a4b5c6d
```

Stops the tmux session, removes the worktree, and archives the session YAML. Safe to run even if the tmux session or worktree is already gone.

### Free disk space with prune

```bash
ao-rs prune                         # remove worktrees for all terminal sessions
ao-rs prune --project my-repo       # one project only
ao-rs prune --dry-run               # preview what would be removed
```

Removes the git worktree checkout (`~/.worktrees/<project>/<shortid>/`) for each terminal session. Sessions remain visible in the dashboard — only the disk checkout goes away.

Run this after a batch of sessions merge:

```bash
ao-rs prune --dry-run
ao-rs prune
```

### cleanup vs prune — which to use

| Command | What it does | Sessions visible after? |
|---------|-------------|------------------------|
| `ao-rs prune` | Removes worktrees only | ✅ Yes — still in dashboard |
| `ao-rs cleanup` | Removes worktrees **and** archives YAML to `.archive/` | ❌ No — hidden from dashboard |

**Use `ao-rs prune` for routine disk cleanup.** Use `ao-rs cleanup` only when you want to permanently hide sessions from the dashboard.

### Stop the lifecycle service

```bash
ao-rs stop
```

Sends SIGTERM to the process holding `~/.ao-rs/lifecycle.pid` (the `watch` or `dashboard` process). If the pidfile is stale, it is removed.

### Doctor

```bash
ao-rs doctor
```

Checks:
- Required tools on PATH (`git`, `gh`, `tmux`, `claude`)
- GitHub auth (`gh auth status`)
- Config file loadability and validation (unknown fields are warnings, not errors)
- Sessions directory presence

Run after initial setup and whenever something behaves unexpectedly.

### Remap session metadata

If a worktree or tmux session was moved or renamed out-of-band:

```bash
ao-rs session remap 3a4b5c6d --workspace /new/path/to/worktree
ao-rs session remap 3a4b5c6d --runtime-handle new-tmux-name
ao-rs session remap 3a4b5c6d --workspace /new/path --runtime-handle new-name
```

Remap only updates the YAML — it does not recreate the runtime. Chain with `restore` if you also need to respawn the agent:

```bash
ao-rs session remap 3a4b5c6d --workspace /new/path
ao-rs session restore 3a4b5c6d
```

Use `--force` to accept a `--workspace` path that does not yet exist on disk.

---

## 11. Configuration Reference (summary)

`ao-rs.yaml` lives in the project directory. `ao-rs start` generates a default. Full reference: **[config.md](config.md)**.

Key sections:

```yaml
port: 3000                         # dashboard port

defaults:
  agent: claude-code               # default agent for all spawns
  runtime: tmux                    # default runtime
  notifiers: []                    # default notifier list

projects:
  my-repo:
    repo: owner/my-repo            # required: GitHub owner/repo
    path: /Users/me/work/my-repo   # required: absolute path
    default_branch: main           # branch worktrees base off
    agent: claude-code             # per-project agent override
    agent_config:
      permissions: permissionless
      model: sonnet
      rules: |-
        Custom agent rules injected into the system prompt.

reactions:                         # see Section 8
  ...

notification_routing:              # see Section 9
  urgent: [stdout, ntfy]
  warning: [stdout]
  info: [stdout]
```

Unknown fields are warned and ignored — you will not get a hard error for TS-era config keys.

### Multi-project setup

A single `ao-rs.yaml` can manage multiple repos. Each entry under `projects:` gets its own session namespace and worktree directory.

```yaml
defaults:
  agent: claude-code
  runtime: tmux

projects:
  frontend:
    repo: acme/frontend
    path: /home/user/code/frontend
    default_branch: main
    agent_config:
      permissions: permissionless
      model: sonnet

  backend:
    repo: acme/backend
    path: /home/user/code/backend
    default_branch: main
    agent_config:
      permissions: permissionless
      rules: |-
        Run `go test ./...` before opening a PR.

  infra:
    repo: acme/infra
    path: /home/user/code/infra
    default_branch: main
```

Filter commands to one project with `--project`:

```bash
ao-rs status --project backend
ao-rs spawn --issue 99 --project frontend
ao-rs prune --project infra
```

Without `--project`, `ao-rs status` and `ao-rs watch` show/watch all projects.

---

## 12. Troubleshooting

### Rate limit errors from `gh`

`ao-rs status --pr` fans out one `gh pr list` call per session. With many sessions, you may hit GitHub's secondary rate limits. Mitigations:

- Use `ao-rs status` (without `--pr`) for routine checks
- Increase the watch interval: `ao-rs watch --interval 30`
- Stagger spawning instead of batch-spawning everything at once

### Stale pidfile — watch won't start

```
error: lifecycle loop already running (pid 12345)
```

If PID 12345 is gone but the file remains:

```bash
rm ~/.ao-rs/lifecycle.pid
ao-rs watch
```

Or use `ao-rs stop` which handles stale pidfiles automatically.

### Worktree already exists

```
error: worktree already exists at ~/.worktrees/my-repo/3a4b5c6d
```

This usually means the session was killed without cleanup. Remove the worktree manually:

```bash
git -C /path/to/your/repo worktree remove --force ~/.worktrees/my-repo/3a4b5c6d
ao-rs cleanup
```

Then re-spawn.

### Agent stuck / not responding

1. Check the session status: `ao-rs status --pr`
2. Attach and look: `ao-rs session attach 3a4b5c6d`
3. Send a nudge: `ao-rs send 3a4b5c6d "are you blocked on something?"`
4. If the tmux session is dead: `ao-rs session restore 3a4b5c6d`

Configure `agent-stuck` in `ao-rs.yaml` to get notified automatically when an agent goes silent for more than a threshold.

### Cost always shows `-`

Cost data comes from Claude Code's JSONL logs. It shows `-` until:
- The agent is Claude Code (not Cursor or Aider — those don't produce JSONL cost logs)
- The agent has completed at least one turn and written to its log

Give the session a minute after first activity, then run `ao-rs status --cost` again. If it still shows `-` after several turns, check that `claude` is the agent plugin in use for this session.

### `ao-rs pr` shows "no PR found"

The session has not opened a PR yet, or the branch was pushed to a fork that `gh` cannot see with the current auth. Check:

```bash
ao-rs session attach 3a4b5c6d   # look at what the agent is doing
gh pr list --head ao-3a4b5c6d   # check directly
```

If the PR exists but branch detection failed, bind it manually:

```bash
ao-rs session claim-pr 3a4b5c6d 42
```

---

## 13. Tips & Workflows

### Track spend with `--cost`

```bash
ao-rs status --cost --all
```

The cost column shows per-session token counts and USD estimates. A monthly cost ledger is persisted at `~/.ao-rs/cost-ledger/YYYY-MM.yaml` and survives session deletion, giving you a permanent spend record.

### Batch spawn a milestone

1. Label the GitHub issues for the milestone consistently
2. Collect issue numbers: `gh issue list --milestone "v2.0" --json number --jq '.[].number' | tr '\n' ','`
3. Spawn them all: `ao-rs batch-spawn --issues 42,43,44,45,46`
4. Start the dashboard: `ao-rs dashboard --open`
5. Watch the lanes fill as agents work

With reactions configured, you can leave them running overnight. You will be notified only when something needs human judgment.

### Lightweight local issue workflow (no GitHub)

```bash
# Create issues without leaving the terminal
ao-rs issue new --title "Improve parser error messages"
ao-rs issue new --title "Add retry logic to the HTTP client"

# List and browse
ao-rs issue list
ao-rs issue show 1

# Spawn from the file
ao-rs spawn --local-issue docs/issues/0001-improve-parser-error-messages.md
```

### Debug the lifecycle loop

```bash
RUST_LOG=ao_core=debug ao-rs watch
```

This traces every poll tick, transition decision, and reaction dispatch. Useful for understanding why a session is not transitioning or why a reaction is not firing.

Default log level is `warn,ao_core=info` — info includes status changes and reaction events. `debug` adds the per-tick probe results.

### Connect the dashboard to a remote ao-rs instance

The dashboard API is plain HTTP. If ao-rs is running on a remote machine:

```bash
# On the remote machine
ao-rs dashboard --port 3000

# SSH tunnel to your local machine
ssh -L 3000:localhost:3000 user@remote-host

# Open locally
open http://localhost:3000
```

The SSE stream at `/api/events` works through the tunnel for live updates.

### Auto-merge only on specific projects

Keep global auto-merge off and enable it per project:

```yaml
# ao-rs.yaml
reactions:
  approved-and-green:
    auto: false        # global: notify only
    action: notify
    priority: action

projects:
  safe-to-auto-merge:
    repo: acme/safe-to-auto-merge
    path: /Users/me/work/safe-to-auto-merge
    reactions:
      approved-and-green:
        auto: true
        action: auto-merge
        merge_method: squash
```
