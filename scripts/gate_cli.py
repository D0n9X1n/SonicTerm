"""Gate cli."""

from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import platform
import sys
import tempfile
from typing import Iterable, Mapping, Sequence, TextIO

import gate_catalog
import gate_docs
import gate_execution
import gate_files
import gate_harness
import gate_model
import gate_process
import gate_report


def run_gate(steps: Sequence[gate_model.Step], root: Path, log_dir: Path, host: str, *,
             environ: Mapping[str, str] | None = None,
             console: TextIO | None = None) -> gate_report.GateReport:
    """Run every step in order, record Git state around the run, and write the summaries."""
    out = console if console is not None else sys.stdout
    env = dict(os.environ if environ is None else environ)
    outputs = gate_files.runner_outputs(log_dir, steps)
    problem = (gate_files.sigchld_problem() or gate_files.log_dir_problem(root, log_dir)
               or gate_files.runner_output_problem(root, outputs))
    if problem:
        raise ValueError(problem)
    log_dir.mkdir(parents=True, exist_ok=True)
    identity = gate_files._dir_identity(log_dir)
    gate_report._say(out, f"[local-gate] host={host} steps={len(steps)} root={root}")
    gate_report._say(out, f"[local-gate] logs={log_dir}")
    before = gate_files.snapshot_git(root, outputs)
    results: list[StepResult] = []
    problems: list[str] = []
    interrupted = replaced = False
    for index, step in enumerate(steps, 1):
        # Every output is checked without following links before and after the steps that could
        # plant one; a replaced log directory stops the run, since nothing there is the runner's.
        moved = gate_files._log_dir_replaced(log_dir, identity)
        if moved:
            problems.append(moved)
            replaced = True
            break
        log_path = gate_process._step_log_path(log_dir, index, step)
        alias = gate_files._output_alias(log_path)
        if alias:
            problems.append(gate_files._alias_problem(log_path, alias))
        gate_report._say(out, f"[local-gate] start {step.id} timeout={step.timeout_s}s: {gate_harness.command_text(step)}")
        result = gate_execution.run_step(step, index, root, log_dir, env)
        results.append(result)
        gate_report._say(out, f"[local-gate] finish {step.id} result={result.status} "
                  f"exit={gate_process._exit_text(result.exit_code)} elapsed={result.elapsed_s:.1f}s")
        interrupted = result.status == gate_model.INTERRUPTED
        # Checked before any read of the step's log: through a replaced directory, even a
        # no-follow open of the log path reads whatever that directory now holds.
        moved = gate_files._log_dir_replaced(log_dir, identity)
        if moved:
            problems.append(moved)
            replaced = True
            break
        alias = gate_files._output_alias(result.log_path)
        if alias:
            problems.append(f"{result.log_path} became {alias} during its step; its tail was not read")
        elif result.status != gate_model.PASS:
            gate_report._print_tail(out, result.log_path)
        if interrupted:
            break
    after = gate_files.snapshot_git(root, outputs)
    if not replaced:
        moved = gate_files._log_dir_replaced(log_dir, identity)
        if moved:
            problems.append(moved)
            replaced = True
    summaries = (log_dir / "summary.txt", log_dir / "summary.json")
    if not replaced:
        for path in summaries:
            alias = gate_files._output_alias(path)
            if alias:
                problems.append(gate_files._alias_problem(path, alias))
    report = gate_report.GateReport(host, tuple(results), before, after,
                        tuple(gate_files.describe_changes(before, after)), log_dir, interrupted, tuple(problems))
    lines = gate_report.summary_lines(report)
    if not replaced:
        gate_files._write_output(summaries[0], "\n".join(lines) + "\n")
        gate_files._write_output(summaries[1], json.dumps(gate_report.summary_json(report, steps), indent=2) + "\n")
    for line in lines:
        gate_report._say(out, line)
    return report


def detect_host(system: str | None = None) -> str | None:
    """Map platform.system() to a table host, or None for an unsupported host."""
    name = platform.system() if system is None else system
    if name == "Darwin":
        return "macos"
    if name == "Linux":
        return "linux"
    if name == "Windows" or name.upper().startswith(("CYGWIN", "MSYS", "MINGW")):
        return "windows"
    return None


