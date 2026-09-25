#!/usr/bin/env python3
"""Per-crate line-coverage report and regression floor for the coverage gate.

Reads the `cargo llvm-cov report --json --summary-only` export and the
`cargo metadata --no-deps` inventory that scripts/rust-logic-coverage.sh writes
from one instrumented run, then:

* aggregates line coverage per workspace member from repository-relative
  `crates/<member>/...` paths;
* excludes test, vendored, generated (Cargo build output and committed
  rust-bindgen bindings), build-script, and out-of-tree files and prints every
  exclusion category with its file and line counts;
* reports a member with no eligible instrumented line as `not measured` with
  its reason, never as 0% or 100%;
* compares the measured members with scripts/coverage-baseline.json.

The baseline records the host that produced it (target triple and runner
image), a tolerance in percentage points, per-crate floors, and the crates
declared not measured. The floor is enforced only in CI (`GITHUB_ACTIONS=true`)
on that host. There it fails when a measured crate drops more than the
tolerance below its floor or has no floor, when a crate with a floor stops being
measured or leaves the workspace, when a member is neither measured nor
declared, when a declared crate starts reporting lines, and when a declaration
names a crate that is not a member. CI on any other host fails as not
comparable: a baseline from another host is never a passing result. Outside CI
every delta is informational and the floor gives no verdict.

The baseline changes only through `--update-baseline --reason TEXT`, whose
result is committed as a reviewed diff. A failing CI run prints a proposed
baseline between marker lines for a reviewer to adopt; the check never writes
the baseline.

Exit status: 0 when the floor holds or the run is informational, 1 when the
floor fails in CI, 2 for usage or input errors.
"""

from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass, field
from fractions import Fraction
import json
import math
import os
from pathlib import Path
import posixpath
import sys
import tempfile
from typing import Dict, List, Mapping, Optional, Tuple

SCHEMA = "sonicterm-coverage-baseline/1"
DEFAULT_TOLERANCE_PP = 1.0
LLVM_EXPORT_TYPE = "llvm.coverage.json.export"
PROPOSAL_BEGIN = "----- BEGIN PROPOSED scripts/coverage-baseline.json -----"
PROPOSAL_END = "----- END PROPOSED scripts/coverage-baseline.json -----"

# Upstream source trees kept as reviewed imports. The tuple mirrors the `path`
# of every library in scripts/native-dependencies.json; a contract test keeps
# the two identical.
VENDORED_ROOTS = (
    "crates/sonicterm-freetype/freetype2",
    "crates/sonicterm-freetype/libpng",
    "crates/sonicterm-freetype/zlib",
    "crates/sonicterm-harfbuzz/harfbuzz",
    "crates/sonicterm-winit",
)

# Cargo's conventional integration-test, benchmark, and example directories.
TEST_DIRECTORIES = frozenset({"tests", "benches", "examples"})

# Committed generator output inside first-party crates, with its generator.
# These files count under `generated`, so regenerating bindings never moves a
# crate's figure. A contract test keeps this set equal to the first-party
# sources that carry a generator marker.
GENERATED_SOURCES = {
    "crates/sonicterm-freetype/src/lib.rs": "rust-bindgen 0.71.1 via scripts/regenerate-freetype.sh",
    "crates/sonicterm-freetype/src/types.rs": "rust-bindgen 0.71.1 via scripts/regenerate-freetype.sh",
    "crates/sonicterm-harfbuzz/src/lib.rs": "rust-bindgen 0.71.1 via scripts/regenerate-harfbuzz.sh",
}

# Members that ship on one platform. On another host their rows measure only
# what compiles there, which is not execution coverage on the target platform.
PLATFORM_CRATES = {
    "sonicterm-mac": "macos",
    "sonicterm-windows": "windows",
    "sonicterm-linux": "linux",
}
OS_LABELS = {"macos": "macOS", "windows": "Windows", "linux": "Linux"}

# Exclusion categories in report order, each printed with its reason.
EXCLUSION_REASONS = {
    "test": "test code: *_tests.rs files and tests/, benches/, examples/ trees",
    "vendored": "vendored upstream source listed in scripts/native-dependencies.json",
    "generated": "generated code: Cargo target output and committed rust-bindgen bindings",
    "outside-repository": "outside the repository source root",
    "build-script": "Cargo build script; runs at compile time, not under test",
    "outside-members": "inside the repository but in no workspace member",
}
# Categories unusual enough that their paths are listed, not only counted.
LISTED_CATEGORIES = frozenset({"generated", "outside-repository", "build-script", "outside-members"})
LISTED_PATH_LIMIT = 10

DROP = "drop"
NO_BASELINE = "no-baseline"
NOW_MEASURED = "declared-now-measured"
MISSING_MEASURED = "missing-measured"
MISSING_MEMBER = "missing-member"
UNEXPLAINED = "unexplained"
STALE_DECLARATION = "stale-declaration"

