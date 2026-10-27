"""Rust comment control flow."""

from __future__ import annotations
import re
from typing import Iterable, Iterator, Sequence

import rust_comment_branches
import rust_comment_contracts
import rust_comment_model


def _permitted_path(tokens: Sequence[rust_comment_model.Token], start: int, end: int) -> int | None:
    if start >= end or tokens[start].kind != "IDENT":
        return None
    cursor = start + 1
    while cursor + 1 < end and tokens[cursor].text == "::" and tokens[cursor + 1].kind == "IDENT":
        cursor += 2
    return cursor


def _permitted_index_key(tokens: Sequence[rust_comment_model.Token], start: int, end: int) -> bool:
    if start + 1 == end and tokens[start].kind == "NUMBER":
        return bool(
            re.fullmatch(
                r"(?:0[xX][0-9A-Fa-f_]+|0[oO][0-7_]+|0[bB][01_]+|[0-9][0-9_]*)(?:[ui](?:8|16|32|64|128|size))?",
                tokens[start].text,
            )
        )
    return _permitted_path(tokens, start, end) == end


def _parse_permitted_leaf(
    tokens: Sequence[rust_comment_model.Token],
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


def _permitted_leaf(tokens: Sequence[rust_comment_model.Token], pairs: dict[int, int], start: int, end: int) -> bool:
    return _parse_permitted_leaf(tokens, pairs, start, end) == end


def _if_has_expression_parent(tokens: Sequence[rust_comment_model.Token], pairs: dict[int, int], index: int) -> bool:
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
    tokens: Sequence[rust_comment_model.Token],
    pairs: dict[int, int],
    start: int,
    end: int,
    text: str,
) -> int | None:
    cursor = start
    while cursor < end:
        token = tokens[cursor]
        if token.text in rust_comment_model.OPEN_TO_CLOSE and cursor in pairs:
            cursor = pairs[cursor] + 1
            continue
        if token.text == text:
            return cursor
        cursor += 1
    return None


