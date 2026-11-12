#!/usr/bin/env python3
"""Report issue, pull-request and CI-run citations in the tracked first-party text people read.

Comments, docstrings, help and log text, and wiki pages say what the code does, why it is needed and how
it prevents a problem. Which issue, pull request or CI run led to a change belongs in the commit message,
the pull request or the issue, where it has a state; in the source it goes stale and reads as a reason.

Flagged:
- an issue or pull-request number: `#1234`, `issue 1234`, `PR 1234`, `pull request 1234`, or a link to this
  repository's `/issues/1234` or `/pull/1234` (three to five digits; `#123456` and other hex colours are not
  numbers here). A full link to another project's tracker names an upstream bug a reader can follow, so it
  is not flagged;
- a CI run, job or attempt id: `run 12345678901`, `(run 12345678901)`, `job 121555774360`, an
  `actions/runs/<id>` link, or a bare eleven- or twelve-digit id in a comment or docstring.

Scope is every tracked first-party `.rs`, `.py`, `.sh`, `.ps1`, `.yml`, `.yaml`, `.toml`, `.md`, `.wxs`,
`.swift` and `.m` file, without the vendored native trees, the retained winit subset and the generated
FreeType and HarfBuzz bindings. Markdown and non-test sources are read whole, so help text, log strings and
report headings are covered. In a test source (`*_tests.*`, `*_test_*`, `tests/`) and a fixture
(`*fixture*`), whose strings are test data, only comments and Python docstrings are read. `ALLOWED` names
each remaining exception and why it is data; an exception that no longer matches fails the check, so the
list cannot outlive its reason.

The command prints each finding as `path:line: text`, exits 1 when any remain, and exits 2 when the
tracked-file list or a file cannot be read, so an incomplete scan never passes.
"""

from __future__ import annotations

import argparse
import io
import re
import subprocess
import sys
import tokenize
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

PROGRAM = "check-history-references"
GIT_TIMEOUT_S = 60
SUFFIXES = frozenset({".rs", ".py", ".sh", ".ps1", ".yml", ".yaml", ".toml", ".md", ".wxs", ".swift", ".m"})
# Upstream trees kept as reviewed source, and the generated bindings the regeneration scripts rewrite.
EXCLUDED_PREFIXES = (
    "crates/sonicterm-freetype/freetype2/",
    "crates/sonicterm-freetype/libpng/",
    "crates/sonicterm-freetype/zlib/",
    "crates/sonicterm-harfbuzz/harfbuzz/",
    "crates/sonicterm-winit/",
)
EXCLUDED_FILES = frozenset({
    # The checker and its tests spell the forms it flags.
    "scripts/check-history-references.py",
    "scripts/check-history-references_tests.py",
    "crates/sonicterm-freetype/src/lib.rs",
    "crates/sonicterm-freetype/src/types.rs",
    "crates/sonicterm-harfbuzz/src/lib.rs",
})

# `#1234` not inside a word, a hex colour or a longer number; `&#1234;` is a character reference.
_HASH_NUMBER = r"(?<![\w&#/])#[0-9]{3,5}(?![0-9A-Fa-f])"
_NAMED_NUMBER = r"\b(?:issues?|PRs?|pull requests?|epics?)\s+#?[0-9]{3,5}\b"
_LINKED_NUMBER = r"D0n9X1n/SonicTerm/(?:issues|pull)/[0-9]{1,6}\b"
_RUN_ID = r"\b(?:runs?|jobs?|attempts?)\b[\s(]{1,2}#?[0-9]{8,}\b|actions/runs/[0-9]{6,}"
REFERENCE = re.compile("|".join((_HASH_NUMBER, _NAMED_NUMBER, _LINKED_NUMBER, _RUN_ID)), re.IGNORECASE)
# A bare CI id: eleven or twelve digits, not part of a GUID, a version or a longer number.
BARE_ID = re.compile(r"(?<![\w.-])[0-9]{11,12}(?![\w.-])")
# Comment text in each source family; Python is tokenized instead.
_SLASH_COMMENT = re.compile(r"(?<![:\"'])//+!?\s?(.*)$")
_HASH_COMMENT = re.compile(r"(?:^|\s)#(?![!\[{])\s?(.*)$")
_XML_COMMENT = re.compile(r"<!--(.*?)(?:-->|$)")
_BLOCK_LINE = re.compile(r"^\s*(?:/\*+|\*)\s?(.*)$")


