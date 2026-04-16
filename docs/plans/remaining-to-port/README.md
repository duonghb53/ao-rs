# Remaining-to-port task plans

This folder splits `docs/remaining-to-port.md` into small, priority-ordered tasks.

## Conventions

- **Status**: `planned` | `in_progress` | `done`
- **Filenames**: `x-y-<slug>.md` (e.g. `1-1-project-level-reaction-resolution.md`)
- **Goal**: each file is a standalone, minimal plan with acceptance criteria + test plan.

## Priority 1 — Functional gaps affecting runtime behavior

- [1.1 Project-level reaction resolution](./1-1-project-level-reaction-resolution.md)
- [1.2 Review thread resolution (is_resolved)](./1-2-review-thread-resolution-is-resolved.md)
- [1.3 Workspace plugin hooks (symlinks, postCreate, restore)](./1-3-workspace-plugin-hooks-symlinks-postcreate-restore.md)
- [1.4 Session restore prompt redelivery](./1-4-session-restore-prompt-redelivery.md)

## Priority 2 — Missing CLI commands

- [2.1 stop](./2-1-stop-command.md)
- [2.2 open](./2-2-open-command.md)
- [2.3 verify](./2-3-verify-command.md)
- [2.4 update](./2-4-update-command.md)
- [2.5 setup umbrella](./2-5-setup-umbrella.md)
- [2.6 plugin umbrella](./2-6-plugin-umbrella.md)
- [2.7 config-help](./2-7-config-help.md)

## Priority 3 — Missing CLI flags on existing commands

- [3.1 start missing flags](./3-1-start-missing-flags.md)
- [3.2 status missing flags](./3-2-status-missing-flags.md)
- [3.3 spawn missing flags](./3-3-spawn-missing-flags.md)
- [3.4 send missing flags](./3-4-send-missing-flags.md)
- [3.5 session kill missing flag purge-session](./3-5-session-kill-missing-flag-purge-session.md)
- [3.6 session claim-pr command](./3-6-session-claim-pr-command.md)
- [3.7 session remap command](./3-7-session-remap-command.md)
- [3.8 doctor missing flags](./3-8-doctor-missing-flags.md)
- [3.9 dashboard missing flag rebuild](./3-9-dashboard-missing-flag-rebuild.md)

## Priority 4 — Trait surface gaps (documented in `traits.rs`)

- [4.1 Scm trait missing methods](./4-1-scm-trait-missing-methods.md)
- [4.2 Tracker trait missing methods](./4-2-tracker-trait-missing-methods.md)
- [4.3 Agent trait stub defaults](./4-3-agent-trait-stub-defaults.md)

## Priority 5 — Plugin-level gaps

- [5.1 agent-cursor gaps](./5-1-agent-cursor-gaps.md)
- [5.2 agent-aider gaps](./5-2-agent-aider-gaps.md)
- [5.3 agent-codex gaps](./5-3-agent-codex-gaps.md)
- [5.4 tracker-github gaps](./5-4-tracker-github-gaps.md)
- [5.5 workspace-worktree and clone gaps](./5-5-workspace-worktree-and-clone-gaps.md)
- [5.6 scm-gitlab parity unknown](./5-6-scm-gitlab-parity-unknown.md)
- [5.7 notifier-ntfy gaps](./5-7-notifier-ntfy-gaps.md)

## Priority 6 — Parity-only modules (meta)

- [6.0 parity-only modules](./6-0-parity-only-modules.md)

## Priority 7 — Minor / cosmetic gaps

- [7.1 paths subset](./7-1-paths-subset.md)
- [7.2 activity log timestamps](./7-2-activity-log-timestamps.md)
- [7.3 events minimal surface](./7-3-events-minimal-surface.md)
- [7.4 default merge method](./7-4-default-merge-method.md)
- [7.5 github enterprise remote parsing](./7-5-github-enterprise-remote-parsing.md)

