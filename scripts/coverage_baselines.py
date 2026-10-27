"""Coverage baselines."""

from __future__ import annotations
from typing import Dict, List, Mapping, Optional, Tuple

import coverage_model
import coverage_reports


_BASELINE_KEYS = {"schema", "host", "tolerance_pp", "reason", "crates", "not_measured", "macos"}


def parse_baseline(document: object, source: str) -> coverage_model.Baseline:
    """Validate a baseline document strictly and return its parsed form."""
    if not isinstance(document, dict):
        raise coverage_model.InputError(f"baseline {source} is not a JSON object")
    problems = []
    unknown = sorted(set(document) - _BASELINE_KEYS)
    if unknown:
        problems.append(f"unknown keys {unknown}")
    if document.get("schema") != coverage_model.SCHEMA:
        problems.append(f"schema must be {coverage_model.SCHEMA!r}")
    host = document.get("host")
    target = host.get("target") if isinstance(host, dict) else None
    runner = host.get("runner") if isinstance(host, dict) else None
    if not isinstance(host, dict) or set(host) != {"target", "runner"}:
        problems.append("host must hold exactly target and runner")
    if not isinstance(target, str) or not target.strip() or not isinstance(runner, str) or not runner.strip():
        problems.append("host target and runner must be non-empty strings")
    tolerance = document.get("tolerance_pp")
    # A non-finite tolerance passes `> 0` and then fails inside Fraction; refuse it here.
    if not coverage_reports._finite_number(tolerance) or not tolerance > 0:
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
        parsed = _parse_entry(f"crates.{name}", entry, problems)
        if parsed is not None:
            crates[name] = parsed
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
    macos = _parse_macos(document["macos"], problems) if "macos" in document else None
    if problems:
        raise coverage_model.InputError(f"baseline {source} is invalid: " + "; ".join(problems))
    return coverage_model.Baseline(target, runner, float(tolerance), reason, crates, not_measured, macos)


def _parse_entry(label: str, entry: object, problems: List[str]) -> Optional[coverage_model.Entry]:
    """Validate one recorded floor, a crate's or a macOS file's; report a problem and return None when invalid."""
    if not isinstance(entry, dict) or set(entry) != {"lines", "covered", "percent"}:
        problems.append(f"{label} must hold exactly lines, covered, and percent")
        return None
    lines, covered, percent = entry["lines"], entry["covered"], entry["percent"]
    if not coverage_reports._count(lines) or lines == 0 or not coverage_reports._count(covered) or covered > lines:
        problems.append(f"{label} needs lines > 0 and 0 <= covered <= lines")
        return None
    exact = covered * 100 / lines
    # NaN compares false against the slop, so finiteness is checked first.
    if not coverage_reports._finite_number(percent) or abs(percent - exact) > coverage_model.PERCENT_SLOP:
        problems.append(f"{label}.percent must be the finite value {round(exact, 2)} for {covered}/{lines}")
        return None
    return coverage_model.Entry(lines, covered)


def _parse_macos(raw: object, problems: List[str]) -> Optional[coverage_model.MacosSection]:
    """Validate the optional macOS section: its own host, its reason, and at least one file floor."""
    if not isinstance(raw, dict) or set(raw) != {"host", "reason", "files"}:
        problems.append("macos must hold exactly host, reason, and files")
        return None
    host = raw["host"]
    target = host.get("target") if isinstance(host, dict) else None
    runner = host.get("runner") if isinstance(host, dict) else None
    if (not isinstance(host, dict) or set(host) != {"target", "runner"} or not isinstance(target, str)
            or not target.strip() or not isinstance(runner, str) or not runner.strip()):
        problems.append("macos.host must hold exactly non-empty target and runner strings")
    reason = raw["reason"]
    if not isinstance(reason, str) or not reason.strip():
        problems.append("macos.reason must state why the macOS floors last changed")
    files: Dict[str, Entry] = {}
    raw_files = raw["files"]
    if not isinstance(raw_files, dict) or not raw_files:
        problems.append("macos.files must name at least one file")
        raw_files = {}
    for path, entry in raw_files.items():
        parsed = _parse_entry(f"macos.files.{path}", entry, problems)
        if parsed is not None:
            files[path] = parsed
    return coverage_model.MacosSection(str(target), str(runner), str(reason), files)


def load_baseline(path: str) -> coverage_model.Baseline:
    """Read and validate the baseline file at `path`, refusing non-finite numbers."""
    return parse_baseline(coverage_reports._read_json(path, "baseline", strict=True), str(path))
