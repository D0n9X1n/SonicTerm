"""Perf statistics tests."""

from __future__ import annotations
import unittest

import perf_statistics


class StatisticsTests(unittest.TestCase):
    def test_median_of_odd_and_even_counts(self):
        # The median is the middle value, or the mean of the two middle values.
        self.assertEqual(perf_statistics.median([3.0, 1.0, 2.0]), 2.0)
        self.assertEqual(perf_statistics.median([4.0, 1.0, 3.0, 2.0]), 2.5)

    def test_nearest_rank_p95_takes_an_observed_sample(self):
        # Nearest rank is ceil(0.95 * n) and never interpolates between samples.
        self.assertEqual(perf_statistics.percentile_95([float(value) for value in range(1, 21)]), 19.0)
        self.assertEqual(perf_statistics.percentile_95([float(value) for value in range(1, 11)]), 10.0)
        self.assertEqual(perf_statistics.percentile_95([float(value) for value in range(1, 101)]), 95.0)
        self.assertEqual(perf_statistics.percentile_95([7.0]), 7.0)

    def test_empty_samples_have_no_statistic(self):
        # An empty sample set reports no figure instead of inventing a zero.
        with self.assertRaises(ValueError):
            perf_statistics.median([])
        with self.assertRaises(ValueError):
            perf_statistics.percentile_95([])

    def test_frame_metrics_pool_samples_and_spread_comes_from_runs(self):
        # Pooled median and p95 cover every frame; the noise floor is the per-run spread.
        summary = perf_statistics.frame_summary([[1.0, 2.0, 3.0], [10.0, 20.0, 30.0, 40.0]])
        self.assertEqual(summary.median, 10.0)
        self.assertEqual(summary.percentile_95, 40.0)
        self.assertEqual(summary.run_median_range, (2.0, 25.0))
        self.assertEqual(summary.run_p95_range, (3.0, 40.0))
        self.assertEqual(summary.runs, 2)
        self.assertEqual(summary.samples, 7)
        # The extremes of pooled frames (1 and 40) are never reported as the spread.
        self.assertNotEqual(summary.run_median_range, (1.0, 40.0))

    def test_frame_summary_skips_runs_without_samples(self):
        # A valid run that recorded no frames for a metric adds no per-run figure.
        summary = perf_statistics.frame_summary([[], [5.0, 7.0]])
        self.assertEqual(summary.runs, 1)
        self.assertEqual(summary.run_median_range, (6.0, 6.0))
        self.assertIsNone(perf_statistics.frame_summary([[], []]))

    def test_run_level_metrics_report_median_and_range(self):
        # Run-level metrics report the median and min-max of one value per run.
        summary = perf_statistics.run_summary([5.0, None, 3.0, 4.0])
        self.assertEqual((summary.median, summary.minimum, summary.maximum), (4.0, 3.0, 5.0))
        self.assertEqual(summary.runs, 3)
        self.assertIsNone(perf_statistics.run_summary([None, None]))


class AbbaScheduleTests(unittest.TestCase):
    def drive(self, schedule, outcomes):
        """Run the schedule, taking each side's validity from its queue; return the order."""
        order = []
        while True:
            side = schedule.next_side()
            if side is None:
                return order
            order.append(side)
            schedule.record(side, outcomes[side].pop(0))

    def test_valid_runs_alternate_abba(self):
        # Base and head alternate A B B A so slow drift affects both sides equally.
        schedule = perf_statistics.AbbaSchedule(target_runs=5, retry_limit=3)
        order = self.drive(schedule, {"base": [True] * 5, "head": [True] * 5})
        self.assertEqual(order, ["base", "head", "head", "base"] * 2 + ["base", "head"])
        self.assertTrue(schedule.complete)
        self.assertIsNone(schedule.failed_side)

    def test_invalid_run_is_retried_in_a_later_slot(self):
        # An invalid run does not count; its side gets another run in its next ABBA slot.
        schedule = perf_statistics.AbbaSchedule(target_runs=2, retry_limit=3)
        order = self.drive(schedule, {"base": [False, True, True], "head": [True, True]})
        self.assertEqual(order, ["base", "head", "head", "base", "base"])
        self.assertEqual(schedule.valid_runs, {"base": 2, "head": 2})
        self.assertEqual(schedule.invalid_runs, {"base": 1, "head": 0})

    def test_a_finished_side_gives_its_slots_to_the_other(self):
        # Once one side has its valid runs, the other keeps running until it has its own.
        schedule = perf_statistics.AbbaSchedule(target_runs=1, retry_limit=3)
        order = self.drive(schedule, {"base": [True], "head": [False, True]})
        self.assertEqual(order, ["base", "head", "head"])
        self.assertTrue(schedule.complete)

    def test_retries_stop_after_three_per_side(self):
        # A fourth invalid run on one side ends the scenario and names that side.
        schedule = perf_statistics.AbbaSchedule(target_runs=1, retry_limit=3)
        order = self.drive(schedule, {"base": [False] * 4, "head": [True]})
        self.assertEqual(order, ["base", "head", "base", "base", "base"])
        self.assertEqual(schedule.failed_side, "base")
        self.assertFalse(schedule.complete)
        self.assertIsNone(schedule.next_side())

    def test_retry_limit_counts_each_side_separately(self):
        # Three retries on each side still let both sides reach their valid runs.
        schedule = perf_statistics.AbbaSchedule(target_runs=2, retry_limit=3)
        self.drive(schedule, {"base": [False, False, False, True, True],
                              "head": [False, False, False, True, True]})
        self.assertTrue(schedule.complete)
        self.assertEqual(schedule.invalid_runs, {"base": 3, "head": 3})
