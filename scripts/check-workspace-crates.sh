#!/usr/bin/env bash
# Run every workspace unit, binary, and integration-test target once.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "[workspace-gate] cargo test --workspace --lib --bins --tests --no-fail-fast"
cargo test --workspace --lib --bins --tests --no-fail-fast
