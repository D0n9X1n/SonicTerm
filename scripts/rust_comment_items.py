"""Rust comment items."""

from __future__ import annotations
from bisect import bisect_right
from typing import Iterable, Iterator, Sequence

import rust_comment_model


def _matching(unit: rust_comment_model.SourceUnit, index: int) -> int | None:
    return unit.pairs.get(index)


def _token_index_at_or_after(tokens: Sequence[rust_comment_model.Token], offset: int) -> int:
    starts = [token.start for token in tokens]
    return bisect_right(starts, offset - 1)


def _find_body(tokens: Sequence[rust_comment_model.Token], pairs: dict[int, int], fn_index: int) -> tuple[int | None, int | None]:
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


def _signature_start(tokens: Sequence[rust_comment_model.Token], fn_index: int) -> int:
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


def _reverse_match(tokens: Sequence[rust_comment_model.Token], close_index: int, opening: str, closing: str) -> int | None:
    depth = 0
    for index in range(close_index, -1, -1):
        if tokens[index].text == closing:
            depth += 1
        elif tokens[index].text == opening:
            depth -= 1
            if depth == 0:
                return index
    return None


def _lexical_visibility(tokens: Sequence[rust_comment_model.Token], start: int, fn_index: int) -> tuple[bool, bool, bool]:
    segment = tokens[start:fn_index]
    public = any(token.text == "pub" for token in segment)
    restricted = False
    for index, token in enumerate(segment):
        if token.text == "pub" and index + 1 < len(segment) and segment[index + 1].text == "(":
            restricted = True
    unsafe = any(token.text == "unsafe" for token in segment)
    return public, restricted, unsafe


def _top_level_indices(unit: rust_comment_model.SourceUnit, start_index: int, end_index: int) -> Iterator[int]:
    index = start_index
    while index < end_index:
        yield index
        token = unit.tokens[index]
        if token.text in rust_comment_model.OPEN_TO_CLOSE and index in unit.pairs:
            index = unit.pairs[index] + 1
        else:
            index += 1
