"""Perf result schema."""

from __future__ import annotations
import math
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_attribution_schema
import perf_guard_schema
import perf_latency_schema
import perf_settings
import perf_timeline_schema


def _monitor_ok(monitor: object) -> bool:
    """A measurement display gives its name or null, its refresh in millihertz or null, and its scale factor."""
    return (isinstance(monitor, dict)
            and all(key in monitor for key in ("name", "refresh_rate_millihertz", "scale_factor"))
            and (monitor["name"] is None or isinstance(monitor["name"], str))
            and (monitor["refresh_rate_millihertz"] is None or perf_latency_schema._is_int(monitor["refresh_rate_millihertz"]))
            and perf_latency_schema._is_number(monitor["scale_factor"]))


# The presenter's boolean fields; `software_render_mode` is a string or null.
PRESENTER_FLAGS = ("software_rendering", "software_render_degraded", "windows_gdi")


def _presenter_ok(presenter: object) -> bool:
    """A presenter names its software render mode or null and three booleans."""
    return (isinstance(presenter, dict) and "software_render_mode" in presenter
            and (presenter["software_render_mode"] is None or isinstance(presenter["software_render_mode"], str))
            and all(isinstance(presenter.get(key), bool) for key in PRESENTER_FLAGS))


def _finite_non_negative(value: object) -> bool:
    """A number that is finite and at least zero; NaN, infinity, a negative and a boolean are not."""
    return perf_latency_schema._is_number(value) and math.isfinite(value) and value >= 0


