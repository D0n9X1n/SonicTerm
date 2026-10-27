"""Perf counter schema tests."""

from __future__ import annotations
import contextlib
import io
from pathlib import Path
import tempfile
import unittest

import perf_arguments
import perf_harness_sources
import perf_home_tests
import perf_memory_tests
import perf_result_schema
import perf_result_tests
import perf_scenarios
import perf_settings


# The result.json frame-counter contract, written out here so a test fails when the script drifts from it:
# each section's integer counts, then its histograms (the suffix names the unit).
COUNTER_CONTRACT = {
    "window": (("attempts", "presented", "cached", "settled", "retry", "surface_retry", "stopped", "failed",
                "contention_parser", "contention_images", "defer_timeout", "defer_contention", "defer_sync",
                "defer_streaming", "stream_clock_exempt", "display_link_ticks", "display_link_admissions", "display_link_fallbacks",
                "contention_retry_armed", "dirt_ack_dropped", "native_request_redraw",
                "user_request_redraw", "redraw_requested"),
               ("present_interval_ms", "handler_ms", "flush_to_redraw_ms")),
    "app": (("wake_init", "wake_poll", "wake_wait_cancelled", "wake_resume_time", "wake_user", "ui_parser_locks",
             "fg_probe_calls", "fg_probe_panes", "fg_worker_probes", "fg_worker_panes", "fg_results_stale",
             "native_request_redraw_unregistered", "ui_guard_custody_ns", "ui_guard_custodies",
             "frame_dispatch_ns", "frame_dispatches", "frame_dispatch_failed_ns", "frame_dispatches_failed",
             "hover_lookups", "hover_detections", "hover_cache_hits", "hover_rows_read", "path_probes_submitted",
             "quick_select_rows_scanned", "status_line_samples", "status_line_wakes", "status_line_redraws",
             "status_line_sel_counts", "status_line_sel_cells", "status_line_history_shows",
             "status_line_full_passes", "startup_window_ns", "startup_adapter_install_ns", "startup_renderer_ns",
             "startup_load_checks_ns", "startup_pane_spawn_ns"),
            ("about_to_wait_ms", "user_event_ms", "new_events_ms", "ui_parser_wait_us", "fg_probe_us",
             "fg_worker_probe_us")),
    "vt": (("parse_bytes", "batches", "flushes", "flushes_untargeted", "flushes_coalesced",
            "flushes_suppressed", "sync_timeouts", "parser_lock_wait_ns", "parser_sections"),
           ("parser_lock_wait_us", "parser_lock_hold_us", "parse_us")),
    "renderer": (("vertex_bytes", "index_bytes", "damage_permille_sum", "damaged_frames",
                  "damage_waste_permille_sum", "software_frames",
                  "gpu_frames", "row_cache_hits", "row_cache_misses", "shape_requests", "full_frames",
                  "partial_frames", "partial_fallbacks", "row_cells_hashed",
                  # row_cache_invalidate_us is summed microseconds as a plain count, not a histogram.
                  "row_cache_invalidate_visits", "row_cache_invalidate_us", "recolor_glyphs_visited",
                  # font_fallback_applies is supporting evidence; a base older than the counter shows n/a.
                  "font_fallback_applies",
                  "shape_ns", "raster_ns", "raster_calls", "raster_tiles", "font_generation_applies",
                  "font_prepare_ns", "font_generation_prepare_ns", "render_attempts", "render_attempts_presented",
                  "render_attempt_ns", "render_attempt_shape_ns", "render_attempt_raster_ns",
                  "render_attempt_shape_requests", "render_attempt_raster_calls", "render_attempt_raster_tiles",
                  "apply_attempts", "apply_attempts_presented", "apply_attempt_ns", "apply_attempt_shape_ns",
                  "apply_attempt_raster_ns", "apply_attempt_shape_requests", "apply_attempt_raster_calls",
                  "apply_attempt_raster_tiles",
                  # Glyph atlas growths and growths no frame presented; a base older than them shows n/a.
                  "glyph_atlas_growths", "atlas_growth_abandoned",
                  # Title and chrome-run cache reuses and prepares; a base older than them shows n/a.
                  "tab_title_reuses", "tab_title_prepares", "chrome_run_reuses", "chrome_run_prepares"),
                 ("assembly_us", "atlas_growth_to_present_ms")),
}
CONTRACT_FIELD_COUNT = sum(len(counts) + len(histograms) for counts, histograms in COUNTER_CONTRACT.values())
MILLISECOND_BOUNDS = [4, 7, 9, 12, 17, 25, 34, 50, 100]
MICROSECOND_BOUNDS = [10, 50, 100, 500, 1000, 5000]


