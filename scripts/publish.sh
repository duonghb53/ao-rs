#!/usr/bin/env bash
# Publish all ao-rs workspace crates to crates.io in dependency order.
#
# Usage:
#   ./scripts/publish.sh           # publish current version
#   ./scripts/publish.sh --dry-run # verify packaging without uploading
#
# Prerequisites:
#   cargo login   (run once with token from https://crates.io/settings/tokens)
#   npm           (to build the React UI)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DRY_RUN=false
CARGO_PUBLISH_FLAGS=""

for arg in "$@"; do
  case $arg in
    --dry-run) DRY_RUN=true; CARGO_PUBLISH_FLAGS="--dry-run" ;;
  esac
done

# ── colours ──────────────────────────────────────────────────────────────────
BOLD='\033[1m'; GREEN='\033[0;32m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
step()  { echo -e "\n${BOLD}${CYAN}▶ $*${NC}"; }
ok()    { echo -e "  ${GREEN}✓ $*${NC}"; }
fail()  { echo -e "  ${RED}✗ $*${NC}"; exit 1; }

# ── preflight ─────────────────────────────────────────────────────────────────
step "Preflight checks"

command -v cargo >/dev/null 2>&1 || fail "cargo not found"
command -v npm   >/dev/null 2>&1 || fail "npm not found (required to build UI)"
ok "tools present"

# Must be on main with clean working tree (unless dry-run)
if [[ "$DRY_RUN" == "false" ]]; then
  branch=$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)
  [[ "$branch" == "main" ]] || fail "not on main branch (currently: $branch)"
  [[ -z "$(git -C "$ROOT" status --porcelain)" ]] || fail "working tree is dirty — commit or stash first"
  ok "git clean on main"
fi

# ── build UI ──────────────────────────────────────────────────────────────────
step "Building React UI"
UI_DIR="$ROOT/crates/ao-desktop/ui"
(cd "$UI_DIR" && npm install --silent && npm run build)
ok "UI built → crates/ao-desktop/ui/dist/"

# ── tests ─────────────────────────────────────────────────────────────────────
step "Running test suite"
(cd "$ROOT" && cargo t --workspace 2>&1 | tail -5)
ok "tests passed"

# ── publish helper ────────────────────────────────────────────────────────────
# Publishes one crate, then sleeps so crates.io can index it before dependents.
publish() {
  local crate="$1"
  echo -e "  publishing ${BOLD}${crate}${NC}..."
  (cd "$ROOT" && cargo publish -p "$crate" $CARGO_PUBLISH_FLAGS 2>&1) \
    || fail "cargo publish failed for $crate"
  if [[ "$DRY_RUN" == "false" ]]; then
    sleep 20   # wait for crates.io to index before publishing dependents
  fi
  ok "$crate published"
}

# ── publish in dependency order ───────────────────────────────────────────────
step "Publishing crates to crates.io"
[[ "$DRY_RUN" == "true" ]] && echo "  (dry-run — nothing will be uploaded)"

# Layer 0: core (no internal deps)
publish ao-core

# Layer 1: plugins (all depend on ao-core only)
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

# Layer 2: dashboard (embeds UI, depends on ao-core + plugins)
# build.rs copies ui/dist → ui-dist/ automatically during cargo build
publish ao-dashboard

# Layer 3: CLI binary (depends on everything)
publish ao-rs

# ── done ──────────────────────────────────────────────────────────────────────
echo ""
if [[ "$DRY_RUN" == "true" ]]; then
  echo -e "${BOLD}${GREEN}Dry run complete — re-run without --dry-run to publish.${NC}"
else
  VERSION=$(grep '^version' "$ROOT/Cargo.toml" | head -1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')
  echo -e "${BOLD}${GREEN}Published v${VERSION} to crates.io!${NC}"
  echo -e "  Install: ${CYAN}cargo install ao-rs${NC}"
fi
