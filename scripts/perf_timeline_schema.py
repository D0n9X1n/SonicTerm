"""Perf timeline schema."""

from __future__ import annotations
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_attribution_schema
import perf_latency_schema


# The harness's per-sample echo timeline, `echo_timeline` schema 1 (timeline.rs). Every population is recomputed
# from these per-sample fields; supplied coverage is compared with the recomputation, never trusted.
TIMELINE_SCHEMA = 1
# The echo-timeline schemas this script can validate, as a harness declares them in `--list`.
ECHO_TIMELINE_SCHEMAS = (1,)
# The variants whose credited samples carry an echo timeline under the declared schema: split_in_scope's.
TIMELINE_SCOPE = (("S2", "default"), ("S2", "flood"))
TIMELINE_PART_NAMES = ("input_to_parse", "parse_to_publication", "publication_to_frame_request",
                       "frame_request_to_entry", "entry_to_render", "render", "render_exit_to_credited_end")
TIMELINE_AVAILABILITIES = ("recorded", "unavailable", "rejected")
TIMELINE_ENUMS = {
    "ordering": ("ordered", "published_during_dispatch", "split_only", "clock-order"),
    "read_stamp": ("stamped", "not-configured", "missing-or-unrepresentable"),
    "admission": ("permit", "fallback", "admitted"),
    "permit_identity": ("known", "unknown", "none"),
}
TIMELINE_READINESS_OUTCOMES = ("native_request", "marked_in_flight", "none")
TIMELINE_SITES = ("output_service", "admission")
# The producer's integer widths: `u64` durations, counts and identities, and `u32` event-loop and dispatch sequences.
U32_MAX = 2 ** 32 - 1
TIMELINE_U64_KEYS = ("render_exit_to_dispatch_return_ns", "dispatch_return_to_credited_end_ns",
                     "latest_read_to_parse_ns", "permit_present_ns", "permit_absent_ns", "tick_to_entry_ns",
                     "flood_services", "token", "window", "pane")
TIMELINE_U32_KEYS = ("credited_dispatch_seq",)
TIMELINE_BOOL_KEYS = ("ready_before_publication", "overflow", "tick_qualified", "m3_complete")
TIMELINE_STR_KEYS = ("unavailable_reason", "rejection_reason", "ordering_reason")
TIMELINE_KEYS = ("schema", "availability", "unavailable_reason", "rejection_reason", "ordering", "ordering_reason",
                 "ready_before_publication", "overflow", "parts_ns", "render_exit_to_dispatch_return_ns",
                 "dispatch_return_to_credited_end_ns", "latest_read_to_parse_ns", "read_stamp", "readiness",
                 "admission", "permit_present_ns", "permit_absent_ns", "permit_identity", "tick_qualified",
                 "tick_to_entry_ns", "flood_services", "m3_complete", "token", "window", "pane",
                 "credited_dispatch_seq")
# Identities the harness keeps whenever it knows them independently, whatever the availability.
TIMELINE_IDENTITY_KEYS = ("token", "window", "pane")
# Every classification derived from a record; all null unless the timeline was recorded.
TIMELINE_DIAGNOSTIC_KEYS = tuple(key for key in TIMELINE_KEYS if key not in (
    "schema", "availability", "unavailable_reason", "rejection_reason") + TIMELINE_IDENTITY_KEYS)
# The fields derived from the record's events; all null once an event was dropped.
TIMELINE_EVENT_KEYS = ("readiness", "admission", "permit_present_ns", "permit_absent_ns", "permit_identity",
                       "tick_qualified", "tick_to_entry_ns", "flood_services", "m3_complete", "credited_dispatch_seq",
                       "ready_before_publication", "parts_ns", "render_exit_to_dispatch_return_ns",
                       "dispatch_return_to_credited_end_ns")
