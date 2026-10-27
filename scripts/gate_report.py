"""Gate report."""

from __future__ import annotations
from dataclasses import dataclass, replace
import json
import os
from pathlib import Path
import stat
from typing import Iterable, Mapping, Sequence, TextIO

import gate_files
import gate_harness
import gate_model
import gate_process


@dataclass(frozen=True)
class GateReport:
    """Everything one gate run observed, from which the summary and exit status derive."""

    host: str
    results: tuple[StepResult, ...]
    before: GitSnapshot
    after: GitSnapshot
    changes: tuple[str, ...]
    log_dir: Path
    interrupted: bool = False
    # Links or a replaced log directory found at runner outputs during the run.
    output_problems: tuple[str, ...] = ()

    @property
    def exit_code(self) -> int:
        """Return 130 when interrupted, 1 when a step failed or the tree or an output changed, else 0."""
        if self.interrupted:
            return 130
        if self.changes or self.output_problems or any(not result.accepted for result in self.results):
            return 1
        return 0


def summary_lines(report: GateReport) -> list[str]:
    """Render the human summary that is printed and written to summary.txt."""
    counts = {status: 0 for status in (gate_model.PASS, gate_model.CLEANED_NOT_NATURAL, gate_model.FAIL, gate_model.TIMEOUT, gate_model.LAUNCH, gate_model.INTERRUPTED)}
    for result in report.results:
        counts[result.status] += 1
    lines = [
        f"[local-gate] summary host={report.host} steps={len(report.results)} "
        f"passed={counts[gate_model.PASS]} cleaned={counts[gate_model.CLEANED_NOT_NATURAL]} failed={counts[gate_model.FAIL]} timed_out={counts[gate_model.TIMEOUT]} "
        f"launch_errors={counts[gate_model.LAUNCH]} interrupted={counts[gate_model.INTERRUPTED]}"
    ]
    for result in report.results:
        line = (
            f"  {result.status:<11} {result.id:<24} {result.elapsed_s:8.1f}s "
            f"exit={gate_process._exit_text(result.exit_code):<4} log={result.log_path.name}"
        )
        if result.detail:
            line += f" ({result.detail})"
        lines.append(line)
        for phase in result.phases:
            lines.append(f"    {phase.name} {phase.status} exit={gate_process._exit_text(phase.exit_code)} "
                         f"policy={phase.policy.value} argv={json.dumps(phase.argv)} env={json.dumps(phase.env)} "
                         f"custody={json.dumps(phase.custody)}")
    if not report.before.available:
        lines.append(f"[local-gate] Git state unavailable: {report.before.error}")
    elif report.before.entries:
        lines.append(
            f"[local-gate] pre-existing changes, present before the run and not caused by it "
            f"({len(report.before.entries)}):"
        )
        for path, code, _fingerprint in report.before.entries[:gate_model.PRE_EXISTING_LIMIT]:
            lines.append(f"  {code} {gate_files._display_path(path)}")
        hidden = len(report.before.entries) - gate_model.PRE_EXISTING_LIMIT
        if hidden > 0:
            lines.append(f"  ... and {hidden} more (see summary.json)")
    else:
        lines.append("[local-gate] the tree was clean before the run")
    if report.changes:
        lines.append(f"[local-gate] the run changed the tree ({len(report.changes)}); nothing was reverted:")
        lines.extend(f"  {change}" for change in report.changes)
    elif report.before.available:
        lines.append("[local-gate] the run left tracked and untracked state unchanged")
    if report.output_problems:
        lines.append(f"[local-gate] runner outputs changed during the run ({len(report.output_problems)}):")
        lines.extend(f"  {problem}" for problem in report.output_problems)
    if report.interrupted:
        verdict = "INTERRUPTED"
    else:
        verdict = (gate_model.CLEANED_NOT_NATURAL if any(result.status == gate_model.CLEANED_NOT_NATURAL for result in report.results) else gate_model.PASS) if report.exit_code == 0 else gate_model.FAIL
    lines.append(f"[local-gate] verdict={verdict} exit={report.exit_code} logs={report.log_dir}")
    return lines


def summary_json(report: GateReport, steps: Sequence[gate_model.Step]) -> dict[str, object]:
    """Return the machine-readable summary written to summary.json."""
    by_id = {step.id: step for step in steps}
    return {
        "host": report.host,
        "exit_code": report.exit_code,
        "verdict": (gate_model.INTERRUPTED if report.interrupted else gate_model.FAIL if report.exit_code else
                    gate_model.CLEANED_NOT_NATURAL if any(result.status == gate_model.CLEANED_NOT_NATURAL for result in report.results) else gate_model.PASS),
        "interrupted": report.interrupted,
        "output_problems": list(report.output_problems),
        "steps": [
            {
                "id": result.id,
                "command": gate_harness.command_text(by_id[result.id]) if result.id in by_id else None,
                "status": result.status,
                "exit_code": result.exit_code,
                "elapsed_s": round(result.elapsed_s, 3),
                "timeout_s": by_id[result.id].timeout_s if result.id in by_id else None,
                "log": str(result.log_path),
                "detail": result.detail,
                "leftover_processes": result.leftover_processes,
                "windows_policy": result.windows_policy.value,
                "custody": result.custody,
                "phases": [{"name": phase.name, "argv": list(phase.argv), "env_overrides": dict(phase.env),
                            "policy": phase.policy.value, "status": phase.status, "exit_code": phase.exit_code,
                            "detail": phase.detail, "custody": phase.custody} for phase in result.phases],
            }
            for result in report.results
        ],
        "git": {
            "available": report.before.available,
            "error": report.before.error,
            "head_before": report.before.head,
            "head_after": report.after.head,
            "pre_existing": [
                {"path": gate_files._display_path(path), "status": code}
                for path, code, _fingerprint in report.before.entries
            ],
            "changes": list(report.changes),
        },
    }


def _say(out: TextIO, text: str) -> None:
    """Print one console line, escaping what the console cannot encode instead of raising.

    A log tail can hold any text, and a Windows console or pipe is often cp1252, so a
    diagnostic must never stop the run. The saved step logs keep their raw bytes.
    """
    encoding = getattr(out, "encoding", None) or "utf-8"
    try:
        text.encode(encoding)
    except UnicodeEncodeError:
        text = text.encode(encoding, errors="backslashreplace").decode(encoding, errors="replace")
    except LookupError:
        text = text.encode("ascii", errors="backslashreplace").decode("ascii")
    print(text, file=out, flush=True)


def _print_tail(out: TextIO, log_path: Path) -> None:
    """Print a failed step's log tail, never following a symlink a step left at the log path."""
    flags = (os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
             | getattr(os, "O_BINARY", 0))
    try:
        with os.fdopen(os.open(log_path, flags), "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                return
            data = handle.read()
    except OSError:
        return
    for line in data.decode("utf-8", errors="replace").splitlines()[-gate_model.TAIL_LINES:]:
        _say(out, f"    | {line}")
