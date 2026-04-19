# ao-rs config (`ao-rs.yaml`)

`ao-rs.yaml` lives in a project directory (discovered by walking up from the current working directory). It controls **defaults**, **projects**, **reactions**, and **notification routing**.

This document defines the **supported subset** and the **validation strategy** for Rust (`ao-rs`). The loader is intentionally strict about *meaningful* mistakes while remaining migration-friendly for TS configs.

## Supported top-level keys

- `port` (number): dashboard port.
- `terminalPort` / `directTerminalPort` (number, optional)
- `power.preventIdleSleep` (bool, optional)
- `defaults` (optional)
  - `runtime` (string)
  - `agent` (string)
  - `workspace` (string)
  - `tracker` (string)
  - `notifiers` (list of notifier names, e.g. `[stdout, discord]`)
  - `orchestrator_rules` (string, optional): rules prepended to every orchestrator prompt across all projects. Per-project `orchestrator_rules` are appended after these.
  - `orchestrator` / `worker` (optional): role-level defaults that apply to every project.
    - `agent` (string, optional): plugin name override for this role.
    - `agentConfig` / `agent_config` (optional): fields identical to the project-level `agent_config`. Setting `agent_config.model` here pins a single orchestrator (or worker) model across **every** project you manage — a project can still override it.
      - `permissions` (string)
      - `rules` (string, optional)
      - `rulesFile` / `rules_file` (string, optional)
      - `model` (string, optional)
      - `orchestratorModel` (string, optional)
      - `opencodeSessionId` (string, optional)
- `projects` (map, optional)
  - `<projectId>.repo` (**required**): `"owner/repo"`
  - `<projectId>.path` (**required**): absolute path (must start with `/`; `~` is rejected)
  - `<projectId>.defaultBranch` / `default_branch` (string, optional; defaults to `"main"`)
  - plus the fields present in `ao-rs.yaml.example`
- `reactions` (map, optional): keyed by reaction key (see below)
- `notificationRouting` / `notification_routing` / `notification-routing` (map, optional)
- `notifiers` (map, optional): stored for parity (plugin configs); not all entries are consumed yet.
- `plugins` (list, optional): stored for parity only.

## Unknown-field policy

- **Unknown fields are warned and ignored.**
- The warning includes a best-effort **field path** so you can fix typos quickly.

## Validation (errors)

Misconfigurations that can break behavior are rejected with clear errors that include the **file path** and the **field**:

### Reactions

- **Reaction keys** must be from the supported set:
  - `ci-failed`
  - `changes-requested`
  - `merge-conflicts`
  - `approved-and-green`
  - `agent-idle`
  - `agent-stuck`
  - `agent-needs-input`
  - `agent-exited`
  - `all-complete`
- **Durations** in `reactions.*.threshold` and `reactions.*.escalate_after` must match \(^\d+(s|m|h)$\) (examples: `"10s"`, `"5m"`, `"2h"`).

### Notifiers + routing

- `defaults.notifiers[]` and `notification_routing.<priority>[]` must reference a supported notifier name:
  - `stdout`, `desktop`, `ntfy`, `discord`, `slack`

### Projects

- `projects.*.repo` must be `"owner/repo"`.
- `projects.*.path` must be an **absolute path** (must start with `/`; `~` is not supported).

## Notifier config

Push notifiers require credentials. These can be provided as either environment variables **or** YAML fields — YAML takes precedence:

```yaml
notifiers:
  discord:
    webhookUrl: https://discord.com/api/webhooks/<id>/<token>
  slack:
    webhookUrl: https://hooks.slack.com/services/<id>
  ntfy:
    topic: my-topic
    url: https://ntfy.sh          # optional, defaults to ntfy.sh

defaults:
  notifiers: [stdout, discord]    # names to activate
```

Environment variable fallbacks (when YAML field is absent):

| Notifier | Env var |
| --- | --- |
| `discord` | `AO_DISCORD_WEBHOOK_URL` |
| `slack` | `AO_SLACK_WEBHOOK_URL` |
| `ntfy` | `AO_NTFY_TOPIC` + optional `AO_NTFY_URL` |

## Multi-project setup

A single `ao-rs.yaml` can manage several repos:

