"""Perf attribution schema."""

from __future__ import annotations

import perf_latency_schema


# Why a counted phase has no attribution watch; the comparison reads each as unavailable, never as passed.
ATTRIBUTION_UNAVAILABLE = ("api-disabled", "counters-off", "no-baseline", "not-armed")
_SYNC_KEYS = {"set", "epoch", "resets"}
# The App's counts are `u64`; a value outside that range cannot have come from it.
U64_MAX = 2 ** 64 - 1


def _is_count(value: object) -> bool:
    """Whether `value` is an unsigned 64-bit count."""
    return perf_latency_schema._is_int(value) and 0 <= value <= U64_MAX


def _sync_reading_ok(value: object) -> bool:
    """Whether `value` is a pane's synchronized-output reading: a boolean `set` and unsigned counts."""
    return (isinstance(value, dict) and set(value) == _SYNC_KEYS and isinstance(value["set"], bool)
            and _is_count(value["epoch"]) and _is_count(value["resets"]))


def _armed_record_shaped(record: dict) -> bool:
    """Whether an armed record has every field with its type: unsigned counts and well-formed readings."""
    return (all(_is_count(record.get(key)) for key in ("arming", "pane", "updates", "seq_start"))
            and (record.get("seq_end") is None or _is_count(record.get("seq_end")))
            and _sync_reading_ok(record.get("baseline"))
            and (record.get("end") is None or _sync_reading_ok(record.get("end"))))


def armed_record_contradictions(record: dict) -> list[str]:
    """What a well-shaped armed record says against itself: arming needs a closed baseline, and the pane's resets
    and epoch only grow, so the final reading is never below the baseline and the sequence never runs backwards."""
    problems = []
    baseline, end = record["baseline"], record.get("end")
    if baseline["set"]:
        problems.append("the baseline shows an open update, which arming refuses")
    if end is not None:
        for key in ("epoch", "resets"):
            if end[key] < baseline[key]:
                problems.append(f"the final {key} {end[key]} is below the baseline's {baseline[key]}")
    if record.get("seq_end") is not None and record["seq_end"] < record["seq_start"]:
        problems.append(f"the sequence ends at {record['seq_end']}, before its start {record['seq_start']}")
    return problems


def attribution_problems(data: dict, attribution_api: bool | None, schema: int | None) -> list[str]:
    """Every problem with a result's S10 attribution: the recorded cfg against the comparison's, and each counted
    phase's record. A harness that declares no schema records none, so nothing is checked. A phase is armed
    only in a build that calls the API: a side never inherits the other side's availability."""
    if schema is None:
        return []
    problems = []
    recorded = data.get("s10_attribution_api")
    if not isinstance(recorded, bool):
        problems.append(f"s10_attribution_api is {recorded!r}, not a boolean")
    elif attribution_api is not None and recorded != attribution_api:
        problems.append(f"s10_attribution_api is {recorded}, but the comparison built with the cfg "
                        f"{'on' if attribution_api else 'off'}")
    for phase in data.get("phases") or []:
        if not isinstance(phase, dict) or "s10_attribution" not in phase:
            continue
        name, record = phase.get("name"), phase["s10_attribution"]
        state = record.get("state") if isinstance(record, dict) else None
        if state == "unavailable":
            if record.get("reason") not in ATTRIBUTION_UNAVAILABLE or set(record) != {"state", "reason"}:
                problems.append(f"phase {name!r} s10_attribution {record!r} names no known reason")
        elif state == "armed":
            if not _armed_record_shaped(record):
                problems.append(f"phase {name!r} s10_attribution is armed but malformed: {record!r}")
            elif record["updates"] != phase.get("updates"):
                problems.append(f"phase {name!r} s10_attribution updates {record['updates']} are not the "
                                f"phase's {phase.get('updates')!r}")
            else:
                problems.extend(f"phase {name!r} s10_attribution: {problem}"
                                for problem in armed_record_contradictions(record))
            if recorded is not True:
                problems.append(f"phase {name!r} is armed, but this build does not call the attribution API")
        else:
            problems.append(f"phase {name!r} s10_attribution {record!r} is neither unavailable nor armed")
    return problems


def occlusion_invalidated(data: dict) -> bool:
    """Whether an invalid result names an occlusion, the one environmental invalidation the smoke retries.

    The harness writes its reason to `invalid_reason`, which is read with `notes` and `reason`;
    any mention of occlusion counts.
    """
    if data.get("status") == "valid":
        return False
    texts = [note for note in data.get("notes") or [] if isinstance(note, str)]
    texts.extend(str(data[key]) for key in ("reason", "invalid_reason") if data.get(key))
    return any("occlu" in text.lower() for text in texts)