# The accessor's and the protocol's rejections, and why a timeline was unavailable; any other reason is malformed.
TIMELINE_REJECTION_REASONS = ("malformed-initial-permit", "mismatch", "not-taken", "already-taken", "no-arm-instant")
TIMELINE_UNAVAILABLE_REASONS = ("cfg-off", "not-recorded", "gate-off", "no-pane", "exhausted")
# The raw-record validator's reasons: a recorded execution it finds malformed is clock-order and derives nothing.
TIMELINE_VALIDATION_REASONS = ("loop-sequence", "loop-time-order", "dispatch-pair", "admission-orphan",
                               "admission-count", "admission-order", "render-pair", "flood-sequence",
                               "initial-permit", "tick-identity", "tick-sequence", "permit-transition")
# A credited sample's own split reasons other than `split`: when one is set, the timeline keeps it as its reason.
TIMELINE_SPLIT_REASONS = tuple(reason for reason in perf_latency_schema.SPLIT_REASONS if reason != "split")
# What each ordering reason the producer writes implies, as (statuses, bound pair, readiness, ready before
# publication). `True` requires the field, `False` forbids it, `None` leaves it free; the readiness flag's entry
# is the value it must take when present. The `None` key is an ordered or published-during-dispatch sample.
# timeline.rs analyze: overflow returns before binding (1001), an unrepresentable instant (1011) and a malformed
# record (1021) return before it or keep it, binding sets the pair whatever the sample's own reason (1025-1031), and
# readiness needs the split (981, 1054-1064); a sample's own split reason then wins the disposition (1074-1075).
TIMELINE_REASON_SHAPES = {
    None: (("ordered", "published_during_dispatch"), True, True, None),
    "overflow": (("split_only",), False, False, None),
    "no-credited-pair": (("split_only",), False, None, None),
    "ambiguous-credited-pair": (("split_only",), False, None, None),
    "missing-event:output-check": (("split_only",), True, False, None),
    "readiness-after-entry": (("split_only",), True, True, None),
    "ready-before-publication": (("split_only",), True, True, True),
    "event-order": (("clock-order",), True, True, False),
    "sum": (("clock-order",), True, True, False),
    "unrepresentable-instant": (("clock-order",), None, None, None),
    **{reason: (("clock-order",), False, False, None) for reason in TIMELINE_VALIDATION_REASONS},
    # A sample's own split reason: the pair may be bound (1025-1031), but no split means no readiness (981, 1054).
    **{reason: (("clock-order" if reason == "clock-order" else "split_only",), None, False, None)
       for reason in TIMELINE_SPLIT_REASONS},
}
# The fields only a uniquely bound credited pair supplies, all present together or all null.
TIMELINE_BOUND_KEYS = ("credited_dispatch_seq", "admission", "permit_identity", "tick_qualified")
TIMELINE_POPULATIONS = ("ordered", "b2", "m4", "m3", "read_stamped", "b2_of_credited", "m3_of_credited")
TIMELINE_COVERAGE_KEYS = ("schema", "availability", "credited", "recorded", *TIMELINE_POPULATIONS)
# The frozen two-sided gate as exact fractions: each side's ordered population at least 4/5 of its credited
# samples, and the sides at most 1/10 (ten percentage points) apart. Compared by integer cross-products.
TIMELINE_COVERAGE_GATE = (4, 5)
TIMELINE_COVERAGE_GAP = (1, 10)


def _nonnegative_int(value: object) -> bool:
    return perf_latency_schema._is_int(value) and value >= 0


def _within(value: object, limit: int) -> bool:
    """A strict integer (never a bool or a float) in `0..=limit`."""
    return perf_latency_schema._is_int(value) and 0 <= value <= limit


def _bounded_number(value: object) -> bool:
    """A finite non-negative number that converts to a float without overflow."""
    if perf_latency_schema._is_int(value):
        return 0 <= value <= perf_attribution_schema.U64_MAX
    return perf_latency_schema._finite_nonnegative(value)


