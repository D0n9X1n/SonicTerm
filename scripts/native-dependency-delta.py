#!/usr/bin/env python3
"""Compare SonicTerm's vendored native trees with their pinned upstream archives.

This reaches the network, unlike `native-dependencies.py check`: for each library it downloads
the pinned archive, verifies its sha256 against the manifest, reads the `include` subset in
memory (nothing is written to disk) and lists every retained file that differs from it or that
the archive lacks. That set must equal the library's `local_modifications`, so an unrecorded
edit, a recorded edit that was lost, and an added file are each reported.

It is the first step of an upgrade: run it, carry each listed modification onto the new
upstream, re-pin `tree_sha256` with `native-dependencies.py check`, then update the list.

`--latest` reports instead whether a newer stable upstream release exists: it lists each
library's `latest_source` tags with one `git ls-remote` whose process tree is killed at the deadline, keeps those its stable-version
pattern matches, and prints pinned against latest. It always exits 0 once the manifest loads,
because a newer release is a prompt to review, not a defect, and a failed lookup is reported
per library.
"""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import importlib.util
import io
import os
from pathlib import Path, PurePosixPath
import re
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
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

# The one upstream index provider: tag names from `git ls-remote --tags`, which every manifest
# library's repository answers without an API token or pagination.
LATEST_PROVIDERS = ("git-tags",)
# Seconds and bytes one listing may take; libpng's ~100 KB of tags is the largest today.
LATEST_TIMEOUT_S = 60
LATEST_MAX_BYTES = 1 << 20
# How often a running listing's deadline and output size are checked.
LATEST_POLL_S = 0.05


def latest_source(library: dict) -> dict:
    """The library's validated `latest_source`: a known provider, a repository URL, and a
    pattern whose numeric groups spell a stable version (pre-release tags must not match)."""
    name = library["name"]
    source = library.get("latest_source")
    if not isinstance(source, dict) or set(source) != {"provider", "url", "pattern"}:
        raise DependencyError("library {} latest_source must hold exactly provider, url and "
                              "pattern".format(name))
    if source["provider"] not in LATEST_PROVIDERS:
        raise DependencyError("library {} latest_source provider {!r} is not one of {}".format(
            name, source["provider"], ", ".join(LATEST_PROVIDERS)))
    try:
        pattern = re.compile(source["pattern"])
    except (re.error, TypeError) as error:
        raise DependencyError("library {} latest_source pattern is invalid: {}".format(name, error))
    if pattern.groups < 1:
        raise DependencyError("library {} latest_source pattern captures no version "
                              "component".format(name))
    pinned = stable_version(pattern, library.get("tag") or "")
    if pinned is None or ".".join(map(str, pinned)) != library["version"]:
        # The pinned tag must parse back to the pinned version, or a comparison is meaningless.
        raise DependencyError("library {} latest_source pattern does not read tag {!r} as "
                              "version {}".format(name, library.get("tag"), library["version"]))
    return source


def stable_version(pattern: re.Pattern, tag: str) -> tuple[int, ...] | None:
    """`tag`'s version as integers when `pattern` matches it whole, else None. An optional
    group that did not take part (zlib's `v1.4` has no patch number) is left out."""
    match = pattern.fullmatch(tag)
    if not match:
        return None
    return tuple(int(group) for group in match.groups() if group is not None)


def _comparable(version: tuple[int, ...]) -> tuple[int, ...]:
    """`version` padded with zeros, so `1.4` and `1.4.0` compare equal and `1.4` beats `1.3.2`."""
    return version + (0,) * (8 - len(version))


def _kill_tree(process: subprocess.Popen) -> None:
    """End `process` and every helper it started: its whole process group on POSIX, its tree on
    Windows, so a transport helper cannot outlive the deadline."""
    try:
        if os.name == "nt":
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(process.pid)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        else:
            os.killpg(process.pid, signal.SIGKILL)
    except (OSError, subprocess.TimeoutExpired):
        # The group already ended, or taskkill could not run; kill the parent at least.
        process.kill()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass


def _written(output) -> int:
    """Bytes a child has written to the temporary file `output`, read from the file itself, since
    the parent's own position does not move when the child writes."""
    return os.fstat(output.fileno()).st_size