def frame_counters(values=None):
    """One phase's complete frame_counters object; `values` maps `section.field` to a count or (buckets, sum_us).

    Every histogram's sum is `sum_us`, an exact integer in microseconds whatever the histogram's unit.
    """
    values = values or {}
    sections = {}
    for section, (counts, histograms) in COUNTER_CONTRACT.items():
        body = {name: values.get(f"{section}.{name}", 0) for name in counts}
        for name in histograms:
            unit = name.rsplit("_", 1)[1]
            bounds = MILLISECOND_BOUNDS if unit == "ms" else MICROSECOND_BOUNDS
            buckets, total_us = values.get(f"{section}.{name}", ([0] * (len(bounds) + 1), 0))
            body[name] = {"unit": unit, "bounds": list(bounds), "counts": list(buckets), "sum_us": total_us}
        sections[section] = body
    return sections


def counters_result(values=None, **overrides):
    """A valid result of a run with the gate forced on, every phase carrying frame_counters(values)."""
    result = perf_result_tests.valid_result(**overrides)
    result["frame_counters"] = "on"
    for phase in result["phases"]:
        phase["frame_counters"] = frame_counters(values)
    return result


FEATURES_TABLE = "\n[features]\nperf-counters = []\n"
COUNTERS_MANIFEST = perf_home_tests.HEAD_MANIFEST + FEATURES_TABLE
BASE_COUNTERS_MANIFEST = perf_home_tests.BASE_MANIFEST + FEATURES_TABLE
# The logging crate's source, with and without the filtered init the counters harness calls.
LOGGING_LIB = "crates/sonicterm-logging/src/lib.rs"
LOGGING_WITH_FILTER = ("pub fn init_in(cfg: &LoggingConfig, dir: &Path) -> io::Result<LoggingGuard> {}\n"
                       "pub fn init_in_with_filter(dir: &Path, filter: &str) -> io::Result<LoggingGuard> {}\n")
LOGGING_WITHOUT_FILTER = "pub fn init_in(cfg: &LoggingConfig, dir: &Path) -> io::Result<LoggingGuard> {}\n"
# Stands for a key the test deletes instead of setting.
MISSING = object()


