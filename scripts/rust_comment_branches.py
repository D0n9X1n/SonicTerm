"""Rust comment branches."""

from __future__ import annotations
from typing import Iterable, Iterator, Sequence

import rust_comment_model


def _selecting_tokens(tokens: Sequence[rust_comment_model.Token]) -> set[str]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_markers
    return rust_comment_markers._semantic_tokens(" ".join(token.text for token in tokens if token.kind in {"IDENT", "NUMBER", "STRING"}))


def _predicate_tokens(tokens: Sequence[rust_comment_model.Token]) -> set[str]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    return {
        token.text
        for token in tokens
        if token.kind == "IDENT" and token.text not in rust_comment_model.IDENTIFIER_STOPLIST
    }


def _validate_when_text(text: str, selecting: set[str], predicate: set[str]) -> str | None:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_markers
    semantic = rust_comment_markers._semantic_tokens(text)
    if not semantic:
        return "// When: explanation needs at least one semantic token"
    if semantic <= selecting:
        return "// When: explanation must add meaning beyond the selecting predicate or pattern"
    if predicate and not (rust_comment_markers._exact_identifier_tokens(text) & predicate):
        return "// When: marker must name this branch predicate"
    return None


def _first_body_comments(unit: rust_comment_model.SourceUnit, start: int, end: int) -> list[rust_comment_model.Comment]:
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


def _contains(tokens: Sequence[rust_comment_model.Token], values: set[str]) -> bool:
    return any(token.text in values for token in tokens)


def _ignored_result(tokens: Sequence[rust_comment_model.Token]) -> bool:
    for index in range(len(tokens) - 2):
        if tokens[index].text == "let" and tokens[index + 1].text == "_" and tokens[index + 2].text == "=":
            return True
    return False


def _platform_tokens(tokens: Sequence[rust_comment_model.Token]) -> bool:
    values = {token.text for token in tokens}
    return "cfg" in values or bool(values & {"target_os", "target_arch"})


def _shared_when_instances(
    unit: rust_comment_model.SourceUnit,
    construct: rust_comment_model.Token,
    local_offsets: set[int],
) -> list[rust_comment_model.MarkerInstance]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_markers
    comments = rust_comment_markers._contiguous_line_comments_before(unit, construct.line)
    if not comments or rust_comment_markers._marker_name(comments[0]) != "When":
        return []
    instances = [instance for instance in rust_comment_markers._marker_instances(comments) if instance.marker == "When"]
    return [
        instance
        for instance in instances
        if not any(comment.start in local_offsets for comment in instance.comments)
    ]


def _branch_marker(
    unit: rust_comment_model.SourceUnit,
    branch: rust_comment_model.Branch,
    shared_instances: Sequence[rust_comment_model.MarkerInstance],
    used: set[int],
    diagnostics: list[rust_comment_model.Diagnostic],
) -> None:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_contracts
    import rust_comment_markers
    import rust_comment_model
    local = rust_comment_markers._first_body_instances(unit, branch.body_start, branch.body_end, "When")
    rust_comment_contracts._use_instances(shared_instances, used)
    rust_comment_contracts._use_instances(local, used)
    if len(shared_instances) > 1 or len(local) > 1:
        anchor = (local or list(shared_instances))[1].anchor
        diagnostics.append(
            rust_comment_model.Diagnostic(
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
            rust_comment_model.Diagnostic(
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
            rust_comment_model.Diagnostic(
                unit.path,
                branch.anchor.line,
                branch.anchor.column,
                "when",
                "mandatory branch needs substantive // When: as first body content or at its branch head",
            )
        )
        return
    instance = instances[0]
    if not rust_comment_contracts._validate_instance_sizes(unit, instances, diagnostics):
        return
    if shared:
        if not branch.predicate:
            diagnostics.append(
                rust_comment_model.Diagnostic(
                    unit.path,
                    branch.anchor.line,
                    branch.anchor.column,
                    "when",
                    "shared // When: cannot cover a tokenless predicate",
                )
            )
            return
        if not (rust_comment_markers._exact_identifier_tokens(instance.body) & branch.predicate):
            diagnostics.append(
                rust_comment_model.Diagnostic(
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
            rust_comment_model.Diagnostic(
                unit.path,
                instance.anchor.line,
                instance.anchor.column,
                "when",
                error,
            )
        )
