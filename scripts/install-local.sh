#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT"

echo "Installing ao-rs from $(pwd)/crates/ao-cli ..."
cargo install --path crates/ao-cli --locked

echo
echo "Installed binary location:"
command -v ao-rs || true
echo
echo "Try:"
echo "  ao-rs --help"
echo "  ao-rs doctor"
