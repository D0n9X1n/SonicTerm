"""Coverage comparison."""

from __future__ import annotations
import argparse
from fractions import Fraction
import json
from typing import Dict, List, Mapping, Optional, Tuple

import coverage_baselines
import coverage_model
import coverage_reports


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
        return coverage_model.LOCAL_RUNNER
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
    platform = coverage_model.PLATFORM_CRATES.get(name)
    host = host_os(target)
    if platform is None or platform == host:
        return ""
    return f"compiled on {coverage_model.OS_LABELS.get(host, target)}; not {coverage_model.OS_LABELS[platform]} execution coverage"


def compare(
    crates: Dict[str, coverage_model.CrateCoverage], inventory: coverage_model.Inventory, baseline: coverage_model.Baseline
) -> Tuple[Dict[str, str], List[coverage_model.Finding]]:
    """Return each member's row status and every finding that fails the floor."""
    tolerance = Fraction(str(baseline.tolerance_pp))
    statuses: Dict[str, str] = {}
    findings: List[coverage_model.Finding] = []
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
                    findings.append(coverage_model.Finding(coverage_model.DROP, name, (
                        f"{name}: {measured} is {float(-delta):.2f} pp below its floor {_pct(entry.percent)} "
                        f"({entry.covered}/{entry.lines}); the tolerance is {baseline.tolerance_pp} pp. Add tests, "
                        "or rebaseline from this run's verified evidence in a reviewed change that states the cause.")))
                else:
                    statuses[name] = "ok"
            elif declared is not None:
                statuses[name] = "NOW MEASURED"
                findings.append(coverage_model.Finding(coverage_model.NOW_MEASURED, name, (
                    f"{name}: declared not measured ({declared}) but now reports {measured}. "
                    "Replace the declaration with a floor in a reviewed baseline change.")))
            else:
                statuses[name] = "NO BASELINE"
                findings.append(coverage_model.Finding(coverage_model.NO_BASELINE, name, (
                    f"{name}: measures {measured} but has no baseline entry. A measured crate fails "
                    "until a reviewed baseline change adds its floor.")))
        elif entry is not None:
            statuses[name] = "MISSING"
            findings.append(coverage_model.Finding(coverage_model.MISSING_MEASURED, name, (
                f"{name}: has a floor of {_pct(entry.percent)} ({entry.covered}/{entry.lines}) but reports "
                f"no eligible line ({coverage_reports.unmeasured_detail(crate)}). A previously measured crate must not leave "
                "the floor silently; restore it, or replace the entry with a declared reason in a reviewed change.")))
        elif declared is not None:
            statuses[name] = "declared"
        else:
            statuses[name] = "UNEXPLAINED"
            findings.append(coverage_model.Finding(coverage_model.UNEXPLAINED, name, (
                f"{name}: workspace member with no eligible line ({coverage_reports.unmeasured_detail(crate)}), no floor, "
                "and no not-measured declaration. Declare why it is not measured in a reviewed change.")))
    for name in sorted(set(baseline.crates) - set(inventory.members)):
        entry = baseline.crates[name]
        findings.append(coverage_model.Finding(coverage_model.MISSING_MEMBER, name, (
            f"{name}: has a floor of {_pct(entry.percent)} but is no longer a workspace member. "
            "Remove its entry in a reviewed change.")))
    for name in sorted(set(baseline.not_measured) - set(inventory.members)):
        findings.append(coverage_model.Finding(coverage_model.STALE_DECLARATION, name, (
            f"{name}: declared not measured but is not a workspace member. "
            "Remove the declaration in a reviewed change.")))
    return statuses, findings


