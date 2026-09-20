#!/usr/bin/env python3
"""Verify and stage SonicTerm's vendored native dependencies, offline.

`check` re-derives each pinned vendor tree's digest from the working copy and compares it
with the manifest. `stage` turns a locally downloaded upstream archive into a reviewable
staging directory outside the repository. Neither subcommand reaches the network, runs
anything the archive shipped, or writes inside a vendor tree: the maintainer reviews the
staged diff and imports it by hand.

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
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Iterable, Sequence

SCHEMA_VERSION = 1
TREE_ALGORITHM = b"sonicterm-native-tree-v1"
# Bounds applied across the whole archive before any member is written to disk.
MAX_ARCHIVE_ENTRIES = 100_000
MAX_ARCHIVE_BYTES = 2 * 1024 * 1024 * 1024
PATCH_TIMEOUT_SECONDS = 120
STAGED_FILE_MODE = 0o644


class DependencyError(Exception):
    """A verification or staging failure that `main` reports as exit code 1."""


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


def file_sha256(path: Path) -> str:
    """Digest one file in bounded blocks so a large archive is not held in memory."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def glob_to_regex(pattern: str) -> re.Pattern[str]:
    """Translate an include glob where `**` spans directories and `*`/`?` stay in one segment."""
    parts: list[str] = []
    index = 0
    while index < len(pattern):
        if pattern.startswith("**", index):
            parts.append(".*")
            index += 2
        elif pattern[index] == "*":
            parts.append("[^/]*")
            index += 1
        elif pattern[index] == "?":
            parts.append("[^/]")
            index += 1
        else:
            parts.append(re.escape(pattern[index]))
            index += 1
    return re.compile("".join(parts) + r"\Z")


def _require(mapping: dict, key: str, library: str):
    if key not in mapping:
        raise DependencyError("library {} is missing required key {!r}".format(library, key))
    return mapping[key]


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
        for key in ("path", "archive", "include", "patches"):
            _require(library, key, name)
        for key in ("url", "sha256", "root"):
            _require(library["archive"], key, "{}.archive".format(name))
        if not library["include"]:
            raise DependencyError("library {} has an empty include list".format(name))
    return libraries


def select_library(libraries: Sequence[dict], name: str) -> dict:
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


def verify_patches(repo_root: Path, library: dict) -> list[Path]:
    """Confirm each recorded patch is present and byte-identical to what was reviewed."""
    resolved: list[Path] = []
    for entry in library["patches"]:
        path = repo_root / PurePosixPath(entry["path"])
        if not path.is_file():
            raise DependencyError("patch is missing: {}".format(path))
        actual = file_sha256(path)
        if actual != entry["sha256"]:
            raise DependencyError("patch {} sha256 {} does not match manifest {}".format(
                path, actual, entry["sha256"]))
        resolved.append(path)
    return resolved


def check_library(repo_root: Path, library: dict) -> None:
    """Verify one library's pinned tree, its patches, and any configured version checks."""
    expected = library.get("tree_sha256")
    if not expected:
        raise DependencyError("library {} has no pinned tree_sha256; stage it and record the "
                              "printed digest".format(library["name"]))
    tree = repo_root / PurePosixPath(library["path"])
    actual = tree_sha256(tree)
    if actual != expected:
        raise DependencyError("library {} tree_sha256 {} does not match manifest {}".format(
            library["name"], actual, expected))
    verify_patches(repo_root, library)
    verify_versions(tree, library)


def verify_versions(tree: Path, library: dict) -> None:
    """Validate both unified and split upstream version definitions."""
    configured = library.get("version_check")
    checks = [] if configured is None else (
        list(configured) if isinstance(configured, list) else [configured])
    for check in checks:
        verify_version(tree, check)


