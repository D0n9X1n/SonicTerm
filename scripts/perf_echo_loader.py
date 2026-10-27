"""Perf echo loader."""

from __future__ import annotations
import argparse
import importlib.util
import json
from pathlib import Path
import platform
import sys

import perf_attribution_schema
import perf_echo_analysis
import perf_inventory
import perf_latency_schema
import perf_settings
import perf_timeline_schema


def _perf_flags_module():
    """perf-flags.py, loaded on first use: its load_evidence is the one validated path from a run's downloaded
    artifacts to its accepted results, and the attribution reads evidence only through it."""
    if "perf_flags_for_echo" not in sys.modules:
        spec = importlib.util.spec_from_file_location("perf_flags_for_echo", Path(__file__).resolve().with_name(
            "perf-flags.py"))
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
    return sys.modules["perf_flags_for_echo"]


def load_echo_execution(path: Path, label: str, platform: str = "macos") -> tuple[perf_echo_analysis.EchoExecution | None, list]:
    """One workflow execution's downloaded perf-comparison artifacts, read through perf-flags.py's load_evidence:
    run identity, settings and capabilities validated, the final set inventory reconciled with the attempts on
    disk, a blocked or failed side accepting nothing, and each accepted attempt's outcome and result validated
    under the head's declared capabilities. On top of that: every attempt's classification kind is one perf-compare
    writes, and each accepted result's attributed and total equal its samples' recomputed counts. Any failure
    refuses the execution with its reason."""
    flags = _perf_flags_module()
    try:
        evidence = flags.load_evidence(path)
    except flags.NotComparable as error:
        return None, [f"{label}: {error}"]
    found = evidence.sets.get((platform, perf_echo_analysis.ECHO_LABEL, perf_echo_analysis.ECHO_DATASET))
    if found is None:
        return None, [f"{label}: {path} has no {platform} {perf_echo_analysis.ECHO_LABEL} {perf_echo_analysis.ECHO_DATASET} set in its inventory"]
    problems = []
    skipped = []
    for name, files in sorted(found.evidence.items()):
        classification = json.loads(files[perf_inventory.CLASSIFICATION_FILE])
        kind = classification["kind"]
        if kind not in perf_echo_analysis.ECHO_CLASSIFICATION_KINDS:
            problems.append(f"{label} {name}: classification kind {kind!r} is not one perf-compare writes")
        elif kind != "valid":
            skipped.append(f"{name} ({kind})")
        elif not any(name in found.entry[side]["accepted"] for side in ("base", "head")):
            # When: a valid attempt is not accepted, its side ended blocked or failed and discarded it.
            skipped.append(f"{name} (valid, discarded by a {found.entry[_echo_attempt_side(name)]['status']!r} side)")
    sides: dict = {"base": [], "head": []}
    for side in ("base", "head"):
        for name, result in found.results[side]:
            latency = result.get("latency")
            samples = latency.get("samples") if isinstance(latency, dict) else None
            if not isinstance(samples, list):
                problems.append(f"{label} {name}: the accepted result has no latency samples")
                continue
            # When: a harness declares neither the split nor the timeline schema, the shared validator accepts any
            # number, NaN included, so every sample is checked here before the exact arithmetic reads it.
            malformed = [f"{label} {name} sample {position}: {problem}" for position, sample in enumerate(samples, 1)
                         if (problem := _echo_sample_problem(sample))]
            if malformed:
                problems += malformed
                continue
            credited = sum(1 for sample in samples if sample.get("latency_ms") is not None)
            if latency.get("total") != len(samples) or latency.get("attributed") != credited:
                problems.append(f"{label} {name}: latency attributed {latency.get('attributed')!r} and total "
                                f"{latency.get('total')!r} are not the samples' {credited} and {len(samples)}")
                continue
            sides[side].append(perf_echo_analysis.EchoRun(samples, latency["attributed"], latency["total"]))
    if problems:
        return None, problems
    identity = {key: found.identity[key] for key in perf_echo_analysis.ECHO_IDENTITY_KEYS}
    statuses = {side: found.entry[side]["status"] for side in ("base", "head")}
    return perf_echo_analysis.EchoExecution(label, sides["base"], sides["head"], skipped, identity, statuses), []


def _echo_sample_problem(sample: object) -> str | None:
    """Why one latency sample cannot be read by the attribution, or None: it must be an object whose latency_ms
    is null (not credited) or a finite number at least 0, an integer at most U64_MAX. A bare number, the oldest
    shape, carries no fields the attribution reads; Fraction cannot hold NaN or infinity."""
    if not isinstance(sample, dict):
        return f"{type(sample).__name__} is not a sample object"
    if "latency_ms" not in sample:
        return "has no latency_ms"
    value = sample["latency_ms"]
    if perf_latency_schema._is_int(value) and value > perf_attribution_schema.U64_MAX:
        # When: an integer too large for a float would overflow in isfinite, so it is refused by bound first,
        # and never printed whole.
        return "latency_ms is an integer above 2**64 - 1"
    if value is not None and not perf_timeline_schema._bounded_number(value):
        return f"latency_ms {value!r} is neither null nor a finite number >= 0"
    return None


def _echo_attempt_side(name: str) -> str:
    """The side an attempt directory's name ends with."""
    return name.rsplit("-", 1)[1]


def echo_attribution_main(args: argparse.Namespace) -> int:
    """`--echo-attribution`: read the replay's and the controls' artifacts through the validated evidence path,
    refuse malformed evidence and executions that are not distinct and paired, and print the §4 verdict. Exit 0
    means the evidence was read, not that a mechanism is confirmed."""
    platform = args.echo_platform or "macos"
    problems: list = []
    replay = []
    for index, path in enumerate(args.echo_attribution, 1):
        execution, found = load_echo_execution(path, f"replay {index}", platform)
        problems += found
        if execution is not None:
            replay.append(execution)
    controls: dict = {}
    for name, path in args.echo_control or []:
        execution, found = load_echo_execution(Path(path), f"{name} {len(controls.get(name, [])) + 1}", platform)
        problems += found
        if execution is not None:
            controls.setdefault(name, []).append(execution)
    if not problems:
        problems = perf_echo_analysis.echo_identity_problems(replay, controls)
    if problems:
        print("[perf-compare] echo attribution refused: the evidence is malformed or not paired:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return perf_settings.EXIT_FAIL
    print(perf_echo_analysis.format_echo_attribution(perf_echo_analysis.echo_attribution(replay, controls)), end="")
    return perf_settings.EXIT_PASS