def render_table(
    crates: Dict[str, coverage_model.CrateCoverage],
    inventory: coverage_model.Inventory,
    baseline: coverage_model.Baseline,
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
            note = declared if declared is not None else coverage_reports.unmeasured_detail(crate)
            rows.append([name, statuses[name], "-", "-", "not measured", "-", "-", note])
    right_aligned = {2, 3, 4, 5, 6}
    widths = [max(len(row[column]) for row in [headers] + rows) for column in range(len(headers) - 1)]

    def format_row(cells: List[str]) -> str:
        parts = [cell.rjust(widths[column]) if column in right_aligned else cell.ljust(widths[column])
                 for column, cell in enumerate(cells[:-1])]
        return "  ".join(parts + [cells[-1]]).rstrip()

    return [format_row(headers)] + [format_row(row) for row in rows]


def render_exclusions(exclusions: Dict[str, coverage_model.ExclusionTotals]) -> List[str]:
    """Render every exclusion category with its counts and reason."""
    lines = ["Excluded from the per-crate figures:"]
    for category, reason in coverage_model.EXCLUSION_REASONS.items():
        totals = exclusions[category]
        lines.append(f"  {category:<18} files={totals.files:<4} lines={totals.lines:<7} {reason}")
        if category in coverage_model.LISTED_CATEGORIES:
            lines.extend(f"      {path}" for path in totals.paths[:coverage_model.LISTED_PATH_LIMIT])
            if len(totals.paths) > coverage_model.LISTED_PATH_LIMIT:
                lines.append(f"      ... and {len(totals.paths) - coverage_model.LISTED_PATH_LIMIT} more")
    return lines


def _entry_document(entry: coverage_model.Entry) -> dict:
    """Return the JSON form of one recorded floor."""
    return {"lines": entry.lines, "covered": entry.covered, "percent": round(entry.covered * 100 / entry.lines, 2)}


def baseline_document(baseline: coverage_model.Baseline) -> dict:
    """Return the canonical JSON document for a baseline, with sorted crate keys."""
    document = {
        "schema": coverage_model.SCHEMA,
        "host": {"target": baseline.target, "runner": baseline.runner},
        "tolerance_pp": baseline.tolerance_pp,
        "reason": baseline.reason,
        "crates": {name: _entry_document(entry) for name, entry in sorted(baseline.crates.items())},
        "not_measured": {name: baseline.not_measured[name] for name in sorted(baseline.not_measured)},
    }
    # The macOS section follows the crate floors, so rewriting either leaves the other's lines unchanged.
    if baseline.macos is not None:
        document["macos"] = {
            "host": {"target": baseline.macos.target, "runner": baseline.macos.runner},
            "reason": baseline.macos.reason,
            "files": {path: _entry_document(entry) for path, entry in sorted(baseline.macos.files.items())},
        }
    return document


def render_document(document: dict) -> str:
    """Serialize a baseline document deterministically; a non-finite number raises instead of writing NaN."""
    return json.dumps(document, indent=2, ensure_ascii=True, allow_nan=False) + "\n"


def additions_proposal(baseline: coverage_model.Baseline, crates: Dict[str, coverage_model.CrateCoverage], findings: List[coverage_model.Finding]) -> dict:
    """Propose the current baseline plus entries for crates that lack one.

    Existing floors are kept verbatim, so the adopted diff shows only additions.
    The reasons are left empty: the baseline refuses them until a reviewer writes them.
    """
    entries = dict(baseline.crates)
    declared = dict(baseline.not_measured)
    for finding in findings:
        if finding.kind in (coverage_model.NO_BASELINE, coverage_model.NOW_MEASURED):
            crate = crates[finding.crate]
            entries[finding.crate] = coverage_model.Entry(crate.lines, crate.covered)
            declared.pop(finding.crate, None)
        elif finding.kind == coverage_model.UNEXPLAINED:
            declared[finding.crate] = ""
    return baseline_document(coverage_model.Baseline(baseline.target, baseline.runner, baseline.tolerance_pp, "", entries, declared,
                                      baseline.macos))


def rebaseline_proposal(baseline: coverage_model.Baseline, crates: Dict[str, coverage_model.CrateCoverage], target: str, runner: str) -> dict:
    """Propose a complete baseline for this host, keeping declared reasons that still apply."""
    entries = {name: coverage_model.Entry(crate.lines, crate.covered) for name, crate in crates.items() if crate.measured}
    declared = {name: baseline.not_measured.get(name, "") for name, crate in crates.items() if not crate.measured}
    return baseline_document(coverage_model.Baseline(target, runner, baseline.tolerance_pp, "", entries, declared, baseline.macos))


def provenance(target: str, runner: str, toolchain: Optional[str], tool: str, env: Mapping[str, str]) -> str:
    """Describe where the measured numbers came from, for the reviewer's stated reason."""
    parts = [f"{target} on {runner}", toolchain or "rustc version not given", tool]
    url, sha, artifact = run_url(env), env.get("GITHUB_SHA"), evidence_artifact(env)
    if url:
        parts.append(url)
    if sha:
        parts.append(f"commit {sha}")
    if artifact:
        parts.append(f"evidence artifact {artifact}")
    return ", ".join(parts)


def evidence_artifact(env: Mapping[str, str], prefix: str = coverage_model.EVIDENCE_ARTIFACT_PREFIX) -> Optional[str]:
    """Name the evidence artifact a coverage job uploads for this run attempt, or None outside CI."""
    run_id, attempt = env.get("GITHUB_RUN_ID"), env.get("GITHUB_RUN_ATTEMPT")
    if env.get("GITHUB_ACTIONS") != "true" or not run_id or not attempt:
        return None
    return f"{prefix}-{run_id}-{attempt}"


def run_url(env: Mapping[str, str]) -> Optional[str]:
    """Return the Actions URL of this run, when the environment names one."""
    server, repository, run_id = (env.get(key) for key in ("GITHUB_SERVER_URL", "GITHUB_REPOSITORY", "GITHUB_RUN_ID"))
    return f"{server}/{repository}/actions/runs/{run_id}" if server and repository and run_id else None


def drop_guidance(env: Mapping[str, str]) -> List[str]:
    """Point a DROP at this run's evidence and the reviewed procedure; a DROP never gets a proposal."""
    artifact = evidence_artifact(env) or "this run's evidence artifact"
    url = run_url(env)
    where = f" of {url}" if url else ""
    return [
        f"A DROP is never proposed. The evidence for this run is the artifact {artifact}{where}. If the drop "
        "is not a regression, retrieve and verify that artifact, then run --update-baseline --provenance with "
        f"a reason that states the cause, as {coverage_model.REBASELINE_PROCEDURE} describes. A toolchain or image change "
        "alone does not justify lowering a floor.",
        "",
    ]


def render_proposal(document: dict, measured_on: str) -> List[str]:
    """Render a proposed baseline between markers, with adoption instructions."""
    return [
        "Proposed baseline, a preview only. The floor never adopts it by itself: floor numbers change",
        "only through --update-baseline --provenance with this run's retained evidence, as",
        f"{coverage_model.REBASELINE_PROCEDURE} describes. A reviewer writes the reason and every",
        "empty \"not_measured\" reason; an empty reason fails the next run.",
        f"Measured on: {measured_on}",
        coverage_model.PROPOSAL_BEGIN,
        *render_document(document).rstrip("\n").splitlines(),
        coverage_model.PROPOSAL_END,
        "",
    ]


def run_check(args: argparse.Namespace, env: Mapping[str, str], out) -> int:
    """Print the per-crate report and evaluate the floor; return the exit status."""
    report = coverage_reports.load_report(args.report)
    inventory = coverage_reports.load_inventory(args.metadata)
    baseline = coverage_baselines.load_baseline(args.baseline)
    crates, exclusions = coverage_reports.aggregate(report, inventory, args.source_root)
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
    if enforced and any(finding.kind == coverage_model.DROP for finding in findings):
        lines += drop_guidance(env)
    measured_on = provenance(args.target, runner, args.toolchain, report.tool, env)
    measured = sum(1 for crate in crates.values() if crate.measured)
    declared = sum(1 for status in statuses.values() if status == "declared")
    if enforced and not findings:
        lines.append(f"coverage floor PASS: {measured} measured crates within {baseline.tolerance_pp} pp "
                     f"of their floors; {declared} declared not measured")
        status = 0
    elif enforced:
        if any(finding.kind in coverage_model.PROPOSABLE for finding in findings):
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
    existing: Optional[coverage_model.Baseline], crates: Dict[str, coverage_model.CrateCoverage], inventory: coverage_model.Inventory,
    target: str, runner: str, reason: str,
) -> coverage_model.Baseline:
    """Build a baseline holding every measured member and every still-applicable declaration."""
    tolerance = existing.tolerance_pp if existing else coverage_model.DEFAULT_TOLERANCE_PP
    previous = existing.not_measured if existing else {}
    entries: Dict[str, coverage_model.Entry] = {}
    declared: Dict[str, str] = {}
    undeclared = []
    for name in sorted(inventory.members):
        crate = crates[name]
        if crate.measured:
            entries[name] = coverage_model.Entry(crate.lines, crate.covered)
        elif previous.get(name, "").strip():
            declared[name] = previous[name]
        else:
            undeclared.append(f"{name} ({coverage_reports.unmeasured_detail(crate)})")
    # The tool cannot invent why a crate is unmeasured; a human states it in the baseline first.
    if undeclared:
        raise coverage_model.InputError("refusing to write a baseline: these members are not measured and not declared: "
                         + "; ".join(undeclared)
                         + ". Add each to \"not_measured\" with its reason, then rerun.")
    return coverage_model.Baseline(target, runner, tolerance, reason, entries, declared, existing.macos if existing else None)


