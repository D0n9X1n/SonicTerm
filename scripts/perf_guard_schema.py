"""Perf guard schema."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import hashlib
import json
from pathlib import Path
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_latency_schema


# ---------------------------------------------------------------------------------------------------------------
# Guard correlation: each phase's raw take, validated as one chain and joined offline.
# ---------------------------------------------------------------------------------------------------------------

# The phase record's `guard_correlation.status` values; a phase record without the field is incomplete.
GUARD_STATUSES = ("written", "unavailable", "gate_off", "take_exhausted", "write_failed")
# Statuses with no take: the App was not called or counted nothing, so no take_seq is consumed.
GUARD_NO_TAKE = ("unavailable", "gate_off")
# Each status's availability class; one run's phases share one class, the one its build and gate decide.
GUARD_CLASS = {"unavailable": "unavailable", "gate_off": "gate_off", "written": "take",
               "take_exhausted": "take", "write_failed": "take"}
GUARD_FIELD_KEYS = ("status", "file", "take_seq", "bytes", "sha256", "window")
GUARD_WINDOW_KEYS = ("label", "start_ns", "end_ns")
GUARD_SIDECAR_DIR = "guard-correlation"
GUARD_WINDOW_LABEL = "phase start to counter-end observation"
GUARD_U64_MAX = 2**64 - 1
# Recording capacities, frozen with the App's records.
GUARD_SECTION_CAPACITY = 32_768
GUARD_ABANDONED_CAPACITY = 64
GUARD_LOSS_CAPACITY = 64
GUARD_SPAN_CAPACITY = 16_384
_GUARD_NONCE = re.compile(r"[0-9a-f]{32}")
_GUARD_HASH = re.compile(r"[0-9a-f]{64}")
_GUARD_HEADER = ("schema", "run_nonce", "harness_hash", "phase_index", "phase_name", "take_seq", "clock", "window",
                 "taken_at_ns", "panes", "spans")
_GUARD_PANE_KEYS = ("pane_id", "closed", "identities_exhausted", "prev_next_section_seq", "next_section_seq",
                    "carried_pending", "records", "pending", "abandoned", "abandoned_overflow", "abandoned_unlocated",
                    "issued_dropped_located", "issued_dropped_unlocated", "losses", "loss_events", "losses_merged",
                    "refused_closed", "refused_exhausted", "refused_unlocated", "first_refusal_ns")
_GUARD_PANE_U64 = ("pane_id", "prev_next_section_seq", "next_section_seq", "abandoned_overflow", "abandoned_unlocated",
                   "issued_dropped_located", "issued_dropped_unlocated", "loss_events", "refused_closed",
                   "refused_exhausted", "refused_unlocated")
_GUARD_BATCH_KEYS = ("spans", "spans_issued", "spans_dropped_located", "spans_dropped_unlocated", "losses",
                     "loss_events", "losses_merged", "collections_issued", "collections_refused_exhausted",
                     "prev_next_collection_seq", "next_collection_seq", "identities_exhausted", "open_collections")
_GUARD_BATCH_U64 = ("spans_issued", "spans_dropped_located", "spans_dropped_unlocated", "loss_events",
                    "collections_issued", "collections_refused_exhausted", "prev_next_collection_seq",
                    "next_collection_seq", "open_collections")


def _is_u64(value: object) -> bool:
    return perf_latency_schema._is_int(value) and 0 <= value <= GUARD_U64_MAX


def _is_opt_u64(value: object) -> bool:
    return value is None or _is_u64(value)


def _same_opt_u64(left: object, right: object) -> bool:
    """Type-strict equality of two optional integers: a bool or a float never equals an integer here."""
    if left is None or right is None:
        return left is None and right is None
    return perf_latency_schema._is_int(left) and perf_latency_schema._is_int(right) and left == right


def _guard_window_ok(window: object) -> bool:
    return (isinstance(window, dict) and set(window) == set(GUARD_WINDOW_KEYS)
            and window["label"] == GUARD_WINDOW_LABEL and isinstance(window["label"], str)
            and _is_opt_u64(window["start_ns"]) and _is_opt_u64(window["end_ns"]))


def guard_field_problems(value: object) -> list[str]:
    """Why a phase record's `guard_correlation` is malformed: every key present, the window well-formed, and the
    other keys shaped by the status. Only `written` names a file, size and digest; a take_seq is named only when
    a take was issued (`written`, `write_failed`)."""
    if not isinstance(value, dict):
        return ["guard_correlation is not an object"]
    if set(value) != set(GUARD_FIELD_KEYS):
        return [f"guard_correlation keys are {sorted(value)}, not {sorted(GUARD_FIELD_KEYS)}"]
    status = value["status"]
    if not isinstance(status, str) or status not in GUARD_STATUSES:
        return [f"guard_correlation status {status!r} is not one of {GUARD_STATUSES}"]
    problems = []
    if not _guard_window_ok(value["window"]):
        problems.append("guard_correlation window is malformed")
    if status == "written":
        if not (isinstance(value["file"], str) and _is_u64(value["take_seq"]) and _is_u64(value["bytes"])
                and isinstance(value["sha256"], str) and _GUARD_HASH.fullmatch(value["sha256"])):
            problems.append("a written guard_correlation lacks its file, take_seq, bytes or sha256")
        return problems
    if any(value[key] is not None for key in ("file", "bytes", "sha256")):
        problems.append(f"a {status} guard_correlation names a file, size or digest")
    if not (_is_u64(value["take_seq"]) if status == "write_failed" else value["take_seq"] is None):
        problems.append(f"a {status} guard_correlation has the wrong take_seq")
    return problems


def _guard_field_ok(value: object) -> bool:
    return not guard_field_problems(value)


def guard_availability(guard_api: bool | None, frame_counters: object) -> str | None:
    """The availability class the run's build and gate decide: `unavailable` without the cfg, `take` with it and
    the gate on, `gate_off` with it and the gate off; None when nothing decided the cfg."""
    if guard_api is None:
        return None
    if not guard_api:
        return "unavailable"
    return "take" if frame_counters == "on" else "gate_off"


def guard_availability_problem(fields: Iterable[object], availability: str | None) -> str | None:
    """Why the phases' guard-correlation statuses cannot belong to one run, or None. One run's statuses share one
    availability class, the one its build and gate decide (`availability`, None when nothing decided it). The live
    join and perf-flags' artifact reader both call this, so neither accepts what the other rejects."""
    classes = {GUARD_CLASS[value["status"]] for value in fields
               if isinstance(value, dict) and value.get("status") in GUARD_CLASS}
    if len(classes) > 1:
        return f"availability changes within the run: {sorted(classes)}"
    if availability is not None and classes and classes != {availability}:
        return f"status class {sorted(classes)} is impossible for a run whose availability is {availability}"
    return None