def _checkpoint_ok(point: object) -> bool:
    """A checkpoint names its index, label and time; its footprint file is a path or null.

    Its optional `fresh_after_unix_s` is a finite, non-negative time, its optional
    `frame_texture_bytes` a non-negative integer, and its optional sampling record a known state, a
    non-negative attempt count and a boolean.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    return (isinstance(point, dict) and perf_latency_schema._is_int(point.get("index")) and isinstance(point.get("label"), str)
            and perf_latency_schema._is_number(point.get("unix_s"))
            and (point.get("footprint_file") is None or isinstance(point.get("footprint_file"), str))
            and ("fresh_after_unix_s" not in point or _finite_non_negative(point["fresh_after_unix_s"]))
            and ("frame_texture_bytes" not in point
                 or (perf_latency_schema._is_int(point["frame_texture_bytes"]) and point["frame_texture_bytes"] >= 0))
            and ("sampling" not in point or point["sampling"] in perf_metrics.CHECKPOINT_SAMPLING_STATES)
            and ("attempts" not in point or (perf_latency_schema._is_int(point["attempts"]) and point["attempts"] >= 0))
            and ("last_attempt_complete" not in point or isinstance(point["last_attempt_complete"], bool))
            and ("atlas_readings" not in point
                 or (isinstance(point["atlas_readings"], list)
                     and all(map(_atlas_reading_ok, point["atlas_readings"]))))
            and ("completeness" not in point or _completeness_ok(point["completeness"])))


# Why a perf-end completeness reading has no counts: the renderer's three reasons, and a build without the cfg.
COMPLETENESS_REASONS = ("no certificate", "scene changed", "atlas changed", "api-disabled")
_COMPLETENESS_COUNTS = ("missing_terminal", "missing_chrome")
_COMPLETENESS_DIMENSIONS = ("atlas_width", "atlas_height")


def _completeness_ok(reading: object) -> bool:
    """A perf-end glyph completeness reading: `certified` with exactly its two non-negative distinct-character
    counts, positive atlas dimensions and a scale, or `unavailable` with exactly a known reason and a scale."""
    if not isinstance(reading, dict) or not (perf_latency_schema._is_number(reading.get("scale"))
                                             and math.isfinite(reading["scale"]) and reading["scale"] > 0):
        return False
    if reading.get("state") == "certified":
        return (set(reading) == {"state", "scale", *_COMPLETENESS_COUNTS, *_COMPLETENESS_DIMENSIONS}
                and all(perf_latency_schema._is_int(reading[key]) and reading[key] >= 0 for key in _COMPLETENESS_COUNTS)
                and all(perf_latency_schema._is_int(reading[key]) and reading[key] > 0 for key in _COMPLETENESS_DIMENSIONS))
    return (reading.get("state") == "unavailable" and set(reading) == {"state", "reason", "scale"}
            and reading["reason"] in COMPLETENESS_REASONS)


def _atlas_reading_ok(reading: object) -> bool:
    """One sampling attempt's atlas reading: its attempt, the main window's native label or null, each live
    window's counted glyph atlas growths by native label or null, and the closed windows' count or null."""
    if not isinstance(reading, dict) or not perf_latency_schema._is_int(reading.get("attempt")):
        return False
    counted = reading.get("counted_glyph_atlas_growths")
    closed = reading.get("closed_glyph_atlas_growths")
    return ((reading.get("main_window") is None or isinstance(reading.get("main_window"), str))
            and (counted is None or (isinstance(counted, dict)
                                     and all(isinstance(label, str) and _count_ok(value)
                                             for label, value in counted.items())))
            and (closed is None or _count_ok(closed)))


# The longest dispatches a phase records with their times; a harness that predates them records none.
SLOW_DISPATCH_LIMIT = 64
SLOW_DISPATCH_KEYS = ("start_unix_s", "end_unix_s", "ms")
# Each phase field's check; a field the result leaves out is a metric the harness does not have.
_PHASE_FIELDS = {
    "cpu_user_s": perf_latency_schema._is_number, "cpu_system_s": perf_latency_schema._is_number, "presented_frames": perf_latency_schema._is_int,
    "redraw_requested": perf_latency_schema._is_int, "dispatch_ms": perf_latency_schema._numbers, "present_interval_ms": perf_latency_schema._numbers,
    "dispatch_presenting_ms": perf_latency_schema._numbers, "dispatch_nonpresenting_ms": perf_latency_schema._numbers,
    "go_to_first_output_ms": _finite_non_negative, "first_output_to_present_ms": _finite_non_negative,
    "allocations_per_frame": lambda value: value is None or perf_latency_schema._numbers(value),
    "slow_dispatches": lambda value: isinstance(value, list) and all(
        isinstance(item, dict) and all(_finite_non_negative(item.get(key)) for key in SLOW_DISPATCH_KEYS)
        for item in value),
    "dispatch_count": lambda value: perf_latency_schema._is_int(value) and value >= 0,
    # The logical updates S10's stream phase played; a harness older than the field leaves it out.
    "updates": lambda value: perf_latency_schema._is_int(value) and value > 0,
    # The guard-correlation transport and its own time; a harness older than them leaves both out.
    "guard_correlation": lambda value: perf_guard_schema._guard_field_ok(value),
    "guard_correlation_transport_ns": lambda value: perf_latency_schema._is_int(value) and value >= 0,
}

# The hover counters, shown together: once any of them saw an event in a phase, each row of the group prints, so
# a real 0 detections or 0 probes beside nonzero lookups is evidence rather than an omitted row.
HOVER_COUNTER_FIELDS = ("hover_lookups", "hover_detections", "hover_cache_hits", "hover_rows_read",
                        "path_probes_submitted", "quick_select_rows_scanned")

# result.json's `frame_counters`: whether the binary has the perf-counters feature and the run forced the gate on.
FRAME_COUNTER_STATES = ("unsupported", "off", "on")
# With the gate on, each phase's deltas: per section, its integer counts, then its histograms. A histogram's
# unit is its name's suffix, and the unit fixes its bucket bounds; the last bucket is the overflow.
FRAME_COUNTER_FIELDS = {
    "window": (("attempts", "presented", "cached", "settled", "retry", "surface_retry", "stopped", "failed",
                "contention_parser", "contention_images", "defer_timeout", "defer_contention",
                # defer_sync counts frames held by a visible open synchronized update; a base older than it shows n/a.
                "defer_sync", "defer_streaming",
                # stream_clock_exempt counts settled hardware keypress attempts that kept the streaming clock; a
                # base older than it shows n/a.
                "stream_clock_exempt",
                # The display-link counts: accepted ticks, tick-authorized admissions and ceiling fallbacks. They
                # read 0 off macOS and never prove display-phase alignment; a base older than them shows n/a.
                "display_link_ticks", "display_link_admissions", "display_link_fallbacks",
                "contention_retry_armed",
                # dirt_ack_dropped counts receipts dropped at a collection; a base older than it shows n/a.
                "dirt_ack_dropped", "native_request_redraw", "user_request_redraw", "redraw_requested"),
               ("present_interval_ms", "handler_ms", "flush_to_redraw_ms")),
    "app": (("wake_init", "wake_poll", "wake_wait_cancelled", "wake_resume_time", "wake_user", "ui_parser_locks",
             "fg_probe_calls", "fg_probe_panes", "fg_worker_probes", "fg_worker_panes", "fg_results_stale",
             "native_request_redraw_unregistered",
             # Guard custody (first parser guard to last release) and frame dispatch (first guard to the render
             # call's return; failed collections apart), as sums in ns and counts; a base older than them shows n/a.
             "ui_guard_custody_ns", "ui_guard_custodies", "frame_dispatch_ns", "frame_dispatches",
             "frame_dispatch_failed_ns", "frame_dispatches_failed",
             # URL and path hover work: lookups, detections run, cache hits, rows detection read, probes hover
             # submitted and rows quick select scanned. They are App counters, so a base older than them shows
             # n/a and the comparison gives head-side evidence, not a before/after detection delta.
             *HOVER_COUNTER_FIELDS,
             # Status line memory samples, serviced status line wakes and strip-only redraw requests, then
             # selection counts, the cells they read and `History: trimmed` appearances; App counters, so a
             # base older than them shows n/a.
             "status_line_samples", "status_line_wakes", "status_line_redraws", "status_line_sel_counts",
             "status_line_sel_cells", "status_line_history_shows",
             # Full status line passes (0 while the strip is hidden and unheard) and the main window's
             # startup step times from the wgpu adapter to its first pane; a base older shows n/a.
             "status_line_full_passes", "startup_window_ns", "startup_adapter_install_ns", "startup_renderer_ns",
             "startup_load_checks_ns", "startup_pane_spawn_ns"),
            # fg_probe_* is the retired event-loop probe, a real 0 on a head that probes on the worker.
            ("about_to_wait_ms", "user_event_ms", "new_events_ms", "ui_parser_wait_us", "fg_probe_us",
             "fg_worker_probe_us")),
    "vt": (("parse_bytes", "batches", "flushes", "flushes_untargeted", "flushes_coalesced", "flushes_suppressed",
            # sync_timeouts counts synchronized updates released at the 150 ms bound; a base older than it shows n/a.
            "sync_timeouts",
            # The workers' parser-lock wait summed in ns, and the sections it sums over; a base older shows n/a.
            "parser_lock_wait_ns", "parser_sections"),
           ("parser_lock_wait_us", "parser_lock_hold_us", "parse_us")),
    "renderer": (("vertex_bytes", "index_bytes", "damage_permille_sum", "damaged_frames",
                  # damage_waste_permille_sum is the union rect's share minus what its parts cover, over
                  # damaged_frames; a base older than it shows n/a.
                  "damage_waste_permille_sum", "software_frames",
                  "gpu_frames", "row_cache_hits", "row_cache_misses", "shape_requests", "full_frames",
                  # Partial-assembly counters: presented partial frames, partial plans reassembled Full and
                  # cells hashed into row-cache keys; a base older than them shows n/a.
                  "partial_frames", "partial_fallbacks", "row_cells_hashed",
                  # row_cache_invalidate_us is summed microseconds kept as a plain count, not a histogram.
                  "row_cache_invalidate_visits", "row_cache_invalidate_us", "recolor_glyphs_visited",
                  # font_fallback_applies is supporting evidence; a base older than the counter shows n/a.
                  "font_fallback_applies",
                  # Render-attempt and font-preparation counters; every _ns field is summed nanoseconds.
                  "shape_ns", "raster_ns", "raster_calls", "raster_tiles", "font_generation_applies",
                  "font_prepare_ns", "font_generation_prepare_ns", "render_attempts", "render_attempts_presented",
                  "render_attempt_ns", "render_attempt_shape_ns", "render_attempt_raster_ns",
                  "render_attempt_shape_requests", "render_attempt_raster_calls", "render_attempt_raster_tiles",
                  "apply_attempts", "apply_attempts_presented", "apply_attempt_ns", "apply_attempt_shape_ns",
                  "apply_attempt_raster_ns", "apply_attempt_shape_requests", "apply_attempt_raster_calls",
                  "apply_attempt_raster_tiles",
                  # Glyph atlas growths and growths no frame presented; a base older than them shows n/a.
                  "glyph_atlas_growths", "atlas_growth_abandoned",
                  # Tab titles and chrome runs drawn from their caches (reuses) and shaped on a miss
                  # (prepares, whose requests also count in shape_requests); a base older than them shows n/a.
                  "tab_title_reuses", "tab_title_prepares", "chrome_run_reuses", "chrome_run_prepares"),
                 ("assembly_us", "atlas_growth_to_present_ms")),
}
HISTOGRAM_BOUNDS = {"ms": [4, 7, 9, 12, 17, 25, 34, 50, 100], "us": [10, 50, 100, 500, 1000, 5000]}
# A histogram's `sum_us` is an exact integer in microseconds whatever its unit; this converts it to the unit.
MICROSECONDS_PER_UNIT = {"ms": 1000, "us": 1}


def frame_counter_state(data: Mapping) -> object:
    """The result's frame_counters state; a result without the key comes from a harness built without the feature."""
    return data.get("frame_counters", "unsupported")


