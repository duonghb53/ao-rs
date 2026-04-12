#!/usr/bin/env bash
# ao-rs vs ao-ts benchmark comparison
# Usage: ./scripts/benchmark.sh [ao-ts-path]
#
# Compares startup time, memory usage, binary size, build time,
# and codebase metrics between ao-rs and the TypeScript original.

set -euo pipefail

AO_RS_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AO_TS_ROOT="${1:-$HOME/study/agent-orchestrator}"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

divider() { echo -e "${DIM}$(printf '%.0s─' {1..60})${NC}"; }

echo -e "\n${BOLD}${CYAN}  ao-rs vs ao-ts  Benchmark${NC}\n"
divider

# ── Binary / install ──────────────────────────────────────────
echo -e "\n${BOLD}Binary Size${NC}"

AO_RS_BIN="$AO_RS_ROOT/target/release/ao-rs"
if [[ ! -f "$AO_RS_BIN" ]]; then
  echo "  building ao-rs (release)..."
  (cd "$AO_RS_ROOT" && cargo build --release -p ao-cli 2>/dev/null)
fi
AO_RS_SIZE=$(du -sh "$AO_RS_BIN" | awk '{print $1}')
echo -e "  ao-rs binary:  ${GREEN}${AO_RS_SIZE}${NC}  (single static binary)"

if [[ -d "$AO_TS_ROOT" ]]; then
  AO_TS_SIZE=$(du -sh "$AO_TS_ROOT/node_modules" 2>/dev/null | awk '{print $1}' || echo "N/A")
  echo -e "  ao-ts node_modules:  ${RED}${AO_TS_SIZE}${NC}  (runtime dependency tree)"
else
  echo "  ao-ts: not found at $AO_TS_ROOT"
fi

divider

# ── Startup time ──────────────────────────────────────────────
echo -e "\n${BOLD}Startup Time${NC} (status command, avg of 5 runs)"

rs_total=0
for i in {1..5}; do
  t=$( { TIMEFORMAT='%R'; time "$AO_RS_BIN" status >/dev/null 2>&1; } 2>&1 )
  rs_total=$(echo "$rs_total + $t" | bc)
done
rs_avg=$(echo "scale=3; $rs_total / 5" | bc)
echo -e "  ao-rs:  ${GREEN}${rs_avg}s${NC}"

if [[ -d "$AO_TS_ROOT" ]]; then
  ts_total=0
  for i in {1..5}; do
    t=$( { TIMEFORMAT='%R'; time npx --prefix "$AO_TS_ROOT" ao status >/dev/null 2>&1; } 2>&1 || true )
    ts_total=$(echo "$ts_total + $t" | bc)
  done
  ts_avg=$(echo "scale=3; $ts_total / 5" | bc)
  speedup=$(echo "scale=1; $ts_avg / $rs_avg" | bc)
  echo -e "  ao-ts:  ${RED}${ts_avg}s${NC}  (${speedup}x slower)"
fi

divider

# ── Memory usage ──────────────────────────────────────────────
echo -e "\n${BOLD}Memory Usage${NC} (peak RSS, status command)"

rs_mem=$(/usr/bin/time -l "$AO_RS_BIN" status 2>&1 | grep "maximum resident" | awk '{print $1}')
rs_mem_mb=$(echo "scale=1; $rs_mem / 1048576" | bc)
echo -e "  ao-rs:  ${GREEN}${rs_mem_mb} MB${NC}"

if [[ -d "$AO_TS_ROOT" ]]; then
  ts_mem=$( (cd "$AO_TS_ROOT" && /usr/bin/time -l npx ao status 2>&1) | grep "maximum resident" | awk '{print $1}' || echo "0" )
  ts_mem_mb=$(echo "scale=1; $ts_mem / 1048576" | bc)
  ratio=$(echo "scale=1; $ts_mem_mb / $rs_mem_mb" | bc 2>/dev/null || echo "?")
  echo -e "  ao-ts:  ${RED}${ts_mem_mb} MB${NC}  (${ratio}x more)"
fi

divider

# ── Codebase metrics ──────────────────────────────────────────
echo -e "\n${BOLD}Codebase Metrics${NC}"

rs_files=$(find "$AO_RS_ROOT/crates" -name "*.rs" | wc -l | tr -d ' ')
rs_lines=$(find "$AO_RS_ROOT/crates" -name "*.rs" -exec cat {} + | wc -l | tr -d ' ')
rs_tests=$(cd "$AO_RS_ROOT" && cargo test --workspace 2>&1 | grep "test result" | awk '{sum += $4} END {print sum}')

echo -e "  ${BOLD}ao-rs${NC}"
echo -e "    Files:  $rs_files .rs files"
echo -e "    Lines:  $(printf "%'d" "$rs_lines") lines of Rust"
echo -e "    Tests:  ${GREEN}$rs_tests${NC} passing"

if [[ -d "$AO_TS_ROOT" ]]; then
  ts_files=$(find "$AO_TS_ROOT/packages" -name "*.ts" -o -name "*.tsx" | wc -l | tr -d ' ')
  ts_lines=$(find "$AO_TS_ROOT/packages" -name "*.ts" -o -name "*.tsx" -exec cat {} + | wc -l | tr -d ' ')
  ts_test_files=$(find "$AO_TS_ROOT/packages" -name "*.test.*" | wc -l | tr -d ' ')

  echo -e "  ${BOLD}ao-ts${NC}"
  echo -e "    Files:  $ts_files .ts/.tsx files"
  echo -e "    Lines:  $(printf "%'d" "$ts_lines") lines of TypeScript"
  echo -e "    Tests:  $ts_test_files test files"
fi

divider

# ── Build time ────────────────────────────────────────────────
echo -e "\n${BOLD}Build Time${NC} (incremental, single crate touch)"

touch "$AO_RS_ROOT/crates/ao-cli/src/main.rs"
rs_build=$( { TIMEFORMAT='%R'; time (cd "$AO_RS_ROOT" && cargo build --release -p ao-cli 2>/dev/null); } 2>&1 )
echo -e "  ao-rs (incremental):  ${GREEN}${rs_build}s${NC}"

divider
echo -e "\n${DIM}Run: ./scripts/benchmark.sh [path-to-agent-orchestrator]${NC}\n"