class FrameCounterSchemaTests(unittest.TestCase):
    def check(self, result, counters=True):
        return perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0, counters=counters)

    def test_a_result_without_the_key_is_unsupported(self):
        # Older harnesses write no frame_counters, which reads as a binary built without the feature.
        self.assertEqual(perf_result_schema.frame_counter_state(perf_result_tests.valid_result()), "unsupported")
        for state in ("off", "unsupported"):
            with self.subTest(state=state):
                self.assertEqual(self.check(perf_result_tests.valid_result(frame_counters=state), counters=False), [])
        self.assertEqual(self.check(counters_result()), [])

    def test_the_state_is_one_of_three_values(self):
        # A misspelled or null state is a schema failure, never read as a run without counters.
        for state in ("ON", "maybe", None, True, 1):
            with self.subTest(state=state):
                self.assertTrue(self.check(perf_result_tests.valid_result(frame_counters=state), counters=False))

    def test_with_the_gate_on_every_phase_carries_every_field_with_its_type(self):
        # A missing, partial or mistyped object would read as zeros, so each one is a schema failure.
        def broken(section, name, value):
            counters = frame_counters()
            if value is MISSING:
                del counters[section][name]
            else:
                counters[section][name] = value
            return counters

        histogram = frame_counters()["window"]["handler_ms"]
        cases = {
            "a phase without frame_counters": MISSING,
            "frame_counters that is not an object": [],
            "a missing section": {key: body for key, body in frame_counters().items() if key != "vt"},
            "a missing count": broken("window", "attempts", MISSING),
            "a missing full-plan frame count": broken("renderer", "full_frames", MISSING),
            "a missing row-cache invalidation visit count": broken("renderer", "row_cache_invalidate_visits", MISSING),
            "a missing row-cache invalidation time": broken("renderer", "row_cache_invalidate_us", MISSING),
            "a missing recolor visit count": broken("renderer", "recolor_glyphs_visited", MISSING),
            "a missing assembly histogram": broken("renderer", "assembly_us", MISSING),
            "a missing fallback-apply count": broken("renderer", "font_fallback_applies", MISSING),
            "a negative fallback-apply count": broken("renderer", "font_fallback_applies", -1),
            "a negative recolor visit count": broken("renderer", "recolor_glyphs_visited", -1),
            "a fractional invalidation visit count": broken("renderer", "row_cache_invalidate_visits", 0.5),
            # The invalidation time is a plain integer of microseconds, never a histogram object.
            "a histogram for the invalidation time": broken("renderer", "row_cache_invalidate_us",
                                                            frame_counters()["renderer"]["assembly_us"]),
            "a count for the assembly histogram": broken("renderer", "assembly_us", 12),
            "the assembly histogram in ms": broken("renderer", "assembly_us", dict(
                frame_counters()["renderer"]["assembly_us"], unit="ms", bounds=MILLISECOND_BOUNDS, counts=[0] * 10)),
            "a missing unregistered-window count": broken("app", "native_request_redraw_unregistered", MISSING),
            "a missing histogram": broken("app", "fg_worker_probe_us", MISSING),
            "a negative count": broken("window", "attempts", -1),
            "a fractional count": broken("window", "attempts", 1.5),
            "a boolean count": broken("vt", "batches", True),
            "a histogram for a count": broken("window", "attempts", histogram),
            "a count for a histogram": broken("window", "handler_ms", 3),
            "too few buckets": broken("window", "handler_ms", dict(histogram, counts=[0] * 9)),
            "a negative bucket": broken("window", "handler_ms", dict(histogram, counts=[-1] + [0] * 9)),
            "a fractional bucket": broken("window", "handler_ms", dict(histogram, counts=[0.5] + [0] * 9)),
            "the wrong unit": broken("window", "handler_ms", dict(histogram, unit="us")),
            "the other unit's bounds": broken("window", "handler_ms", dict(histogram, bounds=MICROSECOND_BOUNDS,
                                                                            counts=[0] * 7)),
            "a negative sum_us": broken("window", "handler_ms", dict(histogram, sum_us=-1)),
            "a missing sum_us": broken("window", "handler_ms", {key: item for key, item in histogram.items()
                                                                if key != "sum_us"}),
            "a string sum_us": broken("window", "handler_ms", dict(histogram, sum_us="0")),
            "a fractional sum_us": broken("window", "handler_ms", dict(histogram, sum_us=1.5)),
            "a boolean sum_us": broken("window", "handler_ms", dict(histogram, sum_us=True)),
            # The retired key: the contract replaced `sum` in the unit with `sum_us`.
            "the retired sum beside sum_us": broken("window", "handler_ms", dict(histogram, sum=0)),
            "the retired sum alone": broken("window", "handler_ms", {"sum": 0, **{
                key: item for key, item in histogram.items() if key != "sum_us"}}),
        }
        for case, counters in cases.items():
            with self.subTest(case):
                result = counters_result()
                if counters is MISSING:
                    del result["phases"][0]["frame_counters"]
                else:
                    result["phases"][0]["frame_counters"] = counters
                problems = self.check(result)
                self.assertTrue(any("frame_counters" in problem for problem in problems), problems)

    def test_only_a_run_with_the_gate_on_carries_phase_counters(self):
        # With the gate off, or without the feature, a phase object of counters is a contract break.
        for state in ("off", "unsupported", MISSING):
            with self.subTest(state=state):
                result = counters_result()
                if state is MISSING:
                    del result["frame_counters"]
                else:
                    result["frame_counters"] = state
                self.assertTrue(self.check(result, counters=False))

    def test_a_base_may_lack_a_newer_field_but_types_still_count(self):
        # An older base reports the contract it had: a missing field or section is not a failure there, but a
        # field it does report must have its type. The head's counters must be whole.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["renderer"]["full_frames"]
        self.assertEqual(perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        self.assertTrue(any("full_frames" in problem for problem in self.check(lacking)))
        no_section = counters_result()
        del no_section["phases"][0]["frame_counters"]["renderer"]
        # A base built before the four row-cache, recolor and assembly fields joined the contract lacks them all.
        older = counters_result()
        for name in ("row_cache_invalidate_visits", "row_cache_invalidate_us", "recolor_glyphs_visited", "assembly_us"):
            del older["phases"][0]["frame_counters"]["renderer"][name]
        self.assertEqual(perf_result_schema.validate_result(older, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        self.assertTrue(self.check(older))
        self.assertEqual(perf_result_schema.validate_result(no_section, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        # A base built before the fallback-apply count joined the contract lacks only that field: n/a, not a failure.
        before_fallback = counters_result()
        del before_fallback["phases"][0]["frame_counters"]["renderer"]["font_fallback_applies"]
        self.assertEqual(
            perf_result_schema.validate_result(before_fallback, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        self.assertTrue(any("font_fallback_applies" in problem for problem in self.check(before_fallback)))
        mistyped = counters_result({"window.attempts": -1})
        self.assertTrue(perf_result_schema.validate_result(mistyped, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True))

    def test_partial_counters_skip_an_absent_key_but_never_a_present_null(self):
        # The partial rule is about keys an older contract never had. A key that is present with a null or
        # wrongly typed value is malformed, at the section and at the field level, on any side.
        def base_problems(edit):
            result = counters_result()
            edit(result["phases"][0]["frame_counters"])
            return perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True)

        self.assertEqual(base_problems(lambda counters: counters.pop("renderer")), [])
        self.assertEqual(base_problems(lambda counters: counters["renderer"].pop("full_frames")), [])
        cases = {"a null section": lambda counters: counters.update(renderer=None),
                 "a list section": lambda counters: counters.update(renderer=[]),
                 "a null count": lambda counters: counters["renderer"].update(full_frames=None),
                 "a null histogram": lambda counters: counters["renderer"].update(assembly_us=None)}
        for case, edit in cases.items():
            with self.subTest(case):
                problems = base_problems(edit)
                self.assertTrue(any("renderer" in problem for problem in problems), problems)

    def test_the_state_must_match_whether_the_run_passed_counters(self):
        # A counters run whose harness ignored the flag measured nothing; a plain run must not pay the gate's cost.
        for state in ("off", "unsupported"):
            with self.subTest(state=state):
                problems = self.check(perf_result_tests.valid_result(frame_counters=state), counters=True)
                self.assertTrue(any("--counters" in problem for problem in problems), problems)
        self.assertTrue(self.check(counters_result(), counters=False))
        # A run that ended before its first phase still reports the gate it forced on.
        early = counters_result(status="invalid", exit_code=3, phases=[], grid=None)
        self.assertEqual(perf_result_schema.validate_result(early, perf_memory_tests.HARNESS_HASH, 3, counters=True), [])


GUARD_COUNTERS = {"app": ("ui_guard_custody_ns", "ui_guard_custodies", "frame_dispatch_ns", "frame_dispatches",
                           "frame_dispatch_failed_ns", "frame_dispatches_failed"),
                  "vt": ("parser_lock_wait_ns", "parser_sections")}


class GuardCounterTests(unittest.TestCase):
    """The guard-custody, frame-dispatch and worker-wait totals are counter fields an older base lacks."""

    def test_the_new_totals_are_counter_fields_a_base_may_lack(self):
        # Each new total is a count field of its section; a head must carry it, and a base built before it
        # passes as partial, so the comparison shows n/a for it and never a measured zero.
        for section, names in GUARD_COUNTERS.items():
            for name in names:
                with self.subTest(field=f"{section}.{name}"):
                    self.assertIn(name, perf_result_schema.FRAME_COUNTER_FIELDS[section][0])
        older = frame_counters()
        for section, names in GUARD_COUNTERS.items():
            for name in names:
                del older[section][name]
        self.assertTrue(perf_result_schema.frame_counter_problems(older))
        self.assertEqual(perf_result_schema.frame_counter_problems(older, partial=True), [])


class CounterBuildTests(unittest.TestCase):
    def tree(self, manifest, logging_source):
        """A worktree with the app manifest and, unless None, the logging crate's lib.rs."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / perf_settings.APP_MANIFEST).parent.mkdir(parents=True)
        (root / perf_settings.APP_MANIFEST).write_text(manifest, encoding="utf-8")
        if logging_source is not None:
            (root / LOGGING_LIB).parent.mkdir(parents=True)
            (root / LOGGING_LIB).write_text(logging_source, encoding="utf-8")
        return root

    def test_support_needs_the_feature_and_the_filtered_logging_init(self):
        # The head's counters harness calls sonicterm_logging::init_in_with_filter, so a tree that declares the
        # feature without that function (4d19e855, d957b88f) cannot build the overlaid harness with it.
        self.assertTrue(perf_harness_sources.tree_supports_counters(self.tree(COUNTERS_MANIFEST, LOGGING_WITH_FILTER)))
        unsupported = {
            "the feature without the function": (COUNTERS_MANIFEST, LOGGING_WITHOUT_FILTER),
            "the function only in a comment": (COUNTERS_MANIFEST, "// pub fn init_in_with_filter(dir: &Path)\n"),
            "the feature without the logging crate": (COUNTERS_MANIFEST, None),
            "the function without the feature": (perf_home_tests.BASE_MANIFEST, LOGGING_WITH_FILTER),
            "neither": (perf_home_tests.BASE_MANIFEST, LOGGING_WITHOUT_FILTER),
        }
        for case, (manifest, logging_source) in unsupported.items():
            with self.subTest(case):
                self.assertFalse(perf_harness_sources.tree_supports_counters(self.tree(manifest, logging_source)))

    def test_a_tree_supports_counters_only_when_its_features_table_declares_the_key(self):
        # A comment, another table or a value naming the feature is not a declaration.
        declared = (FEATURES_TABLE,
                    '[features]\ndefault = []\nperf-counters = ["sonicterm-gpu/perf-counters"]  # the gate\n',
                    '[features]\n"perf-counters" = []\n', COUNTERS_MANIFEST)
        absent = (perf_home_tests.BASE_MANIFEST, perf_home_tests.HEAD_MANIFEST, "[features]\ndefault = []\n", "[features]\n# perf-counters = []\n",
                  "[package.metadata]\nperf-counters = []\n", "# [features]\n# perf-counters = []\n",
                  '[features]\ndefault = ["perf-counters"]\n')
        for manifest in declared:
            with self.subTest(manifest=manifest):
                self.assertTrue(perf_harness_sources.declares_perf_counters(manifest))
        for manifest in absent:
            with self.subTest(manifest=manifest):
                self.assertFalse(perf_harness_sources.declares_perf_counters(manifest))

    def test_the_build_adds_the_feature_only_when_asked(self):
        # The feature pair is appended to the locked build; the plain build is unchanged.
        plain = perf_scenarios.build_argv("perf_scenarios", release=True)
        self.assertEqual(perf_scenarios.build_argv("perf_scenarios", release=True, counters=False), plain)
        self.assertEqual(perf_scenarios.build_argv("perf_scenarios", release=True, counters=True),
                         plain + ("--features", "perf-counters"))

    def test_only_a_counters_run_passes_counters_to_the_harness(self):
        # The harness forces the gate on only for a run started with --counters.
        argv = perf_scenarios.harness_argv(Path("/b"), "S1", "default", perf_memory_tests.HARNESS_HASH, Path("/s"), counters=True)
        self.assertIn("--counters", argv)
        self.assertNotIn("--counters", perf_scenarios.harness_argv(Path("/b"), "S1", "default", perf_memory_tests.HARNESS_HASH, Path("/s")))


class CounterCliTests(unittest.TestCase):
    def parse(self, *argv):
        with contextlib.redirect_stderr(io.StringIO()):
            return perf_arguments.parse_args(list(argv))

    def test_counters_and_their_run_count_parse(self):
        # --counters-runs is optional; without it the counters set takes --runs.
        args = self.parse("--base", "main", "--head", "HEAD", "--counters", "--counters-runs", "2")
        self.assertEqual((args.counters, args.counters_runs), (True, 2))
        args = self.parse("--base", "main", "--head", "HEAD")
        self.assertEqual((args.counters, args.counters_runs), (False, None))

    def test_the_smoke_and_a_bare_run_count_are_refused(self):
        # The smoke takes no comparison option; a counters run count without --counters has nothing to count.
        for argv in (("--smoke", "--counters"), ("--smoke", "--counters-runs", "2"),
                     ("--base", "main", "--head", "HEAD", "--counters-runs", "2"),
                     ("--base", "main", "--head", "HEAD", "--counters", "--counters-runs", "0")):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                self.parse(*argv)