def _count_ok(value: object) -> bool:
    return perf_latency_schema._is_int(value) and value >= 0


def _histogram_problem(value: object, unit: str) -> str | None:
    """Why a value is not a histogram in `unit`, or None when it is one."""
    if not isinstance(value, dict):
        return "is not a histogram object"
    bounds = HISTOGRAM_BOUNDS[unit]
    if value.get("unit") != unit:
        return f"has unit {value.get('unit')!r}, not {unit!r}"
    if value.get("bounds") != bounds:
        return f"has bounds {value.get('bounds')!r}, not {bounds}"
    counts = value.get("counts")
    if not isinstance(counts, list) or len(counts) != len(bounds) + 1 or not all(_count_ok(item) for item in counts):
        return f"counts is not {len(bounds) + 1} non-negative integers"
    if "sum" in value:
        return "has the retired key sum; the contract's sum is sum_us, in microseconds"
    if not _count_ok(value.get("sum_us")):
        return "sum_us is not a non-negative integer of microseconds"
    return None


def frame_counter_problems(counters: object, partial: bool = False) -> list[str]:
    """Check one phase's frame_counters object: every section and field, each of its type; [] when it is whole.

    `partial` accepts an absent section or field key, for a base built before the contract gained it; a key
    that is present must hold a value of its type, so a present null is always a problem.
    """
    if not isinstance(counters, dict):
        return ["is not an object"]
    problems = []
    for section, (counts, histograms) in FRAME_COUNTER_FIELDS.items():
        # When: the key is absent, an older base never had the section; a present null is malformed.
        if section not in counters:
            if not partial:
                problems.append(f"lacks the {section} section")
            continue
        body = counters[section]
        if not isinstance(body, dict):
            problems.append(f"the {section} section is {type(body).__name__}, not an object")
            continue
        for field_name in counts + histograms:
            if field_name not in body:
                if not partial:
                    problems.append(f"lacks {section}.{field_name}")
            elif field_name in counts and not _count_ok(body[field_name]):
                problems.append(f"{section}.{field_name} is not a non-negative integer")
            elif field_name in histograms:
                problem = _histogram_problem(body[field_name], field_name.rsplit("_", 1)[1])
                if problem:
                    problems.append(f"{section}.{field_name} {problem}")
    return problems


