"""Perf counter rows."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_latency_schema
import perf_metrics
import perf_statistics
import perf_timeline_schema
# Why a counters set has no base side: the base declares no perf-counters feature.
COUNTERS_HEAD_ONLY = "n/a: the base does not declare perf-counters, so counters run on the head only"
OVERHEAD_HEADER = ("| Scenario | Metric (unit) | Counters off | Counters on | Change |\n"
                   "| --- | --- | --- | --- | --- |\n")
# The scenarios whose timed metrics would show the counters' own cost: typing and the output flood.
OVERHEAD_SCENARIOS = ("S2", "S3")
OVERHEAD_NOTE = "counters-on vs counters-off on the head; sequential sets, not interleaved"
COUNTERS_UNSUPPORTED = ("counters: head does not support them (its sonicterm-app declares no perf-counters "
                        "feature), so the counters set was skipped")


def overhead_applies(label: str) -> bool:
    """Whether a scenario/variant label gets an overhead row set."""
    return label.partition("/")[0] in OVERHEAD_SCENARIOS


def _figure(value: float) -> str:
    """A count's median: whole numbers without a fraction, others with one decimal."""
    return f"{value:.0f}" if float(value).is_integer() else f"{value:.1f}"


def _bucket_label(index: int, bounds: Sequence[int], unit: str) -> str:
    """A bucket as its upper bound, or `>last` for the overflow bucket; never an exact value."""
    return f"≤{bounds[index]} {unit}" if index < len(bounds) else f">{bounds[-1]} {unit}"


def _histogram_cell(histograms: Sequence[Mapping], unit: str) -> tuple[str, float] | None:
    """Pool a field's histograms across runs: the cell (p95 and max as bucket bounds, the mean from sum_us/count
    in the histogram's unit) and its mean; None when it has no events."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    bounds = perf_result_schema.HISTOGRAM_BOUNDS[unit]
    pooled = [sum(histogram["counts"][index] for histogram in histograms) for index in range(len(bounds) + 1)]
    events = sum(pooled)
    if events == 0:
        return None
    # Nearest rank, in integers so the 95% boundary is exact.
    rank = (95 * events + 99) // 100
    running, percentile_index = 0, len(pooled) - 1
    for index, count in enumerate(pooled):
        running += count
        if running >= rank:
            percentile_index = index
            break
    max_index = max(index for index, count in enumerate(pooled) if count)
    mean = sum(histogram["sum_us"] for histogram in histograms) / perf_result_schema.MICROSECONDS_PER_UNIT[unit] / events
    return (f"p95 {_bucket_label(percentile_index, bounds, unit)}, max {_bucket_label(max_index, bounds, unit)}, "
            f"mean {mean:.2f} {unit} ({events} events)"), mean


def _counter_phases(side: perf_metrics.SideRuns) -> dict[str, list[dict]]:
    """A side's frame_counters objects per phase name, one per valid run that reported the phase."""
    phases: dict[str, list[dict]] = {}
    for outcome in side.outcomes:
        for phase in (outcome.result or {}).get("phases") or []:
            if isinstance(phase.get("frame_counters"), dict):
                phases.setdefault(str(phase.get("name")), []).append(phase["frame_counters"])
    return phases


def _counter_cell(per_run: Sequence[dict], run_count: int, section: str, field_name: str,
                  histogram_unit: str | None) -> tuple[str | None, float | None, bool]:
    """One side's cell for a field: its text (None when no run reported it), the figure a change compares
    (a count's median, a histogram's mean) and whether any run saw an event."""
    present = [sections[section][field_name] for sections in per_run
               if isinstance(sections.get(section), dict) and field_name in sections[section]]
    if not present:
        return None, None, False
    if histogram_unit is None and field_name.endswith("_ns"):
        # Nanoseconds stay exact through every delta; only the display converts to microseconds.
        figure = perf_statistics.median(present)
        text = f"{figure / 1000:.2f} ({min(present) / 1000:.2f}–{max(present) / 1000:.2f})"
        active = any(present)
    elif histogram_unit is None:
        figure = perf_statistics.median(present)
        text, active = f"{_figure(figure)} ({min(present)}–{max(present)})", any(present)
    else:
        summary = _histogram_cell(present, histogram_unit)
        text, figure, active = (("no events", None, False) if summary is None else (summary[0], summary[1], True))
    if len(present) < run_count:
        text += f", {len(present)}/{run_count} runs"
    return text, figure, active


