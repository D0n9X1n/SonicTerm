"""Perf metrics."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_font_diagnostics
import perf_latency_schema
import perf_memory
import perf_outcomes
import perf_statistics


@dataclass
class SideRuns:
    """One side's valid runs of a scenario, or why it has none: `blocked` (cannot run) or `failed` (head only)."""

    outcomes: list = field(default_factory=list)
    blocked: str | None = None
    failed: str | None = None


@dataclass(frozen=True)
class NotAvailable:
    """A run metric the run cannot report, with the reason its cell names: `n/a: <reason>`."""

    reason: str


class PartialValue(float):
    """A checkpoint memory figure read from a partial sample: it counts, and its cell says how many runs."""


# A side whose harness has no checkpoint-memory hook: result.json says so, or an older harness leaves it out.
UNSUPPORTED_CHECKPOINT_MEMORY = "unsupported"
CHECKPOINT_MEMORY_STATES = ("supported", "unsupported")
# result.json's `hooks.trim`. `unsupported` is an untrimmed baseline whose memory readings stay figures; an
# older harness writes no `hooks` at all.
TRIM_HOOK_OUTCOMES = ("not-reached", "unsupported", "skipped", "trimmed")
# The short S12 plan's covered-window trim experiment, as result.json's `trim_experiment` names it, and the
# checkpoint its trim rules govern.
TRIM_EXPERIMENT = "s12-short-trim"
TRIM_CHECKPOINT = "covered"
# Why a trim experiment's covered reading is not compared: a hooked sample taken before the hook's trim, or
# trim tags on a side whose harness says it cannot trim.
TRIM_STALE = "stale"
TRIM_SCHEMA = "schema"
# A supported trim experiment whose hook did not trim: its covered reading is never compared as a trimmed one,
# and its raw figure is kept in a separate row.
TRIM_NOT_RUN = {"skipped": "trim skipped", "not-reached": "trim not reached"}
TRIM_NOT_RECORDED = "trim not recorded"
# The sources a successful trim names on a checkpoint line.
TRIM_SOURCES = ("hook", "scheduler")


def trim_experiment_run(result: Mapping) -> bool:
    """Whether a result is the short S12 trim experiment, the one run whose covered phase is held covered by
    the harness on every host and whose covered window may be trimmed on request."""
    return (result.get("scenario") == "S12" and result.get("short") is True
            and result.get("trim_experiment") == TRIM_EXPERIMENT)


def trim_reading_problem(result: Mapping, label: str, sample: perf_memory.MemorySample) -> str | None:
    """Why a trim experiment's covered sample cannot be compared, or None when it can.

    An unsupported hook is an untrimmed baseline and must carry no trim tags. A trimmed hook's sample counts
    only when it says the window is trimmed, names a `hook` or `scheduler` source, and carries a `trim_seq`
    at least the number the hook reported, so it was taken after that trim. A supported experiment that did
    not trim (skipped, not reached, or no recorded hook) is never compared as a trimmed reading. Every other
    checkpoint and run keeps the ordinary rules.
    """
    if label != TRIM_CHECKPOINT or not trim_experiment_run(result):
        return None
    hooks = result.get("hooks")
    trim = hooks.get("trim") if isinstance(hooks, dict) else None
    if sample.malformed_trim_tags:
        # When: a trim tag is present but unreadable, the sample cannot count as tagged or untagged.
        return TRIM_SCHEMA
    tagged = sample.trimmed is not None or sample.trim_source is not None or sample.trim_seq is not None
    if trim == "unsupported":
        return TRIM_SCHEMA if tagged else None
    if trim != "trimmed":
        return TRIM_NOT_RUN.get(trim, TRIM_NOT_RECORDED)
    after_hook = result.get("trim_seq_after_hook")
    if not perf_latency_schema._is_int(after_hook):
        # When: a trimmed hook without its number cannot be checked against any sample.
        return TRIM_SCHEMA
    if not sample.trim_seq or sample.trim_seq < after_hook:
        # When: trim_seq is missing, zero or older than the hook's, the sample predates that trim.
        return TRIM_STALE
    if sample.trimmed is None or sample.trim_source not in TRIM_SOURCES:
        # When: a current sample lacks its trim state or names no trimming source, its tags are malformed.
        return TRIM_SCHEMA
    if sample.trimmed is not True:
        # When: trimmed is false, the window was no longer trimmed when the sample was taken.
        return TRIM_STALE
    return None


