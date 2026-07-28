#!/usr/bin/env python3
"""Emit the v1.2.0 resource-invariant baseline inventory."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
from typing import Final

SCHEMA: Final[tuple[str, ...]] = (
    "ID",
    "crate:path/symbol",
    "owner now",
    "proposed owner",
    "class",
    "retained/transient",
    "formula",
    "current bound",
    "pressure policy",
    "worker/native handles",
    "test ID",
    "package",
)

FORMAL_PACKAGES: Final[frozenset[str]] = frozenset(
    {
        "WP-CONTRACT",
        "WP-LIFECYCLE",
        "WP-OBSERVE",
        "WP-GRID",
        "WP-VT",
        "WP-RENDER",
        "WP-FONT",
        "WP-MEDIA",
        "WP-PTY",
        "WP-APP-IO",
        "WP-MUX",
        "WP-WINDOW",
        "WP-INTEGRATE",
        "WP-ADVERSARY",
        "WP-PLATFORM",
        "WP-DOCS-RELEASE",
    }
)

ROWS: Final[tuple[tuple[str, ...], ...]] = (
    (
        "RI-GRID-AGGREGATE",
        "sonicterm-grid:src/grid.rs/Grid",
        "each Grid",
        "WP-GRID governor",
        "terminal cells",
        "retained",
        "visible + scrollback + saved primary cells",
        "MAX_GRID_CELLS per Grid",
        "compact and trim under aggregate pressure",
        "none",
        "v120_grid_aggregate_retention_has_one_governor",
        "WP-GRID",
    ),
    (
        "RI-VT-MEDIA-CAPTURE",
        "sonicterm-vt:src/vt.rs/Parser",
        "each Parser",
        "WP-VT governor",
        "escape and media capture bytes",
        "transient",
        "generic escape + active media payload bytes",
        "independent parser limits only",
        "abort capture before aggregate budget is exceeded",
        "none",
        "v120_parser_media_capture_shares_one_budget",
        "WP-VT",
    ),
    (
        "RI-MUX-BLOCKED-WORKER",
        "sonicterm-mux:src/server.rs/handle_connection_with_shutdown",
        "connection reader and writer threads",
        "WP-MUX connection owner",
        "blocked workers",
        "transient",
        "reader + writer + shutdown participants per connection",
        "bounded channels; no compositional owner",
        "cancel, unblock, join, then release streams",
        "reader/writer threads and stream handles",
        "v120_blocked_worker_owner_orders_cancel_join_and_drop",
        "WP-MUX",
    ),
    (
        "RI-ATLAS-IDENTITY-888",
        "sonicterm-text:src/glyph_atlas.rs/GlyphAtlas",
        "atlas and downstream caches",
        "WP-FONT identity authority",
        "atlas identity and cached UVs",
        "retained",
        "generation + eviction epoch + dependent cache entries",
        "local epochs; no owner-wide stale-entry count",
        "invalidate every dependent cache on identity replacement",
        "atlas pixel allocation",
        "v120_stale_atlas_identity_invalidates_all_dependents_888",
        "WP-FONT",
    ),
    (
        "RI-NATIVE-SURFACE",
        "sonicterm-gpu:src/software_windows.rs/WindowsSoftwareFrame",
        "software frame and native decoder",
        "WP-RENDER surface governor",
        "decoded pixels and surfaces",
        "transient",
        "decoded bytes + destination surface bytes",
        "separate surface-size checks",
        "reject before decode/allocation crosses combined bound",
        "native decoder and surface handles",
        "v120_native_decode_and_surface_share_bounds",
        "WP-RENDER",
    ),
    (
        "RI-QUEUE-ACCOUNTING",
        "sonicterm-mux:src/server.rs/SubscriberSink",
        "each bounded mailbox",
        "WP-MUX accounting owner",
        "queued messages and payload bytes",
        "transient",
        "queued control frames + output payload bytes",
        "message count and frame ceiling",
        "reserve control capacity and reject over byte budget",
        "channel sender and receiver",
        "v120_queue_accounting_covers_messages_and_payload_bytes",
        "WP-MUX",
    ),
    (
        "RI-REGISTRY-CLEANUP",
        "sonicterm-app:src/app/scrollbar_visibility.rs/update_and_collect",
        "per-window scrollbar_vis map",
        "WP-WINDOW lifecycle owner",
        "window tab and pane registries",
        "retained",
        "live registry entries after close and child exit",
        "pruned to the visible pane set on every render/hover pass",
        "remove every keyed entry before owner teardown completes",
        "window and pane lifecycle handles",
        "v120_registry_cleanup_removes_all_owned_entries",
        "WP-WINDOW",
    ),
)

TEST_ATTRIBUTE = re.compile(
    r'#\[ignore\s*=\s*"v120-invariant-baseline:([^:"]+):([^:"]+)"\]'
)
# A sentinel a Wave 2 package has satisfied: the gate is gone and the test runs.
#
# "Runs" is the whole claim, so matching the name alone is not enough. A bare
# name match accepts `// fn v120_x() {}`, a plain helper that Cargo never
# registers, and `#[ignore] fn v120_x()`, which Cargo registers and then skips.
# Each of those reports an acceptance criterion as covered while nothing
# executes, which is the one failure this gate exists to prevent.
#
# So the match starts at an unambiguous `#[test]` line and walks the
# attributes between it and the function, rejecting the pair if `#[ignore]`
# appears among them. Comment lines are stripped before any of this, because
# a commented-out block otherwise satisfies every pattern in it.
IMPLEMENTED_TEST = re.compile(
    r"^[ \t]*#\[test\][ \t]*\n"  # a real #[test], not inside a comment
    r"(?P<attrs>(?:[ \t]*#\[[^\n]*\][ \t]*\n)*)"  # attributes before the fn
    r"[ \t]*(?:pub[ \t]+)?(?:async[ \t]+)?fn[ \t]+(?P<name>v120_[a-z0-9_]+)[ \t]*\(",
    re.MULTILINE,
)
IGNORE_ATTRIBUTE = re.compile(r"#\[\s*ignore\b")
LINE_COMMENT = re.compile(r"^[ \t]*//.*$", re.MULTILINE)
PACKAGE_TOKEN = re.compile(r"\bWP-[A-Z0-9-]+\b")


def implemented_sentinels(text: str) -> set[str]:
    """Names of sentinels in `text` that Cargo will actually run.

    Requires an immediately preceding `#[test]` and rejects `#[ignore]`
    anywhere between that attribute and the function signature.
    """
    # Blank the line comments rather than deleting them, so the remaining
    # text keeps its line structure and a commented-out `#[test]` cannot
    # pair with the next real function below it.
    uncommented = LINE_COMMENT.sub("", text)
    names: set[str] = set()
    for match in IMPLEMENTED_TEST.finditer(uncommented):
        if IGNORE_ATTRIBUTE.search(match.group("attrs")):
            continue
        names.add(match.group("name"))
    return names


def repository_root() -> Path:
    return Path(__file__).resolve().parent.parent


def validate_rows(root: Path) -> None:
    if any(len(row) != len(SCHEMA) for row in ROWS):
        raise ValueError("every inventory row must use the 12-column schema")

    for index, label in ((0, "IDs"), (4, "classes"), (10, "test IDs"), (11, "packages")):
        values = [row[index].strip() for row in ROWS]
        if any(not value for value in values):
            raise ValueError(f"inventory {label} must be nonempty")
        if index != 11 and len(values) != len(set(values)):
            raise ValueError(f"inventory {label} must be unique")

    for row in ROWS:
        package = row[11]
        if package not in FORMAL_PACKAGES:
            raise ValueError(f"inventory package is not a formal package: {package}")
        proposed_packages = PACKAGE_TOKEN.findall(row[3])
        if proposed_packages != [package]:
            invalid = next(
                (candidate for candidate in proposed_packages if candidate not in FORMAL_PACKAGES),
                None,
            )
            if invalid is not None:
                raise ValueError(f"inventory package is not a formal package: {invalid}")
            raise ValueError(
                f"proposed owner package mismatch for {row[0]}: {proposed_packages} != {package}"
            )

    source_test_ids: dict[str, str] = {}
    implemented_test_ids: set[str] = set()
    crates = root / "crates"
    # Recursive: crates that group modules into subdirectories keep their
    # sibling test files there too (`sonicterm-app/src/app/*_tests.rs`). A
    # one-level glob cannot see those, so a sentinel implemented in one would
    # read as missing.
    for path in sorted(crates.glob("*/src/**/*_tests.rs")):
        text = path.read_text(encoding="utf-8")
        for test_id, package in TEST_ATTRIBUTE.findall(text):
            if test_id in source_test_ids:
                raise ValueError(f"duplicate gated test ID in source: {test_id}")
            source_test_ids[test_id] = package
        implemented_test_ids.update(implemented_sentinels(text))

    for row in ROWS:
        test_id, package = row[10], row[11]
        if test_id not in source_test_ids:
            # A Wave 2 package satisfies its baseline invariant by implementing
            # the sentinel and removing the gate. That is the completion
            # condition, so an implemented sentinel is success rather than a
            # missing row — but it still has to exist, because deleting one
            # would quietly drop an accepted acceptance criterion.
            if test_id in implemented_test_ids:
                continue
            raise ValueError(
                f"gated test ID does not exist in source, "
                f"neither gated nor implemented: {test_id}"
            )
        if source_test_ids[test_id] != package:
            raise ValueError(
                f"gated test package mismatch for {test_id}: "
                f"{source_test_ids[test_id]} != {package}"
            )


def json_bytes() -> bytes:
    rows = [dict(zip(SCHEMA, row, strict=True)) for row in ROWS]
    document = {"schema": list(SCHEMA), "row_count": len(rows), "rows": rows}
    return (json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


def markdown_bytes() -> bytes:
    def cell(value: str) -> str:
        return value.replace("|", "\\|").replace("\n", " ")

    lines = [
        "# v1.2.0 Resource Invariant Baseline",
        "",
        "<!-- Authoritative inventory is scripts/resource-inventory.py; generated output is untracked. -->",
        "",
        "| " + " | ".join(SCHEMA) + " |",
        "| " + " | ".join("---" for _ in SCHEMA) + " |",
    ]
    lines.extend("| " + " | ".join(cell(value) for value in row) + " |" for row in ROWS)
    return ("\n".join(lines) + "\n").encode()


def expected_files() -> dict[str, bytes]:
    payloads = {
        "resource-inventory.json": json_bytes(),
        "resource-inventory.md": markdown_bytes(),
    }
    checksum_lines = [
        f"{hashlib.sha256(payloads[name]).hexdigest()}  {name}" for name in sorted(payloads)
    ]
    payloads["resource-inventory.sha256"] = ("\n".join(checksum_lines) + "\n").encode()
    return payloads


def write_outputs(output: Path, payloads: dict[str, bytes]) -> None:
    output.mkdir(parents=True, exist_ok=True)
    for name, content in payloads.items():
        (output / name).write_bytes(content)


def check_outputs(output: Path, payloads: dict[str, bytes]) -> bool:
    expected_names = set(payloads)
    actual_names = {path.name for path in output.iterdir()} if output.is_dir() else set()
    if actual_names != expected_names:
        return False
    return all((output / name).read_bytes() == content for name, content in payloads.items())


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="verify generated output is current")
    args = parser.parse_args()

    root = repository_root()
    try:
        validate_rows(root)
    except ValueError as error:
        print(f"resource inventory validation failed: {error}", file=sys.stderr)
        return 1

    output = root / "target" / "v1.2.0-baseline"
    payloads = expected_files()
    if args.check:
        if not check_outputs(output, payloads):
            print(f"resource inventory output is missing or stale: {output}", file=sys.stderr)
            return 1
    else:
        write_outputs(output, payloads)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