def guard_overlap(records: Sequence[tuple[int, int]], spans: Sequence[tuple[int, int]]) -> tuple[int, int, int]:
    """The observed overlap of one pane's worker waits with its UI spans: (overlap ns, sections with a positive
    overlap, distinct spans overlapping any wait). `spans` must be disjoint; both are sorted here, so input order
    never changes the result. Touching intervals overlap by 0."""
    waits = sorted(records)
    held = sorted(spans)
    overlap_ns, sections, touched = 0, 0, set()
    first_open = 0
    for wait_from, wait_to in waits:
        while first_open < len(held) and held[first_open][1] <= wait_from:
            first_open += 1
        wait_overlap = 0
        cursor = first_open
        while cursor < len(held) and held[cursor][0] < wait_to:
            span_from, span_to = held[cursor]
            shared = min(wait_to, span_to) - max(wait_from, span_from)
            if shared > 0:
                wait_overlap += shared
                touched.add(cursor)
            cursor += 1
        overlap_ns += wait_overlap
        sections += wait_overlap > 0
    return overlap_ns, sections, len(touched)


def _guard_intervals(items: object, keys: tuple[str, ...]) -> bool:
    return isinstance(items, list) and all(isinstance(item, dict) and set(item) == set(keys)
                                           and all(_is_u64(item[key]) for key in keys) for item in items)


