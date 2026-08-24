#!/usr/bin/env python3
"""Validate the tracked bilingual Markdown wiki source."""

from __future__ import annotations

import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
from urllib.parse import unquote, urlsplit

ENGLISH_MARKER = "## English"
CHINESE_MARKER = "## 中文"
HEADING_PATTERN = re.compile(r"^(#{1,6})[ \t]+")
LINK_PATTERN = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
FENCE_PATTERN = re.compile(r"^[ \t]{0,3}(`{3,}|~{3,})")
EXTERNAL_SCHEMES = frozenset(
    {
        "data",
        "ftp",
        "ftps",
        "http",
        "https",
        "irc",
        "ircs",
        "mailto",
        "news",
        "ssh",
        "tel",
    }
)


def repository_root() -> Path:
    """Return the repository root containing this checker."""
    return Path(__file__).resolve().parent.parent


def tracked_wiki_paths(root: Path) -> list[PurePosixPath]:
    """Return tracked wiki files in deterministic repository-relative order."""
    completed = subprocess.run(
        ["git", "ls-files", "--", "wiki/**"],
        capture_output=True,
        check=False,
        cwd=root,
    )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"git ls-files failed: {detail or 'unknown error'}")
    return sorted(
        PurePosixPath(line)
        for line in completed.stdout.decode("utf-8").splitlines()
        if line
    )


def split_language_halves(
    path: PurePosixPath, lines: list[str], errors: list[str]
) -> tuple[list[str], list[str]] | None:
    """Split a page after enforcing its exact ordered language markers."""
    english = [index for index, line in enumerate(lines) if line == ENGLISH_MARKER]
    chinese = [index for index, line in enumerate(lines) if line == CHINESE_MARKER]
    if len(english) != 1:
        errors.append(
            f"{path}: expected exactly one {ENGLISH_MARKER!r} marker; found {len(english)}"
        )
    if len(chinese) != 1:
        errors.append(
            f"{path}: expected exactly one {CHINESE_MARKER!r} marker; found {len(chinese)}"
        )
    if len(english) != 1 or len(chinese) != 1:
        return None
    if english[0] >= chinese[0]:
        errors.append(f"{path}: {ENGLISH_MARKER!r} must precede {CHINESE_MARKER!r}")
        return None
    return lines[english[0] + 1 : chinese[0]], lines[chinese[0] + 1 :]


def heading_depths(lines: list[str]) -> list[int]:
    """Return heading depths outside fenced code blocks in source order."""
    depths: list[int] = []
    fence: str | None = None
    fence_length = 0
    for line in lines:
        marker = FENCE_PATTERN.match(line)
        if marker:
            run = marker.group(1)
            if fence is None:
                fence = run[0]
                fence_length = len(run)
            elif run[0] == fence and len(run) >= fence_length:
                fence = None
                fence_length = 0
            continue
        if fence is None and (heading := HEADING_PATTERN.match(line)):
            depths.append(len(heading.group(1)))
    return depths


def link_targets(lines: list[str]) -> list[tuple[int, str]]:
    """Return inline Markdown link destinations outside fenced code blocks."""
    links: list[tuple[int, str]] = []
    fence: str | None = None
    fence_length = 0
    for line_number, line in enumerate(lines, start=1):
        marker = FENCE_PATTERN.match(line)
        if marker:
            run = marker.group(1)
            if fence is None:
                fence = run[0]
                fence_length = len(run)
            elif run[0] == fence and len(run) >= fence_length:
                fence = None
                fence_length = 0
            continue
        if fence is not None:
            continue
        for match in LINK_PATTERN.finditer(line):
            destination = match.group(1).strip()
            if destination.startswith("<") and destination.endswith(">"):
                destination = destination[1:-1].strip()
            links.append((line_number, destination))
    return links


def validate_links(
    path: PurePosixPath,
    lines: list[str],
    page_stems: set[str],
    errors: list[str],
) -> None:
    """Validate bare cross-page links while allowing local anchors and URLs."""
    for line_number, raw_target in link_targets(lines):
        target = unquote(raw_target)
        if target.startswith("#"):
            continue
        parsed = urlsplit(target)
        if parsed.scheme.lower() in EXTERNAL_SCHEMES or parsed.netloc:
            continue
        page_target = parsed.path
        if page_target.endswith(".md"):
            errors.append(
                f"{path}:{line_number}: cross-page link must omit .md: {raw_target}"
            )
            continue
        if page_target not in page_stems:
            errors.append(
                f"{path}:{line_number}: cross-page link target does not exist: {raw_target}"
            )


