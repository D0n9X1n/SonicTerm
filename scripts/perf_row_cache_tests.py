"""Perf row cache tests."""

from __future__ import annotations
import dataclasses
import unittest

import perf_atlas_tests
import perf_comparison_rows
import perf_comparison_tests
import perf_counter_rows
import perf_counter_rows_tests
import perf_counter_schema_tests
import perf_memory
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests

class RowGlyphCacheReportTests(unittest.TestCase):
    """The row glyph cache's checkpoint field and the derived counter rows its evaluation reads."""

    LINE = ("2026-10-03T00:00:01Z  INFO memory: memory snapshot process_resident_bytes=10 "
            "session_total_bytes=3 renderer_total_bytes=7 renderer_row_glyph_cache_bytes=5 "
            "checkpoint_index=0 checkpoint_label=\"end\" checkpoint_attempt=1 checkpoint_complete=true")

    def test_the_checkpoint_field_parses_and_an_older_line_lacks_it(self):
        # The field is optional: a line from an older build reads None and still parses.
        self.assertEqual(perf_memory.parse_memory_line(self.LINE).renderer_row_glyph_cache_bytes, 5)
        self.assertIsNone(perf_memory.parse_memory_line(perf_memory_tests.memory_line()).renderer_row_glyph_cache_bytes)

    def test_complete_samples_differing_only_in_row_cache_bytes_conflict(self):
        # The field joins the totals a reading compares, so two complete samples of one attempt that agree
        # on every other total but differ in row-cache bytes are conflicting samples, not one reading.
        first = dataclasses.replace(perf_atlas_tests.tagged_sample(0, 1, True), renderer_row_glyph_cache_bytes=10)
        second = dataclasses.replace(perf_atlas_tests.tagged_sample(0, 1, True), renderer_row_glyph_cache_bytes=11)
        self.assertEqual(perf_memory.checkpoint_memory([first, second], 0).problem, "conflicting samples")
        self.assertIsNone(perf_memory.checkpoint_memory([first, first], 0).problem)

    def test_the_checkpoint_row_is_reported_n_a_on_an_older_base_and_partial_when_partial(self):
        # A base whose lines lack the field reads n/a for that row while its other totals still compare;
        # a reading from a partial attempt is marked partial, as renderer_total_bytes is.
        def outcome(cache_bytes, complete=True):
            out = perf_comparison_tests.timed_outcome([1.0])
            sample = dataclasses.replace(out.memory[0], renderer_row_glyph_cache_bytes=cache_bytes,
                                         checkpoint_complete=complete)
            return dataclasses.replace(out, memory=[sample])
        rows = perf_comparison_rows.comparison_rows("S3/default", perf_metrics.SideRuns([outcome(None)]),
                                    perf_metrics.SideRuns([outcome(64 * 1048576)]))
        self.assertEqual(perf_comparison_tests.row_for(rows, "end renderer_row_glyph_cache_bytes (MiB)")[2:4],
                         ["n/a", "64.00 (64.00–64.00)"])
        self.assertEqual(perf_comparison_tests.row_for(rows, "end renderer_total_bytes (MiB)")[2], "100.00 (100.00–100.00)")
        partial = perf_comparison_rows.comparison_rows("S3/default", perf_metrics.SideRuns([outcome(None)]),
                                       perf_metrics.SideRuns([outcome(64 * 1048576, complete=False)]))
        self.assertIn("partial", perf_comparison_tests.row_for(partial, "end renderer_row_glyph_cache_bytes (MiB)")[3])

    def test_derived_rows_name_their_denominators_and_pool_as_labelled(self):
        # The hit ratio pools every counters run; the assembly mean is exact per run (sum_us / samples)
        # with the pooled mean; shape requests per drawn frame and the fallback ratio are context rows.
        base = perf_counter_rows_tests.counters_side(
            {"renderer.row_cache_hits": 30, "renderer.row_cache_misses": 70, "renderer.shape_requests": 40,
             "renderer.gpu_frames": 10, "renderer.assembly_us": ([0, 0, 4, 0, 0, 0, 0], 400)},
            {"renderer.row_cache_hits": 10, "renderer.row_cache_misses": 90, "renderer.shape_requests": 60,
             "renderer.gpu_frames": 10, "renderer.assembly_us": ([0, 0, 2, 0, 0, 0, 0], 300)})
        head = perf_counter_rows_tests.counters_side(
            {"renderer.row_cache_hits": 90, "renderer.row_cache_misses": 10, "renderer.shape_requests": 10,
             "renderer.gpu_frames": 10, "renderer.partial_frames": 3, "renderer.partial_fallbacks": 1,
             "renderer.assembly_us": ([0, 4, 0, 0, 0, 0, 0], 200)})
        rows, _omitted = perf_counter_rows.counter_rows("S3/default", base, head)
        derived = {row[2]: row for row in rows}
        hit = derived["row-cache hit ratio = hits / (hits + misses), counters runs pooled"]
        self.assertEqual(hit[3:], ["0.200 (40/200, 2/2 runs)", "0.900 (90/100, 1/1 runs)", "+350.0%"])
        assembly = derived["assembly mean per counters run = assembly_sum_us / Σ assembly_buckets, per run"]
        self.assertEqual(assembly[3], "run 1 100.00 us, run 2 150.00 us; pooled 116.67 us (2/2 runs)")
        self.assertEqual(assembly[4], "run 1 50.00 us; pooled 50.00 us (1/1 runs)")
        shapes = derived["shape+measure requests per drawn frame = shape_requests / (gpu_frames + "
                         "software_frames), context only"]
        self.assertEqual(shapes[3:5], ["5.000 (100/20, 2/2 runs)", "1.000 (10/10, 1/1 runs)"])
        fallback = derived["partial fallback ratio = partial_fallbacks / (partial_frames + partial_fallbacks), "
                           "context only"]
        self.assertEqual(fallback[3:], ["n/a (denominator 0, 2/2 runs)", "0.250 (1/4, 1/1 runs)", "n/a"])

    def test_cache_reuse_rows_are_pooled_per_assembly_and_a_base_without_them_reads_na(self):
        # The title and chrome-run reuse rows divide the pooled reuses by the pooled assembly samples; a
        # base that predates the counters carries no field and reads n/a, while the head's figure stands.
        older = perf_counter_schema_tests.counters_result({"renderer.assembly_us": ([0, 0, 4, 0, 0, 0, 0], 400)})
        for phase in older["phases"]:
            # A base built before the counters existed reports none of them.
            for name in ("tab_title_reuses", "tab_title_prepares", "chrome_run_reuses", "chrome_run_prepares"):
                del phase["frame_counters"]["renderer"][name]
        base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=older)])
        head = perf_counter_rows_tests.counters_side(
            {"renderer.tab_title_reuses": 9, "renderer.chrome_run_reuses": 10,
             "renderer.assembly_us": ([0, 3, 0, 0, 0, 0, 0], 300)},
            {"renderer.tab_title_reuses": 3, "renderer.chrome_run_reuses": 0,
             "renderer.assembly_us": ([0, 1, 0, 0, 0, 0, 0], 100)})
        rows, _omitted = perf_counter_rows.counter_rows("S8/default", base, head)
        derived = {row[2]: row for row in rows}
        titles = derived["tab-title reuses per assembly = tab_title_reuses / Σ assembly_buckets, "
                         "counters runs pooled"]
        self.assertEqual(titles[4], "3.000 (12/4, 2/2 runs)")
        self.assertTrue(titles[3].startswith("n/a"), titles[3])
        runs = derived["chrome-run reuses per assembly = chrome_run_reuses / Σ assembly_buckets, "
                       "counters runs pooled"]
        self.assertEqual(runs[4], "2.500 (10/4, 2/2 runs)")
        idle = perf_counter_rows_tests.counters_side({"renderer.tab_title_reuses": 0, "renderer.assembly_us": ([0] * 7, 0)})
        rows, _omitted = perf_counter_rows.counter_rows("S8/default", idle, idle)
        idle_row = {row[2]: row for row in rows}.get(
            "tab-title reuses per assembly = tab_title_reuses / Σ assembly_buckets, counters runs pooled")
        self.assertIsNone(idle_row, "a phase that assembled nothing prints no reuse row")

    def test_assembly_means_keep_every_runs_position(self):
        # A run that assembled nothing and a run whose result lacks the histogram keep their positions
        # and read n/a; the pooled mean is still the sum over every sampled run's events.
        missing = perf_counter_schema_tests.counters_result({"renderer.assembly_us": ([0, 2, 0, 0, 0, 0, 0], 100)})
        del missing["phases"][0]["frame_counters"]["renderer"]["assembly_us"]
        side = perf_metrics.SideRuns(outcomes=[
            perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({"renderer.row_cache_hits": 1})),
            perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({"renderer.assembly_us": ([0, 4, 0, 0, 0, 0, 0], 200)})),
            perf_outcomes_tests.make_outcome(result=missing),
            perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({"renderer.assembly_us": ([0, 1, 0, 0, 0, 0, 0], 100)}))])
        rows, _omitted = perf_counter_rows.counter_rows("S3/default", perf_metrics.SideRuns(blocked=perf_counter_rows.COUNTERS_HEAD_ONLY), side)
        assembly = [row for row in rows if row[2].startswith("assembly mean per counters run")]
        self.assertEqual(assembly[0][4], "run 1 n/a (no assembly), run 2 50.00 us, run 3 n/a (no histogram), "
                                         "run 4 100.00 us; pooled 60.00 us (2/4 runs)")
        quiet = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({}))])
        self.assertEqual(perf_counter_rows._assembly_means(perf_counter_rows._counter_phases(quiet)["workload"]),
                         ("run 1 n/a (no assembly); pooled n/a (0/1 runs)", None))

    def test_a_side_without_the_fields_reads_n_a_and_all_zero_rows_are_left_out(self):
        # A base lacking the counters reads n/a with no change; a phase where no side has a denominator
        # prints no derived row at all.
        older = perf_counter_schema_tests.counters_result({})
        del older["phases"][0]["frame_counters"]["renderer"]["row_cache_hits"]
        base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=older)])
        head = perf_counter_rows_tests.counters_side({"renderer.row_cache_hits": 3, "renderer.row_cache_misses": 1})
        rows, _omitted = perf_counter_rows.counter_rows("S7/default", base, head)
        hit = [row for row in rows if row[2].startswith("row-cache hit ratio")]
        self.assertEqual(hit[0][3:], ["n/a", "0.750 (3/4, 1/1 runs)", "n/a"])
        quiet, _omitted = perf_counter_rows.counter_rows("S1/default", perf_counter_rows_tests.counters_side({}), perf_counter_rows_tests.counters_side({}))
        self.assertFalse([row for row in quiet if "=" in row[2]])
