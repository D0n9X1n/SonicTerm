"""Perf comparison rows."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_latency_schema
import perf_metrics
import perf_outcomes
import perf_statistics


def blocked_set_results(label: str, reason: str, set_names: Sequence[str]) -> list:
    """Results for a scenario that runs no set: each set's two sides are blocked for `reason`."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    import perf_sets
    return [perf_sets.SetResult(label, set_name, perf_metrics.SideRuns(blocked=reason), perf_metrics.SideRuns(blocked=reason)) for set_name in set_names]


def _transition_phase(outcome: perf_outcomes.RunOutcome, name: str) -> dict | None:
    """The run's transition phase `name`, or None when the run has none."""
    for phase in (outcome.result or {}).get("phases") or []:
        if isinstance(phase, dict) and phase.get("name") == name and phase.get("kind") == "transition":
            return phase
    return None


def _completion_cell(side: perf_metrics.SideRuns, name: str) -> tuple[str, float | None]:
    """A side's completion cell for transition `name`: the median and range of the runs that completed, how many
    contributed, `single observation` when exactly one did, then every accepted run's value or missing reason
    keyed by its run id; and the median for the change column."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    import perf_result_schema
    if side.blocked or side.failed or not side.outcomes:
        return perf_metrics._missing_cell(side), None
    entries, values = [], []
    for outcome in side.outcomes:
        run_id = Path(outcome.evidence).name
        phase = _transition_phase(outcome, name)
        if phase is not None and perf_result_schema._finite_non_negative(phase.get("completion_ms")):
            values.append(phase["completion_ms"])
            entries.append(f"{run_id} {phase['completion_ms']:.2f}")
        else:
            reason = (phase or {}).get("completion_missing") or "not recorded"
            entries.append(f"{run_id} n/a: {reason}")
    summary = perf_statistics.run_summary(values)
    text = "n/a" if summary is None else f"{summary.median:.2f} ({summary.minimum:.2f}–{summary.maximum:.2f})"
    text += f", {len(values)}/{len(side.outcomes)} runs" + (", single observation" if len(values) == 1 else "")
    return f"{text}; {', '.join(entries)}", None if summary is None else summary.median


def transition_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[list[str]]:
    """One completion row per transition phase either side ran, listing every accepted run."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    names: list[str] = []
    for side in (base, head):
        for outcome in side.outcomes:
            for phase in (outcome.result or {}).get("phases") or []:
                if isinstance(phase, dict) and phase.get("kind") == "transition" and phase.get("name") not in names:
                    names.append(phase.get("name"))
    rows = []
    for name in names:
        (base_cell, base_median), (head_cell, head_median) = (_completion_cell(base, name),
                                                              _completion_cell(head, name))
        rows.append([label, f"{name} completion (ms)", base_cell, head_cell, perf_metrics.percent_change(base_median, head_median)])
    return rows


# A print phase's optional output diagnostics: ms from GO to its roles' first output, and from that output to the
# first frame presented after it. Either may be absent on either ref; the table shows how many runs recorded each,
# and they are never run metrics, so no flag or regression list reads them.
OUTPUT_DIAGNOSTICS = (("go_to_first_output_ms", "GO to first output"),
                      ("first_output_to_present_ms", "first output to present"))


def _diagnostic_cell(side: perf_metrics.SideRuns, name: str, key: str) -> tuple[str, float | None]:
    """A side's cell for diagnostic `key` of transition `name`: the median and range of the accepted runs that
    recorded it and how many did, and the median for the change column."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    import perf_result_schema
    if side.blocked or side.failed or not side.outcomes:
        return perf_metrics._missing_cell(side), None
    values = [phase[key] for outcome in side.outcomes
              if (phase := _transition_phase(outcome, name)) is not None and perf_result_schema._finite_non_negative(phase.get(key))]
    summary = perf_statistics.run_summary(values)
    text = "n/a" if summary is None else f"{summary.median:.2f} ({summary.minimum:.2f}–{summary.maximum:.2f})"
    return f"{text}, recorded in {len(values)} of {len(side.outcomes)} runs", None if summary is None else summary.median


def diagnostic_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[list[str]]:
    """One row per output diagnostic any accepted run of either side recorded, with each side's run count."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    rows = []
    for key, title in OUTPUT_DIAGNOSTICS:
        names: list[str] = []
        for side in (base, head):
            for outcome in side.outcomes:
                for phase in (outcome.result or {}).get("phases") or []:
                    if isinstance(phase, dict) and phase.get("kind") == "transition" and key in phase \
                            and phase.get("name") not in names:
                        names.append(phase.get("name"))
        for name in names:
            (base_cell, base_median), (head_cell, head_median) = (_diagnostic_cell(base, name, key),
                                                                  _diagnostic_cell(head, name, key))
            rows.append([label, f"{name} {title} (ms)", base_cell, head_cell, perf_metrics.percent_change(base_median, head_median)])
    return rows