def _timeline_types_broken(timeline: dict) -> bool:
    """Whether a field has a type, value or width the producer cannot write."""
    if timeline.get("availability") not in TIMELINE_AVAILABILITIES:
        return True
    if any(timeline.get(key) is not None and not _within(timeline.get(key), perf_attribution_schema.U64_MAX) for key in TIMELINE_U64_KEYS):
        return True
    if any(timeline.get(key) is not None and not _within(timeline.get(key), U32_MAX) for key in TIMELINE_U32_KEYS):
        return True
    if any(timeline.get(key) is not None and not isinstance(timeline.get(key), bool) for key in TIMELINE_BOOL_KEYS):
        return True
    if any(timeline.get(key) is not None and not isinstance(timeline.get(key), str) for key in TIMELINE_STR_KEYS):
        return True
    for key, allowed in TIMELINE_ENUMS.items():
        value = timeline.get(key)
        if value is not None and not (isinstance(value, str) and value in allowed):
            return True
    parts = timeline.get("parts_ns")
    if parts is not None and not (isinstance(parts, dict) and set(parts) == set(TIMELINE_PART_NAMES)
                                  and all(_within(value, perf_attribution_schema.U64_MAX) for value in parts.values())):
        return True
    readiness = timeline.get("readiness")
    return readiness is not None and not (
        isinstance(readiness, dict) and set(readiness) == {"outcome", "loop_seq", "site"}
        and readiness["outcome"] in TIMELINE_READINESS_OUTCOMES and _within(readiness["loop_seq"], U32_MAX)
        and (readiness["site"] is None or readiness["site"] in TIMELINE_SITES))


def _timeline_parts_total(timeline: dict) -> int | None:
    """The seven parts' sum, or None when they are absent or malformed."""
    parts = timeline.get("parts_ns")
    if not (isinstance(parts, dict) and all(_within(parts.get(name), perf_attribution_schema.U64_MAX) for name in TIMELINE_PART_NAMES)):
        return None
    return sum(parts[name] for name in TIMELINE_PART_NAMES)


def _timeline_sub_parts_broken(timeline: dict) -> bool:
    """The two tail sub-parts are present exactly with the parts and sum to the seventh."""
    parts = timeline.get("parts_ns")
    tail = (timeline.get("render_exit_to_dispatch_return_ns"), timeline.get("dispatch_return_to_credited_end_ns"))
    if any((value is None) != (parts is None) for value in tail):
        return True
    seventh = parts.get("render_exit_to_credited_end") if isinstance(parts, dict) else None
    return all(_nonnegative_int(value) for value in (*tail, seventh)) and tail[0] + tail[1] != seventh


def _timeline_parts_sum_broken(timeline: dict, sample: dict) -> bool:
    """The seven integer-ns parts sum to the sample's credited latency, within the serialized rounding."""
    total, latency_ms = _timeline_parts_total(timeline), sample.get("latency_ms")
    return total is not None and _bounded_number(latency_ms) and abs(total / 1e6 - latency_ms) > perf_latency_schema.SPLIT_SUM_TOLERANCE_MS


def _timeline_split_parts_broken(timeline: dict, sample: dict) -> bool:
    """An ordered timeline's first two parts are the split's first two, and its last five the split's third, each
    within the serialized rounding; a redistribution between parts that keeps the total is caught here."""
    parts, split = timeline.get("parts_ns"), sample.get("split")
    if timeline.get("ordering") != "ordered" or _timeline_parts_total(timeline) is None:
        return False
    if not isinstance(split, dict):
        return True
    figures = (parts["input_to_parse"], parts["parse_to_publication"],
               sum(parts[name] for name in TIMELINE_PART_NAMES[2:]))
    for nanoseconds, name in zip(figures, perf_latency_schema.SPLIT_PARTS):
        milliseconds = split.get(name)
        if not _bounded_number(milliseconds) or abs(nanoseconds / 1e6 - milliseconds) > perf_latency_schema.SPLIT_SUM_TOLERANCE_MS:
            return True
    return False


