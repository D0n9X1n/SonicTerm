"""Perf atlas rows."""

from __future__ import annotations
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_latency_schema
import perf_memory
import perf_metrics
import perf_outcomes
import perf_statistics


# The per-run native renderer id row each logical renderer carries after its facts: categorical evidence,
# read by `glyph_atlas_rows` itself rather than through a facts reader.
NATIVE_ID_FIELD = "renderer_native_id"
NATIVE_ID_ROW = (NATIVE_ID_FIELD, "id", None, False)


def _checkpoint_sample(outcome: perf_outcomes.RunOutcome, point: Mapping) -> perf_memory.MemorySample | None:
    """A checkpoint's own memory sample by the rules `run_metrics` reads it with, or None when the run has no
    usable one: no hook, no tagged line, a conflicting reading, or a sample older than its freshness time."""
    if (outcome.result or {}).get("checkpoint_memory") != "supported":
        return None
    reading = perf_memory.checkpoint_memory(outcome.memory, point["index"])
    if reading is None or reading.problem is not None:
        return None
    fresh_after = point.get("fresh_after_unix_s")
    if fresh_after is not None and reading.sample.unix_s < fresh_after:
        return None
    return reading.sample


def _checkpoint_atlases(outcome: perf_outcomes.RunOutcome) -> list[tuple[Mapping, tuple, Mapping | None]]:
    """Each well-formed checkpoint of a run with its sample's glyph atlas facts (empty when it has none) and the
    harness's atlas reading taken at the same sampling attempt, or None when the harness recorded none."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    found = []
    for point in (outcome.result or {}).get("checkpoints") or []:
        if not perf_result_schema._checkpoint_ok(point):
            continue
        sample = _checkpoint_sample(outcome, point)
        if sample is None:
            found.append((point, (), None))
            continue
        # The reading belongs to the sample only when both come from the same attempt.
        reading = next((reading for reading in point.get("atlas_readings") or []
                        if reading["attempt"] == sample.checkpoint_attempt), None)
        found.append((point, sample.glyph_atlases, reading))
    return found


# A renderer breakdown identity: `role[native label]`.
_RENDERER_IDENTITY = re.compile(r"([A-Za-z_][\w-]*)\[(.*)\]")


def logical_renderers(atlases: Sequence[perf_memory.GlyphAtlasFacts], reading: Mapping | None) -> list[tuple[str, perf_memory.GlyphAtlasFacts]]:
    """Each renderer's facts under a logical identity that is the same in every run and on both sides.

    A visible renderer's label is its native window id (an object address on macOS, an HWND on Windows), new
    in every run, so the visible renderer the harness's reading names as its main window is `main`. A warm
    renderer keeps its pool slot, `warm[slot]`, which is already stable. Any other visible renderer is
    `visible#k`, numbered in label order, which is stable only while a run has one of them; a run without a
    reading has no `main`, so its visible renderers are all numbered rather than guessed. Any other role keeps
    its identity as reported.
    """
    main_window = (reading or {}).get("main_window")
    named, others = [], []
    for facts in atlases:
        parsed = _RENDERER_IDENTITY.fullmatch(facts.renderer)
        if parsed is None or parsed.group(1) != "visible":
            named.append((facts.renderer, facts))
        elif main_window is not None and parsed.group(2) == main_window:
            named.append(("main", facts))
        else:
            others.append(facts)
    others.sort(key=lambda facts: facts.renderer)
    named.extend((f"visible#{number}", facts) for number, facts in enumerate(others, start=1))
    return named


def _native_label(facts: perf_memory.GlyphAtlasFacts) -> str:
    """The native label inside a renderer's `role[label]` identity, or the whole identity when it has none."""
    parsed = _RENDERER_IDENTITY.fullmatch(facts.renderer)
    return parsed.group(2) if parsed else facts.renderer


def _distinct_cell(values: Sequence[object]) -> str:
    """One value when every run agrees, otherwise each distinct value with how many runs reported it."""
    counts: dict[str, int] = {}
    for value in values:
        counts[str(value)] = counts.get(str(value), 0) + 1
    if len(counts) == 1:
        return next(iter(counts))
    return "; ".join(f"{text} ×{count}" for text, count in sorted(counts.items()))