# --- Candidate flags ------------------------------------------------------------------------

# Bumped whenever a flag metric, statistic or rule changes; perf-flags.py refuses runs whose versions differ.
FLAG_METRICS_VERSION = 1
# The direction in which each flag metric worsens. A metric absent here is never checked.
FLAG_DIRECTIONS = {"latency": "higher", "completion": "higher", "present interval": "higher",
                   "fps": "lower", "throughput": "lower"}
# Sample metrics pool their samples; scalar metrics hold one value per run.
FLAG_SAMPLE_METRICS = ("latency", "present interval")


@dataclass(frozen=True)
class FlagCheck:
    """One candidate-flag check: the base's per-run range of a statistic, the head's value, and whether the head
    lies beyond the range in the worsening direction. Samples count attributed keypresses for latency, intervals
    for interval rows, and runs for scalar metrics."""

    label: str
    dataset: str
    phase: str
    metric: str
    statistic: str
    direction: str
    base_low: float
    base_high: float
    head_value: float
    base_samples: int
    head_samples: int
    flagged: bool


def flag_values(result: Mapping | None) -> dict[tuple[str, str], object]:
    """One accepted run's flag inputs keyed (phase, metric): sample lists or one scalar. A harness without phase
    kinds contributes nothing, and a hold contributes nothing."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    phases = (result or {}).get("phases")
    if not isinstance(phases, list) or not phases \
            or not all(isinstance(phase, dict) and phase.get("kind") in perf_result_schema.PHASE_KINDS for phase in phases):
        return {}
    values: dict[tuple[str, str], object] = {}
    for phase in phases:
        name = phase.get("name")
        if phase["kind"] == "sustained":
            if perf_latency_schema._numbers(phase.get("present_interval_ms")) and phase["present_interval_ms"]:
                values[(name, "present interval")] = list(phase["present_interval_ms"])
            start, end = phase.get("start_unix_s"), phase.get("end_unix_s")
            if perf_latency_schema._is_number(start) and perf_latency_schema._is_number(end) and end > start and perf_latency_schema._is_int(phase.get("presented_frames")):
                values[(name, "fps")] = phase["presented_frames"] / (end - start)
        elif phase["kind"] == "transition" and perf_result_schema._finite_non_negative(phase.get("completion_ms")):
            values[(name, "completion")] = phase["completion_ms"]
    attributed = perf_latency_schema.latency_values(result.get("latency"))
    if attributed:
        values[("", "latency")] = attributed
    throughput = result.get("throughput")
    if isinstance(throughput, dict) and perf_latency_schema._is_number(throughput.get("seconds")) and throughput["seconds"] > 0 \
            and perf_latency_schema._is_int(throughput.get("bytes")):
        values[("", "throughput")] = throughput["bytes"] / throughput["seconds"] / 1_000_000
    return values


def unclassified_result(result) -> bool:
    """Whether an accepted run of either side comes from a harness without phase kinds: its rows then go to the
    report's separate unclassified block, and it contributes no candidate flag."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    for side in (result.base, result.head):
        for outcome in side.outcomes:
            phases = (outcome.result or {}).get("phases")
            if not isinstance(phases, list) or not all(isinstance(phase, dict) and phase.get("kind") in perf_result_schema.PHASE_KINDS
                                                       for phase in phases):
                return True
    return False