CHECKPOINT_SAMPLING_STATES = ("complete", "exhausted", "active")


def run_metrics(outcome: perf_outcomes.RunOutcome) -> dict[tuple[str, str, str], object]:
    """Extract one valid run's metrics, keyed (name, unit, kind).

    `frame` metrics hold that run's samples, pooled across runs; `run` and `footprint` metrics hold
    one value per run. A field the result lacks yields no key, so it prints `n/a`, never zero.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_execution
    import perf_result_schema
    result = outcome.result or {}
    metrics: dict[tuple[str, str, str], object] = {}
    trim_run = trim_experiment_run(result)
    for phase in result.get("phases") or []:
        name = phase.get("name")
        start, end = phase.get("start_unix_s"), phase.get("end_unix_s")
        wall_s = end - start if perf_latency_schema._is_number(start) and perf_latency_schema._is_number(end) else None
        if wall_s is not None:
            metrics[(f"{name} wall", "s", "run")] = wall_s
        if trim_run and name == TRIM_CHECKPOINT:
            # The trim experiment holds this phase covered on every host, so it draws almost nothing: a rate or
            # an interval would describe the harness, not the renderer. It reports activity counts instead.
            if perf_latency_schema._is_int(phase.get("presented_frames")):
                metrics[(f"{name} presented frames", "count", "run")] = phase["presented_frames"]
            if perf_latency_schema._is_int(phase.get("redraw_requested")):
                metrics[(f"{name} redraws requested", "count", "run")] = phase["redraw_requested"]
            if perf_latency_schema._is_number(phase.get("cpu_user_s")) and perf_latency_schema._is_number(phase.get("cpu_system_s")):
                metrics[(f"{name} CPU", "s", "run")] = phase["cpu_user_s"] + phase["cpu_system_s"]
            continue
        kind = phase.get("kind")
        if kind == "hold":
            # A hold is idle by design: its seconds and activity counts, never a rate, an interval or a completion.
            if wall_s is not None:
                del metrics[(f"{name} wall", "s", "run")]
                metrics[(f"{name} hold", "s", "run")] = wall_s
            for key, label in (("presented_frames", "presented frames"), ("redraw_requested", "redraws requested")):
                if perf_latency_schema._is_int(phase.get(key)):
                    metrics[(f"{name} {label}", "count", "run")] = phase[key]
            if perf_latency_schema._is_number(phase.get("cpu_user_s")) and perf_latency_schema._is_number(phase.get("cpu_system_s")):
                metrics[(f"{name} CPU", "s", "run")] = phase["cpu_user_s"] + phase["cpu_system_s"]
            continue
        if kind == "transition":
            # A transition ends at its endpoint, so a rate or an interval percentile over it means nothing; its
            # completion has its own per-run rows (transition_rows), and these are its presentation counts.
            if perf_latency_schema._is_number(phase.get("first_present_ms")):
                metrics[(f"{name} first present", "ms", "run")] = phase["first_present_ms"]
            for key, label in (("presented_frames", "presented frames"),
                               ("nonpresenting_redraws", "non-presenting redraws")):
                if perf_latency_schema._is_int(phase.get(key)):
                    metrics[(f"{name} {label}", "count", "run")] = phase[key]
            if perf_latency_schema._is_number(phase.get("cpu_user_s")) and perf_latency_schema._is_number(phase.get("cpu_system_s")):
                metrics[(f"{name} CPU", "s", "run")] = phase["cpu_user_s"] + phase["cpu_system_s"]
            continue
        if wall_s and perf_latency_schema._is_int(phase.get("presented_frames")):
            metrics[(f"{name} presented frames", "fps", "run")] = phase["presented_frames"] / wall_s
        # Presented frames per logical update, divided by this run's own recorded count, never a constant;
        # a phase without `updates` (every phase but S10's stream, or an older harness) has no figure.
        if perf_latency_schema._is_int(phase.get("presented_frames")) and perf_latency_schema._is_int(phase.get("updates")) and phase["updates"] > 0:
            metrics[(f"{name} presented frames per update", "ratio", "run")] = \
                phase["presented_frames"] / phase["updates"]
        if wall_s and perf_latency_schema._is_int(phase.get("redraw_requested")):
            metrics[(f"{name} redraws requested", "per s", "run")] = phase["redraw_requested"] / wall_s
        if perf_latency_schema._is_number(phase.get("cpu_user_s")) and perf_latency_schema._is_number(phase.get("cpu_system_s")):
            metrics[(f"{name} CPU", "s", "run")] = phase["cpu_user_s"] + phase["cpu_system_s"]
        # The pooled dispatch rows stay; the presenting and non-presenting ones are each one cluster, so a
        # shift in the presenting share moves neither (a harness without them shows n/a).
        for key, label in (("dispatch_ms", "dispatch"), ("dispatch_presenting_ms", "presenting dispatch"),
                           ("dispatch_nonpresenting_ms", "non-presenting dispatch"),
                           ("present_interval_ms", "present interval")):
            if perf_latency_schema._numbers(phase.get(key)):
                metrics[(f"{name} {label}", "ms", "frame")] = phase[key]
        if perf_latency_schema._numbers(phase.get("allocations_per_frame")):
            metrics[(f"{name} allocations per frame", "count", "frame")] = phase["allocations_per_frame"]
    attributed = perf_latency_schema.latency_values(result.get("latency"))
    if attributed:
        metrics[("keypress-to-present latency", "ms", "frame")] = attributed
    throughput = result.get("throughput")
    if isinstance(throughput, dict) and perf_latency_schema._is_number(throughput.get("seconds")) and throughput["seconds"] > 0 \
            and perf_latency_schema._is_int(throughput.get("bytes")):
        metrics[("throughput", "MB/s", "run")] = throughput["bytes"] / throughput["seconds"] / 1_000_000
    if perf_latency_schema._is_number(result.get("uncover_ms")):
        metrics[("uncover", "ms", "run")] = result["uncover_ms"]
    if perf_latency_schema._is_int(result.get("scrollback_rows_retained")):
        metrics[("scrollback rows retained", "rows", "run")] = result["scrollback_rows_retained"]
    supported = result.get("checkpoint_memory") == "supported"
    for point in result.get("checkpoints") or []:
        if not perf_result_schema._checkpoint_ok(point):
            continue
        if "frame_texture_bytes" in point:
            # The frame texture's own reading, not a memory sample; a base without the feature has none.
            metrics[(f"{point['label']} frame_texture_bytes", "B", "run")] = point["frame_texture_bytes"]
        keys = (f"{point['label']} renderer_total_bytes", f"{point['label']} process_resident_bytes",
                f"{point['label']} grid bytes per pane", f"{point['label']} renderer_row_glyph_cache_bytes")
        if not supported:
            # A harness without the hook takes no checkpoint sample; a periodic one is never substituted.
            for key in keys:
                metrics[(key, "MiB", "run")] = NotAvailable(UNSUPPORTED_CHECKPOINT_MEMORY)
            continue
        reading = perf_memory.checkpoint_memory(outcome.memory, point["index"])
        if reading is None:
            continue  # No tagged line reached the log: memory is unavailable, not a failure.
        if reading.problem is not None:
            for key in keys:
                metrics[(key, "MiB", "run")] = NotAvailable(reading.problem)
            continue
        sample = reading.sample
        fresh_after = point.get("fresh_after_unix_s")
        if fresh_after is not None and sample.unix_s < fresh_after:
            continue  # Taken before the checkpoint's reading is fresh: unavailable, never a stale figure.
        trim_problem = trim_reading_problem(result, point["label"], sample)
        if trim_problem is not None:
            for key in keys:
                metrics[(key, "MiB", "run")] = NotAvailable(trim_problem)
            if trim_problem != TRIM_SCHEMA:
                # When: the reading is real but not a credited trim, its figure stays visible on its own row.
                metrics[(f"{point['label']} renderer_total_bytes, uncredited trim", "MiB", "run")] = \
                    sample.renderer_total_bytes / perf_execution.MIB
            continue
        figure = PartialValue if reading.partial else float
        metrics[(keys[0], "MiB", "run")] = figure(sample.renderer_total_bytes / perf_execution.MIB)
        if sample.process_resident_bytes is not None:
            metrics[(keys[1], "MiB", "run")] = figure(sample.process_resident_bytes / perf_execution.MIB)
        grid_per_pane = sample.grid_bytes_per_pane()
        if grid_per_pane is not None:
            metrics[(keys[2], "MiB", "run")] = figure(grid_per_pane / perf_execution.MIB)
        if sample.renderer_row_glyph_cache_bytes is not None:
            # Summed over every renderer, so it is read against live renderers x 512 MiB; never gated.
            metrics[(keys[3], "MiB", "run")] = figure(sample.renderer_row_glyph_cache_bytes / perf_execution.MIB)
    for stem, record in outcome.footprints.items():
        if perf_latency_schema._is_int(record.get("bytes")):
            metrics[(f"{stem.partition('-')[2] or stem} footprint", "MiB", "footprint")] = record["bytes"] / perf_execution.MIB
    return metrics


def lap_metrics(outcome: perf_outcomes.RunOutcome) -> dict[tuple[str, str, str], object]:
    """Extract one `--laps` run's render_timing laps, one frame metric per window and lap."""
    metrics: dict[tuple[str, str, str], list[float]] = {}
    for sample in outcome.laps:
        for lap, value in sample.laps.items():
            metrics.setdefault((f"{sample.window} {lap}", "ms", "frame"), []).append(value)
    return metrics


