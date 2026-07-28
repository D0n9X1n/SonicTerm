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
    # Bytes, decoded explicitly as UTF-8 rather than `text=True`. `text=True`
    # decodes with the *locale* encoding, which is cp1252 on the Windows
    # runner, and `cargo metadata` is UTF-8: dependency authors and
    # descriptions carry non-Latin-1 characters. The mismatch raises inside
    # subprocess's reader thread, leaving `stdout` as None for a confusing
    # `TypeError` at the json.loads below rather than a decode error here.
    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1"],
        capture_output=True,
        check=False,
        cwd=repository_root(),
    )
    if completed.returncode != 0:
        print("check-rust-version: cargo metadata failed:", file=sys.stderr)
        print(completed.stderr.decode("utf-8", errors="replace").strip(), file=sys.stderr)
        return 1

    metadata = json.loads(completed.stdout.decode("utf-8"))
    workspace = set(metadata["workspace_members"])
    # Keyed by declared version so a disagreement among first-party crates is
    # visible rather than resolved by iteration order. Today all 23 inherit
    # `rust-version.workspace = true`, so this holds one entry; if a crate ever
    # sets its own, picking whichever package the metadata happened to list
    # last would check an arbitrary one of them and pass while another crate
    # declares something the lock cannot build.
    declared_by_crate: dict[str, list[str]] = {}
    highest: tuple[tuple[int, ...], str, str, str] | None = None
    for package in metadata["packages"]:
        version = package.get("rust_version")
        if version is None:
            continue
        if package["id"] in workspace:
            declared_by_crate.setdefault(version, []).append(package["name"])
            continue
        candidate = (parse(version), version, package["name"], package["version"])
        if highest is None or candidate[0] > highest[0]:
            highest = candidate

    if not declared_by_crate:
        print("check-rust-version: no workspace rust-version found.", file=sys.stderr)
        return 1

    if len(declared_by_crate) > 1:
        print(
            "FAILED: first-party crates declare more than one rust-version, so "
            "there is no single declaration to check:",
            file=sys.stderr,
        )
        for version, names in sorted(declared_by_crate.items()):
            print(f"  {version}: {', '.join(sorted(names))}", file=sys.stderr)
        return 1

    declared = next(iter(declared_by_crate))

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
