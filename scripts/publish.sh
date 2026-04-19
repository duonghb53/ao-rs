#!/usr/bin/env bash
# Publish all ao-rs workspace crates to crates.io in dependency order.
#
# Usage:
#   ./scripts/publish.sh                        # publish all
#   ./scripts/publish.sh --dry-run              # verify packaging only
#   ./scripts/publish.sh --start-from ao-plugin-notifier-discord  # resume after failure
#   ./scripts/publish.sh --skip-tests           # skip test suite (faster re-runs)
#
# Prerequisites:
#   cargo login   (run once — token from https://crates.io/settings/tokens)
#   npm           (to build the React UI)
#
# Rate limit: crates.io allows ~1 new crate/minute.
# Script sleeps 65s between publishes automatically.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DRY_RUN=false
SKIP_TESTS=false
START_FROM=""
CARGO_FLAGS=""

for arg in "$@"; do
  case $arg in
    --dry-run)             DRY_RUN=true; CARGO_FLAGS="--dry-run" ;;
    --skip-tests)          SKIP_TESTS=true ;;
    --start-from)          ;;   # handled below via shift pattern
  esac
done

# Parse --start-from <crate>
for ((i=1; i<=$#; i++)); do
  if [[ "${!i}" == "--start-from" ]]; then
    j=$((i+1))
    START_FROM="${!j}"
  fi
done

# ── colours ───────────────────────────────────────────────────────────────────
BOLD='\033[1m'; GREEN='\033[0;32m'; RED='\033[0;31m'; CYAN='\033[0;36m'
YELLOW='\033[0;33m'; NC='\033[0m'
step()  { echo -e "\n${BOLD}${CYAN}▶ $*${NC}"; }
ok()    { echo -e "  ${GREEN}✓ $*${NC}"; }
skip()  { echo -e "  ${YELLOW}⊘ $* (skipped)${NC}"; }
fail()  { echo -e "  ${RED}✗ $*${NC}"; exit 1; }

# ── preflight ─────────────────────────────────────────────────────────────────
step "Preflight checks"
command -v cargo >/dev/null 2>&1 || fail "cargo not found"
command -v npm   >/dev/null 2>&1 || fail "npm not found"
command -v curl  >/dev/null 2>&1 || fail "curl not found"
ok "tools present"

if [[ "$DRY_RUN" == "false" ]]; then
  branch=$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)
  [[ "$branch" == "main" ]] || fail "not on main branch (currently: $branch)"
  [[ -z "$(git -C "$ROOT" status --porcelain)" ]] || fail "working tree dirty — commit first"
  ok "git clean on main"
fi

VERSION=$(grep '^version' "$ROOT/Cargo.toml" | head -1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')
echo -e "  version: ${BOLD}v${VERSION}${NC}"
[[ -n "$START_FROM" ]] && echo -e "  resuming from: ${BOLD}${START_FROM}${NC}"

# ── build UI ──────────────────────────────────────────────────────────────────
step "Building React UI"
(cd "$ROOT/crates/ao-desktop/ui" && npm install --silent && npm run build --silent)
ok "UI built → crates/ao-desktop/ui/dist/"

# ── tests ─────────────────────────────────────────────────────────────────────
if [[ "$SKIP_TESTS" == "true" ]]; then
  step "Tests"; skip "skipped via --skip-tests"
else
  step "Running test suite"
  (cd "$ROOT" && cargo t --workspace 2>&1 | tail -3)
  ok "tests passed"
fi

# ── publish helpers ───────────────────────────────────────────────────────────
RATE_LIMIT_SLEEP=65   # crates.io: ~1 new crate/minute
SKIP_UNTIL=""
[[ -n "$START_FROM" ]] && SKIP_UNTIL="$START_FROM"

# Check if a crate version is already on crates.io.
already_published() {
  local crate="$1"
  local status
  status=$(curl -sf -o /dev/null -w "%{http_code}" \
    "https://crates.io/api/v1/crates/${crate}/${VERSION}" 2>/dev/null || echo "0")
  [[ "$status" == "200" ]]
}

publish() {
  local crate="$1"

  # --start-from: skip until we reach the target crate.
  if [[ -n "$SKIP_UNTIL" ]]; then
    if [[ "$crate" == "$SKIP_UNTIL" ]]; then
      SKIP_UNTIL=""   # found it — stop skipping from next crate onward
    else
      skip "$crate"
      return
    fi
  fi

  # Skip if already published (safe to re-run).
  if [[ "$DRY_RUN" == "false" ]] && already_published "$crate"; then
    skip "$crate (v${VERSION} already on crates.io)"
    return
  fi

  echo -e "  publishing ${BOLD}${crate}${NC}..."
  (cd "$ROOT" && cargo publish -p "$crate" $CARGO_FLAGS 2>&1) \
    || fail "cargo publish failed for $crate"

  if [[ "$DRY_RUN" == "false" ]]; then
    echo -e "    ${YELLOW}waiting ${RATE_LIMIT_SLEEP}s for crates.io to index...${NC}"
    sleep "$RATE_LIMIT_SLEEP"
  fi

  ok "$crate"
}

# ── publish in dependency order ───────────────────────────────────────────────
step "Publishing to crates.io  (v${VERSION})"
[[ "$DRY_RUN" == "true" ]] && echo -e "  ${YELLOW}dry-run — nothing will be uploaded${NC}"

# Layer 0
publish ao-core

# Layer 1: plugins
publish ao-plugin-agent-aider
publish ao-plugin-agent-claude-code
publish ao-plugin-agent-codex
publish ao-plugin-agent-cursor
publish ao-plugin-notifier-desktop
publish ao-plugin-notifier-discord
publish ao-plugin-notifier-ntfy
publish ao-plugin-notifier-slack
publish ao-plugin-notifier-stdout
publish ao-plugin-runtime-process
publish ao-plugin-runtime-tmux
publish ao-plugin-scm-github
publish ao-plugin-scm-gitlab
publish ao-plugin-tracker-github
publish ao-plugin-tracker-linear
publish ao-plugin-workspace-clone
publish ao-plugin-workspace-worktree

# Layer 2: dashboard (build.rs copies ui/dist → ui-dist/ automatically)
publish ao-dashboard

# Layer 3: CLI binary
publish ao-rs

# ── done ──────────────────────────────────────────────────────────────────────
echo ""
if [[ "$DRY_RUN" == "true" ]]; then
  echo -e "${BOLD}${GREEN}Dry run complete.${NC}  Re-run without --dry-run to publish."
else
  echo -e "${BOLD}${GREEN}Published v${VERSION} to crates.io!${NC}"
  echo -e "  Install: ${CYAN}cargo install ao-rs${NC}"
fi
