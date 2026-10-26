"""Rust comment contracts."""

from __future__ import annotations
import re
from typing import Iterable, Iterator, Sequence

import rust_comment_model


def _comment_markers(comments: Sequence[rust_comment_model.Comment], marker: str) -> list[rust_comment_model.Comment]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_markers
    return [comment for comment in comments if rust_comment_markers._marker_name(comment) == marker]


def _use_instances(instances: Sequence[rust_comment_model.MarkerInstance], used: set[int]) -> None:
    for instance in instances:
        for comment in instance.comments:
            used.add(comment.start)


def _instance_tokens(instances: Sequence[rust_comment_model.MarkerInstance]) -> set[str]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_markers
    tokens: set[str] = set()
    for instance in instances:
        tokens.update(rust_comment_markers._exact_identifier_tokens(instance.body))
    return tokens


def _validate_instance_sizes(
    unit: rust_comment_model.SourceUnit,
    instances: Sequence[rust_comment_model.MarkerInstance],
    diagnostics: list[rust_comment_model.Diagnostic],
) -> bool:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_markers
    import rust_comment_model
    valid = True
    for instance in instances:
        error = rust_comment_markers._marker_size_error(instance)
        if error:
            diagnostics.append(
                rust_comment_model.Diagnostic(
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
    unit: rust_comment_model.SourceUnit,
    token: rust_comment_model.Token,
    marker: str,
    used: set[int],
    diagnostics: list[rust_comment_model.Diagnostic],
    missing_message: str,
    validator=None,
) -> None:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_markers
    import rust_comment_model
    import rust_comment_sources
    if marker == "SAFETY":
        comments, _, _ = rust_comment_sources._prelude(unit, token.start)
        matches = _comment_markers(comments, marker)
        if len(matches) == 1 and rust_comment_markers._substantive(matches[0], marker):
            used.add(matches[0].start)
            return
        for comment in matches:
            used.add(comment.start)
        if len(matches) > 1:
            anchor = matches[0]
            diagnostics.append(rust_comment_model.Diagnostic(unit.path, anchor.line, anchor.column, _rule_for_marker(marker), f"duplicate // {marker}: markers for one contract unit"))
        else:
            diagnostics.append(rust_comment_model.Diagnostic(unit.path, token.line, token.column, _rule_for_marker(marker), missing_message))
        return

    instances = rust_comment_markers._strict_prelude_instances(unit, token, marker)
    _use_instances(instances, used)
    if not instances:
        diagnostics.append(
            rust_comment_model.Diagnostic(unit.path, token.line, token.column, _rule_for_marker(marker), missing_message)
        )
        return
    if not _validate_instance_sizes(unit, instances, diagnostics):
        return
    if not all(instance.body.strip() for instance in instances):
        diagnostics.append(
            rust_comment_model.Diagnostic(
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
                rust_comment_model.Diagnostic(
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
    function: rust_comment_model.Function,
    functions: Sequence[rust_comment_model.Function],
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


def _function_body_tokens(function: rust_comment_model.Function) -> list[rust_comment_model.Token]:
    if function.body_open is None or function.body_close is None:
        return []
    return function.unit.tokens[function.body_open + 1:function.body_close]


def _inside_ranges(offset: int, ranges: Sequence[tuple[int, int]]) -> bool:
    return any(start <= offset < end for start, end in ranges)


def _outside_ranges(
    tokens: Sequence[rust_comment_model.Token],
    ranges: Sequence[tuple[int, int]],
) -> list[rust_comment_model.Token]:
    return [token for token in tokens if not _inside_ranges(token.start, ranges)]


def _receiver_identifier(
    tokens: Sequence[rust_comment_model.Token],
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
    tokens: Sequence[rust_comment_model.Token],
    excluded: Sequence[tuple[int, int]] = (),
) -> set[str]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_lexer
    pairs = rust_comment_lexer._delimiter_pairs(tokens)
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
    identifiers.update(_counted_lock_identifiers(tokens, pairs, excluded))
    return identifiers


# Helpers that lock the mutex named by their first argument: `lock_parser(&pane.parser)`
# acquires `parser` exactly as `pane.parser.lock()` does.
COUNTED_LOCK_HELPERS = frozenset({"lock_parser", "lock_counted"})


def _counted_lock_identifiers(
    tokens: Sequence[rust_comment_model.Token],
    pairs: dict[int, int],
    excluded: Sequence[tuple[int, int]] = (),
) -> set[str]:
    identifiers: set[str] = set()
    for index in range(len(tokens) - 1):
        if _inside_ranges(tokens[index].start, excluded):
            continue
        if (
            tokens[index].kind != "IDENT"
            or tokens[index].text not in COUNTED_LOCK_HELPERS
            or tokens[index + 1].text != "("
            or (index > 0 and tokens[index - 1].text == "fn")
        ):
            continue
        close = pairs.get(index + 1)
        if close is None:
            continue
        last = None
        for token in tokens[index + 2:close]:
            if token.text == ",":
                break
            if token.kind == "IDENT" and token.text != "mut":
                last = token.text
        if last:
            identifiers.add(last)
    return identifiers


def _atomic_sites(
    tokens: Sequence[rust_comment_model.Token],
    excluded: Sequence[tuple[int, int]] = (),
) -> list[tuple[str, set[str]]]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_lexer
    import rust_comment_model
    pairs = rust_comment_lexer._delimiter_pairs(tokens)
    grouped: dict[tuple[int, str], set[str]] = {}
    for index in range(len(tokens) - 2):
        if _inside_ranges(tokens[index].start, excluded):
            continue
        if (
            tokens[index].text != "Ordering"
            or tokens[index + 1].text != "::"
            or tokens[index + 2].text not in rust_comment_model.ATOMIC_ORDERINGS
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


def _release_tokens(tokens: Sequence[rust_comment_model.Token]) -> set[str]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_lexer
    import rust_comment_model
    release: set[str] = set()
    for index, token in enumerate(tokens):
        if token.text == "." and index + 2 < len(tokens) and tokens[index + 1].kind == "IDENT":
            receiver = _receiver_identifier(tokens, rust_comment_lexer._delimiter_pairs(tokens), index)
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
            close = rust_comment_lexer._delimiter_pairs(tokens).get(index + 1)
            if close is not None:
                release.update(
                    item.text for item in tokens[index + 2:close] if item.kind == "IDENT"
                )
    return release - rust_comment_model.IDENTIFIER_STOPLIST


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
