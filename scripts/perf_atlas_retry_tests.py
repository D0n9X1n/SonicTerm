"""Perf atlas retry tests."""

from __future__ import annotations
from pathlib import Path
import unittest

import perf_attribution_report
import perf_counter_schema_tests
import perf_driver_tests
import perf_latency_tests
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_report
import perf_result_schema
import perf_result_tests
import perf_sets
import perf_trim_tests


def recovery_records(changes=None):
    """S1/atlas-retry's 32 well-formed records: 8 episodes of A (reset, not presented) and B-D (presented).

    `changes` maps a record index to the fields that replace that record's values."""
    changes = changes or {}
    records = []
    for index in range(32):
        frame = "ABCD"[index % 4]
        record = {"episode": index // 4, "frame": frame, "attempts": 1, "presented": 0 if frame == "A" else 1,
                  "resets": 1 if frame == "A" else 0, "hits": 0 if frame in "AB" else 70,
                  "misses": 70 if frame == "B" else 0, "shapes": 70 if frame == "B" else 0, "atlas_dim": 1024}
        record.update(changes.get(index, {}))
        records.append(record)
    return records


def recovery_result(recovery=perf_counter_schema_tests.MISSING, **overrides):
    """A valid S1/atlas-retry counters result carrying `recovery` (the well-formed block unless given)."""
    result = perf_counter_schema_tests.counters_result(**overrides)
    result.setdefault("scenario", "S1")
    result.setdefault("variant", "atlas-retry")
    if recovery is perf_counter_schema_tests.MISSING:
        recovery = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
    if recovery is not None:
        result["atlas_recovery"] = recovery
    return result


class AtlasRetryVariantTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    def test_only_the_counters_set_runs_a_counters_only_variant(self):
        # S1/atlas-retry measures its injected episodes through counters, so the timed, laps and alloc sets are
        # dropped for it; every other variant keeps every set it was given.
        sets = [("timed", "a", False, False, 5), ("laps", "a", True, False, 2),
                ("counters", "a", False, True, 2), ("alloc", "b", False, False, 5)]
        self.assertEqual(perf_result_schema.variant_sets("S1", "default", sets), sets)
        self.assertEqual(perf_result_schema.variant_sets("S3", "atlas-retry", sets), sets)
        self.assertEqual(perf_result_schema.variant_sets("S1", "atlas-retry", sets), [sets[2]])

    def test_a_counters_only_variant_is_refused_when_no_counters_set_runs(self):
        # Selecting the variant without a counters set would measure nothing, so the selection is refused,
        # naming the variant; ordinary selections are never refused.
        self.assertIsNone(perf_result_schema.counters_only_problem([("S1", "default")], False))
        self.assertIsNone(perf_result_schema.counters_only_problem([("S1", "atlas-retry")], True))
        problem = perf_result_schema.counters_only_problem([("S1", "default"), ("S1", "atlas-retry")], False)
        self.assertIn("S1/atlas-retry", problem)
        self.assertIn("--counters", problem)

    def test_the_comparison_refuses_the_variant_without_counters(self):
        # The refusal reaches the driver before any run: nothing is planned, and compare_main's ValueError
        # handling turns it into a failed comparison.
        with self.assertRaisesRegex(ValueError, "S1/atlas-retry runs only in the counters set"):
            self.compare(listing=perf_trim_tests.ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",), head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST)

    def test_the_comparison_refuses_the_variant_when_the_head_has_no_counters(self):
        # --counters with a head that does not declare perf-counters builds no counters set, so it is refused too.
        with self.assertRaisesRegex(ValueError, "S1/atlas-retry runs only in the counters set"):
            self.compare(listing=perf_trim_tests.ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",), options=("--counters",))

    def test_a_counters_only_comparison_runs_counter_plans_and_passes_strictly(self):
        # Selected with --counters on both sides, the variant runs counters plans only and its complete
        # counter-only results leave no strict problem, so --require-base passes.
        code, _gate, _calls, plans, _work, out = self.compare(
            listing=perf_trim_tests.ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",),
            options=("--counters", "--counters-runs", "2", "--require-base"),
            head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST, base_manifest=perf_counter_schema_tests.BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, 0, self.printed)
        self.assertTrue(plans)
        self.assertTrue(all(plan.counters for plan in plans))
        self.assertEqual(sorted(plan.side for plan in plans), ["base", "base", "head", "head"])
        self.assertTrue((out / "comparison.md").is_file())
        results = [perf_sets.SetResult("S1/atlas-retry", "counters",
                                  perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=recovery_result())] * 2),
                                  perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=recovery_result())] * 2), target_runs=2)]
        self.assertEqual(perf_sets.strict_problems(results), [])

    def test_a_well_formed_recovery_block_validates(self):
        # The harness's block for a valid counters run passes; a timed-out run may omit it.
        self.assertIsNone(perf_result_schema.atlas_recovery_problem(recovery_result()))
        self.assertIsNone(perf_result_schema.atlas_recovery_problem(recovery_result(None, status="timeout")))
        self.assertIsNone(perf_result_schema.atlas_recovery_problem(perf_counter_schema_tests.counters_result(scenario="S1", variant="default")))

    def test_each_malformed_recovery_block_is_refused(self):
        # Every way the block can misdescribe the episodes is a schema problem that keeps the run out of the
        # comparison, and validate_result reports it.
        good = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
        cases = {
            "missing on a valid run": recovery_result(None),
            "seven episodes": recovery_result(dict(good, episodes=7)),
            "distinct keys not an integer": recovery_result(dict(good, distinct_keys="70")),
            "distinct keys a boolean": recovery_result(dict(good, distinct_keys=True)),
            "records not a list": recovery_result(dict(good, records={})),
            "31 records": recovery_result(dict(good, records=recovery_records()[:31])),
            "frames out of order": recovery_result(dict(good, records=[
                recovery_records()[index] for index in (1, 0, *range(2, 32))])),
            "wrong episode": recovery_result(dict(good, records=recovery_records({4: {"episode": 0}}))),
            "a field missing": recovery_result(dict(good, records=[
                {key: value for key, value in record.items() if key != "shapes"} if index == 6 else record
                for index, record in enumerate(recovery_records())])),
            "two attempts": recovery_result(dict(good, records=recovery_records({2: {"attempts": 2}}))),
            "A presented": recovery_result(dict(good, records=recovery_records({0: {"presented": 1}}))),
            "A without a reset": recovery_result(dict(good, records=recovery_records({8: {"resets": 0}}))),
            "C reset": recovery_result(dict(good, records=recovery_records({2: {"resets": 1}}))),
            "D not presented": recovery_result(dict(good, records=recovery_records({3: {"presented": 0}}))),
            "B-D at two dimensions": recovery_result(dict(good, records=recovery_records({31: {"atlas_dim": 2048}}))),
            "on another variant": recovery_result(scenario="S1", variant="default"),
            "on a run without counters": dict(recovery_result(), frame_counters="off"),
            "a valid run without counters": dict(recovery_result(None), frame_counters="off"),
        }
        for name, data in cases.items():
            with self.subTest(case=name):
                self.assertIsNotNone(perf_result_schema.atlas_recovery_problem(data))

    def test_impossible_numbers_in_the_recovery_block_are_refused(self):
        # Equal-comparing floats and booleans are not the harness's integers, counters cannot be negative,
        # and a scene with no row keys or an atlas with no size measured nothing; each refuses the block.
        good = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
        every_dim_zero = recovery_records({index: {"atlas_dim": 0} for index in range(32)})
        cases = {
            "episodes as a float": dict(good, episodes=8.0),
            "no distinct keys": dict(good, distinct_keys=0),
            "negative distinct keys": dict(good, distinct_keys=-1),
            "a boolean episode index": dict(good, records=recovery_records({4: {"episode": True}})),
            "a float episode index": dict(good, records=recovery_records({0: {"episode": 0.0}})),
            "negative misses": dict(good, records=recovery_records({1: {"misses": -1}})),
            "negative hits": dict(good, records=recovery_records({2: {"hits": -5}})),
            "negative shapes": dict(good, records=recovery_records({5: {"shapes": -1}})),
            "a zero atlas dimension throughout": dict(good, records=every_dim_zero),
        }
        for name, recovery in cases.items():
            with self.subTest(case=name):
                self.assertIsNotNone(perf_result_schema.atlas_recovery_problem(recovery_result(recovery)))
        self.assertIsNone(perf_result_schema.atlas_recovery_problem(recovery_result(good)))

    def test_validate_result_reports_a_recovery_problem(self):
        # The recovery check is part of the schema check, so a bad block refuses the run.
        broken = recovery_result({"episodes": 8, "distinct_keys": 70, "records": []})
        problems = perf_result_schema.validate_result(broken, perf_memory_tests.HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("atlas_recovery" in problem for problem in problems), problems)
        self.assertFalse(any("atlas_recovery" in problem
                             for problem in perf_result_schema.validate_result(recovery_result(), perf_memory_tests.HARNESS_HASH, 0, counters=True)))

    def test_the_recovery_table_sums_each_frame_over_accepted_runs(self):
        # Each frame's cell sums misses, hits, shapes and attempts over every episode of every valid run; the
        # first row counts runs, episodes and distinct keys, and the last totals A-D.
        reused = recovery_records({index: {"misses": 0, "hits": 70, "shapes": 0} for index in range(1, 32, 4)})
        base = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=recovery_result())] * 2)
        head = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=recovery_result({"episodes": 8, "distinct_keys": 70,
                                                                   "records": reused}))] * 2)
        rows = perf_attribution_report.atlas_recovery_rows("S1/atlas-retry", base, head)
        self.assertEqual(rows[0], ["runs", "2 runs, 16 episodes, distinct keys 70",
                                   "2 runs, 16 episodes, distinct keys 70"])
        self.assertEqual([row[0] for row in rows], ["runs", "A", "B", "C", "D", "A-D total"])
        self.assertEqual(rows[2], ["B", "misses 1120, hits 0, shapes 1120, attempts 16",
                                   "misses 0, hits 1120, shapes 0, attempts 16"])
        self.assertEqual(rows[5][1], "misses 1120, hits 2240, shapes 1120, attempts 64")
        document = perf_report.comparison_document([], [], [], [], [], recovery_rows=rows)
        self.assertIn("### Atlas retry recovery", document)
        self.assertIn("| B | misses 1120, hits 0, shapes 1120, attempts 16 |", document)

    def test_no_recovery_table_without_recovery_runs(self):
        # Another variant, or a side with no accepted recovery runs on either side, adds no table, and the
        # document has no recovery section.
        runs = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=recovery_result())])
        self.assertEqual(perf_attribution_report.atlas_recovery_rows("S1/default", runs, runs), [])
        empty = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result())])
        self.assertEqual(perf_attribution_report.atlas_recovery_rows("S1/atlas-retry", empty, perf_metrics.SideRuns(blocked="exit 5")), [])
        head_only = perf_attribution_report.atlas_recovery_rows("S1/atlas-retry", perf_metrics.SideRuns(blocked="exit 5"), runs)
        self.assertEqual(head_only[1][1], "n/a")
        self.assertNotIn("Atlas retry recovery", perf_report.comparison_document([], [], [], [], []))



