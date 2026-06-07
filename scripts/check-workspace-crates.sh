#!/usr/bin/env bash
# Run a per-package unit/build gate for every Cargo workspace member.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if command -v python3 >/dev/null 2>&1; then
  python_bin=python3
elif command -v python >/dev/null 2>&1; then
  python_bin=python
else
  echo "python3 or python is required to parse cargo metadata" >&2
  exit 1
fi

metadata="$(cargo metadata --no-deps --format-version 1)"

crate_targets="$(
  printf '%s' "$metadata" | "$python_bin" -c '
import json
import sys

metadata = json.load(sys.stdin)
members = set(metadata["workspace_members"])
packages = [p for p in metadata["packages"] if p["id"] in members]
packages.sort(key=lambda p: p["manifest_path"])

for package in packages:
    kinds = {kind for target in package["targets"] for kind in target["kind"]}
    print("{}\t{}\t{}".format(
        package["name"],
        1 if "lib" in kinds else 0,
        1 if "bin" in kinds else 0,
    ))
'
)"

while IFS=$'\t' read -r package has_lib has_bin; do
  if [[ -z "$package" ]]; then
    continue
  fi

  args=(-p "$package")
  if [[ "$has_lib" == "1" ]]; then
    args+=(--lib)
  fi
  if [[ "$has_bin" == "1" ]]; then
    args+=(--bins)
  fi
  args+=(--tests)

  echo "[crate-gate] cargo test ${args[*]}"
  cargo test "${args[@]}"
done <<< "$crate_targets"