def _timeline_occupancy_broken(timeline: dict) -> bool:
    """On an ordered timeline, the permit's present and absent time cover readiness to entry exactly."""
    present, absent, parts = (timeline.get("permit_present_ns"), timeline.get("permit_absent_ns"),
                              timeline.get("parts_ns"))
    if timeline.get("ordering") != "ordered" or not isinstance(parts, dict) or None in (present, absent):
        return False
    return present + absent != parts.get("frame_request_to_entry")


def _timeline_shape(timeline: dict, index: int) -> bool | tuple | None:
    """Entry `index` of the timeline's reason shape; None for an unknown reason or an unrecorded timeline."""
    shape = TIMELINE_REASON_SHAPES.get(timeline.get("ordering_reason"))
    return shape[index] if shape is not None and timeline.get("ordering") is not None else None


def _timeline_requirement_broken(present: bool, requirement: bool | None) -> bool:
    """A field `present` or not, against its shape's requirement: required, forbidden or free."""
    return requirement is not None and present != requirement


def _timeline_precedence_broken(timeline: dict, sample: dict) -> bool:
    """A sample's own split reason other than `split` is the timeline's reason, unless the timeline could not be
    read (an unrepresentable instant) or its raw record is malformed, which take precedence over it; a split sample
    never carries another sample's split reason, and an overflow is named only on a split sample."""
    reason, own = timeline.get("ordering_reason"), sample.get("split_reason")
    if timeline.get("ordering") is None:
        return False
    if own != "split":
        return reason not in (own, "unrepresentable-instant", *TIMELINE_VALIDATION_REASONS)
    return reason in TIMELINE_SPLIT_REASONS


def _timeline_overflow_reason_broken(timeline: dict, sample: dict) -> bool:
    """An overflowed record keeps the sample's own split reason, else names the overflow; only it names one."""
    reason, overflow = timeline.get("ordering_reason"), timeline.get("overflow")
    if overflow is True:
        return reason not in ("overflow", sample.get("split_reason"))
    return reason == "overflow"