def _guard_shape(sidecar: object) -> list[str]:
    """Named type problems in a sidecar; an empty list means every later check can index it safely. Every frozen
    field is required, so a missing one is never read as null."""
    if not isinstance(sidecar, dict):
        return ["the sidecar is not an object"]
    if list(sidecar) != list(_GUARD_HEADER):
        return [f"the sidecar's keys are {list(sidecar)}, not the frozen header"]
    problems = []
    if not (perf_latency_schema._is_int(sidecar["schema"]) and sidecar["schema"] == 1):
        problems.append(f"schema {sidecar['schema']!r} is not the integer 1")
    clock = sidecar["clock"]
    if not (isinstance(clock, dict) and set(clock) == {"pid", "run_nonce"} and _is_u64(clock["pid"])
            and isinstance(clock["run_nonce"], str)):
        problems.append("clock is malformed")
    if not _guard_window_ok(sidecar["window"]):
        problems.append("window is malformed")
    if not (_is_u64(sidecar["take_seq"]) and _is_opt_u64(sidecar["taken_at_ns"]) and _is_u64(sidecar["phase_index"])
            and isinstance(sidecar["phase_name"], str) and isinstance(sidecar["run_nonce"], str)
            and (sidecar["harness_hash"] is None or isinstance(sidecar["harness_hash"], str))):
        problems.append("a header field has the wrong type")
    panes = sidecar["panes"]
    if not isinstance(panes, list) or not all(isinstance(pane, dict) for pane in panes):
        return problems + ["panes is not a list of objects"]
    for position, pane in enumerate(panes):
        if set(pane) != set(_GUARD_PANE_KEYS):
            missing = sorted(set(_GUARD_PANE_KEYS) - set(pane))
            extra = sorted(set(pane) - set(_GUARD_PANE_KEYS))
            problems.append(f"pane {position} lacks {missing} or adds {extra}")
            continue
        pending = pane["pending"]
        if not (all(_is_u64(pane[key]) for key in _GUARD_PANE_U64)
                and all(isinstance(pane[key], bool) for key in ("closed", "identities_exhausted", "losses_merged"))
                and _is_opt_u64(pane["carried_pending"]) and _is_opt_u64(pane["first_refusal_ns"])
                and _guard_intervals(pane["records"], ("section_seq", "before_lock_ns", "locked_at_ns"))
                and _guard_intervals(pane["abandoned"], ("section_seq", "registered_ns", "abandoned_ns"))
                and _guard_intervals(pane["losses"], ("from_ns", "to_ns"))
                and (pending is None or (isinstance(pending, dict) and set(pending) == {"section_seq", "registered_ns"}
                                         and _is_u64(pending["section_seq"])
                                         and _is_opt_u64(pending["registered_ns"])))):
            problems.append(f"pane {position} has a field of the wrong type")
    batch = sidecar["spans"]
    if not isinstance(batch, dict) or set(batch) != set(_GUARD_BATCH_KEYS):
        return problems + ["spans lacks a frozen field or adds one"]
    if not (all(_is_u64(batch[key]) for key in _GUARD_BATCH_U64)
            and isinstance(batch["losses_merged"], bool) and isinstance(batch["identities_exhausted"], bool)
            and _guard_intervals(batch["spans"], ("pane_id", "collection_seq", "acquired_ns", "released_ns"))
            and _guard_intervals(batch["losses"], ("from_ns", "to_ns"))):
        problems.append("spans has a field of the wrong type")
    return problems


@dataclass
class GuardPaneState:
    """One pane's evidence after its last validated take."""

    next_section_seq: int
    # The pending identity with its original registration time (None when it was unreadable).
    pending: tuple[int, int | None] | None
    closed: bool
    exhausted: bool
    prunable: bool
    # The latest `locked_at_ns` of any published section, -1 before the first.
    last_locked_ns: int


@dataclass
class GuardChain:
    """What take k−1 left for take k: identities, clocks, sticky flags and each pane's last state."""

    take_seq: int = 0
    taken_at_ns: int = -1
    run_nonce: str | None = None
    pid: int | None = None
    harness_hash: str | None = None
    next_collection_seq: int = 1
    spans_exhausted: bool = False
    panes: dict[int, GuardPaneState] = field(default_factory=dict)
    # Each pane's latest released span, -1 before the first.
    span_released: dict[int, int] = field(default_factory=dict)