def _counter_label(field_name: str, histogram_unit: str | None) -> str:
    """A field's unit label: a histogram's unit; for an integer, `us, summed` when its name ends in _us
    (a summed duration in microseconds), `us, summed from ns` when it ends in _ns, otherwise `count`."""
    if histogram_unit is not None:
        return histogram_unit
    if field_name.endswith("_ns"):
        return "us, summed from ns"
    return "us, summed" if field_name.endswith("_us") else "count"


# The matched fields of one render-attempt class, by role; a class's prefix is `render_` or `apply_`.
ATTEMPT_SPLIT_ROLES = ("attempts", "attempt_ns", "attempt_shape_ns", "attempt_raster_ns", "attempt_shape_requests",
                       "attempt_raster_calls", "attempt_raster_tiles")


def attempt_split(per_run: Sequence[dict], prefix: str) -> tuple[str | None, int]:
    """One side's pooled split of a render-attempt class in one phase, and its pooled attempt count.

    Only runs that carry every field of the class count, and their raw totals are summed before any division,
    so the shares come from one population and the shaping, rasterizing and remaining shares add up to 100%.
    The text is None when no run carries the class.
    """
    fields = [prefix + role for role in ATTEMPT_SPLIT_ROLES]
    runs = [sections["renderer"] for sections in per_run
            if isinstance(sections.get("renderer"), dict)
            and all(perf_latency_schema._is_int(sections["renderer"].get(field)) for field in fields)]
    if not runs:
        return None, 0
    pooled = {role: sum(run[prefix + role] for run in runs) for role in ATTEMPT_SPLIT_ROLES}
    attempts, attempt_ns = pooled["attempts"], pooled["attempt_ns"]
    scope = f"{len(runs)}/{len(per_run)} runs"
    if attempts == 0:
        return f"no {prefix.rstrip('_')} attempts ({scope})", 0
    if attempt_ns == 0:
        return f"{attempts} attempts with no measured time ({scope})", attempts
    shape, raster = pooled["attempt_shape_ns"], pooled["attempt_raster_ns"]
    other = attempt_ns - shape - raster
    text = (f"{attempts} attempts ({scope}): shaping {100 * shape / attempt_ns:.1f}%, rasterizing "
            f"{100 * raster / attempt_ns:.1f}%, other {100 * other / attempt_ns:.1f}%; per attempt "
            f"{attempt_ns / attempts / 1000:.2f} us, {pooled['attempt_shape_requests'] / attempts:.1f} shape "
            f"requests, {pooled['attempt_raster_calls'] / attempts:.1f} raster calls, "
            f"{pooled['attempt_raster_tiles'] / attempts:.1f} tiles")
    return text, attempts


def presenter_counter_notes(label: str, side_name: str, side: perf_metrics.SideRuns) -> list[str]:
    """Notes for each valid counters run whose renderer frame counts disagree with its recorded presenter.

    result.json records the presenter on Windows and macOS: frames drawn through GDI count as software_frames and
    frames presented through wgpu as gpu_frames. A run that recorded no presenter (a base older than the macOS
    record) is not checked.
    """
    notes = []
    for index, outcome in enumerate(side.outcomes, 1):
        presenter = (outcome.result or {}).get("presenter")
        if not isinstance(presenter, Mapping):
            continue
        totals = {"software_frames": 0, "gpu_frames": 0}
        for phase in (outcome.result or {}).get("phases") or []:
            renderer = (phase.get("frame_counters") or {}).get("renderer")
            for name in totals:
                if isinstance(renderer, Mapping) and perf_latency_schema._is_int(renderer.get(name)):
                    totals[name] += renderer[name]
        gdi = presenter.get("windows_gdi") is True
        unexpected, presenter_name = (("gpu_frames", "GDI") if gdi else ("software_frames", "wgpu"))
        if totals[unexpected]:
            notes.append(f"{label} {side_name} run {index} presented through {presenter_name}, but its counters "
                         f"report {totals[unexpected]} {unexpected}")
    return notes


