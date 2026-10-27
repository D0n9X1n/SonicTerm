"""Perf build policy."""

from __future__ import annotations
import argparse
import os
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_focus
import perf_sets
import perf_settings
# local-gate.py's host names, by sys.platform.
GATE_HOSTS = {"darwin": "macos", "win32": "windows"}


def gate_hosts(platform_name: str) -> tuple[str, ...]:
    """The hosts a synthetic gate step names: the current one, since local-gate selects and labels by host."""
    return (GATE_HOSTS.get(platform_name, "linux"),)


def gate_problem(gate, os_name: str | None = None) -> str | None:
    """Explain why run_step cannot bound and reap children on this interpreter, or return None.

    On Windows the gate's job owns every process, so no process-group id must stay reserved.
    """
    problem = gate.sigchld_problem()
    if problem:
        return problem
    if (os_name or os.name) != "nt" and not gate.leader_watches():
        return ("this Python sees a child's exit only by reaping it (it has neither waitid nor kqueue), "
                "so run_step cannot keep a run's process-group id reserved")
    return None


def _resolve_sha(host_run: Callable, ref: str) -> str:
    """Resolve a ref to its commit SHA with read-only `git rev-parse`."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_report
    record = host_run(("git", "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"))
    sha = record.stdout.strip()
    if perf_focus._command_failure(record) or not perf_report._SHA.fullmatch(sha):
        raise ValueError(f"cannot resolve {ref!r} to a commit: {record.stderr.strip() or sha or 'no output'}")
    return sha


def _allocation_metric(key: tuple[str, str, str]) -> bool:
    """Whether a metric belongs in the allocation table; the counting allocator's timings do not."""
    return key[0].endswith("allocations per frame")


def release_profile_overrides(environ: Mapping[str, str]) -> str:
    """Name the CARGO_PROFILE_RELEASE_* overrides both builds inherited, or `none`.

    CI relaxes the release profile to fit a pull request's time budget; both refs build with the same
    overrides, so the comparison stays fair, but the binaries differ from the shipped profile.
    """
    overrides = sorted(f"{name}={value}" for name, value in environ.items()
                       if name.startswith("CARGO_PROFILE_RELEASE_"))
    return " ".join(overrides) if overrides else "none"


def comparison_command(args: argparse.Namespace) -> str:
    """Reconstruct the invocation, so the details block records how the table was produced."""
    words = ["python3", "scripts/perf-compare.py", "--base", args.base, "--head", args.head]
    for value in args.scenario or ["all"]:
        words += ["--scenario", value]
    words += ["--runs", str(args.runs or perf_settings.DEFAULT_RUNS)]
    words += [flag for flag, chosen in (("--short", args.short), ("--laps", args.laps), ("--alloc", args.alloc),
                                        ("--counters", args.counters), ("--require-base", args.require_base))
              if chosen]
    if args.counters_runs is not None:
        words += ["--counters-runs", str(args.counters_runs)]
    for value in args.laps_scenario or []:
        words += ["--laps-scenario", value]
    if args.laps_runs is not None:
        words += ["--laps-runs", str(args.laps_runs)]
    if args.keep:
        words.append("--keep")
    if args.prebuilt is not None:
        words += ["--prebuilt", str(args.prebuilt), "--prebuilt-run-id", args.prebuilt_run_id,
                  "--prebuilt-attempt", args.prebuilt_attempt,
                  "--prebuilt-manifest-sha256", args.prebuilt_manifest_sha256]
    return " ".join(words)


def comparison_renderer(results: Iterable[perf_sets.SetResult]) -> dict | None:
    """The adapter the comparison's valid runs drew through, for the host block; None when none reported one."""
    for result in results:
        for side in (result.base, result.head):
            for outcome in side.outcomes:
                if outcome.renderer is not None:
                    return outcome.renderer
    return None


# --- Build once, measure on many runners: the producer's manifest and the consumer's checks -------

PREBUILT_SCHEMA_VERSION = 1
MANIFEST_FILE = "manifest.json"
# Under a comparison's work directory: the consumer's copies, with no `assets` beside them.
PREBUILT_DIRECTORY = "prebuilt"
TIMING_FILE = "timing.json"
# The marks timing.json records, in the order they happen.
TIMING_MARKS = ("start", "listed", "measure_start", "measure_end", "report_written")
_HEX_SHA256 = re.compile(r"[0-9a-f]{64}")
