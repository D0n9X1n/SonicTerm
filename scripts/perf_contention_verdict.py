"""S13's contention verdict: whether a 2x2 grid with one sustained flood keeps the idle pane's typing as
responsive as two side-by-side panes, read from each variant's counters runs.

The rule was fixed before any CI run measured S13:

- Each variant needs at least `MIN_SAMPLES` credited typing samples, every run's window counters
  (the miss attribution included), and the flood alive throughout `typing` and `tail`: the harness
  observes the flooding pane's own scrolled-line count every 100 ms of `typing` and `tail`, and in
  each phase, from its start to its end, no stretch may pass `max_flood_gap_ms` without a reading
  that advanced: an unobserved prefix or suffix, busy-only looks and a stall all count (`vt.parse_bytes`
  of at least `MIN_FLOOD_BYTES` per phase is a second, coarse floor). The flooding and typing panes
  of both variants must have the same grid within `SPLIT_ROUNDING_CELLS` per axis, the rounding of a
  split, so pane count is the only difference.
  Otherwise there is no verdict.
- `unchanged` holds when the 4-pane p95 is within `P95_TOLERANCE` of the 2-pane p95, and every failed
  collection in the 4-pane variant (parser, image and budget-expired misses together) is under
  `FAILURE_LIMIT` of its collections. Otherwise the verdict is `not unchanged`, which asks for the
  measured mechanism to be investigated.

Collections are counted by `parser_lock_wait_us`, which every collection records, hit or miss, so the
failure fraction cannot be diluted or inflated by how a miss is retried.
"""

from __future__ import annotations

import math
from typing import Iterable, Sequence

import perf_latency_schema

SCENARIO = "S13"
TWO_PANES = "S13/default"
FOUR_PANES = "S13/sustained-4"
# The release-only equal grids, in doubling order, with each layout's pane count. Every layout asks
# for 30x5 panes; a host may size the window differently (a macOS host gave 31x3), so the rule needs
# every pane of every layout and run to match the others, which keeps the per-pane workload equal.
WIDE = ("S13/wide-8", "S13/wide-16", "S13/wide-32")
WIDE_PANES = (8, 16, 32)
SCALES = "scales"
DOES_NOT_SCALE = "does not scale"
MIN_SAMPLES = 200
P95_TOLERANCE = 0.10
FAILURE_LIMIT = 0.01
# A stopped flood leaves the busy parser idle and the comparison meaningless; a live `yes` writes many
# megabytes in a 5 s hold, so 1 MiB per phase is a floor, not an estimate of throughput.
MIN_FLOOD_BYTES = 1 << 20
# Stacked panes in a window whose height does not divide evenly differ by at most this many cells.
SPLIT_ROUNDING_CELLS = 1
FAILURE_FIELDS = ("contention_parser", "contention_images", "contention_budget_expired")
LIVENESS_PHASES = ("typing", "tail")
UNCHANGED = "unchanged"
NOT_UNCHANGED = "not unchanged"
NO_VERDICT = "no verdict"
COLUMNS = ("Variant", "Samples", "p95 (ms)", "Collections", "Failed (parser/images/budget)", "Failed %",
           "Flood bytes typing/tail (min)", "VT lock wait p95", "VT parse p95")
# The microsecond histogram bucket bounds the VT parser's counters use, as `perf_result_schema` declares.
US_BOUNDS = (10, 50, 100, 500, 1000, 5000)
HEADER = "| " + " | ".join(COLUMNS) + " |\n|" + "---|" * len(COLUMNS) + "\n"
NOTE = (f"Head counters runs only. Verdict rule, fixed before CI: at least {MIN_SAMPLES} samples per variant; "
        f"`{UNCHANGED}` when the 4-pane p95 is within {P95_TOLERANCE:.0%} of the 2-pane p95 and all failed "
        f"collections (parser, image and budget-expired) are under {FAILURE_LIMIT:.0%} of collections in the "
        f"4-pane variant; a missing attribution, too few samples or a stopped flood gives `{NO_VERDICT}`. "
        f"A release run adds the wide layouts: `{SCALES}` unless one fails {FAILURE_LIMIT:.0%} or more of its "
        f"collections or its p95 grows more than {P95_TOLERANCE:.0%} per doubling; this is reported, not a gate.")