def counter_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> tuple[list[list[str]], int]:
    """Rows of the counters table, and how many fields that were 0 on both sides were left out.

    Per phase, a count is the median of its per-run deltas with their range, and a change compares the
    medians; a histogram pools every run's buckets, and a change compares the means. A base without the
    perf-counters feature, or a field the base's contract lacks, reads `n/a`, with no change.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    import perf_result_schema
    head_only = base.blocked == COUNTERS_HEAD_ONLY
    rows = [[label, "", "status", "n/a" if head_only else perf_metrics._status_cell(base), perf_metrics._status_cell(head), ""]]
    if head.blocked or head.failed:
        return rows, 0
    sides = (base, head)
    # A head-only set has no base counters; a blocked or failed base has none to read either.
    per_side = [{} if head_only or base.blocked or base.failed else _counter_phases(base), _counter_phases(head)]
    phase_names = list(per_side[1]) + [name for name in per_side[0] if name not in per_side[1]]
    omitted = 0
    for phase_name in phase_names:
        hover_active = _hover_group_active(phase_name, sides, per_side)
        for section, (counts, histograms) in perf_result_schema.FRAME_COUNTER_FIELDS.items():
            for field_name in counts + histograms:
                unit = None if field_name in counts else field_name.rsplit("_", 1)[1]
                cells = [_counter_cell(phases.get(phase_name, []), len(side.outcomes), section, field_name, unit)
                         for side, phases in zip(sides, per_side)]
                shown_zero = hover_active and section == "app" and field_name in perf_result_schema.HOVER_COUNTER_FIELDS
                if not shown_zero and not any(active for _text, _figure, active in cells):
                    omitted += 1
                    continue
                texts = [text if text is not None else ("n/a" if head_only else perf_metrics._missing_cell(side))
                         for (text, _figure, _active), side in zip(cells, sides)]
                rows.append([label, phase_name, f"{section}.{field_name} ({_counter_label(field_name, unit)})",
                             texts[0], texts[1],
                             perf_metrics.percent_change(cells[0][1], cells[1][1])])
        rows.extend(_attempt_split_rows(label, phase_name, sides, per_side, head_only))
        rows.extend(_derived_counter_rows(label, phase_name, sides, per_side, head_only))
    return rows, omitted


def _hover_group_active(phase_name: str, sides: Sequence[perf_metrics.SideRuns], per_side: Sequence[dict]) -> bool:
    """Whether any hover counter saw an event in `phase_name` on either side."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    return any(_counter_cell(phases.get(phase_name, []), len(side.outcomes), "app", field_name, None)[2]
               for side, phases in zip(sides, per_side) for field_name in perf_result_schema.HOVER_COUNTER_FIELDS)


# The only variant whose samples are split; S2/flood types beside a flood and is never armed.
SPLIT_LABEL = "S2/default"


def _side_latency_samples(side: perf_metrics.SideRuns) -> list[dict] | None:
    """Every latency sample of a side's valid runs, or None when the side has no valid run to read."""
    if side.blocked or side.failed or not side.outcomes:
        return None
    samples = []
    for outcome in side.outcomes:
        latency = (outcome.result or {}).get("latency")
        if isinstance(latency, dict) and isinstance(latency.get("samples"), list):
            samples.extend(sample for sample in latency["samples"] if isinstance(sample, dict))
    return samples