# Findings a proposal resolves by adding entries. Drops and disappearances need
# a reviewer's decision, so no proposal lowers or deletes an existing floor.
PROPOSABLE = frozenset({NO_BASELINE, NOW_MEASURED, UNEXPLAINED})

# A stored percent is rounded to two decimals; allow that rounding plus float noise.
PERCENT_SLOP = 0.0051


class InputError(Exception):
    """A missing or malformed input, reported with exit status 2."""


@dataclass(frozen=True)
class ReportFile:
    """Line counts for one file of the llvm-cov export."""

    filename: str
    lines: int
    covered: int


@dataclass(frozen=True)
class Report:
    """The llvm-cov export: per-file line counts and the tool that produced them."""

    files: Tuple[ReportFile, ...]
    tool: str


@dataclass(frozen=True)
class Member:
    """A workspace member: package name, repository-relative directory, build scripts."""

    name: str
    directory: str
    build_scripts: frozenset


@dataclass
class Inventory:
    """Workspace members from `cargo metadata --no-deps`."""

    root: str
    target_directory: Optional[str]
    members: Dict[str, Member]

    def owner(self, relative: str) -> Optional[Member]:
        """Return the member whose directory most specifically contains `relative`."""
        best = None
        for member in self.members.values():
            if _within(relative, member.directory) and (
                best is None or len(member.directory) > len(best.directory)
            ):
                best = member
        return best


@dataclass
class CrateCoverage:
    """Eligible line totals for one member and the count of its excluded files."""

    lines: int = 0
    covered: int = 0
    eligible_files: int = 0
    excluded: Counter = field(default_factory=Counter)

    @property
    def measured(self) -> bool:
        """Whether the member has at least one eligible instrumented line."""
        return self.lines > 0

    @property
    def percent(self) -> Fraction:
        """Exact line coverage in percent; only valid for a measured member."""
        return Fraction(self.covered * 100, self.lines)


@dataclass
class ExclusionTotals:
    """Files and lines excluded under one category."""

    files: int = 0
    lines: int = 0
    paths: List[str] = field(default_factory=list)


@dataclass(frozen=True)
class Entry:
    """One crate's floor: the line counts it was recorded from."""

    lines: int
    covered: int

    @property
    def percent(self) -> Fraction:
        """Exact recorded line coverage in percent."""
        return Fraction(self.covered * 100, self.lines)


@dataclass
class Baseline:
    """The reviewed floor and the host identity it is valid for."""

    target: str
    runner: str
    tolerance_pp: float
    reason: str
    crates: Dict[str, Entry]
    not_measured: Dict[str, str]


@dataclass(frozen=True)
class Finding:
    """One reason the floor fails in CI."""

    kind: str
    crate: str
    message: str


@dataclass(frozen=True)
class Placement:
    """Where one report file lands: its owning member and exclusion category."""

    path: str
    member: Optional[str]
    category: Optional[str]


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
    hooks = {"parse_constant": _reject_constant, "parse_float": _finite_float} if strict else {}
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle, **hooks)
    except OSError as error:
        raise InputError(f"cannot read {what} {path}: {error.strerror or error}") from error
    except ValueError as error:
        raise InputError(f"{what} {path} is not valid UTF-8 JSON: {error}") from error


def load_report(path: str) -> Report:
    """Load and validate a `cargo llvm-cov report --json --summary-only` export."""
    document = _read_json(path, "coverage report")
    if not isinstance(document, dict) or document.get("type") != LLVM_EXPORT_TYPE:
        raise InputError(f"coverage report {path} is not an {LLVM_EXPORT_TYPE} document")
    data = document.get("data")
    if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0], dict):
        raise InputError(f"coverage report {path} must hold exactly one export object in data")
    entries = data[0].get("files")
    # An empty export would mark every crate missing; it is a broken run, not a coverage result.
    if not isinstance(entries, list) or not entries:
        raise InputError(f"coverage report {path} lists no files; the instrumented run produced nothing to compare")
    files = []
    for index, entry in enumerate(entries):
        filename = entry.get("filename") if isinstance(entry, dict) else None
        summary = entry.get("summary") if isinstance(entry, dict) else None
        lines = summary.get("lines") if isinstance(summary, dict) else None
        if not isinstance(filename, str) or not filename or not isinstance(lines, dict):
            raise InputError(f"coverage report {path}: file entry {index} has no filename or line summary")
        count, covered = lines.get("count"), lines.get("covered")
        if not _count(count) or not _count(covered) or covered > count:
            raise InputError(f"coverage report {path}: {filename} has invalid line counts {count!r}/{covered!r}")
        files.append(ReportFile(filename, count, covered))
    tool_info = document.get("cargo_llvm_cov")
    version = tool_info.get("version") if isinstance(tool_info, dict) else None
    tool = f"cargo-llvm-cov {version}" if isinstance(version, str) and version else "llvm-cov export"
    return Report(tuple(files), tool)


