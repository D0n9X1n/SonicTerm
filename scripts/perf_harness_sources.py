"""Perf harness sources."""

from __future__ import annotations
import hashlib
import os
from pathlib import Path
import re
import shutil
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_settings


# --- Harness hash and overlay ---------------------------------------------------------------

HARNESS_EXAMPLES = (perf_settings.HARNESS_EXAMPLE, perf_settings.ALLOC_EXAMPLE)
_EXAMPLE_HEADER = re.compile(r"\[\[\s*example\s*\]\]\s*(?:#.*)?")
_EXAMPLE_NAME = re.compile(r'\s*name\s*=\s*"([^"]+)"\s*(?:#.*)?')


def _table_spans(lines: Sequence[str]) -> list[tuple[int, int]]:
    """Return each table's line span: its header through its last key line, so comments before the next header stay put."""
    headers = [index for index, line in enumerate(lines) if line.startswith("[")]
    spans = []
    for position, start in enumerate(headers):
        limit = headers[position + 1] if position + 1 < len(headers) else len(lines)
        end = start + 1
        for index in range(start + 1, limit):
            stripped = lines[index].strip()
            if stripped and not stripped.startswith("#"):
                end = index + 1
        spans.append((start, end))
    return spans


def _example_name(lines: Sequence[str], start: int, end: int) -> str | None:
    """Return the name of the `[[example]]` table at a span, or None for any other table."""
    if not _EXAMPLE_HEADER.fullmatch(lines[start].strip()):
        return None
    for line in lines[start + 1:end]:
        match = _EXAMPLE_NAME.fullmatch(line.rstrip("\r\n"))
        if match:
            return match[1]
    return None


def harness_entries(manifest: str) -> str:
    """Return the text of the two harness `[[example]]` tables, in fixed order and with `\\n` line ends."""
    lines = manifest.splitlines(keepends=True)
    found = {}
    for start, end in _table_spans(lines):
        name = _example_name(lines, start, end)
        if name in HARNESS_EXAMPLES:
            if name in found:
                raise ValueError(f"[[example]] {name} is declared twice")
            found[name] = "".join(lines[start:end]).replace("\r\n", "\n").rstrip("\n") + "\n"
    missing = [name for name in HARNESS_EXAMPLES if name not in found]
    if missing:
        raise ValueError(f"{perf_settings.APP_MANIFEST} does not declare [[example]] {', '.join(missing)}")
    return "\n".join(found[name] for name in HARNESS_EXAMPLES)


def overlay_manifest(manifest: str, entries: str) -> str:
    """Give a manifest exactly the head's harness entries: unchanged when they match, else replaced or inserted.

    Every existing harness table is removed and the head's are appended; an array-of-tables
    element may follow any table, and every other line is kept as it was.
    """
    try:
        if harness_entries(manifest) == entries:
            return manifest
    except ValueError:
        pass  # Missing or duplicated entries are replaced below.
    lines = manifest.splitlines(keepends=True)
    removed = set()
    for start, end in _table_spans(lines):
        if _example_name(lines, start, end) in HARNESS_EXAMPLES:
            removed.update(range(start, end))
    kept = "".join(line for index, line in enumerate(lines) if index not in removed)
    return kept.rstrip() + "\n\n" + entries


def harness_hash(directory: Path, entries: str) -> str:
    """sha256 over the harness directory's sorted relative paths and bytes, then the entries' text.

    Every path, file and the entries are length-prefixed, so no two layouts share a byte
    stream. A symlink or an empty directory is refused: neither can be overlaid faithfully.
    """
    files = []
    for current, subdirectories, names in os.walk(directory):
        for name in subdirectories + names:
            if (Path(current) / name).is_symlink():
                raise ValueError(f"harness path {Path(current) / name} is a symlink")
        files.extend(Path(current) / name for name in names)
    if not files:
        raise ValueError(f"harness directory {directory} holds no files")
    digest = hashlib.sha256()
    for path in sorted(files, key=lambda path: path.relative_to(directory).as_posix()):
        relative = path.relative_to(directory).as_posix().encode("utf-8")
        data = path.read_bytes()
        digest.update(len(relative).to_bytes(8, "big") + relative)
        digest.update(len(data).to_bytes(8, "big") + data)
    text = entries.replace("\r\n", "\n").encode("utf-8")
    digest.update(len(text).to_bytes(8, "big") + text)
    return digest.hexdigest()


def _read_manifest(root: Path) -> str:
    """Read the app manifest without translating its line ends."""
    return (root / perf_settings.APP_MANIFEST).read_bytes().decode("utf-8")