def glyph_atlas_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[list[str]]:
    """Per checkpoint and logical renderer, each glyph atlas fact on each side, from the checkpoint's memory sample.

    Rows are keyed by `logical_renderers`, so the same window lines up across runs and sides although its
    native id differs in each; a `renderer_native_id` row lists those ids per run as evidence. A cell shows the value every run reported, or each distinct value with its run count (a categorical fit
    such as `no_headroom`, `does_not_fit` or `evicted` included), and how many runs had it when not all did. A
    side without the facts, such as a base built before them, reads `n/a`. A numeric fact's change compares the
    sides' medians; a categorical one has none. No row is added when neither side reports facts.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_comparison_rows
    import perf_metrics
    sides = (base, head)
    # values[side][(checkpoint label, logical renderer, field)] = one value per run that reported it
    values: list[dict[tuple[str, str, str], list[object]]] = [{}, {}]
    order: list[tuple[str, str]] = []
    for side_values, side in zip(values, sides):
        for outcome in side.outcomes:
            for point, atlases, reading in _checkpoint_atlases(outcome):
                for renderer, facts in logical_renderers(atlases, reading):
                    if (point["label"], renderer) not in order:
                        order.append((point["label"], renderer))
                    for field_name, _unit, read, _numeric in perf_comparison_rows.GLYPH_ATLAS_ROW_FACTS:
                        side_values.setdefault((point["label"], renderer, field_name), []).append(read(facts))
                    # The native id stays as per-run evidence; it never keys a row.
                    side_values.setdefault((point["label"], renderer, NATIVE_ID_FIELD), []).append(
                        _native_label(facts))
    rows = []
    for checkpoint_label, renderer in order:
        for field_name, unit, _read, numeric in (*perf_comparison_rows.GLYPH_ATLAS_ROW_FACTS, NATIVE_ID_ROW):
            key = (checkpoint_label, renderer, field_name)
            cells, figures = [], []
            for side, side_values in zip(sides, values):
                reported = side_values.get(key, [])
                if not reported or side.blocked or side.failed:
                    cells.append(perf_metrics._missing_cell(side))
                    figures.append(None)
                    continue
                cell = _distinct_cell(reported)
                if len(reported) < len(side.outcomes):
                    cell += f", {len(reported)}/{len(side.outcomes)} runs"
                cells.append(cell)
                figures.append(perf_statistics.median(reported) if numeric else None)
            change = perf_metrics.percent_change(*figures) if numeric else ""
            rows.append([label, f"{checkpoint_label} {renderer} {field_name} ({unit})", cells[0], cells[1], change])
    return rows


def _counted_growths_by(outcome: perf_outcomes.RunOutcome, unix_s: float) -> int | None:
    """The `renderer.glyph_atlas_growths` deltas of every phase that ended by `unix_s`, summed; None when no
    such phase counted, or one that ended by then has no count."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    counted = None
    for phase in (outcome.result or {}).get("phases") or []:
        if not isinstance(phase, dict) or not perf_latency_schema._is_number(phase.get("end_unix_s")) or phase["end_unix_s"] > unix_s:
            continue
        renderer = (phase.get("frame_counters") or {}).get("renderer")
        if not isinstance(renderer, Mapping) or not perf_result_schema._count_ok(renderer.get("glyph_atlas_growths")):
            return None
        counted = (counted or 0) + renderer["glyph_atlas_growths"]
    return counted


# A run's reconciliation verdicts, from the one that decides a cell to the one that decides it last.
RECONCILE_MISMATCH, RECONCILE_INCONCLUSIVE, RECONCILE_CONSISTENT = "mismatch", "inconclusive", "consistent"