# Each rule as (name, broken(timeline, sample), what it requires); the first broken rule names the problem.
TIMELINE_RULES = (
    ("keys", lambda timeline, sample: set(timeline) != set(TIMELINE_KEYS),
     f"carries exactly the schema's {len(TIMELINE_KEYS)} keys"),
    ("schema", lambda timeline, sample: not (perf_latency_schema._is_int(timeline.get("schema"))
                                             and timeline["schema"] == TIMELINE_SCHEMA),
     f"schema is {TIMELINE_SCHEMA}"),
    ("field-types", lambda timeline, sample: _timeline_types_broken(timeline),
     "every field has its schema type, value and the producer's integer width"),
    ("availability-reason", lambda timeline, sample: (
        timeline.get("unavailable_reason") is None, timeline.get("rejection_reason") is None) != {
        "recorded": (True, True), "unavailable": (False, True), "rejected": (True, False),
    }.get(timeline.get("availability")),
     "a recorded timeline has no reason; an unavailable or rejected one has its own reason only"),
    ("rejection-reason", lambda timeline, sample: timeline.get("availability") == "rejected"
     and timeline.get("rejection_reason") not in TIMELINE_REJECTION_REASONS,
     f"a rejection is one of {', '.join(TIMELINE_REJECTION_REASONS)}"),
    ("unavailable-reason", lambda timeline, sample: timeline.get("availability") == "unavailable"
     and timeline.get("unavailable_reason") not in TIMELINE_UNAVAILABLE_REASONS,
     f"an unavailable timeline names one of {', '.join(TIMELINE_UNAVAILABLE_REASONS)}"),
    ("unrecorded-carries-diagnostics", lambda timeline, sample: timeline.get("availability") != "recorded"
     and any(timeline.get(key) is not None for key in TIMELINE_DIAGNOSTIC_KEYS),
     "an unavailable or rejected timeline carries no classification and no population"),
    ("recorded-fields", lambda timeline, sample: timeline.get("availability") == "recorded"
     and None in (timeline.get("overflow"), timeline.get("ordering"), timeline.get("read_stamp")),
     "a recorded timeline has overflow, ordering and read_stamp"),
    ("parts-without-ordered", lambda timeline, sample: (timeline.get("parts_ns") is not None)
     != (timeline.get("ordering") == "ordered"),
     "parts_ns is present exactly when ordering is ordered"),
    ("ordered-with-overflow", lambda timeline, sample: timeline.get("ordering") == "ordered"
     and timeline.get("overflow") is True,
     "an overflowed record is never ordered"),
    ("ordered-split", lambda timeline, sample: timeline.get("ordering") == "ordered"
     and not (sample.get("split_reason") == "split" and timeline.get("credited_dispatch_seq") is not None),
     "an ordered sample has a split and a credited dispatch pair"),
    ("overflow-event-fields", lambda timeline, sample: timeline.get("overflow") is True
     and any(timeline.get(key) is not None for key in TIMELINE_EVENT_KEYS),
     "an overflowed record derives nothing from its events"),
    ("malformed-derives-nothing", lambda timeline, sample: timeline.get("ordering") is not None
     and timeline.get("ordering_reason") in TIMELINE_VALIDATION_REASONS
     and (timeline.get("ordering") != "clock-order" or timeline.get("overflow") is not False
          or any(timeline.get(key) is not None for key in TIMELINE_EVENT_KEYS)),
     "a raw-validation reason is a recorded, unoverflowed clock-order that derives nothing, whatever its status"),
    ("reason-known", lambda timeline, sample: timeline.get("ordering") is not None
     and timeline.get("ordering_reason") not in TIMELINE_REASON_SHAPES,
     "the ordering reason is one the producer writes"),
    ("reason-status", lambda timeline, sample: timeline.get("ordering") is not None
     and timeline.get("ordering") not in (_timeline_shape(timeline, 0) or ()),
     "each ordering reason has its status: none for ordered and published_during_dispatch, split_only or "
     "clock-order as the producer decides it"),
    ("reason-precedence", _timeline_precedence_broken,
     "a sample's own split reason stays the timeline's reason, below only an unrepresentable instant and a "
     "malformed record"),
    ("reason-overflow", _timeline_overflow_reason_broken,
     "an overflow keeps the sample's own split reason, else is named overflow, and only an overflow is"),
    ("bound-pair", lambda timeline, sample: len({timeline.get(key) is None for key in TIMELINE_BOUND_KEYS}) != 1
     or (timeline.get("admission") == "permit") != (timeline.get("permit_identity") in ("known", "unknown")),
     "a bound pair supplies its admission, permit identity and tick qualification together, and only a permit "
     "admission consumed a permit"),
    ("reason-binding", lambda timeline, sample: _timeline_requirement_broken(
        timeline.get("credited_dispatch_seq") is not None, _timeline_shape(timeline, 1)),
     "a binding reason agrees with the bound pair: none for no or an ambiguous pair, an overflow or a malformed "
     "record, one for every reason decided after binding"),
    ("reason-readiness", lambda timeline, sample: _timeline_requirement_broken(
        timeline.get("readiness") is not None, _timeline_shape(timeline, 2)),
     "the readiness check is present for every reason decided from it and absent when it was never found"),
    # timeline.rs:981 and 1054-1068: readiness is read only through the sample's split, whatever reason wins.
    ("readiness-split", lambda timeline, sample: timeline.get("readiness") is not None
     and not (sample.get("split_reason") == "split" and isinstance(sample.get("split"), dict)),
     "a readiness check needs the sample's split, through which it is read"),
    ("unrepresentable-pair", lambda timeline, sample: timeline.get("ordering_reason") == "unrepresentable-instant"
     and (timeline.get("credited_dispatch_seq") is None) != (timeline.get("readiness") is None),
     "an unrepresentable instant derives nothing before binding, or both the pair and its readiness after it"),
    ("after-entry-intervals", lambda timeline, sample: timeline.get("ordering_reason") == "readiness-after-entry"
     and any(timeline.get(key) is not None for key in ("permit_present_ns", "permit_absent_ns", "flood_services",
                                                         "m3_complete")),
     "readiness after the entry supplies no permit occupancy and no flood interval"),
    # timeline.rs:1091-1110: occupancy and the flood interval are read only with a bound pair and readiness.
    ("context-prerequisites", lambda timeline, sample: any(
        timeline.get(key) is not None for key in ("permit_present_ns", "permit_absent_ns", "flood_services",
                                                  "m3_complete"))
     and (timeline.get("credited_dispatch_seq") is None or timeline.get("readiness") is None),
     "permit occupancy and the flood interval need a bound pair and its readiness check"),
    ("ready-before-reason", lambda timeline, sample: (
        timeline.get("ready_before_publication") is not None and timeline.get("readiness") is None)
     or (_timeline_shape(timeline, 3) is not None
         and timeline.get("ready_before_publication") is not _timeline_shape(timeline, 3)),
     "readiness before publication is read only with a readiness check, and agrees with its reason"),
    # timeline.rs:875-881 and 1069-1073: an unrepresentable instant has no publication instant, and the early
    # return (1009-1012) sets nothing, so neither has a readiness-before-publication flag.
    ("unrepresentable-publication", lambda timeline, sample:
     timeline.get("ordering_reason") == "unrepresentable-instant"
     and timeline.get("ready_before_publication") is not None,
     "an unrepresentable instant has no publication instant, so no readiness-before-publication flag"),
    ("ordered-prerequisites", lambda timeline, sample: timeline.get("ordering") == "ordered"
     and (timeline.get("ready_before_publication") is None or timeline.get("flood_services") is None
          or timeline.get("m3_complete") is None
          or (timeline.get("admission") != "fallback" and timeline.get("permit_present_ns") is None)),
     "an ordered sample read its readiness against the publication, its flood interval and, unless it fell back, "
     "its permit occupancy"),
    ("sub-parts", lambda timeline, sample: _timeline_sub_parts_broken(timeline),
     "the two tail sub-parts are present with the parts and sum to render_exit_to_credited_end"),
    ("parts-sum", _timeline_parts_sum_broken,
     f"the seven parts sum to latency_ms within {perf_latency_schema.SPLIT_SUM_TOLERANCE_MS} ms"),
    ("split-parts", _timeline_split_parts_broken,
     f"an ordered timeline's first two parts are the split's first two, and its last five the split's "
     f"publication_to_present_ms, within {perf_latency_schema.SPLIT_SUM_TOLERANCE_MS} ms"),
    ("occupancy-sum", lambda timeline, sample: _timeline_occupancy_broken(timeline),
     "an ordered sample's permit present and absent time sum exactly to frame_request_to_entry"),
    ("tick-to-entry-without-qualified", lambda timeline, sample: (timeline.get("tick_to_entry_ns") is not None)
     != (timeline.get("tick_qualified") is True),
     "tick_to_entry_ns is present exactly when tick_qualified is true"),
    ("tick-qualified-identity", lambda timeline, sample: timeline.get("tick_qualified") is True
     and (timeline.get("permit_identity"), timeline.get("admission")) != ("known", "permit"),
     "a tick-qualified sample consumed a known permit"),
    ("permit-durations", lambda timeline, sample: (timeline.get("permit_present_ns") is None)
     != (timeline.get("permit_absent_ns") is None)
     or (timeline.get("permit_present_ns") is not None and timeline.get("admission") in (None, "fallback")),
     "permit durations come as a pair, never for a fallback or an unknown admission"),
    # timeline.rs:981-989: the read-to-parse duration ends at the split's parse instant, so it needs a split.
    ("read-to-parse-split", lambda timeline, sample: timeline.get("latest_read_to_parse_ns") is not None
     and sample.get("split_reason") != "split",
     "latest_read_to_parse_ns needs the sample's split, whose parse instant it ends at"),
    ("read-to-parse-stamp", lambda timeline, sample: timeline.get("latest_read_to_parse_ns") is not None
     and timeline.get("read_stamp") != "stamped",
     "latest_read_to_parse_ns needs a retained chunk read"),
    ("flood-pair", lambda timeline, sample: (timeline.get("flood_services") is None)
     != (timeline.get("m3_complete") is None),
     "flood_services and m3_complete come as a pair"),
    ("ready-before-publication-ordered", lambda timeline, sample: timeline.get("ready_before_publication") is True
     and timeline.get("ordering") == "ordered",
     "readiness before publication is never forced into the order"),
)