# Variants measured only in the counters set: their evidence is the counter record of injected episodes,
# so a timed, laps or alloc run of them would measure nothing comparable.
COUNTERS_ONLY_VARIANTS = frozenset({("S1", "atlas-retry")})
# Diagnostics that need a harness API cfg (local-gate.py HARNESS_API_CFGS): with it off, the variant is unavailable
# on both sides and never measured. This is a declared availability rule, not a tolerance for a blocked run.
HARNESS_CFG_VARIANTS = {("S1", "atlas-retry"): "perf_atlas_retry_api"}
# S1/atlas-retry's recovery episodes: eight of frames A-D each.
ATLAS_RECOVERY_EPISODES = 8
ATLAS_RECOVERY_FRAMES = ("A", "B", "C", "D")
ATLAS_RECOVERY_FIELDS = ("attempts", "presented", "resets", "hits", "misses", "shapes", "atlas_dim")


def variant_sets(scenario_id: str, variant: str, sets: Sequence[tuple]) -> list[tuple]:
    """The run sets one selected variant takes: every set, or only the counters set for a counters-only variant."""
    if (scenario_id, variant) in COUNTERS_ONLY_VARIANTS:
        return [entry for entry in sets if entry[0] == "counters"]
    return list(sets)


def counters_only_problem(selected: Sequence[tuple[str, str]], counters_set: bool) -> str | None:
    """Why the selection cannot run: a counters-only variant selected when no counters set runs."""
    named = [f"{scenario_id}/{variant}" for scenario_id, variant in selected
             if (scenario_id, variant) in COUNTERS_ONLY_VARIANTS]
    if named and not counters_set:
        return (f"{', '.join(named)} runs only in the counters set; pass --counters with a head that "
                "declares perf-counters")
    return None