def _split_cells(samples: list[dict] | None, side: perf_metrics.SideRuns) -> dict[str, tuple[str, float | None]]:
    """One side's split rows: each row's cell and the figure its change compares."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    if samples is None:
        missing = "n/a" if side.blocked == COUNTERS_HEAD_ONLY else perf_metrics._missing_cell(side)
        return {"missing": (missing, None)}
    credited = [sample for sample in samples if sample.get("split_reason") not in (None, perf_latency_schema.NOT_CREDITED)]
    splits = [sample["split"] for sample in credited if isinstance(sample.get("split"), dict)]
    reasons: dict[str, int] = {}
    for sample in credited:
        reasons[sample["split_reason"]] = reasons.get(sample["split_reason"], 0) + 1
    flags = {name: sum(1 for split in splits if test(split)) for name, test in (
        ("suppressed", lambda split: split.get("delivery") == "suppressed"),
        ("coalesced", lambda split: split.get("coalesced") is True),
        ("sync_open", lambda split: split.get("sync_open") is True))}
    reason_text = ", ".join(f"{reason} {count}" for reason, count in sorted(reasons.items())) or "none"
    cells = {"reasons": (f"{reason_text}; " + ", ".join(f"{name} {count}" for name, count in flags.items()), None)}
    if not splits:
        # A base built without the feature splits nothing, so every credited sample reads unsupported. An empty
        # reason set is no such evidence: nothing credited is unavailable, and supported samples are 0% covered.
        unsupported = bool(reasons) and set(reasons) == {"unsupported"}
        empty = "n/a (unsupported)" if unsupported else "n/a (no split)"
        if unsupported:
            cells["coverage"] = (empty, None)
        elif credited:
            cells["coverage"] = (f"0.0% (0/{len(credited)})", 0.0)
        else:
            cells["coverage"] = ("unavailable", None)
        return {"empty": (empty, None), **cells}
    for part in perf_latency_schema.SPLIT_PARTS:
        values = [split[part] for split in splits]
        cells[f"{part} median"] = (f"{perf_statistics.median(values):.3f} ms ({len(values)} splits)", perf_statistics.median(values))
        cells[f"{part} p95"] = (f"{perf_statistics.percentile_95(values):.3f} ms", perf_statistics.percentile_95(values))
    lags = [split["delivery_lag_us"] for split in splits]
    cells["delivery_lag_us p95"] = (f"{perf_statistics.percentile_95(lags):.1f} us", perf_statistics.percentile_95(lags))
    coverage = len(splits) / len(credited)
    cells["coverage"] = (f"{coverage * 100:.1f}% ({len(splits)}/{len(credited)})", coverage * 100)
    return cells


# The split rows' labels, in order, and the key of each row's cell.
SPLIT_ROWS = (
    ("split input to parse, median (ms)", "input_to_parse_ms median"),
    ("split input to parse, p95 (ms)", "input_to_parse_ms p95"),
    ("split parse to publication, median (ms)", "parse_to_publication_ms median"),
    ("split parse to publication, p95 (ms)", "parse_to_publication_ms p95"),
    ("split publication to present, median (ms)", "publication_to_present_ms median"),
    ("split publication to present, p95 (ms)", "publication_to_present_ms p95"),
    ("split delivery lag, p95 (us)", "delivery_lag_us p95"),
    ("split coverage (%)", "coverage"),
    ("split reasons (credited samples)", "reasons"),
)


def split_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns, latency_split_schema: int | None) -> list[list[str]]:
    """The counters table's split rows for S2/default's typing phase, only when the head harness declares split
    schema 1. A side without the split reads `n/a (unsupported)`; a change compares the two figures."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    if label != SPLIT_LABEL or latency_split_schema != 1:
        return []
    sides = [_split_cells(_side_latency_samples(side), side) for side in (base, head)]
    rows = []
    for row_label, key in SPLIT_ROWS:
        texts, figures = [], []
        for cells in sides:
            text, figure = cells.get(key) or cells.get("empty") or cells["missing"]
            texts.append(text)
            figures.append(figure)
        rows.append([label, "typing", row_label, texts[0], texts[1], perf_metrics.percent_change(figures[0], figures[1])])
    return rows


# The scenarios whose credited samples carry an echo timeline: split_in_scope's variants.
TIMELINE_LABELS = ("S2/default", "S2/flood")
# The timeline rows' labels, in order, and each row's population.
TIMELINE_ROWS = (
    ("echo timeline recorded / credited", "recorded"),
    ("echo timeline ordered / credited", "ordered"),
    ("echo timeline B2 / ordered", "b2"),
    ("echo timeline B2 / credited", "b2_of_credited"),
    ("echo timeline M4 / ordered", "m4"),
    ("echo timeline M3 / ordered", "m3"),
    ("echo timeline M3 / credited", "m3_of_credited"),
    ("echo timeline read-stamped / ordered", "read_stamped"),
)


