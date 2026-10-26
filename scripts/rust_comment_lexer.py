"""Rust comment lexer."""

from __future__ import annotations
from bisect import bisect_right
import re
from typing import Iterable, Iterator, Sequence

import rust_comment_model


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


def lex_rust(source: str) -> rust_comment_model.Lexed:
    """Tokenize Rust while excluding comments, strings, and macro token trees."""
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    starts = _line_starts(source)
    tokens: list[Token] = []
    comments: list[rust_comment_model.Comment] = []
    index = 0
    length = len(source)

    def add(kind: str, start: int, end: int) -> None:
        # Resolve this runtime-only dependency after module initialization.
        import rust_comment_model
        line, column = _location(starts, start)
        tokens.append(rust_comment_model.Token(kind, source[start:end], start, end, line, column))

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
            comments.append(rust_comment_model.Comment(source[index:end], index, end, line, column))
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
            comments.append(rust_comment_model.Comment(source[index:cursor], index, cursor, line, column))
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
            or macro_name.text in rust_comment_model.RUST_KEYWORDS
            or tokens[token_index + 1].text not in rust_comment_model.OPEN_TO_CLOSE
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
    return rust_comment_model.Lexed(source, tokens, comments, starts)


def _delimiter_pairs(tokens: Sequence[rust_comment_model.Token]) -> dict[int, int]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    stack: list[tuple[str, int]] = []
    pairs: dict[int, int] = {}
    for index, token in enumerate(tokens):
        if token.text in rust_comment_model.OPEN_TO_CLOSE:
            stack.append((token.text, index))
        elif token.text in rust_comment_model.CLOSE_TO_OPEN:
            wanted = rust_comment_model.CLOSE_TO_OPEN[token.text]
            if stack and stack[-1][0] == wanted:
                _, opening = stack.pop()
                pairs[opening] = index
                pairs[index] = opening
    return pairs


def _attributes(tokens: Sequence[rust_comment_model.Token], pairs: dict[int, int]) -> list[rust_comment_model.Span]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    spans: list[rust_comment_model.Span] = []
    for index, token in enumerate(tokens[:-1]):
        if token.text != "#" or tokens[index + 1].text != "[":
            continue
        close = pairs.get(index + 1)
        if close is not None:
            spans.append(rust_comment_model.Span(token.start, tokens[close].end))
    return spans