def list_tags(url: str) -> list[str]:
    """Tag names at `url`, read from one `git ls-remote` whose whole process tree is killed at
    `LATEST_TIMEOUT_S`. Output goes to a file, not a pipe, so a helper still holding it cannot
    block a read; more than `LATEST_MAX_BYTES` on either stream ends the tree while it runs."""
    environment = dict(os.environ, GIT_TERMINAL_PROMPT="0")
    # A new session (POSIX) or process group (Windows) makes every helper git starts killable.
    group = ({"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == "nt"
             else {"start_new_session": True})
    with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
        process = subprocess.Popen(["git", "ls-remote", "--tags", "--refs", url], stdout=stdout_file,
                                   stderr=stderr_file, env=environment, **group)
        # The files are checked while the tree runs, so neither the deadline nor the size cap waits
        # for git to finish: either one ends the whole tree at once.
        deadline = time.monotonic() + LATEST_TIMEOUT_S
        while process.poll() is None:
            if _written(stdout_file) > LATEST_MAX_BYTES or _written(stderr_file) > LATEST_MAX_BYTES:
                _kill_tree(process)
                raise DependencyError("git ls-remote {} answered more than {} bytes".format(
                    url, LATEST_MAX_BYTES))
            if time.monotonic() >= deadline:
                _kill_tree(process)
                raise DependencyError("git ls-remote {} took over {} s".format(url, LATEST_TIMEOUT_S))
            time.sleep(LATEST_POLL_S)
        # Helpers git left behind are ended too; they could not have added to a finished listing.
        # A child that finished between two polls is held to the same cap on either stream.
        _kill_tree(process)
        if _written(stdout_file) > LATEST_MAX_BYTES or _written(stderr_file) > LATEST_MAX_BYTES:
            raise DependencyError("git ls-remote {} answered more than {} bytes".format(
                url, LATEST_MAX_BYTES))
        stdout_file.seek(0)
        listing = stdout_file.read(LATEST_MAX_BYTES + 1)
        stderr_file.seek(0)
        stderr = stderr_file.read(4096).decode("utf-8", "replace").strip()
    if process.returncode != 0:
        last_line = stderr.splitlines()[-1] if stderr else "exit {}".format(process.returncode)
        raise DependencyError("git ls-remote {} failed: {}".format(url, last_line))
    lines = listing.decode("utf-8", "replace").splitlines()
    return [line.split("refs/tags/", 1)[1] for line in lines if "refs/tags/" in line]


def latest_line(library: dict, list_tags_at: Callable[[str], list[str]] = list_tags) -> str:
    """One report line for `library`: its pinned version against the newest stable upstream tag,
    or the reason the lookup failed. Never raises for a lookup failure."""
    name = library["name"]
    count = len(library["local_modifications"])
    try:
        source = latest_source(library)
        pattern = re.compile(source["pattern"])
        versions = [(version, tag) for tag in list_tags_at(source["url"])
                    if (version := stable_version(pattern, tag)) is not None]
        if not versions:
            raise DependencyError("no tag at {} matches {}".format(source["url"], source["pattern"]))
    except (DependencyError, OSError) as error:
        return "error {}: {}".format(name, error)
    newest, tag = max(versions, key=lambda entry: (_comparable(entry[0]), entry[1]))
    pinned = _comparable(tuple(int(part) for part in library["version"].split(".")))
    latest = ".".join(map(str, newest))
    if _comparable(newest) > pinned:
        state = "update"
    elif _comparable(newest) == pinned:
        state = "current"
    else:
        # When: the pinned version is newer than every stable tag (a tag was deleted or renamed).
        state = "ahead"
    return "{} {} pinned {} latest {} (tag {}), {} local modification(s)".format(
        state, name, library["version"], latest, tag, count)


def report_latest(selected: Sequence[dict],
                  list_tags_at: Callable[[str], list[str]] = list_tags) -> int:
    """Print one `latest_line` per library; report only, so it returns 0."""
    for library in selected:
        print(latest_line(library, list_tags_at), flush=True)
    return 0


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
    parser.add_argument("--latest", action="store_true",
                        help="report each library's newest stable upstream release; exits 0")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.with_tests and not _self_tests_pass():
        return 1
    manifest = args.manifest or args.repo_root / "scripts" / "native-dependencies.json"
    try:
        libraries = verifier.load_manifest(manifest)
        selected = [verifier.select_library(libraries, args.library)] if args.library else libraries
        return report_latest(selected) if args.latest else run(args.repo_root, selected)
    except (DependencyError, OSError) as error:
        print("native-dependency-delta: {}".format(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
