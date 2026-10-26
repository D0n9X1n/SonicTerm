"""Rust comment markers."""

from __future__ import annotations
import re
from typing import Iterable, Iterator, Sequence

import rust_comment_model


def _marker_name(comment: rust_comment_model.Comment) -> str | None:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    stripped = comment.text.strip()
    for marker in rust_comment_model.MARKERS:
        if stripped.startswith(f"// {marker}:"):
            return marker
    return None


def _marker_body(comment: rust_comment_model.Comment, marker: str) -> str:
    return comment.text.strip()[len(f"// {marker}:"):].strip()


def _substantive(comment: rust_comment_model.Comment, marker: str) -> bool:
    return bool(_semantic_tokens(_marker_body(comment, marker)))


def _exact_identifier_tokens(text: str) -> set[str]:
    """Return exact Rust-like identifiers after removing non-discriminating words."""
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    return {
        token
        for token in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", text.replace("`", ""))
        if token not in rust_comment_model.IDENTIFIER_STOPLIST
    }


def _ordinary_line_comment(comment: rust_comment_model.Comment) -> bool:
    stripped = comment.text.lstrip()
    return stripped.startswith("//") and not stripped.startswith("///") and "\n" not in comment.text


def _contiguous_line_comments_before(unit: rust_comment_model.SourceUnit, target_line: int) -> list[rust_comment_model.Comment]:
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


def _marker_instances(comments: Sequence[rust_comment_model.Comment]) -> list[rust_comment_model.MarkerInstance]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    instances: list[rust_comment_model.MarkerInstance] = []
    index = 0
    while index < len(comments):
        marker = _marker_name(comments[index])
        if marker is None:
            index += 1
            continue
        end = index + 1
        while end < len(comments) and _marker_name(comments[end]) is None:
            end += 1
        instances.append(rust_comment_model.MarkerInstance(marker, tuple(comments[index:end])))
        index = end
    return instances


def _strict_prelude_instances(
    unit: rust_comment_model.SourceUnit,
    token: rust_comment_model.Token,
    marker: str,
) -> list[rust_comment_model.MarkerInstance]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_modules
    _, attributes = rust_comment_modules._preceding_attributes(unit, token)
    target_line = token.line
    if attributes:
        target_line = min(unit.lexed.line_column(span.start)[0] for span in attributes)
    comments = _contiguous_line_comments_before(unit, target_line)
    return [instance for instance in _marker_instances(comments) if instance.marker == marker]


def _first_body_instances(
    unit: rust_comment_model.SourceUnit,
    start: int,
    end: int,
    marker: str,
) -> list[rust_comment_model.MarkerInstance]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_branches
    comments = rust_comment_branches._first_body_comments(unit, start, end)
    if not comments or _marker_name(comments[0]) != marker:
        return []
    contiguous = [comments[0]]
    for comment in comments[1:]:
        if comment.line != contiguous[-1].line + 1:
            break
        contiguous.append(comment)
    return [instance for instance in _marker_instances(contiguous) if instance.marker == marker]


def _marker_size_error(instance: rust_comment_model.MarkerInstance) -> str | None:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    if instance.marker not in rust_comment_model.CONTRACT_MARKERS:
        return None
    if len(instance.comments) > rust_comment_model.MAX_MARKER_LINES:
        return "marker exceeds 2 lines or 160 characters"
    pieces = [_marker_body(instance.comments[0], instance.marker)]
    pieces.extend(comment.text.strip()[2:].strip() for comment in instance.comments[1:])
    if sum(len(piece) for piece in pieces) > rust_comment_model.MAX_MARKER_CHARACTERS:
        return "marker exceeds 2 lines or 160 characters"
    return None


def _semantic_tokens(text: str) -> set[str]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    text = re.sub(r"^(?:When|SAFETY|Lock order|Ordering|Lifecycle)\s*:\s*", "", text, flags=re.I)
    words: list[str] = []
    for raw in re.findall(r"[A-Za-z_][A-Za-z0-9_]*|\d+(?:\.\d+)?", text):
        for underscore in raw.split("_"):
            pieces = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", underscore).split()
            words.extend(piece.lower() for piece in pieces if piece)
    return {word for word in words if word not in rust_comment_model.STOPWORDS}


def _comments_between(unit: rust_comment_model.SourceUnit, start: int, end: int) -> list[rust_comment_model.Comment]:
    return [comment for comment in unit.lexed.comments if start <= comment.start and comment.end <= end]


def _first_nonspace_offset(unit: rust_comment_model.SourceUnit, start: int, end: int) -> int:
    cursor = start
    while cursor < end and unit.text[cursor].isspace():
        cursor += 1
    return cursor
