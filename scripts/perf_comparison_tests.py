"""Perf comparison tests."""

from __future__ import annotations
import unittest

import perf_atlas_rows
import perf_comparison_rows
import perf_font_diagnostics
import perf_inventory
import perf_memory
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_result_schema
import perf_result_tests


def timed_outcome(dispatch, uncover=None, latency=None, footprint=None, memory=True):
    """A valid timed run whose workload phase carries the given dispatch samples.

    Its harness samples memory at checkpoints, and `memory` gives its `end` checkpoint one complete
    tagged sample.
    """
    phase = dict(perf_result_tests.valid_result()["phases"][0], dispatch_ms=dispatch)
    result = perf_result_tests.valid_result(phases=[phase], uncover_ms=uncover, latency=latency, checkpoint_memory="supported")
    samples = [perf_memory.MemorySample(60.0, 200 * 1048576, 100 * 1048576, 1, checkpoint_index=0,
                                 checkpoint_label="end", checkpoint_attempt=1,
                                 checkpoint_complete=True)] if memory else []
    footprints = {"0-end": {"bytes": footprint}} if footprint is not None else {}
    return perf_outcomes_tests.make_outcome(result=result, memory=samples, footprints=footprints)


def row_for(rows, metric):
    """Return the table row whose metric column matches, failing when it is missing."""
    matches = [row for row in rows if row[1] == metric]
    if len(matches) != 1:
        raise AssertionError(f"{metric!r} not found once in {[row[1] for row in rows]}")
    return matches[0]