def _safe_member_path(name: str) -> PurePosixPath:
    """Reject any member name that could resolve outside the extraction root."""
    cleaned = name.rstrip("/")
    if not cleaned or cleaned.startswith("/") or "\\" in cleaned or ":" in cleaned:
        raise DependencyError("unsafe archive member path: {!r}".format(name))
    candidate = PurePosixPath(cleaned)
    if candidate.is_absolute() or any(part == ".." for part in candidate.parts):
        raise DependencyError("unsafe archive member path: {!r}".format(name))
    return candidate


def _scan_archive(archive: tarfile.TarFile, root: str) -> list[tuple[tarfile.TarInfo, str]]:
    """Validate and bound every member, then return those under the manifest's archive root."""
    seen: set[str] = set()
    under_root: list[tuple[tarfile.TarInfo, str]] = []
    entries = total = 0
    root_seen = False
    for member in archive:
        entries += 1
        if entries > MAX_ARCHIVE_ENTRIES:
            raise DependencyError("archive holds more than {} entries".format(MAX_ARCHIVE_ENTRIES))
        total += max(member.size, 0)
        if total > MAX_ARCHIVE_BYTES:
            raise DependencyError("archive exceeds {} bytes".format(MAX_ARCHIVE_BYTES))
        path = _safe_member_path(member.name)
        if not member.isdir():
            key = path.as_posix()
            if key in seen:
                raise DependencyError("duplicate archive member: {}".format(key))
            seen.add(key)
        if path.parts[0] != root:
            continue
        root_seen = True
        relative = PurePosixPath(*path.parts[1:]).as_posix()
        if relative:
            under_root.append((member, relative))
    if not root_seen:
        raise DependencyError("archive root {!r} not found in archive".format(root))
    return under_root


def extract_selected(archive_path: Path, library: dict, destination: Path) -> int:
    """Write only the included regular files, dropping upstream modes and link members."""
    patterns = [glob_to_regex(pattern) for pattern in library["include"]]
    written = 0
    with tarfile.open(archive_path, "r:*") as archive:
        for member, relative in _scan_archive(archive, library["archive"]["root"]):
            if member.isdir():
                continue
            if not any(pattern.match(relative) for pattern in patterns):
                # A release may ship links or specials we never vendor; ignoring them keeps a
                # valid upstream archive usable, and nothing outside `include` is written.
                continue
            if not member.isfile():
                raise DependencyError("selected archive member is not a regular file: {}"
                                      .format(member.name))
            target = destination / PurePosixPath(relative)
            target.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            if source is None:
                raise DependencyError("cannot read archive member: {}".format(member.name))
            with source, target.open("wb") as handle:
                shutil.copyfileobj(source, handle)
            target.chmod(STAGED_FILE_MODE)
            written += 1
    if written == 0:
        raise DependencyError("include patterns selected no files from the archive")
    return written


def _assert_patch_stays_inside(patch_path: Path) -> None:
    """Refuse a patch whose headers name a path outside the staging root."""
    targets = []
    for line in patch_path.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.startswith("--- ") or line.startswith("+++ "):
            candidate = line[4:].split("\t", 1)[0].strip()
            if candidate and candidate != "/dev/null":
                targets.append(candidate)
    if not targets:
        raise DependencyError("patch has no file headers: {}".format(patch_path))
    for target in targets:
        stripped = target
        for prefix in ("a/", "b/"):
            if stripped.startswith(prefix):
                stripped = stripped[len(prefix):]
                break
        else:
            if target.startswith("/"):
                raise DependencyError("patch {} names an absolute path {!r}".format(
                    patch_path, target))
        if stripped.startswith("/") or any(p == ".." for p in PurePosixPath(stripped).parts):
            raise DependencyError("patch {} escapes the staging root via {!r}".format(
                patch_path, target))