def load_inventory(path: str) -> Inventory:
    """Load workspace members from `cargo metadata --no-deps --format-version 1` output."""
    document = _read_json(path, "cargo metadata")
    if not isinstance(document, dict):
        raise InputError(f"cargo metadata {path} is not a JSON object")
    root, member_ids, packages = (
        document.get("workspace_root"),
        document.get("workspace_members"),
        document.get("packages"),
    )
    if not isinstance(root, str) or not isinstance(member_ids, list) or not isinstance(packages, list):
        raise InputError(f"cargo metadata {path} lacks workspace_root, workspace_members, or packages")
    root = _absolute(root)
    by_id = {package.get("id"): package for package in packages if isinstance(package, dict)}
    members: Dict[str, Member] = {}
    for member_id in member_ids:
        package = by_id.get(member_id)
        name = package.get("name") if package else None
        manifest = package.get("manifest_path") if package else None
        if not isinstance(name, str) or not isinstance(manifest, str):
            raise InputError(f"cargo metadata {path}: workspace member {member_id!r} has no package entry")
        directory = _relative(posixpath.dirname(_absolute(manifest)), root)
        # A root package would own every path; this virtual workspace has none, so refuse the shape.
        if directory is None:
            raise InputError(f"cargo metadata {path}: member {name} is not in a subdirectory of {root}")
        if name in members:
            raise InputError(f"cargo metadata {path}: duplicate member name {name}")
        build_scripts = set()
        for target in package.get("targets") or []:
            source = target.get("src_path") if isinstance(target, dict) else None
            if isinstance(source, str) and "custom-build" in (target.get("kind") or []):
                relative = _relative(_absolute(source), root)
                if relative is not None:
                    build_scripts.add(relative)
        members[name] = Member(name, directory, frozenset(build_scripts))
    if not members:
        raise InputError(f"cargo metadata {path} lists no workspace members")
    target_directory = document.get("target_directory")
    return Inventory(root, _absolute(target_directory) if isinstance(target_directory, str) else None, members)


def place(filename: str, inventory: Inventory, source_roots: List[str], generated_roots: List[str]) -> Placement:
    """Attribute one report file to a member and decide whether it is excluded."""
    path = _normalize(filename)
    # Generated output is checked first because the target directory usually sits inside the checkout.
    if any(_within(path, root) for root in generated_roots):
        return Placement(path, None, "generated")
    relative = next((rel for rel in (_relative(path, root) for root in source_roots) if rel is not None), None)
    if relative is None:
        return Placement(path, None, "outside-repository")
    owner = inventory.owner(relative)
    member = owner.name if owner else None
    if any(_within(relative, root) for root in VENDORED_ROOTS):
        return Placement(relative, member, "vendored")
    if relative in GENERATED_SOURCES:
        return Placement(relative, member, "generated")
    if owner is None:
        return Placement(relative, None, "outside-members")
    inner = _relative(relative, owner.directory) or ""
    parts = inner.split("/")
    if relative in owner.build_scripts or inner == "build.rs":
        return Placement(relative, member, "build-script")
    if parts[-1].endswith("_tests.rs") or any(part in TEST_DIRECTORIES for part in parts[:-1]):
        return Placement(relative, member, "test")
    return Placement(relative, member, None)