@dataclass(frozen=True)
class Allowed:
    """One data exception: the file, the pattern its data matches, and why it is not a citation."""

    path: str
    pattern: str
    reason: str


ALLOWED = (
    Allowed("crates/sonicterm-text/src/start_size_inputs_recorded.rs", r"job [0-9]{12} line [0-9]+",
            "recorded atlas working sets carry the CI job and log line each row was read from; the provenance "
            "checks compare them, so they are data, not a reason"),
    Allowed("scripts/capture-multiplexer-url-wrap.py", r"SonicTerm/issues/1669#hard-wrapped",
            "the long URL the multiplexer captures print; the recorded url-wrap fixtures hold these exact bytes"),
    Allowed("CLAUDE.md", r"run\s+42970532300 took|fixes #1892", "the rule's own example of a citation to avoid"),
    Allowed("wiki/Development-and-Release.md", r"`#1234`, `issue 1234`", "the step's own example of what it flags"),
    Allowed("wiki/Development-and-Release-zh-CN.md", r"`#1234`、`issue 1234`", "the step's own example of what it flags"),
    Allowed("wiki/Release-Process.md", r"`Refs #123`", "an example of a commit keyword that does not close"),
    Allowed("wiki/Release-Process-zh-CN.md", r"`Refs #123`", "an example of a commit keyword that does not close"),
)


class ScanError(RuntimeError):
    """The tracked-file list or a tracked file cannot be read."""


@dataclass(frozen=True)
class Finding:
    """One citation: where it is and the line that holds it."""

    path: str
    line: int
    text: str


def in_scope(path: str) -> bool:
    """Whether `path` is first-party text this check reads."""
    return (PurePosixPath(path).suffix in SUFFIXES and path not in EXCLUDED_FILES
            and not path.startswith(EXCLUDED_PREFIXES))


def is_test_data(path: str) -> bool:
    """Whether `path` is a test or fixture source, whose strings are data and are not read."""
    name = PurePosixPath(path).name
    return ("_tests." in name or "_test_" in name or "fixture" in name or name.endswith("_test.rs")
            or "/tests/" in f"/{path}")


def python_text(source: str, strings: bool) -> list[tuple[int, str]]:
    """Comments and docstrings of a Python source, and every string literal when `strings` is set."""
    found: list[tuple[int, str]] = []
    previous = tokenize.NEWLINE
    tokens = tokenize.generate_tokens(io.StringIO(source).readline)
    for token in tokens:
        if token.type == tokenize.COMMENT:
            found.append((token.start[0], token.string))
        elif token.type == tokenize.STRING:
            # A string that starts a logical line is a docstring or a bare string statement: prose.
            docstring = previous in (tokenize.NEWLINE, tokenize.NL, tokenize.INDENT, tokenize.DEDENT)
            if strings or docstring:
                for offset, text in enumerate(token.string.splitlines()):
                    found.append((token.start[0] + offset, text))
        if token.type not in (tokenize.COMMENT, tokenize.NL):
            previous = token.type
    return found


def comment_text(path: str, lines: list[str]) -> list[tuple[int, str]]:
    """The comment portion of each line of a non-Python source."""
    suffix = PurePosixPath(path).suffix
    found: list[tuple[int, str]] = []
    in_block = False
    for number, line in enumerate(lines, start=1):
        if suffix in (".rs", ".swift", ".m"):
            # When: a `/* */` block is open or opens here, its lines are comment text.
            if in_block or line.lstrip().startswith("/*"):
                match = _BLOCK_LINE.match(line)
                found.append((number, match.group(1) if match else line))
                in_block = "*/" not in line
                continue
            match = _SLASH_COMMENT.search(line)
        elif suffix == ".wxs":
            match = _XML_COMMENT.search(line)
        else:
            match = _HASH_COMMENT.search(line)
        if match:
            found.append((number, match.group(1)))
    return found