class PhaseDurationTests(unittest.TestCase):
    def test_a_phase_kinds_harness_rejects_impossible_durations(self):
        # Under the capability every duration is finite and at least zero and a phase never ends before it starts;
        # each broken alone is a schema failure naming the field. A legacy harness keeps its old, looser checks.
        cases = {
            "dispatch_ms": {"dispatch_ms": [1.0, float("inf")]},
            "present_interval_ms": {"present_interval_ms": [16.6, -1.0]},
            "cpu_user_s": {"cpu_user_s": float("nan")},
            "cpu_system_s": {"cpu_system_s": -10.0},
            "ends before it starts": {"start_unix_s": 70.0, "end_unix_s": 10.0},
        }
        self.assertEqual(perf_result_schema.validate_result(perf_result_tests.valid_result(phases=[perf_latency_tests.kind_phase()]), perf_memory_tests.HARNESS_HASH, 0, phase_kinds=1),
                         [])
        for expected, fields in cases.items():
            with self.subTest(case=expected):
                capable = perf_result_schema.validate_result(perf_result_tests.valid_result(phases=[perf_latency_tests.kind_phase(**fields)]), perf_memory_tests.HARNESS_HASH, 0,
                                               phase_kinds=1)
                self.assertTrue(any(expected in problem for problem in capable), capable)
                legacy_phase = dict(perf_result_tests.valid_result()["phases"][0], **fields)
                legacy = perf_result_schema.validate_result(perf_result_tests.valid_result(phases=[legacy_phase]), perf_memory_tests.HARNESS_HASH, 0)
                self.assertFalse(any(expected in problem and "phase-kinds" in problem for problem in legacy), legacy)


class CountersOnlyKindTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    def counters_only(self, listing, selected, phases, **overrides):
        """comparison.md of a counters-only comparison whose every run carries `phases` with the gate on."""
        def counters_run(plan):
            result = perf_counter_schema_tests.counters_result(phases=[dict(phase) for phase in phases], **overrides)
            return perf_outcomes_tests.make_outcome(plan=plan, result=result)
        code, _gate, _calls, _plans, _work, out = self.compare(
            listing=listing, scenarios=(selected,), options=("--counters", "--counters-runs", "2"),
            head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST, base_manifest=perf_counter_schema_tests.BASE_COUNTERS_MANIFEST, counters_run=counters_run)
        self.assertEqual(code, 0, self.printed)
        return (out / "comparison.md").read_text(encoding="utf-8")

    def test_atlas_retry_reports_recovery_activity_beside_its_recovery_table(self):
        # S1/atlas-retry's recovery hold reports its elapsed hold and CPU from the counters runs, and the recovery
        # table stays.
        recovery = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
        listing = {**perf_trim_tests.ATLAS_RETRY_LIST, "capabilities": {"latency_split_schema": 1, "phase_kinds": 1}}
        document = self.counters_only(listing, "S1/atlas-retry", [perf_latency_tests.kind_phase("hold", name="recovery")],
                                      scenario="S1", variant="atlas-retry", atlas_recovery=recovery)
        section = document.partition("### Counters-only workloads")[2].partition("\n### ")[0]
        self.assertIn("recovery hold (s)", section)
        self.assertIn("recovery CPU (s)", section)
        self.assertNotIn("recovery presented frames (fps)", section)
        self.assertIn("### Atlas retry recovery", document)

    def test_an_older_harness_counters_only_variant_goes_to_the_unclassified_block(self):
        # Counters-only runs of a harness without phase kinds are kept as recorded in the unclassified block.
        recovery = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
        document = self.counters_only(perf_trim_tests.ATLAS_RETRY_LIST, "S1/atlas-retry", [perf_result_tests.valid_result()["phases"][0]],
                                      scenario="S1", variant="atlas-retry", atlas_recovery=recovery)
        unclassified = document.partition("### Unclassified (older harness)")[2]
        self.assertIn("workload presented frames (fps)", unclassified)
        self.assertNotIn("### Counters-only workloads", document)


class EntryPointTests(unittest.TestCase):
    def test_each_perf_test_file_ends_with_its_entry_point(self):
        # The gate runs these files directly, so unittest.main() must come after every test class: a class
        # defined below it never exists when the run starts.
        import ast
        for name in ("perf-compare_tests.py", "perf-flags_tests.py", "perf-critical-path_tests.py"):
            with self.subTest(file=name):
                tree = ast.parse(Path(__file__).with_name(name).read_text(encoding="utf-8"))
                guards = [index for index, node in enumerate(tree.body) if isinstance(node, ast.If)
                          and "__main__" in ast.unparse(node.test)]
                self.assertEqual(guards, [len(tree.body) - 1], f"{name}: the entry point is not last")
