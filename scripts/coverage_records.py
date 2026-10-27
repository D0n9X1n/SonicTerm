"""Coverage records."""

from __future__ import annotations
import argparse
from dataclasses import dataclass, field
import json
from pathlib import Path
from typing import Dict, List, Mapping, Optional, Tuple

import coverage_comparison
import coverage_model
import coverage_reports


def _exit_status(value: object) -> Optional[int]:
    """Return N for the text `exit status N` that the writer produces, or None."""
    prefix = "exit status "
    digits = value[len(prefix):] if isinstance(value, str) and value.startswith(prefix) else ""
    return int(digits) if digits.isascii() and digits.isdigit() and str(int(digits)) == digits else None


def checks_problems(checks: object) -> List[str]:
    """Refuse `checks` unless it holds exactly both entries, each a status the gate script writes, in a pair a run
    can reach: the gate unfinished or failed with the floor not run, or the gate passed with the floor
    unfinished or finished."""
    if not isinstance(checks, dict):
        return ["the record's checks is not an object"]
    missing, extra = sorted(set(coverage_model.CHECK_PHASES) - set(checks)), sorted(set(checks) - set(coverage_model.CHECK_PHASES))
    if missing or extra:
        parts = ([f"lacks {', '.join(missing)}"] if missing else []) + (
            [f"has unknown entries {', '.join(extra)}"] if extra else [])
        return [f"the record's checks {' and '.join(parts)}; a complete measurement carries exactly subset-gate "
                "and floor"]
    gate, floor_status = checks["subset-gate"], checks["floor"]
    gate_exit, floor_exit = _exit_status(gate), _exit_status(floor_status)
    reachable = ((gate == "not finished" and floor_status == "not run")
                 or (gate_exit is not None and gate_exit != 0 and floor_status == "not run")
                 or (gate_exit == 0 and (floor_status == "not finished" or floor_exit is not None)))
    if reachable:
        return []
    return [f"the record's checks pair subset-gate {gate!r} with floor {floor_status!r}, which the gate script "
            "never writes"]


def build_record(root: str, phase: str, state: str, exit_status: int, env: Mapping[str, str],
                 pin: Mapping[str, object], target: Optional[str] = None, rustc_verbose: Optional[str] = None, llvm_cov: Optional[str] = None,
                 report: Optional[str] = None, metadata: Optional[str] = None) -> dict:
    """Describe one coverage run as far as it got; report digests appear only for a complete measurement.

    `phase` is the phase that is running, failed, or, for `floor` only, done. The gate script writes a
    `running` record before each phase, so a run killed inside a phase leaves a record that says so.
    The checkout identity always comes from `pin`, sampled once before the first record; only the
    `publish` record samples the checkout again, to name where it differs from the pin.
    """
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    phases = coverage_model.MEASUREMENT_PHASES + coverage_model.CHECK_PHASES
    if phase not in phases or state not in coverage_model.RUN_STATES:
        raise coverage_model.InputError(f"record-provenance needs a phase in {list(phases)} and a state in {list(coverage_model.RUN_STATES)}")
    if state == "done" and phase != coverage_model.CHECK_PHASES[-1]:
        raise coverage_model.InputError("only the floor phase can finish a run")
    if state == "failed" and exit_status == 0:
        raise coverage_model.InputError("a failed phase needs its non-zero exit status")
    if exit_status < 0:
        raise coverage_model.InputError("an exit status is never negative")
    position = phases.index(phase)
    drift = coverage_checkout.checkout_drift(pin, root) if phase == "publish" else None
    # The report and inventory exist only once every measurement phase has finished.
    complete = position >= len(coverage_model.MEASUREMENT_PHASES)
    checks = {}
    for name in coverage_model.CHECK_PHASES:
        index = phases.index(name)
        if index < position or (index == position and state == "done"):
            checks[name] = "exit status 0"
        elif index == position:
            checks[name] = f"exit status {exit_status}" if state == "failed" else "not finished"
        else:
            checks[name] = "not run"
    report_sha = inventory_sha = None
    tool = llvm_cov
    failed_phase = failure = None
    if complete:
        if not report or not metadata:
            raise coverage_model.InputError("a complete measurement needs --report and --metadata")
        report_sha, inventory_sha = coverage_checkout.file_sha256(report), coverage_checkout.file_sha256(metadata)
        # The report names the tool that produced it, which is the version a verifier compares.
        tool = coverage_reports.load_report(report).tool
    else:
        failed_phase = phase
        failure = f"exit status {exit_status}" if state == "failed" else "interrupted before the phase finished"
        if drift:
            named = "checkout drifted from its pin: " + "; ".join(drift)
            failure = f"{failure}; {named}" if state == "failed" else named
    return {
        "schema": coverage_model.PROVENANCE_SCHEMA,
        "measurement": "complete" if complete else "incomplete",
        "failed_phase": failed_phase,
        "failure": failure,
        "checks": checks,
        **_run_fields(env, pin, target, rustc_verbose, tool, coverage_comparison.evidence_artifact(env)),
        "report_sha256": report_sha,
        "inventory_sha256": inventory_sha,
        "checkout_drift": drift,
    }