COUNTERS_FEATURE = "perf-counters"
# Marks a tree whose renderer reports its frame texture's extent; the harness then writes frame_texture_bytes.
FRAME_TEXTURE_FEATURE = "perf-frame-texture"
# Marks a tree whose App can take a memory sample tagged with a perf checkpoint; the harness then samples at each.
CHECKPOINT_MEMORY_FEATURE = "perf-hook-checkpoint-memory"
# Marks a tree whose App has the S2 echo watch; the harness then splits S2/default latency at the flush.
ECHO_TRACE_FEATURE = "perf-echo-trace"
# Marks a tree whose App has the covered-window trim hook; the harness then asks it to trim at the cover.
TRIM_HOOK_FEATURE = "perf-hook-trim"
# Every perf feature a tree may declare, in the order a build passes them; later hooks append.
PERF_FEATURES = (COUNTERS_FEATURE, FRAME_TEXTURE_FEATURE, CHECKPOINT_MEMORY_FEATURE, ECHO_TRACE_FEATURE,
                 TRIM_HOOK_FEATURE)
# The app source a hook's method lives in, and the definition each hook feature needs there.
APP_SOURCE_DIRECTORY = "crates/sonicterm-app/src"
HOOK_METHODS = {CHECKPOINT_MEMORY_FEATURE: re.compile(r"^\s*pub fn __perf_checkpoint_memory\b", re.M),
                TRIM_HOOK_FEATURE: re.compile(r"^\s*pub fn __trim_covered_now\b", re.M)}
_TABLE_HEADER = re.compile(r"\s*\[\s*([^\[\]]+?)\s*\]\s*(?:#.*)?")


def declares_feature(manifest: str, feature: str) -> bool:
    """Whether the manifest's `[features]` table declares `feature`; a comment or another table does not."""
    key = re.compile(rf'\s*(?:{re.escape(feature)}|"{re.escape(feature)}")\s*=')
    table = None
    for line in manifest.splitlines():
        header = _TABLE_HEADER.fullmatch(line)
        if header:
            table = header[1]
        elif line.lstrip().startswith("["):
            # An array-of-tables header such as [[example]] ends the previous table.
            table = None
        elif table == "features" and key.match(line):
            return True
    return False


def declares_perf_counters(manifest: str) -> bool:
    """Whether the manifest's `[features]` table declares perf-counters; a comment or another table does not."""
    return declares_feature(manifest, COUNTERS_FEATURE)


LOGGING_LIB = "crates/sonicterm-logging/src/lib.rs"
# The logging API the counters harness calls; a definition at the start of a line, so a comment does not count.
_FILTERED_LOGGING_INIT = re.compile(r"^pub fn init_in_with_filter\b", re.M)


def tree_supports_counters(root: Path) -> bool:
    """Whether a worktree can build the overlaid harness with the perf-counters feature.

    The tree's app manifest must declare the feature, and its logging crate must define
    `pub fn init_in_with_filter`: the head's harness, which every tree builds, calls it in its
    perf-counters code. A tree that declares the feature without that function (an early counters
    commit) cannot compile the harness with it, so it is treated as having no counters, builds
    without the feature, and leaves the counters set to the head. Both checks read source text.
    """
    if not declares_perf_counters(_read_manifest(root)):
        return False
    try:
        logging_source = (root / LOGGING_LIB).read_bytes().decode("utf-8")
    except (OSError, UnicodeDecodeError):
        return False  # No readable logging crate: the harness's logging call cannot resolve either.
    return _FILTERED_LOGGING_INIT.search(logging_source) is not None


# Where something other than code starts in Rust source: a line or block comment, a raw or byte or
# plain string literal, or a quote that opens a character literal or a lifetime.
_RUST_NON_CODE = re.compile(r"""//|/\*|(?<![\w])b?r#*"|(?<![\w])b"|"|'""")
# A nested block comment's openings and closings.
_BLOCK_COMMENT_EDGE = re.compile(r"/\*|\*/")
# Inside a string: an escape (which may escape a quote) or the closing quote.
_STRING_EDGE = re.compile(r'\\.|"', re.S)
# A character literal starting at a quote; a quote that does not start one opens a lifetime.
_CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F]{1,6}\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")


