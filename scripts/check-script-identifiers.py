#!/usr/bin/env python3
"""Report short identifiers bound in the tracked first-party Python scripts.

This is the script half of the naming rule; Clippy's `min_ident_chars` is the
Rust half. A bound name is too short when it is one character, one letter
followed by digits, or two letters, unless it is `_`, starts with `_`, is `self`
or `cls`, or is in the `allowed-idents-below-min-chars` list that Clippy reads
from clippy.toml. The bindings checked are assignment targets (plain,
augmented, annotated and `:=`), `for` and comprehension targets, `with ... as`
and `except ... as` names, function and lambda parameters, function and class
names, and import aliases. Attribute and subscript targets bind no local name.

The command scans the tracked `scripts/*.py` files, prints each finding as
`path:line name`, and exits 1 when any remain. It exits 2 when clippy.toml, the
tracked-file list or a script cannot be read or parsed, so an incomplete scan
never passes. It runs on Python 3.9, which has no tomllib, so a small scanner
reads that one clippy.toml key.
"""

from __future__ import annotations

import argparse
import ast
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
from typing import Iterator

PROGRAM = "check-script-identifiers"
CONFIG_FILE = "clippy.toml"
ALLOWLIST_KEY = "allowed-idents-below-min-chars"
SCRIPTS_DIRECTORY = "scripts"
GIT_TIMEOUT_S = 60
EXEMPT_NAMES = frozenset({"self", "cls"})
# One letter followed by digits, such as `p1`; `sha256` and `kernel32` are descriptive names.
_LETTER_DIGITS = re.compile(r"[^\W\d_]\d+")

_BARE_KEY = re.compile(r"[A-Za-z0-9_-]+")
# Booleans, numbers and dates: every TOML value that is not a string, an array or an inline table.
_SCALAR = re.compile(r"[A-Za-z0-9_:.+-]+")
_ESCAPES = {"b": "\b", "t": "\t", "n": "\n", "f": "\f", "r": "\r", '"': '"', "\\": "\\"}
_QUOTES = ('"', "'")


class ConfigError(ValueError):
    """clippy.toml does not hold one readable top-level allowlist."""


class ScanError(RuntimeError):
    """The tracked script list cannot be read."""