def atlas_recovery_problem(data: Mapping) -> str | None:
    """Why result.json's `atlas_recovery` cannot be read, or None when it can.

    Only a valid S1/atlas-retry run with frame counters on carries it, and such a run must: 8 episodes of
    A, B, C, D in order, one attempt each, A resetting without presenting, B-D presenting without a reset,
    and B-D at one atlas dimension. Every number is an exact integer (never a float or a boolean that
    compares equal), every counter is nonnegative, and the distinct key count and the atlas dimension are
    positive. Any other result must not carry it.
    """
    counters_only = (data.get("scenario"), data.get("variant")) in COUNTERS_ONLY_VARIANTS
    recovery = data.get("atlas_recovery")
    if not counters_only or data.get("frame_counters") != "on":
        if counters_only and data.get("status") == "valid":
            # When: a valid S1/atlas-retry result ran without counters, it measured nothing.
            return "S1/atlas-retry ran without frame counters"
        return None if recovery is None else "atlas_recovery on a result that is not an S1/atlas-retry counters run"
    if recovery is None:
        return "a valid S1/atlas-retry counters run has no atlas_recovery" if data.get("status") == "valid" else None
    if not isinstance(recovery, dict) or not perf_latency_schema._is_int(recovery.get("episodes")) \
            or recovery["episodes"] != ATLAS_RECOVERY_EPISODES or not perf_latency_schema._is_int(recovery.get("distinct_keys")) \
            or recovery["distinct_keys"] < 1 or not isinstance(recovery.get("records"), list):
        return "atlas_recovery needs integer episodes 8, a positive integer distinct_keys and a records list"
    records = recovery["records"]
    if len(records) != ATLAS_RECOVERY_EPISODES * len(ATLAS_RECOVERY_FRAMES):
        return f"atlas_recovery has {len(records)} records, not 32"
    recovered_dims = set()
    for index, record in enumerate(records):
        frame = ATLAS_RECOVERY_FRAMES[index % 4]
        if not isinstance(record, dict) or not perf_latency_schema._is_int(record.get("episode")) \
                or record["episode"] != index // 4 or record.get("frame") != frame:
            return f"atlas_recovery record {index} is not episode {index // 4} {frame}"
        if not all(perf_latency_schema._is_int(record.get(name)) for name in ATLAS_RECOVERY_FIELDS):
            return f"atlas_recovery record {index} lacks an integer field"
        if any(record[name] < 0 for name in ATLAS_RECOVERY_FIELDS) or record["atlas_dim"] < 1:
            return f"atlas_recovery record {index} has a negative count or a zero atlas dimension"
        if record["attempts"] != 1:
            return f"atlas_recovery record {index} has {record['attempts']} attempts"
        expected = (1, 0) if frame == "A" else (0, 1)
        if (record["resets"], record["presented"]) != expected:
            return f"atlas_recovery record {index} ({frame}) has resets {record['resets']}, presented {record['presented']}"
        if frame != "A":
            recovered_dims.add(record["atlas_dim"])
    if len(recovered_dims) != 1:
        return "atlas_recovery frames B-D draw at more than one atlas dimension"
    return None


# A phase's kinds, a transition's endpoints and the reasons a transition reached no endpoint, as the harness writes
# them. An endpoint not reached is missing as expired (the deadline passed) or incomplete.
PHASE_KINDS = ("sustained", "transition", "hold")
TRANSITION_ENDPOINTS = ("sentinel-parsed", "image-registered-then-presented", "first-present-after-entry",
                        "first-present-with-image", "first-present-then-about-to-wait")
NO_PRESENTATION = "no-presentation"
PRESENT_FIELDS = ("first_present_ms", "last_present_ms", "first_present_seq", "last_present_seq")


def _phase_presentation_problems(phase: dict) -> list[str]:
    """A phase's presentation trace: first and last presentation offsets and counts exactly when it presented a
    frame (else present_missing no-presentation), first no later than last, and non-presenting redraws a count no
    larger than its redraws."""
    name, frames = phase.get("name"), phase.get("presented_frames")
    present = [phase.get(key) for key in PRESENT_FIELDS]
    problems = []
    if perf_latency_schema._is_int(frames) and frames > 0:
        if any(value is None for value in present) or "present_missing" in phase:
            problems.append(f"phase {name!r} presented {frames} frames but does not record its first and last "
                            "presentation")
        elif not all(_finite_non_negative(value) for value in present) \
                or not all(perf_latency_schema._is_int(value) for value in present[2:]):
            problems.append(f"phase {name!r} presentation offsets and counts {present} are not finite non-negative "
                            "numbers with integer counts")
        elif present[0] > present[1] or present[2] > present[3]:
            problems.append(f"phase {name!r} first presentation follows its last: {present}")
    elif any(key in phase for key in PRESENT_FIELDS) or phase.get("present_missing") != NO_PRESENTATION:
        problems.append(f"phase {name!r} presented no frame but does not record present_missing "
                        f"{NO_PRESENTATION!r} alone")
    skipped, redraws = phase.get("nonpresenting_redraws"), phase.get("redraw_requested")
    if not _count_ok(skipped) or (perf_latency_schema._is_int(redraws) and skipped > redraws):
        problems.append(f"phase {name!r} nonpresenting_redraws {skipped!r} is not a count of at most its "
                        f"{redraws!r} redraws")
    return problems