```yaml
projects:
  frontend:
    repo: acme/frontend
    path: /home/user/code/frontend
    default_branch: main
    agent: claude-code
    agent_config:
      permissions: permissionless
      model: sonnet

  backend:
    repo: acme/backend
    path: /home/user/code/backend
    default_branch: main
    agent: claude-code
    agent_config:
      permissions: permissionless
      # per-project worker rules
      rules: |-
        Follow Go conventions. Run `go test ./...` before opening a PR.

  infra:
    repo: acme/infra
    path: /home/user/code/infra
    default_branch: main
    orchestrator_rules: |-
      You are the infra orchestrator. Only spawn workers for Terraform changes.
```

Each project gets its own session namespace (`~/.ao-rs/sessions/<projectId>/`) and worktree directory (`~/.worktrees/<projectId>/`).

Filter commands to one project with `--project <name>`:
```bash
ao-rs status --project backend
ao-rs spawn --issue 99 --project frontend
ao-rs prune --project infra
```

## Model selection (orchestrator vs. worker)

ao-rs separates the **orchestrator** model from the **worker** model so you can run the coordinator on a stronger model (e.g. `opus`) and its workers on a cheaper/faster model (e.g. `sonnet`). Each role resolves its model independently using a layered fallback chain.

### Where to set it

Three common places, in increasing specificity:

| Location | Scope | Use when |
|---|---|---|
| `defaults.orchestrator.agent_config.model` | **Every** project's orchestrator | You want a single pinned orchestrator model across all projects |
| `defaults.worker.agent_config.model` | Every project's worker | You want a single pinned worker model across all projects |
| `projects.<id>.agent_config.model` | Workers on this project | Per-project override for workers |
| `projects.<id>.agent_config.orchestratorModel` | Orchestrator on this project | Per-project override just for the orchestrator (keeps the worker model unchanged) |
| `projects.<id>.orchestrator.agent_config.model` | Orchestrator on this project | Equivalent to `orchestratorModel` — use whichever reads more naturally |
| `projects.<id>.worker.agent_config.model` | Workers on this project | Role-specific override separate from the shared `agent_config` |

**Recommended setup:** set `defaults.orchestrator.agent_config.model` once, set a worker `model` per project (or once in `defaults.worker.agent_config.model`), and only add per-project overrides when a specific project actually needs a different model.

### Fallback chain (orchestrator role)

When ao-rs picks the orchestrator model for a given project, it walks this list top-to-bottom and uses the first value that is set:

1. `projects.<id>.orchestrator.agent_config.orchestratorModel`
2. `projects.<id>.orchestrator.agent_config.model`
3. `projects.<id>.agent_config.orchestratorModel`
4. `projects.<id>.agent_config.model`
5. `defaults.orchestrator.agent_config.orchestratorModel`
6. `defaults.orchestrator.agent_config.model`  ← **the "one place" setting**

If none are set, the agent plugin uses its own built-in default (e.g. `claude` with no `--model` flag).

### Fallback chain (worker role)

1. `projects.<id>.worker.agent_config.model`
2. `projects.<id>.agent_config.model`
3. `defaults.worker.agent_config.model`

Workers never consult `orchestratorModel` — that field is ignored for worker sessions.

### Example: one orchestrator model for every project

```yaml
defaults:
  orchestrator:
    agent: claude-code
    agent_config:
      model: opus         # every project's orchestrator runs on opus
  worker:
    agent: claude-code
    agent_config:
      model: sonnet       # every project's workers run on sonnet

projects:
  api:
    repo: acme/api
    path: /home/user/code/api
    # No model overrides — inherits opus/sonnet from defaults.

  frontend:
    repo: acme/frontend
    path: /home/user/code/frontend
    agent_config:
      model: haiku        # frontend workers on haiku, orchestrator still opus
```

### Example: per-project orchestrator override

```yaml
projects:
  critical-infra:
    repo: acme/infra
    path: /home/user/code/infra
    agent_config:
      model: sonnet                # workers
      orchestratorModel: opus      # this project's orchestrator uses opus
```

Use `ao-rs doctor` to confirm the config loads without warnings after editing.

## Tooling

- `ao-rs doctor` reports config load/validation failures as **FAIL**, and unsupported/unknown fields as **warnings**.
- `ao-rs start` generates a minimal `ao-rs.yaml` from the current git repo — run it once per project directory, then merge the configs manually.

