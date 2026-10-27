"""Perf attribution report."""

from __future__ import annotations
from collections import Counter
from dataclasses import asdict, dataclass, field
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_attribution_schema
import perf_latency_schema
import perf_memory
import perf_metrics
import perf_sets
import perf_statistics


def atlas_recovery_rows(label: str, base: perf_metrics.SideRuns, head: perf_metrics.SideRuns) -> list[list[str]]:
    """The atlas retry recovery table for S1/atlas-retry: one row per frame A-D and a sequence total, each
    cell summed over every episode of every accepted counters run, with the run and episode counts and each
    side's distinct row keys in the first row. Empty for any other label or when no run recorded episodes."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    if tuple(label.split("/", 1)) not in perf_result_schema.COUNTERS_ONLY_VARIANTS:
        return []
    per_side = []
    for side in (base, head):
        recoveries = [outcome.result["atlas_recovery"] for outcome in side.outcomes
                      if isinstance((outcome.result or {}).get("atlas_recovery"), dict)]
        per_side.append(recoveries)
    if not any(per_side):
        return []

    def cell(recoveries: list, frames: Sequence[str]) -> str:
        if not recoveries:
            return "n/a"
        sums = {name: sum(record[name] for recovery in recoveries for record in recovery["records"]
                          if record["frame"] in frames)
                for name in ("misses", "hits", "shapes", "attempts")}
        return (f"misses {sums['misses']}, hits {sums['hits']}, shapes {sums['shapes']}, "
                f"attempts {sums['attempts']}")

    def header(recoveries: list) -> str:
        if not recoveries:
            return "n/a"
        keys = sorted({recovery["distinct_keys"] for recovery in recoveries})
        episodes = sum(recovery["episodes"] for recovery in recoveries)
        return f"{len(recoveries)} runs, {episodes} episodes, distinct keys {', '.join(map(str, keys))}"

    rows = [["runs", header(per_side[0]), header(per_side[1])]]
    for frame in perf_result_schema.ATLAS_RECOVERY_FRAMES:
        rows.append([frame, cell(per_side[0], (frame,)), cell(per_side[1], (frame,))])
    rows.append(["A-D total", cell(per_side[0], perf_result_schema.ATLAS_RECOVERY_FRAMES), cell(per_side[1], perf_result_schema.ATLAS_RECOVERY_FRAMES)])
    return rows


# --- S10 attribution: each counters run's presents against the updates they show ----------------------------

ATTRIBUTION_FILE = "attribution.json"
# The harness API cfg (local-gate.py HARNESS_API_CFGS) whose decision lets the harness arm the App's watch.
ATTRIBUTION_CFG = "perf_s10_attribution_api"
# The harness-only cfg that lets the harness take the App's guard-correlation records.
GUARD_SPANS_CFG = "perf_guard_spans_api"
ATTRIBUTION_HEADER = ("| Scenario | Side | Runs | State | Fresh − updates | Presents per update 0/1/2/≥3 | Max | "
                      "Non-update (pre/sentinel/prompt/both) | Never shown | Open-update | Cached | Verdict |\n"
                      "|---|---|---|---|---|---|---|---|---|---|---|---|")
ATTRIBUTION_NOTE = (
    "Each presented frame of an S10 counters run is attributed to the update its guarded parser state shows: "
    "update k once k resets followed the baseline read before GO with the update closed. A run is complete only "
    "when every presented frame of the phase has exactly one line, no line is malformed or overflowed, no frame "
    "shows an open update, the fixture played exactly its updates and the identity fresh − updates = non-update − "
    "never shown + Σ max(0, presents − 1) holds; any other run is incomplete, never passed. A build without the "
    "attribution cfg, an App that armed nothing and a non-counting run are unavailable. A counters excess explains "
    "the timed excess only as reproduction under instrumentation, never as its timed cause. S10/default has no "
    "update identity, so it reports marker classes only.")


@dataclass
class AttributionRun:
    """One counters run's S10 attribution: its state, the reasons it is not complete, and its counts."""

    state: str
    reasons: list[str] = field(default_factory=list)
    updates: int = 0
    fresh: int = 0
    cached: int | None = None
    buckets: tuple[int, int, int, int] = (0, 0, 0, 0)
    max_presents: int = 0
    non_update: dict[str, int] = field(default_factory=dict)
    never_shown: int = 0
    open_updates: int = 0
    admissions: dict[str, int] = field(default_factory=dict)
    identity_holds: bool | None = None


def _counted_phase(result: dict | None) -> dict | None:
    """The phase that plays counted updates, or None."""
    for phase in (result or {}).get("phases") or []:
        if isinstance(phase, dict) and perf_latency_schema._is_int(phase.get("updates")):
            return phase
    return None


def _window_count(phase: dict, name: str) -> int | None:
    """A window counter of the phase's frame-counter delta, or None when the run did not count it."""
    counters = phase.get("frame_counters")
    window = counters.get("window") if isinstance(counters, dict) else None
    value = window.get(name) if isinstance(window, dict) else None
    return value if perf_latency_schema._is_int(value) else None