def rust_code_only(source: str) -> str:
    """`source` with every comment (line, doc and nested block) and every string, byte-string,
    raw-string and character literal replaced by spaces. Line breaks are kept, so a line-anchored
    pattern still sees the code's lines, and only code can match it."""
    pieces = []
    position = 0
    length = len(source)
    while True:
        found = _RUST_NON_CODE.search(source, position)
        if found is None:
            pieces.append(source[position:])
            break
        start = found.start()
        pieces.append(source[position:start])
        token = found.group()
        if token == "//":
            end = source.find("\n", start)
            end = length if end < 0 else end
        elif token == "/*":
            depth, end = 0, length
            cursor = start
            while (edge := _BLOCK_COMMENT_EDGE.search(source, cursor)) is not None:
                depth += 1 if edge.group() == "/*" else -1
                cursor = edge.end()
                if depth == 0:
                    end = cursor
                    break
        elif token.endswith('"') and "r" in token:
            closer = '"' + "#" * token.count("#")
            close_at = source.find(closer, found.end())
            end = length if close_at < 0 else close_at + len(closer)
        elif token.endswith('"'):
            end = length
            cursor = found.end()
            while (edge := _STRING_EDGE.search(source, cursor)) is not None:
                cursor = edge.end()
                if edge.group() == '"':
                    end = cursor
                    break
        else:
            literal = _CHAR_LITERAL.match(source, start)
            if literal is None:
                pieces.append("'")  # A lifetime: the quote is code.
                position = start + 1
                continue
            end = literal.end()
        pieces.append(re.sub(r"[^\n]", " ", source[start:end]))
        position = end
    return "".join(pieces)


def tree_supports_hook(root: Path, feature: str, method_regex: re.Pattern) -> bool:
    """Whether a worktree can build the harness with the hook `feature`.

    The app manifest must declare the feature, and some app source file must define the method the
    harness calls under it (`method_regex`, anchored at a line start). Comments and string literals
    are blanked first (`rust_code_only`), so a mention in a comment, a doc line or a string does not
    count. A tree that declares the feature without the method cannot compile the harness with it.
    """
    if not declares_feature(_read_manifest(root), feature):
        return False
    for source in sorted((root / APP_SOURCE_DIRECTORY).rglob("*.rs")):
        try:
            text = source.read_bytes().decode("utf-8")
        except (OSError, UnicodeDecodeError):
            continue  # An unreadable file cannot define the method.
        if method_regex.search(rust_code_only(text)):
            return True
    return False


def tree_features(root: Path) -> tuple[str, ...]:
    """The perf features a worktree builds the harness with, in `PERF_FEATURES` order.

    perf-counters needs the logging API too (`tree_supports_counters`); perf-frame-texture needs
    only its declaration, since its accessor ships in the same change as the feature; each hook
    needs its method (`tree_supports_hook`). perf-echo-trace implies perf-counters, so a tree adds it
    only when it declares it and supports counters.
    """
    manifest = _read_manifest(root)
    features = []
    if tree_supports_counters(root):
        features.append(COUNTERS_FEATURE)
    if declares_feature(manifest, FRAME_TEXTURE_FEATURE):
        features.append(FRAME_TEXTURE_FEATURE)
    for feature, method in HOOK_METHODS.items():
        if tree_supports_hook(root, feature, method):
            features.append(feature)
    if COUNTERS_FEATURE in features and declares_feature(manifest, ECHO_TRACE_FEATURE):
        features.append(ECHO_TRACE_FEATURE)
    return tuple(feature for feature in PERF_FEATURES if feature in features)


def tree_harness_hash(root: Path) -> str:
    """Return the harness hash of one tree: its example directory and its two manifest entries."""
    return harness_hash(root / perf_settings.HARNESS_DIRECTORY, harness_entries(_read_manifest(root)))


def tree_harness_cfgs(root: Path, table: Sequence) -> tuple[str, ...]:
    """The harness API cfgs whose every method `root` defines for its owner type in its owning crate, in table order.

    A method counts only as a `pub fn <name>` inside an inherent `impl <Owner>` block (a path-qualified owner
    counts; a trait impl does not), in code with comments and strings blanked (`rust_code_only`). Test-only code
    does not count: a `*_tests.rs` file, a file under `#![cfg(test)]`, and an impl, module or method under
    `#[cfg(test)]`.
    Finding such a definition does not prove its signature or that the build's target compiles it."""
    enabled = []
    for entry in table:
        if all(_crate_defines(root / crate, owner, name) for crate, owner, name in entry.methods):
            enabled.append(entry.name)
    return tuple(enabled)