def _guard_pane_problems(pane: dict, chain: GuardChain) -> list[str]:
    """§5.1's pane rules for one transfer, against the chain: identities, accounting, consistency, and clock
    continuity with the previous take."""
    pane_id = pane["pane_id"]
    before = chain.panes.get(pane_id)
    problems = []
    prev_next, next_seq = pane["prev_next_section_seq"], pane["next_section_seq"]
    expected_prev = 1 if before is None else before.next_section_seq
    if prev_next != expected_prev:
        problems.append(f"pane {pane_id}: prev_next_section_seq {prev_next}, expected {expected_prev}")
    if next_seq < prev_next:
        problems.append(f"pane {pane_id}: next_section_seq {next_seq} is below prev_next {prev_next}")
    if next_seq == GUARD_U64_MAX and not pane["identities_exhausted"]:
        problems.append(f"pane {pane_id}: next_section_seq is u64::MAX without identities_exhausted")
    if before is not None and ((before.closed and not pane["closed"])
                               or (before.exhausted and not pane["identities_exhausted"])):
        problems.append(f"pane {pane_id}: a sticky flag went from true to false")
    carried = pane["carried_pending"]
    carried_before = before.pending if before is not None else None
    if carried != (carried_before[0] if carried_before is not None else None):
        problems.append(f"pane {pane_id}: carried_pending {carried} is not the previous take's pending identity")
    identities = [record["section_seq"] for record in pane["records"]]
    identities += [entry["section_seq"] for entry in pane["abandoned"]]
    if pane["pending"] is not None:
        identities.append(pane["pending"]["section_seq"])
    for identity in identities:
        if not (prev_next <= identity < next_seq or identity == carried):
            problems.append(f"pane {pane_id}: identity {identity} is outside [{prev_next}, {next_seq}) and not carried")
    if len(identities) != len(set(identities)):
        problems.append(f"pane {pane_id}: an identity appears more than once")
    resolved = (len(pane["records"]) + len(pane["abandoned"]) + pane["abandoned_overflow"]
                + pane["abandoned_unlocated"] + pane["issued_dropped_located"] + pane["issued_dropped_unlocated"]
                + (pane["pending"] is not None))
    issued = next_seq - prev_next + (carried is not None)
    if resolved != issued:
        problems.append(f"pane {pane_id}: {resolved} dispositions for {issued} issued identities")
    if pane["loss_events"] != pane["issued_dropped_located"] + pane["abandoned_overflow"]:
        problems.append(f"pane {pane_id}: loss_events is not issued_dropped_located + abandoned_overflow")
    if pane["loss_events"] > 0 and not pane["losses"]:
        problems.append(f"pane {pane_id}: loss_events without losses")
    located_refusals = pane["refused_closed"] + pane["refused_exhausted"] - pane["refused_unlocated"]
    if located_refusals < 0 or (located_refusals > 0) != (pane["first_refusal_ns"] is not None):
        problems.append(f"pane {pane_id}: refusal counts disagree with first_refusal_ns and refused_unlocated")
    if (len(pane["records"]) > GUARD_SECTION_CAPACITY or len(pane["abandoned"]) > GUARD_ABANDONED_CAPACITY
            or len(pane["losses"]) > GUARD_LOSS_CAPACITY):
        problems.append(f"pane {pane_id}: a list exceeds its capacity")
    if any(entry["registered_ns"] > entry["abandoned_ns"] for entry in pane["abandoned"]) \
            or any(loss["from_ns"] > loss["to_ns"] for loss in pane["losses"]):
        problems.append(f"pane {pane_id}: an interval ends before it starts")
    # Clock continuity: a worker's sections are sequential, so each starts after the previous one locked,
    # including the previous take's; a section issued after the previous take registered after its clock read.
    previous_locked = before.last_locked_ns if before is not None else -1
    for record in sorted(pane["records"], key=lambda record: record["section_seq"]):
        if record["before_lock_ns"] > record["locked_at_ns"] or record["before_lock_ns"] < previous_locked:
            problems.append(f"pane {pane_id}: section {record['section_seq']} is out of clock order")
        if record["section_seq"] != carried and record["before_lock_ns"] < chain.taken_at_ns:
            problems.append(f"pane {pane_id}: section {record['section_seq']} predates the previous take")
        previous_locked = record["locked_at_ns"]
    for entry in pane["abandoned"]:
        if entry["section_seq"] != carried and entry["registered_ns"] < chain.taken_at_ns:
            problems.append(f"pane {pane_id}: abandoned section {entry['section_seq']} predates the previous take")
    pending = pane["pending"]
    if pending is not None and pending["section_seq"] != carried and pending["registered_ns"] is not None \
            and pending["registered_ns"] < chain.taken_at_ns:
        problems.append(f"pane {pane_id}: pending section {pending['section_seq']} predates the previous take")
    if carried is not None and carried_before is not None:
        registered = carried_before[1]
        if pending is not None and pending["section_seq"] == carried \
                and not _same_opt_u64(pending["registered_ns"], registered):
            problems.append(f"pane {pane_id}: carried section {carried} changed its registration time")
        for entry in pane["abandoned"]:
            if entry["section_seq"] == carried and not _same_opt_u64(entry["registered_ns"], registered):
                problems.append(f"pane {pane_id}: carried section {carried} changed its registration time")
        for record in pane["records"]:
            if record["section_seq"] == carried and registered is not None and record["before_lock_ns"] < registered:
                problems.append(f"pane {pane_id}: carried section {carried} locked before it registered")
    return problems