def apply_patches(stage: Path, patches: Iterable[Path]) -> None:
    """Apply reviewed local patches with git, never running anything the archive shipped."""
    git = shutil.which("git")
    if git is None:
        raise DependencyError("git is required to apply patches")
    environment = dict(os.environ, GIT_CONFIG_NOSYSTEM="1")
    for patch_path in patches:
        _assert_patch_stays_inside(patch_path)
        # --check first so a stale patch is reported before any staged file is touched.
        for arguments in (["apply", "--check", "-p1"], ["apply", "-p1"]):
            try:
                completed = subprocess.run([git, *arguments, str(patch_path)], cwd=str(stage),
                                           env=environment, capture_output=True, check=False,
                                           timeout=PATCH_TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                raise DependencyError("git apply timed out for {}".format(patch_path))
            if completed.returncode != 0:
                raise DependencyError("git {} failed for {}: {}".format(
                    " ".join(arguments[:-1]), patch_path,
                    completed.stderr.decode("utf-8", errors="replace").strip()))


def _prepare_output(output: Path) -> None:
    if output.is_symlink():
        raise DependencyError("output must not be a symlink: {}".format(output))
    if output.exists():
        if not output.is_dir():
            raise DependencyError("output path exists and is not a directory: {}".format(output))
        if any(output.iterdir()):
            raise DependencyError("output directory is not empty: {}".format(output))


def stage_library(repo_root: Path, library: dict, archive_path: Path, output: Path,
                  print_hash: bool) -> str:
    """Stage into scratch space and publish to `output` only once every check has passed."""
    if not archive_path.is_file():
        raise DependencyError("archive is missing: {}".format(archive_path))
    _prepare_output(output)
    actual = file_sha256(archive_path)
    if actual != library["archive"]["sha256"]:
        raise DependencyError("archive sha256 {} does not match manifest {}".format(
            actual, library["archive"]["sha256"]))
    patches = verify_patches(repo_root, library)
    # All work happens in scratch space, so a failure never leaves a partial tree at `output`
    # and the cleanup below can only remove a directory this tool created.
    scratch = Path(tempfile.mkdtemp(prefix="sonicterm-native-stage-"))
    try:
        built = scratch / "tree"
        built.mkdir()
        extract_selected(archive_path, library, built)
        apply_patches(built, patches)
        verify_versions(built, library)
        digest = tree_sha256(built)
        expected = library.get("tree_sha256")
        if expected and digest != expected:
            raise DependencyError("staged tree_sha256 {} does not match manifest {}".format(
                digest, expected))
        _prepare_output(output)
        shutil.copytree(built, output, dirs_exist_ok=True)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    if print_hash or not library.get("tree_sha256"):
        print("tree_sha256 {}".format(digest))
    return digest


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    default_root = Path(__file__).resolve().parent.parent
    # Shared by the top level and every subcommand so either position works. SUPPRESS stops
    # the subparser copy from overwriting a value given ahead of the subcommand.
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--repo-root", type=Path, default=argparse.SUPPRESS)
    common.add_argument("--manifest", type=Path, default=argparse.SUPPRESS)

    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0], parents=[common])
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check", parents=[common],
                                help="verify pinned vendor trees offline")
    check.add_argument("--library")
    stage = commands.add_parser("stage", parents=[common],
                                help="stage a local upstream archive for review")
    stage.add_argument("name")
    stage.add_argument("--archive", type=Path, required=True)
    stage.add_argument("--output", type=Path, required=True)
    stage.add_argument("--print-tree-hash", action="store_true")

    args = parser.parse_args(argv)
    args.repo_root = getattr(args, "repo_root", None) or default_root
    if getattr(args, "manifest", None) is None:
        args.manifest = args.repo_root / "scripts" / "native-dependencies.json"
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    try:
        libraries = load_manifest(args.manifest)
        if args.command == "check":
            selected = [select_library(libraries, args.library)] if args.library else libraries
            for library in selected:
                check_library(args.repo_root, library)
                print("ok {} {}".format(library["name"], library.get("version", "")).rstrip())
            return 0
        stage_library(args.repo_root, select_library(libraries, args.name), args.archive,
                      args.output, args.print_tree_hash)
        print("staged {} at {}".format(args.name, args.output))
        return 0
    except DependencyError as error:
        print("native-dependencies: {}".format(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