def _reconcile_run(atlases: Sequence[perf_memory.GlyphAtlasFacts], reading: Mapping | None,
                   phase_counted: int | None) -> tuple[str, str] | None:
    """One run's verdict at one checkpoint and the figures behind it; None when it has nothing to compare.

    The comparison is an equality at one boundary: a renderer's snapshot growths count since it was built, and
    the reading's per-window counted growths, taken in the same sampling attempt, count since its window was
    created, so each live window's two figures must be equal. Its figures read `logical snapshot/counted`. A
    warm renderer has no window and draws nothing, so it is not compared. A visible renderer the reading did
    not count, a counted window the snapshot does not hold, or a run without a per-window reading is
    inconclusive: the summed figures alone differ by startup growth and closed windows, so neither their
    equality nor their inequality proves anything, and both are shown. Closed windows' counted growths are
    listed apart; their renderers are no longer in the snapshot.
    """
    if not atlases:
        return None
    counted = (reading or {}).get("counted_glyph_atlas_growths")
    if counted is None:
        if phase_counted is None:
            return None
        snapshot = sum(facts.growths for facts in atlases)
        return RECONCILE_INCONCLUSIVE, f"snapshot {snapshot}, counted {phase_counted}"
    verdict = RECONCILE_CONSISTENT
    figures = []
    held = set()
    for renderer, facts in logical_renderers(atlases, reading):
        if renderer.startswith("warm["):
            continue
        native = _native_label(facts)
        held.add(native)
        window_counted = counted.get(native)
        if window_counted is None:
            # When: the window has no counted figure, nothing the snapshot holds can be checked against it.
            figures.append(f"{renderer} {facts.growths}/none")
            verdict = RECONCILE_MISMATCH if verdict == RECONCILE_MISMATCH else RECONCILE_INCONCLUSIVE
            continue
        figures.append(f"{renderer} {facts.growths}/{window_counted}")
        if window_counted != facts.growths:
            verdict = RECONCILE_MISMATCH
    for native in sorted(set(counted) - held):
        figures.append(f"unheld {native} none/{counted[native]}")
        verdict = RECONCILE_MISMATCH if verdict == RECONCILE_MISMATCH else RECONCILE_INCONCLUSIVE
    closed = reading.get("closed_glyph_atlas_growths")
    if closed:
        figures.append(f"closed {closed}")
    return verdict, ", ".join(figures)


def glyph_atlas_reconciliation_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[list[str]]:
    """Counters-table rows reconciling each checkpoint's snapshot growths with the frame counters' growths.

    Each run's verdict comes from `_reconcile_run`; a cell names the worst verdict across its runs (a mismatch,
    then an inconclusive run, else consistent), how many runs had it when it is not consistent, and every run's
    figures separated by `; `. "Consistent" is printed only when every live window's figures are equal. A side
    with nothing to compare reads `n/a`, and no row is added when neither side has anything.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_counter_rows
    import perf_metrics
    sides = (base, head)
    verdicts: list[dict[str, list[tuple[str, str]]]] = [{}, {}]
    order: list[str] = []
    for side_verdicts, side in zip(verdicts, sides):
        if side.blocked or side.failed:
            continue
        for outcome in side.outcomes:
            for point, atlases, reading in _checkpoint_atlases(outcome):
                found = _reconcile_run(atlases, reading, _counted_growths_by(outcome, point["unix_s"]))
                if found is None:
                    continue
                if point["label"] not in order:
                    order.append(point["label"])
                side_verdicts.setdefault(point["label"], []).append(found)
    rows = []
    for checkpoint_label in order:
        cells = []
        for side, side_verdicts in zip(sides, verdicts):
            found = side_verdicts.get(checkpoint_label)
            if not found:
                cells.append("n/a" if side.blocked == perf_counter_rows.COUNTERS_HEAD_ONLY else perf_metrics._missing_cell(side))
                continue
            figures = "; ".join(figure for _verdict, figure in found)
            for verdict in (RECONCILE_MISMATCH, RECONCILE_INCONCLUSIVE):
                count = sum(run_verdict == verdict for run_verdict, _figure in found)
                if count:
                    cells.append(f"{verdict} in {count} of {len(found)} runs: {figures}")
                    break
            else:
                cells.append(f"{RECONCILE_CONSISTENT}: {figures}")
        rows.append([label, checkpoint_label, "glyph_atlas_growths, snapshot/counted", cells[0], cells[1], ""])
    return rows


def laps_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[list[str]]:
    """Rows of the separate laps table: each render_timing lap pooled, with its per-run spread."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    return perf_metrics._rows(label, base, head, perf_metrics.lap_metrics, None)