def partial_update(
    existing: Optional[coverage_model.Baseline], crates: Dict[str, coverage_model.CrateCoverage], inventory: coverage_model.Inventory,
    target: str, runner: str, reason: str, names: List[str],
) -> coverage_model.Baseline:
    """Replace only the named crates' floors, keeping every other entry verbatim."""
    if existing is None:
        raise coverage_model.InputError("--crate updates entries of an existing baseline, and none was found")
    # Floors measured on two hosts are not comparable, so one file never mixes them.
    if (existing.target, existing.runner) != (target, runner):
        raise coverage_model.InputError(f"--crate cannot mix hosts: the baseline is for {existing.target} on {existing.runner}, "
                         f"this report is {target} on {runner}; update every crate for a new host instead")
    entries = dict(existing.crates)
    declared = dict(existing.not_measured)
    for name in names:
        if name not in inventory.members:
            raise coverage_model.InputError(f"--crate {name} is not a workspace member")
        crate = crates[name]
        if not crate.measured:
            raise coverage_model.InputError(f"--crate {name} has no eligible line in this report ({coverage_reports.unmeasured_detail(crate)}); "
                             "declare it in \"not_measured\" with a reason instead")
        entries[name] = coverage_model.Entry(crate.lines, crate.covered)
        declared.pop(name, None)
    return coverage_model.Baseline(existing.target, existing.runner, existing.tolerance_pp, reason, entries, declared, existing.macos)


def describe_update(old: Optional[coverage_model.Baseline], new: coverage_model.Baseline) -> List[str]:
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