def aggregate(
    report: Report, inventory: Inventory, source_root: Optional[str] = None
) -> Tuple[Dict[str, CrateCoverage], Dict[str, ExclusionTotals]]:
    """Sum eligible lines per member and tally excluded files by category."""
    base = source_root or inventory.root
    source_roots = _root_forms(base)
    generated_roots = set(_root_forms(posixpath.join(_absolute(base), "target")))
    generated_roots.update(_root_forms(posixpath.join(inventory.root, "target")))
    if inventory.target_directory:
        generated_roots.update(_root_forms(inventory.target_directory))
    crates = {name: CrateCoverage() for name in inventory.members}
    exclusions = {category: ExclusionTotals() for category in EXCLUSION_REASONS}
    seen: Dict[str, str] = {}
    for entry in report.files:
        placement = place(entry.filename, inventory, source_roots, sorted(generated_roots))
        # Two spellings of one file would count its lines twice; refuse the report instead.
        if placement.path in seen:
            raise InputError(f"coverage report lists {placement.path} twice "
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


def unmeasured_detail(crate: CrateCoverage) -> str:
    """Explain why a member has no eligible instrumented line."""
    if crate.eligible_files:
        return f"{crate.eligible_files} reported file(s) contain no instrumented line"
    if crate.excluded:
        categories = ", ".join(f"{name}: {count}" for name, count in sorted(crate.excluded.items()))
        return f"every reported file is excluded ({categories})"
    return "no reported file"


_BASELINE_KEYS = {"schema", "host", "tolerance_pp", "reason", "crates", "not_measured"}


def parse_baseline(document: object, source: str) -> Baseline:
    """Validate a baseline document strictly and return its parsed form."""
    if not isinstance(document, dict):
        raise InputError(f"baseline {source} is not a JSON object")
    problems = []
    unknown = sorted(set(document) - _BASELINE_KEYS)
    if unknown:
        problems.append(f"unknown keys {unknown}")
    if document.get("schema") != SCHEMA:
        problems.append(f"schema must be {SCHEMA!r}")
    host = document.get("host")
    target = host.get("target") if isinstance(host, dict) else None
    runner = host.get("runner") if isinstance(host, dict) else None
    if not isinstance(host, dict) or set(host) != {"target", "runner"}:
        problems.append("host must hold exactly target and runner")
    if not isinstance(target, str) or not target.strip() or not isinstance(runner, str) or not runner.strip():
        problems.append("host target and runner must be non-empty strings")
    tolerance = document.get("tolerance_pp")
    # A non-finite tolerance passes `> 0` and then fails inside Fraction; refuse it here.
    if not _finite_number(tolerance) or not tolerance > 0:
        problems.append("tolerance_pp must be a positive, finite number of percentage points")
    reason = document.get("reason")
    if not isinstance(reason, str) or not reason.strip():
        problems.append("reason must state why the baseline last changed")
    crates: Dict[str, Entry] = {}
    raw_crates = document.get("crates")
    if not isinstance(raw_crates, dict):
        problems.append("crates must be an object")
        raw_crates = {}
    for name, entry in raw_crates.items():
        if not isinstance(entry, dict) or set(entry) != {"lines", "covered", "percent"}:
            problems.append(f"crates.{name} must hold exactly lines, covered, and percent")
            continue
        lines, covered, percent = entry["lines"], entry["covered"], entry["percent"]
        if not _count(lines) or lines == 0 or not _count(covered) or covered > lines:
            problems.append(f"crates.{name} needs lines > 0 and 0 <= covered <= lines")
            continue
        exact = covered * 100 / lines
        # NaN compares false against the slop, so finiteness is checked first.
        if not _finite_number(percent) or abs(percent - exact) > PERCENT_SLOP:
            problems.append(f"crates.{name}.percent must be the finite value {round(exact, 2)} "
                            f"for {covered}/{lines}")
            continue
        crates[name] = Entry(lines, covered)
    not_measured: Dict[str, str] = {}
    raw_declared = document.get("not_measured")
    if not isinstance(raw_declared, dict):
        problems.append("not_measured must be an object")
        raw_declared = {}
    for name, why in raw_declared.items():
        if not isinstance(why, str) or not why.strip():
            problems.append(f"not_measured.{name} must state why the crate is not measured")
            continue
        not_measured[name] = why
    overlap = sorted(set(raw_crates) & set(raw_declared))
    if overlap:
        problems.append(f"crates and not_measured both name {overlap}")
    if problems:
        raise InputError(f"baseline {source} is invalid: " + "; ".join(problems))
    return Baseline(target, runner, float(tolerance), reason, crates, not_measured)


def load_baseline(path: str) -> Baseline:
    """Read and validate the baseline file at `path`, refusing non-finite numbers."""
    return parse_baseline(_read_json(path, "baseline", strict=True), str(path))


def _pct(value: Fraction) -> str:
    """Format an exact percentage for the table."""
    return f"{float(value):.2f}%"


def _delta(value: Fraction) -> str:
    """Format a signed percentage-point difference."""
    rounded = float(value)
    return "+0.00" if abs(rounded) < 0.005 else f"{rounded:+.2f}"


def runner_identity(env: Mapping[str, str]) -> str:
    """Name the host running the check: the GitHub runner image in CI, else `local`."""
    if env.get("GITHUB_ACTIONS") != "true":
        return "local"
    return "{}/{}".format(env.get("ImageOS") or "unknown-image", env.get("RUNNER_ARCH") or "unknown-arch")


def host_os(target: str) -> Optional[str]:
    """Return the operating system a target triple compiles for, when recognized."""
    if "apple-darwin" in target:
        return "macos"
    if "windows" in target:
        return "windows"
    if "linux" in target:
        return "linux"
    return None


def platform_note(name: str, target: str) -> str:
    """Label a single-platform crate measured on a host of another platform."""
    platform = PLATFORM_CRATES.get(name)
    host = host_os(target)
    if platform is None or platform == host:
        return ""
    return f"compiled on {OS_LABELS.get(host, target)}; not {OS_LABELS[platform]} execution coverage"


def compare(
    crates: Dict[str, CrateCoverage], inventory: Inventory, baseline: Baseline
) -> Tuple[Dict[str, str], List[Finding]]:
    """Return each member's row status and every finding that fails the floor."""
    tolerance = Fraction(str(baseline.tolerance_pp))
    statuses: Dict[str, str] = {}
    findings: List[Finding] = []
    for name in sorted(inventory.members):
        crate = crates[name]
        entry = baseline.crates.get(name)
        declared = baseline.not_measured.get(name)
        if crate.measured:
            measured = f"{_pct(crate.percent)} ({crate.covered}/{crate.lines} lines)"
            if entry is not None:
                delta = crate.percent - entry.percent
                # Exact fractions: a drop of exactly the tolerance holds on every host.
                if delta < -tolerance:
                    statuses[name] = "DROP"
                    findings.append(Finding(DROP, name, (
                        f"{name}: {measured} is {float(-delta):.2f} pp below its floor {_pct(entry.percent)} "
                        f"({entry.covered}/{entry.lines}); the tolerance is {baseline.tolerance_pp} pp. Add tests, "
                        "or lower the floor in a reviewed baseline change that states the reason.")))
                else:
                    statuses[name] = "ok"
            elif declared is not None:
                statuses[name] = "NOW MEASURED"
                findings.append(Finding(NOW_MEASURED, name, (
                    f"{name}: declared not measured ({declared}) but now reports {measured}. "
                    "Replace the declaration with a floor in a reviewed baseline change.")))
            else:
                statuses[name] = "NO BASELINE"
                findings.append(Finding(NO_BASELINE, name, (
                    f"{name}: measures {measured} but has no baseline entry. A measured crate fails "
                    "until a reviewed baseline change adds its floor.")))
        elif entry is not None:
            statuses[name] = "MISSING"
            findings.append(Finding(MISSING_MEASURED, name, (
                f"{name}: has a floor of {_pct(entry.percent)} ({entry.covered}/{entry.lines}) but reports "
                f"no eligible line ({unmeasured_detail(crate)}). A previously measured crate must not leave "
                "the floor silently; restore it, or replace the entry with a declared reason in a reviewed change.")))
        elif declared is not None:
            statuses[name] = "declared"
        else:
            statuses[name] = "UNEXPLAINED"
            findings.append(Finding(UNEXPLAINED, name, (
                f"{name}: workspace member with no eligible line ({unmeasured_detail(crate)}), no floor, "
                "and no not-measured declaration. Declare why it is not measured in a reviewed change.")))
    for name in sorted(set(baseline.crates) - set(inventory.members)):
        entry = baseline.crates[name]
        findings.append(Finding(MISSING_MEMBER, name, (
            f"{name}: has a floor of {_pct(entry.percent)} but is no longer a workspace member. "
            "Remove its entry in a reviewed change.")))
    for name in sorted(set(baseline.not_measured) - set(inventory.members)):
        findings.append(Finding(STALE_DECLARATION, name, (
            f"{name}: declared not measured but is not a workspace member. "
            "Remove the declaration in a reviewed change.")))
    return statuses, findings


def render_table(
    crates: Dict[str, CrateCoverage],
    inventory: Inventory,
    baseline: Baseline,
    statuses: Dict[str, str],
    target: str,
    informational: bool,
) -> List[str]:
    """Render one row per workspace member; unmeasured rows never show a percentage."""
    headers = ["crate", "status", "lines", "covered", "coverage", "baseline",
               "delta (info)" if informational else "delta", "note"]
    rows = []
    for name in sorted(inventory.members):
        crate = crates[name]
        entry = baseline.crates.get(name)
        if crate.measured:
            recorded = _pct(entry.percent) if entry else "-"
            delta = _delta(crate.percent - entry.percent) if entry else "-"
            rows.append([name, statuses[name], str(crate.lines), str(crate.covered), _pct(crate.percent),
                         recorded, delta, platform_note(name, target)])
        else:
            # No current measurement, so no number: a vanished floor is named in its finding only.
            declared = baseline.not_measured.get(name)
            note = declared if declared is not None else unmeasured_detail(crate)
            rows.append([name, statuses[name], "-", "-", "not measured", "-", "-", note])
    right_aligned = {2, 3, 4, 5, 6}
    widths = [max(len(row[i]) for row in [headers] + rows) for i in range(len(headers) - 1)]

    def format_row(cells: List[str]) -> str:
        parts = [cell.rjust(widths[i]) if i in right_aligned else cell.ljust(widths[i])
                 for i, cell in enumerate(cells[:-1])]
        return "  ".join(parts + [cells[-1]]).rstrip()

    return [format_row(headers)] + [format_row(row) for row in rows]


def render_exclusions(exclusions: Dict[str, ExclusionTotals]) -> List[str]:
    """Render every exclusion category with its counts and reason."""
    lines = ["Excluded from the per-crate figures:"]
    for category, reason in EXCLUSION_REASONS.items():
        totals = exclusions[category]
        lines.append(f"  {category:<18} files={totals.files:<4} lines={totals.lines:<7} {reason}")
        if category in LISTED_CATEGORIES:
            lines.extend(f"      {path}" for path in totals.paths[:LISTED_PATH_LIMIT])
            if len(totals.paths) > LISTED_PATH_LIMIT:
                lines.append(f"      ... and {len(totals.paths) - LISTED_PATH_LIMIT} more")
    return lines


def baseline_document(baseline: Baseline) -> dict:
    """Return the canonical JSON document for a baseline, with sorted crate keys."""
    return {
        "schema": SCHEMA,
        "host": {"target": baseline.target, "runner": baseline.runner},
        "tolerance_pp": baseline.tolerance_pp,
        "reason": baseline.reason,
        "crates": {
            name: {"lines": entry.lines, "covered": entry.covered,
                   "percent": round(entry.covered * 100 / entry.lines, 2)}
            for name, entry in sorted(baseline.crates.items())
        },
        "not_measured": {name: baseline.not_measured[name] for name in sorted(baseline.not_measured)},
    }


def render_document(document: dict) -> str:
    """Serialize a baseline document deterministically; a non-finite number raises instead of writing NaN."""
    return json.dumps(document, indent=2, ensure_ascii=True, allow_nan=False) + "\n"


def additions_proposal(baseline: Baseline, crates: Dict[str, CrateCoverage], findings: List[Finding]) -> dict:
    """Propose the current baseline plus entries for crates that lack one.

    Existing floors are kept verbatim, so the adopted diff shows only additions.
    The reasons are left empty: the baseline refuses them until a reviewer writes them.
    """
    entries = dict(baseline.crates)
    declared = dict(baseline.not_measured)
    for finding in findings:
        if finding.kind in (NO_BASELINE, NOW_MEASURED):
            crate = crates[finding.crate]
            entries[finding.crate] = Entry(crate.lines, crate.covered)
            declared.pop(finding.crate, None)
        elif finding.kind == UNEXPLAINED:
            declared[finding.crate] = ""
    return baseline_document(Baseline(baseline.target, baseline.runner, baseline.tolerance_pp, "", entries, declared))


def rebaseline_proposal(baseline: Baseline, crates: Dict[str, CrateCoverage], target: str, runner: str) -> dict:
    """Propose a complete baseline for this host, keeping declared reasons that still apply."""
    entries = {name: Entry(crate.lines, crate.covered) for name, crate in crates.items() if crate.measured}
    declared = {name: baseline.not_measured.get(name, "") for name, crate in crates.items() if not crate.measured}
    return baseline_document(Baseline(target, runner, baseline.tolerance_pp, "", entries, declared))


def provenance(target: str, runner: str, toolchain: Optional[str], tool: str, env: Mapping[str, str]) -> str:
    """Describe where the measured numbers came from, for the reviewer's stated reason."""
    parts = [f"{target} on {runner}", toolchain or "rustc version not given", tool]
    server, repository, run_id, sha = (
        env.get(key) for key in ("GITHUB_SERVER_URL", "GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_SHA")
    )
    if server and repository and run_id:
        parts.append(f"{server}/{repository}/actions/runs/{run_id}")
    if sha:
        parts.append(f"commit {sha}")
    return ", ".join(parts)


def render_proposal(document: dict, measured_on: str) -> List[str]:
    """Render a proposed baseline between markers, with adoption instructions."""
    return [
        "Proposed baseline. The floor never adopts it by itself. To adopt it, copy the JSON",
        "between the markers into scripts/coverage-baseline.json, write the top-level",
        "\"reason\" and every empty \"not_measured\" reason, and commit it for review.",
        "An empty reason fails the next run.",
        f"Measured on: {measured_on}",
        PROPOSAL_BEGIN,
        *render_document(document).rstrip("\n").splitlines(),
        PROPOSAL_END,
        "",
    ]


def run_check(args: argparse.Namespace, env: Mapping[str, str], out) -> int:
    """Print the per-crate report and evaluate the floor; return the exit status."""
    report = load_report(args.report)
    inventory = load_inventory(args.metadata)
    baseline = load_baseline(args.baseline)
    crates, exclusions = aggregate(report, inventory, args.source_root)
    statuses, findings = compare(crates, inventory, baseline)
    runner = runner_identity(env)
    in_ci = env.get("GITHUB_ACTIONS") == "true"
    enforced = in_ci and (args.target, runner) == (baseline.target, baseline.runner)
    if enforced:
        mode = "enforced: CI on the baseline host"
    elif in_ci:
        mode = "not comparable: CI on a host other than the baseline's"
    else:
        mode = "informational: outside CI the floor is not enforced and gives no verdict"
    lines = [
        f"Per-crate line coverage ({report.tool}; test, vendored, generated, and build-script code excluded)",
        f"  host:     {args.target} on {runner} ({args.toolchain or 'rustc version not given'})",
        f"  baseline: {baseline.target} on {baseline.runner}, tolerance {baseline.tolerance_pp} pp ({args.baseline})",
        f"  mode:     {mode}",
        "",
    ]
    lines += render_table(crates, inventory, baseline, statuses, args.target, informational=not enforced)
    lines.append("")
    lines += render_exclusions(exclusions)
    lines.append("")
    if findings:
        if enforced:
            lines.append("Floor findings (each fails CI):")
        elif in_ci:
            lines.append("Findings (informational; the host mismatch alone fails this run):")
        else:
            lines.append("Findings (informational; CI on the baseline host would fail on each):")
        lines += [f"  - {finding.message}" for finding in findings]
    else:
        lines.append("Floor findings: none")
    lines.append("")
    measured_on = provenance(args.target, runner, args.toolchain, report.tool, env)
    measured = sum(1 for crate in crates.values() if crate.measured)
    declared = sum(1 for status in statuses.values() if status == "declared")
    if enforced and not findings:
        lines.append(f"coverage floor PASS: {measured} measured crates within {baseline.tolerance_pp} pp "
                     f"of their floors; {declared} declared not measured")
        status = 0
    elif enforced:
        if any(finding.kind in PROPOSABLE for finding in findings):
            lines += render_proposal(additions_proposal(baseline, crates, findings), measured_on)
        lines.append(f"coverage floor FAIL: {len(findings)} finding(s) listed above")
        status = 1
    elif in_ci:
        lines += render_proposal(rebaseline_proposal(baseline, crates, args.target, runner), measured_on)
        lines.append(f"coverage floor FAIL: not comparable: the baseline is for {baseline.target} on "
                     f"{baseline.runner}, this run is {args.target} on {runner}; a baseline from another "
                     "host is never a passing result")
        status = 1
    else:
        lines.append(f"coverage floor: informational only; no verdict outside CI. CI on {baseline.target} "
                     f"on {baseline.runner} enforces the floor.")
        status = 0
    print("\n".join(lines), file=out)
    return status


def full_update(
    existing: Optional[Baseline], crates: Dict[str, CrateCoverage], inventory: Inventory,
    target: str, runner: str, reason: str,
) -> Baseline:
    """Build a baseline holding every measured member and every still-applicable declaration."""
    tolerance = existing.tolerance_pp if existing else DEFAULT_TOLERANCE_PP
    previous = existing.not_measured if existing else {}
    entries: Dict[str, Entry] = {}
    declared: Dict[str, str] = {}
    undeclared = []
    for name in sorted(inventory.members):
        crate = crates[name]
        if crate.measured:
            entries[name] = Entry(crate.lines, crate.covered)
        elif previous.get(name, "").strip():
            declared[name] = previous[name]
        else:
            undeclared.append(f"{name} ({unmeasured_detail(crate)})")
    # The tool cannot invent why a crate is unmeasured; a human states it in the baseline first.
    if undeclared:
        raise InputError("refusing to write a baseline: these members are not measured and not declared: "
                         + "; ".join(undeclared)
                         + ". Add each to \"not_measured\" with its reason, then rerun.")
    return Baseline(target, runner, tolerance, reason, entries, declared)


def partial_update(
    existing: Optional[Baseline], crates: Dict[str, CrateCoverage], inventory: Inventory,
    target: str, runner: str, reason: str, names: List[str],
) -> Baseline:
    """Replace only the named crates' floors, keeping every other entry verbatim."""
    if existing is None:
        raise InputError("--crate updates entries of an existing baseline, and none was found")
    # Floors measured on two hosts are not comparable, so one file never mixes them.
    if (existing.target, existing.runner) != (target, runner):
        raise InputError(f"--crate cannot mix hosts: the baseline is for {existing.target} on {existing.runner}, "
                         f"this report is {target} on {runner}; update every crate for a new host instead")
    entries = dict(existing.crates)
    declared = dict(existing.not_measured)
    for name in names:
        if name not in inventory.members:
            raise InputError(f"--crate {name} is not a workspace member")
        crate = crates[name]
        if not crate.measured:
            raise InputError(f"--crate {name} has no eligible line in this report ({unmeasured_detail(crate)}); "
                             "declare it in \"not_measured\" with a reason instead")
        entries[name] = Entry(crate.lines, crate.covered)
        declared.pop(name, None)
    return Baseline(existing.target, existing.runner, existing.tolerance_pp, reason, entries, declared)


def describe_update(old: Optional[Baseline], new: Baseline) -> List[str]:
    """Summarize what an update changes, for the reviewer reading the command output."""
    lines = []
    if old is None:
        lines.append(f"new baseline for {new.target} on {new.runner}")
    elif (old.target, old.runner) != (new.target, new.runner):
        lines.append(f"host: {old.target} on {old.runner} -> {new.target} on {new.runner}")
    old_crates = old.crates if old else {}
    for name in sorted(set(old_crates) | set(new.crates)):
        before, after = old_crates.get(name), new.crates.get(name)
        if before == after:
            continue
        if before is None:
            lines.append(f"  + {name}: {_pct(after.percent)} ({after.covered}/{after.lines})")
        elif after is None:
            lines.append(f"  - {name}: floor {_pct(before.percent)} removed")
        else:
            lines.append(f"  ~ {name}: {_pct(before.percent)} ({before.covered}/{before.lines}) -> "
                         f"{_pct(after.percent)} ({after.covered}/{after.lines})")
    old_declared = old.not_measured if old else {}
    lines += [f"  - {name}: not-measured declaration removed"
              for name in sorted(set(old_declared) - set(new.not_measured))]
    lines += [f"  + {name}: declared not measured" for name in sorted(set(new.not_measured) - set(old_declared))]
    lines.append(f"reason: {new.reason}")
    return lines


def _write_atomic(path: Path, text: str) -> None:
    """Replace `path` with `text` so an interrupted write never leaves a partial baseline."""
    handle, temporary = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp", dir=str(path.parent))
    try:
        with os.fdopen(handle, "w", encoding="utf-8", newline="\n") as stream:
            stream.write(text)
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def run_update(args: argparse.Namespace, out) -> int:
    """Write the baseline from a report for the stated host identity."""
    reason = (args.reason or "").strip()
    if not reason:
        raise InputError("--update-baseline requires a non-empty --reason stating why the floor changes")
    runner = (args.runner or "").strip()
    if not runner:
        raise InputError("--update-baseline requires --runner naming the host that produced the report, "
                         "for example macos14/ARM64")
    report = load_report(args.report)
    inventory = load_inventory(args.metadata)
    path = Path(args.baseline)
    existing = load_baseline(str(path)) if path.exists() else None
    crates, _ = aggregate(report, inventory, args.source_root)
    if args.crate:
        updated = partial_update(existing, crates, inventory, args.target, runner, reason, args.crate)
    else:
        updated = full_update(existing, crates, inventory, args.target, runner, reason)
    text = render_document(baseline_document(updated))
    # Never write a file the check would refuse to load.
    parse_baseline(json.loads(text), str(path))
    _write_atomic(path, text)
    for line in describe_update(existing, updated):
        print(line, file=out)
    print(f"Wrote {path} for {args.target} on {runner}. The host identity is as stated by --target and "
          "--runner; review and commit the diff.", file=out)
    return 0


def build_parser() -> argparse.ArgumentParser:
    """Return the command-line parser."""
    parser = argparse.ArgumentParser(description="Per-crate line-coverage report and regression floor.")
    parser.add_argument("--report", required=True, help="cargo llvm-cov report --json --summary-only output")
    parser.add_argument("--metadata", required=True, help="cargo metadata --no-deps --format-version 1 output")
    parser.add_argument("--baseline", required=True, help="baseline JSON, normally scripts/coverage-baseline.json")
    parser.add_argument("--target", required=True, help="host target triple of the instrumented build (rustc -vV)")
    parser.add_argument("--toolchain", help="rustc version line, printed as provenance")
    parser.add_argument("--source-root", help="directory the report's absolute filenames sit under "
                                              "(default: the metadata workspace_root)")
    parser.add_argument("--update-baseline", action="store_true",
                        help="write the baseline from this report instead of checking it")
    parser.add_argument("--reason", help="why the baseline changes; required with --update-baseline")
    parser.add_argument("--runner", help="runner identity of the host that produced the report, for example "
                                         "macos14/ARM64; required with --update-baseline")
    parser.add_argument("--crate", action="append", metavar="NAME",
                        help="with --update-baseline, replace only this crate's floor (repeatable)")
    return parser


def main(argv: Optional[List[str]] = None, env: Optional[Mapping[str, str]] = None, stdout=None, stderr=None) -> int:
    """Run the check or the baseline update and return the exit status."""
    env = os.environ if env is None else env
    stdout = sys.stdout if stdout is None else stdout
    stderr = sys.stderr if stderr is None else stderr
    args = build_parser().parse_args(argv)
    try:
        if args.update_baseline:
            return run_update(args, stdout)
        misplaced = [flag for flag, value in (("--reason", args.reason), ("--runner", args.runner),
                                              ("--crate", args.crate)) if value is not None]
        # Check mode takes the runner from the CI environment, so a stated one cannot fake a match.
        if misplaced:
            raise InputError(", ".join(misplaced) + " is only valid with --update-baseline")
        return run_check(args, env, stdout)
    except InputError as error:
        print(f"coverage-floor: {error}", file=stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