def _guard_span_problems(batch: dict, chain: GuardChain, pane_ids: set[int]) -> list[str]:
    """§5.1's span rules for the batch against the chain, with clock continuity across takes."""
    problems = []
    prev_next, next_seq = batch["prev_next_collection_seq"], batch["next_collection_seq"]
    if prev_next != chain.next_collection_seq:
        problems.append(f"spans: prev_next_collection_seq {prev_next}, expected {chain.next_collection_seq}")
    if next_seq < prev_next or (next_seq == GUARD_U64_MAX and not batch["identities_exhausted"]):
        problems.append("spans: next_collection_seq is out of range")
    if chain.spans_exhausted and not batch["identities_exhausted"]:
        problems.append("spans: a sticky flag went from true to false")
    keys = [(span["pane_id"], span["collection_seq"]) for span in batch["spans"]]
    if len(keys) != len(set(keys)):
        problems.append("spans: (pane_id, collection_seq) is not unique")
    if any(not prev_next <= seq < next_seq for _, seq in keys):
        problems.append("spans: a collection_seq is outside the issued range")
    collections = {seq for _, seq in keys}
    if len(collections) > batch["collections_issued"] or batch["collections_issued"] > next_seq - prev_next:
        problems.append("spans: recorded collections disagree with collections_issued")
    if batch["spans_issued"] != len(batch["spans"]) + batch["spans_dropped_located"] + batch["spans_dropped_unlocated"]:
        problems.append("spans: spans_issued does not equal its dispositions")
    if (batch["loss_events"] > 0) != bool(batch["losses"]):
        problems.append("spans: loss_events and losses disagree")
    if batch["spans_dropped_located"] > 0 and batch["loss_events"] == 0:
        problems.append("spans: located drops without a loss event")
    if any(loss["from_ns"] > loss["to_ns"] for loss in batch["losses"]):
        problems.append("spans: a loss interval ends before it starts")
    if any(pane_id not in pane_ids for pane_id, _ in keys):
        problems.append("spans: a span's pane has no container")
    if len(batch["spans"]) > GUARD_SPAN_CAPACITY or len(batch["losses"]) > GUARD_LOSS_CAPACITY:
        problems.append("spans: a list exceeds its capacity")
    by_pane: dict[int, list[tuple[int, int]]] = {}
    for span in batch["spans"]:
        if span["acquired_ns"] > span["released_ns"]:
            problems.append("spans: a span is released before it is acquired")
        if span["acquired_ns"] < chain.taken_at_ns:
            problems.append("spans: a span predates the previous take")
        by_pane.setdefault(span["pane_id"], []).append((span["acquired_ns"], span["released_ns"]))
    for pane_id, intervals in by_pane.items():
        intervals.sort()
        if any(later[0] < earlier[1] for earlier, later in zip(intervals, intervals[1:])) \
                or intervals[0][0] < chain.span_released.get(pane_id, -1):
            problems.append(f"spans: pane {pane_id}'s spans overlap")
    return problems