def _timeline_problem(timeline: object, sample: dict) -> str | None:
    """The first rule one credited sample's timeline breaks, named, or None."""
    if not isinstance(timeline, dict):
        return "object: is neither null nor an object"
    for name, broken, requirement in TIMELINE_RULES:
        if broken(timeline, sample):
            return f"{name}: {requirement}"
    return None


def _timeline_ordered(timeline: dict) -> bool:
    return timeline.get("availability") == "recorded" and timeline.get("ordering") == "ordered"


def _timeline_in_b2(timeline: dict) -> bool:
    return (_timeline_ordered(timeline) and timeline.get("admission") != "fallback"
            and timeline.get("permit_present_ns") is not None and timeline.get("permit_absent_ns") is not None)


def _timeline_in_m4(timeline: dict) -> bool:
    return _timeline_ordered(timeline) and timeline.get("tick_qualified") is True


def _timeline_in_m3(timeline: dict) -> bool:
    return _timeline_ordered(timeline) and timeline.get("m3_complete") is True


def _timeline_read_stamped(timeline: dict) -> bool:
    return _timeline_ordered(timeline) and timeline.get("read_stamp") == "stamped"


def timeline_coverage(timelines: Sequence[dict]) -> dict | None:
    """Every population recomputed from credited in-scope samples' timeline fields, each a named numerator and
    denominator. A missing or malformed timeline is passed in as an empty object: it stays in the credited
    denominator and joins no population.

    None when no credited sample was in scope; `unavailable`, with null populations, when none was recorded,
    which is never a measured zero."""
    credited = len(timelines)
    if credited == 0:
        return None

    def count(member) -> int:
        return sum(1 for timeline in timelines if member(timeline))

    def population(numerator: int, denominator: int) -> dict:
        return {"numerator": numerator, "denominator": denominator}

    recorded = count(lambda timeline: timeline.get("availability") == "recorded")
    common = {"schema": TIMELINE_SCHEMA, "credited": credited, "recorded": population(recorded, credited)}
    if recorded == 0:
        return {**common, "availability": "unavailable", **{name: None for name in TIMELINE_POPULATIONS}}
    ordered = count(_timeline_ordered)
    return {**common, "availability": "available",
            "ordered": population(ordered, credited),
            "b2": population(count(_timeline_in_b2), ordered),
            "m4": population(count(_timeline_in_m4), ordered),
            "m3": population(count(_timeline_in_m3), ordered),
            "read_stamped": population(count(_timeline_read_stamped), ordered),
            "b2_of_credited": population(count(_timeline_in_b2), credited),
            "m3_of_credited": population(count(_timeline_in_m3), credited)}


