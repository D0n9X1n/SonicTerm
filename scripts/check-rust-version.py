#!/usr/bin/env python3
"""Verify the declared `rust-version` can actually build the locked workspace.

The declaration is a promise to whoever provisions a toolchain from it. CI
installs floating `stable`, so CI never tests that promise: the declared
minimum can fall arbitrarily far behind what the lockfile needs and every gate
stays green while a fresh contributor on the documented version hits a
resolution failure the lockfile guarantees.

This reads the effective floor from the dependency graph — each package's own
`rust-version`, as resolved — and fails when the declaration is below it.

Deliberately not a pinned-toolchain CI job: that would need a full second build
against an old compiler on every run, and would only prove the same inequality
this computes in about a second.
"""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys


def parse(version: str) -> tuple[int, ...]:
    """`1.88.0` and `1.88` both sort as `(1, 88, 0)`."""
    parts = [int(part) for part in version.split(".")]
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts)


def repository_root() -> Path:
    return Path(__file__).resolve().parent.parent


def main() -> int:
    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1"],
        capture_output=True,
        text=True,
        check=False,
        cwd=repository_root(),
    )
    if completed.returncode != 0:
        print("check-rust-version: cargo metadata failed:", file=sys.stderr)
        print(completed.stderr.strip(), file=sys.stderr)
        return 1

    metadata = json.loads(completed.stdout)
    workspace = set(metadata["workspace_members"])

    declared: str | None = None
    highest: tuple[tuple[int, ...], str, str, str] | None = None
    for package in metadata["packages"]:
        version = package.get("rust_version")
        if version is None:
            continue
        if package["id"] in workspace:
            # First-party crates inherit the workspace value, so any one of
            # them reports the declaration. They are not part of the floor
            # they are being checked against.
            declared = version
            continue
        candidate = (parse(version), version, package["name"], package["version"])
        if highest is None or candidate[0] > highest[0]:
            highest = candidate

    if declared is None:
        print("check-rust-version: no workspace rust-version found.", file=sys.stderr)
        return 1

    if highest is None:
        print(
            "check-rust-version: no dependency declares a rust-version.",
            file=sys.stderr,
        )
        return 1

    _, required, crate, crate_version = highest

    if parse(declared) < parse(required):
        print(
            f"FAILED: workspace rust-version is {declared}, but the locked graph "
            f"requires {required} ({crate} {crate_version}).\n"
            f"Raise `rust-version` in Cargo.toml to {required}, or lower the "
            f"dependency that needs it.",
            file=sys.stderr,
        )
        return 1

    print(
        f"check-rust-version: declared {declared} >= required {required} "
        f"({crate} {crate_version}). OK."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
