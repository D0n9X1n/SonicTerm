"""Pins S13's frozen verdict rule: its thresholds, the no-verdict gaps, and the counterexample in which
budget-expired misses alone break `unchanged`."""

from __future__ import annotations

import unittest

import perf_contention_verdict as verdict_rule


def run(samples, collections=1_000, parser=0, images=0, budget=0, flood=(8 << 20, 8 << 20), gap_ms=100,
        grids=((120, 10), (120, 10)), watched=("typing", "tail")):
    """One counters result: `samples` latencies, its typing window's misses, per-phase flood bytes, each
    watched phase's longest unprogressed stretch, and the flooding and typing panes' grids."""
    counts = [0] * 16
    counts[0] = collections
    window = {"contention_parser": parser, "contention_images": images, "contention_budget_expired": budget,
              "parser_lock_wait_us": {"unit": "us", "bounds": [], "counts": counts, "sum_us": 0}}
    phases = [{"name": "typing", "frame_counters": {"window": window, "vt": {"parse_bytes": flood[0]}}},
              {"name": "tail", "frame_counters": {"window": {}, "vt": {"parse_bytes": flood[1]}}}]
    flood_phases = [{"name": name, "duration_ms": 30_000, "looks": 300, "busy": 0, "longest_gap_ms": gap_ms}
                    for name in watched]
    contention = {"role_grids": [list(grid) for grid in grids], "max_flood_gap_ms": 1_000,
                  "flood": {"phases": flood_phases}}
    return {"latency": {"samples": [{"latency_ms": value} for value in samples]}, "phases": phases,
            "contention": contention}


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

    def test_a_flood_without_progress_for_over_a_second_gives_no_verdict(self):
        # The harness accounts each phase from its start to its end, so a stall, an unobserved prefix
        # or suffix, or busy-only looks all arrive here as one long gap, and none supports a verdict.
        two = evidence(run([10.0] * 300))
        stalled = evidence(run([10.0] * 300, gap_ms=4_000))
        outcome, why = verdict_rule.verdict(two, stalled)
        self.assertEqual(outcome, verdict_rule.NO_VERDICT)
        self.assertIn("no progress for 4000 ms", why)
        unrecorded = run([10.0] * 300)
        del unrecorded["contention"]
        self.assertEqual(verdict_rule.verdict(two, evidence(unrecorded))[0], verdict_rule.NO_VERDICT)

    def test_each_measured_phase_must_be_watched(self):
        # A run whose monitor never covered `tail` cannot show the flood was alive there.
        two = evidence(run([10.0] * 300))
        half = evidence(run([10.0] * 300, watched=("typing",)))
        outcome, why = verdict_rule.verdict(two, half)
        self.assertEqual(outcome, verdict_rule.NO_VERDICT)
        self.assertIn("did not watch the flood through tail", why)

    def test_pane_grids_must_match_up_to_split_rounding(self):
        # Pane count must be the only difference between the variants. One cell is a split's rounding
        # (a macOS runner gave stacked panes 8 and 7 rows in a 17-row window); any more leaves no verdict.
        two = evidence(run([10.0] * 300, grids=((125, 8), (125, 7))))
        rounded = evidence(run([10.0] * 300, grids=((125, 8), (125, 8))))
        self.assertEqual(verdict_rule.verdict(two, rounded)[0], verdict_rule.UNCHANGED)
        two = evidence(run([10.0] * 300, grids=((120, 10), (120, 10))))
        for grids in (((120, 20), (120, 20)), ((110, 10), (110, 10)), ((120, 12), (120, 12))):
            other = evidence(run([10.0] * 300, grids=grids))
            outcome, why = verdict_rule.verdict(two, other)
            self.assertEqual(outcome, verdict_rule.NO_VERDICT, grids)
            self.assertIn("pane grids differ", why)
        self.assertEqual(verdict_rule.verdict(two, evidence(run([10.0] * 300)))[0], verdict_rule.UNCHANGED)

    def test_rows_appear_only_when_s13_ran(self):
        self.assertEqual(verdict_rule.verdict_rows({}), [])
        rows = verdict_rule.verdict_rows({verdict_rule.TWO_PANES: [run([10.0] * 300)],
                                          verdict_rule.FOUR_PANES: [run([10.0] * 300, budget=1)]})
        self.assertEqual([row[0] for row in rows], [verdict_rule.TWO_PANES, verdict_rule.FOUR_PANES, "Verdict"])
        self.assertEqual(rows[1][4], "0/0/1")
        self.assertEqual(rows[2][1], verdict_rule.UNCHANGED)
        self.assertTrue(all(len(row) == len(verdict_rule.COLUMNS) for row in rows))


