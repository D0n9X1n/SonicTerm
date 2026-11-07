"""Pins S13's frozen verdict rule: its thresholds, the no-verdict gaps, and the counterexample in which
budget-expired misses alone break `unchanged`."""

from __future__ import annotations

import unittest

import perf_contention_verdict as verdict_rule


def run(samples, collections=1_000, parser=0, images=0, budget=0, flood=(8 << 20, 8 << 20)):
    """One counters result: `samples` latencies, its typing window's misses and per-phase flood bytes."""
    counts = [0] * 16
    counts[0] = collections
    window = {"contention_parser": parser, "contention_images": images, "contention_budget_expired": budget,
              "parser_lock_wait_us": {"unit": "us", "bounds": [], "counts": counts, "sum_us": 0}}
    phases = [{"name": "typing", "frame_counters": {"window": window, "vt": {"parse_bytes": flood[0]}}},
              {"name": "tail", "frame_counters": {"window": {}, "vt": {"parse_bytes": flood[1]}}}]
    return {"latency": {"samples": [{"latency_ms": value} for value in samples]}, "phases": phases}


def evidence(*results):
    return verdict_rule.variant_evidence(results)


class ContentionVerdictTests(unittest.TestCase):
    def test_the_thresholds_are_frozen(self):
        # Changing any of them changes what an S13 run proves, so they are pinned here.
        self.assertEqual((verdict_rule.MIN_SAMPLES, verdict_rule.P95_TOLERANCE, verdict_rule.FAILURE_LIMIT),
                         (200, 0.10, 0.01))
        self.assertEqual(verdict_rule.FAILURE_FIELDS,
                         ("contention_parser", "contention_images", "contention_budget_expired"))

    def test_a_close_p95_with_few_failures_is_unchanged(self):
        two = evidence(run([10.0] * 300))
        four = evidence(run([10.5] * 300, parser=3, images=2, budget=4))
        self.assertEqual(verdict_rule.verdict(two, four)[0], verdict_rule.UNCHANGED)

    def test_budget_expired_misses_alone_are_not_unchanged(self):
        # The counterexample fixed with the rule: 100 budget-expired failures in 1,000 collections, no parser
        # or image miss and an acceptable p95 must not read as unchanged.
        two = evidence(run([10.0] * 300))
        four = evidence(run([10.0] * 300, collections=1_000, budget=100))
        outcome, why = verdict_rule.verdict(two, four)
        self.assertEqual(outcome, verdict_rule.NOT_UNCHANGED)
        self.assertIn("100 of 1000", why)

    def test_a_p95_beyond_ten_percent_is_not_unchanged(self):
        two = evidence(run([10.0] * 300))
        self.assertEqual(verdict_rule.verdict(two, evidence(run([11.0] * 300)))[0], verdict_rule.UNCHANGED)
        self.assertEqual(verdict_rule.verdict(two, evidence(run([11.2] * 300)))[0], verdict_rule.NOT_UNCHANGED)

    def test_the_failure_fraction_counts_every_kind_against_collections(self):
        two = evidence(run([10.0] * 300))
        # 4 + 3 + 3 = 10 of 1,000 is exactly 1%: not under the limit.
        at_limit = evidence(run([10.0] * 300, parser=4, images=3, budget=3))
        self.assertEqual(verdict_rule.verdict(two, at_limit)[0], verdict_rule.NOT_UNCHANGED)
        below = evidence(run([10.0] * 300, parser=4, images=3, budget=2))
        self.assertEqual(verdict_rule.verdict(two, below)[0], verdict_rule.UNCHANGED)

    def test_samples_pool_across_runs_and_too_few_give_no_verdict(self):
        two = evidence(run([10.0] * 150), run([10.0] * 150))
        self.assertEqual(len(two["samples"]), 300)
        thin = evidence(run([10.0] * 199))
        outcome, why = verdict_rule.verdict(two, thin)
        self.assertEqual(outcome, verdict_rule.NO_VERDICT)
        self.assertIn("199 samples", why)

    def test_missing_attribution_or_a_stopped_flood_gives_no_verdict(self):
        two = evidence(run([10.0] * 300))
        unattributed = run([10.0] * 300)
        del unattributed["phases"][0]["frame_counters"]["window"]["contention_budget_expired"]
        self.assertEqual(verdict_rule.verdict(two, evidence(unattributed))[0], verdict_rule.NO_VERDICT)
        stopped = evidence(run([10.0] * 300, flood=(8 << 20, 1_000)))
        outcome, why = verdict_rule.verdict(two, stopped)
        self.assertEqual(outcome, verdict_rule.NO_VERDICT)
        self.assertIn("flood stopped", why)
        self.assertEqual(verdict_rule.verdict(two, evidence())[0], verdict_rule.NO_VERDICT)

    def test_rows_appear_only_when_s13_ran(self):
        self.assertEqual(verdict_rule.verdict_rows({}), [])
        rows = verdict_rule.verdict_rows({verdict_rule.TWO_PANES: [run([10.0] * 300)],
                                          verdict_rule.FOUR_PANES: [run([10.0] * 300, budget=1)]})
        self.assertEqual([row[0] for row in rows], [verdict_rule.TWO_PANES, verdict_rule.FOUR_PANES, "Verdict"])
        self.assertEqual(rows[1][4], "0/0/1")
        self.assertEqual(rows[2][1], verdict_rule.UNCHANGED)
        self.assertTrue(all(len(row) == len(verdict_rule.COLUMNS) for row in rows))


if __name__ == "__main__":
    unittest.main(verbosity=2)
