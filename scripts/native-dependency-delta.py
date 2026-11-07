#!/usr/bin/env python3
"""Compare SonicTerm's vendored native trees with their pinned upstream archives.

This reaches the network, unlike `native-dependencies.py check`: for each library it downloads
the pinned archive, verifies its sha256 against the manifest, reads the `include` subset in
memory (nothing is written to disk) and lists every retained file that differs from it or that
the archive lacks. That set must equal the library's `local_modifications`, so an unrecorded
edit, a recorded edit that was lost, and an added file are each reported.

It is the first step of an upgrade: run it, carry each listed modification onto the new
upstream, re-pin `tree_sha256` with `native-dependencies.py check`, then update the list.
"""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import importlib.util
import io
from pathlib import Path, PurePosixPath
import sys
import tarfile
from typing import Callable, Sequence
import urllib.request

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location("native_dependencies", _HERE / "native-dependencies.py")
assert _SPEC is not None and _SPEC.loader is not None
verifier = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(verifier)
DependencyError = verifier.DependencyError


def _included(relative: str, patterns: Sequence[str]) -> bool:
    """Whether `relative` matches an `include` glob segment by segment; a trailing `**` takes
    every file below its directory. Matching per segment keeps `*` inside one directory."""
    parts = relative.split("/")
    for pattern in patterns:
        pattern_parts = pattern.split("/")
        if pattern_parts[-1] == "**":
            prefix = pattern_parts[:-1]
            if len(parts) > len(prefix) and all(
                    fnmatch.fnmatchcase(part, glob) for part, glob in zip(parts, prefix)):
                return True
        elif len(parts) == len(pattern_parts) and all(
                fnmatch.fnmatchcase(part, glob) for part, glob in zip(parts, pattern_parts)):
            return True
    return False


def archive_subset(data: bytes, library: dict) -> dict[str, bytes]:
    """The pinned archive's `include` files, keyed by path below its root, after its sha256 is checked."""
    archive = library["archive"]
    actual = hashlib.sha256(data).hexdigest()
    if actual != archive["sha256"]:
        raise DependencyError("library {} archive sha256 {} does not match manifest {}".format(
            library["name"], actual, archive["sha256"]))
    root = PurePosixPath(archive["root"])
    files: dict[str, bytes] = {}
    with tarfile.open(fileobj=io.BytesIO(data)) as bundle:
        for member in bundle.getmembers():
            if not member.isfile():
                continue
            member_path = PurePosixPath(member.name)
            if member_path.parts[:len(root.parts)] != root.parts:
                raise DependencyError("library {} archive entry {} is outside its root {}".format(
                    library["name"], member.name, root))
            relative = PurePosixPath(*member_path.parts[len(root.parts):]).as_posix()
            if _included(relative, library["include"]):
                extracted = bundle.extractfile(member)
                assert extracted is not None
                files[relative] = extracted.read()
    return files


def tree_files(root: Path) -> dict[str, bytes]:
    """Every regular file under `root`, keyed by its POSIX path relative to it."""
    verifier.tree_sha256(root)  # Refuses symlinks and non-regular entries exactly as `check` does.
    return {path.relative_to(root).as_posix(): path.read_bytes()
            for path in root.rglob("*") if path.is_file()}


def modified_files(upstream: dict[str, bytes], local: dict[str, bytes]) -> list[str]:
    """Retained files that differ from the upstream subset or that it lacks, sorted."""
    return sorted(path for path, data in local.items() if upstream.get(path) != data)


def diff_problems(library: dict, upstream: dict[str, bytes], local: dict[str, bytes]) -> list[str]:
    """How the tree's actual delta from upstream disagrees with `local_modifications`."""
    actual = set(modified_files(upstream, local))
    listed = {modification["path"] for modification in library["local_modifications"]}
    name = library["name"]
    problems = ["library {} {} differs from upstream but is not in local_modifications"
                .format(name, path) for path in sorted(actual - listed)]
    problems += ["library {} lists {} in local_modifications but it matches upstream"
                 .format(name, path) for path in sorted(listed - actual)]
    return problems


def download(url: str) -> bytes:
    """Fetch `url`; the caller verifies the bytes against the manifest before reading them."""
    with urllib.request.urlopen(url, timeout=120) as response:
        return response.read()


def diff_library(repo_root: Path, library: dict, fetch: Callable[[str], bytes] = download) -> list[str]:
    """Compare one library's tree with its pinned archive; returns the disagreements."""
    upstream = archive_subset(fetch(library["archive"]["url"]), library)
    local = tree_files(repo_root / PurePosixPath(library["path"]))
    return diff_problems(library, upstream, local)


def run(repo_root: Path, selected: Sequence[dict]) -> int:
    """Report every library's delta disagreements; exit 1 if any library has one."""
    failed = False
    for library in selected:
        problems = diff_library(repo_root, library)
        for problem in problems:
            print("native-dependency-delta: {}".format(problem), file=sys.stderr)
        failed |= bool(problems)
        count = len(library["local_modifications"])
        print("{} {} {} local modification(s)".format(
            "FAIL" if problems else "ok", library["name"], count), flush=True)
    return 1 if failed else 0

def _self_tests_pass() -> bool:
    """Run `native-dependency-delta_tests.py` offline, so the gate step proves the tool before using it."""
    import unittest
    spec = importlib.util.spec_from_file_location("native_dependency_delta_tests",
                                                  _HERE / "native-dependency-delta_tests.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    tests = unittest.defaultTestLoader.loadTestsFromModule(module)
    result = unittest.TextTestRunner(stream=sys.stderr, verbosity=1).run(tests)
    # An empty suite proves nothing, so it fails rather than passes.
    return result.wasSuccessful() and result.testsRun > 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo-root", type=Path, default=_HERE.parent)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--library")
    parser.add_argument("--with-tests", action="store_true",
                        help="run this tool's offline regression tests first, as the gate step does")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.with_tests and not _self_tests_pass():
        return 1
    manifest = args.manifest or args.repo_root / "scripts" / "native-dependencies.json"
    try:
        libraries = verifier.load_manifest(manifest)
        selected = [verifier.select_library(libraries, args.library)] if args.library else libraries
        return run(args.repo_root, selected)
    except (DependencyError, OSError) as error:
        print("native-dependency-delta: {}".format(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
