"""Coverage reports."""

from __future__ import annotations
import json
import math
import os
import posixpath
from typing import Dict, List, Mapping, Optional, Tuple

import coverage_model


def _normalize(path: str) -> str:
    """Return `path` with forward slashes and `.`/`..` segments collapsed."""
    return posixpath.normpath(path.replace("\\", "/"))


def _absolute(path: str) -> str:
    """Return a normalized absolute form without rewriting an already absolute path."""
    normalized = _normalize(path)
    if normalized.startswith("/") or (len(normalized) > 1 and normalized[1] == ":"):
        return normalized
    return _normalize(os.path.abspath(path))


def _within(path: str, root: str) -> bool:
    """Whether `path` is `root` or lies beneath it."""
    return path == root or path.startswith(root.rstrip("/") + "/")


def _relative(path: str, root: str) -> Optional[str]:
    """Return `path` relative to `root`, or None when it lies outside."""
    if not _within(path, root) or path == root:
        return None
    return path[len(root.rstrip("/")) + 1:]


def _root_forms(root: str) -> List[str]:
    """Return the given and symlink-resolved forms of `root`, most specific first.

    llvm-cov can report resolved paths (macOS `/private/tmp` for `/tmp`) while
    `cargo metadata` reports the path as given, so both forms must match.
    """
    forms = {_absolute(root), _normalize(os.path.realpath(root))}
    return sorted(forms, key=len, reverse=True)


def _count(value: object) -> bool:
    """Whether `value` is a non-negative integer and not a boolean."""
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def _finite_number(value: object) -> bool:
    """Whether `value` is an int or float, not a boolean, and finite as a float."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return False
    try:
        return math.isfinite(float(value))
    except OverflowError:
        return False


def _reject_constant(name: str) -> float:
    """Refuse the NaN and Infinity literals that Python's json module accepts by default."""
    raise ValueError(f"non-finite number {name} is not allowed")


def _finite_float(text: str) -> float:
    """Parse a JSON float literal, refusing one that overflows to infinity."""
    value = float(text)
    if not math.isfinite(value):
        raise ValueError(f"non-finite number {text} is not allowed")
    return value


def _read_json(path: str, what: str, strict: bool = False) -> object:
    """Parse the JSON file at `path`, converting failures into InputError.

    `strict` refuses non-finite numbers, for inputs whose numbers are compared.
    """
    # Resolve this runtime-only dependency after module initialization.
    import coverage_model
    hooks = {"parse_constant": _reject_constant, "parse_float": _finite_float} if strict else {}
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle, **hooks)
    except OSError as error:
        raise coverage_model.InputError(f"cannot read {what} {path}: {error.strerror or error}") from error
    except ValueError as error:
        raise coverage_model.InputError(f"{what} {path} is not valid UTF-8 JSON: {error}") from error


def load_report(path: str) -> coverage_model.Report:
    """Load and validate a `cargo llvm-cov report --json --summary-only` export."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_model
    document = _read_json(path, "coverage report")
    if not isinstance(document, dict) or document.get("type") != coverage_model.LLVM_EXPORT_TYPE:
        raise coverage_model.InputError(f"coverage report {path} is not an {coverage_model.LLVM_EXPORT_TYPE} document")
    data = document.get("data")
    if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0], dict):
        raise coverage_model.InputError(f"coverage report {path} must hold exactly one export object in data")
    entries = data[0].get("files")
    # An empty export would mark every crate missing; it is a broken run, not a coverage result.
    if not isinstance(entries, list) or not entries:
        raise coverage_model.InputError(f"coverage report {path} lists no files; the instrumented run produced nothing to compare")
    files = []
    for index, entry in enumerate(entries):
        filename = entry.get("filename") if isinstance(entry, dict) else None
        summary = entry.get("summary") if isinstance(entry, dict) else None
        lines = summary.get("lines") if isinstance(summary, dict) else None
        if not isinstance(filename, str) or not filename or not isinstance(lines, dict):
            raise coverage_model.InputError(f"coverage report {path}: file entry {index} has no filename or line summary")
        count, covered = lines.get("count"), lines.get("covered")
        if not _count(count) or not _count(covered) or covered > count:
            raise coverage_model.InputError(f"coverage report {path}: {filename} has invalid line counts {count!r}/{covered!r}")
        files.append(coverage_model.ReportFile(filename, count, covered))
    tool_info = document.get("cargo_llvm_cov")
    version = tool_info.get("version") if isinstance(tool_info, dict) else None
    tool = f"cargo-llvm-cov {version}" if isinstance(version, str) and version else "llvm-cov export"
    return coverage_model.Report(tuple(files), tool)


def load_inventory(path: str) -> coverage_model.Inventory:
    """Load workspace members from `cargo metadata --no-deps --format-version 1` output."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_model
    document = _read_json(path, "cargo metadata")
    if not isinstance(document, dict):
        raise coverage_model.InputError(f"cargo metadata {path} is not a JSON object")
    root, member_ids, packages = (
        document.get("workspace_root"),
        document.get("workspace_members"),
        document.get("packages"),
    )
    if not isinstance(root, str) or not isinstance(member_ids, list) or not isinstance(packages, list):
        raise coverage_model.InputError(f"cargo metadata {path} lacks workspace_root, workspace_members, or packages")
    root = _absolute(root)
    by_id = {package.get("id"): package for package in packages if isinstance(package, dict)}
    members: Dict[str, coverage_model.Member] = {}
    for member_id in member_ids:
        package = by_id.get(member_id)
        name = package.get("name") if package else None
        manifest = package.get("manifest_path") if package else None
        if not isinstance(name, str) or not isinstance(manifest, str):
            raise coverage_model.InputError(f"cargo metadata {path}: workspace member {member_id!r} has no package entry")
        directory = _relative(posixpath.dirname(_absolute(manifest)), root)
        # A root package would own every path; this virtual workspace has none, so refuse the shape.
        if directory is None:
            raise coverage_model.InputError(f"cargo metadata {path}: member {name} is not in a subdirectory of {root}")
        if name in members:
            raise coverage_model.InputError(f"cargo metadata {path}: duplicate member name {name}")
        build_scripts = set()
        for target in package.get("targets") or []:
            source = target.get("src_path") if isinstance(target, dict) else None
            if isinstance(source, str) and "custom-build" in (target.get("kind") or []):
                relative = _relative(_absolute(source), root)
                if relative is not None:
                    build_scripts.add(relative)
        members[name] = coverage_model.Member(name, directory, frozenset(build_scripts))
    if not members:
        raise coverage_model.InputError(f"cargo metadata {path} lists no workspace members")
    target_directory = document.get("target_directory")
    return coverage_model.Inventory(root, _absolute(target_directory) if isinstance(target_directory, str) else None, members)


