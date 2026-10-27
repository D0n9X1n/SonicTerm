"""Perf counter rows tests."""

from __future__ import annotations
import json
import unittest

import perf_atlas_tests
import perf_counter_rows
import perf_counter_schema_tests
import perf_execution
import perf_fallback_rows
import perf_foreground_tests
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_report
import perf_result_schema
import perf_result_tests


def counters_side(*values_per_run):
    """The head's valid counters runs, one per values map."""
    return perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result(values)) for values in values_per_run])


class CounterTableTests(unittest.TestCase):
    def test_a_counters_run_whose_frames_disagree_with_its_presenter_is_noted(self):
        # result.json records the presenter on Windows and macOS: GDI frames count as software_frames, wgpu frames
        # as gpu_frames. A run whose counts contradict its record is named in a note, never passed silently; a
        # consistent run, or one that recorded no presenter (an older base), adds nothing.
        gdi = dict(perf_foreground_tests.WGPU_PRESENTER, software_render_degraded=True, windows_gdi=True)
        consistent = perf_metrics.SideRuns(outcomes=[
            perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({"renderer.software_frames": 40}, presenter=gdi)),
            perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({"renderer.gpu_frames": 40}, presenter=perf_foreground_tests.WGPU_PRESENTER)),
            perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({"renderer.gpu_frames": 40})),
            perf_outcomes_tests.make_outcome(result=perf_result_tests.without_presenter(perf_counter_schema_tests.counters_result({"renderer.software_frames": 40})))])
        self.assertEqual(perf_counter_rows.presenter_counter_notes("S1/default", "head", consistent), [])
        wrong = perf_metrics.SideRuns(outcomes=[
            perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({"renderer.gpu_frames": 3, "renderer.software_frames": 37},
                                                presenter=gdi)),
            perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result({"renderer.software_frames": 5}, presenter=perf_foreground_tests.WGPU_PRESENTER))])
        notes = perf_counter_rows.presenter_counter_notes("S1/default", "head", wrong)
        self.assertEqual(len(notes), 2, notes)
        self.assertIn("S1/default head run 1", notes[0])
        self.assertIn("GDI", notes[0])
        self.assertIn("3 gpu_frames", notes[0])
        self.assertIn("S1/default head run 2", notes[1])
        self.assertIn("5 software_frames", notes[1])

    def test_counts_are_medians_and_histograms_bucket_bounds(self):
        # Counts are medians across runs; a histogram's p95 and max are bucket bounds, overflow included, and
        # its mean is the pooled sum_us over the event count, in the histogram's unit (sum_us is always
        # microseconds). A field 0 in every run is left out and counted.
        side = counters_side(
            {"window.attempts": 4, "window.handler_ms": ([0, 0, 0, 5, 5, 0, 0, 0, 0, 0], 130_000)},
            {"window.attempts": 6, "app.wake_user": 3, "window.handler_ms": ([0, 0, 0, 4, 5, 0, 0, 0, 0, 1], 170_000),
             "vt.parse_us": ([0, 0, 0, 1, 0, 0, 1], 6050)})
        head_only = perf_metrics.SideRuns(blocked=perf_counter_rows.COUNTERS_HEAD_ONLY)
        rows, omitted = perf_counter_rows.counter_rows("S1/default", head_only, side)
        self.assertEqual(rows, [
            ["S1/default", "", "status", "n/a", "2 valid runs", ""],
            ["S1/default", "workload", "window.attempts (count)", "n/a", "5 (4–6)", "n/a"],
            ["S1/default", "workload", "window.handler_ms (ms)", "n/a",
             "p95 ≤17 ms, max >100 ms, mean 15.00 ms (20 events)", "n/a"],
            ["S1/default", "workload", "app.wake_user (count)", "n/a", "1.5 (0–3)", "n/a"],
            ["S1/default", "workload", "vt.parse_us (us)", "n/a",
             "p95 >5000 us, max >5000 us, mean 3025.00 us (2 events)", "n/a"],
            ["S1/default", "workload", "renderer attempt split (pooled)", "n/a", "no render attempts (2/2 runs)", ""]])
        self.assertEqual(omitted, perf_counter_schema_tests.CONTRACT_FIELD_COUNT - 4)

    def test_the_s11_release_reshow_phase_prints_full_frames_max_assembly_and_presented(self):
        # S11/release's re-show is read from the generic per-phase counters: its phase is named reshow, and the
        # table prints that phase's full frames, the max assembly time and the presented frames on both sides.
        def reshow_side(full_frames, presented, assembly):
            result = perf_counter_schema_tests.counters_result({"renderer.full_frames": full_frames, "window.presented": presented,
                                      "renderer.assembly_us": assembly})
            result["phases"][0]["name"] = "reshow"
            return perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=result)])
        base = reshow_side(2, 3, ([0, 0, 0, 2, 1, 0, 0], 2600))
        head = reshow_side(1, 3, ([0, 0, 0, 3, 0, 0, 0], 900))
        rows, _omitted = perf_counter_rows.counter_rows("S11/release", base, head)
        self.assertIn(["S11/release", "reshow", "renderer.full_frames (count)", "2 (2–2)", "1 (1–1)", "-50.0%"], rows)
        self.assertIn(["S11/release", "reshow", "window.presented (count)", "3 (3–3)", "3 (3–3)", "+0.0%"], rows)
        self.assertIn(["S11/release", "reshow", "renderer.assembly_us (us)",
                       "p95 ≤1000 us, max ≤1000 us, mean 866.67 us (3 events)",
                       "p95 ≤500 us, max ≤500 us, mean 300.00 us (3 events)", "-65.4%"], rows)

    def test_an_integer_microsecond_field_is_labelled_a_summed_duration(self):
        # An integer field named *_us holds summed microseconds, so it reads (us, summed), never (count); its change
        # still compares the medians. A plain count keeps (count).
        base = counters_side({"renderer.row_cache_invalidate_us": 1000, "renderer.row_cache_hits": 10})
        head = counters_side({"renderer.row_cache_invalidate_us": 1250, "renderer.row_cache_hits": 12})
        rows, _omitted = perf_counter_rows.counter_rows("S2/default", base, head)
        self.assertIn(["S2/default", "workload", "renderer.row_cache_invalidate_us (us, summed)", "1000 (1000–1000)",
                       "1250 (1250–1250)", "+25.0%"], rows)
        self.assertIn(["S2/default", "workload", "renderer.row_cache_hits (count)", "10 (10–10)", "12 (12–12)",
                       "+20.0%"], rows)

    def test_a_head_only_set_is_never_compared(self):
        # Without base counters every base and change cell is n/a; a head without valid runs has a status row only.
        self.assertIn("Change", perf_fallback_rows.COUNTERS_HEADER)
        head_only = perf_metrics.SideRuns(blocked=perf_counter_rows.COUNTERS_HEAD_ONLY)
        rows, omitted = perf_counter_rows.counter_rows("S1/default", head_only, perf_metrics.SideRuns(failed="focus: theft"))
        self.assertEqual((rows, omitted), ([["S1/default", "", "status", "n/a", "failed: focus: theft", ""]], 0))

    def test_both_sides_compare_and_a_field_the_base_lacks_reads_n_a(self):
        # A count's change compares medians and a histogram's compares means; a newer field the base's contract
        # lacks reads n/a there, with no change.
        base_result = perf_counter_schema_tests.counters_result({"window.attempts": 4, "window.handler_ms": ([0, 0, 0, 2, 0, 0, 0, 0, 0, 0], 20_000)})
        del base_result["phases"][0]["frame_counters"]["renderer"]["full_frames"]
        base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=base_result)])
        head = counters_side({"window.attempts": 6, "renderer.full_frames": 3,
                              "window.handler_ms": ([0, 0, 0, 2, 0, 0, 0, 0, 0, 0], 24_000)})
        rows, omitted = perf_counter_rows.counter_rows("S1/default", base, head)
        self.assertEqual(rows, [
            ["S1/default", "", "status", "1 valid run", "1 valid run", ""],
            ["S1/default", "workload", "window.attempts (count)", "4 (4–4)", "6 (6–6)", "+50.0%"],
            ["S1/default", "workload", "window.handler_ms (ms)", "p95 ≤12 ms, max ≤12 ms, mean 10.00 ms (2 events)",
             "p95 ≤12 ms, max ≤12 ms, mean 12.00 ms (2 events)", "+20.0%"],
            ["S1/default", "workload", "renderer.full_frames (count)", "n/a", "3 (3–3)", "n/a"],
            # Both sides carry the attempt fields and drew none, which reads as one explicit row.
            ["S1/default", "workload", "renderer attempt split (pooled)", "no render attempts (1/1 runs)",
             "no render attempts (1/1 runs)", ""]])
        self.assertEqual(omitted, perf_counter_schema_tests.CONTRACT_FIELD_COUNT - 3)

    def test_foreground_worker_fields_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # The foreground worker's counters joined the app section: a head must report them, a base built before
        # them reads n/a, and a base that supports them and saw nothing prints a real 0.
        worker_fields = ("fg_worker_probes", "fg_worker_panes", "fg_results_stale", "fg_worker_probe_us")
        lacking = perf_counter_schema_tests.counters_result()
        for name in worker_fields:
            del lacking["phases"][0]["frame_counters"]["app"][name]
        self.assertEqual(perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        for name in worker_fields:
            with self.subTest(name=name):
                self.assertTrue(any(f"app.{name}" in problem for problem in problems), problems)
        head = counters_side({"app.fg_worker_probes": 2, "app.fg_worker_panes": 3})
        older_base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])
        rows, _omitted = perf_counter_rows.counter_rows("S2/flood", older_base, head)
        cells = {row[2]: (row[3], row[4]) for row in rows[1:]}
        self.assertEqual(cells["app.fg_worker_probes (count)"], ("n/a", "2 (2–2)"))
        self.assertEqual(cells["app.fg_worker_panes (count)"], ("n/a", "3 (3–3)"))
        supported_zero = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])
        rows, _omitted = perf_counter_rows.counter_rows("S2/flood", supported_zero, head)
        cells = {row[2]: (row[3], row[4]) for row in rows[1:]}
        self.assertEqual(cells["app.fg_worker_probes (count)"], ("0 (0–0)", "2 (2–2)"))

    def test_nanosecond_fields_are_labelled_and_shown_in_microseconds_without_rounding_the_figure(self):
        # A _ns field is summed nanoseconds: its cell shows microseconds with two decimals, while the change
        # compares the exact nanosecond medians, so 1,999 ns and 2,999 ns read 2.00 and 3.00 us and +50.0%.
        base = counters_side({"renderer.shape_ns": 1999})
        head = counters_side({"renderer.shape_ns": 2999})
        rows, _omitted = perf_counter_rows.counter_rows("S9/default", base, head)
        cells = {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}
        self.assertEqual(cells["renderer.shape_ns (us, summed from ns)"], ("2.00 (2.00–2.00)", "3.00 (3.00–3.00)",
                                                                          "+50.0%"))

    def test_the_attempt_split_pools_raw_totals_so_its_shares_add_up(self):
        # Three runs (attempt, shape, raster ns) = (100, 90, 0), (100, 0, 90), (1000, 500, 500): field-by-field
        # medians would give (100, 90, 90), a negative remainder. Pooling gives (1200, 590, 590): 49.2%, 49.2%
        # and 1.7% for the other 20 ns, with per-attempt means over the 3 pooled attempts.
        def run(attempt_ns, shape_ns, raster_ns):
            return {"renderer.apply_attempts": 1, "renderer.apply_attempt_ns": attempt_ns,
                    "renderer.apply_attempt_shape_ns": shape_ns, "renderer.apply_attempt_raster_ns": raster_ns,
                    "renderer.apply_attempt_shape_requests": 4, "renderer.apply_attempt_raster_calls": 2,
                    "renderer.render_attempts": 1, "renderer.render_attempt_ns": attempt_ns}
        head = counters_side(run(100, 90, 0), run(100, 0, 90), run(1000, 500, 500))
        text, attempts = perf_counter_rows.attempt_split(perf_counter_rows._counter_phases(head)["workload"], "apply_")
        self.assertEqual(attempts, 3)
        self.assertEqual(text, "3 attempts (3/3 runs): shaping 49.2%, rasterizing 49.2%, other 1.7%; per attempt "
                               "0.40 us, 4.0 shape requests, 2.0 raster calls, 0.0 tiles")
        # The two splits join the counters table after the phase's fields, once each.
        rows, _omitted = perf_counter_rows.counter_rows("S9/default", counters_side(), head)
        names = [row[2] for row in rows if row[2].startswith("renderer attempt split")]
        self.assertEqual(names, ["renderer attempt split: every attempt (pooled)",
                                 "renderer attempt split: fallback apply attempts (pooled)"])

    def test_the_attempt_split_reports_no_attempts_excludes_incomplete_runs_and_reads_n_a_on_an_older_base(self):
        # Zero apply attempts reads "no apply attempts", never 0%. A run missing any field of the class is left
        # out of every field of it, and the scope names how many runs counted. A base built before the counters
        # reads n/a for the split, and a phase where no side drew an attempt adds no split rows.
        quiet = counters_side({"renderer.render_attempts": 2, "renderer.render_attempt_ns": 50})
        phases = perf_counter_rows._counter_phases(quiet)["workload"]
        self.assertEqual(perf_counter_rows.attempt_split(phases, "apply_"), ("no apply attempts (1/1 runs)", 0))
        complete = perf_counter_schema_tests.counters_result({"renderer.apply_attempts": 1, "renderer.apply_attempt_ns": 10})
        lacking = perf_counter_schema_tests.counters_result({"renderer.apply_attempts": 5, "renderer.apply_attempt_ns": 999})
        del lacking["phases"][0]["frame_counters"]["renderer"]["apply_attempt_raster_tiles"]
        side = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=complete), perf_outcomes_tests.make_outcome(result=lacking)])
        text, attempts = perf_counter_rows.attempt_split(perf_counter_rows._counter_phases(side)["workload"], "apply_")
        self.assertEqual(attempts, 1)
        self.assertTrue(text.startswith("1 attempts (1/2 runs): "), text)
        older = perf_counter_schema_tests.counters_result()
        for name in list(older["phases"][0]["frame_counters"]["renderer"]):
            if "attempt" in name:
                del older["phases"][0]["frame_counters"]["renderer"][name]
        rows, _omitted = perf_counter_rows.counter_rows("S9/default", perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=older)]), quiet)
        split = {row[2]: (row[3], row[4]) for row in rows if row[2].startswith("renderer attempt split")}
        self.assertEqual(split["renderer attempt split: fallback apply attempts (pooled)"],
                         ("n/a", "no apply attempts (1/1 runs)"))
        # A phase where neither side drew an attempt reads as one explicit row, never omitted.
        rows, _omitted = perf_counter_rows.counter_rows("S9/default", counters_side({}), counters_side({}))
        split = [row for row in rows if row[2].startswith("renderer attempt split")]
        self.assertEqual([row[2:5] for row in split], [["renderer attempt split (pooled)",
                                                        "no render attempts (1/1 runs)",
                                                        "no render attempts (1/1 runs)"]])

    def test_the_harness_attempt_fixture_pools_to_exact_shares_and_lists_each_run(self):
        # The fixture is written by the harness from cumulative totals through its own delta and serializer
        # (counters_tests.rs). Run 1's (1999, 999, 999) -> (2999, 1499, 1499) ns arrives as (1000, 500, 500),
        # so pooling with run 2 gives (4000, 1500, 2000): 37.5%, 50.0% and 12.5%, never a negative remainder.
        fixture = json.loads(perf_atlas_tests.ATTEMPT_FIXTURE.read_text(encoding="utf-8"))
        def run(renderer):
            result = perf_counter_schema_tests.counters_result()
            result["phases"][0]["frame_counters"]["renderer"].update(renderer)
            return perf_outcomes_tests.make_outcome(result=result)
        head = perf_metrics.SideRuns(outcomes=[run(entry["renderer"]) for entry in fixture["runs"]])
        text, attempts = perf_counter_rows.attempt_split(perf_counter_rows._counter_phases(head)["workload"], "apply_")
        self.assertEqual(attempts, 4)
        self.assertEqual(text, "4 attempts (2/2 runs): shaping 37.5%, rasterizing 50.0%, other 12.5%; per attempt "
                               "1.00 us, 2.0 shape requests, 1.5 raster calls, 1.0 tiles")
        rows, _omitted = perf_counter_rows.counter_rows("S9/default", counters_side(), head)
        cells = {row[2]: row[4] for row in rows[1:]}
        self.assertEqual(cells["renderer.apply_attempt_ns (us, summed from ns)"], "2.00 (1.00–3.00)")
        details = perf_counter_rows.attempt_split_details("S9/default", counters_side(), head)
        self.assertEqual(len(details), 2)
        self.assertTrue(details[0].startswith("- S9/default head run 1 workload: every attempt 1 attempts"), details)
        self.assertIn("shaping 50.0%, rasterizing 50.0%, other 0.0%", details[0])
        # A run of a phase with attempts is listed even when it applied nothing, so both pooled rows' whole
        # populations can be read run by run; a phase with only ordinary attempts is listed too.
        quiet = perf_counter_schema_tests.counters_result({"renderer.render_attempts": 2, "renderer.render_attempt_ns": 50})
        mixed = perf_metrics.SideRuns(outcomes=[*head.outcomes, perf_outcomes_tests.make_outcome(result=quiet)])
        details = perf_counter_rows.attempt_split_details("S9/default", counters_side({}), mixed)
        self.assertEqual(len(details), 4, details)
        self.assertIn("head run 3 workload: every attempt 2 attempts (1/1 runs)", details[-1])
        self.assertIn("fallback apply attempts no apply attempts (1/1 runs)", details[-1])
        ordinary = perf_counter_rows.attempt_split_details("S9/default", counters_side({}), perf_metrics.SideRuns(
            outcomes=[perf_outcomes_tests.make_outcome(result=quiet)]))
        self.assertEqual(len(ordinary), 2, ordinary)
        self.assertIn("head run 1 workload: every attempt 2 attempts", ordinary[-1])

    def test_fallback_applies_read_n_a_on_an_older_base_and_a_real_zero_on_a_supporting_one(self):
        # font_fallback_applies joined the renderer section: a base built before it reads n/a with no change, a
        # supporting base that applied nothing prints a real 0, and the head's value is compared against that 0.
        lacking = perf_counter_schema_tests.counters_result()
        del lacking["phases"][0]["frame_counters"]["renderer"]["font_fallback_applies"]
        head = counters_side({"renderer.font_fallback_applies": 2})
        older_base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])
        rows, _omitted = perf_counter_rows.counter_rows("S9/default", older_base, head)
        cells = {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}
        self.assertEqual(cells["renderer.font_fallback_applies (count)"], ("n/a", "2 (2–2)", "n/a"))
        supported_zero = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])
        rows, _omitted = perf_counter_rows.counter_rows("S9/default", supported_zero, head)
        cells = {row[2]: (row[3], row[4]) for row in rows[1:]}
        self.assertEqual(cells["renderer.font_fallback_applies (count)"], ("0 (0–0)", "2 (2–2)"))

    def test_dropped_acknowledgements_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # dirt_ack_dropped joined the window section: a head must report it, a base built before it reads n/a
        # with no change shown, a supporting base that dropped nothing prints a real 0, and a gate-off run
        # carries no phase counters, so it is never checked for the field.
        lacking = perf_counter_schema_tests.counters_result()
        del lacking["phases"][0]["frame_counters"]["window"]["dirt_ack_dropped"]
        problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("window.dirt_ack_dropped" in problem for problem in problems), problems)
        self.assertEqual(perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"window.dirt_ack_dropped": 3})

        def dropped_cells(base):
            rows, _omitted = perf_counter_rows.counter_rows("S3/default", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["window.dirt_ack_dropped (count)"]

        self.assertEqual(dropped_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])),
                         ("n/a", "3 (3–3)", "n/a"))
        self.assertEqual(dropped_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])),
                         ("0 (0–0)", "3 (3–3)", perf_metrics.percent_change(0, 3)))
        gate_off = perf_result_tests.valid_result(frame_counters="off")
        self.assertEqual(perf_result_schema.validate_result(gate_off, perf_memory_tests.HARNESS_HASH, 0, counters=False), [])

    def test_hover_counters_print_their_zeros_beside_any_hover_event_and_n_a_on_an_older_base(self):
        # The hover counters are head-only App fields: a head must report them, and a base built before them
        # reads n/a with no change. Once any of them saw an event in a phase, every hover row prints, so a real 0
        # detections beside nonzero lookups is shown instead of omitted; a phase with no hover event omits them.
        lacking = perf_counter_schema_tests.counters_result()
        del lacking["phases"][0]["frame_counters"]["app"]["hover_detections"]
        problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("app.hover_detections" in problem for problem in problems), problems)
        self.assertEqual(perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"app.hover_lookups": 40, "app.hover_cache_hits": 40})
        rows, _omitted = perf_counter_rows.counter_rows("S6/rest", perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)]), head)
        cells = {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}
        self.assertEqual(cells["app.hover_detections (count)"], ("n/a", "0 (0–0)", "n/a"))
        self.assertEqual(cells["app.path_probes_submitted (count)"], ("0 (0–0)", "0 (0–0)", perf_metrics.percent_change(0, 0)))
        self.assertEqual(cells["app.hover_cache_hits (count)"][1], "40 (40–40)")
        idle = counters_side({"window.attempts": 3})
        rows, _omitted = perf_counter_rows.counter_rows("S1/default", idle, idle)
        self.assertFalse(any(row[2].startswith("app.hover_") for row in rows[1:]), rows)

    def test_stream_clock_exemptions_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # stream_clock_exempt joined the window section: a head must report it, a base built before it reads n/a
        # with no change shown, a supporting base that exempted nothing prints a real 0, and a gate-off run
        # carries no phase counters, so it is never checked for the field.
        lacking = perf_counter_schema_tests.counters_result()
        del lacking["phases"][0]["frame_counters"]["window"]["stream_clock_exempt"]
        problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("window.stream_clock_exempt" in problem for problem in problems), problems)
        self.assertEqual(perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"window.stream_clock_exempt": 5})

        def exempt_cells(base):
            rows, _omitted = perf_counter_rows.counter_rows("S2/default", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["window.stream_clock_exempt (count)"]

        self.assertEqual(exempt_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])),
                         ("n/a", "5 (5–5)", "n/a"))
        self.assertEqual(exempt_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])),
                         ("0 (0–0)", "5 (5–5)", perf_metrics.percent_change(0, 5)))
        gate_off = perf_result_tests.valid_result(frame_counters="off")
        self.assertEqual(perf_result_schema.validate_result(gate_off, perf_memory_tests.HARNESS_HASH, 0, counters=False), [])

    def test_display_link_counts_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # The three display-link counts joined the window section: a head must report each, a base built before
        # them reads n/a with no change shown, a supporting base whose link never ran prints a real 0, and a
        # gate-off run carries no phase counters, so it is never checked for them.
        for name in ("display_link_ticks", "display_link_admissions", "display_link_fallbacks"):
            with self.subTest(name=name):
                lacking = perf_counter_schema_tests.counters_result()
                del lacking["phases"][0]["frame_counters"]["window"][name]
                problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
                self.assertTrue(any(f"window.{name}" in problem for problem in problems), problems)
                self.assertEqual(
                    perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
                head = counters_side({f"window.{name}": 4})

                def link_cells(base):
                    rows, _omitted = perf_counter_rows.counter_rows("S4/default", base, head)
                    return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}[f"window.{name} (count)"]

                self.assertEqual(link_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])),
                                 ("n/a", "4 (4–4)", "n/a"))
                self.assertEqual(link_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])),
                                 ("0 (0–0)", "4 (4–4)", perf_metrics.percent_change(0, 4)))
        gate_off = perf_result_tests.valid_result(frame_counters="off")
        self.assertEqual(perf_result_schema.validate_result(gate_off, perf_memory_tests.HARNESS_HASH, 0, counters=False), [])

    def test_damage_waste_is_required_on_the_head_and_n_a_on_an_older_base(self):
        # damage_waste_permille_sum joined the renderer section: a head must report it, a base built before it
        # reads n/a with no change shown, a supporting base that wasted nothing prints a real 0, and a gate-off
        # run carries no phase counters, so it is never checked for the field.
        lacking = perf_counter_schema_tests.counters_result()
        del lacking["phases"][0]["frame_counters"]["renderer"]["damage_waste_permille_sum"]
        problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("renderer.damage_waste_permille_sum" in problem for problem in problems), problems)
        self.assertEqual(perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"renderer.damage_waste_permille_sum": 40})

        def waste_cells(base):
            rows, _omitted = perf_counter_rows.counter_rows("S6/sweep", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["renderer.damage_waste_permille_sum (count)"]

        self.assertEqual(waste_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])),
                         ("n/a", "40 (40–40)", "n/a"))
        self.assertEqual(waste_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])),
                         ("0 (0–0)", "40 (40–40)", perf_metrics.percent_change(0, 40)))
        gate_off = perf_result_tests.valid_result(frame_counters="off")
        self.assertEqual(perf_result_schema.validate_result(gate_off, perf_memory_tests.HARNESS_HASH, 0, counters=False), [])

    def test_partial_counters_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # partial_frames, partial_fallbacks and row_cells_hashed joined the renderer section: a head must
        # report each, a base built before them reads n/a with no change shown, a supporting base that
        # counted none prints a real 0, and a gate-off run carries no phase counters, so it is never checked.
        for name in ("partial_frames", "partial_fallbacks", "row_cells_hashed"):
            with self.subTest(name=name):
                lacking = perf_counter_schema_tests.counters_result()
                del lacking["phases"][0]["frame_counters"]["renderer"][name]
                problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
                self.assertTrue(any(f"renderer.{name}" in problem for problem in problems), problems)
                self.assertEqual(
                    perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
                head = counters_side({f"renderer.{name}": 12})

                def partial_cells(base):
                    rows, _omitted = perf_counter_rows.counter_rows("S2/typing", base, head)
                    return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}[f"renderer.{name} (count)"]

                self.assertEqual(partial_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])),
                                 ("n/a", "12 (12–12)", "n/a"))
                self.assertEqual(partial_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])),
                                 ("0 (0–0)", "12 (12–12)", perf_metrics.percent_change(0, 12)))
        gate_off = perf_result_tests.valid_result(frame_counters="off")
        self.assertEqual(perf_result_schema.validate_result(gate_off, perf_memory_tests.HARNESS_HASH, 0, counters=False), [])

    def test_suppressed_flushes_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # flushes_suppressed joined the vt section: a head's typing phase must report it, a base built before it
        # reads n/a with no change shown, a supporting base that coalesced nothing prints a real 0, and a run
        # with the gate off carries no phase counters, so it is never checked for the field.
        typing = perf_counter_schema_tests.counters_result()
        typing["phases"][0]["name"] = "typing"
        del typing["phases"][0]["frame_counters"]["vt"]["flushes_suppressed"]
        problems = perf_result_schema.validate_result(typing, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("vt.flushes_suppressed" in problem for problem in problems), problems)
        self.assertEqual(perf_result_schema.validate_result(typing, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        def typing_side(values):
            result = perf_counter_schema_tests.counters_result(values)
            result["phases"][0]["name"] = "typing"
            return perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=result)])

        def suppressed_row(base):
            rows, _omitted = perf_counter_rows.counter_rows("S5/default", base, head)
            return [row for row in rows[1:] if row[2] == "vt.flushes_suppressed (count)"]

        # Both sides measure the same typing phase, so only the field's presence can differ.
        head = typing_side({"vt.flushes": 9, "vt.flushes_suppressed": 4})
        older_base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=typing)])
        self.assertEqual(suppressed_row(older_base),
                         [["S5/default", "typing", "vt.flushes_suppressed (count)", "n/a", "4 (4–4)", "n/a"]])
        self.assertEqual(suppressed_row(typing_side({})),
                         [["S5/default", "typing", "vt.flushes_suppressed (count)", "0 (0–0)", "4 (4–4)",
                           perf_metrics.percent_change(0, 4)]])
        gate_off = perf_result_tests.valid_result(frame_counters="off")
        self.assertEqual(perf_result_schema.validate_result(gate_off, perf_memory_tests.HARNESS_HASH, 0, counters=False), [])

    def test_sync_deferrals_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # defer_sync joined the window section: a head must report it, a base built before it reads n/a with no
        # change shown, a supporting base that never held a frame prints a real 0, and a gate-off run carries no
        # phase counters, so it is never checked for the field.
        lacking = perf_counter_schema_tests.counters_result()
        del lacking["phases"][0]["frame_counters"]["window"]["defer_sync"]
        problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("window.defer_sync" in problem for problem in problems), problems)
        self.assertEqual(perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"window.defer_sync": 6})

        def held_cells(base):
            rows, _omitted = perf_counter_rows.counter_rows("S10/sync", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["window.defer_sync (count)"]

        self.assertEqual(held_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])),
                         ("n/a", "6 (6–6)", "n/a"))
        self.assertEqual(held_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])),
                         ("0 (0–0)", "6 (6–6)", perf_metrics.percent_change(0, 6)))
        gate_off = perf_result_tests.valid_result(frame_counters="off")
        self.assertEqual(perf_result_schema.validate_result(gate_off, perf_memory_tests.HARNESS_HASH, 0, counters=False), [])

    def test_sync_timeouts_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # sync_timeouts joined the vt section: a head must report it, a base built before it reads n/a with no
        # change shown, a supporting base whose updates all ended in time prints a real 0, and a gate-off run
        # carries no phase counters, so it is never checked for the field.
        lacking = perf_counter_schema_tests.counters_result()
        del lacking["phases"][0]["frame_counters"]["vt"]["sync_timeouts"]
        problems = perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("vt.sync_timeouts" in problem for problem in problems), problems)
        self.assertEqual(perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"vt.sync_timeouts": 2})

        def timeout_cells(base):
            rows, _omitted = perf_counter_rows.counter_rows("S10/sync", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["vt.sync_timeouts (count)"]

        self.assertEqual(timeout_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=lacking)])),
                         ("n/a", "2 (2–2)", "n/a"))
        self.assertEqual(timeout_cells(perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])),
                         ("0 (0–0)", "2 (2–2)", perf_metrics.percent_change(0, 2)))
        gate_off = perf_result_tests.valid_result(frame_counters="off")
        self.assertEqual(perf_result_schema.validate_result(gate_off, perf_memory_tests.HARNESS_HASH, 0, counters=False), [])

    def test_event_loop_probe_work_falling_to_zero_shows_against_a_base_that_probed(self):
        # The event-loop probe fields stay in the contract with real zeros on a head that moved probing to the
        # worker, so a base that probed on the event loop shows that work falling to 0 beside the worker's rows.
        base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result(
            {"app.fg_probe_calls": 7, "app.fg_probe_panes": 7,
             "app.fg_probe_us": ([0, 5, 2, 0, 0, 0, 0], 700)}))])
        head = counters_side({"app.fg_worker_probes": 7, "app.fg_worker_panes": 7})
        rows, _omitted = perf_counter_rows.counter_rows("S2/default", base, head)
        cells = {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}
        self.assertEqual(cells["app.fg_probe_calls (count)"], ("7 (7–7)", "0 (0–0)", perf_metrics.percent_change(7, 0)))
        self.assertEqual(cells["app.fg_probe_panes (count)"][:2], ("7 (7–7)", "0 (0–0)"))
        self.assertEqual(cells["app.fg_probe_us (us)"][1], "no events")
        self.assertEqual(cells["app.fg_worker_probes (count)"][:2], ("0 (0–0)", "7 (7–7)"))
        # A head that left the legacy fields out breaks the contract it still declares.
        lacking = perf_counter_schema_tests.counters_result()
        del lacking["phases"][0]["frame_counters"]["app"]["fg_probe_calls"]
        self.assertTrue(any("app.fg_probe_calls" in problem
                            for problem in perf_result_schema.validate_result(lacking, perf_memory_tests.HARNESS_HASH, 0, counters=True)))

    def test_overhead_covers_s2_and_s3_only(self):
        # Typing and the output flood are where the counters' own cost would show.
        self.assertEqual([perf_counter_rows.overhead_applies(label) for label in ("S2/default", "S2/flood", "S3/default",
                                                                     "S1/default", "S10/sync")],
                         [True, True, True, False, False])

    def test_the_document_carries_both_tables_after_the_timed_table(self):
        # Each table appears only when it has rows, with its note.
        rows = [["S2/default", "status", "1 valid run", "1 valid run", ""]]
        document = perf_report.comparison_document(rows, [], [], [], [], counter_rows=[["S2/default", "", "status", "n/a",
                                                                               "1 valid run", ""]],
                                            counters_note="Counters that were 0 in every run are left out: 49 here.",
                                            overhead_rows=rows)
        for expected in ("### Frame counters", perf_fallback_rows.COUNTERS_HEADER, "left out: 49 here.", "### Counters overhead",
                         perf_counter_rows.OVERHEAD_HEADER, "counters-on vs counters-off on the head; sequential sets, not interleaved"):
            with self.subTest(expected=expected):
                self.assertIn(expected, document)
        self.assertLess(document.index(perf_execution.TABLE_HEADER), document.index("### Frame counters"))
        plain = perf_report.comparison_document(rows, [], [], [], [])
        self.assertNotIn("Frame counters", plain)
        self.assertNotIn("Counters overhead", plain)