def workspace_package_names(root: Path) -> tuple[list[str], str | None]:
    """Read workspace package names from Cargo's resolved workspace metadata."""
    completed = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        capture_output=True,
        check=False,
        cwd=root,
    )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace").strip()
        return [], f"cargo metadata failed: {detail or 'unknown error'}"
    try:
        metadata = json.loads(completed.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        return [], f"cargo metadata returned invalid UTF-8 JSON: {error}"
    workspace_members = set(metadata["workspace_members"])
    names = sorted(
        package["name"]
        for package in metadata["packages"]
        if package["id"] in workspace_members
    )
    return names, None


def validate_home_links(
    halves: tuple[list[str], list[str]] | None,
    other_stems: set[str],
    errors: list[str],
) -> None:
    """Require each Home language half to navigate to every other wiki page."""
    if halves is None:
        return
    for language, lines in zip(("English", "中文"), halves, strict=True):
        linked = {
            unquote(urlsplit(target).path)
            for _, target in link_targets(lines)
            if not target.startswith("#")
        }
        for stem in sorted(other_stems - linked):
            errors.append(f"wiki/Home.md: {language} half is missing link to {stem}")


def validate_crate_reference(
    halves: tuple[list[str], list[str]] | None,
    package_names: list[str],
    errors: list[str],
) -> None:
    """Require every workspace package name in each Crate Reference half."""
    if halves is None:
        return
    for language, lines in zip(("English", "中文"), halves, strict=True):
        text = "\n".join(lines)
        for name in package_names:
            if not re.search(rf"(?<![\w-]){re.escape(name)}(?![\w-])", text):
                errors.append(
                    f"wiki/Crate-Reference.md: {language} half is missing workspace crate {name}"
                )


def main() -> int:
    """Validate wiki layout, bilingual structure, links, navigation, and crates."""
    root = repository_root()
    errors: list[str] = []
    try:
        tracked = tracked_wiki_paths(root)
    except RuntimeError as error:
        print(f"check-wiki: {error}", file=sys.stderr)
        return 1

    nested = [
        path for path in tracked if path.suffix == ".md" and len(path.parts) != 2
    ]
    for path in nested:
        errors.append(f"{path}: nested Markdown pages are not allowed")

    pages = [
        path
        for path in tracked
        if path.suffix == ".md" and len(path.parts) == 2
    ]
    if not pages:
        errors.append("wiki: no tracked Markdown pages found")

    page_stems = {path.stem for path in pages}
    halves_by_path: dict[PurePosixPath, tuple[list[str], list[str]] | None] = {}
    for path in pages:
        lines = (root / path).read_text(encoding="utf-8").splitlines()
        halves = split_language_halves(path, lines, errors)
        halves_by_path[path] = halves
        if halves is not None:
            english_depths = heading_depths(halves[0])
            chinese_depths = heading_depths(halves[1])
            if english_depths != chinese_depths:
                errors.append(
                    f"{path}: heading-depth sequences differ: "
                    f"English {english_depths}; 中文 {chinese_depths}"
                )
        validate_links(path, lines, page_stems, errors)

    home = PurePosixPath("wiki/Home.md")
    if home not in halves_by_path:
        errors.append(f"{home}: required page is missing")
    else:
        validate_home_links(halves_by_path[home], page_stems - {"Home"}, errors)

    crate_reference = PurePosixPath("wiki/Crate-Reference.md")
    if crate_reference not in halves_by_path:
        errors.append(f"{crate_reference}: required page is missing")
    else:
        package_names, metadata_error = workspace_package_names(root)
        if metadata_error is not None:
            errors.append(f"{crate_reference}: {metadata_error}")
        else:
            validate_crate_reference(
                halves_by_path[crate_reference], package_names, errors
            )

    if errors:
        for error in sorted(errors):
            print(f"check-wiki: {error}", file=sys.stderr)
        return 1
    print(f"check-wiki: ok ({len(pages)} pages)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
