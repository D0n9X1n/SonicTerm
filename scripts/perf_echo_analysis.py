"""Perf echo analysis."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
from fractions import Fraction
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_latency_schema
import perf_metrics
import perf_timeline_schema


# --- S2/flood echo attribution (#1607 plan v6.1 §4) ------------------------------------------

# The seven additive parts of an ordered sample's credited latency, spelled as the frozen schema 1 spells them.
# v6.1's `publication_to_service` and `service_to_entry` are v5 V2's `publication_to_frame_request` and
# `frame_request_to_entry`.
ECHO_PARTS = perf_timeline_schema.TIMELINE_PART_NAMES
# §4 step 3: the primary part each mechanism screens. M3 and M4 share frame_request_to_entry and are told apart by
# the flood count and the permit's absence.
ECHO_MECHANISM_PARTS = {"M1": "render", "M2": "publication_to_frame_request", "M3": "frame_request_to_entry",
                        "M4": "frame_request_to_entry"}
ECHO_MECHANISM_NAMES = {"M1": "renderer work", "M2": "upstream readiness", "M3": "flood competition",
                        "M4": "pacing"}
# §4 step 4 and v5 V5: the controls that can confirm each mechanism. C5 is broad, so M3 is only ever supported.
ECHO_MECHANISM_CONTROLS = {"M1": ("C1", "C2", "C3"), "M2": ("C1", "C2", "C3"), "M3": ("C5",), "M4": ("C4",)}
ECHO_CONTROL_NAMES = ("C1", "C2", "C3", "C4", "C5")
# Ruling D1 keeps control identity (v5 V3's `sonic::perf_control` check) in the experimental stack, so a control
# named on the command line is not verified to be that control: every confirmation it gives says so.
ECHO_UNVERIFIED = "control identity unverified"
# The frozen thresholds as exact fractions: a primary part's share of Δ_total, M4's permit-absent share of its part's
# Δ, a control's reduction, and M3's rise in flood services per sample (absolute, so a zero base is defined).
ECHO_PRIMARY_SHARE = Fraction(1, 2)
ECHO_PERMIT_ABSENT_SHARE = Fraction(1, 2)
ECHO_REDUCTION = Fraction(1, 2)
ECHO_FLOOD_RISE = Fraction(1)
# §4's "in both runs" and "in two runs": a replay, and each control, is two executions of its workflow pair.
ECHO_EXECUTIONS = 2
# The set the attribution reads: S2/flood's counters dataset, the only one whose samples carry a timeline.
ECHO_LABEL = "S2/flood"
ECHO_DATASET = "counters"
ECHO_PLATFORMS = ("macos", "windows")
# Every final classification kind perf-compare writes: classify_outcome's and the set checks' in run_set. Any other
# kind in an attempt's classification.json is malformed evidence.
ECHO_CLASSIFICATION_KINDS = ("valid", "invalid", "blocked", "schema", "refused", "cleanup", "timeout", "unexpected",
                             "launcher", "focus", "font", "home", "session", "occluded", "deadline", "adapter",
                             "grid", "display", "renderer", "presenter")
# The run identity every execution must carry, and the part of it two executions of one comparison share. A
# control is its own branch, so its head differs; its base, harness, settings, capabilities and platform do not.
# guard_api is the build's guard-correlation cfg (null for an identity older than it): it changes what the
# harness instruments, so executions that differ in it, null against a boolean included, are not one comparison.
ECHO_IDENTITY_KEYS = ("run_id", "run_attempt", "platform", "head_sha", "base_sha", "harness_hash", "settings",
                      "capabilities", "guard_api")
ECHO_SHARED_KEYS = ("platform", "head_sha", "base_sha", "harness_hash", "settings", "capabilities", "guard_api")
ECHO_CONTROL_SHARED_KEYS = ("platform", "base_sha", "harness_hash", "settings", "capabilities", "guard_api")
# A GitHub Actions workflow run id: a positive decimal number with no leading zero. perf-flags.py accepts any text
# for local artifacts; the attribution counts workflow executions, so it reads only real run ids.
ECHO_RUN_ID = re.compile(r"[1-9][0-9]*")


@dataclass
class EchoRun:
    """One accepted S2/flood counters run: its latency samples, and how many it attributed of how many it took."""
    samples: list
    attributed: int
    total: int


@dataclass
class EchoExecution:
    """One workflow execution's accepted S2/flood counters runs per side, with its run identity (empty when none
    was read), each side's final inventory status (empty when healthy) and the rejected attempts it skipped."""
    label: str
    base: list
    head: list
    skipped: list = field(default_factory=list)
    identity: dict = field(default_factory=dict)
    statuses: dict = field(default_factory=dict)