def _timeline_coverage_shape_problem(coverage: object) -> str | None:
    """Why a supplied coverage object has the wrong shape, or None: exactly its keys, strict integers (never a bool
    or a float, which compare equal to integers) and each population null or a numerator and a denominator."""
    if coverage is None:
        return None
    if not isinstance(coverage, dict) or set(coverage) != set(TIMELINE_COVERAGE_KEYS):
        return f"echo_timeline_coverage is not exactly {list(TIMELINE_COVERAGE_KEYS)}"
    if not (perf_latency_schema._is_int(coverage["schema"]) and coverage["schema"] == TIMELINE_SCHEMA):
        return f"echo_timeline_coverage schema {coverage['schema']!r} is not {TIMELINE_SCHEMA}"
    if coverage["availability"] not in ("available", "unavailable"):
        return f"echo_timeline_coverage availability {coverage['availability']!r} is not available or unavailable"
    if not _within(coverage["credited"], perf_attribution_schema.U64_MAX):
        return f"echo_timeline_coverage credited {coverage['credited']!r} is not a u64 count"
    for name in ("recorded", *TIMELINE_POPULATIONS):
        fraction = coverage[name]
        if fraction is None and name != "recorded":
            continue
        if not (isinstance(fraction, dict) and set(fraction) == {"numerator", "denominator"}
                and all(_within(fraction[key], perf_attribution_schema.U64_MAX) for key in fraction)):
            return f"echo_timeline_coverage {name} {fraction!r} is not a numerator and a denominator of u64 counts"
    return None