def scan_file(path: str, source: str) -> list[Finding]:
    """Every citation in one tracked file, before the allow list."""
    lines = source.splitlines()
    test_data = is_test_data(path)
    if PurePosixPath(path).suffix == ".py":
        try:
            prose = python_text(source, strings=not test_data)
        except (tokenize.TokenError, IndentationError, SyntaxError) as error:
            raise ScanError(f"{path}: cannot tokenize: {error}") from error
        # Whole lines of a non-test script are also read, so a citation in code is not missed.
        whole = [] if test_data else list(enumerate(lines, start=1))
    elif PurePosixPath(path).suffix == ".md" or not test_data:
        prose = comment_text(path, lines)
        whole = list(enumerate(lines, start=1))
    else:
        prose = comment_text(path, lines)
        whole = []
    findings: dict[int, Finding] = {}
    for number, text in whole:
        if REFERENCE.search(text):
            findings[number] = Finding(path, number, lines[number - 1].strip())
    for number, text in prose:
        if REFERENCE.search(text) or BARE_ID.search(text):
            findings[number] = Finding(path, number, lines[number - 1].strip())
    return [findings[number] for number in sorted(findings)]


def apply_allowed(findings: list[Finding], allowed: tuple[Allowed, ...]) -> tuple[list[Finding], list[Allowed]]:
    """Drop findings an exception covers; return what remains and the exceptions that matched nothing."""
    used: set[Allowed] = set()
    remaining: list[Finding] = []
    for finding in findings:
        match = next((entry for entry in allowed
                      if entry.path == finding.path and re.search(entry.pattern, finding.text)), None)
        if match is None:
            remaining.append(finding)
        else:
            used.add(match)
    return remaining, [entry for entry in allowed if entry not in used]


def tracked_files(root: Path) -> list[str]:
    """The tracked files under `root`, as repository-relative POSIX paths."""
    try:
        completed = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, check=False, timeout=GIT_TIMEOUT_S)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ScanError(f"git ls-files failed: {error}") from error
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace").strip()
        raise ScanError(f"git ls-files failed: {detail or 'unknown error'}")
    try:
        return sorted(path for path in completed.stdout.decode("utf-8").split("\0") if path)
    except UnicodeDecodeError as error:
        raise ScanError(f"git ls-files returned a non-UTF-8 path: {error}") from error


def scan(root: Path) -> tuple[list[Finding], list[Allowed], int]:
    """Scan every in-scope tracked file; return findings, unused exceptions and the file count."""
    findings: list[Finding] = []
    paths = [path for path in tracked_files(root) if in_scope(path)]
    for path in paths:
        try:
            source = (root / path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            raise ScanError(f"{path}: cannot read: {error}") from error
        findings.extend(scan_file(path, source))
    remaining, unused = apply_allowed(findings, ALLOWED)
    return remaining, unused, len(paths)


def main(argv: list[str] | None = None) -> int:
    """Run the check over a repository root."""
    parser = argparse.ArgumentParser(prog=PROGRAM, description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args(argv)
    try:
        findings, unused, file_count = scan(args.root)
    except ScanError as error:
        print(f"{PROGRAM}: {error}", file=sys.stderr)
        return 2
    for finding in findings:
        print(f"{finding.path}:{finding.line}: {finding.text}")
    for entry in unused:
        print(f"{entry.path}: allowed pattern {entry.pattern!r} matches nothing; remove it ({entry.reason})")
    if findings or unused:
        print(f"{PROGRAM}: FAIL; {len(findings)} citation(s), {len(unused)} unused exception(s). Say why the "
              "code is needed and how it prevents the problem; keep issue, PR and run numbers in the commit, "
              "pull request or issue.", file=sys.stderr)
        return 1
    print(f"{PROGRAM}: PASS; {file_count} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