def percent_change(base_value: float | None, head_value: float | None) -> str:
    """Return the head's change from the base as a signed percentage, or `n/a`."""
    if base_value is None or head_value is None or base_value == 0:
        return "n/a"
    return f"{(head_value - base_value) / base_value * 100:+.1f}%"


def _missing_cell(side: SideRuns) -> str:
    """The cell of a metric a side cannot report."""
    return "blocked" if side.blocked else "failed" if side.failed else "n/a"


def _status_cell(side: SideRuns) -> str:
    if side.blocked:
        return f"blocked: {side.blocked}"
    if side.failed:
        return f"failed: {side.failed}"
    return f"{len(side.outcomes)} valid run" + ("" if len(side.outcomes) == 1 else "s")


def _rows(label: str, base: SideRuns, head: SideRuns, extract: Callable[[perf_outcomes.RunOutcome], dict],
          include: Callable[[tuple[str, str, str]], bool] | None) -> list[list[str]]:
    """Build a scenario's rows: a status row, then two rows per frame metric and one per run metric."""
    sides = (base, head)
    rows = [[label, "status", _status_cell(base), _status_cell(head), ""]]
    per_side = [[extract(outcome) for outcome in side.outcomes] for side in sides]
    keys: list[tuple[str, str, str]] = []
    for metrics in per_side[0] + per_side[1]:
        keys.extend(key for key in metrics if key not in keys and (include is None or include(key)))
    for key in keys:
        name, unit, kind = key
        values = [[metrics[key] for metrics in side_metrics if key in metrics] for side_metrics in per_side]
        if kind == "frame":
            summaries = [perf_statistics.frame_summary(side_values) for side_values in values]
            for statistic, attribute in (("median", "median"), ("p95", "percentile_95")):
                cells, figures = [], []
                for side, summary in zip(sides, summaries):
                    figure = None if side.blocked or summary is None else getattr(summary, attribute)
                    if figure is None:
                        cells.append(_missing_cell(side))
                    else:
                        spread = summary.run_median_range if statistic == "median" else summary.run_p95_range
                        cells.append(f"{figure:.2f} ({spread[0]:.2f}–{spread[1]:.2f})")
                    figures.append(figure)
                rows.append([label, f"{name} {statistic} ({unit})", cells[0], cells[1], percent_change(*figures)])
            continue
        cells, figures = [], []
        for side, side_values in zip(sides, values):
            reasons = [value.reason for value in side_values if isinstance(value, NotAvailable)]
            numbers = [value for value in side_values if not isinstance(value, NotAvailable)]
            summary = None if side.blocked else perf_statistics.run_summary(numbers)
            if summary is None:
                # A side that ran but cannot report the metric says why, as `n/a: <reason>`.
                usable = reasons and not side.blocked and not side.failed
                cells.append(f"n/a: {reasons[0]}" if usable else _missing_cell(side))
                figures.append(None)
                continue
            cell = f"{summary.median:.2f} ({summary.minimum:.2f}–{summary.maximum:.2f})"
            # A footprint can fail without failing its run, so its cell says how many runs have it.
            if kind == "footprint" or summary.runs < len(side.outcomes):
                cell += f", {summary.runs}/{len(side.outcomes)} runs"
            partial = sum(isinstance(value, PartialValue) for value in numbers)
            if partial:
                cell += f", {partial} partial"
            cells.append(cell)
            figures.append(summary.median)
        rows.append([label, f"{name} ({unit})", cells[0], cells[1], percent_change(*figures)])
    return rows