def _phase_duration_problems(phase: dict) -> list[str]:
    """A phase-kinds phase's durations: every dispatch and present interval and both CPU times finite and at least
    zero, and its end no earlier than its start. A harness without the capability keeps its looser checks."""
    name, problems = phase.get("name"), []
    for key in ("dispatch_ms", "dispatch_presenting_ms", "dispatch_nonpresenting_ms", "present_interval_ms"):
        values = phase.get(key)
        if isinstance(values, list) and not all(_finite_non_negative(value) for value in values):
            problems.append(f"phase {name!r} {key} holds a value that is not finite and non-negative "
                            "(phase-kinds harness)")
    for key in ("cpu_user_s", "cpu_system_s"):
        if key in phase and not _finite_non_negative(phase[key]):
            problems.append(f"phase {name!r} {key} {phase[key]!r} is not finite and non-negative (phase-kinds harness)")
    start, end = phase.get("start_unix_s"), phase.get("end_unix_s")
    if perf_latency_schema._is_number(start) and perf_latency_schema._is_number(end) and not (math.isfinite(start) and math.isfinite(end) and end >= start):
        problems.append(f"phase {name!r} ends before it starts: {start!r} to {end!r} (phase-kinds harness)")
    return problems


def phase_kind_problems(phases: Sequence[dict]) -> list[str]:
    """Problems with a phase-kinds harness's phases: every phase has a known kind; a transition has a known
    endpoint and exactly one of completion_ms (finite, at least 0) or a completion_missing reason valid for its
    endpoint; a hold or sustained phase carries neither; and every phase records its presentation trace. Each
    violation is a schema failure, never a fallback."""
    problems = []
    for phase in phases:
        name, kind = phase.get("name"), phase.get("kind")
        if kind not in PHASE_KINDS:
            problems.append(f"phase {name!r} kind {kind!r} is not one of {PHASE_KINDS}")
        elif kind == "transition":
            endpoint = phase.get("endpoint")
            has_ms, has_missing = "completion_ms" in phase, "completion_missing" in phase
            if endpoint not in TRANSITION_ENDPOINTS:
                problems.append(f"phase {name!r} endpoint {endpoint!r} is not a transition endpoint")
            if has_ms == has_missing:
                problems.append(f"phase {name!r} carries not exactly one of completion_ms and completion_missing")
            elif has_ms and not _finite_non_negative(phase["completion_ms"]):
                problems.append(f"phase {name!r} completion_ms {phase['completion_ms']!r} is not finite and "
                                "non-negative")
            elif has_missing:
                allowed = ("expired", "incomplete")
                if phase["completion_missing"] not in allowed:
                    problems.append(f"phase {name!r} completion_missing {phase['completion_missing']!r} is not a "
                                    f"reason for {endpoint}")
        else:
            for key in ("endpoint", "completion_ms", "completion_missing"):
                if key in phase:
                    problems.append(f"phase {name!r} is a {kind} phase but carries {key}")
        problems.extend(_phase_presentation_problems(phase))
        problems.extend(_phase_duration_problems(phase))
    return problems


