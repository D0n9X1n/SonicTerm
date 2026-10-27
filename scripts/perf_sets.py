"""Perf sets."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_font_diagnostics
import perf_latency_schema
import perf_outcomes
import perf_settings
import perf_statistics


# --- Comparison: run sets, worktrees, host block and document --------------------------------

# The harness reports these fields of the measurement display; a null field is one it could not read.
DISPLAY_FIELDS = ("name", "refresh_rate_millihertz", "scale_factor")


def display_of(result: Mapping | None) -> dict | None:
    """Return a result's measurement display, or None when it reported none; unreported fields stay null."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    monitor = (result or {}).get("monitor")
    return monitor if perf_result_schema._monitor_ok(monitor) else None


def describe_display(monitor: Mapping | None) -> str:
    """Name a display as `<name>, <Hz> Hz, scale <s>`; no display reads `unknown`, and so does an unreported rate."""
    if monitor is None:
        return "unknown"
    rate = monitor.get("refresh_rate_millihertz")
    rate_text = "unknown" if rate is None else f"{rate / 1000:g}"
    return f"{monitor.get('name') or 'unnamed display'}, {rate_text} Hz, scale {monitor['scale_factor']:g}"


def display_differences(first: Mapping, other: Mapping) -> list[str]:
    """List the display fields both runs reported that differ; a field either run left null is not compared."""
    return [key for key in DISPLAY_FIELDS
            if first.get(key) is not None and other.get(key) is not None and first[key] != other[key]]


@dataclass
class DisplayReference:
    """The display every valid run of a comparison must share.

    The first valid run that reported a display sets it; a later passing run fills in any field it
    lacked, so a rate first learned later still binds every run after it.
    """

    monitor: dict | None = None

    def learn(self, measured: Mapping | None) -> None:
        """Adopt a passing run's display, filling in only the fields the reference did not know yet."""
        if measured is None:
            return
        if self.monitor is None:
            self.monitor = dict(measured)
            return
        for key in DISPLAY_FIELDS:
            if self.monitor.get(key) is None and measured.get(key) is not None:
                self.monitor[key] = measured[key]


class StopComparison(Exception):
    """A schema failure, a refusal or an unresolved cleanup: the comparison stops with exit 1 and the reason."""


@dataclass
class SetResult:
    """One run set (timed, laps or alloc) of one scenario: each side's valid runs and every attempt."""

    label: str
    set_name: str
    base: SideRuns
    head: SideRuns
    attempts: list = field(default_factory=list)
    # The valid runs each side needed; a set that never ran (a blocked delivery) keeps 0 and is blocked instead.
    target_runs: int = 0
    # The runs asked for before a short-mode cap; above target_runs only for a capped variant.
    requested_runs: int = 0
    # Why a declared diagnostic is unavailable on both sides (its harness cfg is off); such a set never runs.
    unavailable: str | None = None


def grid_size(grid: object) -> tuple[int, int] | None:
    """A result's grid as (columns, rows); the harness writes `{cols, rows}`. Anything else is unknown."""
    if isinstance(grid, dict):
        columns = grid.get("cols", grid.get("columns"))
        if perf_latency_schema._is_int(columns) and perf_latency_schema._is_int(grid.get("rows")):
            return columns, grid["rows"]
    return None


def grid_text(size: tuple[int, int] | None) -> str:
    """Name a grid as `<columns>x<rows>`, or `unknown`."""
    return "unknown" if size is None else f"{size[0]}x{size[1]}"


def renderer_identity(renderer: Mapping | None) -> dict | None:
    """The adapter fields a pair must share; whether the adapter was selected or reused is not identity."""
    return None if renderer is None else {key: value for key, value in renderer.items() if key != "event"}


