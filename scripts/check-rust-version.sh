#!/usr/bin/env bash
#
# Verify the declared `rust-version` can actually build the locked workspace.
#
# The declaration is a promise to whoever provisions a toolchain from it. CI
# installs floating `stable`, so CI never tests that promise: the declared
# minimum can fall arbitrarily far behind what the lockfile needs and every
# gate stays green while a fresh contributor on the documented version hits a
# resolution failure the lockfile guarantees.
#
# This reads the effective floor from the dependency graph — each package's own
# `rust-version`, as resolved — and fails when the declaration is below it.
#
# Deliberately not a pinned-toolchain CI job: that would need a full second
# build against an old compiler on every run, and it would only prove the same
# arithmetic this does in a second.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

python_cmd=python3
if ! command -v "$python_cmd" >/dev/null 2>&1; then
    python_cmd=python
fi

metadata=$(cargo metadata --format-version 1 2>/dev/null)

# The Python program arrives on fd 3 rather than stdin, because stdin is
# carrying the metadata JSON it reads.
"$python_cmd" /dev/fd/3 3<<'PY' <<<"$metadata"
import json
import sys


def parse(version):
    """`1.88.0` and `1.88` both sort as (1, 88, 0)."""
    parts = [int(part) for part in version.split(".")]
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts)


metadata = json.load(sys.stdin)
workspace = set(metadata["workspace_members"])

declared = None
highest = None
for package in metadata["packages"]:
    version = package.get("rust_version")
    if version is None:
        continue
    if package["id"] in workspace:
        # First-party crates inherit the workspace value, so any one of them
        # reports the declaration. They are not part of the floor they are
        # being checked against.
        declared = version
        continue
    candidate = (parse(version), version, package["name"], package["version"])
    if highest is None or candidate[0] > highest[0]:
        highest = candidate

if declared is None:
    print("check-rust-version: no workspace rust-version found.", file=sys.stderr)
    raise SystemExit(1)

if highest is None:
    print("check-rust-version: no dependency declares a rust-version.", file=sys.stderr)
    raise SystemExit(1)

_, required, crate, crate_version = highest

if parse(declared) < parse(required):
    print(
        f"FAILED: workspace rust-version is {declared}, but the locked graph "
        f"requires {required} ({crate} {crate_version}).\n"
        f"Raise `rust-version` in Cargo.toml to {required}, or lower the "
        f"dependency that needs it.",
        file=sys.stderr,
    )
    raise SystemExit(1)

print(
    f"check-rust-version: declared {declared} >= required {required} "
    f"({crate} {crate_version}). OK."
)
PY