def guard_take_problems(sidecar: dict, phase_index: int, phase_name: str, phase_window: dict, chain: GuardChain,
                        expected_hash: str | None) -> list[str]:
    """§5.1: why take k does not validate against the chain and its own phase record; empty when it does. The
    chain is not advanced."""
    problems = _guard_shape(sidecar)
    if problems:
        return problems
    window = sidecar["window"]
    if not all(_same_opt_u64(window[key], phase_window[key]) for key in ("start_ns", "end_ns")):
        problems.append("the sidecar's window differs from its phase record's latched window")
    taken_at = sidecar["taken_at_ns"]
    if taken_at is None:
        problems.append("taken_at_ns is unknown")
    elif taken_at <= chain.taken_at_ns:
        problems.append("taken_at_ns does not increase")
    edges = [value for value in (window["start_ns"], window["end_ns"], taken_at) if value is not None]
    if any(earlier > later for earlier, later in zip(edges, edges[1:])):
        problems.append("the window is not ordered start ≤ end ≤ taken_at_ns")
    if sidecar["phase_index"] != phase_index or sidecar["phase_name"] != phase_name:
        problems.append(f"the sidecar names phase {sidecar['phase_index']} {sidecar['phase_name']!r}, "
                        f"not its own record's {phase_index} {phase_name!r}")
    if sidecar["take_seq"] != chain.take_seq + 1:
        problems.append(f"take_seq {sidecar['take_seq']} does not follow {chain.take_seq}")
    nonce = sidecar["run_nonce"]
    if not _GUARD_NONCE.fullmatch(nonce) or sidecar["clock"]["run_nonce"] != nonce \
            or (chain.run_nonce is not None and nonce != chain.run_nonce):
        problems.append("the run nonce is malformed or differs within the run")
    if chain.pid is not None and sidecar["clock"]["pid"] != chain.pid:
        problems.append("the clock's pid differs within the run")
    harness = sidecar["harness_hash"]
    if harness is not None and ((chain.harness_hash is not None and harness != chain.harness_hash)
                                or (expected_hash is not None and harness != expected_hash)):
        problems.append("the harness hash differs from the run's")
    pane_ids = [pane["pane_id"] for pane in sidecar["panes"]]
    if len(pane_ids) != len(set(pane_ids)):
        problems.append("pane_id is not unique")
    for pane_id, before in chain.panes.items():
        if not before.prunable and pane_id not in pane_ids:
            problems.append(f"pane {pane_id} vanished without being closed and drained")
    for pane in sidecar["panes"]:
        problems.extend(_guard_pane_problems(pane, chain))
    problems.extend(_guard_span_problems(sidecar["spans"], chain, set(pane_ids)))
    return problems


def _guard_advance(chain: GuardChain, sidecar: dict) -> None:
    """Advance the chain past a validated take."""
    chain.take_seq = sidecar["take_seq"]
    chain.taken_at_ns = sidecar["taken_at_ns"]
    chain.run_nonce, chain.pid = sidecar["run_nonce"], sidecar["clock"]["pid"]
    chain.harness_hash = sidecar["harness_hash"] or chain.harness_hash
    chain.next_collection_seq = sidecar["spans"]["next_collection_seq"]
    chain.spans_exhausted = sidecar["spans"]["identities_exhausted"]
    present = set()
    for pane in sidecar["panes"]:
        present.add(pane["pane_id"])
        before = chain.panes.get(pane["pane_id"])
        pending = pane["pending"]
        last_locked = max([record["locked_at_ns"] for record in pane["records"]]
                          + [before.last_locked_ns if before is not None else -1])
        chain.panes[pane["pane_id"]] = GuardPaneState(
            pane["next_section_seq"], (pending["section_seq"], pending["registered_ns"]) if pending else None,
            pane["closed"], pane["identities_exhausted"], pane["closed"] and pending is None, last_locked)
    chain.panes = {pane_id: state for pane_id, state in chain.panes.items()
                   if not state.prunable or pane_id in present}
    for span in sidecar["spans"]["spans"]:
        chain.span_released[span["pane_id"]] = max(chain.span_released.get(span["pane_id"], -1), span["released_ns"])