def run_set(label: str, plans: Mapping[str, perf_outcomes.RunPlan], base_blocked: str | None, runs: int,
            run_case: Callable[[perf_outcomes.RunPlan, Path], perf_outcomes.RunOutcome], evidence: Path, set_name: str = "timed",
            display: DisplayReference | None = None) -> SetResult:
    """Run one set A B B A until each side has `runs` valid runs.

    An invalid run is retried, at most RETRY_LIMIT times per side. A grid that differs from
    the first valid run's makes the pair invalid, and so does a display that differs from
    `display`, the comparison's reference, in any field both reported: name, refresh rate or
    scale. Only a field a run did not report goes unchecked. On Windows, where the window opens at
    whatever grid its display allows, an adapter or presenter that differs from the first valid
    run's makes the pair invalid too. A base that cannot build or run is
    `blocked` and the head still runs; a head that cannot is blocked, and one that exhausts
    its retries fails. A schema failure, a refusal or an unresolved cleanup stops the comparison.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_inventory
    import perf_metrics
    sides = {side: perf_metrics.SideRuns() for side in perf_settings.SIDES}
    result = SetResult(label, set_name, sides["base"], sides["head"], target_runs=runs)
    reasons: dict[str, list[str]] = {side: [] for side in perf_settings.SIDES}
    schedule = perf_statistics.AbbaSchedule(runs)
    if base_blocked:
        sides["base"].blocked = base_blocked
        schedule.retire("base")
    reference_grid = None
    # The first valid run's adapter and presenter; every later run of the set must match them.
    reference_renderer = reference_presenter = None
    if display is None:
        display = DisplayReference()
    attempt = 0
    while True:
        side = schedule.next_side()
        if side is None:
            return result
        attempt += 1
        run_evidence = evidence / f"{attempt:02d}-{side}"
        outcome = run_case(plans[side], run_evidence)
        kind, why = perf_outcomes.classify_outcome(outcome)
        if kind == "valid":
            grid = (outcome.result or {}).get("grid")
            measured = display_of(outcome.result)
            renderer = renderer_identity(outcome.renderer)
            presenter = (outcome.result or {}).get("presenter")
            if reference_grid is not None and grid != reference_grid:
                kind, why = "grid", [f"grid {grid} differs from the pair's {reference_grid}"]
            elif (measured is not None and display.monitor is not None
                  and display_differences(display.monitor, measured)):
                fields = ", ".join(display_differences(display.monitor, measured))
                kind, why = "display", [f"display {describe_display(measured)} differs from the comparison's "
                                        f"{describe_display(display.monitor)} in {fields}"]
            elif renderer is not None and reference_renderer is not None and renderer != reference_renderer:
                kind, why = "renderer", [f"renderer {perf_font_diagnostics.describe_renderer(renderer)} differs from the pair's "
                                         f"{perf_font_diagnostics.describe_renderer(reference_renderer)}"]
            elif presenter is not None and reference_presenter is not None and presenter != reference_presenter:
                kind, why = "presenter", [f"presenter {presenter} differs from the pair's {reference_presenter}"]
            else:
                # Only a run that passed every check sets the grid, adapter and presenter or teaches the display.
                if reference_grid is None:
                    reference_grid = grid
                if reference_renderer is None:
                    reference_renderer = renderer
                if reference_presenter is None:
                    reference_presenter = presenter
                display.learn(measured)
        result.attempts.append((side, str(run_evidence), kind, why))
        if run_evidence.is_dir():
            # outcome.json is written before the set's grid, display, renderer and presenter checks; this is the
            # attempt's final classification, which perf-flags binds a set's accepted attempts to.
            perf_outcomes._write_json(run_evidence / perf_inventory.CLASSIFICATION_FILE, {"side": side, "kind": kind, "reasons": list(why)})
        print(f"[perf-compare] {label} {set_name} {side} run {attempt}: {kind}"
              + (f": {'; '.join(why)}" if why else ""), flush=True)
        verdict = perf_outcomes.compare_verdict(kind)
        if verdict == "stop":
            raise StopComparison(f"{label} {set_name} {side}: {kind}: {'; '.join(why)}")
        if verdict == "blocked":
            sides[side].blocked = "; ".join(why) or kind
            sides[side].outcomes = []
            schedule.retire(side)
            if side == "head":
                return result
            continue
        if verdict == "valid":
            sides[side].outcomes.append(outcome)
        else:
            reasons[side].append(f"{kind}: {'; '.join(why)}")
        schedule.record(side, verdict == "valid")
        failed = schedule.failed_side
        if failed is not None:
            summary = f"no {runs} valid runs after {perf_settings.RETRY_LIMIT} retries: " + " | ".join(reasons[failed])
            sides[failed].outcomes = []
            if failed == "head":
                sides[failed].failed = summary
                return result
            sides[failed].blocked = summary
            schedule.retire(failed)


def strict_problems(results: Iterable[SetResult]) -> list[str]:
    """Name every side of every set without exactly its valid runs: blocked, failed, or not `target_runs`.

    An empty result set and a set without a positive target are problems too.

    The one gap allowed is a counters set whose base has no perf-counters (COUNTERS_HEAD_ONLY), which
    the table reports as n/a. `--require-base` turns any other problem into a failed comparison.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_counter_rows
    results = list(results)
    # When: no set ran, nothing was measured, so there is no comparison to pass.
    if not results:
        return ["no scenario set ran"]
    runnable = [result for result in results if result.unavailable is None]
    if not runnable:
        # When: every selected set is a declared-unavailable diagnostic, nothing was measured either.
        return ["no runnable scenario set: " + "; ".join(f"{result.label} {result.unavailable}"
                                                         for result in results)]
    problems = []
    # A declared-unavailable diagnostic is reported, never a strict problem; every other gap stays one.
    for result in runnable:
        for side_name, side in (("base", result.base), ("head", result.head)):
            where = f"{result.label} {result.set_name} {side_name}"
            # When: the base has no counters feature, its counters cells are n/a by design, not missing runs.
            if side_name == "base" and result.set_name == "counters" and side.blocked == perf_counter_rows.COUNTERS_HEAD_ONLY:
                continue
            if side.blocked:
                problems.append(f"{where}: blocked: {side.blocked}")
            elif side.failed:
                problems.append(f"{where}: failed: {side.failed}")
            elif result.target_runs < 1:
                problems.append(f"{where}: target {result.target_runs} is not positive")
            elif len(side.outcomes) != result.target_runs:
                # Counts are exact: more valid runs than planned is not the comparison the table describes.
                problems.append(f"{where}: {len(side.outcomes)} of {result.target_runs} valid runs")
    return problems


def capped_label(result: SetResult) -> str:
    """A set's row label: `<label> (runs N of M)` when a short-mode cap lowered its runs, else the label."""
    if 0 < result.target_runs < result.requested_runs:
        return f"{result.label} (runs {result.target_runs} of {result.requested_runs})"
    return result.label


def comparison_exit(results: Iterable[SetResult], require_base: bool = False) -> int:
    """Exit 1 when a head set failed, 3 when the head was blocked, else 0; a blocked base is reported, not failed.

    With `require_base`, any strict problem (either side blocked, failed or short) exits 1 instead.
    """
    results = list(results)
    if require_base and strict_problems(results):
        return perf_settings.EXIT_FAIL
    runnable = [result for result in results if result.unavailable is None]
    if results and not runnable:
        # When: only declared-unavailable diagnostics were selected, nothing ran, so the comparison is blocked.
        return perf_settings.EXIT_BLOCKED
    results = runnable
    if any(result.head.failed for result in results):
        return perf_settings.EXIT_FAIL
    if any(result.head.blocked for result in results):
        return perf_settings.EXIT_BLOCKED
    return perf_settings.EXIT_PASS