def _run_fields(env: Mapping[str, str], pin: Mapping[str, object], target: Optional[str],
                rustc_verbose: Optional[str], tool: Optional[str], artifact: Optional[str]) -> dict:
    """The run, host, toolchain and checkout fields every provenance record carries."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    verbose = (rustc_verbose or "").strip()
    return {
        "target": target or None,
        "runner": coverage_comparison.runner_identity(env),
        "image_version": env.get("ImageVersion") or None,
        "rustc_version": verbose.splitlines()[0] if verbose else None,
        "rustc_verbose": verbose or None,
        "cargo_llvm_cov": (tool or "").strip() or None,
        "repository": env.get("GITHUB_REPOSITORY") or None,
        "workflow": coverage_checkout.workflow_path(env),
        "run_id": env.get("GITHUB_RUN_ID") or None,
        "run_attempt": env.get("GITHUB_RUN_ATTEMPT") or None,
        "job": env.get("GITHUB_JOB") or None,
        "event": env.get("GITHUB_EVENT_NAME") or None,
        "ref": env.get("GITHUB_REF") or None,
        "run_url": coverage_comparison.run_url(env),
        "artifact": artifact,
        "commit": pin["commit"],
        "tree": pin["tree"],
        "pull_request_head": env.get("COVERAGE_PULL_REQUEST_HEAD") or None,
        "worktree_changes": pin["worktree_changes"],
        "tree_entries": pin["tree_entries"],
    }


def build_macos_record(root: str, env: Mapping[str, str], pin: Mapping[str, object], target: str,
                       rustc_verbose: str, report: str) -> dict:
    """Describe one finished macOS-only measurement: its run, host, toolchain, checkout and report digest.

    The macOS floor has no subset gate or workspace inventory, so the record binds the report alone. The
    checkout is sampled again here, after the measurement, to name where it differs from the pin.
    """
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    return {
        "schema": coverage_model.PROVENANCE_SCHEMA,
        "measurement": "complete",
        "failed_phase": None,
        "failure": None,
        **_run_fields(env, pin, target, rustc_verbose, coverage_reports.load_report(report).tool,
                      coverage_comparison.evidence_artifact(env, coverage_model.MACOS_ARTIFACT_PREFIX)),
        "report_sha256": coverage_checkout.file_sha256(report),
        "checkout_drift": coverage_checkout.checkout_drift(pin, root),
    }


def build_record_parser() -> argparse.ArgumentParser:
    """Return the parser for `record-provenance`, which the gate script calls at each phase boundary."""
    parser = argparse.ArgumentParser(prog="coverage-floor.py record-provenance",
                                     description="Write the provenance record of one coverage run.")
    parser.add_argument("--output", required=True, help="the record to write; it is replaced atomically")
    parser.add_argument("--root", required=True, help="the checkout the run measures")
    parser.add_argument("--pin", required=True, help="the checkout pin that pin-checkout wrote before any phase")
    parser.add_argument("--phase", required=True, choices=coverage_model.MEASUREMENT_PHASES + coverage_model.CHECK_PHASES)
    parser.add_argument("--state", required=True, choices=coverage_model.RUN_STATES)
    parser.add_argument("--exit-status", type=int, default=0, help="the exit status of a failed phase")
    parser.add_argument("--target", help="host target triple from rustc -vV")
    parser.add_argument("--rustc-verbose", help="the full rustc -vV output")
    parser.add_argument("--llvm-cov", help="cargo llvm-cov --version output, kept for an incomplete run")
    parser.add_argument("--report", help="the JSON report; required once the measurement is complete")
    parser.add_argument("--metadata", help="the workspace inventory; required once the measurement is complete")
    return parser


def run_record(argv: List[str], env: Mapping[str, str]) -> int:
    """Write one provenance record; return DRIFT_EXIT_STATUS when the publish check found drift, else 0."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    args = build_record_parser().parse_args(argv)
    record = build_record(args.root, args.phase, args.state, args.exit_status, env, coverage_checkout.load_pin(args.pin),
                          args.target, args.rustc_verbose, args.llvm_cov, args.report, args.metadata)
    coverage_checkout._write_atomic(Path(args.output), json.dumps(record, indent=2, sort_keys=True, ensure_ascii=True) + "\n")
    return coverage_model.DRIFT_EXIT_STATUS if record["checkout_drift"] else 0


