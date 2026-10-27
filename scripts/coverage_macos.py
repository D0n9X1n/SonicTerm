"""Coverage macos."""

from __future__ import annotations
import argparse
from fractions import Fraction
import json
from pathlib import Path
from typing import Dict, List, Mapping, Optional, Tuple

import coverage_baselines
import coverage_checkout
import coverage_comparison
import coverage_model
import coverage_provenance
import coverage_reports


def _macos_measured(report: coverage_model.Report, source_root: str) -> Dict[str, coverage_model.ReportFile]:
    """Map each reported file under `source_root` to its repository-relative path."""
    roots = coverage_reports._root_forms(source_root)
    measured: Dict[str, ReportFile] = {}
    for report_file in report.files:
        absolute = coverage_reports._absolute(report_file.filename)
        relative = next((path for path in (coverage_reports._relative(absolute, root) for root in roots) if path), None)
        if relative is not None:
            measured[relative] = report_file
    return measured


def run_macos_floor(argv: List[str], env: Mapping[str, str], out) -> int:
    """Check, or with --update rewrite, the baseline's macOS section; return the exit status."""
    parser = argparse.ArgumentParser(prog="coverage-floor.py macos-floor",
                                     description="Per-file floors for macOS-only logic, measured on macOS.")
    parser.add_argument("--report", required=True, help="cargo llvm-cov report --json --summary-only output")
    parser.add_argument("--baseline", required=True, help="baseline JSON, normally scripts/coverage-baseline.json")
    parser.add_argument("--target", required=True, help="host target triple of the instrumented build (rustc -vV)")
    parser.add_argument("--source-root", required=True, help="directory the report's absolute filenames sit under")
    parser.add_argument("--update", action="store_true", help="rewrite the macOS section from this report")
    parser.add_argument("--reason", help="with --update, why the macOS floors change")
    parser.add_argument("--provenance", metavar="FILE",
                        help="with --update, the record of the macos-core run that measured the report")
    parser.add_argument("--runner", help="with --update, a check against the record's runner")
    parser.add_argument("--file", action="append", metavar="PATH",
                        help="with --update, a repository-relative file to add to the section (repeatable)")
    args = parser.parse_args(argv)
    report = coverage_reports.load_report(args.report)
    baseline = coverage_baselines.load_baseline(args.baseline)
    measured = _macos_measured(report, args.source_root)
    if args.update:
        return _update_macos(args, baseline, report, measured, out)
    misplaced = [flag for flag, value in (("--reason", args.reason), ("--runner", args.runner),
                                          ("--file", args.file), ("--provenance", args.provenance))
                 if value is not None]
    # Check mode takes the runner from the CI environment, so a stated one cannot fake a match.
    if misplaced:
        raise coverage_model.InputError(", ".join(misplaced) + " is only valid with --update")
    section = baseline.macos
    if section is None:
        raise coverage_model.InputError(f"baseline {args.baseline} has no macos section to check")
    runner = coverage_comparison.runner_identity(env)
    in_ci = env.get("GITHUB_ACTIONS") == "true"
    enforced = in_ci and (args.target, runner) == (section.target, section.runner)
    tolerance = Fraction(str(baseline.tolerance_pp))
    lines = [
        f"macOS-only logic coverage ({report.tool})",
        f"  host:     {args.target} on {runner}",
        f"  baseline: {section.target} on {section.runner}, tolerance {baseline.tolerance_pp} pp ({args.baseline})",
        "",
    ]
    failures = []
    for file_path, entry in sorted(section.files.items()):
        report_file = measured.get(file_path)
        if report_file is None or report_file.lines == 0:
            failures.append(f"{file_path}: not measured; this run reports no instrumented line for it")
            lines.append(f"{file_path}  not measured  floor {coverage_comparison._pct(entry.percent)} ({entry.covered}/{entry.lines})")
            continue
        percent = Fraction(report_file.covered * 100, report_file.lines)
        delta = percent - entry.percent
        # Exact fractions, as for the crate floors: a drop of exactly the tolerance holds.
        dropped = delta < -tolerance
        lines.append(f"{file_path}  {coverage_comparison._pct(percent)} ({report_file.covered}/{report_file.lines})  "
                     f"floor {coverage_comparison._pct(entry.percent)} ({entry.covered}/{entry.lines})  {coverage_comparison._delta(delta)}  "
                     + ("DROP" if dropped else "ok"))
        if dropped:
            failures.append(f"{file_path}: {coverage_comparison._pct(percent)} is {float(-delta):.2f} pp below its floor "
                            f"{coverage_comparison._pct(entry.percent)}; the tolerance is {baseline.tolerance_pp} pp")
    lines.append("")
    lines += [f"  - {failure}" for failure in failures]
    if enforced and not failures:
        lines.append(f"macOS floor PASS: {len(section.files)} files within {baseline.tolerance_pp} pp of their floors")
        status = 0
    elif enforced:
        lines.append(f"macOS floor FAIL: {len(failures)} finding(s) listed above")
        status = 1
    elif in_ci:
        lines.append(f"macOS floor FAIL: not comparable: the macos section is for {section.target} on "
                     f"{section.runner}, this run is {args.target} on {runner}; a floor from another host is "
                     "never a passing result")
        status = 1
    else:
        lines.append(f"macOS floor: informational only; no verdict outside CI. CI on {section.target} on "
                     f"{section.runner} enforces it.")
        status = 0
    print("\n".join(lines), file=out)
    return status


