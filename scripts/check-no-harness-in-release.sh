#!/usr/bin/env bash
# scripts/check-no-harness-in-release.sh
#
# POSIX shim for the issue-#506 gate so non-Windows CI runners can also
# verify the default release build is clean. Mirrors the PowerShell
# version. Skips politely on non-Windows targets where there's no
# matching exe to inspect.
set -euo pipefail

cd "$(dirname "$0")/.."

target_exe="target/release/sonicterm-windows.exe"
if [[ "$(uname -s)" != MINGW* && "$(uname -s)" != CYGWIN* && "$(uname -s)" != MSYS* ]]; then
  if ! command -v cargo >/dev/null; then
    echo "[#506] skip: no cargo on PATH" && exit 0
  fi
fi

echo "[#506] Building sonicterm-windows --release (no features)…"
cargo build --release -p sonicterm-windows

if [[ ! -f "$target_exe" ]]; then
  echo "[#506] skip: $target_exe not present (cross-target build?)"
  exit 0
fi

if LC_ALL=C tr -c '[:print:]' '\n' < "$target_exe" | grep -E 'harness_pipe|sonic-harness-pipe|sonicterm-harness-' >/dev/null; then
  echo "[#506] FAIL: harness_pipe symbols leaked into default release build:"
  LC_ALL=C tr -c '[:print:]' '\n' < "$target_exe" | grep -E 'harness_pipe|sonic-harness-pipe|sonicterm-harness-' | head -n 10
  exit 1
fi

echo "[#506] OK: no harness symbols in default release build."