def select_steps(host: str, *, release: bool = False, optional: bool = False,
                 ids: Sequence[str] = (), steps: Sequence[gate_model.Step] = gate_catalog.STEPS) -> list[gate_model.Step]:
    """Choose the host's steps in table order, by class flags or by explicit ids."""
    known = {step.id for step in steps}
    unknown = [name for name in ids if name not in known]
    if unknown:
        raise ValueError("unknown step id(s): " + ", ".join(unknown))
    if ids:
        chosen = [step for step in steps if step.id in set(ids)]
        foreign = [step.id for step in chosen if host not in step.hosts]
        if foreign:
            raise ValueError(f"step(s) not defined for {host}: " + ", ".join(foreign))
        return chosen
    classes = {"local"}
    if release:
        classes.add("release")
    if optional:
        classes.add("optional")
    return [step for step in steps if host in step.hosts and step.evidence in classes]


def format_listing(steps: Sequence[gate_model.Step], host: str) -> str:
    """Describe the selected steps with their deadlines, prerequisites, and CI jobs."""
    lines = [f"[local-gate] {len(steps)} step(s) for {gate_model.HOST_LABELS[host]}:"]
    for step in steps:
        lines.append(
            f"  {step.id} [{step.evidence}] timeout={step.timeout_s}s "
            f"needs={','.join(step.prerequisites)} ci={','.join(step.ci_jobs) or '-'}"
        )
        lines.append(f"      {gate_harness.command_text(step)}")
    return "\n".join(lines)


def main(argv: Sequence[str] | None = None) -> int:
    """Parse the command line, then list, render, or run the selected steps."""
    parser = argparse.ArgumentParser(description="Run SonicTerm's local verification gate.")
    parser.add_argument("--root", type=Path, default=gate_model._HERE.parent,
                        help="repository root (default: this script's repository)")
    parser.add_argument("--host", choices=gate_model.HOSTS,
                        help="show another host's steps; only valid with --list")
    parser.add_argument("--with-release", action="store_true",
                        help="also run the host's release-preparation steps")
    parser.add_argument("--with-optional", action="store_true",
                        help="also run the host's optional pre-push steps")
    parser.add_argument("--step", action="append", default=[], metavar="ID",
                        help="run only this step; repeat to run several, in table order")
    parser.add_argument("--log-dir", type=Path,
                        help="directory for per-step logs and summaries (default: a new temp dir)")
    parser.add_argument("--list", action="store_true", help="print the selected steps and exit")
    parser.add_argument("--render", choices=tuple(gate_docs._RENDER_TEXT),
                        help="print the documented gate block in a language and exit")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.render:
        # UTF-8 bytes, so the Chinese block prints on consoles with a legacy code page.
        sys.stdout.buffer.write((gate_docs.render_gate_block(args.render) + "\n").encode("utf-8"))
        sys.stdout.flush()
        return 0
    detected = detect_host()
    host = args.host or detected
    if host is None:
        parser.error(f"unsupported host {platform.system()!r}; use --host with --list")
    if args.host and args.host != detected and not args.list:
        parser.error("--host shows another host's table and requires --list")
    try:
        steps = select_steps(host, release=args.with_release, optional=args.with_optional,
                             ids=args.step)
    except ValueError as error:
        parser.error(str(error))
    if args.list:
        print(format_listing(steps, host), flush=True)
        return 0
    root = args.root.resolve()
    problem = gate_files.sigchld_problem()
    if problem is None and args.log_dir is not None:
        problem = gate_files.log_dir_problem(root, args.log_dir) or gate_files.runner_output_problem(
            root, gate_files.runner_outputs(args.log_dir, steps)
        )
    if problem:
        parser.error(problem)
    log_dir = (args.log_dir.resolve() if args.log_dir
               else Path(tempfile.mkdtemp(prefix="sonicterm-local-gate-")))
    try:
        return run_gate(steps, root, log_dir, host).exit_code
    except KeyboardInterrupt:
        print("[local-gate] interrupted", file=sys.stderr, flush=True)
        return 130
