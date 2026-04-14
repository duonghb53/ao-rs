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
  - `notifiers` (list of notifier names; see below)
  - `orchestrator` / `worker` (optional)
    - `agent` (string, optional)
    - `agentConfig` / `agent_config` (optional)
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

## Tooling

- `ao-rs doctor` reports config load/validation failures as **FAIL**, and unsupported/unknown fields as **warnings**.