def guard_window_reasons(sidecar: dict) -> list[str]:
    """§5.2: why a validated take's window is incomplete; empty when it is complete. Nothing is clipped: a
    section or span crossing an edge makes the window incomplete."""
    window = sidecar["window"]
    start_ns, end_ns = window["start_ns"], window["end_ns"]
    if start_ns is None or end_ns is None:
        return ["the window is unknown"]
    reasons = []
    batch = sidecar["spans"]
    for pane in sidecar["panes"]:
        pane_id = pane["pane_id"]
        for key in ("issued_dropped_unlocated", "abandoned_unlocated", "refused_unlocated"):
            if pane[key]:
                reasons.append(f"pane {pane_id}: unlocated {key}")
        pending = pane["pending"]
        if pending is not None and (pending["registered_ns"] is None or pending["registered_ns"] <= end_ns):
            reasons.append(f"pane {pane_id}: section {pending['section_seq']} is pending")
        if any(entry["registered_ns"] <= end_ns and entry["abandoned_ns"] >= start_ns for entry in pane["abandoned"]):
            reasons.append(f"pane {pane_id}: an abandoned section meets the window")
        if any(loss["from_ns"] <= end_ns and loss["to_ns"] >= start_ns for loss in pane["losses"]):
            reasons.append(f"pane {pane_id}: a loss interval meets the window")
        if pane["first_refusal_ns"] is not None and pane["first_refusal_ns"] <= end_ns:
            reasons.append(f"pane {pane_id}: a refused section's wait was not recorded")
        if pane["identities_exhausted"]:
            reasons.append(f"pane {pane_id}: identities exhausted")
        for record in pane["records"]:
            before, locked = record["before_lock_ns"], record["locked_at_ns"]
            if before < start_ns < locked or before < end_ns < locked:
                reasons.append(f"pane {pane_id}: section {record['section_seq']} crosses the window's edge")
    for span in batch["spans"]:
        acquired, released = span["acquired_ns"], span["released_ns"]
        if acquired < start_ns < released or acquired < end_ns < released:
            reasons.append(f"spans: pane {span['pane_id']}'s collection {span['collection_seq']} crosses the edge")
    if batch["spans_dropped_unlocated"]:
        reasons.append("spans: unlocated spans_dropped_unlocated")
    if batch["identities_exhausted"] or batch["collections_refused_exhausted"]:
        reasons.append("spans: collection identities exhausted")
    if batch["open_collections"]:
        reasons.append("spans: a collection was open at the take")
    if any(loss["from_ns"] <= end_ns and loss["to_ns"] >= start_ns for loss in batch["losses"]):
        reasons.append("spans: a loss interval meets the window")
    return reasons


def guard_window_join(sidecar: dict) -> dict[str, int]:
    """§5.3 for a complete window: per pane, its records inside the window against its spans in it."""
    start_ns, end_ns = sidecar["window"]["start_ns"], sidecar["window"]["end_ns"]
    totals = {"observed_overlap_ns": 0, "observed_overlap_sections": 0, "observed_overlapping_spans": 0}
    for pane in sidecar["panes"]:
        records = [(record["before_lock_ns"], record["locked_at_ns"]) for record in pane["records"]
                   if start_ns <= record["before_lock_ns"] and record["locked_at_ns"] <= end_ns]
        spans = [(span["acquired_ns"], span["released_ns"]) for span in sidecar["spans"]["spans"]
                 if span["pane_id"] == pane["pane_id"] and start_ns <= span["acquired_ns"]
                 and span["released_ns"] <= end_ns]
        overlap_ns, sections, touched = guard_overlap(records, spans)
        totals["observed_overlap_ns"] += overlap_ns
        totals["observed_overlap_sections"] += sections
        totals["observed_overlapping_spans"] += touched
    return totals


def _guard_load(root: Path, field_value: dict) -> tuple[dict | None, str | None]:
    """A written phase's sidecar, read and checked against its record's size and digest; else why not."""
    file_name = field_value["file"]
    if not re.fullmatch(rf"{GUARD_SIDECAR_DIR}/[0-9]+-[a-z0-9-]+\.json", file_name):
        return None, f"the sidecar path {file_name!r} is not a guard-correlation file"
    try:
        raw = (root / file_name).read_bytes()
    except OSError as error:
        return None, f"the sidecar is missing: {error.strerror or error}"
    if len(raw) != field_value["bytes"]:
        return None, f"the sidecar is {len(raw)} bytes, not the recorded {field_value['bytes']}"
    if hashlib.sha256(raw).hexdigest() != field_value["sha256"]:
        return None, "the sidecar's digest does not match its record"
    try:
        sidecar = json.loads(raw)
    except ValueError as error:
        return None, f"the sidecar cannot be parsed: {error}"
    if not isinstance(sidecar, dict):
        return None, "the sidecar is not an object"
    if not _same_opt_u64(sidecar.get("take_seq"), field_value["take_seq"]):
        return None, "the sidecar's take_seq differs from its record"
    return sidecar, None