def flag_checks(results: Iterable) -> list[FlagCheck]:
    """Every candidate-flag check of the accepted runs in `results`. A sample metric's pooled head statistic is
    read against the base's per-run range of the same statistic; a scalar metric's head median against the base's
    per-run range. A head beyond the range in the worsening direction is flagged; an improvement never is."""
    checks = []
    for result in results:
        if unclassified_result(result):
            # When: any run of the set is unkinded, the set is reported only as unclassified, never by kind.
            continue
        sides = [[flag_values(outcome.result) for outcome in side.outcomes] if not side.blocked else []
                 for side in (result.base, result.head)]
        keys: list[tuple[str, str]] = []
        for per_run in sides[0] + sides[1]:
            keys.extend(key for key in per_run if key not in keys)
        for key in keys:
            direction = FLAG_DIRECTIONS.get(key[1])
            base_values, head_values = ([per_run[key] for per_run in side if key in per_run] for side in sides)
            if direction is None or not base_values or not head_values:
                # When: the metric has no direction or a side lacks it, there is no comparison to make.
                continue
            if key[1] in FLAG_SAMPLE_METRICS:
                pooled = [value for samples in head_values for value in samples]
                compared = [(statistic, [measure(samples) for samples in base_values], measure(pooled),
                             sum(map(len, base_values)), len(pooled))
                            for statistic, measure in (("median", perf_statistics.median), ("p95", perf_statistics.percentile_95))]
            else:
                compared = [("median", base_values, perf_statistics.median(head_values), len(base_values), len(head_values))]
            for statistic, base_stats, head_value, base_count, head_count in compared:
                low, high = min(base_stats), max(base_stats)
                flagged = head_value > high if direction == "higher" else head_value < low
                checks.append(FlagCheck(result.label, result.set_name, key[0], key[1], statistic, direction, low,
                                        high, head_value, base_count, head_count, flagged))
    return checks


COUNTERS_ONLY_NOTE = ("These variants run only in the counters set, so their phases are reported by kind from those "
                      "runs, with the gate forced on; read them against each other, never against a timed row.")
UNCLASSIFIED_NOTE = ("These scenarios ran a harness that records no phase kinds, so their phases cannot be read as "
                     "sustained, transition or hold; their rows are kept here as recorded and never enter the "
                     "candidate flags.")
FLAG_HEADER = ("| Scenario | Dataset | Phase | Metric | Statistic | Baseline range | PR | Direction |\n"
               "| --- | --- | --- | --- | --- | --- | --- | --- |\n")
FLAG_NOTE = ("A check reads the PR against the baseline's per-run range of the same statistic, over accepted runs "
             "only; a value beyond it in the worsening direction is a candidate, never a verdict. Holds are never "
             "checked.")


def flag_rows(checks: Iterable[FlagCheck]) -> list[list[str]]:
    """The Candidate flags table: one row per flagged check."""
    return [[check.label, check.dataset, check.phase, check.metric, check.statistic,
             f"{check.base_low:.2f}–{check.base_high:.2f}", f"{check.head_value:.2f}", f"{check.direction} is worse"]
            for check in checks if check.flagged]


def comparison_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns,
                    include: Callable[[tuple[str, str, str]], bool] | None = None) -> list[list[str]]:
    """Rows of the PR table for one scenario, with the latency attribution row where latency was measured."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_atlas_rows
    import perf_execution
    import perf_metrics
    rows = perf_metrics._rows(label, base, head, perf_metrics.run_metrics, include)
    if include is None:
        rows.extend(transition_rows(label, base, head))
        rows.extend(diagnostic_rows(label, base, head))
    coverage = [perf_metrics.latency_coverage(side) for side in (base, head)]
    if include is None and any(coverage):
        cells = [perf_metrics._missing_cell(side) if side.blocked or pair is None else
                 f"{pair[0] / pair[1] * 100:.1f}% ({pair[0]}/{pair[1]})" for side, pair in zip((base, head), coverage)]
        verdict = ("accepted" if perf_metrics.latency_acceptance(*coverage) else
                   f"open: needs ≥{perf_execution.LATENCY_MIN_PERCENT}% attributed on each side, within {perf_execution.LATENCY_MAX_GAP_POINTS} points")
        rows.append([label, "latency attribution coverage (%)", cells[0], cells[1], verdict])
    if include is None:
        # The presenter and grid rows follow the status row; Windows n/a rows close the scenario.
        rows[1:1] = perf_metrics.presenter_rows(label, base, head) + perf_metrics.grid_rows(label, base, head)
        rows.extend(perf_atlas_rows.glyph_atlas_rows(label, base, head))
        rows.extend(perf_metrics.windows_na_rows(label, base, head, rows))
    return rows


# Each glyph atlas fact a checkpoint row reports: its field, unit, the value it reads from the facts, and
# whether it is numeric (a change compares the sides' medians) or categorical (no change).
GLYPH_ATLAS_ROW_FACTS = (
    ("glyph_atlas_dim", "px", lambda facts: facts.dim, True),
    ("glyph_atlas_packed_pixels", "px", lambda facts: facts.packed_pixels, True),
    ("glyph_atlas_fit", "outcome", lambda facts: facts.fit, False),
    ("glyph_atlas_growths", "count", lambda facts: facts.growths, True),
    ("glyph_atlas_evictions", "count", lambda facts: facts.evictions, True),
    ("glyph_atlas_max_tile", "px", lambda facts: f"{facts.max_tile[0]}x{facts.max_tile[1]}", False),
)