def place(filename: str, inventory: coverage_model.Inventory, source_roots: List[str], generated_roots: List[str]) -> coverage_model.Placement:
    """Attribute one report file to a member and decide whether it is excluded."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_model
    path = _normalize(filename)
    # Generated output is checked first because the target directory usually sits inside the checkout.
    if any(_within(path, root) for root in generated_roots):
        return coverage_model.Placement(path, None, "generated")
    relative = next((rel for rel in (_relative(path, root) for root in source_roots) if rel is not None), None)
    if relative is None:
        return coverage_model.Placement(path, None, "outside-repository")
    owner = inventory.owner(relative)
    member = owner.name if owner else None
    if any(_within(relative, root) for root in coverage_model.VENDORED_ROOTS):
        return coverage_model.Placement(relative, member, "vendored")
    if relative in coverage_model.GENERATED_SOURCES:
        return coverage_model.Placement(relative, member, "generated")
    if owner is None:
        return coverage_model.Placement(relative, None, "outside-members")
    inner = _relative(relative, owner.directory) or ""
    parts = inner.split("/")
    if relative in owner.build_scripts or inner == "build.rs":
        return coverage_model.Placement(relative, member, "build-script")
    if parts[-1].endswith("_tests.rs") or any(part in coverage_model.TEST_DIRECTORIES for part in parts[:-1]):
        return coverage_model.Placement(relative, member, "test")
    return coverage_model.Placement(relative, member, None)


def aggregate(
    report: coverage_model.Report, inventory: coverage_model.Inventory, source_root: Optional[str] = None
) -> Tuple[Dict[str, coverage_model.CrateCoverage], Dict[str, coverage_model.ExclusionTotals]]:
    """Sum eligible lines per member and tally excluded files by category."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_model
    base = source_root or inventory.root
    source_roots = _root_forms(base)
    generated_roots = set(_root_forms(posixpath.join(_absolute(base), "target")))
    generated_roots.update(_root_forms(posixpath.join(inventory.root, "target")))
    if inventory.target_directory:
        generated_roots.update(_root_forms(inventory.target_directory))
    crates = {name: coverage_model.CrateCoverage() for name in inventory.members}
    exclusions = {category: coverage_model.ExclusionTotals() for category in coverage_model.EXCLUSION_REASONS}
    seen: Dict[str, str] = {}
    for entry in report.files:
        placement = place(entry.filename, inventory, source_roots, sorted(generated_roots))
        # Two spellings of one file would count its lines twice; refuse the report instead.
        if placement.path in seen:
            raise coverage_model.InputError(f"coverage report lists {placement.path} twice "
                             f"({seen[placement.path]} and {entry.filename})")
        seen[placement.path] = entry.filename
        if placement.category is None:
            crate = crates[placement.member]
            crate.lines += entry.lines
            crate.covered += entry.covered
            crate.eligible_files += 1
            continue
        totals = exclusions[placement.category]
        totals.files += 1
        totals.lines += entry.lines
        totals.paths.append(placement.path)
        if placement.member is not None:
            crates[placement.member].excluded[placement.category] += 1
    return crates, exclusions


def unmeasured_detail(crate: coverage_model.CrateCoverage) -> str:
    """Explain why a member has no eligible instrumented line."""
    if crate.eligible_files:
        return f"{crate.eligible_files} reported file(s) contain no instrumented line"
    if crate.excluded:
        categories = ", ".join(f"{name}: {count}" for name, count in sorted(crate.excluded.items()))
        return f"every reported file is excluded ({categories})"
    return "no reported file"