@dataclass
class EchoAttribution:
    """The §4 verdict: the outcome, the step that decided it, each screened mechanism with its step-4 confirmation,
    and every figure behind it, unavailable populations named."""
    outcome: str
    step: int
    mechanisms: list = field(default_factory=list)
    confirmations: dict = field(default_factory=dict)
    lines: list = field(default_factory=list)


def exact_median(values: Sequence[Fraction | int | float]) -> Fraction:
    """The median of a nonempty sample set as an exact fraction: the middle value, or the mean of the middle two."""
    ordered = sorted(Fraction(value) for value in values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def _echo_credited(run: EchoRun) -> list:
    """The run's credited samples: those with a latency."""
    return [sample for sample in run.samples if isinstance(sample, dict) and sample.get("latency_ms") is not None]


def _echo_credited_count(runs: Sequence[EchoRun]) -> int:
    return sum(len(_echo_credited(run)) for run in runs)


def _echo_timelines(runs: Sequence[EchoRun], member: Callable[[dict], bool]) -> list:
    """The timelines of the runs' credited samples that belong to a population, pooled over the runs."""
    return [sample["echo_timeline"] for run in runs for sample in _echo_credited(run)
            if isinstance(sample.get("echo_timeline"), dict) and member(sample["echo_timeline"])]


def _echo_recorded(timeline: dict) -> bool:
    return timeline.get("availability") == "recorded"


def _echo_mean(values: Sequence[int]) -> Fraction:
    """The exact mean of integer nanoseconds or counts; callers pass a nonempty population, gated first."""
    return Fraction(sum(values), len(values))


def _echo_ns(nanos: Fraction, signed: bool = True) -> str:
    """Integer-ns figures as milliseconds for the report; the decision itself never reads this rounding."""
    return f"{float(nanos) / 1e6:{'+' if signed else ''}.3f} ms"


def _echo_latency_ms(milliseconds: Fraction) -> str:
    """A serialized latency_ms figure, already in milliseconds."""
    return f"{float(milliseconds):.3f} ms"


def _echo_percent(share: Fraction) -> str:
    return f"{float(share) * 100:.1f}%"


def _echo_gate(base: tuple[int, int], head: tuple[int, int]) -> bool:
    """§2's population gate on (numerator, denominator) per side: each at least 80%, the two within 10 points, by
    latency_acceptance's integer cross-products, so both boundaries are exact. An empty denominator fails."""
    return base[1] > 0 and head[1] > 0 and perf_metrics.latency_acceptance(base, head)


def _echo_run_median(run: EchoRun) -> Fraction:
    """A run's credited-latency median in milliseconds, exactly, from the serialized latency_ms."""
    return exact_median([sample["latency_ms"] for sample in _echo_credited(run)])


def _echo_part_values(timelines: Sequence[dict], part: str) -> list:
    return [timeline["parts_ns"][part] for timeline in timelines]


def _echo_totals(timelines: Sequence[dict]) -> list:
    """Each ordered sample's credited latency as the exact integer-ns sum of its seven parts."""
    return [sum(timeline["parts_ns"][part] for part in ECHO_PARTS) for timeline in timelines]


def _echo_side_unavailable(execution: EchoExecution) -> list:
    """Why either side of an execution has no evidence to read: a blocked or failed side, no accepted run, a run
    with no credited sample, or no recorded timeline. Each is named, never read as a zero."""
    problems = []
    for side, runs in (("base", execution.base), ("head", execution.head)):
        status = execution.statuses.get(side)
        if status:
            problems.append(f"{execution.label} {side}: the comparison ended {status!r}, so it accepted no run")
        elif not runs:
            problems.append(f"{execution.label} {side}: no accepted S2/flood counters run")
        problems += [f"{execution.label} {side} run {index}: no credited sample"
                     for index, run in enumerate(runs, 1) if not _echo_credited(run)]
    return problems


def _echo_gates(execution: EchoExecution) -> tuple[bool, str]:
    """§2's two gates on one execution, both sides: keypress attribution and the ordered population over credited,
    each at least 80% per side with at most a 10-point gap. Returns whether both pass and the figures."""
    attributed = [(sum(run.attributed for run in runs), sum(run.total for run in runs))
                  for runs in (execution.base, execution.head)]
    ordered = [(len(_echo_timelines(runs, perf_timeline_schema._timeline_ordered)), _echo_credited_count(runs))
               for runs in (execution.base, execution.head)]
    passed = _echo_gate(*attributed) and _echo_gate(*ordered)
    return passed, (f"keypress attribution {attributed[0][0]}/{attributed[0][1]} base, {attributed[1][0]}/"
                    f"{attributed[1][1]} head; ordered {ordered[0][0]}/{ordered[0][1]} base, {ordered[1][0]}/"
                    f"{ordered[1][1]} head")


def echo_identity_problems(replay: Sequence[EchoExecution],
                           controls: Mapping[str, Sequence[EchoExecution]] | None = None) -> list:
    """Why the executions are not distinct workflow executions of one comparison: a missing run identity, one
    workflow run read twice (by run id, whatever the path spelling), replay executions that differ in refs,
    harness, settings, capabilities, guard_api or platform, or a control that is not paired with the replay (another
    base, harness, settings, capabilities, guard_api or platform, the replay's own head, or a head shared with another
    control). A run id must be a positive decimal workflow run id and its attempt positive."""
    controls = controls or {}
    executions = [*replay, *(execution for runs in controls.values() for execution in runs)]
    problems = []
    for execution in executions:
        missing_keys = [key for key in ECHO_IDENTITY_KEYS if key not in execution.identity]
        if missing_keys:
            # When: any identity key is absent, the execution cannot be bound to a workflow run.
            problems.append(f"{execution.label}: no execution identity ({', '.join(missing_keys)})")
    if problems:
        return problems
    # A blank or malformed run id, or attempt 0, names no workflow execution, so it is refused before uniqueness
    # and pairing read it; a non-boolean guard_api would compare equal to a boolean (1 == True).
    for execution in executions:
        run, attempt, guard_api = (execution.identity[key] for key in ("run_id", "run_attempt", "guard_api"))
        if not (isinstance(run, str) and ECHO_RUN_ID.fullmatch(run)):
            problems.append(f"{execution.label}: run_id {run!r} is not a workflow run id")
        if not (perf_latency_schema._is_int(attempt) and attempt > 0):
            problems.append(f"{execution.label}: run_attempt {attempt!r} is not a workflow run attempt")
        if guard_api is not None and not isinstance(guard_api, bool):
            problems.append(f"{execution.label}: guard_api {guard_api!r} is neither null nor a boolean")
    if problems:
        return problems
    seen: dict = {}
    for execution in executions:
        run = execution.identity["run_id"]
        if run in seen:
            problems.append(f"{execution.label}: workflow run {run} is already read as {seen[run]}")
        seen.setdefault(run, execution.label)
    if not replay:
        return problems
    first = replay[0].identity
    problems += [f"{execution.label}: {key} differs from {replay[0].label}'s" for execution in replay[1:]
                 for key in ECHO_SHARED_KEYS if execution.identity[key] != first[key]]
    heads = {}
    for name, runs in controls.items():
        problems += [f"{execution.label}: {key} differs from {replay[0].label}'s" for execution in runs
                     for key in ECHO_CONTROL_SHARED_KEYS if execution.identity[key] != first[key]]
        own = {execution.identity["head_sha"] for execution in runs}
        if len(own) != 1:
            problems.append(f"{name}: its executions measured {len(own)} heads, not one")
            continue
        head = own.pop()
        if head == first["head_sha"]:
            problems.append(f"{name}: its head is the replay's head, not a control branch")
        if head in heads:
            problems.append(f"{name}: its head is also {heads[head]}'s")
        heads.setdefault(head, name)
    return problems


def _echo_m3(replay: Sequence[EchoExecution], lines: list) -> list:
    """§4 step 3's M3 screen, per execution: on ordered ∩ m3_complete, the head's mean flood services exceed the
    base's by at least 1.0 per sample. Each execution's state is True, False, or None when that population fails
    §2's gate, which is unavailable, never false."""
    states: list = []
    for execution in replay:
        base, head = (_echo_timelines(runs, perf_timeline_schema._timeline_in_m3) for runs in (execution.base, execution.head))
        coverage = ((len(base), _echo_credited_count(execution.base)),
                    (len(head), _echo_credited_count(execution.head)))
        figures = f"ordered ∩ m3_complete {coverage[0][0]}/{coverage[0][1]} base, {coverage[1][0]}/{coverage[1][1]} head"
        if not _echo_gate(*coverage):
            # When: the M3 population is below §2's gate, its mean is not evidence; missing is never zero.
            lines.append(f"- step 3 M3, {execution.label}: unavailable: {figures} of credited, below §2's gate")
            states.append(None)
            continue
        rise = (_echo_mean([timeline["flood_services"] for timeline in head])
                - _echo_mean([timeline["flood_services"] for timeline in base]))
        states.append(rise >= ECHO_FLOOD_RISE)
        lines.append(f"- step 3 M3, {execution.label}: mean flood services rise {float(rise):+.3f} per sample "
                     f"({figures}): {'holds' if states[-1] else 'below 1.0'}")
    return states


def _echo_m4(replay: Sequence[EchoExecution], lines: list, m3_states: Sequence[bool | None]) -> bool:
    """§4 step 3's M4 screen, with v6 X3 and v6.1 B2: in each execution, on ordered ∩ tick_qualified (at least 80%
    of ordered per side, §2), the mean integrated permit-absent time carries at least 50% of frame_request_to_entry's
    Δ on that population, and M3's condition is false in that execution. tick_to_entry stays descriptive."""
    holds = []
    for execution, m3_state in zip(replay, m3_states):
        if m3_state is None:
            lines.append(f"- step 3 M4, {execution.label}: not evaluated: M3 is unavailable there, so its condition "
                         f"cannot be shown not to hold")
            holds.append(False)
            continue
        base, head = (_echo_timelines(runs, perf_timeline_schema._timeline_in_m4) for runs in (execution.base, execution.head))
        coverage = ((len(base), len(_echo_timelines(execution.base, perf_timeline_schema._timeline_ordered))),
                    (len(head), len(_echo_timelines(execution.head, perf_timeline_schema._timeline_ordered))))
        figures = f"tick-qualified {coverage[0][0]}/{coverage[0][1]} base, {coverage[1][0]}/{coverage[1][1]} head"
        if not _echo_gate(*coverage):
            # When: too few ordered samples consumed a known tick, the M4 population is not evidence.
            lines.append(f"- step 3 M4, {execution.label}: unavailable: {figures} of ordered, below §2's gate")
            holds.append(False)
            continue
        part = (_echo_mean(_echo_part_values(head, "frame_request_to_entry"))
                - _echo_mean(_echo_part_values(base, "frame_request_to_entry")))
        absent = (_echo_mean([timeline["permit_absent_ns"] for timeline in head])
                  - _echo_mean([timeline["permit_absent_ns"] for timeline in base]))
        ticks = [_echo_mean([timeline["tick_to_entry_ns"] for timeline in population]) for population in (base, head)]
        carries = part > 0 and absent >= ECHO_PERMIT_ABSENT_SHARE * part
        share = _echo_percent(absent / part) if part else "n/a"
        lines.append(f"- step 3 M4, {execution.label}: permit-absent Δ {_echo_ns(absent)} of frame_request_to_entry Δ "
                     f"{_echo_ns(part)} ({share}; {figures}; tick_to_entry, descriptive, "
                     f"{_echo_ns(ticks[0], False)} base, {_echo_ns(ticks[1], False)} head): "
                     f"{'carries at least 50%' if carries else 'below 50%'}")
        if m3_state:
            lines.append(f"- step 3 M4, {execution.label}: M3's condition holds there, so M4 does not")
        holds.append(carries and not m3_state)
    return all(holds)


def _echo_stratified(replay: Sequence[EchoExecution], lines: list) -> bool:
    """§4 step 4's M3 check: within each head run of both executions, ordered ∩ m3_complete samples with no flood
    service have a lower frame_request_to_entry median than those with at least one. A run with an empty stratum
    leaves the check unavailable, which does not hold."""
    results = []
    for execution in replay:
        for index, run in enumerate(execution.head, 1):
            population = _echo_timelines([run], perf_timeline_schema._timeline_in_m3)
            quiet = [timeline["parts_ns"]["frame_request_to_entry"] for timeline in population
                     if timeline["flood_services"] == 0]
            busy = [timeline["parts_ns"]["frame_request_to_entry"] for timeline in population
                    if timeline["flood_services"] >= 1]
            where = f"- step 4 M3 stratified, {execution.label} head run {index}"
            if not quiet or not busy:
                # When: a stratum is empty, the within-run comparison is unavailable, never a pass.
                lines.append(f"{where}: unavailable ({len(quiet)} with no flood service, {len(busy)} with one or more)")
                results.append(False)
                continue
            results.append(exact_median(quiet) < exact_median(busy))
            lines.append(f"{where}: median {_echo_ns(exact_median(quiet), False)} with no flood service against "
                         f"{_echo_ns(exact_median(busy), False)} with one or more: "
                         f"{'lower' if results[-1] else 'not lower'}")
    return all(results)


def _echo_control_holds(part: str, name: str, control: EchoExecution, reference: EchoExecution,
                        lines: list) -> bool:
    """One control execution against the replay execution it is paired with, both on the ordered population (v5 V5).
    The control's evidence and both §2 gates come first, so no statistic is computed from an empty or ungated side.
    Then R_C = 1 - Δ(control vs base) / Δ(head vs base) for the part's mean, and likewise for the total median
    shift; it holds when both reductions are at least 50% of a positive reference shift."""
    where = f"- step 4 {name}, {control.label} against {reference.label}"
    missing = _echo_side_unavailable(control) + [
        f"{control.label} {side}: no recorded timeline" for side, runs in (("base", control.base),
                                                                          ("head", control.head))
        if runs and not _echo_timelines(runs, _echo_recorded)]
    if missing:
        lines.append(f"{where}: unavailable: {'; '.join(missing)}")
        return False
    passed, figures = _echo_gates(control)
    if not passed:
        lines.append(f"{where}: {figures}, below §2's gates")
        return False
    pools = [[_echo_timelines(runs, perf_timeline_schema._timeline_ordered) for runs in (execution.base, execution.head)]
             for execution in (reference, control)]
    (reference_base, reference_head), (control_base, control_head) = pools
    reference_part = (_echo_mean(_echo_part_values(reference_head, part))
                      - _echo_mean(_echo_part_values(reference_base, part)))
    control_part = _echo_mean(_echo_part_values(control_head, part)) - _echo_mean(_echo_part_values(control_base, part))
    reference_total = exact_median(_echo_totals(reference_head)) - exact_median(_echo_totals(reference_base))
    control_total = exact_median(_echo_totals(control_head)) - exact_median(_echo_totals(control_base))
    if reference_part <= 0 or reference_total <= 0:
        # When: the reference shift is not positive, R_C has no direction to fall in.
        lines.append(f"{where}: reference Δ {_echo_ns(reference_part)} and median shift {_echo_ns(reference_total)} "
                     f"are not both positive, so R_C is undefined")
        return False
    part_reduction, total_reduction = 1 - control_part / reference_part, 1 - control_total / reference_total
    holds = part_reduction >= ECHO_REDUCTION and total_reduction >= ECHO_REDUCTION
    lines.append(f"{where}: R_C {_echo_percent(part_reduction)} for {part}, {_echo_percent(total_reduction)} for "
                 f"the total median shift: {'holds' if holds else 'below 50%'}")
    return holds


def _echo_confirm(mechanism: str, replay: Sequence[EchoExecution], controls: Mapping[str, Sequence[EchoExecution]],
                  lines: list) -> str:
    """§4 step 4 for one screened mechanism: attributed when one of its controls holds in both executions; M3, whose
    C5 is broad, is at most supporting evidence (v5 V5). Without its controls it is screened, unconfirmed. Every
    confirmation names that the control's identity is unverified (ruling D1)."""
    stratified = _echo_stratified(replay, lines) if mechanism == "M3" else False
    given = [name for name in ECHO_MECHANISM_CONTROLS[mechanism] if controls.get(name)]
    if not given:
        return "screened, unconfirmed (no control runs)"
    confirming = []
    for name in given:
        runs = controls[name]
        if len(runs) != ECHO_EXECUTIONS:
            lines.append(f"- step 4 {name}: {len(runs)} execution(s), not two, so it cannot confirm")
            continue
        # Every pair is evaluated, so each one's figures are reported even after one fails.
        held = [_echo_control_holds(ECHO_MECHANISM_PARTS[mechanism], name, control, reference, lines)
                for control, reference in zip(runs, replay)]
        if all(held):
            confirming.append(name)
    if mechanism == "M3":
        if confirming and stratified:
            return (f"supporting evidence only (C5, a broad control, with the stratified check; {ECHO_UNVERIFIED}); "
                    f"screened, unconfirmed")
        return "screened, unconfirmed"
    return f"attributed ({', '.join(confirming)}, {ECHO_UNVERIFIED})" if confirming else "screened, unconfirmed"


def echo_attribution(replay: Sequence[EchoExecution],
                     controls: Mapping[str, Sequence[EchoExecution]] | None = None) -> EchoAttribution:
    """Apply plan v6.1 §4, with its v5, v6 and v6.1 amendments, to the S2/flood echo timelines of a replay's two
    executions and of each control's two executions, paired in order. Analysis only; the evidence is expected read
    through load_echo_execution. Timeline means and shifts are exact over integer nanoseconds; step 1 reads the
    serialized latency_ms. Missing identity, a side or a population without evidence is named, never read as zero."""
    controls = controls or {}
    lines = [f"- {execution.label}: skipped rejected attempts {', '.join(execution.skipped)}"
             for execution in replay if execution.skipped]
    missing = ([] if len(replay) == ECHO_EXECUTIONS else [f"{len(replay)} replay execution(s); §4 reads two"])
    missing += echo_identity_problems(replay, controls)
    missing += [problem for execution in replay for problem in _echo_side_unavailable(execution)]
    if missing:
        return EchoAttribution("inconclusive (evidence unavailable)", 0, lines=lines + [f"- {item}" for item in missing])
    # Step 1: every head run's credited-latency median above every base run's, in both executions.
    reproduced = []
    for execution in replay:
        base, head = ([_echo_run_median(run) for run in runs] for runs in (execution.base, execution.head))
        reproduced.append(min(head) > max(base))
        lines.append(f"- step 1, {execution.label}: head run medians "
                     f"{', '.join(_echo_latency_ms(value) for value in head)}; base "
                     f"{', '.join(_echo_latency_ms(value) for value in base)}: "
                     f"{'reproduced' if reproduced[-1] else 'not reproduced'}")
    if not all(reproduced):
        return EchoAttribution("inconclusive (not reproduced under instrumentation)", 1, lines=lines)
    unrecorded = [f"{execution.label} {side}" for execution in replay
                  for side, runs in (("base", execution.base), ("head", execution.head))
                  if not _echo_timelines(runs, _echo_recorded)]
    if unrecorded:
        undeclared = [execution.label for execution in replay
                      if execution.identity.get("capabilities", {}).get("echo_timeline_schema") is None]
        lines.append(f"- step 2: timeline unavailable on {', '.join(unrecorded)} (recorded nothing"
                     + (f"; the harness of {', '.join(undeclared)} declares no echo_timeline_schema" if undeclared
                        else "") + ")")
        return EchoAttribution("inconclusive (evidence unavailable)", 2, lines=lines)
    # §2's gates: keypress attribution and the ordered population, each at least 80% with at most a 10-point gap.
    gated = []
    for execution in replay:
        passed, figures = _echo_gates(execution)
        gated.append(passed)
        lines.append(f"- §2 gates, {execution.label}: {figures}: {'pass' if passed else 'fail'}")
        ordered = [_echo_credited_count(runs) for runs in (execution.base, execution.head)]
        during = [len(_echo_timelines(runs, lambda timeline: timeline.get("ordering") == "published_during_dispatch"))
                  for runs in (execution.base, execution.head)]
        # A published_during_dispatch share that moves by more than 10 points is itself a candidate (§2).
        if 10 * ordered[0] * ordered[1] < 100 * abs(during[0] * ordered[1] - during[1] * ordered[0]):
            lines.append(f"- §2 population shift, {execution.label}: published_during_dispatch {during[0]}/"
                         f"{ordered[0]} base, {during[1]}/{ordered[1]} head differs by more than 10 points "
                         f"(a candidate)")
    if not all(gated):
        return EchoAttribution("inconclusive (population coverage)", 2, lines=lines)
    # Step 2: the mean screen on ordered.
    screens = []
    for execution in replay:
        base, head = (_echo_timelines(runs, perf_timeline_schema._timeline_ordered) for runs in (execution.base, execution.head))
        deltas = {part: _echo_mean(_echo_part_values(head, part)) - _echo_mean(_echo_part_values(base, part))
                  for part in ECHO_PARTS}
        shifts = {part: exact_median(_echo_part_values(head, part)) - exact_median(_echo_part_values(base, part))
                  for part in ECHO_PARTS}
        total = sum(deltas.values(), Fraction(0))
        screens.append((deltas, shifts, total))
        lines.append(f"- step 2, {execution.label}: Δ_total {_echo_ns(total)} on ordered ({len(base)} base, "
                     f"{len(head)} head)")
    if any(total <= 0 for _, _, total in screens):
        return EchoAttribution("inconclusive (mean and median disagree)", 2, lines=lines)
    primaries = []
    for part in ECHO_PARTS:
        shares = [deltas[part] / total for deltas, _, total in screens]
        # A zero median shift has no sign, so it does not share Δ_I's.
        signs = [shifts[part] * deltas[part] > 0 for deltas, shifts, _ in screens]
        if all(share >= ECHO_PRIMARY_SHARE for share in shares) and all(signs):
            primaries.append(part)
        lines.append(f"- step 2 {part}: s_I {', '.join(_echo_percent(share) for share in shares)}; median shift "
                     f"{', '.join(_echo_ns(shifts[part]) for _, shifts, _ in screens)}"
                     + (": primary" if part in primaries else ""))
    if not primaries:
        return EchoAttribution("distributed / inconclusive", 2, lines=lines)
    # Step 3: the screens on each primary part.
    mechanisms = []
    if "render" in primaries:
        mechanisms.append("M1")
    if "publication_to_frame_request" in primaries:
        mechanisms.append("M2")
    if "frame_request_to_entry" in primaries:
        m3_states = _echo_m3(replay, lines)
        if all(state is True for state in m3_states):
            mechanisms.append("M3")
        if _echo_m4(replay, lines, m3_states):
            mechanisms.append("M4")
    lines += [f"- step 3: primary {part} has no mechanism screen" for part in primaries
              if part not in ECHO_MECHANISM_PARTS.values()]
    if not mechanisms:
        return EchoAttribution("inconclusive (no mechanism screened)", 3, lines=lines)
    named = [f"{mechanism} {ECHO_MECHANISM_NAMES[mechanism]}" for mechanism in mechanisms]
    outcome = named[0] if len(named) == 1 else f"combined ({', '.join(named)})"
    # Step 4: control confirmation.
    confirmations = {mechanism: _echo_confirm(mechanism, replay, controls, lines) for mechanism in mechanisms}
    return EchoAttribution(outcome, 4, mechanisms, confirmations, lines)


def format_echo_attribution(result: EchoAttribution) -> str:
    """The verdict as Markdown: the outcome, each mechanism's confirmation, then every figure behind them."""
    lines = ["## S2/flood echo attribution (#1607 plan v6.1 §4)", "",
             f"- outcome: {result.outcome} (step {result.step})"]
    lines += [f"- {mechanism} {ECHO_MECHANISM_NAMES[mechanism]}: {result.confirmations[mechanism]}"
              for mechanism in result.mechanisms]
    lines += result.lines
    lines.append("- control identity is not verified here (ruling D1): a control named on the command line is "
                 "trusted to be that control, so every attribution is conditional on it.")
    lines.append("- exit status 0 means the evidence was read and validated, not that a mechanism is confirmed: "
                 "read the outcome.")
    return "\n".join(lines) + "\n"