def _side_timeline(side: perf_metrics.SideRuns) -> tuple[str | None, dict | None]:
    """A side's recomputed timeline coverage over all its valid runs, or the cell that says why it has none."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    if side.blocked or side.failed or not side.outcomes:
        return ("n/a" if side.blocked == COUNTERS_HEAD_ONLY else perf_metrics._missing_cell(side)), None
    latencies = [(outcome.result or {}).get("latency") for outcome in side.outcomes]
    latencies = [latency for latency in latencies if isinstance(latency, dict)]
    if not any("echo_timeline_coverage" in latency for latency in latencies):
        return "n/a (no timeline)", None
    # Every credited sample counts: one without a timeline object joins the denominator and no population.
    timelines = [sample["echo_timeline"] if isinstance(sample.get("echo_timeline"), dict) else {}
                 for latency in latencies for sample in latency.get("samples") or []
                 if isinstance(sample, dict) and sample.get("latency_ms") is not None]
    coverage = perf_timeline_schema.timeline_coverage(timelines)
    if coverage is None:
        return "n/a (nothing credited)", None
    return None, coverage


def _timeline_cell(cell: str | None, coverage: dict | None, name: str) -> tuple[str, float | None]:
    """One population's cell, `numerator/denominator (percent)`, and its percentage; an unavailable side reads
    `unavailable`, which is not a measured zero."""
    if coverage is None:
        return cell or "n/a", None
    fraction = coverage.get(name)
    if fraction is None:
        return "unavailable", None
    numerator, denominator = fraction["numerator"], fraction["denominator"]
    if denominator == 0:
        return f"{numerator}/0 (n/a)", None
    percent = numerator / denominator * 100
    return f"{numerator}/{denominator} ({percent:.1f}%)", percent


def points_change(base_percent: float | None, head_percent: float | None) -> str:
    """The head's coverage less the base's, in percentage points, or `n/a` when either side has no figure."""
    if base_percent is None or head_percent is None:
        return "n/a"
    return f"{head_percent - base_percent:+.1f} pts"


def timeline_coverage_verdict(base: dict | None, head: dict | None) -> str:
    """The frozen two-sided coverage/gap gate on the ordered population: unavailable when either side recorded no
    timeline (a stub base, a cfg-off build), else pass or fail with both figures. Both thresholds compare integer
    cross-products, so a boundary such as 82/100 against 92/100 is exact."""
    unavailable = [name for name, coverage in (("base", base), ("head", head))
                   if not (isinstance(coverage, dict) and coverage.get("availability") == "available")]
    if unavailable:
        return f"unavailable ({', '.join(unavailable)} recorded no timeline)"
    (base_n, base_d), (head_n, head_d) = [(coverage["ordered"]["numerator"], coverage["ordered"]["denominator"])
                                          for coverage in (base, head)]
    gate_n, gate_d = perf_timeline_schema.TIMELINE_COVERAGE_GATE
    gap_n, gap_d = perf_timeline_schema.TIMELINE_COVERAGE_GAP
    # numerator/denominator >= gate_n/gate_d, and |base - head| <= gap_n/gap_d, with no division.
    above = all(numerator * gate_d >= denominator * gate_n for numerator, denominator in ((base_n, base_d),
                                                                                         (head_n, head_d)))
    within = gap_d * abs(base_n * head_d - head_n * base_d) <= gap_n * base_d * head_d
    passed = above and within
    figures = f"base {base_n / base_d * 100:.1f}%, head {head_n / head_d * 100:.1f}%"
    return f"{'pass' if passed else 'fail'} ({figures})"


def timeline_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[list[str]]:
    """The counters table's echo-timeline rows for S2's typing phase, when either side's harness wrote a timeline:
    each side's availability, every population's named numerator and denominator, and the two-sided verdict."""
    if label not in TIMELINE_LABELS:
        return []
    sides = [_side_timeline(side) for side in (base, head)]
    if all(coverage is None and cell == "n/a (no timeline)" for cell, coverage in sides):
        return []
    rows = [[label, "typing", "echo timeline availability",
             *[cell if coverage is None else coverage["availability"] for cell, coverage in sides], "n/a"]]
    for row_label, name in TIMELINE_ROWS:
        cells = [_timeline_cell(cell, coverage, name) for cell, coverage in sides]
        rows.append([label, "typing", row_label, cells[0][0], cells[1][0], points_change(cells[0][1], cells[1][1])])
    ordered = [_timeline_cell(cell, coverage, "ordered")[0] for cell, coverage in sides]
    verdict = timeline_coverage_verdict(sides[0][1], sides[1][1])
    rows.append([label, "typing", "echo timeline coverage gate (ordered >= 80%, gap <= 10 points)", *ordered, verdict])
    return rows


def _renderer_runs(per_run: Sequence[dict], fields: Sequence[str]) -> list[dict]:
    """The renderer sections of the runs that carry every integer field in `fields`."""
    return [sections["renderer"] for sections in per_run
            if isinstance(sections.get("renderer"), dict)
            and all(perf_latency_schema._is_int(sections["renderer"].get(field)) for field in fields)]


def _pooled_ratio(per_run: Sequence[dict], numerator: Sequence[str],
                  denominator: Sequence[str]) -> tuple[str | None, float | None]:
    """A pooled ratio over the runs carrying every field: the sums of `numerator` over the sums of
    `denominator`. None text when no run carries the fields; `n/a` when the pooled denominator is 0."""
    runs = _renderer_runs(per_run, list(numerator) + list(denominator))
    if not runs:
        return None, None
    top = sum(run[field] for run in runs for field in numerator)
    bottom = sum(run[field] for run in runs for field in denominator)
    scope = f"{len(runs)}/{len(per_run)} runs"
    if bottom == 0:
        return f"n/a (denominator 0, {scope})", None
    return f"{top / bottom:.3f} ({top}/{bottom}, {scope})", top / bottom


def _pooled_per_assembly(per_run: Sequence[dict], numerator: str) -> tuple[str | None, float | None]:
    """`numerator` per assembled frame, pooled over the runs carrying it and the assembly histogram: the
    sum of `numerator` over the sum of the histogram's sample counts. None text when no run carries both;
    `n/a` when no run assembled a frame."""
    runs = [sections["renderer"] for sections in per_run
            if isinstance(sections.get("renderer"), dict)
            and perf_latency_schema._is_int(sections["renderer"].get(numerator))
            and isinstance(sections["renderer"].get("assembly_us"), dict)]
    if not runs:
        return None, None
    top = sum(run[numerator] for run in runs)
    bottom = sum(sum(run["assembly_us"]["counts"]) for run in runs)
    scope = f"{len(runs)}/{len(per_run)} runs"
    if bottom == 0:
        return f"n/a (no assembly, {scope})", None
    return f"{top / bottom:.3f} ({top}/{bottom}, {scope})", top / bottom


def _assembly_means(per_run: Sequence[dict]) -> tuple[str | None, float | None]:
    """Each counters run's exact assembly mean, `assembly_sum_us / samples`, at its run position, and the
    pooled mean. A run without the histogram reads `n/a (no histogram)` and one that assembled nothing
    `n/a (no assembly)`, so every run keeps its place. None text when no run carries the histogram; the
    pooled figure is None when no run assembled a frame."""
    histograms = [sections["renderer"].get("assembly_us") if isinstance(sections.get("renderer"), dict) else None
                  for sections in per_run]
    if not any(isinstance(histogram, dict) for histogram in histograms):
        return None, None
    cells = []
    for position, histogram in enumerate(histograms, 1):
        if not isinstance(histogram, dict):
            cells.append(f"run {position} n/a (no histogram)")
        elif not sum(histogram["counts"]):
            cells.append(f"run {position} n/a (no assembly)")
        else:
            cells.append(f"run {position} {histogram['sum_us'] / sum(histogram['counts']):.2f} us")
    sampled = [histogram for histogram in histograms if isinstance(histogram, dict) and sum(histogram["counts"])]
    samples = sum(sum(histogram["counts"]) for histogram in sampled)
    if not samples:
        return f"{', '.join(cells)}; pooled n/a ({len(sampled)}/{len(per_run)} runs)", None
    pooled = sum(histogram["sum_us"] for histogram in sampled) / samples
    return f"{', '.join(cells)}; pooled {pooled:.2f} us ({len(sampled)}/{len(per_run)} runs)", pooled


# Derived rows: a label naming the formula and pooling, and the function computing one side's cell.
DERIVED_COUNTER_ROWS = (
    ("row-cache hit ratio = hits / (hits + misses), counters runs pooled",
     lambda per_run: _pooled_ratio(per_run, ("row_cache_hits",), ("row_cache_hits", "row_cache_misses"))),
    ("assembly mean per counters run = assembly_sum_us / Σ assembly_buckets, per run", _assembly_means),
    ("shape+measure requests per drawn frame = shape_requests / (gpu_frames + software_frames), context only",
     lambda per_run: _pooled_ratio(per_run, ("shape_requests",), ("gpu_frames", "software_frames"))),
    ("partial fallback ratio = partial_fallbacks / (partial_frames + partial_fallbacks), context only",
     lambda per_run: _pooled_ratio(per_run, ("partial_fallbacks",), ("partial_frames", "partial_fallbacks"))),
    ("tab-title reuses per assembly = tab_title_reuses / Σ assembly_buckets, counters runs pooled",
     lambda per_run: _pooled_per_assembly(per_run, "tab_title_reuses")),
    ("chrome-run reuses per assembly = chrome_run_reuses / Σ assembly_buckets, counters runs pooled",
     lambda per_run: _pooled_per_assembly(per_run, "chrome_run_reuses")),
)


def _derived_counter_rows(label: str, phase_name: str, sides: Sequence[perf_metrics.SideRuns], per_side: Sequence[dict],
                          head_only: bool) -> list[list[str]]:
    """One phase's derived rows, each printed only when some side has a nonzero denominator; a side whose runs
    lack the fields, or whose denominator is 0, reads `n/a`, and the change compares the two figures."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    rows = []
    for row_label, compute in DERIVED_COUNTER_ROWS:
        cells = [compute(phases.get(phase_name, [])) for phases in per_side]
        if all(figure is None for _text, figure in cells):
            continue
        texts = [text if text is not None else ("n/a" if head_only else perf_metrics._missing_cell(side))
                 for (text, _figure), side in zip(cells, sides)]
        rows.append([label, phase_name, row_label, texts[0], texts[1], perf_metrics.percent_change(cells[0][1], cells[1][1])])
    return rows


