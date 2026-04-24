#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> Building desktop UI"
(cd crates/ao-desktop/ui && npm run build)

echo "==> Cleaning dashboard ui-dist"
rm -rf crates/ao-dashboard/ui-dist

echo "==> Installing ao-cli"
cargo install --path crates/ao-cli --locked --force

echo
echo "Done. Binary:"
command -v ao-rs || true
