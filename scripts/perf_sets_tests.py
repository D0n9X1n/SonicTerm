"""Perf sets tests."""

from __future__ import annotations
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

import perf_comparison_rows
import perf_foreground_tests
import perf_inventory
import perf_memory_tests
import perf_metrics
import perf_outcomes
import perf_outcomes_tests
import perf_result_tests
import perf_sets
import perf_settings
import perf_smoke_tests
import perf_watcher_tests


class RunSetTests(unittest.TestCase):
    def run_set(self, answers, runs=2, base_blocked=None, variant="default"):
        """Drive one run set; each side answers from its queue of outcome kinds or factories, repeating the last."""
        calls = []
        plans = {side: perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, variant, side, Path(f"/{side}"), perf_memory_tests.HARNESS_HASH) for side in perf_settings.SIDES}

        def run_case(plan, evidence):
            calls.append(plan.side)
            queue = answers[plan.side]
            kind = queue.pop(0) if len(queue) > 1 else queue[0]
            if callable(kind):
                return kind(plan)
            if kind == "grid":
                return perf_outcomes_tests.make_outcome(plan=plan, result=perf_result_tests.valid_result(grid={"cols": 200, "rows": 50}))
            return perf_smoke_tests.outcome_of(kind)(plan)
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            result = perf_sets.run_set("S1/default", plans, base_blocked, runs, run_case, Path("/e"))
        # What the comparison printed, for tests that check its run lines.
        self.printed = printed.getvalue()
        return result, calls

    def test_a_run_line_names_a_suspected_occlusion(self):
        # A retried run's line names the harness's reason first, so the log says why the run was retried.
        self.run_set({"base": ["startup", "valid"], "head": ["valid"]}, runs=1)
        self.assertIn(f"[perf-compare] S1/default timed base run 1: occluded: {perf_watcher_tests.STARTUP_OCCLUSION_REASON}", self.printed)

    def test_an_interrupted_run_stops_the_comparison(self):
        # Someone who pressed Ctrl-C wants the comparison to stop, and run_step never counted the harness's group.
        with self.assertRaises(perf_sets.StopComparison) as raised:
            self.run_set({"base": ["interrupted", "valid"], "head": ["valid"]})
        self.assertIn(perf_watcher_tests.UNCOUNTED_STOP, str(raised.exception))

    def test_valid_runs_alternate_and_fill_both_sides(self):
        # Runs alternate A B B A until each side has its valid runs.
        result, calls = self.run_set({"base": ["valid"], "head": ["valid"]})
        self.assertEqual(calls, ["base", "head", "head", "base"])
        self.assertEqual((len(result.base.outcomes), len(result.head.outcomes)), (2, 2))
        self.assertEqual(perf_sets.comparison_exit([result]), 0)

    def test_a_grid_that_differs_from_the_pair_is_invalid_and_retried(self):
        # Base and head must measure the same grid, or the pair compares different screens.
        result, calls = self.run_set({"base": ["valid"], "head": ["grid", "valid"]}, runs=1)
        self.assertEqual(calls, ["base", "head", "head"])
        self.assertEqual(result.attempts[1][2], "grid")
        self.assertEqual(len(result.head.outcomes), 1)

    def test_each_attempt_persists_its_final_classification(self):
        # outcome.json is written before the set's grid check, so a run the set rejects after a valid outcome
        # still reads valid there. The set writes each attempt's final kind and reasons beside it, which perf-flags
        # binds a set's accepted attempts to; an attempt without a directory gets none.
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        plans = {side: perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", side, Path(f"/{side}"), perf_memory_tests.HARNESS_HASH)
                 for side in perf_settings.SIDES}
        answers = {"base": ["valid"], "head": ["grid", "valid"]}

        def run_case(plan, evidence):
            evidence.mkdir(parents=True)
            queue = answers[plan.side]
            kind = queue.pop(0) if len(queue) > 1 else queue[0]
            if kind == "grid":
                return perf_outcomes_tests.make_outcome(plan=plan, result=perf_result_tests.valid_result(grid={"cols": 200, "rows": 50}))
            return perf_smoke_tests.outcome_of(kind)(plan)
        with contextlib.redirect_stdout(io.StringIO()):
            result = perf_sets.run_set("S1/default", plans, None, 1, run_case, root)
        paths = {Path(evidence).name: Path(evidence) / perf_inventory.CLASSIFICATION_FILE
                 for _side, evidence, _kind, _why in result.attempts}
        self.assertTrue(all(path.is_file() for path in paths.values()), "every attempt's classification is written")
        written = {name: json.loads(path.read_text(encoding="utf-8")) for name, path in paths.items()}
        self.assertEqual(written["01-base"], {"side": "base", "kind": "valid", "reasons": []})
        self.assertEqual((written["02-head"]["kind"], written["02-head"]["side"]), ("grid", "head"))
        self.assertEqual(written["02-head"]["reasons"], list(result.attempts[1][3]))
        self.assertEqual(written["03-head"]["kind"], "valid")
        with contextlib.redirect_stdout(io.StringIO()):
            bare = perf_sets.run_set("S1/default", plans, None, 1, lambda plan, _evidence: perf_smoke_tests.outcome_of("valid")(plan),
                                root / "absent")
        self.assertFalse(any((Path(evidence) / perf_inventory.CLASSIFICATION_FILE).exists()
                             for _side, evidence, _kind, _why in bare.attempts))

    def test_host_platform_names_macos_and_windows_and_keeps_other_hosts(self):
        # perf-flags and run-identity.json key evidence by these names, so the two mapped hosts and the
        # pass-through for any other host are pinned.
        self.assertEqual(perf_inventory.host_platform("darwin"), "macos")
        self.assertEqual(perf_inventory.host_platform("win32"), "windows")
        self.assertEqual(perf_inventory.host_platform("linux"), "linux")

    def test_base_that_cannot_build_is_blocked_and_the_head_still_runs(self):
        # The base's build error is printed for the scenario; the head keeps its measured runs.
        result, calls = self.run_set({"base": ["valid"], "head": ["valid"]}, base_blocked="error[E0599]")
        self.assertEqual(calls, ["head", "head"])
        self.assertEqual(result.base.blocked, "error[E0599]")
        rows = perf_comparison_rows.comparison_rows("S1/default", result.base, result.head)
        self.assertEqual(rows[0][2], "blocked: error[E0599]")
        self.assertEqual(perf_sets.comparison_exit([result]), 0)

    def test_a_scenario_the_base_harness_cannot_run_retires_the_base(self):
        # Exit 5 at base blocks that side only.
        result, calls = self.run_set({"base": ["blocked"], "head": ["valid"]})
        self.assertEqual(calls, ["base", "head", "head"])
        self.assertTrue(result.base.blocked)
        self.assertEqual(len(result.head.outcomes), 2)

    def test_head_that_exhausts_its_retries_fails_the_comparison(self):
        # A fourth invalid head run fails the scenario, and the table says so.
        result, calls = self.run_set({"base": ["valid"], "head": ["theft"]}, runs=1)
        self.assertEqual(calls.count("head"), perf_settings.RETRY_LIMIT + 1)
        self.assertIn("focus", result.head.failed)
        self.assertEqual(result.head.outcomes, [])
        self.assertTrue(perf_comparison_rows.comparison_rows("S1/default", result.base, result.head)[0][3].startswith("failed: "))
        self.assertEqual(perf_sets.comparison_exit([result]), 1)

    def test_base_that_exhausts_its_retries_is_blocked(self):
        # A base that cannot produce valid runs is reported blocked; the head is still measured.
        result, _calls = self.run_set({"base": ["theft"], "head": ["valid"]}, runs=1)
        self.assertIn("no 1 valid runs", result.base.blocked)
        self.assertIsNone(result.head.failed)
        self.assertEqual(len(result.head.outcomes), 1)

    def test_an_unmanaged_result_behind_focus_theft_stops_the_comparison(self):
        # The first attempt is unmanaged and also reports theft: a provenance failure, which stops the comparison at
        # once instead of being retried to exit 0.
        with self.assertRaises(perf_sets.StopComparison) as raised:
            self.run_set({"base": ["unmanaged theft", "valid"], "head": ["valid"]})
        self.assertIn("managed", str(raised.exception))

    def test_a_run_step_deadline_stops_the_comparison(self):
        # run_step's deadline killed and collected the harness, but never counted its group: no retry, a stop.
        with self.assertRaises(perf_sets.StopComparison) as raised:
            self.run_set({"base": ["bound", "valid"], "head": ["valid"]})
        self.assertIn(perf_watcher_tests.UNCOUNTED_STOP, str(raised.exception))

    def test_head_blocked_is_blocked_and_an_unmanaged_result_stops(self):
        # A head that cannot run the scenario exits 3; an unmanaged result (provenance) stops the comparison at once.
        result, _calls = self.run_set({"base": ["valid"], "head": ["blocked"]})
        self.assertEqual(perf_sets.comparison_exit([result]), 3)
        with self.assertRaises(perf_sets.StopComparison) as raised:
            self.run_set({"base": ["valid"], "head": ["schema"]})
        self.assertIn("managed", str(raised.exception))

    def test_a_renderer_or_presenter_mismatch_invalidates_the_pair(self):
        # Both sides must draw through the same adapter and presenter, or the pair compares two renderers.
        other_adapter = dict(perf_foreground_tests.HARDWARE_RENDERER, name="Intel(R) Arc(TM) A770 Graphics")
        cases = {"renderer": (perf_foreground_tests.windows_run(), perf_foreground_tests.windows_run(renderer=other_adapter)),
                 "presenter": (perf_foreground_tests.windows_run(), perf_foreground_tests.windows_run(software_render_mode="gpu")),
                 # A degraded macOS run beside one on the hardware path compares two presenters too.
                 "macos presenter": (perf_foreground_tests.macos_run(), perf_foreground_tests.macos_run(software_render_degraded=True))}
        for case, (matched, mismatched) in cases.items():
            with self.subTest(case):
                result, calls = self.run_set({"base": [matched], "head": [mismatched, matched]}, runs=1)
                self.assertEqual(calls, ["base", "head", "head"])
                self.assertEqual(result.attempts[1][2], case.split()[-1])
                self.assertEqual(len(result.head.outcomes), 1)

    def test_a_windows_run_without_an_adapter_never_pairs(self):
        # A run that logged no adapter beside one that did may compare two renderers, so it is not valid,
        # and the head is retried until a run reports its adapter.
        def no_adapter(plan):
            result = perf_result_tests.valid_result(grid={"cols": 250, "rows": 70}, presenter=perf_foreground_tests.WGPU_PRESENTER)
            return perf_outcomes_tests.make_outcome(plan=plan, platform="win32", renderer=None, result=result)
        result, calls = self.run_set({"base": [perf_foreground_tests.windows_run()], "head": [no_adapter, perf_foreground_tests.windows_run()]}, runs=1)
        self.assertEqual(calls, ["base", "head", "head"])
        self.assertEqual(result.attempts[1][2], "adapter")
        self.assertEqual(len(result.head.outcomes), 1)

    def test_a_gdi_variant_without_gdi_is_blocked(self):
        # The gdi variant measures the GDI presenter, so a run that did not present through GDI cannot count.
        result, calls = self.run_set({"base": [perf_foreground_tests.windows_run()], "head": [perf_foreground_tests.windows_run()]}, runs=1, variant="gdi")
        self.assertEqual(calls, ["base", "head"])
        self.assertIn("GDI", result.head.blocked)
        self.assertEqual(perf_sets.comparison_exit([result]), perf_settings.EXIT_BLOCKED)

    def test_a_default_windows_pair_on_hardware_is_valid_and_names_wgpu(self):
        # A hardware adapter that is not degraded measures the wgpu path, and the table says so.
        result, _calls = self.run_set({"base": [perf_foreground_tests.windows_run()], "head": [perf_foreground_tests.windows_run()]}, runs=1)
        self.assertEqual((len(result.base.outcomes), len(result.head.outcomes)), (1, 1))
        presenter_rows = [row for row in perf_comparison_rows.comparison_rows("S1/default", result.base, result.head)
                          if row[1] == "presenter"]
        self.assertEqual(len(presenter_rows), 1)
        self.assertTrue(all("wgpu" in cell for cell in presenter_rows[0][2:4]), presenter_rows)

    def test_a_windows_pair_runs_at_any_shared_grid(self):
        # A Windows window opens at a grid that depends on the display and its scale, so any grid is
        # measured; a pair must share one, so 281x58 beside 250x70 is not a valid pair.
        wide = perf_foreground_tests.windows_run(grid={"cols": 281, "rows": 58})
        result, calls = self.run_set({"base": [wide], "head": [wide]}, runs=1)
        self.assertEqual(calls, ["base", "head"])
        self.assertEqual((len(result.base.outcomes), len(result.head.outcomes)), (1, 1))
        configured = perf_foreground_tests.windows_run(grid={"cols": 250, "rows": 70})
        result, calls = self.run_set({"base": [wide], "head": [configured, wide]}, runs=1)
        self.assertEqual(calls, ["base", "head", "head"])
        self.assertEqual(result.attempts[1][2], "grid")
        self.assertEqual(len(result.head.outcomes), 1)

    def test_the_table_records_each_sides_grid(self):
        # The grid is no longer fixed, so a row beside the presenter names each side's grids; a side
        # whose runs reported none has no row.
        def grid_run(cols, rows):
            return perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(grid={"cols": cols, "rows": rows}))
        base = perf_metrics.SideRuns([grid_run(281, 58)])
        head = perf_metrics.SideRuns([grid_run(281, 58), grid_run(250, 70)])
        rows = [row for row in perf_comparison_rows.comparison_rows("S1/default", base, head) if row[1] == "grid"]
        self.assertEqual(rows, [["S1/default", "grid", "281x58", "281x58; 250x70", ""]])
        bare = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(grid=None))])
        self.assertFalse(any(row[1] == "grid" for row in perf_comparison_rows.comparison_rows("S1/default", bare, bare)))