def _update_macos(args: argparse.Namespace, baseline: coverage_model.Baseline, report: coverage_model.Report, measured: Dict[str, coverage_model.ReportFile],
                  out) -> int:
    """Rewrite only the baseline's macOS section from a verified macos-core run, keeping every crate floor verbatim."""
    reason = (args.reason or "").strip()
    if not reason:
        raise coverage_model.InputError("macos-floor --update requires a non-empty --reason stating why the floors change")
    # As for the crate floors, only a verified CI record changes a floor; a stated host or a local run never does.
    if not args.provenance:
        raise coverage_model.InputError("macos-floor --update requires --provenance FILE, the record of the macos-core run that "
                         f"measured the report; retrieve and verify it as {coverage_model.REBASELINE_PROCEDURE} describes")
    record = coverage_provenance.verify_provenance(args.provenance, args.report, None, report, args.target,
                               (args.runner or "").strip() or None, Path(args.baseline), coverage_provenance.MACOS_EVIDENCE)
    paths = set(baseline.macos.files if baseline.macos else ()) | set(args.file or ())
    if not paths:
        raise coverage_model.InputError("macos-floor --update needs --file PATH while the baseline has no macos section")
    files: Dict[str, coverage_model.Entry] = {}
    for file_path in sorted(paths):
        report_file = measured.get(file_path)
        if report_file is None or report_file.lines == 0:
            raise coverage_model.InputError(f"macos-floor --update: {file_path} has no instrumented line in this report")
        files[file_path] = coverage_model.Entry(report_file.lines, report_file.covered)
    baseline.macos = coverage_model.MacosSection(record.target, record.runner, f"{reason.rstrip('.')}. {record.sentence()}", files)
    text = coverage_comparison.render_document(coverage_comparison.baseline_document(baseline))
    baseline_path = Path(args.baseline)
    coverage_baselines.parse_baseline(json.loads(text), str(baseline_path))
    coverage_checkout._write_atomic(baseline_path, text)
    print(f"Wrote the macos section of {baseline_path} for {record.target} on {record.runner} from the verified "
          "provenance record: "
          + ", ".join(f"{file_path} {coverage_comparison._pct(entry.percent)}" for file_path, entry in files.items())
          + ". Review and commit the diff.", file=out)
    return 0
