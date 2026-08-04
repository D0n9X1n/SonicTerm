#!/usr/bin/env python3
"""Enforce comments on authored Rust API and non-obvious runtime contracts.

The checker deliberately uses a small Rust-aware lexer instead of text grep.
It must distinguish source syntax from comments, attributes, macro token trees,
and shader/source strings before it can make useful claims about documentation.
"""

from __future__ import annotations

import argparse
from bisect import bisect_right
from dataclasses import dataclass, field
import json
from pathlib import Path
import posixpath
import re
import subprocess
import sys
from typing import Iterable, Iterator, Sequence


EXCLUDED_PREFIXES = (
    "crates/sonicterm-freetype/freetype2/",
    "crates/sonicterm-freetype/libpng/",
    "crates/sonicterm-freetype/zlib/",
    "crates/sonicterm-harfbuzz/harfbuzz/",
)
EXCLUDED_FILES = frozenset(
    {
        "crates/sonicterm-freetype/src/lib.rs",
        "crates/sonicterm-freetype/src/types.rs",
        "crates/sonicterm-harfbuzz/src/lib.rs",
        "crates/sonicterm-fontconfig/src/lib.rs",
        "crates/sonicterm-block-glyph/src/customglyph.rs",
    }
)
MARKERS = ("When", "SAFETY", "Lock order", "Ordering", "Lifecycle")
STOPWORDS = frozenset(
    {
        "a",
        "an",
        "and",
        "are",
        "as",
        "at",
        "be",
        "because",
        "been",
        "being",
        "by",
        "condition",
        "else",
        "false",
        "for",
        "from",
        "has",
        "have",
        "if",
        "in",
        "into",
        "is",
        "it",
        "its",
        "not",
        "of",
        "on",
        "or",
        "path",
        "predicate",
        "branch",
        "that",
        "the",
        "then",
        "this",
        "to",
        "true",
        "was",
        "when",
        "where",
        "which",
        "with",
    }
)
ATOMIC_ORDERINGS = frozenset({"Relaxed", "Acquire", "Release", "AcqRel", "SeqCst"})
IDENTIFIER_STOPLIST = frozenset(
    {"self", "Some", "None", "Ok", "Err", "true", "false", "if", "else", "match", "let", "return", "_"}
)
CONTRACT_MARKERS = frozenset({"When", "Lock order", "Ordering", "Lifecycle"})
RUST_KEYWORDS = frozenset(
    {
        "Self",
        "abstract",
        "as",
        "async",
        "await",
        "become",
        "box",
        "break",
        "const",
        "continue",
        "crate",
        "do",
        "dyn",
        "else",
        "enum",
        "extern",
        "false",
        "final",
        "fn",
        "for",
        "gen",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "macro",
        "match",
        "mod",
        "move",
        "mut",
        "override",
        "priv",
        "pub",
        "ref",
        "return",
        "self",
        "static",
        "struct",
        "super",
        "trait",
        "true",
        "try",
        "type",
        "typeof",
        "union",
        "unsafe",
        "unsized",
        "use",
        "virtual",
        "where",
        "while",
        "yield",
    }
)
MAX_MARKER_LINES = 2
MAX_MARKER_CHARACTERS = 160
OPEN_TO_CLOSE = {"(": ")", "[": "]", "{": "}"}
CLOSE_TO_OPEN = {value: key for key, value in OPEN_TO_CLOSE.items()}


@dataclass(frozen=True)
class Token:
    """One non-comment Rust token with a source location."""

    kind: str
    text: str
    start: int
    end: int
    line: int
    column: int


@dataclass(frozen=True)
class Comment:
    """One line or nested block comment preserved for attachment checks."""

    text: str
    start: int
    end: int
    line: int
    column: int


@dataclass
class Lexed:
    """Tokenized Rust source plus comments and line-offset lookup data."""

    source: str
    tokens: list[Token]
    comments: list[Comment]
    line_starts: list[int]

    def line_column(self, offset: int) -> tuple[int, int]:
        """Return a one-based Unicode-codepoint line and column."""
        line_index = bisect_right(self.line_starts, offset) - 1
        return line_index + 1, offset - self.line_starts[line_index] + 1


@dataclass(frozen=True, order=True)
class Diagnostic:
    """A stable source diagnostic emitted by one checker rule."""

    path: str
    line: int
    column: int
    rule: str
    message: str

    def format(self) -> str:
        """Render the machine-stable diagnostic format used by CI."""
        return f"{self.path}:{self.line}:{self.column} [{self.rule}] {self.message}"


@dataclass
class Report:
    """Repository analysis results and the inventory behind them."""

    diagnostics: list[Diagnostic]
    semantic_candidates: list[Diagnostic]
    counts: dict
    paths: dict[str, list[str]]

    def inventory(self) -> dict:
        """Return a deterministic JSON-compatible inventory."""
        return {
            "counts": self.counts,
            "paths": self.paths,
            "diagnostics": [item.format() for item in self.diagnostics],
            "semantic_candidates": [item.format() for item in self.semantic_candidates],
        }


@dataclass
class Span:
    """A source range used for comments, attributes, and syntax bodies."""

    start: int
    end: int


@dataclass(frozen=True)
class MarkerInstance:
    """One marker line and its contiguous continuation comments."""

    marker: str
    comments: tuple[Comment, ...]

    @property
    def anchor(self) -> Comment:
        """Return the marker's prefixed first line."""
        return self.comments[0]

    @property
    def body(self) -> str:
        """Return marker prose without comment prefixes or the class label."""
        pieces = [_marker_body(self.comments[0], self.marker)]
        pieces.extend(comment.text.strip()[2:].strip() for comment in self.comments[1:])
        return " ".join(piece for piece in pieces if piece)


@dataclass
class Branch:
    """One conditional arm and the construct that owns its contract scope."""

    anchor: Token
    body_start: int
    body_end: int
    selecting: set[str]
    predicate: set[str]
    mandatory: bool
    value_selector: bool
    construct: Token | None


@dataclass
class Function:
    """A parsed named function or method and its enclosing context."""

    unit: "SourceUnit"
    token_index: int
    start_index: int
    name: str
    body_open: int | None
    body_close: int | None
    lexical_public: bool
    restricted_public: bool
    unsafe: bool
    nested: bool = False
    test_context: bool = False
    effective_public: bool = False
    public_trait_member: bool = False

    @property
    def start_token(self) -> Token:
        """Return the first signature modifier used as diagnostic anchor."""
        return self.unit.tokens[self.start_index]


@dataclass
class Module:
    """One file-backed or inline Rust module in the resolution graph."""

    key: tuple[str, ...]
    unit: "SourceUnit"
    start: int
    end: int
    parent: "Module | None"
    name: str
    public_decl: bool
    test_context: bool
    reachable: bool = False
    reexports: set[str] = field(default_factory=set)
    children: list["Module"] = field(default_factory=list)


@dataclass
class SourceUnit:
    """A tracked Rust file with token and delimiter indexes."""

    path: str
    text: str
    lexed: Lexed
    tokens: list[Token]
    pairs: dict[int, int]
    reverse_pairs: dict[int, int]
    attributes: list[Span]
    file_test_context: bool
    binary_context: bool
    excluded: bool
    modules: list[Module] = field(default_factory=list)

    def deepest_module(self, offset: int) -> Module | None:
        """Return the smallest module range containing an offset."""
        matches = [module for module in self.modules if module.start <= offset < module.end]
        return min(matches, key=lambda module: module.end - module.start, default=None)


class AnalysisError(RuntimeError):
    """Raised when repository discovery cannot produce a meaningful corpus."""


_IDENTIFIER_START = re.compile(r"[A-Za-z_]|[^\x00-\x7f]")
_IDENTIFIER_CONTINUE = re.compile(r"[A-Za-z0-9_]|[^\x00-\x7f]")


def _line_starts(source: str) -> list[int]:
    starts = [0]
    index = 0
    while index < len(source):
        if source[index] == "\r" and index + 1 < len(source) and source[index + 1] == "\n":
            index += 2
            starts.append(index)
            continue
        if source[index] == "\n":
            index += 1
            starts.append(index)
            continue
        index += 1
    return starts


def _location(starts: Sequence[int], offset: int) -> tuple[int, int]:
    line_index = bisect_right(starts, offset) - 1
    return line_index + 1, offset - starts[line_index] + 1


def _raw_string_end(source: str, index: int) -> int | None:
    prefixes = ("br", "rb", "cr", "rc", "r")
    for prefix in prefixes:
        if not source.startswith(prefix, index):
            continue
        cursor = index + len(prefix)
        hashes = 0
        while cursor < len(source) and source[cursor] == "#":
            hashes += 1
            cursor += 1
        if cursor >= len(source) or source[cursor] != '"':
            continue
        ending = '"' + "#" * hashes
        found = source.find(ending, cursor + 1)
        return len(source) if found < 0 else found + len(ending)
    return None


def _quoted_end(source: str, index: int, quote: str) -> int:
    cursor = index + 1
    while cursor < len(source):
        if source[cursor] == "\\":
            cursor += 2
            continue
        if source[cursor] == quote:
            return cursor + 1
        cursor += 1
    return len(source)


def _char_or_lifetime(source: str, index: int) -> tuple[str, int]:
    cursor = index + 1
    if cursor < len(source) and source[cursor] == "\\":
        cursor += 2
    else:
        cursor += 1
    if cursor < len(source) and source[cursor] == "'":
        return "CHAR", cursor + 1
    cursor = index + 1
    while cursor < len(source) and (
        source[cursor].isalnum() or source[cursor] == "_" or ord(source[cursor]) > 127
    ):
        cursor += 1
    return "LIFETIME", cursor