def _marker_class(line: perf_memory.PresentLine) -> str | None:
    """A present's post-workload class from its guarded markers, or None when it shows neither."""
    if line.sentinel and line.prompt:
        return "sentinel+prompt"
    if line.sentinel:
        return "sentinel"
    return "prompt" if line.prompt else None


def attribution_run(outcome, synchronized: bool) -> AttributionRun:
    """Classify one counters run. `synchronized` is S10/sync, whose updates carry an identity; S10/default reports
    marker classes and admissions only and reads `unsupported`."""
    phase = _counted_phase(outcome.result)
    record = phase.get("s10_attribution") if phase else None
    if not isinstance(record, dict) or record.get("state") != "armed":
        reason = record.get("reason") if isinstance(record, dict) else "no-record"
        return AttributionRun("unavailable", [str(reason)])
    if not perf_attribution_schema._armed_record_shaped(record):
        return AttributionRun("incomplete", ["the armed record is malformed"])
    updates, arming, pane = record["updates"], record["arming"], record["pane"]
    seq_start, seq_end = record["seq_start"], record.get("seq_end")
    run = AttributionRun("incomplete", updates=updates, cached=_window_count(phase, "cached"))
    reasons = run.reasons
    reasons.extend(perf_attribution_schema.armed_record_contradictions(record))
    if seq_end is None or seq_end < seq_start:
        reasons.append(f"the phase's presented count is unknown ({seq_start}..{seq_end!r})")
        seq_end = seq_start
    run.fresh = seq_end - seq_start
    # The sequence range and the phase's fresh presents count the main window's presents; the counters' presented
    # delta (`snapshot_totals` in counters.rs) sums every live and closed window. They agree only because S10 runs
    # one window, so another window's presents break the equality and the run reads incomplete, never complete.
    for label, count in (("presented frames", phase.get("presented_frames")),
                         ("counters' presented delta", _window_count(phase, "presented"))):
        if count != run.fresh:
            reasons.append(f"the phase's {label} {count!r} are not its {run.fresh} sequenced frame(s)")
    lines = [line for line in outcome.present_lines if line.arming == arming]
    if any(line.problem for line in lines) or any(line.problem for line in outcome.present_lines):
        reasons.append("a malformed perf_present line")
    if any(line.kind == "overflow" for line in lines):
        reasons.append("the watch overflowed")
    if any(line.pane != pane for line in lines if line.problem is None):
        reasons.append("a line names another pane")
    window = sorted((line for line in lines if line.kind == "present" and line.problem is None
                     and line.pane == pane and seq_start < line.seq <= seq_end), key=lambda line: line.seq)
    per_seq = Counter(line.seq for line in window)
    gaps = sum(1 for seq in range(seq_start + 1, seq_end + 1) if seq not in per_seq)
    repeats = sum(count - 1 for count in per_seq.values() if count > 1)
    if gaps or repeats:
        reasons.append(f"{gaps} presented frame(s) have no line and {repeats} have more than one")
    run.admissions = dict(Counter(line.admission for line in window))
    baseline, end = record["baseline"], record.get("end")
    if end is None:
        reasons.append("the pane's state at the phase end was not read")
    # The pane's epoch and resets only grow: from the baseline, through every line in sequence order, to the
    # final reading.
    epoch, resets = baseline["epoch"], baseline["resets"]
    for line in window:
        if line.epoch < epoch:
            reasons.append("the update epoch decreased")
        if line.resets < resets:
            reasons.append("the pane's resets decreased")
        epoch, resets = max(epoch, line.epoch), max(resets, line.resets)
    if end is not None and (end["epoch"] < epoch or end["resets"] < resets):
        reasons.append("the final reading is below a presented line's state")
    if not synchronized:
        # S10/default has no update identity, but its transport is still complete or not.
        run.non_update = dict(Counter(_marker_class(line) or "stream" for line in window))
        run.reasons = list(dict.fromkeys(reasons))[:5]
        run.state = "incomplete" if run.reasons else "unsupported"
        return run
    if end is not None and end["resets"] - baseline["resets"] != updates:
        reasons.append(f"the fixture played {end['resets'] - baseline['resets']} updates, not {updates}")
    per_update: Counter = Counter()
    non_update: Counter = Counter()
    open_admissions: Counter = Counter()
    for line in window:
        if line.set:
            open_admissions[line.admission] += 1
            continue
        offset = line.resets - baseline["resets"]
        if offset < 0 or offset > updates:
            reasons.append(f"a line's resets {line.resets} lie outside the baseline's {updates} updates")
        elif offset == 0:
            non_update["pre-workload"] += 1
        elif offset == updates and _marker_class(line) is not None:
            non_update[_marker_class(line)] += 1
        else:
            per_update[offset] += 1
    run.open_updates = sum(open_admissions.values())
    if run.open_updates:
        named = ", ".join(f"{name} {count}" for name, count in sorted(open_admissions.items()))
        reasons.append(f"{run.open_updates} presented frame(s) showed an open update ({named})")
    counts = [per_update.get(update, 0) for update in range(1, updates + 1)]
    run.buckets = (counts.count(0), counts.count(1), counts.count(2), sum(1 for count in counts if count >= 3))
    run.max_presents = max(counts, default=0)
    run.never_shown = run.buckets[0]
    run.non_update = dict(non_update)
    excess = sum(max(0, count - 1) for count in counts)
    run.identity_holds = run.fresh - updates == sum(non_update.values()) - run.never_shown + excess
    if not run.identity_holds and not reasons:
        reasons.append("fresh − updates does not equal non-update − never shown + repeats")
    run.reasons = list(dict.fromkeys(reasons))[:5]
    run.state = "complete" if not run.reasons else "incomplete"
    return run