def percentile_95(values: Sequence[float]) -> float:
    """The nearest-rank 95th percentile."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(0.95 * len(ordered)) - 1)]


def _phase(result: dict, name: str) -> dict | None:
    for phase in result.get("phases") or []:
        if isinstance(phase, dict) and phase.get("name") == name:
            return phase
    return None


def _histogram_p95(histograms: Sequence[dict]) -> str:
    """The pooled histograms' nearest-rank p95 as a bucket bound in µs, `n/a` when none was recorded (a base
    older than the field) or no event fell in any bucket."""
    pooled = [0] * (len(US_BOUNDS) + 1)
    for histogram in histograms:
        counts = histogram.get("counts") if isinstance(histogram, dict) else None
        if isinstance(counts, list) and len(counts) == len(pooled):
            pooled = [total + count for total, count in zip(pooled, counts)]
    events = sum(pooled)
    if events == 0:
        return "n/a"
    rank, running = (95 * events + 99) // 100, 0
    for index, count in enumerate(pooled):
        running += count
        if running >= rank:
            return f"≤{US_BOUNDS[index]} µs" if index < len(US_BOUNDS) else f">{US_BOUNDS[-1]} µs"
    return "n/a"


def variant_evidence(results: Iterable[dict]) -> dict:
    """Pool one variant's counters runs: its typing samples, collections, failures and flood liveness.

    `problems` names everything that keeps the variant from a verdict.
    """
    samples: list[float] = []
    collections = 0
    failures = {name: 0 for name in FAILURE_FIELDS}
    flood_bytes = {name: [] for name in LIVENESS_PHASES}
    pane_grids: list[tuple[tuple, tuple]] = []
    all_grids: list[list[tuple]] = []
    # Reported, never part of the verdict: the typing phase's VT histograms.
    lock_waits: list[dict] = []
    parses: list[dict] = []
    problems: list[str] = []
    results = list(results)
    if not results:
        problems.append("no counters run")
    for index, result in enumerate(results, start=1):
        values = perf_latency_schema.latency_values(result.get("latency"))
        if values is None:
            problems.append(f"run {index} has no readable latency")
        else:
            samples.extend(values)
        typing = _phase(result, "typing")
        vt = ((typing or {}).get("frame_counters") or {}).get("vt") or {}
        lock_waits.append(vt.get("parser_lock_wait_us"))
        parses.append(vt.get("parse_us"))
        window = ((typing or {}).get("frame_counters") or {}).get("window")
        if not isinstance(window, dict) or any(not isinstance(window.get(name), int) for name in FAILURE_FIELDS):
            problems.append(f"run {index} lacks the typing phase's miss attribution")
        else:
            histogram = window.get("parser_lock_wait_us")
            counts = histogram.get("counts") if isinstance(histogram, dict) else None
            if not isinstance(counts, list):
                problems.append(f"run {index} lacks the typing phase's parser_lock_wait_us")
            else:
                collections += sum(counts)
                for name in FAILURE_FIELDS:
                    failures[name] += window[name]
        for name in LIVENESS_PHASES:
            phase = _phase(result, name)
            parsed = (((phase or {}).get("frame_counters") or {}).get("vt") or {}).get("parse_bytes")
            if not isinstance(parsed, int):
                problems.append(f"run {index} lacks {name}'s vt.parse_bytes")
            else:
                flood_bytes[name].append(parsed)
                if parsed < MIN_FLOOD_BYTES:
                    problems.append(f"run {index}'s flood stopped: {name} parsed {parsed} bytes")
        contention = result.get("contention")
        flood = contention.get("flood") if isinstance(contention, dict) else None
        limit = (contention or {}).get("max_flood_gap_ms")
        watched = {phase.get("name"): phase for phase in (flood or {}).get("phases") or []
                   if isinstance(phase, dict)}
        if not isinstance(limit, int):
            problems.append(f"run {index} has no flood progress record")
        for name in LIVENESS_PHASES:
            phase = watched.get(name)
            gap = (phase or {}).get("longest_gap_ms")
            if not isinstance(gap, int) or not isinstance((phase or {}).get("duration_ms"), int):
                # Every measured phase must have been watched from its start to its end.
                problems.append(f"run {index} did not watch the flood through {name}")
            elif isinstance(limit, int) and gap > limit:
                # The gap spans the phase's start to its end: unobserved, busy-only and stalled
                # stretches all count, so a missing prefix or suffix fails like a stall.
                problems.append(f"run {index}'s flood showed no progress for {gap} ms in {name} (limit {limit} ms)")
        grids = (contention or {}).get("role_grids")
        if isinstance(grids, list) and len(grids) >= 2 and all(isinstance(grid, list) for grid in grids):
            pane_grids.append((tuple(grids[0]), tuple(grids[1])))
            all_grids.append([tuple(grid) for grid in grids])
        else:
            problems.append(f"run {index} has no flooding and typing pane grids")
    if len(samples) < MIN_SAMPLES:
        problems.append(f"{len(samples)} samples, below {MIN_SAMPLES}")
    return {"samples": samples, "collections": collections, "failures": failures, "flood_bytes": flood_bytes,
            "pane_grids": pane_grids, "all_grids": all_grids, "problems": problems,
            "lock_wait_p95": _histogram_p95([value for value in lock_waits if value]),
            "parse_p95": _histogram_p95([value for value in parses if value])}


def verdict(two: dict, four: dict) -> tuple[str, str]:
    """The rule's verdict on the pooled evidence, and why."""
    problems = [f"{TWO_PANES}: {problem}" for problem in two["problems"]]
    problems += [f"{FOUR_PANES}: {problem}" for problem in four["problems"]]
    if not problems and four["collections"] == 0:
        problems.append(f"{FOUR_PANES}: no collection was recorded")
    # Pane count must be the only difference: every run's flooding and typing panes share one grid,
    # within a cell of rounding at the split.
    grids = [grid for evidence in (two, four) for pair in evidence["pane_grids"] for grid in pair]
    # One cell per axis is split rounding: a window whose height does not divide evenly gives stacked
    # panes rows that differ by one (a macOS runner gave 8 and 7 in a 17-row window). Anything more is a
    # different workload and leaves no verdict.
    if not problems and grids and any(abs(grid[axis] - other[axis]) > SPLIT_ROUNDING_CELLS
                                      for grid in grids for other in grids for axis in (0, 1)):
        problems.append(f"pane grids differ: {sorted(set(grids))}")
    if problems:
        return NO_VERDICT, "; ".join(problems)
    two_p95, four_p95 = percentile_95(two["samples"]), percentile_95(four["samples"])
    failed = sum(four["failures"].values())
    fraction = failed / four["collections"]
    reasons = []
    if four_p95 > two_p95 * (1 + P95_TOLERANCE):
        reasons.append(f"4-pane p95 {four_p95:.1f} ms exceeds 2-pane {two_p95:.1f} ms by more than "
                       f"{P95_TOLERANCE:.0%}")
    if fraction >= FAILURE_LIMIT:
        reasons.append(f"{failed} of {four['collections']} 4-pane collections failed "
                       f"({fraction:.1%}, limit {FAILURE_LIMIT:.0%})")
    if reasons:
        return NOT_UNCHANGED, "; ".join(reasons)
    return UNCHANGED, (f"4-pane p95 {four_p95:.1f} ms vs 2-pane {two_p95:.1f} ms; {failed} of "
                       f"{four['collections']} collections failed ({fraction:.2%})")