def echo_timeline_problems(latency: dict, echo_timeline_schema: int | None, in_scope: bool) -> list[str]:
    """Every way a latency object breaks the echo-timeline contract the harness declared.

    Without the declaration (a harness that predates it) nothing is checked, but a timeline or a coverage is then
    a problem. Under it, every sample carries the key; a credited in-scope sample carries an object, unavailable
    or rejected included; any other sample carries null; the coverage counts every credited in-scope sample and
    must equal the recomputation."""
    samples = latency.get("samples")
    samples = list(enumerate(samples)) if isinstance(samples, list) else []
    samples = [(position, sample) for position, sample in samples if isinstance(sample, dict)]
    written = "echo_timeline_coverage" in latency or any("echo_timeline" in sample for _, sample in samples)
    if echo_timeline_schema is None:
        # When: the harness declares no timeline schema, it predates the timeline and may write none of it.
        return ["echo_timeline is written, but the harness declares no echo_timeline_schema"] if written else []
    problems = [] if "echo_timeline_coverage" in latency else ["echo_timeline_coverage is missing"]
    timelines = []
    for position, sample in samples:
        if "echo_timeline" not in sample:
            problems.append(f"sample {position} is missing echo_timeline")
        timeline = sample.get("echo_timeline")
        credited = sample.get("latency_ms") is not None
        if not (credited and in_scope):
            if timeline is not None:
                # When: the sample is uncredited or out of scope, no credited pair was bound, so it has no timeline.
                problems.append(f"sample {position} echo_timeline timeline-out-of-scope: only a credited "
                                f"{' or '.join('/'.join(label) for label in TIMELINE_SCOPE)} sample has one")
            continue
        if timeline is None:
            problems.append(f"sample {position} echo_timeline timeline-missing: a credited in-scope sample carries "
                            f"an object, unavailable or rejected included")
        else:
            problem = _timeline_problem(timeline, sample)
            if problem:
                problems.append(f"sample {position} echo_timeline {problem}")
        timelines.append(timeline if isinstance(timeline, dict) else {})
    if "echo_timeline_coverage" in latency:
        supplied = latency["echo_timeline_coverage"]
        shape = _timeline_coverage_shape_problem(supplied)
        recomputed = timeline_coverage(timelines)
        if shape is not None:
            problems.append(shape)
        elif supplied != recomputed:
            problems.append(f"echo_timeline_coverage is {supplied!r}, but the samples give {recomputed!r}")
    return problems