def latency_coverage(side: SideRuns) -> tuple[int, int] | None:
    """Pool a side's attributed and total latency samples, or None when no run measured latency."""
    attributed = total = 0
    for outcome in side.outcomes:
        latency = (outcome.result or {}).get("latency")
        if isinstance(latency, dict) and perf_latency_schema._is_int(latency.get("attributed")) and perf_latency_schema._is_int(latency.get("total")):
            attributed += latency["attributed"]
            total += latency["total"]
    return (attributed, total) if total > 0 else None


def latency_acceptance(base: tuple[int, int] | None, head: tuple[int, int] | None) -> bool:
    """Whether latency can be judged: each side attributes >= 80% of samples, within 10 points of the other."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_execution
    if base is None or head is None:
        return False
    (base_attributed, base_total), (head_attributed, head_total) = base, head
    # Integer cross-multiplication keeps the 80% and 10-point bounds exact.
    if base_attributed * 100 < perf_execution.LATENCY_MIN_PERCENT * base_total or head_attributed * 100 < perf_execution.LATENCY_MIN_PERCENT * head_total:
        return False
    gap = abs(base_attributed * head_total - head_attributed * base_total) * 100
    return gap <= perf_execution.LATENCY_MAX_GAP_POINTS * base_total * head_total


def presenter_text(outcome: perf_outcomes.RunOutcome) -> str | None:
    """Name a run's presenter and adapter, or None when it reported neither (a base older than the macOS record)."""
    presenter = (outcome.result or {}).get("presenter")
    if outcome.renderer is None and not isinstance(presenter, Mapping):
        return None
    if isinstance(presenter, Mapping) and presenter.get("windows_gdi"):
        text = "GDI"
    else:
        text = "wgpu"
        if isinstance(presenter, Mapping) and presenter.get("software_render_degraded"):
            text += ", degraded"
    if outcome.renderer is not None:
        text += f" on {perf_font_diagnostics.describe_renderer(outcome.renderer)}"
    return text


