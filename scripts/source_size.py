"""Count tracked first-party source bytes with exact, audited native and ABI exclusions."""

from __future__ import annotations

import argparse
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys

LIMIT = 1000
EXTENSIONS = frozenset({".rs", ".py", ".sh", ".ps1", ".c", ".h", ".cc", ".cpp", ".hpp", ".m", ".mm", ".wgsl"})
NATIVE_NAMES = frozenset({"freetype", "harfbuzz", "libpng", "zlib", "winit"})
EXACT_EXCEPTIONS = {
    "crates/sonicterm-freetype/src/lib.rs": "generated FreeType bindings",
    "crates/sonicterm-freetype/src/types.rs": "generated FreeType types",
    "crates/sonicterm-harfbuzz/src/lib.rs": "generated HarfBuzz bindings",
}


def physical_lines(data: bytes) -> int:
    """Count LF boundaries and a final unterminated line; CRLF and Unicode need no decoding."""
    return data.count(b"\n") + int(bool(data) and not data.endswith(b"\n"))


def native_roots(manifest: dict) -> tuple[str, ...]:
    """Read exact imported roots, rejecting ambiguous or broadened manifest entries."""
    libraries = manifest["libraries"]
    if len(libraries) != len(NATIVE_NAMES) or {item["name"] for item in libraries} != NATIVE_NAMES:
        raise ValueError("native dependency names changed; review the source-size exclusion policy")
    roots = []
    for item in libraries:
        path = item["path"]
        parts = PurePosixPath(path).parts
        if len(parts) < 2 or parts[0] != "crates" or any(part in ("", ".", "..") for part in path.split("/")):
            raise ValueError(f"unsafe native exclusion root: {path!r}")
        if item["name"] == "winit":
            if path != "crates/sonicterm-winit":
                raise ValueError("winit exclusion must name its retained upstream tree")
        elif len(parts) < 3:
            raise ValueError(f"native exclusion cannot hide a first-party crate: {path}")
        roots.append(path + "/")
    return tuple(sorted(roots))


def tracked_paths(root: Path) -> list[str]:
    """Discover tracked paths without search-tool ignore rules or whitespace splitting."""
    completed = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], check=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
    return sorted(set(part.decode("utf-8", "surrogateescape") for part in completed.stdout.split(b"\0") if part))


def inspect_sources(root: Path, paths: list[str], roots: tuple[str, ...],
                    exceptions: dict[str, str] | None = None) -> tuple[list[tuple[str, int]], list[str]]:
    """Return every measured source and failure; unreadable or stale exclusions fail closed."""
    exceptions = EXACT_EXCEPTIONS if exceptions is None else exceptions
    tracked = set(paths)
    problems = [f"stale exact exception: {path}" for path in exceptions if path not in tracked or not (root / path).is_file()]
    problems.extend(f"stale native root: {prefix}" for prefix in roots if not any(path.startswith(prefix) for path in paths))
    measured = []
    for path in paths:
        if path in exceptions or path.startswith(roots) or PurePosixPath(path).suffix not in EXTENSIONS:
            continue
        try:
            count = physical_lines((root / path).read_bytes())
        except OSError as error:
            problems.append(f"cannot read {path}: {error}")
            continue
        measured.append((path, count))
        if count > LIMIT:
            problems.append(f"{path}: {count} lines exceeds {LIMIT}")
    if not measured:
        problems.append("no first-party sources found")
    return measured, problems


def main(argv: list[str] | None = None) -> int:
    """Print every violation, the maximum source size and exact exclusion reasons."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args(argv)
    try:
        manifest = json.loads((args.root / "scripts/native-dependencies.json").read_text(encoding="utf-8"))
        roots = native_roots(manifest)
        measured, problems = inspect_sources(args.root, tracked_paths(args.root), roots)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"source-size: cannot inspect sources: {error}", file=sys.stderr)
        return 2
    for problem in problems:
        print(f"source-size: {problem}", file=sys.stderr)
    for prefix in roots:
        print(f"excluded native tree: {prefix}")
    for path, reason in EXACT_EXCEPTIONS.items():
        print(f"excluded exact source: {path} ({reason})")
    if measured:
        path, count = max(measured, key=lambda entry: (entry[1], entry[0]))
        print(f"source-size: {'FAIL' if problems else 'PASS'}; {len(measured)} sources; maximum {count}: {path}")
    return int(bool(problems))