_IMPL_HEADER = re.compile(r"^[ \t]*impl\b([^{;]*)\{", re.M)
_MOD_HEADER = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{", re.M)
_CFG_TEST = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
_FILE_CFG_TEST = re.compile(r"#\s*!\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
# The attributes directly above an item: `#[...]` groups (one level of nested brackets), separated by whitespace.
_TRAILING_ATTRIBUTES = re.compile(r"(?:#\s*\[(?:[^\[\]]|\[[^\[\]]*\])*\]\s*)*$")


def _block_end(code: str, open_brace: int) -> int:
    """The index just past the brace that closes the one at `open_brace`, in comment- and string-free code."""
    depth = 0
    for index in range(open_brace, len(code)):
        if code[index] == "{":
            depth += 1
        elif code[index] == "}":
            depth -= 1
            if depth == 0:
                return index + 1
    return len(code)


def _under_cfg_test(code: str, item_start: int) -> bool:
    """Whether the attributes directly above the item at `item_start` include `#[cfg(test)]`."""
    attributes = _TRAILING_ATTRIBUTES.search(code[:item_start].rstrip())
    return attributes is not None and _CFG_TEST.search(attributes.group()) is not None


def _impl_owner(header: str) -> str | None:
    """The owner type an `impl` header names, its last path segment without generics; None for a trait impl."""
    header = re.split(r"\bwhere\b", header, maxsplit=1)[0]
    if re.search(r"\bfor\b", header):
        return None
    header = re.sub(r"^\s*<[^>]*>", "", header).strip()
    return header.split("<", 1)[0].strip().split("::")[-1].strip() or None


def _crate_defines(directory: Path, owner: str, name: str) -> bool:
    """Whether some non-test source file under `directory` defines `pub fn name` in an inherent `impl owner` block
    outside test-only modules and impls."""
    method = re.compile(rf"^\s*(pub\s+fn\s+{re.escape(name)}\b)", re.M)
    for source in sorted(directory.rglob("*.rs")):
        if source.name.endswith("_tests.rs"):
            continue
        try:
            code = rust_code_only(source.read_bytes().decode("utf-8"))
        except (OSError, UnicodeDecodeError):
            continue  # An unreadable file cannot define the method.
        if _FILE_CFG_TEST.search(code):
            continue
        test_modules = [range(found.start(), _block_end(code, found.end() - 1))
                        for found in _MOD_HEADER.finditer(code) if _under_cfg_test(code, found.start())]
        for header in _IMPL_HEADER.finditer(code):
            if _impl_owner(header.group(1)) != owner or _under_cfg_test(code, header.start()):
                continue
            if any(header.start() in span for span in test_modules):
                continue
            body = code[header.end() - 1:_block_end(code, header.end() - 1)]
            # The method's own attributes count too: a `#[cfg(test)]` method exists only in the unit-test build.
            if any(not _under_cfg_test(body, found.start(1)) for found in method.finditer(body)):
                return True
    return False


def harness_cfg_decision(trees: Mapping[str, Path], table: Sequence) -> tuple[tuple[str, ...], dict[str, tuple]]:
    """The comparison's one harness cfg decision and each tree's own: a cfg is on for both sides only when both
    trees define all of its methods, else off for both, so a base never inherits the head's API."""
    per_tree = {side: tree_harness_cfgs(trees[side], table) for side in trees}
    decided = tuple(entry.name for entry in table if all(entry.name in found for found in per_tree.values()))
    return decided, per_tree


def overlay_harness(head_root: Path, base_root: Path) -> None:
    """Copy the head's harness directory and `[[example]]` entries onto another tree; nothing under src/.

    The target's harness is deleted before the copy, so a target that resolves to the source
    or to the main checkout, which holds uncommitted work, is refused before anything is touched.
    """
    target_root = base_root.resolve()
    if target_root == head_root.resolve():
        raise ValueError(f"overlay target {base_root} is its own source {head_root}")
    if target_root == perf_settings.ROOT.resolve():
        raise ValueError(f"overlay target {base_root} is the main checkout {perf_settings.ROOT}")
    entries = harness_entries(_read_manifest(head_root))
    target = base_root / perf_settings.HARNESS_DIRECTORY
    if target.is_symlink():
        target.unlink()
    elif target.exists():
        shutil.rmtree(target)
    shutil.copytree(head_root / perf_settings.HARNESS_DIRECTORY, target, symlinks=True)
    manifest = _read_manifest(base_root)
    overlaid = overlay_manifest(manifest, entries)
    if overlaid != manifest:
        (base_root / perf_settings.APP_MANIFEST).write_bytes(overlaid.encode("utf-8"))