def _brace_starts_condition_expression(
    tokens: Sequence[rust_comment_model.Token],
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
        and tokens[brace - 2].text not in rust_comment_model.RUST_KEYWORDS
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
        if token.text in rust_comment_model.OPEN_TO_CLOSE and cursor in pairs:
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
    tokens: Sequence[rust_comment_model.Token],
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
    tokens: Sequence[rust_comment_model.Token],
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
    tokens: Sequence[rust_comment_model.Token],
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
    unit: rust_comment_model.SourceUnit,
    function: rust_comment_model.Function,
    suppressed: Sequence[tuple[int, int]],
    excluded: Sequence[tuple[int, int]] = (),
) -> list[rust_comment_model.Branch]:
    if function.body_open is None or function.body_close is None:
        return []
    tokens = unit.tokens
    result: list[rust_comment_model.Branch] = []
    chain_owner: dict[int, Token] = {}
    chain_predicates: dict[int, set[str]] = {}
    index = function.body_open + 1
    while index < function.body_close:
        token = tokens[index]
        if (
            token.text != "if"
            or rust_comment_contracts._inside_ranges(token.start, excluded)
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
        condition = rust_comment_contracts._outside_ranges(tokens[condition_start:body_open], excluded)
        body = rust_comment_contracts._outside_ranges(tokens[body_open + 1:body_close], excluded)
        previous_else = index > 0 and tokens[index - 1].text == "else"
        owner = chain_owner.get(index, token)
        prior = chain_predicates.get(index, set())
        predicate = rust_comment_branches._predicate_tokens(condition)
        chain_union = prior | predicate
        mandatory = (
            previous_else
            or not body
            or rust_comment_branches._contains(body, {"return", "break", "continue"})
            or rust_comment_branches._ignored_result(body)
            or rust_comment_branches._contains(body, {"unsafe"})
            or rust_comment_branches._platform_tokens(condition)
        )
        result.append(
            rust_comment_model.Branch(
                token,
                tokens[body_open].end,
                tokens[body_close].start,
                rust_comment_branches._selecting_tokens(condition),
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
                    else_body = rust_comment_contracts._outside_ranges(tokens[else_open + 1:else_close], excluded)
                    value_selector = (
                        not previous_else
                        and _if_has_expression_parent(tokens, unit.pairs, index)
                        and _permitted_leaf(tokens, unit.pairs, body_open + 1, body_close)
                        and _permitted_leaf(tokens, unit.pairs, else_open + 1, else_close)
                    )
                    independent_mandatory = (
                        not else_body
                        or rust_comment_branches._contains(else_body, {"return", "break", "continue"})
                        or rust_comment_branches._ignored_result(else_body)
                        or rust_comment_branches._contains(else_body, {"unsafe"})
                        or rust_comment_branches._platform_tokens(condition)
                        or rust_comment_branches._platform_tokens(else_body)
                    )
                    result.append(
                        rust_comment_model.Branch(
                            tokens[cursor],
                            tokens[else_open].end,
                            tokens[else_close].start,
                            rust_comment_branches._selecting_tokens(condition),
                            chain_union,
                            independent_mandatory or not value_selector,
                            value_selector and not independent_mandatory,
                            owner,
                        )
                    )
        index += 1
    return result


def _statement_let(unit: rust_comment_model.SourceUnit, index: int) -> bool:
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
    unit: rust_comment_model.SourceUnit,
    function: rust_comment_model.Function,
    excluded: Sequence[tuple[int, int]] = (),
) -> list[rust_comment_model.Branch]:
    if function.body_open is None or function.body_close is None:
        return []
    result = []
    tokens = unit.tokens
    for index in range(function.body_open + 1, function.body_close):
        if rust_comment_contracts._inside_ranges(tokens[index].start, excluded) or tokens[index].text != "let":
            continue
        if not _statement_let(unit, index):
            continue
        cursor = index + 1
        while cursor < function.body_close:
            token = tokens[cursor]
            if token.text in rust_comment_model.OPEN_TO_CLOSE and cursor in unit.pairs:
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
        body = rust_comment_contracts._outside_ranges(tokens[cursor + 2:close], excluded)
        mandatory = rust_comment_branches._contains(body, {"return", "break", "continue"})
        selecting_tokens = tokens[index + 1:cursor]
        result.append(
            rust_comment_model.Branch(
                tokens[index],
                tokens[cursor + 1].end,
                tokens[close].start,
                rust_comment_branches._selecting_tokens(selecting_tokens),
                rust_comment_branches._predicate_tokens(selecting_tokens),
                mandatory,
                False,
                None,
            )
        )
    return result


def _match_arms(
    unit: rust_comment_model.SourceUnit,
    function: rust_comment_model.Function,
    excluded: Sequence[tuple[int, int]] = (),
) -> tuple[list[rust_comment_model.Branch], list[tuple[int, int]]]:
    if function.body_open is None or function.body_close is None:
        return [], []
    tokens = unit.tokens
    arms = []
    ranges = []
    index = function.body_open + 1
    while index < function.body_close:
        if rust_comment_contracts._inside_ranges(tokens[index].start, excluded) or tokens[index].text != "match":
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
                body_tokens = rust_comment_contracts._outside_ranges(tokens[expression_start + 1:close], excluded)
                content_start = tokens[expression_start].end
                content_end = tokens[close].start
                next_cursor = close + 1
            else:
                depth_cursor = expression_start
                while depth_cursor < body_close and tokens[depth_cursor].text != ",":
                    if tokens[depth_cursor].text in rust_comment_model.OPEN_TO_CLOSE and depth_cursor in unit.pairs:
                        depth_cursor = unit.pairs[depth_cursor]
                    depth_cursor += 1
                body_tokens = rust_comment_contracts._outside_ranges(tokens[expression_start:depth_cursor], excluded)
                content_start = tokens[arrow].end
                content_end = tokens[depth_cursor].start if depth_cursor < body_close else tokens[body_close].start
                next_cursor = depth_cursor + 1
            no_op = not body_tokens or [token.text for token in body_tokens] == ["(", ")"]
            mandatory = no_op or rust_comment_branches._contains(body_tokens, {"return", "break", "continue"}) or rust_comment_branches._ignored_result(body_tokens) or rust_comment_branches._contains(body_tokens, {"unsafe"}) or rust_comment_branches._platform_tokens(pattern)
            selecting_tokens = scrutinee if pattern and pattern[0].text == "_" else pattern
            predicate = rust_comment_branches._predicate_tokens(scrutinee) | rust_comment_branches._predicate_tokens(pattern)
            arms.append(
                rust_comment_model.Branch(
                    pattern[0],
                    content_start,
                    content_end,
                    rust_comment_branches._selecting_tokens(selecting_tokens),
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
