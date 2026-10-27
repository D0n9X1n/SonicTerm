"""Perf fallback rows."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import math
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_font_diagnostics
import perf_metrics
import perf_statistics


# The only verdicts on whether fallback_receive waits account for the slowest dispatches.
FALLBACK_VERDICTS = ("supported", "inconclusive")
# One millisecond, added to a stamp's resolution as the matching tolerance.
MATCH_SLACK_S = 0.001


@dataclass(frozen=True)
class FallbackRunVerdict:
    """One laps run's verdict, its coverage, why it is inconclusive, and its waits inside and outside the
    examined slow dispatches."""

    verdict: str
    coverage: str
    reasons: tuple[str, ...]
    inside_ms: list[float]
    outside_ms: list[float]
    unparsed: int
    unmatched_enter: int
    unmatched_return: int


def fallback_run_verdict(result: dict | None, log: perf_font_diagnostics.FallbackLog | None) -> FallbackRunVerdict:
    """Whether one laps run's fallback_receive waits account for its slowest dispatches.

    Per phase, the examined dispatches are the recorded slow dispatches at or above the phase's p95 of
    dispatch_ms. A wait `[t - elapsed, t]` matches one when it lies inside `[start - ε, end + ε]` and inside
    the same phase, ε being its stamp's resolution plus 1 ms. The run is supported when one examined dispatch's
    matched waits sum to at least half its duration, and inconclusive otherwise. Coverage is complete when no
    phase has more dispatches at or above p95 than it recorded.
    """
    phases = [phase for phase in (result or {}).get("phases") or [] if isinstance(phase, dict)]
    if log is None:
        return FallbackRunVerdict("inconclusive", "unavailable", ("no log",), [], [], 0, 0, 0)
    inside = [False] * len(log.waits)
    coverage = []
    supported = False
    for phase in phases:
        durations = phase.get("dispatch_ms") or []
        slow = phase.get("slow_dispatches")
        if not durations:
            continue
        if not isinstance(slow, list):
            # When: the harness recorded no slow dispatches, nothing in this phase can be examined.
            coverage.append("unavailable")
            continue
        threshold = perf_statistics.percentile_95(durations)
        coverage.append("complete" if sum(duration_ms >= threshold for duration_ms in durations) <= len(slow) else "incomplete")
        span = (phase.get("start_unix_s", -math.inf), phase.get("end_unix_s", math.inf))
        for dispatch in (item for item in slow if item["ms"] >= threshold):
            matched = 0.0
            for index, wait in enumerate(log.waits):
                slack = wait.resolution_s + MATCH_SLACK_S
                begin, end = wait.end_unix_s - wait.elapsed_ms / 1000, wait.end_unix_s
                in_phase = span[0] - slack <= begin and end <= span[1] + slack
                if in_phase and dispatch["start_unix_s"] - slack <= begin and end <= dispatch["end_unix_s"] + slack:
                    inside[index] = True
                    matched += wait.elapsed_ms
            # A dispatch counts only when real waiting was matched: a zero-length dispatch with no
            # wait would otherwise satisfy 0 >= 0 and claim support from an empty log.
            supported = supported or (matched > 0 and matched >= dispatch["ms"] / 2)
    overall = ("unavailable" if "unavailable" in coverage or not coverage else
               "incomplete" if "incomplete" in coverage else "complete")
    reasons = []
    if not supported:
        reasons += ["no waits"] if not log.waits else ["no examined slow dispatch is half waiting"]
        if overall != "complete":
            reasons.append(f"coverage {overall}")
        if log.unparsed:
            reasons.append(f"{log.unparsed} unparsed")
    return FallbackRunVerdict(
        "supported" if supported else "inconclusive", overall, tuple(reasons),
        [wait.elapsed_ms for wait, hit in zip(log.waits, inside) if hit],
        [wait.elapsed_ms for wait, hit in zip(log.waits, inside) if not hit],
        log.unparsed, log.unmatched_enter, log.unmatched_return)


def _side_run_verdicts(side: perf_metrics.SideRuns) -> list[FallbackRunVerdict]:
    return [fallback_run_verdict(outcome.result, outcome.fallback_log) for outcome in side.outcomes]


def fallback_side_verdict(side: perf_metrics.SideRuns) -> str:
    """A side is supported when one of its laps runs is; a side with no valid run is inconclusive."""
    verdicts = [] if side.blocked or side.failed else _side_run_verdicts(side)
    return "supported" if any(verdict.verdict == "supported" for verdict in verdicts) else "inconclusive"


def _waits_cell(values: Sequence[float]) -> str:
    peak = max(values, default=0.0)
    return f"{len(values)} waits, {sum(values):.1f} ms, max {peak:.1f} ms"


def fallback_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[list[str]]:
    """The laps table's fallback_receive rows: waits inside and outside the examined slow dispatches, pooled
    over a side's runs, then each side's verdict with its coverage, unparsed and unmatched counts."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    sides = (base, head)
    per_side = [None if side.blocked or side.failed else _side_run_verdicts(side) for side in sides]
    rows = []
    for name, attribute in (("inside", "inside_ms"), ("outside", "outside_ms")):
        cells = [perf_metrics._missing_cell(side) if verdicts is None
                 else _waits_cell([wait_ms for verdict in verdicts for wait_ms in getattr(verdict, attribute)])
                 for side, verdicts in zip(sides, per_side)]
        rows.append([label, f"fallback_receive waits {name} slow dispatches", *cells, ""])
    cells = []
    for side, verdicts in zip(sides, per_side):
        if verdicts is None:
            cells.append(perf_metrics._missing_cell(side))
            continue
        coverages = [verdict.coverage for verdict in verdicts]
        coverage = ("unavailable" if "unavailable" in coverages or not coverages else
                    "incomplete" if "incomplete" in coverages else "complete")
        counts = [sum(getattr(verdict, name) for verdict in verdicts)
                  for name in ("unparsed", "unmatched_enter", "unmatched_return")]
        reasons = sorted({reason for verdict in verdicts for reason in verdict.reasons})
        cells.append(f"{fallback_side_verdict(side)} (coverage {coverage}, unparsed {counts[0]}, "
                     f"unmatched_enter {counts[1]}, unmatched_return {counts[2]})"
                     + (f": {'; '.join(reasons)}" if fallback_side_verdict(side) == "inconclusive" and reasons else ""))
    rows.append([label, "fallback_receive verdict", *cells, ""])
    return rows


COUNTERS_HEADER = ("| Scenario | Phase | Counter (unit) | Baseline | PR | Change |\n"
                   "| --- | --- | --- | --- | --- | --- |\n")