def presenter_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """One row naming each side's presenter, when any run reported one."""
    cells = []
    for side in (base, head):
        texts: list[str] = []
        for outcome in side.outcomes:
            text = presenter_text(outcome)
            if text is not None and text not in texts:
                texts.append(text)
        cells.append("; ".join(texts) or None)
    if not any(cells):
        return []
    return [[label, "presenter", cells[0] or _missing_cell(base), cells[1] or _missing_cell(head), ""]]


def grid_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """One row naming each side's grids as `<cols>x<rows>`, when any run reported one; a pair shares one grid."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_sets
    cells = []
    for side in (base, head):
        texts: list[str] = []
        for outcome in side.outcomes:
            size = perf_sets.grid_size((outcome.result or {}).get("grid"))
            if size is not None and perf_sets.grid_text(size) not in texts:
                texts.append(perf_sets.grid_text(size))
        cells.append("; ".join(texts) or None)
    if not any(cells):
        return []
    return [[label, "grid", cells[0] or _missing_cell(base), cells[1] or _missing_cell(head), ""]]


NO_OCCLUSION_NOTE = "Windows reports no occlusion"
NO_FOOTPRINT_NOTE = "Windows has no `footprint`"


def windows_na_rows(label: str, base: SideRuns, head: SideRuns, existing: Sequence[Sequence[str]]) -> list[list[str]]:
    """Rows a Windows comparison states as `n/a`: S12's occlusion figures and every checkpoint's footprint."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    outcomes = base.outcomes + head.outcomes
    if not any(outcome.platform == "win32" for outcome in outcomes):
        return []
    present = {row[1] for row in existing}
    rows = []
    if label.split("/")[0] == "S12":
        for metric in ("uncover (ms)", "memory released while covered (MiB)"):
            if metric not in present:
                rows.append([label, metric, "n/a", "n/a", NO_OCCLUSION_NOTE])
    labels: list[str] = []
    for outcome in outcomes:
        for point in (outcome.result or {}).get("checkpoints") or []:
            if perf_result_schema._checkpoint_ok(point) and point["label"] not in labels:
                labels.append(point["label"])
    for checkpoint in labels:
        metric = f"{checkpoint} footprint (MiB)"
        if metric not in present:
            rows.append([label, metric, "n/a", "n/a", NO_FOOTPRINT_NOTE])
    return rows