class _Scanner:
    """Walk just enough TOML to find one top-level key, skipping every other value whole."""

    def __init__(self, text: str) -> None:
        self.text = text.replace("\r\n", "\n")
        if self.text.startswith("﻿"):
            self.text = self.text[1:]
        self.position = 0

    def fail(self, message: str) -> ConfigError:
        """Return a ConfigError that names the current line."""
        line = self.text.count("\n", 0, self.position) + 1
        return ConfigError(f"{CONFIG_FILE}:{line}: {message}")

    def peek(self, width: int = 1) -> str:
        """Return the next `width` characters without consuming them."""
        return self.text[self.position:self.position + width]

    def skip_blanks(self, newlines: bool) -> None:
        """Skip spaces, tabs and comments, and newlines too when `newlines` is set."""
        while self.position < len(self.text):
            char = self.text[self.position]
            if char in " \t" or (newlines and char == "\n"):
                self.position += 1
            elif char == "#":
                end = self.text.find("\n", self.position)
                self.position = len(self.text) if end < 0 else end
            else:
                return

    def expect_line_end(self) -> None:
        """Require that only a comment follows a key-value pair on its line."""
        self.skip_blanks(newlines=False)
        if self.position < len(self.text) and self.peek() != "\n":
            raise self.fail("expected the end of the line after a value")

    def key(self) -> tuple[str, ...]:
        """Read a bare, quoted or dotted key as its segments."""
        segments = []
        while True:
            self.skip_blanks(newlines=False)
            if self.peek() in _QUOTES:
                segments.append(self.string())
            else:
                bare = _BARE_KEY.match(self.text, self.position)
                if bare is None:
                    raise self.fail("expected a key")
                segments.append(bare.group())
                self.position = bare.end()
            self.skip_blanks(newlines=False)
            if self.peek() != ".":
                return tuple(segments)
            self.position += 1

    def string(self) -> str:
        """Read one single-line basic or literal string and return its value."""
        quote = self.peek()
        if self.peek(3) == quote * 3:
            raise self.fail("expected a single-line string")
        self.position += 1
        characters = []
        while self.position < len(self.text):
            char = self.text[self.position]
            self.position += 1
            if char == quote:
                return "".join(characters)
            if char == "\n":
                break
            # Only basic strings have escapes; a literal string keeps its backslashes.
            if char == "\\" and quote == '"':
                characters.append(self.escape())
            else:
                characters.append(char)
        raise self.fail("unterminated string")

    def escape(self) -> str:
        """Decode the basic-string escape that follows a backslash."""
        code = self.peek()
        self.position += 1
        if code in _ESCAPES:
            return _ESCAPES[code]
        width = {"u": 4, "U": 8}.get(code)
        digits = self.peek(width or 0)
        if width is None or len(digits) != width or any(char not in "0123456789abcdefABCDEF" for char in digits):
            raise self.fail(f"invalid escape \\{code}")
        self.position += width
        codepoint = int(digits, 16)
        if codepoint > 0x10FFFF or 0xD800 <= codepoint <= 0xDFFF:
            raise self.fail(f"invalid escape \\{code}{digits}")
        return chr(codepoint)

    def skip_multiline_string(self) -> None:
        """Skip a multi-line string; its content never matters to the allowlist."""
        quote = self.peek()
        self.position += 3
        while self.position < len(self.text):
            if quote == '"' and self.peek() == "\\":
                self.position += 2
            elif self.peek(3) == quote * 3:
                self.position += 3
                # A run of four or five quotes also closes the string; the extra quotes are content.
                for _ in range(2):
                    if self.peek() == quote:
                        self.position += 1
                return
            else:
                self.position += 1
        raise self.fail("unterminated multi-line string")

    def skip_value(self) -> None:
        """Skip one value of any kind: a string, an array, an inline table or a scalar."""
        char = self.peek()
        if char in _QUOTES:
            if self.peek(3) == char * 3:
                self.skip_multiline_string()
            else:
                self.string()
        elif char == "[":
            self.position += 1
            while True:
                self.skip_blanks(newlines=True)
                if self.peek() == "]":
                    self.position += 1
                    return
                self.skip_value()
                self.skip_blanks(newlines=True)
                if self.peek() == ",":
                    self.position += 1
                elif self.peek() != "]":
                    raise self.fail("expected ',' or ']' in an array")
        elif char == "{":
            self.position += 1
            self.skip_blanks(newlines=False)
            if self.peek() == "}":
                self.position += 1
                return
            while True:
                self.key()
                if self.peek() != "=":
                    raise self.fail("expected '=' in an inline table")
                self.position += 1
                self.skip_blanks(newlines=False)
                self.skip_value()
                self.skip_blanks(newlines=False)
                if self.peek() == "}":
                    self.position += 1
                    return
                if self.peek() != ",":
                    raise self.fail("expected ',' or '}' in an inline table")
                self.position += 1
        else:
            scalar = _SCALAR.match(self.text, self.position)
            if scalar is None:
                raise self.fail("expected a value")
            self.position = scalar.end()

    def string_array(self) -> frozenset[str]:
        """Read the allowlist value, which must be an array of single-line strings."""
        if self.peek() != "[":
            raise self.fail(f"{ALLOWLIST_KEY} must be an array of strings")
        self.position += 1
        names = set()
        while True:
            self.skip_blanks(newlines=True)
            if self.peek() == "]":
                self.position += 1
                return frozenset(names)
            if self.peek() not in _QUOTES:
                raise self.fail(f"{ALLOWLIST_KEY} must hold only strings")
            name = self.string()
            # ".." would append Clippy's default list, which this checker cannot see.
            if name == "..":
                raise self.fail('".." appends Clippy\'s default list and is not supported')
            names.add(name)
            self.skip_blanks(newlines=True)
            if self.peek() == ",":
                self.position += 1
            elif self.peek() != "]":
                raise self.fail(f"expected ',' or ']' in {ALLOWLIST_KEY}")

    def allowlist(self) -> frozenset[str]:
        """Return the one top-level allowlist, stopping at the first table header."""
        found = None
        while True:
            self.skip_blanks(newlines=True)
            # A table header ends the top level, so later keys belong to that table.
            if self.position >= len(self.text) or self.peek() == "[":
                break
            key = self.key()
            if self.peek() != "=":
                raise self.fail("expected '=' after a key")
            self.position += 1
            self.skip_blanks(newlines=False)
            if key == (ALLOWLIST_KEY,):
                if found is not None:
                    raise self.fail(f"{ALLOWLIST_KEY} is set twice")
                found = self.string_array()
            else:
                self.skip_value()
            self.expect_line_end()
        if found is None:
            raise ConfigError(f"{CONFIG_FILE}: no top-level {ALLOWLIST_KEY} list")
        return found


def read_allowlist(text: str) -> frozenset[str]:
    """Return the names in clippy.toml's top-level `allowed-idents-below-min-chars` list."""
    return _Scanner(text).allowlist()


def is_short(name: str, allowed: frozenset[str]) -> bool:
    """Return whether a bound name is one character, one letter followed by digits, or two letters."""
    if name in allowed or name in EXEMPT_NAMES or name.startswith("_"):
        return False
    return len(name) == 1 or (len(name) == 2 and name.isalpha()) or _LETTER_DIGITS.fullmatch(name) is not None


def _target_names(target: ast.expr) -> Iterator[tuple[int, str]]:
    """Yield the names an assignment target binds; attribute and subscript targets bind none."""
    if isinstance(target, ast.Name):
        yield target.lineno, target.id
    elif isinstance(target, (ast.Tuple, ast.List)):
        for element in target.elts:
            yield from _target_names(element)
    elif isinstance(target, ast.Starred):
        yield from _target_names(target.value)


