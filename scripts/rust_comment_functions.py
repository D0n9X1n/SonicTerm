"""Rust comment functions."""

from __future__ import annotations
import re
from typing import Iterable, Iterator, Sequence

import rust_comment_items
import rust_comment_model
import rust_comment_modules
import rust_comment_sources


def _find_traits_and_impls(
    unit: rust_comment_model.SourceUnit,
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
                start = rust_comment_items._signature_start(tokens, index)
                public, restricted, _ = rust_comment_items._lexical_visibility(tokens, start, index)
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


def _functions(unit: rust_comment_model.SourceUnit) -> list[rust_comment_model.Function]:
    functions: list[rust_comment_model.Function] = []
    for index, token in enumerate(unit.tokens):
        if token.text != "fn" or index + 1 >= len(unit.tokens) or unit.tokens[index + 1].kind != "IDENT":
            continue
        start = rust_comment_items._signature_start(unit.tokens, index)
        public, restricted, unsafe = rust_comment_items._lexical_visibility(unit.tokens, start, index)
        body_open, body_close = rust_comment_items._find_body(unit.tokens, unit.pairs, index)
        functions.append(
            rust_comment_model.Function(
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
        function.test_context = unit.file_test_context or rust_comment_modules._direct_test_item(unit, function.start_token)
        module = unit.deepest_module(offset)
        if module and module.test_context:
            function.test_context = True
    return functions


def _non_safety_exempt_at(unit: rust_comment_model.SourceUnit, offset: int, functions: Sequence[rust_comment_model.Function]) -> bool:
    if unit.file_test_context or unit.binary_context:
        return True
    module = unit.deepest_module(offset)
    if module and module.test_context:
        return True
    for function in functions:
        if not function.test_context:
            continue
        _, _, prelude_start = rust_comment_sources._prelude(unit, function.start_token.start)
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


def _function_effective_visibility(unit: rust_comment_model.SourceUnit, function: rust_comment_model.Function, traits, impls, public_types) -> tuple[bool, bool]:
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