def validate_result(data: object, harness_hash: str, process_exit_code: int | None, *,
                    counters: bool = False, partial_counters: bool = False,
                    platform_name: str = "darwin", latency_split_schema: int | None = None,
                    phase_kinds: int | None = None, attribution_api: bool | None = None,
                    attribution_schema: int | None = None, echo_timeline_schema: int | None = None,
                    guard_correlation_schema: int | None = None,
                    scope: tuple[str, str] | None = None) -> list[str]:
    """Check result.json against schema version 1; an empty list means it can be read.

    A result whose `managed` is not true, or whose harness hash differs from the one this
    script passed, is a schema failure: it is a standalone run or another harness's run.
    `counters` says whether the run passed --counters: only then must the gate be on, and only
    with the gate on does every phase carry a whole frame_counters object. `partial_counters` lets a
    base's counters lack fields its older contract never had. `latency_split_schema` is the harness's
    declared split schema: with 1, every non-null latency object must carry the whole split contract; with
    None (an older harness) the split fields are neither required nor trusted.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_metrics
    if not isinstance(data, dict):
        return ["result.json is not an object"]
    problems = []
    version = data.get("schema_version")
    if not perf_latency_schema._is_int(version) or version != perf_settings.SCHEMA_VERSION:
        problems.append(f"schema_version is {version!r}, not {perf_settings.SCHEMA_VERSION}")
    if data.get("managed") is not True:
        problems.append(f"managed is {data.get('managed')!r}, not true")
    if data.get("harness_hash") != harness_hash:
        problems.append(f"harness_hash {data.get('harness_hash')!r} is not the hash passed, {harness_hash}")
    status = data.get("status")
    if not isinstance(status, str):
        problems.append("status is not a string")
    exit_code = data.get("exit_code")
    if not perf_latency_schema._is_int(exit_code):
        problems.append("exit_code is not an integer")
    elif process_exit_code is not None and exit_code != process_exit_code:
        problems.append(f"exit_code {exit_code} differs from the process exit {process_exit_code}")
    if status == "valid" and not isinstance(data.get("grid"), (dict, list)):
        problems.append("a valid result has no grid")
    state = frame_counter_state(data)
    if not isinstance(state, str) or state not in FRAME_COUNTER_STATES:
        problems.append(f"frame_counters is {state!r}, not one of {', '.join(FRAME_COUNTER_STATES)}")
    elif counters and state != "on":
        problems.append(f"frame_counters is {state!r}, but the run passed --counters")
    elif not counters and state == "on":
        problems.append("frame_counters is 'on', but the run did not pass --counters")
    phases = data.get("phases")
    if not isinstance(phases, list) or not all(isinstance(phase, dict) for phase in phases):
        problems.append("phases is not a list of objects")
        phases = []
    for phase in phases:
        name = phase.get("name")
        if not isinstance(name, str) or not perf_latency_schema._is_number(phase.get("start_unix_s")) \
                or not perf_latency_schema._is_number(phase.get("end_unix_s")):
            problems.append(f"phase {name!r} lacks a name or its start and end times")
        problems.extend(f"phase {name!r} field {key} has the wrong type"
                        for key, check in _PHASE_FIELDS.items() if key in phase and not check(phase[key]))
        # When: the harness declares the guard-correlation contract, every finalized phase records its transport.
        if guard_correlation_schema is not None:
            problems.extend(f"phase {name!r} lacks {key}" for key in ("guard_correlation",
                            "guard_correlation_transport_ns") if key not in phase)
        elif "guard_correlation" in phase:
            problems.append(f"phase {name!r} records guard_correlation its harness never declared")
        # When: the gate was on, a phase's counters must be whole; otherwise a phase carries none.
        if state == "on":
            problems.extend(f"phase {name!r} frame_counters {problem}"
                            for problem in frame_counter_problems(phase.get("frame_counters"), partial_counters))
        elif "frame_counters" in phase:
            problems.append(f"phase {name!r} carries frame_counters, but the gate is {state!r}")
    if phase_kinds == 1:
        # When: the harness classifies its phases, each phase's kind, endpoint and presentation trace is validated.
        problems.extend(phase_kind_problems(phases))
    latency = data.get("latency")
    if latency is not None and not (isinstance(latency, dict) and perf_latency_schema.latency_values(latency) is not None
                                    and perf_latency_schema._is_int(latency.get("attributed")) and perf_latency_schema._is_int(latency.get("total"))
                                    and "coverage" in latency
                                    and (latency["coverage"] is None or perf_latency_schema._is_number(latency["coverage"]))):
        problems.append("latency needs samples, attributed, total and coverage")
    elif latency is not None:
        if latency_split_schema == 1:
            problems.extend(f"latency {problem}" for problem in perf_latency_schema.latency_split_problems(latency))
        # The timeline is checked whatever the split schema says, so a null split capability hides nothing. The
        # run's own scenario and variant decide its scope; the result's are read only without one.
        label = scope if scope is not None else (data.get("scenario"), data.get("variant"))
        problems.extend(f"latency {problem}" for problem in
                        perf_timeline_schema.echo_timeline_problems(latency, echo_timeline_schema, label in perf_timeline_schema.TIMELINE_SCOPE))
    if echo_timeline_schema is not None and latency_split_schema != 1:
        # When: the timeline schema is declared without the split schema its parts and reasons extend.
        problems.append("echo_timeline_schema is declared without latency_split_schema 1")
    throughput = data.get("throughput")
    if throughput is not None and not (isinstance(throughput, dict) and perf_latency_schema._is_int(throughput.get("bytes"))
                                       and perf_latency_schema._is_number(throughput.get("seconds"))):
        problems.append("throughput needs bytes and seconds")
    if data.get("uncover_ms") is not None and not perf_latency_schema._is_number(data.get("uncover_ms")):
        problems.append("uncover_ms is not a number")
    if data.get("scrollback_rows_retained") is not None and not perf_latency_schema._is_int(data.get("scrollback_rows_retained")):
        problems.append("scrollback_rows_retained is not an integer")
    if "checkpoint_memory" in data and data["checkpoint_memory"] not in perf_metrics.CHECKPOINT_MEMORY_STATES:
        problems.append("checkpoint_memory is not supported or unsupported")
    if "hooks" in data and not (isinstance(data["hooks"], dict)
                                and data["hooks"].get("trim") in perf_metrics.TRIM_HOOK_OUTCOMES):
        # When: a harness that records its hooks names an outcome this script does not know, it cannot be read.
        problems.append(f"hooks is {data['hooks']!r}, not {{'trim': one of {', '.join(perf_metrics.TRIM_HOOK_OUTCOMES)}}}")
    checkpoints = data.get("checkpoints")
    if not isinstance(checkpoints, list) or not all(_checkpoint_ok(point) for point in checkpoints):
        problems.append("checkpoints is not a list of {index, label, unix_s, footprint_file}")
    elif status == "valid" and not any(point["label"] == "end" for point in checkpoints):
        problems.append("a valid result has no checkpoint labelled end")
    # An early or timed-out run writes null; only a valid run must also report true.
    if "finish_session_settled" not in data or not (data["finish_session_settled"] is None
                                                    or isinstance(data["finish_session_settled"], bool)):
        problems.append("finish_session_settled is not a boolean or null")
    # The measurement window's display after startup; absent or null when the harness did not report one.
    if data.get("monitor") is not None and not _monitor_ok(data["monitor"]):
        problems.append("monitor needs name, refresh_rate_millihertz and scale_factor of the documented types")
    # Windows and macOS runs record their presenter; when present each field has its type.
    presenter = data.get("presenter")
    if presenter is not None and not _presenter_ok(presenter):
        problems.append("presenter needs software_render_mode (a string or null) and the booleans "
                        "software_rendering, software_render_degraded and windows_gdi")
    if platform_name in ("win32", "darwin") and status == "valid" and presenter is None:
        # When: every Windows and macOS run records how it presented, so a valid one without the record cannot be
        # trusted; a macOS row counts only when that record shows the hardware path.
        host = "Windows" if platform_name == "win32" else "macOS"
        problems.append(f"a valid {host} result has no presenter")
    if data.get("trim_experiment") not in (None, perf_metrics.TRIM_EXPERIMENT):
        problems.append(f"trim_experiment is {data.get('trim_experiment')!r}, not null or {perf_metrics.TRIM_EXPERIMENT!r}")
    after_hook = data.get("trim_seq_after_hook")
    hooks = data.get("hooks")
    trim_outcome = hooks.get("trim") if isinstance(hooks, dict) else None
    if trim_outcome == "trimmed" and not (perf_latency_schema._is_int(after_hook) and after_hook > 0):
        # When: a trimmed hook must name its trim, or no covered sample can be checked against it.
        problems.append("hooks.trim is trimmed but trim_seq_after_hook is not a positive integer")
    elif trim_outcome != "trimmed" and after_hook is not None:
        problems.append(f"hooks.trim is {trim_outcome!r} but trim_seq_after_hook is {after_hook!r}, not null")
    if data.get("trim_experiment") is not None and "hooks" not in data:
        problems.append("a trim experiment result records no hooks")
    recovery_problem = atlas_recovery_problem(data)
    if recovery_problem is not None:
        problems.append(recovery_problem)
    if platform_name == "win32" and data.get("synthetic_occlusion") is True and not perf_metrics.trim_experiment_run(data):
        # When: Windows reports no occlusion, so only the short S12 trim experiment may deliver one there.
        problems.append("synthetic_occlusion is true, but Windows reports no occlusion")
    problems.extend(perf_attribution_schema.attribution_problems(data, attribution_api, attribution_schema))
    notes = data.get("notes")
    if not isinstance(notes, list) or not all(isinstance(note, str) for note in notes):
        problems.append("notes is not a list of strings")
    return problems