DELIVERY_FILE = "delivery.json"
# The delivered text the harness classified, which it keeps beside its record.
DELIVERY_TEXT_FILE = "delivery.txt"
# The record schema the harness writes: 2 adds a frame check's `unseen`, `brackets` and `unseen_markers`.
DELIVERY_SCHEMA_VERSION = 2
# Schemas read_delivery accepts; only DELIVERY_SCHEMA_VERSION carries the fields a retry is decided on.
DELIVERY_SCHEMA_VERSIONS = (1, DELIVERY_SCHEMA_VERSION)
# The scenarios whose bytes a Windows comparison replays through ConPTY before the measured runs.
DELIVERY_SCENARIOS = frozenset(("S3", "S9", "S10", "S11"))
DELIVERY_NOTE = "untimed ConPTY replay by the head build, shared by both sides"
# Attempts a replay gets when every failure before the last is one retryable missing frame.
DELIVERY_ATTEMPT_LIMIT = 3
# The one check a retry may follow, and the most missing markers the harness names in it.
RETRYABLE_DELIVERY_CHECK = "sync brackets"
UNSEEN_MARKER_LIMIT = 8
# The exact detail the harness's sync_check writes: three placement counts, then any unpainted count.
FRAME_DETAIL = re.compile(r"enclosed (\d+), empty pair ahead (\d+), absent (\d+)(?:, never painted (\d+))?")
# The most delivered text a replay keeps: the harness's DELIVERY_LIMIT_BYTES.
DELIVERY_TEXT_LIMIT_BYTES = 64 << 20