def _row(label: str, evidence: dict) -> list[str]:
    samples = evidence["samples"]
    failures = evidence["failures"]
    collections = evidence["collections"]
    failed = sum(failures.values())
    minimum = [str(min(values)) if values else "n/a" for values in evidence["flood_bytes"].values()]
    return [label, str(len(samples)), f"{percentile_95(samples):.1f}" if samples else "n/a", str(collections),
            "/".join(str(failures[name]) for name in FAILURE_FIELDS), f"{failed / collections:.2%}" if collections
            else "n/a", "/".join(minimum), evidence["lock_wait_p95"], evidence["parse_p95"]]


def scaling_verdict(wide: Sequence[dict]) -> tuple[str, str]:
    """The release rule on the wide layouts, in doubling order: each needs the evidence a variant
    needs and equal panes (one cell of rounding per axis); it `scales` unless a layout's failed
    collections reach `FAILURE_LIMIT` or its p95 grows more than `P95_TOLERANCE` over the layout half
    its size. A failure is reported, never a release gate: it asks for whole-frame collection to be
    revisited."""
    problems = [f"{label}: {problem}" for label, evidence in zip(WIDE, wide) for problem in evidence["problems"]]
    for label, evidence, panes in zip(WIDE, wide, WIDE_PANES):
        for grids in evidence["all_grids"]:
            if len(grids) != panes:
                problems.append(f"{label}: {len(grids)} panes, not {panes}")
        if not evidence["problems"] and evidence["collections"] == 0:
            problems.append(f"{label}: no collection was recorded")
    # One per-pane workload across every layout and run: pane count is the only thing that doubles.
    every_grid = [grid for evidence in wide for grids in evidence["all_grids"] for grid in grids]
    if every_grid and any(abs(grid[axis] - other[axis]) > SPLIT_ROUNDING_CELLS
                          for grid in every_grid for other in every_grid for axis in (0, 1)):
        problems.append(f"pane grids differ across the layouts: {sorted(set(every_grid))}")
    if problems:
        return NO_VERDICT, "; ".join(problems)
    reasons = []
    for label, evidence in zip(WIDE, wide):
        fraction = sum(evidence["failures"].values()) / evidence["collections"]
        if fraction >= FAILURE_LIMIT:
            reasons.append(f"{label}: {fraction:.1%} of collections failed (limit {FAILURE_LIMIT:.0%})")
    p95s = [percentile_95(evidence["samples"]) for evidence in wide]
    for (smaller, larger), (before, after) in zip(zip(WIDE, WIDE[1:]), zip(p95s, p95s[1:])):
        if after > before * (1 + P95_TOLERANCE):
            reasons.append(f"{larger} p95 {after:.1f} ms grew more than {P95_TOLERANCE:.0%} over {smaller} "
                           f"{before:.1f} ms")
    if reasons:
        return DOES_NOT_SCALE, "; ".join(reasons)
    return SCALES, "p95 " + " / ".join(f"{value:.1f}" for value in p95s) + " ms across 8, 16 and 32 panes"


def verdict_rows(heads: dict[str, Sequence[dict]]) -> list[list[str]]:
    """The S13 section's rows from each variant's head counters results; empty when S13 did not run.
    The wide layouts, which only a release run measures, add their rows and the scaling rule."""
    rows = []
    if TWO_PANES in heads or FOUR_PANES in heads:
        two = variant_evidence(heads.get(TWO_PANES, ()))
        four = variant_evidence(heads.get(FOUR_PANES, ()))
        outcome, why = verdict(two, four)
        rows += [_row(TWO_PANES, two), _row(FOUR_PANES, four), ["Verdict", outcome, why] + [""] * (len(COLUMNS) - 3)]
    if any(label in heads for label in WIDE):
        wide = [variant_evidence(heads.get(label, ())) for label in WIDE]
        outcome, why = scaling_verdict(wide)
        rows += [_row(label, evidence) for label, evidence in zip(WIDE, wide)]
        rows.append(["Scaling", outcome, why] + [""] * (len(COLUMNS) - 3))
    return rows
