# 5.2 agent-aider gaps

Status: planned

## Why

aider agent is minimally integrated; defaults differ from typical TS flows and no cost estimation is available.

## Current state (ao-rs)

- `crates/plugins/agent-aider/src/lib.rs`
  - Launches `aider` without opinionated defaults like `--yes`.
  - No `cost_estimate` override.

## Target behavior (ao-ts parity)

- Align aider launch and prompt behavior with TS plugin strategy.
- Optionally provide cost estimation if aider exposes it (or document as unsupported).

## Proposed approach

1. Review TS aider plugin strategy and decide which defaults are safe:
   - If TS uses `--yes` / other flags, replicate.
2. Ensure prompt/rules behavior is consistent with other agents.
3. If cost is not feasible, make the unsupported status explicit in docs/CLI.

## Files to change

- `crates/plugins/agent-aider/src/lib.rs`
- (Optional) docs about cost support per agent

## Acceptance criteria

- aider launches with the chosen defaults and receives the correct initial prompt.
- Behavior is documented in plugin spec or config docs.

## Test plan

- Unit tests for command-line args assembly.
- Mock runtime send tests if prompt delivery is post-launch.

## Risks / notes

- aider flag compatibility varies by version; detect and degrade gracefully.