def timeout_run(plan):
    """A run the harness ended at its own deadline (exit 4) with its group counted: a retryable `timeout`."""
    return perf_outcomes_tests.make_outcome(plan=plan, status="FAIL", exit_code=perf_settings.HARNESS_TIMEOUT,
                                            result=perf_result_tests.valid_result(status="timeout",
                                                                                  exit_code=perf_settings.HARNESS_TIMEOUT))


def schema_run(plan):
    """A run whose result breaks the schema without being another harness's: a `schema` failure."""
    return perf_outcomes_tests.make_outcome(plan=plan, schema_problems=["phases is not a list of objects"])


def refused_run(plan):
    """A run the harness refused (exit 2)."""
    return perf_outcomes_tests.make_outcome(plan=plan, status="FAIL", exit_code=perf_settings.HARNESS_REFUSED, result=None)


class VariantFailureTests(unittest.TestCase):
    """Failure is tracked per variant across its sets: a failure both sides share ends it after one retry a side,
    and every other failure keeps the per-side retry budget."""

    def run_sets(self, answers, set_names=("timed", "counters"), base_blocked=None):
        """Run one variant's sets with one VariantFailure, one valid run per side each; each side answers from its
        queue of kinds or factories, repeating the last. Return the results and the (set, side) of every run."""
        calls = []
        plans = {side: perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", side, Path(f"/{side}"),
                                             perf_memory_tests.HARNESS_HASH) for side in perf_settings.SIDES}
        failure = perf_sets.VariantFailure()
        results = []
        with contextlib.redirect_stdout(io.StringIO()):
            for set_name in set_names:
                def run_case(plan, _evidence, set_name=set_name):
                    calls.append((set_name, plan.side))
                    queue = answers[plan.side]
                    kind = queue.pop(0) if len(queue) > 1 else queue[0]
                    return kind(plan) if callable(kind) else perf_smoke_tests.outcome_of(kind)(plan)
                results.append(perf_sets.run_set("S1/default", plans, base_blocked, 1, run_case, Path("/e"), set_name,
                                                 variant=failure))
        return results, calls

    def test_a_shared_timeout_ends_the_variant_after_one_retry_a_side(self):
        # Two timeouts a side, in ABBA order, end the variant: its counters set never runs, both sets are stopped,
        # and the comparison fails because it has no evidence for the variant.
        results, calls = self.run_sets({"base": [timeout_run], "head": [timeout_run]})
        self.assertEqual(calls, [("timed", "base"), ("timed", "head"), ("timed", "head"), ("timed", "base")])
        timed, counters = results
        self.assertIn("both failed with timeout on 2 consecutive runs each", timed.stopped)
        self.assertIn("does not prove a harness defect", timed.stopped)
        self.assertEqual(counters.stopped, "not run: the variant ended in its timed set")
        for result in results:
            self.assertEqual((result.base.outcomes, result.head.outcomes), ([], []))
            self.assertTrue(result.base.failed and result.head.failed)
        self.assertEqual(perf_sets.comparison_exit(results), perf_settings.EXIT_FAIL)
        self.assertEqual(len(perf_sets.strict_problems(results)), 2)

    def test_an_invalidation_on_both_sides_is_shared_but_an_occlusion_is_not(self):
        # A non-occlusion invalidation is one the runner cannot cause, so it ends the variant like a timeout; an
        # occlusion can come from the host, so it keeps the budget until the head's fourth invalid run fails it.
        results, calls = self.run_sets({"base": ["invalid"], "head": ["invalid"]})
        self.assertEqual(len(calls), 4)
        self.assertIn("both failed with invalid", results[0].stopped)
        results, calls = self.run_sets({"base": ["occluded"], "head": ["occluded"]})
        self.assertEqual(len(calls), 7)
        self.assertIsNone(results[0].stopped)
        self.assertIn("occluded", results[0].head.failed)

    def test_a_focus_failure_on_both_sides_still_retries(self):
        # The runner can steal focus from both sides, so the shared rule does not apply: each side keeps its budget,
        # the head fails on its fourth invalid run, and the failed head ends the variant before its counters set.
        results, calls = self.run_sets({"base": ["theft"], "head": ["theft"]})
        self.assertEqual([side for _set, side in calls], ["base", "head", "head", "base", "base", "head", "head"])
        timed, counters = results
        self.assertIsNone(timed.stopped)
        self.assertTrue(timed.head.failed.startswith(f"no 1 valid runs after {perf_settings.RETRY_LIMIT} retries"))
        self.assertIn("focus", timed.head.failed)
        self.assertEqual(counters.stopped, "not run: the variant ended in its timed set")

    def test_a_head_only_timeout_keeps_the_full_budget_and_fails(self):
        # The base measures; the head times out on every run, retried RETRY_LIMIT times, and fails the variant.
        results, calls = self.run_sets({"base": ["valid"], "head": [timeout_run]})
        self.assertEqual(calls, [("timed", "base")] + [("timed", "head")] * (perf_settings.RETRY_LIMIT + 1))
        timed, counters = results
        self.assertIsNone(timed.stopped)
        self.assertEqual(len(timed.base.outcomes), 1)
        self.assertIn("timeout", timed.head.failed)
        self.assertEqual(counters.stopped, "not run: the variant ended in its timed set")
        self.assertEqual(perf_sets.comparison_exit(results), perf_settings.EXIT_FAIL)

    def test_different_shared_kinds_on_each_side_are_not_shared(self):
        # A base timeout beside a head invalidation is not the same failure, so the head keeps its whole budget.
        results, calls = self.run_sets({"base": [timeout_run], "head": ["invalid"]}, set_names=("timed",))
        self.assertEqual(len(calls), 7)
        self.assertIsNone(results[0].stopped)
        self.assertIn("invalid", results[0].head.failed)

    def test_a_schema_failure_or_refusal_ends_only_its_variant(self):
        # The run's result cannot be trusted, so the variant is recorded incomplete with the reason, its valid base
        # run is dropped, and nothing is raised: the comparison goes on to its next variant.
        for name, factory in (("schema", schema_run), ("refused", refused_run)):
            with self.subTest(name):
                results, calls = self.run_sets({"base": ["valid"], "head": [factory]})
                self.assertEqual(calls, [("timed", "base"), ("timed", "head")])
                timed, counters = results
                self.assertTrue(timed.stopped.startswith(f"head: {name}: "), timed.stopped)
                self.assertEqual(timed.base.outcomes, [])
                self.assertEqual(timed.base.failed, timed.stopped)
                self.assertEqual(counters.stopped, "not run: the variant ended in its timed set")

    def test_a_blocked_base_shares_no_failure(self):
        # A base that cannot build never ran, so the head's timeouts use its whole budget, and the base's later
        # sets keep its blocked reason rather than a failure.
        results, calls = self.run_sets({"base": ["valid"], "head": [timeout_run]}, base_blocked="error[E0599]")
        self.assertEqual(calls, [("timed", "head")] * (perf_settings.RETRY_LIMIT + 1))
        timed, counters = results
        self.assertIsNone(timed.stopped)
        self.assertIn("timeout", timed.head.failed)
        self.assertEqual((counters.base.blocked, counters.base.failed), ("error[E0599]", None))

    def test_a_streak_carries_into_the_next_set(self):
        # A base out of retries in the timed set still timed out last; in the counters set two head timeouts make
        # the failure shared, so that set ends after three runs instead of its own full budget.
        results, calls = self.run_sets({"base": [timeout_run], "head": ["valid", timeout_run]})
        self.assertIn("no 1 valid runs", results[0].base.blocked)
        self.assertEqual([side for set_name, side in calls if set_name == "counters"], ["base", "head", "head"])
        self.assertIn("both failed with timeout", results[1].stopped)
