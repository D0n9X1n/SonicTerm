"""Perf inventory."""

from __future__ import annotations
from pathlib import Path
import sys
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_execution
import perf_scenarios
import perf_sets
import perf_settings


# Each attempt directory's final classification, written once the set's checks have run: perf-flags binds a
# set's accepted attempts to it.
CLASSIFICATION_FILE = "classification.json"


def host_platform(platform_name: str = sys.platform) -> str:
    """A host's platform key for its sys.platform: `macos`, `windows`, or the name itself."""
    return {"darwin": "macos", "win32": "windows"}.get(platform_name, platform_name)


RUN_IDENTITY_FILE = "run-identity.json"
RUN_IDENTITY_SCHEMA = 1


def listed_capabilities(scenarios: Sequence[perf_scenarios.Scenario]) -> dict:
    """The capabilities the head's list declared; every scenario of one list carries the same ones."""
    first = scenarios[0] if scenarios else None
    return {"latency_split_schema": getattr(first, "latency_split_schema", None),
            "phase_kinds": getattr(first, "phase_kinds", None),
            "s10_attribution": getattr(first, "attribution_schema", None),
            "echo_timeline_schema": getattr(first, "echo_timeline_schema", None),
            "guard_correlation_schema": getattr(first, "guard_correlation_schema", None)}


def run_inventory(results: Iterable[perf_sets.SetResult]) -> list[dict]:
    """Each set's final inventory: per side its blocked or failed status and the attempt directories whose runs it
    accepted. A side that ended blocked or failed accepts none, whatever valid attempts it had."""
    inventory = []
    for result in results:
        entry = {"label": result.label, "dataset": result.set_name}
        for side_name in perf_settings.SIDES:
            side = getattr(result, side_name)
            status = side.blocked or side.failed or ""
            valid = [Path(evidence).name for name, evidence, kind, _why in result.attempts
                     if name == side_name and kind == "valid"]
            # run_set appends a side's outcome exactly when its attempt ends valid, so the two lists align in order.
            accepted = [] if status else [name for name, _outcome in zip(valid, side.outcomes)]
            entry[side_name] = {"status": status, "accepted": accepted}
        inventory.append(entry)
    return inventory


def run_identity_document(shas: Mapping[str, str], harness_hash: str, features: Mapping[str, Sequence[str]],
                          short: bool, profile: Mapping, counters: bool, environ: Mapping[str, str],
                          results: Iterable[perf_sets.SetResult] = (), capabilities: Mapping | None = None,
                          guard_api: bool | None = None) -> dict:
    """run-identity.json: the run, refs, harness, settings and flag-metric definitions perf-flags.py binds each
    artifact's accepted runs to, every set's final inventory of accepted attempts, and the capabilities the head's
    list declared, which both sides were validated under. `guard_api` is whether both sides were built with the
    guard-correlation take's cfg, so the artifact reader binds each phase's availability to the build."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_comparison_rows
    attempt = environ.get("GITHUB_RUN_ATTEMPT", "")
    return {"schema_version": RUN_IDENTITY_SCHEMA, "run_id": environ.get("GITHUB_RUN_ID", ""),
            "run_attempt": int(attempt) if attempt.isdigit() else 0, "platform": host_platform(),
            "base_sha": shas["base"], "head_sha": shas["head"], "harness_hash": harness_hash,
            "settings": {"short": bool(short), "counters": bool(counters),
                         "features": {side: sorted(features[side]) for side in perf_settings.SIDES}, "profile": dict(profile)},
            "flag_metrics_version": perf_comparison_rows.FLAG_METRICS_VERSION, "sets": run_inventory(results),
            # Every known capability is recorded, null when the head's list does not declare it, so perf-flags can
            # require exactly the known keys whatever the head declared.
            "capabilities": {name: (capabilities or {}).get(name) for name in perf_scenarios.HARNESS_CAPABILITIES},
            "guard_api": guard_api}


def render_table(rows: Iterable[Sequence[str]], header: str = perf_execution.TABLE_HEADER) -> str:
    """Render rows as a Markdown table under `header`; a `|` or newline in a cell cannot break it."""
    def cell(text: str) -> str:
        return str(text).replace("|", "\\|").replace("\n", " ")
    return header + "".join("| " + " | ".join(cell(text) for text in row) + " |\n" for row in rows)