def lex_rust(source: str) -> Lexed:
    """Tokenize Rust while excluding comments, strings, and macro token trees."""
    starts = _line_starts(source)
    tokens: list[Token] = []
    comments: list[Comment] = []
    index = 0
    length = len(source)

    def add(kind: str, start: int, end: int) -> None:
        line, column = _location(starts, start)
        tokens.append(Token(kind, source[start:end], start, end, line, column))

    while index < length:
        char = source[index]
        if char.isspace():
            index += 1
            continue
        if source.startswith("//", index):
            end = source.find("\n", index)
            if end < 0:
                end = length
            line, column = _location(starts, index)
            comments.append(Comment(source[index:end], index, end, line, column))
            index = end
            continue
        if source.startswith("/*", index):
            depth = 1
            cursor = index + 2
            while cursor < length and depth:
                if source.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif source.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            line, column = _location(starts, index)
            comments.append(Comment(source[index:cursor], index, cursor, line, column))
            index = cursor
            continue
        raw_end = _raw_string_end(source, index)
        if raw_end is not None:
            add("STRING", index, raw_end)
            index = raw_end
            continue
        if source.startswith(('b"', 'c"'), index):
            end = _quoted_end(source, index + 1, '"')
            add("STRING", index, end)
            index = end
            continue
        if char == '"':
            end = _quoted_end(source, index, '"')
            add("STRING", index, end)
            index = end
            continue
        if source.startswith("b'", index):
            end = _quoted_end(source, index + 1, "'")
            add("CHAR", index, end)
            index = end
            continue
        if char == "'":
            kind, end = _char_or_lifetime(source, index)
            add(kind, index, end)
            index = end
            continue
        if source.startswith("r#", index) and index + 2 < length and _IDENTIFIER_START.match(source[index + 2]):
            cursor = index + 3
            while cursor < length and _IDENTIFIER_CONTINUE.match(source[cursor]):
                cursor += 1
            add("IDENT", index, cursor)
            index = cursor
            continue
        if _IDENTIFIER_START.match(char):
            cursor = index + 1
            while cursor < length and _IDENTIFIER_CONTINUE.match(source[cursor]):
                cursor += 1
            add("IDENT", index, cursor)
            index = cursor
            continue
        if char.isdigit():
            cursor = index + 1
            while cursor < length and (source[cursor].isalnum() or source[cursor] in "_."):
                cursor += 1
            add("NUMBER", index, cursor)
            index = cursor
            continue
        matched = False
        for punctuation in ("=>", "::", "->", "..=", "...", "..", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/="):
            if source.startswith(punctuation, index):
                add("PUNCT", index, index + len(punctuation))
                index += len(punctuation)
                matched = True
                break
        if matched:
            continue
        add("PUNCT", index, index + 1)
        index += 1

    pairs = _delimiter_pairs(tokens)
    macro_ranges: list[tuple[int, int]] = []
    for token_index, token in enumerate(tokens):
        if token.text != "!" or token_index == 0 or token_index + 1 >= len(tokens):
            continue
        macro_name = tokens[token_index - 1]
        if (
            macro_name.kind != "IDENT"
            or macro_name.text in RUST_KEYWORDS
            or tokens[token_index + 1].text not in OPEN_TO_CLOSE
        ):
            continue
        close = pairs.get(token_index + 1)
        # `cfg!(...)` is selecting syntax for the platform-branch rule. Other
        # macro token trees are opaque: code-looking tokens inside them are not
        # Rust items or branches until macro expansion, which this checker never
        # attempts to model.
        if close is not None and tokens[token_index - 1].text != "cfg":
            macro_ranges.append((tokens[token_index + 1].start, tokens[close].end))
    if macro_ranges:
        tokens = [
            token
            for token in tokens
            if not any(start <= token.start < end for start, end in macro_ranges)
        ]
        comments = [
            comment
            for comment in comments
            if not any(start <= comment.start < end for start, end in macro_ranges)
        ]
    return Lexed(source, tokens, comments, starts)


def _delimiter_pairs(tokens: Sequence[Token]) -> dict[int, int]:
    stack: list[tuple[str, int]] = []
    pairs: dict[int, int] = {}
    for index, token in enumerate(tokens):
        if token.text in OPEN_TO_CLOSE:
            stack.append((token.text, index))
        elif token.text in CLOSE_TO_OPEN:
            wanted = CLOSE_TO_OPEN[token.text]
            if stack and stack[-1][0] == wanted:
                _, opening = stack.pop()
                pairs[opening] = index
                pairs[index] = opening
    return pairs


def _attributes(tokens: Sequence[Token], pairs: dict[int, int]) -> list[Span]:
    spans: list[Span] = []
    for index, token in enumerate(tokens[:-1]):
        if token.text != "#" or tokens[index + 1].text != "[":
            continue
        close = pairs.get(index + 1)
        if close is not None:
            spans.append(Span(token.start, tokens[close].end))
    return spans


def _is_excluded(path: str) -> bool:
    return path in EXCLUDED_FILES or path.startswith(EXCLUDED_PREFIXES)


def _is_test_file(path: str) -> bool:
    parts = path.split("/")
    return Path(path).name.endswith("_tests.rs") or "tests" in parts or Path(path).name == "build.rs"


def _is_binary(path: str) -> bool:
    return path == "src/main.rs" or path.endswith("/src/main.rs") or "/examples/" in f"/{path}"


def _normalize_path(root: Path, value: str, caller_cwd: Path) -> str:
    value = value.replace("\\", "/")
    candidate = Path(value)
    if candidate.is_absolute():
        resolved = candidate.resolve()
    else:
        caller_candidate = (caller_cwd / candidate).resolve()
        root_candidate = (root / candidate).resolve()
        resolved = caller_candidate if caller_candidate.exists() else root_candidate
    try:
        return resolved.relative_to(root.resolve()).as_posix()
    except ValueError as error:
        raise AnalysisError(f"path is outside repository: {value}") from error


def _tracked_rust(root: Path) -> list[str]:
    completed = subprocess.run(
        ["git", "ls-files", "-z", "--full-name", "--", "*.rs"],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise AnalysisError(completed.stderr.decode("utf-8", errors="replace").strip())
    return sorted(raw.decode("utf-8").replace("\\", "/") for raw in completed.stdout.split(b"\0") if raw)


def _source_unit(root: Path, path: str) -> SourceUnit:
    text = (root / path).read_text(encoding="utf-8", errors="replace")
    lexed = lex_rust(text)
    tokens = lexed.tokens
    pairs = _delimiter_pairs(tokens)
    return SourceUnit(
        path=path,
        text=text,
        lexed=lexed,
        tokens=tokens,
        pairs=pairs,
        reverse_pairs={value: key for key, value in pairs.items()},
        attributes=_attributes(tokens, pairs),
        file_test_context=_is_test_file(path),
        binary_context=_is_binary(path),
        excluded=_is_excluded(path),
    )


def _span_ending(spans: Sequence[Span], cursor: int) -> Span | None:
    for span in spans:
        if span.end == cursor:
            return span
    return None


def _prelude(unit: SourceUnit, offset: int) -> tuple[list[Comment], list[Span], int]:
    comments = unit.lexed.comments
    attributes = unit.attributes
    found_comments: list[Comment] = []
    found_attributes: list[Span] = []
    cursor = offset
    while True:
        while cursor > 0 and unit.text[cursor - 1].isspace():
            cursor -= 1
        comment = next((item for item in comments if item.end == cursor), None)
        if comment is None:
            # Line comments exclude their trailing newline. After whitespace is
            # skipped, the cursor therefore sits one character past the comment
            # end; accept that boundary so stacked attributes/markers remain one
            # Rust outer-attribute prelude.
            comment = next(
                (
                    item
                    for item in comments
                    if item.end < cursor
                    and unit.text[item.end:cursor].strip() == ""
                    and "\n" not in unit.text[item.end:cursor].lstrip("\r\n")
                ),
                None,
            )
        if comment is not None:
            found_comments.append(comment)
            cursor = comment.start
            continue
        attribute = _span_ending(attributes, cursor)
        if attribute is not None:
            found_attributes.append(attribute)
            cursor = attribute.start
            continue
        break
    found_comments.reverse()
    found_attributes.reverse()
    return found_comments, found_attributes, cursor


def _attribute_texts(unit: SourceUnit, spans: Sequence[Span]) -> list[str]:
    return [unit.text[span.start:span.end] for span in spans]


def _doc_text(unit: SourceUnit, comments: Sequence[Comment], attributes: Sequence[Span]) -> str:
    pieces: list[str] = []
    for comment in comments:
        stripped = comment.text.lstrip()
        if stripped.startswith("///") and not stripped.startswith("////"):
            pieces.append(stripped[3:].strip())
        elif stripped.startswith("/**") and not stripped.startswith("/***"):
            pieces.append(stripped[3:-2].strip())
    for span in attributes:
        text = unit.text[span.start:span.end]
        match = re.search(r"\bdoc\s*=\s*(?:r#*)?\"(.*?)\"", text, re.DOTALL)
        if match:
            pieces.append(match.group(1))
    return "\n".join(pieces).strip()


def _marker_name(comment: Comment) -> str | None:
    stripped = comment.text.strip()
    for marker in MARKERS:
        if stripped.startswith(f"// {marker}:"):
            return marker
    return None


def _marker_body(comment: Comment, marker: str) -> str:
    return comment.text.strip()[len(f"// {marker}:"):].strip()


def _substantive(comment: Comment, marker: str) -> bool:
    return bool(_semantic_tokens(_marker_body(comment, marker)))


def _exact_identifier_tokens(text: str) -> set[str]:
    """Return exact Rust-like identifiers after removing non-discriminating words."""
    return {
        token
        for token in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", text.replace("`", ""))
        if token not in IDENTIFIER_STOPLIST
    }


def _ordinary_line_comment(comment: Comment) -> bool:
    stripped = comment.text.lstrip()
    return stripped.startswith("//") and not stripped.startswith("///") and "\n" not in comment.text


def _contiguous_line_comments_before(unit: SourceUnit, target_line: int) -> list[Comment]:
    by_line = {
        comment.line: comment
        for comment in unit.lexed.comments
        if _ordinary_line_comment(comment)
    }
    found: list[Comment] = []
    line = target_line - 1
    while line in by_line:
        found.append(by_line[line])
        line -= 1
    found.reverse()
    return found


def _marker_instances(comments: Sequence[Comment]) -> list[MarkerInstance]:
    instances: list[MarkerInstance] = []
    index = 0
    while index < len(comments):
        marker = _marker_name(comments[index])
        if marker is None:
            index += 1
            continue
        end = index + 1
        while end < len(comments) and _marker_name(comments[end]) is None:
            end += 1
        instances.append(MarkerInstance(marker, tuple(comments[index:end])))
        index = end
    return instances


def _strict_prelude_instances(
    unit: SourceUnit,
    token: Token,
    marker: str,
) -> list[MarkerInstance]:
    _, attributes = _preceding_attributes(unit, token)
    target_line = token.line
    if attributes:
        target_line = min(unit.lexed.line_column(span.start)[0] for span in attributes)
    comments = _contiguous_line_comments_before(unit, target_line)
    return [instance for instance in _marker_instances(comments) if instance.marker == marker]


def _first_body_instances(
    unit: SourceUnit,
    start: int,
    end: int,
    marker: str,
) -> list[MarkerInstance]:
    comments = _first_body_comments(unit, start, end)
    if not comments or _marker_name(comments[0]) != marker:
        return []
    contiguous = [comments[0]]
    for comment in comments[1:]:
        if comment.line != contiguous[-1].line + 1:
            break
        contiguous.append(comment)
    return [instance for instance in _marker_instances(contiguous) if instance.marker == marker]


def _marker_size_error(instance: MarkerInstance) -> str | None:
    if instance.marker not in CONTRACT_MARKERS:
        return None
    if len(instance.comments) > MAX_MARKER_LINES:
        return "marker exceeds 2 lines or 160 characters"
    pieces = [_marker_body(instance.comments[0], instance.marker)]
    pieces.extend(comment.text.strip()[2:].strip() for comment in instance.comments[1:])
    if sum(len(piece) for piece in pieces) > MAX_MARKER_CHARACTERS:
        return "marker exceeds 2 lines or 160 characters"
    return None


def _semantic_tokens(text: str) -> set[str]:
    text = re.sub(r"^(?:When|SAFETY|Lock order|Ordering|Lifecycle)\s*:\s*", "", text, flags=re.I)
    words: list[str] = []
    for raw in re.findall(r"[A-Za-z_][A-Za-z0-9_]*|\d+(?:\.\d+)?", text):
        for underscore in raw.split("_"):
            pieces = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", underscore).split()
            words.extend(piece.lower() for piece in pieces if piece)
    return {word for word in words if word not in STOPWORDS}


def _comments_between(unit: SourceUnit, start: int, end: int) -> list[Comment]:
    return [comment for comment in unit.lexed.comments if start <= comment.start and comment.end <= end]


def _first_nonspace_offset(unit: SourceUnit, start: int, end: int) -> int:
    cursor = start
    while cursor < end and unit.text[cursor].isspace():
        cursor += 1
    return cursor


def _matching(unit: SourceUnit, index: int) -> int | None:
    return unit.pairs.get(index)


def _token_index_at_or_after(tokens: Sequence[Token], offset: int) -> int:
    starts = [token.start for token in tokens]
    return bisect_right(starts, offset - 1)


def _find_body(tokens: Sequence[Token], pairs: dict[int, int], fn_index: int) -> tuple[int | None, int | None]:
    paren = bracket = angle = 0
    for index in range(fn_index + 1, len(tokens)):
        text = tokens[index].text
        if text == "(":
            paren += 1
        elif text == ")":
            paren = max(0, paren - 1)
        elif text == "[":
            bracket += 1
        elif text == "]":
            bracket = max(0, bracket - 1)
        elif text == "<":
            angle += 1
        elif text == ">":
            angle = max(0, angle - 1)
        elif text == "{" and paren == bracket == angle == 0:
            return index, pairs.get(index)
        elif text == ";" and paren == bracket == angle == 0:
            return None, None
    return None, None


def _signature_start(tokens: Sequence[Token], fn_index: int) -> int:
    index = fn_index - 1
    allowed = {"pub", "async", "const", "unsafe", "extern", "default"}
    while index >= 0:
        token = tokens[index]
        if token.kind == "STRING" and index > 0 and tokens[index - 1].text == "extern":
            index -= 2
            continue
        if token.text == ")":
            opening = _reverse_match(tokens, index, "(", ")")
            if opening is not None and opening > 0 and tokens[opening - 1].text == "pub":
                index = opening - 2
                continue
        if token.text in allowed:
            index -= 1
            continue
        break
    return index + 1


def _reverse_match(tokens: Sequence[Token], close_index: int, opening: str, closing: str) -> int | None:
    depth = 0
    for index in range(close_index, -1, -1):
        if tokens[index].text == closing:
            depth += 1
        elif tokens[index].text == opening:
            depth -= 1
            if depth == 0:
                return index
    return None


def _lexical_visibility(tokens: Sequence[Token], start: int, fn_index: int) -> tuple[bool, bool, bool]:
    segment = tokens[start:fn_index]
    public = any(token.text == "pub" for token in segment)
    restricted = False
    for index, token in enumerate(segment):
        if token.text == "pub" and index + 1 < len(segment) and segment[index + 1].text == "(":
            restricted = True
    unsafe = any(token.text == "unsafe" for token in segment)
    return public, restricted, unsafe


def _top_level_indices(unit: SourceUnit, start_index: int, end_index: int) -> Iterator[int]:
    index = start_index
    while index < end_index:
        yield index
        token = unit.tokens[index]
        if token.text in OPEN_TO_CLOSE and index in unit.pairs:
            index = unit.pairs[index] + 1
        else:
            index += 1


def _module_target(parent: Module, name: str, path_attr: str | None, tracked: set[str]) -> str | None:
    parent_path = Path(parent.unit.path)
    if path_attr:
        candidate = posixpath.normpath((parent_path.parent / path_attr).as_posix())
        return candidate if candidate in tracked else None
    if parent_path.name in {"lib.rs", "main.rs", "mod.rs"}:
        base = parent_path.parent
    else:
        base = parent_path.parent / parent_path.stem
    candidates = [(base / f"{name}.rs").as_posix(), (base / name / "mod.rs").as_posix()]
    return next((candidate for candidate in candidates if candidate in tracked), None)


def _preceding_attributes(unit: SourceUnit, token: Token) -> tuple[list[Comment], list[Span]]:
    comments, attributes, _ = _prelude(unit, token.start)
    return comments, attributes


def _path_attribute(unit: SourceUnit, token: Token) -> str | None:
    _, attributes = _preceding_attributes(unit, token)
    for span in attributes:
        text = unit.text[span.start:span.end]
        match = re.search(r"\bpath\s*=\s*\"([^\"]+)\"", text)
        if match:
            return match.group(1)
    return None


def _cfg_test(unit: SourceUnit, token: Token) -> bool:
    _, attributes = _preceding_attributes(unit, token)
    return any(re.search(r"\bcfg\s*\(\s*test\s*\)", unit.text[span.start:span.end]) for span in attributes)


def _direct_test_item(unit: SourceUnit, token: Token) -> bool:
    _, attributes = _preceding_attributes(unit, token)
    texts = _attribute_texts(unit, attributes)
    return any(re.search(r"#\[\s*(?:test|bench)\b", text) for text in texts)


def _pub_use_names(unit: SourceUnit, module: Module, start_index: int, end_index: int) -> set[str]:
    names: set[str] = set()
    index = start_index
    while index < end_index - 1:
        if unit.tokens[index].text == "pub" and unit.tokens[index + 1].text == "use":
            cursor = index + 2
            parts: list[str] = []
            while cursor < end_index and unit.tokens[cursor].text != ";":
                if unit.tokens[cursor].kind == "IDENT":
                    parts.append(unit.tokens[cursor].text)
                cursor += 1
            while parts and parts[0] in {"crate", "self", "super"}:
                parts.pop(0)
            if parts:
                names.add(parts[0])
            index = cursor + 1
            continue
        if unit.tokens[index].text in OPEN_TO_CLOSE and index in unit.pairs:
            index = unit.pairs[index] + 1
        else:
            index += 1
    return names


def _build_modules(units: dict[str, SourceUnit], tracked: set[str], diagnostics: list[Diagnostic]) -> list[Module]:
    modules: list[Module] = []
    visited_files: set[tuple[str, tuple[str, ...]]] = set()

    def add_module(module: Module, start_index: int, end_index: int) -> None:
        key = (module.unit.path, module.key)
        if key in visited_files:
            return
        visited_files.add(key)
        module.unit.modules.append(module)
        modules.append(module)
        module.reexports = _pub_use_names(module.unit, module, start_index, end_index)
        index = start_index
        while index < end_index:
            token = module.unit.tokens[index]
            public = False
            mod_index = index
            if token.text == "pub" and index + 1 < end_index and module.unit.tokens[index + 1].text == "mod":
                public = True
                mod_index = index + 1
            elif token.text != "mod":
                if token.text in OPEN_TO_CLOSE and index in module.unit.pairs:
                    index = module.unit.pairs[index] + 1
                else:
                    index += 1
                continue
            if mod_index + 1 >= end_index or module.unit.tokens[mod_index + 1].kind != "IDENT":
                index += 1
                continue
            name_token = module.unit.tokens[mod_index + 1]
            after = mod_index + 2
            if after >= end_index:
                break
            test_context = module.test_context or _cfg_test(module.unit, token)
            if module.unit.tokens[after].text == "{":
                close = module.unit.pairs.get(after)
                if close is None:
                    index += 1
                    continue
                child = Module(
                    key=module.key + (name_token.text,),
                    unit=module.unit,
                    start=module.unit.tokens[after].end,
                    end=module.unit.tokens[close].start,
                    parent=module,
                    name=name_token.text,
                    public_decl=public,
                    test_context=test_context,
                )
                module.children.append(child)
                add_module(child, after + 1, close)
                index = close + 1
                continue
            if module.unit.tokens[after].text == ";":
                target = _module_target(module, name_token.text, _path_attribute(module.unit, token), tracked)
                if target is None:
                    diagnostics.append(
                        Diagnostic(
                            module.unit.path,
                            token.line,
                            token.column,
                            "module-resolution",
                            f"module {name_token.text} has no tracked source target",
                        )
                    )
                    index = after + 1
                    continue
                target_unit = units[target]
                child = Module(
                    key=module.key + (name_token.text,),
                    unit=target_unit,
                    start=0,
                    end=len(target_unit.text),
                    parent=module,
                    name=name_token.text,
                    public_decl=public,
                    test_context=test_context or target_unit.file_test_context,
                )
                module.children.append(child)
                add_module(child, 0, len(target_unit.tokens))
                index = after + 1
                continue
            index += 1

    roots = [path for path in units if path == "src/lib.rs" or path.endswith("/src/lib.rs")]
    for path in sorted(roots):
        unit = units[path]
        crate = path[: -len("/src/lib.rs")] if path.endswith("/src/lib.rs") else "crate"
        root = Module((crate,), unit, 0, len(unit.text), None, crate, True, False, reachable=True)
        add_module(root, 0, len(unit.tokens))

    changed = True
    while changed:
        changed = False
        for module in modules:
            if module.parent is None:
                continue
            should_reach = module.parent.reachable and (
                module.public_decl or module.name in module.parent.reexports
            )
            if should_reach and not module.reachable:
                module.reachable = True
                changed = True
    return modules


def _find_traits_and_impls(
    unit: SourceUnit,
) -> tuple[list[tuple[int, int, bool]], list[tuple[int, int, str, bool, int]], set[tuple[int, str]]]:
    """Index trait and impl body ranges, retaining each impl's own anchor."""
    traits: list[tuple[int, int, bool]] = []
    impls: list[tuple[int, int, str, bool, int]] = []
    public_types: set[tuple[int, str]] = set()
    tokens = unit.tokens
    for index, token in enumerate(tokens):
        module = unit.deepest_module(token.start)
        module_id = id(module) if module else 0
        if token.text == "pub" and index + 2 < len(tokens) and tokens[index + 1].text in {"struct", "enum", "union", "type"}:
            if tokens[index + 2].kind == "IDENT":
                public_types.add((module_id, tokens[index + 2].text))
        if token.text == "trait" and index + 1 < len(tokens):
            body = next((cursor for cursor in range(index + 1, len(tokens)) if tokens[cursor].text in {"{", ";"}), None)
            if body is not None and tokens[body].text == "{" and body in unit.pairs:
                start = _signature_start(tokens, index)
                public, restricted, _ = _lexical_visibility(tokens, start, index)
                traits.append((tokens[body].end, tokens[unit.pairs[body]].start, public and not restricted and bool(module and module.reachable)))
        if token.text == "impl":
            body = next((cursor for cursor in range(index + 1, len(tokens)) if tokens[cursor].text == "{"), None)
            if body is not None and body in unit.pairs:
                header = [item.text for item in tokens[index + 1:body]]
                target = ""
                if "for" in header:
                    target = next((item for item in reversed(header) if re.match(r"^[A-Za-z_]", item) and item != "for"), "")
                else:
                    target = next((item for item in header if re.match(r"^[A-Za-z_]", item)), "")
                impls.append(
                    (
                        tokens[body].end,
                        tokens[unit.pairs[body]].start,
                        target,
                        "Drop" in header and "for" in header,
                        index,
                    )
                )
    return traits, impls, public_types


def _functions(unit: SourceUnit) -> list[Function]:
    functions: list[Function] = []
    for index, token in enumerate(unit.tokens):
        if token.text != "fn" or index + 1 >= len(unit.tokens) or unit.tokens[index + 1].kind != "IDENT":
            continue
        start = _signature_start(unit.tokens, index)
        public, restricted, unsafe = _lexical_visibility(unit.tokens, start, index)
        body_open, body_close = _find_body(unit.tokens, unit.pairs, index)
        functions.append(
            Function(
                unit,
                index,
                start,
                unit.tokens[index + 1].text,
                body_open,
                body_close,
                public,
                restricted,
                unsafe,
            )
        )
    for function in functions:
        offset = function.start_token.start
        function.nested = any(
            other is not function
            and other.body_open is not None
            and other.body_close is not None
            and other.unit.tokens[other.body_open].start < offset < other.unit.tokens[other.body_close].end
            for other in functions
        )
        function.test_context = unit.file_test_context or _direct_test_item(unit, function.start_token)
        module = unit.deepest_module(offset)
        if module and module.test_context:
            function.test_context = True
    return functions


def _non_safety_exempt_at(unit: SourceUnit, offset: int, functions: Sequence[Function]) -> bool:
    if unit.file_test_context or unit.binary_context:
        return True
    module = unit.deepest_module(offset)
    if module and module.test_context:
        return True
    for function in functions:
        if not function.test_context:
            continue
        _, _, prelude_start = _prelude(unit, function.start_token.start)
        end = (
            unit.tokens[function.body_close].end
            if function.body_close is not None
            else function.start_token.end
        )
        if prelude_start <= offset < end:
            return True
    return False


def _enclosing(start: int, ranges: Sequence[tuple[int, int, object]]) -> object | None:
    matches = [item for begin, end, item in ranges if begin <= start < end]
    return matches[-1] if matches else None


def _function_effective_visibility(unit: SourceUnit, function: Function, traits, impls, public_types) -> tuple[bool, bool]:
    offset = function.start_token.start
    trait = _enclosing(offset, [(start, end, public) for start, end, public in traits])
    if trait is not None:
        return bool(trait), bool(trait)
    if not function.lexical_public or function.restricted_public or function.nested or function.test_context or unit.binary_context:
        return False, False
    module = unit.deepest_module(offset)
    if not module or not module.reachable:
        return False, False
    impl = _enclosing(
        offset,
        [(start, end, (target, is_drop)) for start, end, target, is_drop, _ in impls],
    )
    if impl is not None:
        target, _ = impl
        if (id(module), target) not in public_types:
            declared_private = any(
                token.text in {"struct", "enum", "union", "type"}
                and cursor + 1 < len(unit.tokens)
                and unit.tokens[cursor + 1].text == target
                and not (cursor > 0 and unit.tokens[cursor - 1].text == "pub")
                for cursor, token in enumerate(unit.tokens)
            )
            if declared_private:
                return False, False
    return True, False


def _comment_markers(comments: Sequence[Comment], marker: str) -> list[Comment]:
    return [comment for comment in comments if _marker_name(comment) == marker]


def _use_instances(instances: Sequence[MarkerInstance], used: set[int]) -> None:
    for instance in instances:
        for comment in instance.comments:
            used.add(comment.start)


def _instance_tokens(instances: Sequence[MarkerInstance]) -> set[str]:
    tokens: set[str] = set()
    for instance in instances:
        tokens.update(_exact_identifier_tokens(instance.body))
    return tokens


def _validate_instance_sizes(
    unit: SourceUnit,
    instances: Sequence[MarkerInstance],
    diagnostics: list[Diagnostic],
) -> bool:
    valid = True
    for instance in instances:
        error = _marker_size_error(instance)
        if error:
            diagnostics.append(
                Diagnostic(
                    unit.path,
                    instance.anchor.line,
                    instance.anchor.column,
                    _rule_for_marker(instance.marker),
                    error,
                )
            )
            valid = False
    return valid


def _attach_marker(
    unit: SourceUnit,
    token: Token,
    marker: str,
    used: set[int],
    diagnostics: list[Diagnostic],
    missing_message: str,
    validator=None,
) -> None:
    if marker == "SAFETY":
        comments, _, _ = _prelude(unit, token.start)
        matches = _comment_markers(comments, marker)
        if len(matches) == 1 and _substantive(matches[0], marker):
            used.add(matches[0].start)
            return
        for comment in matches:
            used.add(comment.start)
        if len(matches) > 1:
            anchor = matches[0]
            diagnostics.append(Diagnostic(unit.path, anchor.line, anchor.column, _rule_for_marker(marker), f"duplicate // {marker}: markers for one contract unit"))
        else:
            diagnostics.append(Diagnostic(unit.path, token.line, token.column, _rule_for_marker(marker), missing_message))
        return

    instances = _strict_prelude_instances(unit, token, marker)
    _use_instances(instances, used)
    if not instances:
        diagnostics.append(
            Diagnostic(unit.path, token.line, token.column, _rule_for_marker(marker), missing_message)
        )
        return
    if not _validate_instance_sizes(unit, instances, diagnostics):
        return
    if not all(instance.body.strip() for instance in instances):
        diagnostics.append(
            Diagnostic(
                unit.path,
                instances[0].anchor.line,
                instances[0].anchor.column,
                _rule_for_marker(marker),
                f"// {marker}: marker needs substantive contract text",
            )
        )
        return
    if validator is not None:
        error = validator(instances)
        if error:
            diagnostics.append(
                Diagnostic(
                    unit.path,
                    instances[0].anchor.line,
                    instances[0].anchor.column,
                    _rule_for_marker(marker),
                    error,
                )
            )


def _rule_for_marker(marker: str) -> str:
    return {
        "SAFETY": "safety",
        "Lock order": "lock-order",
        "Ordering": "ordering",
        "Lifecycle": "lifecycle",
        "When": "when",
    }[marker]


def _nested_function_ranges(
    function: Function,
    functions: Sequence[Function],
) -> list[tuple[int, int]]:
    if function.body_open is None or function.body_close is None:
        return []
    unit = function.unit
    body_start = unit.tokens[function.body_open].end
    body_end = unit.tokens[function.body_close].start
    ranges = []
    for other in functions:
        if other is function or not (body_start <= other.start_token.start < body_end):
            continue
        end = (
            unit.tokens[other.body_close].end
            if other.body_close is not None
            else other.start_token.end
        )
        ranges.append((other.start_token.start, end))
    return ranges


def _function_body_tokens(function: Function) -> list[Token]:
    if function.body_open is None or function.body_close is None:
        return []
    return function.unit.tokens[function.body_open + 1:function.body_close]


def _inside_ranges(offset: int, ranges: Sequence[tuple[int, int]]) -> bool:
    return any(start <= offset < end for start, end in ranges)


def _outside_ranges(
    tokens: Sequence[Token],
    ranges: Sequence[tuple[int, int]],
) -> list[Token]:
    return [token for token in tokens if not _inside_ranges(token.start, ranges)]


def _receiver_identifier(
    tokens: Sequence[Token],
    pairs: dict[int, int],
    dot_index: int,
) -> str | None:
    cursor = dot_index - 1
    if cursor < 0:
        return None
    if tokens[cursor].text in {"]",
        ")",
    }:
        opening = pairs.get(cursor)
        if opening is None:
            return None
        cursor = opening - 1
    return tokens[cursor].text if cursor >= 0 and tokens[cursor].kind == "IDENT" else None


def _lock_identifiers(
    tokens: Sequence[Token],
    excluded: Sequence[tuple[int, int]] = (),
) -> set[str]:
    pairs = _delimiter_pairs(tokens)
    identifiers: set[str] = set()
    for index in range(len(tokens) - 3):
        if _inside_ranges(tokens[index].start, excluded):
            continue
        if (
            tokens[index].text != "."
            or tokens[index + 1].text not in {"lock", "read", "write", "borrow_mut"}
            or tokens[index + 2].text != "("
            or pairs.get(index + 2) != index + 3
        ):
            continue
        receiver = _receiver_identifier(tokens, pairs, index)
        if receiver:
            identifiers.add(receiver)
    return identifiers


def _atomic_sites(
    tokens: Sequence[Token],
    excluded: Sequence[tuple[int, int]] = (),
) -> list[tuple[str, set[str]]]:
    pairs = _delimiter_pairs(tokens)
    grouped: dict[tuple[int, str], set[str]] = {}
    for index in range(len(tokens) - 2):
        if _inside_ranges(tokens[index].start, excluded):
            continue
        if (
            tokens[index].text != "Ordering"
            or tokens[index + 1].text != "::"
            or tokens[index + 2].text not in ATOMIC_ORDERINGS
        ):
            continue
        variant = index + 2
        openings = [
            opening
            for opening, close in pairs.items()
            if opening < close and tokens[opening].text == "(" and opening < variant < close
        ]
        if not openings:
            continue
        opening = max(openings)
        receiver = None
        if opening >= 2 and tokens[opening - 2].text == ".":
            receiver = _receiver_identifier(tokens, pairs, opening - 2)
        if receiver is None and opening >= 1 and tokens[opening - 1].kind == "IDENT":
            receiver = tokens[opening - 1].text
        if receiver:
            grouped.setdefault((opening, receiver), set()).add(tokens[variant].text)
    return [(receiver, orderings) for (_, receiver), orderings in sorted(grouped.items())]


def _release_tokens(tokens: Sequence[Token]) -> set[str]:
    release: set[str] = set()
    for index, token in enumerate(tokens):
        if token.text == "." and index + 2 < len(tokens) and tokens[index + 1].kind == "IDENT":
            receiver = _receiver_identifier(tokens, _delimiter_pairs(tokens), index)
            if receiver:
                release.add(receiver)
            if tokens[index + 2].text == "(":
                release.add(tokens[index + 1].text)
        if token.text in {"=", "-=", "+="} and index > 0:
            cursor = index - 1
            if tokens[cursor].kind == "IDENT":
                release.add(tokens[cursor].text)
        if token.kind == "IDENT" and index + 1 < len(tokens) and tokens[index + 1].text == "(":
            release.add(token.text)
            close = _delimiter_pairs(tokens).get(index + 1)
            if close is not None:
                release.update(
                    item.text for item in tokens[index + 2:close] if item.kind == "IDENT"
                )
    return release - IDENTIFIER_STOPLIST


def _lock_sequence_text(text: str) -> bool:
    return "->" in text or bool(re.search(r"\b(?:before|then|after)\b", text, re.I))


def _lock_validator(required: set[str]):
    def validate(instances: Sequence[MarkerInstance]) -> str | None:
        text = " ".join(instance.body for instance in instances)
        tokens = _instance_tokens(instances)
        rank = re.search(r"\brank\s*[0-9]+\b|\bRANK_[A-Z0-9_]+\b", text)
        if not _lock_sequence_text(text):
            return "// Lock order: marker must state an acquisition sequence"
        if rank is None and not required <= tokens:
            return "// Lock order: must name every acquired lock identifier"
        return None

    return validate


def _ordering_validator(sites: Sequence[tuple[str, set[str]]]):
    def validate(instances: Sequence[MarkerInstance]) -> str | None:
        tokens = _instance_tokens(instances)
        if any(receiver not in tokens or not orderings <= tokens for receiver, orderings in sites):
            return "// Ordering: must name each atomic receiver and exact ordering variant"
        return None

    return validate


def _lifecycle_validator(target: str, release: set[str]):
    def validate(instances: Sequence[MarkerInstance]) -> str | None:
        tokens = _instance_tokens(instances)
        if target not in tokens or not (tokens & release):
            return "// Lifecycle: must name the Drop type and a release token"
        return None

    return validate


def _selecting_tokens(tokens: Sequence[Token]) -> set[str]:
    return _semantic_tokens(" ".join(token.text for token in tokens if token.kind in {"IDENT", "NUMBER", "STRING"}))


def _predicate_tokens(tokens: Sequence[Token]) -> set[str]:
    return {
        token.text
        for token in tokens
        if token.kind == "IDENT" and token.text not in IDENTIFIER_STOPLIST
    }


def _validate_when_text(text: str, selecting: set[str], predicate: set[str]) -> str | None:
    semantic = _semantic_tokens(text)
    if not semantic:
        return "// When: explanation needs at least one semantic token"
    if semantic <= selecting:
        return "// When: explanation must add meaning beyond the selecting predicate or pattern"
    if predicate and not (_exact_identifier_tokens(text) & predicate):
        return "// When: marker must name this branch predicate"
    return None


def _first_body_comments(unit: SourceUnit, start: int, end: int) -> list[Comment]:
    cursor = start
    found: list[Comment] = []
    while True:
        while cursor < end and unit.text[cursor].isspace():
            cursor += 1
        comment = next((item for item in unit.lexed.comments if item.start == cursor), None)
        if comment is None:
            break
        found.append(comment)
        cursor = comment.end
    return found


def _contains(tokens: Sequence[Token], values: set[str]) -> bool:
    return any(token.text in values for token in tokens)


def _ignored_result(tokens: Sequence[Token]) -> bool:
    for index in range(len(tokens) - 2):
        if tokens[index].text == "let" and tokens[index + 1].text == "_" and tokens[index + 2].text == "=":
            return True
    return False


def _platform_tokens(tokens: Sequence[Token]) -> bool:
    values = {token.text for token in tokens}
    return "cfg" in values or bool(values & {"target_os", "target_arch"})


def _shared_when_instances(
    unit: SourceUnit,
    construct: Token,
    local_offsets: set[int],
) -> list[MarkerInstance]:
    comments = _contiguous_line_comments_before(unit, construct.line)
    if not comments or _marker_name(comments[0]) != "When":
        return []
    instances = [instance for instance in _marker_instances(comments) if instance.marker == "When"]
    return [
        instance
        for instance in instances
        if not any(comment.start in local_offsets for comment in instance.comments)
    ]


def _branch_marker(
    unit: SourceUnit,
    branch: Branch,
    shared_instances: Sequence[MarkerInstance],
    used: set[int],
    diagnostics: list[Diagnostic],
) -> None:
    local = _first_body_instances(unit, branch.body_start, branch.body_end, "When")
    _use_instances(shared_instances, used)
    _use_instances(local, used)
    if len(shared_instances) > 1 or len(local) > 1:
        anchor = (local or list(shared_instances))[1].anchor
        diagnostics.append(
            Diagnostic(
                unit.path,
                anchor.line,
                anchor.column,
                "when",
                "duplicate // When: markers for one contract unit",
            )
        )
        return
    shared = shared_instances[0] if shared_instances else None
    if shared and local:
        diagnostics.append(
            Diagnostic(
                unit.path,
                local[0].anchor.line,
                local[0].anchor.column,
                "when",
                "branch is covered by both shared and local // When:",
            )
        )
        return
    instances = local or ([shared] if shared else [])
    if not instances:
        diagnostics.append(
            Diagnostic(
                unit.path,
                branch.anchor.line,
                branch.anchor.column,
                "when",
                "mandatory branch needs substantive // When: as first body content or at its branch head",
            )
        )
        return
    instance = instances[0]
    if not _validate_instance_sizes(unit, instances, diagnostics):
        return
    if shared:
        if not branch.predicate:
            diagnostics.append(
                Diagnostic(
                    unit.path,
                    branch.anchor.line,
                    branch.anchor.column,
                    "when",
                    "shared // When: cannot cover a tokenless predicate",
                )
            )
            return
        if not (_exact_identifier_tokens(instance.body) & branch.predicate):
            diagnostics.append(
                Diagnostic(
                    unit.path,
                    branch.anchor.line,
                    branch.anchor.column,
                    "when",
                    "shared // When: does not name this branch predicate",
                )
            )
            return
    error = _validate_when_text(instance.body, branch.selecting, branch.predicate)
    if error:
        diagnostics.append(
            Diagnostic(
                unit.path,
                instance.anchor.line,
                instance.anchor.column,
                "when",
                error,
            )
        )


def _permitted_path(tokens: Sequence[Token], start: int, end: int) -> int | None:
    if start >= end or tokens[start].kind != "IDENT":
        return None
    cursor = start + 1
    while cursor + 1 < end and tokens[cursor].text == "::" and tokens[cursor + 1].kind == "IDENT":
        cursor += 2
    return cursor


def _permitted_index_key(tokens: Sequence[Token], start: int, end: int) -> bool:
    if start + 1 == end and tokens[start].kind == "NUMBER":
        return bool(
            re.fullmatch(
                r"(?:0[xX][0-9A-Fa-f_]+|0[oO][0-7_]+|0[bB][01_]+|[0-9][0-9_]*)(?:[ui](?:8|16|32|64|128|size))?",
                tokens[start].text,
            )
        )
    return _permitted_path(tokens, start, end) == end


def _parse_permitted_leaf(
    tokens: Sequence[Token],
    pairs: dict[int, int],
    start: int,
    end: int,
) -> int | None:
    if start >= end:
        return None
    cursor = start
    token = tokens[cursor]
    if token.text == "&":
        cursor += 1
        if cursor < end and tokens[cursor].text == "mut":
            cursor += 1
        return _parse_permitted_leaf(tokens, pairs, cursor, end)
    if token.text == "-":
        if cursor + 2 == end and tokens[cursor + 1].kind == "NUMBER" and ".." not in tokens[cursor + 1].text:
            return end
        return None
    if token.text == "(":
        close = pairs.get(cursor)
        if close is None or close >= end or _parse_permitted_leaf(tokens, pairs, cursor + 1, close) != close:
            return None
        cursor = close + 1
    elif token.kind in {"STRING", "CHAR"} or (token.kind == "NUMBER" and ".." not in token.text):
        cursor += 1
    else:
        parsed = _permitted_path(tokens, cursor, end)
        if parsed is None:
            return None
        cursor = parsed

    while cursor < end:
        if tokens[cursor].text == ".":
            if cursor + 1 >= end or tokens[cursor + 1].kind != "IDENT" or tokens[cursor + 1].text == "await":
                return None
            cursor += 2
            continue
        if tokens[cursor].text == "[":
            close = pairs.get(cursor)
            if close is None or close >= end or not _permitted_index_key(tokens, cursor + 1, close):
                return None
            cursor = close + 1
            continue
        return None
    return cursor


def _permitted_leaf(tokens: Sequence[Token], pairs: dict[int, int], start: int, end: int) -> bool:
    return _parse_permitted_leaf(tokens, pairs, start, end) == end


def _if_has_expression_parent(tokens: Sequence[Token], pairs: dict[int, int], index: int) -> bool:
    cursor = index - 1
    while cursor >= 1 and tokens[cursor].text == "]":
        opening = pairs.get(cursor)
        if opening is None or opening == 0:
            break
        marker = opening - 1
        if tokens[marker].text == "!":
            marker -= 1
        if marker < 0 or tokens[marker].text != "#":
            break
        cursor = marker - 1
    return cursor >= 0 and tokens[cursor].text not in {"{", "}", ";"}


def _top_level_token(
    tokens: Sequence[Token],
    pairs: dict[int, int],
    start: int,
    end: int,
    text: str,
) -> int | None:
    cursor = start
    while cursor < end:
        token = tokens[cursor]
        if token.text in OPEN_TO_CLOSE and cursor in pairs:
            cursor = pairs[cursor] + 1
            continue
        if token.text == text:
            return cursor
        cursor += 1
    return None


def _brace_starts_condition_expression(
    tokens: Sequence[Token],
    pairs: dict[int, int],
    condition_start: int,
    segment_start: int,
    brace: int,
) -> bool:
    if brace == condition_start:
        return True
    previous = tokens[brace - 1].text if brace > condition_start else ""
    macro_bang = (
        previous == "!"
        and brace >= 2
        and tokens[brace - 2].kind == "IDENT"
        and tokens[brace - 2].text not in RUST_KEYWORDS
    )
    if not macro_bang and previous in {
        "!",
        "!=",
        "%",
        "&",
        "&&",
        "*",
        "+",
        "-",
        "/",
        "<",
        "<<",
        "<=",
        "=",
        "==",
        ">",
        ">=",
        ">>",
        "^",
        "as",
        "async",
        "const",
        "loop",
        "move",
        "unsafe",
        "|",
        "||",
    }:
        return True
    prefix: list[Token] = []
    cursor = segment_start
    while cursor < brace:
        token = tokens[cursor]
        if token.text in OPEN_TO_CLOSE and cursor in pairs:
            cursor = pairs[cursor] + 1
            continue
        prefix.append(token)
        cursor += 1
    last_let = max((index for index, token in enumerate(prefix) if token.text == "let"), default=-1)
    last_assign = max((index for index, token in enumerate(prefix) if token.text == "="), default=-1)
    if last_let > last_assign:
        return True
    return False


def _if_expression_end(
    tokens: Sequence[Token],
    pairs: dict[int, int],
    if_index: int,
    end: int,
) -> int | None:
    body_open = _if_body_open(tokens, pairs, if_index + 1, end)
    if body_open is None or body_open not in pairs:
        return None
    cursor = pairs[body_open] + 1
    if cursor >= end or tokens[cursor].text != "else":
        return cursor
    cursor += 1
    if cursor < end and tokens[cursor].text == "if":
        return _if_expression_end(tokens, pairs, cursor, end)
    if cursor < end and tokens[cursor].text == "{" and cursor in pairs:
        return pairs[cursor] + 1
    return cursor


def _if_body_open(
    tokens: Sequence[Token],
    pairs: dict[int, int],
    start: int,
    end: int,
) -> int | None:
    cursor = start
    segment_start = start
    while cursor < end:
        token = tokens[cursor]
        if token.text in {"(", "["} and cursor in pairs:
            cursor = pairs[cursor] + 1
            continue
        if token.text == "match":
            nested = _match_body_open(tokens, pairs, cursor + 1, end)
            if nested is None or nested not in pairs:
                return None
            cursor = pairs[nested] + 1
            segment_start = cursor
            continue
        if token.text == "if":
            nested_end = _if_expression_end(tokens, pairs, cursor, end)
            if nested_end is None:
                return None
            cursor = nested_end
            segment_start = cursor
            continue
        if token.text != "{":
            cursor += 1
            continue
        close = pairs.get(cursor)
        if close is None:
            return None
        if _brace_starts_condition_expression(tokens, pairs, start, segment_start, cursor):
            cursor = close + 1
            segment_start = cursor
            continue
        return cursor
    return None


def _match_body_open(
    tokens: Sequence[Token],
    pairs: dict[int, int],
    start: int,
    end: int,
) -> int | None:
    cursor = start
    segment_start = start
    while cursor < end:
        token = tokens[cursor]
        if token.text in {"(", "["} and cursor in pairs:
            cursor = pairs[cursor] + 1
            continue
        if token.text == "match":
            nested = _match_body_open(tokens, pairs, cursor + 1, end)
            if nested is None or nested not in pairs:
                return None
            cursor = pairs[nested] + 1
            segment_start = cursor
            continue
        if token.text == "if":
            nested_end = _if_expression_end(tokens, pairs, cursor, end)
            if nested_end is None:
                return None
            cursor = nested_end
            segment_start = cursor
            continue
        if token.text != "{":
            cursor += 1
            continue
        close = pairs.get(cursor)
        if close is None:
            return None
        arrow = _top_level_token(tokens, pairs, cursor + 1, close, "=>")
        empty = cursor + 1 == close
        expression_block = _brace_starts_condition_expression(
            tokens,
            pairs,
            start,
            segment_start,
            cursor,
        )
        if arrow is not None or (empty and not expression_block):
            return cursor
        cursor = close + 1
        segment_start = cursor
    return None


def _if_branches(
    unit: SourceUnit,
    function: Function,
    suppressed: Sequence[tuple[int, int]],
    excluded: Sequence[tuple[int, int]] = (),
) -> list[Branch]:
    if function.body_open is None or function.body_close is None:
        return []
    tokens = unit.tokens
    result: list[Branch] = []
    chain_owner: dict[int, Token] = {}
    chain_predicates: dict[int, set[str]] = {}
    index = function.body_open + 1
    while index < function.body_close:
        token = tokens[index]
        if (
            token.text != "if"
            or _inside_ranges(token.start, excluded)
            or any(start <= token.start < end for start, end in suppressed)
        ):
            index += 1
            continue
        condition_start = index + 1
        body_open = _if_body_open(tokens, unit.pairs, condition_start, function.body_close)
        if body_open is None or body_open not in unit.pairs:
            index += 1
            continue
        body_close = unit.pairs[body_open]
        condition = _outside_ranges(tokens[condition_start:body_open], excluded)
        body = _outside_ranges(tokens[body_open + 1:body_close], excluded)
        previous_else = index > 0 and tokens[index - 1].text == "else"
        owner = chain_owner.get(index, token)
        prior = chain_predicates.get(index, set())
        predicate = _predicate_tokens(condition)
        chain_union = prior | predicate
        mandatory = (
            previous_else
            or not body
            or _contains(body, {"return", "break", "continue"})
            or _ignored_result(body)
            or _contains(body, {"unsafe"})
            or _platform_tokens(condition)
        )
        result.append(
            Branch(
                token,
                tokens[body_open].end,
                tokens[body_close].start,
                _selecting_tokens(condition),
                predicate,
                mandatory,
                False,
                owner,
            )
        )
        cursor = body_close + 1
        if cursor < len(tokens) and tokens[cursor].text == "else":
            if cursor + 1 < len(tokens) and tokens[cursor + 1].text == "if":
                chain_owner[cursor + 1] = owner
                chain_predicates[cursor + 1] = chain_union
            elif cursor + 1 < len(tokens) and tokens[cursor + 1].text == "{":
                else_open = cursor + 1
                else_close = unit.pairs.get(else_open)
                if else_close is not None:
                    else_body = _outside_ranges(tokens[else_open + 1:else_close], excluded)
                    value_selector = (
                        not previous_else
                        and _if_has_expression_parent(tokens, unit.pairs, index)
                        and _permitted_leaf(tokens, unit.pairs, body_open + 1, body_close)
                        and _permitted_leaf(tokens, unit.pairs, else_open + 1, else_close)
                    )
                    independent_mandatory = (
                        not else_body
                        or _contains(else_body, {"return", "break", "continue"})
                        or _ignored_result(else_body)
                        or _contains(else_body, {"unsafe"})
                        or _platform_tokens(condition)
                        or _platform_tokens(else_body)
                    )
                    result.append(
                        Branch(
                            tokens[cursor],
                            tokens[else_open].end,
                            tokens[else_close].start,
                            _selecting_tokens(condition),
                            chain_union,
                            independent_mandatory or not value_selector,
                            value_selector and not independent_mandatory,
                            owner,
                        )
                    )
        index += 1
    return result


def _statement_let(unit: SourceUnit, index: int) -> bool:
    cursor = index - 1
    while cursor >= 1 and unit.tokens[cursor].text == "]":
        opening = unit.pairs.get(cursor)
        if opening is None:
            break
        marker = opening - 1
        if marker >= 0 and unit.tokens[marker].text == "!":
            marker -= 1
        if marker < 0 or unit.tokens[marker].text != "#":
            break
        cursor = marker - 1
    return cursor < 0 or unit.tokens[cursor].text in {"{", "}", ";"}


def _let_else_branches(
    unit: SourceUnit,
    function: Function,
    excluded: Sequence[tuple[int, int]] = (),
) -> list[Branch]:
    if function.body_open is None or function.body_close is None:
        return []
    result = []
    tokens = unit.tokens
    for index in range(function.body_open + 1, function.body_close):
        if _inside_ranges(tokens[index].start, excluded) or tokens[index].text != "let":
            continue
        if not _statement_let(unit, index):
            continue
        cursor = index + 1
        while cursor < function.body_close:
            token = tokens[cursor]
            if token.text in OPEN_TO_CLOSE and cursor in unit.pairs:
                cursor = unit.pairs[cursor] + 1
                continue
            if token.text == "if":
                nested_end = _if_expression_end(tokens, unit.pairs, cursor, function.body_close)
                if nested_end is None:
                    break
                cursor = nested_end
                continue
            if token.text == "match":
                nested_body = _match_body_open(tokens, unit.pairs, cursor + 1, function.body_close)
                if nested_body is None or nested_body not in unit.pairs:
                    break
                cursor = unit.pairs[nested_body] + 1
                continue
            if token.text in {";", "else"}:
                break
            cursor += 1
        if cursor >= function.body_close or tokens[cursor].text != "else" or cursor + 1 >= len(tokens) or tokens[cursor + 1].text != "{":
            continue
        close = unit.pairs.get(cursor + 1)
        if close is None:
            continue
        body = _outside_ranges(tokens[cursor + 2:close], excluded)
        mandatory = _contains(body, {"return", "break", "continue"})
        selecting_tokens = tokens[index + 1:cursor]
        result.append(
            Branch(
                tokens[index],
                tokens[cursor + 1].end,
                tokens[close].start,
                _selecting_tokens(selecting_tokens),
                _predicate_tokens(selecting_tokens),
                mandatory,
                False,
                None,
            )
        )
    return result


def _match_arms(
    unit: SourceUnit,
    function: Function,
    excluded: Sequence[tuple[int, int]] = (),
) -> tuple[list[Branch], list[tuple[int, int]]]:
    if function.body_open is None or function.body_close is None:
        return [], []
    tokens = unit.tokens
    arms = []
    ranges = []
    index = function.body_open + 1
    while index < function.body_close:
        if _inside_ranges(tokens[index].start, excluded) or tokens[index].text != "match":
            index += 1
            continue
        body_open = _match_body_open(tokens, unit.pairs, index + 1, function.body_close)
        if body_open is None or body_open not in unit.pairs:
            index += 1
            continue
        body_close = unit.pairs[body_open]
        scrutinee = tokens[index + 1:body_open]
        arm_start = body_open + 1
        cursor = arm_start
        while cursor < body_close:
            arrow = _top_level_token(tokens, unit.pairs, cursor, body_close, "=>")
            if arrow is None:
                break
            pattern = tokens[cursor:arrow]
            if not pattern:
                break
            expression_start = arrow + 1
            if expression_start >= body_close:
                break
            if tokens[expression_start].text == "{":
                close = unit.pairs.get(expression_start)
                if close is None:
                    break
                body_tokens = _outside_ranges(tokens[expression_start + 1:close], excluded)
                content_start = tokens[expression_start].end
                content_end = tokens[close].start
                next_cursor = close + 1
            else:
                depth_cursor = expression_start
                while depth_cursor < body_close and tokens[depth_cursor].text != ",":
                    if tokens[depth_cursor].text in OPEN_TO_CLOSE and depth_cursor in unit.pairs:
                        depth_cursor = unit.pairs[depth_cursor]
                    depth_cursor += 1
                body_tokens = _outside_ranges(tokens[expression_start:depth_cursor], excluded)
                content_start = tokens[arrow].end
                content_end = tokens[depth_cursor].start if depth_cursor < body_close else tokens[body_close].start
                next_cursor = depth_cursor + 1
            no_op = not body_tokens or [token.text for token in body_tokens] == ["(", ")"]
            mandatory = no_op or _contains(body_tokens, {"return", "break", "continue"}) or _ignored_result(body_tokens) or _contains(body_tokens, {"unsafe"}) or _platform_tokens(pattern)
            selecting_tokens = scrutinee if pattern and pattern[0].text == "_" else pattern
            predicate = _predicate_tokens(scrutinee) | _predicate_tokens(pattern)
            arms.append(
                Branch(
                    pattern[0],
                    content_start,
                    content_end,
                    _selecting_tokens(selecting_tokens),
                    predicate,
                    mandatory,
                    False,
                    tokens[index],
                )
            )
            # Match guards use `if` inside arm-selection syntax, so suppress only
            # the pattern/guard span. Branches inside the arm body remain separate
            # constructs with their own reachability contracts.
            ranges.append((pattern[0].start, tokens[arrow].end))
            cursor = next_cursor
        index += 1
    return arms, ranges


def _analyze_unit(unit: SourceUnit, selected: bool) -> tuple[list[Diagnostic], list[Diagnostic], dict]:
    diagnostics: list[Diagnostic] = []
    semantic: list[Diagnostic] = []
    used_markers: set[int] = set()
    traits, impls, public_types = _find_traits_and_impls(unit)
    functions = _functions(unit)
    counts = {
        "public_candidates": 0,
        "branches_mandatory": 0,
        "branches_exempt": 0,
        "branches_advisory": 0,
        "branches_value_selectors": 0,
        "unsafe_required": 0,
        "lock_required": 0,
        "ordering_required": 0,
        "lifecycle_required": 0,
    }

    for function in functions:
        effective, trait_member = _function_effective_visibility(unit, function, traits, impls, public_types)
        function.effective_public = effective
        function.public_trait_member = trait_member
        if effective and not unit.excluded and not function.test_context and not unit.binary_context:
            counts["public_candidates"] += 1
            comments, attributes, _ = _prelude(unit, function.start_token.start)
            docs = _doc_text(unit, comments, attributes)
            if not docs and selected:
                message = "public trait function needs purpose rustdoc" if trait_member else "effectively public function needs purpose rustdoc"
                diagnostics.append(Diagnostic(unit.path, function.start_token.line, function.start_token.column, "public-doc", message))
            if function.unsafe and "# Safety" not in docs and selected:
                diagnostics.append(Diagnostic(unit.path, function.start_token.line, function.start_token.column, "safety-doc", "effectively public unsafe function rustdoc needs a # Safety section"))

        if function.unsafe:
            counts["unsafe_required"] += 1
            if selected:
                _attach_marker(unit, function.start_token, "SAFETY", used_markers, diagnostics, "unsafe function needs one substantive // SAFETY: marker immediately above")

        if not (function.test_context or unit.file_test_context or unit.binary_context):
            nested_ranges = _nested_function_ranges(function, functions)
            body_tokens = _function_body_tokens(function)
            if body_tokens:
                lock_ids = _lock_identifiers(body_tokens, nested_ranges)
                if len(lock_ids) >= 2:
                    counts["lock_required"] += 1
                    if selected:
                        _attach_marker(
                            unit,
                            function.start_token,
                            "Lock order",
                            used_markers,
                            diagnostics,
                            "function with multiple lock identifiers needs // Lock order: immediately above",
                            _lock_validator(lock_ids),
                        )
                atomic_sites = _atomic_sites(body_tokens, nested_ranges)
                non_seqcst = any(orderings - {"SeqCst"} for _, orderings in atomic_sites)
                ordering_markers = _strict_prelude_instances(
                    unit,
                    function.start_token,
                    "Ordering",
                )
                if non_seqcst:
                    counts["ordering_required"] += 1
                if selected and (non_seqcst or ordering_markers):
                    _attach_marker(
                        unit,
                        function.start_token,
                        "Ordering",
                        used_markers,
                        diagnostics,
                        "function using non-SeqCst atomic ordering needs // Ordering: immediately above",
                        _ordering_validator(atomic_sites),
                    )

            arms, suppressed = _match_arms(unit, function, nested_ranges)
            branch_rows = (
                arms
                + _let_else_branches(unit, function, nested_ranges)
                + _if_branches(unit, function, suppressed, nested_ranges)
            )
            local_offsets = {
                comment.start
                for branch in branch_rows
                if branch.mandatory
                for instance in _first_body_instances(
                    unit,
                    branch.body_start,
                    branch.body_end,
                    "When",
                )
                for comment in instance.comments
            }
            shared_by_construct: dict[int, list[MarkerInstance]] = {}
            for branch in branch_rows:
                shared: list[MarkerInstance] = []
                if branch.construct is not None:
                    key = branch.construct.start
                    if key not in shared_by_construct:
                        shared_by_construct[key] = _shared_when_instances(
                            unit,
                            branch.construct,
                            local_offsets,
                        )
                    shared = shared_by_construct[key]
                if branch.mandatory:
                    counts["branches_mandatory"] += 1
                    if selected:
                        _branch_marker(unit, branch, shared, used_markers, diagnostics)
                else:
                    counts["branches_exempt"] += 1
                    counts["branches_advisory"] += 1
                    if branch.value_selector:
                        counts["branches_value_selectors"] += 1
                    if selected:
                        message = (
                            "value-selection branch has no mandatory // When: requirement"
                            if branch.value_selector
                            else "ordinary branch has no mandatory // When: requirement"
                        )
                        semantic.append(
                            Diagnostic(
                                unit.path,
                                branch.anchor.line,
                                branch.anchor.column,
                                "when-advisory",
                                message,
                            )
                        )

    for index, token in enumerate(unit.tokens):
        if token.text != "unsafe":
            continue
        next_text = unit.tokens[index + 1].text if index + 1 < len(unit.tokens) else ""
        if next_text == "fn":
            continue
        kind = None
        if next_text == "{":
            kind = "block"
        elif next_text == "impl":
            kind = "impl"
        elif next_text == "extern":
            cursor = index + 2
            if cursor < len(unit.tokens) and unit.tokens[cursor].kind == "STRING":
                cursor += 1
            if cursor >= len(unit.tokens) or unit.tokens[cursor].text != "fn":
                kind = "extern block"
        if kind:
            counts["unsafe_required"] += 1
            if selected:
                _attach_marker(unit, token, "SAFETY", used_markers, diagnostics, f"unsafe {kind} needs one substantive // SAFETY: marker immediately above")

    for start, end, target, is_drop, impl_index in impls:
        impl_token = unit.tokens[impl_index]
        if not is_drop or _non_safety_exempt_at(unit, impl_token.start, functions):
            continue
        counts["lifecycle_required"] += 1
        if selected:
            body_tokens = [token for token in unit.tokens if start <= token.start < end]
            _attach_marker(
                unit,
                impl_token,
                "Lifecycle",
                used_markers,
                diagnostics,
                "Drop impl needs // Lifecycle: immediately above",
                _lifecycle_validator(target, _release_tokens(body_tokens)),
            )

    if selected:
        detached_messages = {
            "When": "// When: marker is not attached to a mandatory branch",
            "SAFETY": "// SAFETY: marker is not attached to an unsafe construct",
            "Lock order": "// Lock order: marker is not attached to a qualifying function",
            "Ordering": "// Ordering: marker is not attached to a qualifying function",
            "Lifecycle": "// Lifecycle: marker is not attached to a Drop impl",
        }
        for comment in unit.lexed.comments:
            marker = _marker_name(comment)
            if not marker or comment.start in used_markers:
                continue
            if marker != "SAFETY" and _non_safety_exempt_at(unit, comment.start, functions):
                continue
            diagnostics.append(
                Diagnostic(
                    unit.path,
                    comment.line,
                    comment.column,
                    _rule_for_marker(marker),
                    detached_messages[marker],
                )
            )

    return diagnostics, semantic, counts


def _crate_key(path: str) -> str:
    parts = path.split("/")
    return parts[1] if len(parts) > 2 and parts[0] == "crates" else "(root)"


def analyze_repository(root: Path, paths: list[str] | None = None, caller_cwd: Path | None = None) -> Report:
    """Analyze one tracked repository with optional diagnostic path filtering."""
    root = root.resolve()
    caller_cwd = (caller_cwd or Path.cwd()).resolve()
    tracked_paths = _tracked_rust(root)
    if not tracked_paths:
        raise AnalysisError("no tracked Rust files")
    units = {path: _source_unit(root, path) for path in tracked_paths}
    resolution_diagnostics: list[Diagnostic] = []
    _build_modules(units, set(tracked_paths), resolution_diagnostics)
    check_paths = [path for path, unit in units.items() if not unit.excluded and not unit.file_test_context]
    analysis_paths = [path for path, unit in units.items() if not unit.excluded]
    if not analysis_paths:
        raise AnalysisError("no in-scope tracked Rust files")
    selected_paths: set[str] | None = None
    if paths:
        selected_paths = {_normalize_path(root, value, caller_cwd) for value in paths}
        expanded: set[str] = set()
        for selected in selected_paths:
            selected_path = root / selected
            if selected_path.is_dir():
                expanded.update(path for path in tracked_paths if path.startswith(selected.rstrip("/") + "/"))
            else:
                expanded.add(selected)
        selected_paths = expanded

    diagnostics = [item for item in resolution_diagnostics if selected_paths is None or item.path in selected_paths]
    semantic: list[Diagnostic] = []
    aggregate = {
        "public_candidates": 0,
        "branches_mandatory": 0,
        "branches_exempt": 0,
        "branches_advisory": 0,
        "branches_value_selectors": 0,
        "unsafe_required": 0,
        "lock_required": 0,
        "ordering_required": 0,
        "lifecycle_required": 0,
    }
    for path in sorted(analysis_paths):
        unit = units[path]
        selected = selected_paths is None or path in selected_paths
        unit_diagnostics, unit_semantic, counts = _analyze_unit(unit, selected)
        diagnostics.extend(unit_diagnostics)
        semantic.extend(unit_semantic)
        if selected:
            for key, value in counts.items():
                aggregate[key] += value
    diagnostics.sort()
    semantic.sort()
    excluded = sorted(path for path, unit in units.items() if unit.excluded)
    test_context = sorted(path for path, unit in units.items() if unit.file_test_context)
    value_selectors_by_crate: dict[str, int] = {}
    for candidate in semantic:
        if "value-selection branch" not in candidate.message:
            continue
        crate = _crate_key(candidate.path)
        value_selectors_by_crate[crate] = value_selectors_by_crate.get(crate, 0) + 1
    counts = {
        "files": {"resolution": len(tracked_paths), "check": len(check_paths), "excluded": len(excluded), "test_context": len(test_context)},
        "public_docs": {"candidates": aggregate["public_candidates"], "missing": sum(item.rule == "public-doc" for item in diagnostics)},
        "branches": {
            "mandatory": aggregate["branches_mandatory"],
            "exempt": aggregate["branches_exempt"],
            "advisory": aggregate["branches_advisory"],
            "value_selectors": aggregate["branches_value_selectors"],
            "value_selectors_by_crate": dict(sorted(value_selectors_by_crate.items())),
        },
        "unsafe": {"required": aggregate["unsafe_required"]},
        "lock_order": {"required": aggregate["lock_required"]},
        "ordering": {"required": aggregate["ordering_required"]},
        "lifecycle": {"required": aggregate["lifecycle_required"]},
    }
    return Report(
        diagnostics,
        semantic,
        counts,
        {"resolution": sorted(tracked_paths), "check": sorted(check_paths), "excluded": excluded, "test_context": test_context},
    )


def _repository_root(cwd: Path) -> Path:
    completed = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise AnalysisError(completed.stderr.decode("utf-8", errors="replace").strip())
    return Path(completed.stdout.decode("utf-8").strip()).resolve()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--check", action="store_true")
    modes.add_argument("--inventory", action="store_true")
    modes.add_argument("--semantic-candidates", action="store_true")
    parser.add_argument("--paths", nargs="+")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    """Run the selected checker mode from any directory in the repository."""
    args = _parser().parse_args(argv)
    try:
        root = _repository_root(Path.cwd())
        report = analyze_repository(root, paths=args.paths, caller_cwd=Path.cwd())
    except (AnalysisError, OSError) as error:
        print(f"check-authored-rust-comments: {error}", file=sys.stderr)
        return 2
    if args.inventory:
        print(json.dumps(report.inventory(), sort_keys=True, indent=2))
        return 0
    if args.semantic_candidates:
        for diagnostic in report.semantic_candidates:
            print(diagnostic.format())
        return 0
    for diagnostic in report.diagnostics:
        print(diagnostic.format())
    return 1 if report.diagnostics else 0


if __name__ == "__main__":
    raise SystemExit(main())