class ComparisonTableTests(unittest.TestCase):
    def test_presented_frames_per_update_divides_by_each_runs_recorded_updates(self):
        # S10's stream phase records the updates its workload played. The row divides each run's presented
        # frames by that run's own count, never a constant: runs of 300 and 1,200 updates give 0.5 and 1.0.
        # A base whose harness predates the field reads n/a with no change; a phase without the field (any
        # non-S10 phase) gets no row; a malformed count is a result problem.
        def stream_outcome(presented, updates):
            phase = dict(perf_result_tests.valid_result()["phases"][0], name="stream", presented_frames=presented)
            if updates is not None:
                phase["updates"] = updates
            return perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(phases=[phase]))

        metric = "stream presented frames per update (ratio)"
        head = perf_metrics.SideRuns([stream_outcome(150, 300), stream_outcome(1200, 1200)])
        for label in ("S10/default", "S10/sync"):
            with self.subTest(label=label):
                older = perf_metrics.SideRuns([stream_outcome(300, None)])
                self.assertEqual(row_for(perf_comparison_rows.comparison_rows(label, older, head), metric)[2:],
                                 ["n/a", "0.75 (0.50–1.00)", "n/a"])
                recorded = perf_metrics.SideRuns([stream_outcome(600, 300)])
                self.assertEqual(row_for(perf_comparison_rows.comparison_rows(label, recorded, head), metric)[2:],
                                 ["2.00 (2.00–2.00)", "0.75 (0.50–1.00)", perf_metrics.percent_change(2.0, 0.75)])
        rows = perf_comparison_rows.comparison_rows("S1/default", perf_metrics.SideRuns([timed_outcome([1.0])]),
                                    perf_metrics.SideRuns([timed_outcome([1.0])]))
        self.assertFalse([row for row in rows if "per update" in row[1]], rows)
        for bad in (0, -3, 1.5, "300"):
            with self.subTest(updates=bad):
                phase = dict(perf_result_tests.valid_result()["phases"][0], name="stream", updates=bad)
                problems = perf_result_schema.validate_result(perf_result_tests.valid_result(phases=[phase]), perf_memory_tests.HARNESS_HASH, 0)
                self.assertTrue(any("updates" in problem for problem in problems), problems)

    def test_frame_rows_show_pooled_statistics_with_the_per_run_spread(self):
        # Each cell is the pooled figure followed by the min-max of per-run figures.
        base = perf_metrics.SideRuns([timed_outcome([1.0, 2.0]), timed_outcome([3.0, 4.0])])
        head = perf_metrics.SideRuns([timed_outcome([2.0, 3.0]), timed_outcome([4.0, 5.0])])
        rows = perf_comparison_rows.comparison_rows("S1/default", base, head)
        self.assertEqual(row_for(rows, "workload dispatch median (ms)"),
                         ["S1/default", "workload dispatch median (ms)", "2.50 (1.50–3.50)", "3.50 (2.50–4.50)", "+40.0%"])
        self.assertEqual(row_for(rows, "workload dispatch p95 (ms)")[2:], ["4.00 (2.00–4.00)", "5.00 (3.00–5.00)", "+25.0%"])
        self.assertEqual(row_for(rows, "workload presented frames (fps)")[2:], ["2.00 (2.00–2.00)", "2.00 (2.00–2.00)", "+0.0%"])
        self.assertEqual(row_for(rows, "workload CPU (s)")[2], "2.00 (2.00–2.00)")
        self.assertEqual(row_for(rows, "end renderer_total_bytes (MiB)")[2], "100.00 (100.00–100.00)")

    def test_a_field_missing_at_base_prints_n_a(self):
        # A metric only the head reports has no baseline and no change.
        rows = perf_comparison_rows.comparison_rows("S12/default", perf_metrics.SideRuns([timed_outcome([1.0])]),
                                    perf_metrics.SideRuns([timed_outcome([1.0], uncover=50.0)]))
        self.assertEqual(row_for(rows, "uncover (ms)")[2:], ["n/a", "50.00 (50.00–50.00)", "n/a"])

    def test_blocked_base_prints_blocked_with_its_error(self):
        # A base that cannot build or run the scenario names the error instead of numbers.
        rows = perf_comparison_rows.comparison_rows("S1/default", perf_metrics.SideRuns(blocked="error[E0599]: no method `run_action`"),
                                    perf_metrics.SideRuns([timed_outcome([1.0])]))
        self.assertEqual(rows[0][1:3], ["status", "blocked: error[E0599]: no method `run_action`"])
        self.assertEqual(row_for(rows, "workload dispatch median (ms)")[2], "blocked")
        table = perf_inventory.render_table(rows)
        self.assertTrue(table.startswith("| Scenario | Metric (unit) | Baseline | PR | Change |\n| --- |"))

    def test_footprint_row_says_how_many_runs_have_it(self):
        # A footprint failure leaves that run out, and the cell counts the runs that have it.
        base = perf_metrics.SideRuns([timed_outcome([1.0], footprint=300 * 1048576), timed_outcome([1.0])])
        head = perf_metrics.SideRuns([timed_outcome([1.0], footprint=310 * 1048576), timed_outcome([1.0], footprint=320 * 1048576)])
        row = row_for(perf_comparison_rows.comparison_rows("S1/default", base, head), "end footprint (MiB)")
        self.assertEqual(row[2], "300.00 (300.00–300.00), 1/2 runs")
        self.assertEqual(row[3], "315.00 (310.00–320.00), 2/2 runs")

    def test_memory_unavailable_in_a_short_run_is_n_a_not_a_failure(self):
        # A run whose checkpoint sample never reached the log has no memory figure for it.
        rows = perf_comparison_rows.comparison_rows("S1/default", perf_metrics.SideRuns([timed_outcome([1.0], memory=False)]),
                                    perf_metrics.SideRuns([timed_outcome([1.0])]))
        self.assertEqual(row_for(rows, "end renderer_total_bytes (MiB)")[2], "n/a")

    def test_latency_acceptance_needs_80_percent_on_both_sides_within_10_points(self):
        # Coverage below 80% on a side, or sides more than 10 points apart, leaves acceptance open.
        self.assertTrue(perf_metrics.latency_acceptance((80, 100), (90, 100)))
        self.assertFalse(perf_metrics.latency_acceptance((79, 100), (85, 100)))
        self.assertFalse(perf_metrics.latency_acceptance((80, 100), (91, 100)))
        self.assertFalse(perf_metrics.latency_acceptance(None, (90, 100)))
        latency = {"samples": [5.0, 6.0], "attributed": 2, "total": 4, "coverage": 0.5}
        rows = perf_comparison_rows.comparison_rows("S2/default", perf_metrics.SideRuns([timed_outcome([1.0], latency=latency)]),
                                    perf_metrics.SideRuns([timed_outcome([1.0], latency=latency)]))
        coverage = row_for(rows, "latency attribution coverage (%)")
        self.assertEqual(coverage[2:4], ["50.0% (2/4)", "50.0% (2/4)"])
        self.assertTrue(coverage[4].startswith("open"))
        self.assertEqual(row_for(rows, "keypress-to-present latency median (ms)")[2], "5.50 (5.50–5.50)")

    def test_latency_statistics_use_only_attributed_samples(self):
        # An unattributed sample has no latency and is never credited to any frame.
        latency = {"samples": [{"inject_unix_s": 1.0, "latency_ms": 4.0, "reason": None},
                               {"inject_unix_s": 2.0, "latency_ms": None, "reason": "revision-changed"},
                               {"inject_unix_s": 3.0, "latency_ms": 6.0, "reason": None}],
                   "attributed": 2, "total": 3, "coverage": 2 / 3}
        rows = perf_comparison_rows.comparison_rows("S2/default", perf_metrics.SideRuns([timed_outcome([1.0], latency=latency)]),
                                    perf_metrics.SideRuns([timed_outcome([1.0], latency=latency)]))
        self.assertEqual(row_for(rows, "keypress-to-present latency median (ms)")[2], "5.00 (5.00–5.00)")
        self.assertEqual(row_for(rows, "latency attribution coverage (%)")[2], "66.7% (2/3)")

    def test_cells_escape_table_pipes(self):
        # An error text holding `|` cannot break the Markdown table.
        table = perf_inventory.render_table([["S1", "status", "blocked: a | b", "n/a", "n/a"]])
        self.assertIn("blocked: a \\| b", table)

    def test_laps_table_pools_each_lap_separately(self):
        # Laps runs have their own table: each window and lap, pooled with its per-run spread.
        def laps_run(present):
            samples = [perf_font_diagnostics.RenderTimingSample(1.0, "main", {"total": 4.0, "present": value}) for value in present]
            return perf_outcomes_tests.make_outcome(laps=samples)
        rows = perf_atlas_rows.laps_rows("S1/default", perf_metrics.SideRuns([laps_run([1.0, 3.0])]), perf_metrics.SideRuns([laps_run([2.0, 4.0])]))
        self.assertEqual(row_for(rows, "main present median (ms)")[2:], ["2.00 (2.00–2.00)", "3.00 (3.00–3.00)", "+50.0%"])
        self.assertEqual(row_for(rows, "main total p95 (ms)")[2], "4.00 (4.00–4.00)")