class WideScalingTests(unittest.TestCase):
    """The release-only scaling rule over S13's wide layouts."""

    def wide(self, p95s=(10.0, 10.5, 11.0), failed=(0, 0, 0), grid=(30, 5), collections=(1_000,) * 3):
        return {label: [run([value] * 300, parser=miss, grids=(grid,) * panes, collections=count)]
                for label, value, miss, panes, count
                in zip(verdict_rule.WIDE, p95s, failed, verdict_rule.WIDE_PANES, collections)}

    def test_a_layout_without_collections_gives_no_verdict(self):
        # Equal grids and enough samples, but one layout recorded no collection: no verdict, named,
        # for whichever layout it is, rather than a division by zero.
        for empty, label in enumerate(verdict_rule.WIDE):
            counts = tuple(0 if index == empty else 1_000 for index in range(3))
            row = self.scaling(self.wide(collections=counts))
            self.assertEqual(row[1], verdict_rule.NO_VERDICT, label)
            self.assertIn(f"{label}: no collection was recorded", row[2])

    def scaling(self, heads):
        rows = verdict_rule.verdict_rows(heads)
        return next(row for row in rows if row[0] == "Scaling")

    def test_steady_layouts_scale(self):
        # Within 10% per doubling and under 1% failed collections: the rule says it scales.
        self.assertEqual(self.scaling(self.wide())[1], verdict_rule.SCALES)

    def test_a_p95_jump_or_failed_collections_do_not_scale(self):
        # A 20% jump from 16 to 32 panes, or 1% failed collections in one layout, breaks the rule.
        self.assertEqual(self.scaling(self.wide(p95s=(10.0, 10.5, 12.6)))[1], verdict_rule.DOES_NOT_SCALE)
        self.assertEqual(self.scaling(self.wide(failed=(0, 10, 0)))[1], verdict_rule.DOES_NOT_SCALE)

    def test_unequal_workloads_or_a_missing_layout_give_no_verdict(self):
        # A host's own equal size for every layout (31x3 on one macOS host) is fine.
        self.assertEqual(self.scaling(self.wide(grid=(31, 3)))[1], verdict_rule.SCALES)
        # Shrinking panes as the count doubles is a different workload per pane, not scaling.
        heads = self.wide()
        heads["S13/wide-8"] = [run([10.0] * 300, grids=((120, 20),) * 8)]
        heads["S13/wide-16"] = [run([10.0] * 300, grids=((60, 10),) * 16)]
        self.assertEqual(self.scaling(heads)[1], verdict_rule.NO_VERDICT)
        # A layout that recorded fewer roles than its plan is not that layout.
        heads = self.wide()
        heads["S13/wide-32"] = [run([10.0] * 300, grids=((30, 5),) * 2)]
        self.assertEqual(self.scaling(heads)[1], verdict_rule.NO_VERDICT)
        # Panes unequal within a layout, or a missing layout, prove nothing.
        heads = self.wide()
        heads["S13/wide-16"] = [run([10.0] * 300, grids=((30, 5),) * 15 + ((15, 5),))]
        self.assertEqual(self.scaling(heads)[1], verdict_rule.NO_VERDICT)
        heads = self.wide()
        del heads["S13/wide-32"]
        self.assertEqual(self.scaling(heads)[1], verdict_rule.NO_VERDICT)

    def test_a_pr_run_without_wide_layouts_adds_no_scaling_rows(self):
        # The PR pipeline never runs them, so its S13 section is unchanged.
        heads = {verdict_rule.TWO_PANES: [run([10.0] * 300)], verdict_rule.FOUR_PANES: [run([10.0] * 300)]}
        self.assertFalse(any(row[0] == "Scaling" for row in verdict_rule.verdict_rows(heads)))

if __name__ == "__main__":
    unittest.main(verbosity=2)