def _parameters(arguments: ast.arguments) -> Iterator[ast.arg]:
    """Yield every parameter of a function or lambda, including `*args` and `**kwargs`."""
    yield from arguments.posonlyargs
    yield from arguments.args
    if arguments.vararg is not None:
        yield arguments.vararg
    yield from arguments.kwonlyargs
    if arguments.kwarg is not None:
        yield arguments.kwarg


def binding_sites(tree: ast.AST) -> Iterator[tuple[int, str]]:
    """Yield `(line, name)` for every binding the naming rule covers."""
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            for target in node.targets:
                yield from _target_names(target)
        elif isinstance(node, (ast.AugAssign, ast.AnnAssign, ast.NamedExpr, ast.For, ast.AsyncFor,
                               ast.comprehension)):
            yield from _target_names(node.target)
        elif isinstance(node, (ast.With, ast.AsyncWith)):
            for item in node.items:
                if item.optional_vars is not None:
                    yield from _target_names(item.optional_vars)
        elif isinstance(node, ast.ExceptHandler):
            if node.name is not None:
                yield node.lineno, node.name
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            # Python 3.9 aliases carry no position, so an alias reports its statement's line.
            for alias in node.names:
                if alias.asname is not None:
                    yield node.lineno, alias.asname
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            yield node.lineno, node.name
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            for parameter in _parameters(node.args):
                yield parameter.lineno, parameter.arg


def short_bindings(source: str | bytes, allowed: frozenset[str], filename: str = "<script>") -> list[tuple[int, str]]:
    """Parse one script and return its short bindings as sorted, distinct `(line, name)` pairs."""
    tree = ast.parse(source, filename=filename)
    return sorted({(line, name) for line, name in binding_sites(tree) if is_short(name, allowed)})


def tracked_scripts(root: Path) -> list[str]:
    """Return the tracked `.py` files directly under scripts/, as repository-relative paths."""
    try:
        completed = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z", "--", f"{SCRIPTS_DIRECTORY}/*.py"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=GIT_TIMEOUT_S,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ScanError(f"git ls-files failed: {error}") from error
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace").strip()
        raise ScanError(f"git ls-files failed: {detail or 'unknown error'}")
    try:
        listed = completed.stdout.decode("utf-8").split("\0")
    except UnicodeDecodeError as error:
        raise ScanError(f"git ls-files returned a non-UTF-8 path: {error}") from error
    # A pathspec `*` also matches `/`, so keep only the direct children of scripts/.
    directory = PurePosixPath(SCRIPTS_DIRECTORY)
    return sorted(path for path in listed if path and PurePosixPath(path).parent == directory)


def _plural(count: int, noun: str) -> str:
    """Return `count noun`, adding an s to the noun unless the count is one."""
    return f"{count} {noun}" + ("" if count == 1 else "s")


def main(argv: list[str] | None = None) -> int:
    """Scan the tracked scripts; return 0 when clean, 1 for findings and 2 for an incomplete scan."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent,
                        help="repository root (default: this checkout)")
    arguments = parser.parse_args(argv)
    root = arguments.root
    try:
        allowed = read_allowlist((root / CONFIG_FILE).read_text(encoding="utf-8"))
        scripts = tracked_scripts(root)
    except (OSError, UnicodeDecodeError, ConfigError, ScanError) as error:
        print(f"{PROGRAM}: {error}", file=sys.stderr)
        return 2
    # An empty list means a wrong root or checkout, not a clean tree, so it fails closed.
    if not scripts:
        print(f"{PROGRAM}: no tracked scripts under {SCRIPTS_DIRECTORY}/; nothing was checked", file=sys.stderr)
        return 2
    findings = []
    problems = []
    for path in scripts:
        try:
            pairs = short_bindings((root / path).read_bytes(), allowed, filename=path)
        except SyntaxError as error:
            location = f":{error.lineno}" if error.lineno else ""
            problems.append(f"{path}{location}: cannot parse: {error.msg}")
            continue
        except (OSError, ValueError) as error:
            problems.append(f"{path}: cannot read: {error}")
            continue
        findings.extend(f"{path}:{line} {name}" for line, name in pairs)
    for finding in findings:
        print(finding)
    for problem in problems:
        print(f"{PROGRAM}: {problem}", file=sys.stderr)
    if problems:
        return 2
    if findings:
        print(f"{PROGRAM}: {_plural(len(findings), 'short identifier')} in tracked scripts; "
              "rename each to name its quantity or role", file=sys.stderr)
        return 1
    print(f"{PROGRAM}: ok ({_plural(len(scripts), 'script')})")
    return 0


if __name__ == "__main__":
    # Identifiers can be non-ASCII; escape what a legacy console encoding cannot print instead of failing.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(errors="backslashreplace")
    raise SystemExit(main())