def _attempt_split_rows(label: str, phase_name: str, sides: Sequence[perf_metrics.SideRuns], per_side: Sequence[dict],
                        head_only: bool) -> list[list[str]]:
    """The pooled render-attempt split rows of one phase, all attempts then apply attempts. A phase where no
    side drew an attempt reads as one row saying so; none when no side carries the fields. A side whose runs
    lack the fields reads `n/a`."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    splits = {prefix: [attempt_split(phases.get(phase_name, []), prefix) for phases in per_side]
              for prefix in ("render_", "apply_")}
    if all(text is None for text, _attempts in splits["render_"]):
        return []
    if not any(attempts for _text, attempts in splits["render_"]):
        # No side drew an attempt: one explicit row, rather than an omitted phase or two empty rows.
        texts = [text if text is not None else ("n/a" if head_only else perf_metrics._missing_cell(side))
                 for (text, _attempts), side in zip(splits["render_"], sides)]
        return [[label, phase_name, "renderer attempt split (pooled)", texts[0], texts[1], ""]]
    rows = []
    for prefix, name in (("render_", "every attempt"), ("apply_", "fallback apply attempts")):
        texts = [text if text is not None else ("n/a" if head_only else perf_metrics._missing_cell(side))
                 for (text, _attempts), side in zip(splits[prefix], sides)]
        rows.append([label, phase_name, f"renderer attempt split: {name} (pooled)", texts[0], texts[1], ""])
    return rows


def attempt_split_details(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[str]:
    """Each run's own split, for every phase in which any run on either side drew a render attempt, so both
    pooled rows' whole populations can be read run by run: every run of that phase is listed, including runs
    that applied nothing and runs that lack the fields (`n/a`). Lines for the details block."""
    sides = (("base", base), ("head", head))
    applying = {name for _side_name, side in sides for name, phases in _counter_phases(side).items()
                if any(attempt_split([counters], "render_")[1] for counters in phases)}
    lines = []
    for side_name, side in sides:
        for index, outcome in enumerate(side.outcomes, 1):
            for phase in (outcome.result or {}).get("phases") or []:
                if str(phase.get("name")) not in applying:
                    continue
                counters = phase.get("frame_counters")
                texts = [attempt_split([counters], prefix)[0] if isinstance(counters, dict) else None
                         for prefix in ("render_", "apply_")]
                every_text, apply_text = (text or "n/a" for text in texts)
                lines.append(f"- {label} {side_name} run {index} {phase.get('name')}: every attempt {every_text}; "
                             f"fallback apply attempts {apply_text}")
    return lines
