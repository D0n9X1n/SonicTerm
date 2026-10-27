"""Rust comment sources."""

from __future__ import annotations
from pathlib import Path
import re
import subprocess
from typing import Iterable, Iterator, Sequence

import rust_comment_model


def _is_excluded(path: str) -> bool:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    if path in rust_comment_model.AUTHORED_VENDOR_FILES:
        return False
    return path in rust_comment_model.EXCLUDED_FILES or path.startswith(rust_comment_model.EXCLUDED_PREFIXES)


def _is_test_file(path: str) -> bool:
    parts = path.split("/")
    return Path(path).name.endswith("_tests.rs") or "tests" in parts or Path(path).name == "build.rs"


def _is_binary(path: str) -> bool:
    return path == "src/main.rs" or path.endswith("/src/main.rs") or "/examples/" in f"/{path}"


def _normalize_path(root: Path, value: str, caller_cwd: Path) -> str:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
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
        raise rust_comment_model.AnalysisError(f"path is outside repository: {value}") from error


def _tracked_rust(root: Path) -> list[str]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    completed = subprocess.run(
        ["git", "ls-files", "-z", "--full-name", "--", "*.rs"],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise rust_comment_model.AnalysisError(completed.stderr.decode("utf-8", errors="replace").strip())
    return sorted(raw.decode("utf-8").replace("\\", "/") for raw in completed.stdout.split(b"\0") if raw)


def _source_unit(root: Path, path: str) -> rust_comment_model.SourceUnit:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_lexer
    import rust_comment_model
    text = (root / path).read_text(encoding="utf-8", errors="replace")
    lexed = rust_comment_lexer.lex_rust(text)
    tokens = lexed.tokens
    pairs = rust_comment_lexer._delimiter_pairs(tokens)
    return rust_comment_model.SourceUnit(
        path=path,
        text=text,
        lexed=lexed,
        tokens=tokens,
        pairs=pairs,
        reverse_pairs={value: key for key, value in pairs.items()},
        attributes=rust_comment_lexer._attributes(tokens, pairs),
        file_test_context=_is_test_file(path),
        binary_context=_is_binary(path),
        excluded=_is_excluded(path),
    )


def _span_ending(spans: Sequence[rust_comment_model.Span], cursor: int) -> rust_comment_model.Span | None:
    for span in spans:
        if span.end == cursor:
            return span
    return None


def _prelude(unit: rust_comment_model.SourceUnit, offset: int) -> tuple[list[rust_comment_model.Comment], list[rust_comment_model.Span], int]:
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


def _attribute_texts(unit: rust_comment_model.SourceUnit, spans: Sequence[rust_comment_model.Span]) -> list[str]:
    return [unit.text[span.start:span.end] for span in spans]


def _doc_text(unit: rust_comment_model.SourceUnit, comments: Sequence[rust_comment_model.Comment], attributes: Sequence[rust_comment_model.Span]) -> str:
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
