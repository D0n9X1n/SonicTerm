#!/usr/bin/env python3
"""Verify SonicTerm's vendored native dependencies offline.

`check` re-derives each pinned vendor tree's digest from the working copy and compares it
with the manifest. It never reaches the network, runs anything upstream shipped, or writes
inside a vendor tree.

The vendored sources in this repository are the source of truth. They were imported and
patched by hand, once, under review; the manifest records what that import was, not a
recipe for rebuilding it. Nothing here reconstructs a vendor tree from an archive plus
local patch files, so no such patch files are kept.

What a matching digest does and does not establish: it proves the working copy still holds
exactly the bytes that were reviewed and committed, so drift, truncation, and local edits
are caught. It says nothing about who published those bytes or whether they are free of
defects. Publisher identity and fix provenance come from the recorded base release
(`archive`, `revision`, `tag`) and from each `upstream_fixes` entry's upstream revision and
URL, both of which a maintainer reads upstream rather than deriving from this repository.

Tree digest, algorithm "sonicterm-native-tree-v1":

    sha256( b"sonicterm-native-tree-v1\\n" + ascii(count) + b"\\n" + concat(records) )
    record = ascii(len(path)) + b"\\n" + path + ascii(len(bytes)) + b"\\n" + bytes

Paths are UTF-8 POSIX paths relative to the library root, sorted by raw bytes; both fields
are length-delimited so no separator is ambiguous. Content is the raw file bytes, portable
only because the vendored trees are checked in with `-text`. Mode is excluded so a Windows
checkout agrees with a POSIX one, and every non-regular entry is an error rather than
something the digest would have to describe.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
from typing import Sequence

# Schema 2 replaced schema 1's `patches` entries, which named local patch files, with
# `upstream_fixes` provenance records. The shapes are incompatible, so the old number is
# refused rather than reinterpreted.
SCHEMA_VERSION = 2
TREE_ALGORITHM = b"sonicterm-native-tree-v1"
UPSTREAM_FIX_KEYS = ("revision", "url")


class DependencyError(Exception):
    """A verification failure that `main` reports as exit code 1."""


def tree_sha256(root: Path) -> str:
    """Digest every regular file under `root` using the documented framing."""
    if root.is_symlink() or not root.is_dir():
        raise DependencyError("vendor tree is missing or not a directory: {}".format(root))
    entries: list[tuple[bytes, Path]] = []
    for current, directories, names in os.walk(root, followlinks=False):
        here = Path(current)
        for name in directories:
            # os.walk lists a symlinked directory but will not descend into it, so refusing
            # it keeps the digest total over the tree rather than silently partial.
            if (here / name).is_symlink():
                raise DependencyError("symlinked directory in tree: {}".format(here / name))
        for name in names:
            entry = here / name
            if not stat.S_ISREG(entry.lstat().st_mode):
                raise DependencyError("non-regular file in tree: {}".format(entry))
            entries.append((entry.relative_to(root).as_posix().encode("utf-8"), entry))
    entries.sort(key=lambda item: item[0])
    digest = hashlib.sha256()
    digest.update(TREE_ALGORITHM + b"\n" + str(len(entries)).encode("ascii") + b"\n")
    for relative, absolute in entries:
        data = absolute.read_bytes()
        digest.update(str(len(relative)).encode("ascii") + b"\n" + relative)
        digest.update(str(len(data)).encode("ascii") + b"\n" + data)
    return digest.hexdigest()


def _require(mapping: dict, key: str, library: str):
    if key not in mapping:
        raise DependencyError("library {} is missing required key {!r}".format(library, key))
    return mapping[key]


def _validate_upstream_fixes(library: dict, name: str) -> None:
    """Require every recorded fix to be provenance a reader can follow upstream."""
    fixes = _require(library, "upstream_fixes", name)
    if not isinstance(fixes, list):
        raise DependencyError("library {} upstream_fixes must be a list".format(name))
    for fix in fixes:
        if not isinstance(fix, dict):
            raise DependencyError("library {} has a non-object upstream_fixes entry".format(name))
        for key in UPSTREAM_FIX_KEYS:
            value = fix.get(key)
            if not isinstance(value, str) or not value.strip():
                raise DependencyError("library {} upstream_fixes entry needs a non-empty {!r}"
                                      .format(name, key))
        extra = sorted(set(fix) - set(UPSTREAM_FIX_KEYS))
        if extra:
            # `path` and `sha256` are the retired schema-1 fields. Accepting them would
            # advertise a local patch file that nothing reads and nothing applies.
            raise DependencyError(
                "library {} upstream_fixes entry has unsupported key(s) {}; records carry "
                "{} only".format(name, ", ".join(repr(key) for key in extra),
                                 " and ".join(repr(key) for key in UPSTREAM_FIX_KEYS)))


def load_manifest(manifest_path: Path) -> list[dict]:
    """Read the manifest and reject a schema this tool cannot claim to understand."""
    try:
        document = json.loads(manifest_path.read_text(encoding="utf-8"))
    except OSError as error:
        raise DependencyError("cannot read manifest {}: {}".format(manifest_path, error))
    except json.JSONDecodeError as error:
        raise DependencyError("manifest {} is not valid JSON: {}".format(manifest_path, error))
    if document.get("schema_version") != SCHEMA_VERSION:
        raise DependencyError("manifest schema_version {!r} is not supported (expected {})".format(
            document.get("schema_version"), SCHEMA_VERSION))
    libraries = document.get("libraries")
    if not isinstance(libraries, list) or not libraries:
        raise DependencyError("manifest has no libraries")
    seen: set[str] = set()
    for library in libraries:
        name = _require(library, "name", "<unnamed>")
        if name in seen:
            raise DependencyError("duplicate library name {!r}".format(name))
        seen.add(name)
        if "patches" in library:
            # Nothing reads this key any more, so leaving it would look verified while
            # being ignored entirely.
            raise DependencyError("library {} still declares {!r}; schema {} records upstream "
                                  "fixes as {!r}".format(name, "patches", SCHEMA_VERSION,
                                                         "upstream_fixes"))
        for key in ("path", "archive", "include"):
            _require(library, key, name)
        for key in ("url", "sha256", "root"):
            _require(library["archive"], key, "{}.archive".format(name))
        if not library["include"]:
            raise DependencyError("library {} has an empty include list".format(name))
        _validate_upstream_fixes(library, name)
    return libraries


def select_library(libraries: Sequence[dict], name: str) -> dict:
    """Return the manifest entry for `name`, or fail rather than check nothing."""
    for library in libraries:
        if library["name"] == name:
            return library
    raise DependencyError("unknown library {!r}".format(name))


def verify_version(tree: Path, check: dict) -> None:
    """Confirm the pinned version still appears in the header the build compiles."""
    path = tree / PurePosixPath(check["path"])
    if not path.is_file():
        raise DependencyError("version_check file is missing: {}".format(path))
    pattern = re.compile(check["pattern"])
    if pattern.groups != 1:
        raise DependencyError("version_check pattern {!r} must have exactly one capture group"
                              .format(check["pattern"]))
    match = pattern.search(path.read_text(encoding="utf-8", errors="replace"))
    if match is None:
        raise DependencyError("version_check pattern {!r} did not match {}".format(
            check["pattern"], path))
    if match.group(1) != check["value"]:
        raise DependencyError("version_check {}: found {!r}, manifest pins {!r}".format(
            path, match.group(1), check["value"]))


def verify_versions(tree: Path, library: dict) -> None:
    """Validate both unified and split upstream version definitions."""
    configured = library.get("version_check")
    checks = [] if configured is None else (
        list(configured) if isinstance(configured, list) else [configured])
    for check in checks:
        verify_version(tree, check)


def check_library(repo_root: Path, library: dict) -> None:
    """Verify one library's pinned tree and any configured version checks."""
    expected = library.get("tree_sha256")
    if not expected:
        raise DependencyError("library {} has no pinned tree_sha256; record the digest of the "
                              "reviewed vendor tree".format(library["name"]))
    tree = repo_root / PurePosixPath(library["path"])
    actual = tree_sha256(tree)
    if actual != expected:
        raise DependencyError("library {} tree_sha256 {} does not match manifest {}".format(
            library["name"], actual, expected))
    verify_versions(tree, library)


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    default_root = Path(__file__).resolve().parent.parent
    # Shared by the top level and the subcommand so either position works. SUPPRESS stops
    # the subparser copy from overwriting a value given ahead of the subcommand.
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--repo-root", type=Path, default=argparse.SUPPRESS)
    common.add_argument("--manifest", type=Path, default=argparse.SUPPRESS)

    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0], parents=[common])
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check", parents=[common],
                                help="verify pinned vendor trees offline")
    check.add_argument("--library")

    args = parser.parse_args(argv)
    args.repo_root = getattr(args, "repo_root", None) or default_root
    if getattr(args, "manifest", None) is None:
        args.manifest = args.repo_root / "scripts" / "native-dependencies.json"
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    try:
        libraries = load_manifest(args.manifest)
        selected = [select_library(libraries, args.library)] if args.library else libraries
        for library in selected:
            name = library["name"]
            print(f"[native-dependencies] start {name}", file=sys.stderr, flush=True)
            try:
                check_library(args.repo_root, library)
            except DependencyError:
                print(f"[native-dependencies] finish {name} exit=1", file=sys.stderr, flush=True)
                raise
            print("ok {} {}".format(name, library.get("version", "")).rstrip(), flush=True)
            print(f"[native-dependencies] finish {name} exit=0", file=sys.stderr, flush=True)
        return 0
    except DependencyError as error:
        print("native-dependencies: {}".format(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