def timed_excess(side: perf_metrics.SideRuns) -> list[int]:
    """Each timed run's presented frames minus updates in its counted phase."""
    excess = []
    for outcome in side.outcomes:
        phase = _counted_phase(outcome.result)
        if phase is not None and perf_latency_schema._is_int(phase.get("presented_frames")):
            excess.append(phase["presented_frames"] - phase["updates"])
    return excess


def attribution_verdict(runs: Sequence[AttributionRun], synchronized: bool, timed: Sequence[int]) -> str:
    """One side's #1598 verdict on this platform. Only complete runs decide; the evidence boundary needs the
    counters runs' excess within ±1 of the same workflow's timed median, and even then it attributes the timed
    excess under instrumentation only."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_counter_rows
    if not runs:
        return "pending: no counters run"
    unavailable = [run for run in runs if run.state == "unavailable"]
    if unavailable:
        return f"unavailable: {unavailable[0].reasons[0] if unavailable[0].reasons else 'no record'}"
    incomplete = [run for run in runs if run.state == "incomplete"]
    if incomplete:
        return f"pending: {len(incomplete)} of {len(runs)} run(s) incomplete ({incomplete[0].reasons[0]})"
    if not synchronized:
        return "unsupported: S10/default has no update identity"
    if any(run.state != "complete" for run in runs):
        return "pending: a run is neither complete nor incomplete"
    if max(run.max_presents for run in runs) >= 2:
        return ("repeat-presentation: an update was presented 2 or more times; reproduce it with the "
                "windows_synchronized_output.rs pattern and fix it in a separate PR")
    if not timed:
        return "pending: no timed run to bound the evidence"
    timed_median = perf_statistics.median(timed)
    excesses = [run.fresh - run.updates for run in runs]
    if all(abs(excess - timed_median) <= 1 for excess in excesses):
        return (f"ratio holds on this platform: every update presented at most once; the timed excess "
                f"({perf_counter_rows._figure(timed_median)}) is attributed under instrumentation only")
    return (f"pending: the counters excess {excesses} is not within ±1 of the timed median "
            f"{perf_counter_rows._figure(timed_median)}, so the timed excess stays unresolved")


def attribution_report(label: str, counters: perf_sets.SetResult, timed: perf_sets.SetResult | None,
                       comparable: bool) -> tuple[list[list[str]], dict]:
    """The S10 attribution rows and evidence of one counters set, per side. An unequal-feature comparison gives
    rows but no verdict."""
    synchronized = label.endswith("/sync")
    rows: list[list[str]] = []
    evidence: dict[str, dict] = {}
    for side_name in ("base", "head"):
        side = getattr(counters, side_name)
        runs = [attribution_run(outcome, synchronized) for outcome in side.outcomes]
        timed_runs = timed_excess(getattr(timed, side_name)) if timed is not None else []
        verdict = (attribution_verdict(runs, synchronized, timed_runs) if comparable
                   else "no verdict: the two refs build different perf features")
        if side.blocked or side.failed:
            verdict = f"unavailable: {side.blocked or side.failed}"
        states = Counter(run.state for run in runs)
        non_update = Counter()
        for run in runs:
            non_update.update(run.non_update)
        rows.append([
            label, side_name, str(len(runs)),
            ", ".join(f"{state} {count}" for state, count in sorted(states.items())) or "n/a",
            ", ".join(str(run.fresh - run.updates) for run in runs) or "n/a",
            " · ".join("/".join(str(value) for value in run.buckets) for run in runs) or "n/a",
            str(max((run.max_presents for run in runs), default=0)),
            "/".join(str(non_update.get(name, 0)) for name in ("pre-workload", "sentinel", "prompt",
                                                              "sentinel+prompt")),
            ", ".join(str(run.never_shown) for run in runs) or "n/a",
            ", ".join(str(run.open_updates) for run in runs) or "n/a",
            ", ".join("n/a" if run.cached is None else str(run.cached) for run in runs) or "n/a",
            verdict])
        evidence[side_name] = {"runs": [asdict(run) for run in runs], "timed_excess": timed_runs,
                               "verdict": verdict}
    return rows, evidence