def run_record_macos(argv: List[str], env: Mapping[str, str]) -> int:
    """Write the macOS-only measurement's record; return DRIFT_EXIT_STATUS when the checkout drifted, else 0."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    parser = argparse.ArgumentParser(prog="coverage-floor.py record-macos-provenance",
                                     description="Write the provenance record of one macOS-only coverage run.")
    parser.add_argument("--output", required=True, help="the record to write; it is replaced atomically")
    parser.add_argument("--root", required=True, help="the checkout the run measures")
    parser.add_argument("--pin", required=True, help="the checkout pin that pin-checkout wrote before measuring")
    parser.add_argument("--target", required=True, help="host target triple from rustc -vV")
    parser.add_argument("--rustc-verbose", required=True, help="the full rustc -vV output")
    parser.add_argument("--report", required=True, help="the JSON report the measurement wrote")
    args = parser.parse_args(argv)
    record = build_macos_record(args.root, env, coverage_checkout.load_pin(args.pin), args.target, args.rustc_verbose, args.report)
    coverage_checkout._write_atomic(Path(args.output), json.dumps(record, indent=2, sort_keys=True, ensure_ascii=True) + "\n")
    return coverage_model.DRIFT_EXIT_STATUS if record["checkout_drift"] else 0


def run_pin(argv: List[str]) -> int:
    """Pin the checkout one coverage run measures: its commit, tree, path map, and uncommitted changes."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    parser = argparse.ArgumentParser(prog="coverage-floor.py pin-checkout",
                                     description="Pin the checkout one coverage run measures.")
    parser.add_argument("--root", required=True, help="the checkout the run measures")
    parser.add_argument("--output", required=True, help="the pin to write; it is replaced atomically")
    args = parser.parse_args(argv)
    pin = {"schema": coverage_model.PIN_SCHEMA, **coverage_checkout.checkout_state(args.root)}
    coverage_checkout._write_atomic(Path(args.output), json.dumps(pin, indent=2, sort_keys=True, ensure_ascii=True) + "\n")
    return 0


@dataclass(frozen=True)
class Provenance:
    """A verified record: the run, host, and toolchain behind one report."""

    target: str
    runner: str
    image_version: str
    rustc_version: str
    cargo_llvm_cov: str
    repository: str
    run_id: str
    run_attempt: str
    job: str
    commit: str
    artifact: str
    run_url: Optional[str]
    pull_request_head: Optional[str]

    def sentence(self) -> str:
        """Describe the run, host, and toolchain for the reason an update writes."""
        run = self.run_url or f"run {self.run_id} of {self.repository}"
        text = (f"Measured by {run} attempt {self.run_attempt}, job {self.job}, evidence artifact {self.artifact}, "
                f"on {self.target} on {self.runner} (image {self.image_version}) with {self.rustc_version} and "
                f"{self.cargo_llvm_cov}, at commit {self.commit}")
        if self.pull_request_head:
            text += f", the test merge of pull-request head {self.pull_request_head}"
        return text + "."