def guard_correlation_verdicts(phases: Sequence[object], root: Path, expected_hash: str | None,
                               availability: str | None = None) -> list[dict]:
    """Each phase's guard-correlation verdict: `complete` with its overlap, `incomplete` with reasons, or `n/a`.

    Takes are validated in phase order as one chain (§5.1): a missing, truncated, mismatched, misbound,
    write_failed, take_exhausted or malformed take makes itself and every later take incomplete. Two run-wide
    rules are decided before any verdict is final: a null harness hash in any take makes every take incomplete
    (`harness_unbound`), and statuses of more than one availability class, or of a class the run's build and
    gate (`availability`) rule out, make every phase incomplete. An incomplete or unavailable verdict never
    carries a number.
    """
    raws = [phase.get("guard_correlation") if isinstance(phase, dict) else None for phase in phases]
    fields = [raw if raw is not None and not guard_field_problems(raw) else None for raw in raws]
    run_problem = guard_availability_problem(fields, availability)
    loaded: dict[int, tuple[dict | None, str | None]] = {}
    unbound = False
    for index, value in enumerate(fields):
        if value is not None and value["status"] == "written":
            loaded[index] = _guard_load(root, value)
            sidecar = loaded[index][0]
            if sidecar is not None and "harness_hash" in sidecar and sidecar["harness_hash"] is None:
                unbound = True
    chain = GuardChain()
    broken: str | None = None
    verdicts = []
    for index, phase in enumerate(phases):
        name = phase.get("name") if isinstance(phase, dict) else None
        value = fields[index]
        verdict = {"phase_index": index, "phase": name, "status": None, "verdict": "incomplete", "reasons": []}
        verdicts.append(verdict)
        if value is None:
            reason = ("the phase record has no guard_correlation" if raws[index] is None
                      else "guard_correlation: " + "; ".join(guard_field_problems(raws[index])))
            verdict["reasons"].append(reason)
            broken = broken or f"phase {index}: {reason}"
            continue
        status = value["status"]
        verdict["status"] = status
        if status in GUARD_NO_TAKE:
            verdict["verdict"] = "n/a"
            continue
        if broken is not None:
            verdict["reasons"].append(f"an earlier take broke the chain: {broken}")
        if status != "written":
            verdict["reasons"].append(f"the take is {status}")
            broken = broken or f"phase {index} is {status}"
            continue
        sidecar, problem = loaded[index]
        if sidecar is None:
            verdict["reasons"].append(problem)
            broken = broken or f"phase {index}: {problem}"
            continue
        if broken is not None:
            continue
        problems = guard_take_problems(sidecar, index, name, value["window"], chain, expected_hash)
        if problems:
            verdict["reasons"].extend(problems)
            broken = f"phase {index} did not validate"
            continue
        _guard_advance(chain, sidecar)
        reasons = guard_window_reasons(sidecar)
        if reasons:
            verdict["reasons"].extend(reasons)
            continue
        verdict["verdict"] = "complete"
        verdict.update(guard_window_join(sidecar))
    for verdict in verdicts:
        override = run_problem if run_problem is not None else ("harness_unbound" if unbound else None)
        if override is None or (run_problem is None and verdict["verdict"] == "n/a"):
            continue
        verdict["verdict"] = "incomplete"
        verdict["reasons"].insert(0, override)
        for key in ("observed_overlap_ns", "observed_overlap_sections", "observed_overlapping_spans"):
            verdict.pop(key, None)
    return verdicts


def format_guard_verdict(verdict: Mapping[str, object]) -> str:
    """One report line: the overlap of a complete phase, `incomplete` with its reasons, or `n/a`."""
    head = f"[perf-compare] guard correlation {verdict['phase']}:"
    if verdict["verdict"] == "n/a":
        return f"{head} n/a ({verdict['status']})"
    if verdict["verdict"] != "complete":
        return f"{head} incomplete: {'; '.join(verdict['reasons'])}"
    return (f"{head} observed overlap {verdict['observed_overlap_ns']} ns, "
            f"{verdict['observed_overlap_sections']} sections, {verdict['observed_overlapping_spans']} spans")
