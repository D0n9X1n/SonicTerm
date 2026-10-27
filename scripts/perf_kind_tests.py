"""Perf kind tests."""

from __future__ import annotations
from perf_test_fixture import perf
import inspect
import json
from pathlib import Path
import unittest
from unittest import mock

import perf_atlas_retry_tests
import perf_attribution_report
import perf_build_tests
import perf_comparison_rows
import perf_comparison_tests
import perf_driver_tests
import perf_execution
import perf_latency_tests
import perf_metrics
import perf_outcomes_tests
import perf_report
import perf_result_tests
import perf_scenarios
import perf_sets




def kind_outcome(*phases, latency=None, throughput=None, evidence="/e/01-head"):
    """A valid run outcome of a phase-kinds harness carrying `phases` (kind_phase bodies)."""
    result = perf_result_tests.valid_result(phases=list(phases), latency=latency, throughput=throughput)
    return perf_outcomes_tests.make_outcome(result=result, evidence=Path(evidence))


def transition_phase(completion=None, missing=None, **fields):
    """A transition phase reaching its endpoint `completion` ms after its start, or missing it for `missing`."""
    ending = {"completion_ms": completion} if missing is None else {"completion_missing": missing}
    return perf_latency_tests.kind_phase("transition", name="image", endpoint="image-registered-then-presented", **ending, **fields)


def side_with(*outcomes):
    return perf_metrics.SideRuns(list(outcomes))


def require(module, name, test):
    """`module.name`, or a test failure (never an error) when the module does not define it."""
    found = getattr(module, name, None)
    if found is None:
        test.fail(f"{module.__name__} defines no {name}")
    return found


class PhaseKindReportTests(unittest.TestCase):
    def test_a_transition_reports_its_completion_per_run_and_no_rates(self):
        # A transition's rows are its completion per accepted run (a value or the missing reason), the median and
        # range of those that completed, each side's contributing count, and its first present, presented frames
        # and non-presenting redraws; never an FPS or an interval percentile.
        base = side_with(kind_outcome(transition_phase(40.0), evidence="/e/01-base"),
                         kind_outcome(transition_phase(missing="expired"), evidence="/e/04-base"))
        head = side_with(kind_outcome(transition_phase(50.0), evidence="/e/02-head"),
                         kind_outcome(transition_phase(60.0), evidence="/e/03-head"))
        rows = perf_comparison_rows.comparison_rows("S11/release", base, head)
        completion = perf_comparison_tests.row_for(rows, "image completion (ms)")
        self.assertEqual(completion[2:], [
            "40.00 (40.00–40.00), 1/2 runs, single observation; 01-base 40.00, 04-base n/a: expired",
            "55.00 (50.00–60.00), 2/2 runs; 02-head 50.00, 03-head 60.00", "+37.5%"])
        self.assertEqual(perf_comparison_tests.row_for(rows, "image first present (ms)")[2], "1.00 (1.00–1.00)")
        self.assertEqual(perf_comparison_tests.row_for(rows, "image presented frames (count)")[3], "120.00 (120.00–120.00)")
        self.assertEqual(perf_comparison_tests.row_for(rows, "image non-presenting redraws (count)")[3], "10.00 (10.00–10.00)")
        metrics = [row[1] for row in rows]
        self.assertFalse([metric for metric in metrics if "fps" in metric or "interval" in metric], metrics)

    def test_a_hold_reports_activity_counts_only(self):
        # A hold is idle by design: its seconds, presented frames, redraws requested and CPU, no rate or interval.
        hold = side_with(kind_outcome(perf_latency_tests.kind_phase("hold", name="idle")))
        metrics = [row[1] for row in perf_comparison_rows.comparison_rows("S1/default", hold, hold)]
        idle = [metric for metric in metrics if metric.startswith("idle ")]
        self.assertEqual(idle, ["idle hold (s)", "idle presented frames (count)",
                                "idle redraws requested (count)", "idle CPU (s)"])

    def test_a_sustained_phase_and_a_legacy_phase_keep_their_rows(self):
        # Sustained phases and phases of a harness that predates kinds report what they reported before.
        for phase in (perf_latency_tests.kind_phase("sustained"), perf_result_tests.valid_result()["phases"][0]):
            with self.subTest(kind=phase.get("kind")):
                side = side_with(kind_outcome(phase))
                metrics = [row[1] for row in perf_comparison_rows.comparison_rows("S2/default", side, side)]
                self.assertIn("workload presented frames (fps)", metrics)
                self.assertIn("workload present interval p95 (ms)", metrics)
                self.assertNotIn("workload completion (ms)", metrics)


def flag_result(label, base_outcomes, head_outcomes, set_name="timed"):
    return perf_sets.SetResult(label, set_name, side_with(*base_outcomes), side_with(*head_outcomes))


def sustained_with(intervals=(16.6, 16.7), frames=120, wall_s=60.0):
    return perf_latency_tests.kind_phase("sustained", present_interval_ms=list(intervals), presented_frames=frames,
                      start_unix_s=10.0, end_unix_s=10.0 + wall_s)


class CandidateFlagTests(unittest.TestCase):
    def checks(self, *results):
        return require(perf, "flag_checks", self)(list(results))

    def flagged(self, *results):
        return {(check.phase, check.metric, check.statistic) for check in self.checks(*results) if check.flagged}

    def test_a_worsening_beyond_the_base_range_is_flagged_and_an_improvement_never(self):
        # Intervals: higher is worse, judged per statistic against the base's per-run range of that statistic;
        # a sub-refresh increase is still flagged. FPS: lower is worse. Improvements beyond the range never are.
        base = [kind_outcome(sustained_with((16.6, 16.7))), kind_outcome(sustained_with((16.6, 16.8)))]
        slower = [kind_outcome(sustained_with((16.9, 17.0), frames=100))]
        faster = [kind_outcome(sustained_with((10.0, 10.1), frames=200))]
        self.assertEqual(self.flagged(flag_result("S2/default", base, slower)),
                         {("workload", "present interval", "median"), ("workload", "present interval", "p95"),
                          ("workload", "fps", "median")})
        self.assertEqual(self.flagged(flag_result("S2/default", base, faster)), set())
        within = [kind_outcome(sustained_with((16.6, 16.75)))]
        self.assertEqual(self.flagged(flag_result("S2/default", base, within)), set())

    def test_a_transition_is_flagged_when_its_head_median_exceeds_every_base_run(self):
        # Completion: the head median against the base per-run maximum, with no refresh-period waiver.
        base = [kind_outcome(transition_phase(40.0)), kind_outcome(transition_phase(42.0))]
        late = [kind_outcome(transition_phase(42.5)), kind_outcome(transition_phase(43.0))]
        early = [kind_outcome(transition_phase(20.0))]
        self.assertEqual(self.flagged(flag_result("S11/release", base, late)), {("image", "completion", "median")})
        self.assertEqual(self.flagged(flag_result("S11/release", base, early)), set())
        check = next(check for check in self.checks(flag_result("S11/release", base, late))
                     if check.metric == "completion")
        self.assertEqual((check.base_low, check.base_high, check.head_value), (40.0, 42.0, 42.75))
        self.assertEqual((check.base_samples, check.head_samples), (2, 2))

    def test_holds_unknown_metrics_and_legacy_results_are_never_checked(self):
        # A hold is never flagged however it moves; a metric absent from the direction map is never checked; a
        # result of a harness without phase kinds contributes no check at all.
        busy = [kind_outcome(perf_latency_tests.kind_phase("hold", name="idle", presented_frames=5000))]
        quiet = [kind_outcome(perf_latency_tests.kind_phase("hold", name="idle", presented_frames=1))]
        self.assertEqual(self.checks(flag_result("S1/default", quiet, busy)), [])
        legacy = [perf_outcomes_tests.make_outcome()]
        self.assertEqual(self.checks(flag_result("S2/default", legacy, legacy)), [])
        base = [kind_outcome(sustained_with(frames=120))]
        head = [kind_outcome(sustained_with(frames=60))]
        directions = dict(require(perf, "FLAG_DIRECTIONS", self))
        del directions["fps"]
        with mock.patch.object(perf_comparison_rows, "FLAG_DIRECTIONS", directions):
            metrics = {check.metric for check in self.checks(flag_result("S2/default", base, head))}
        self.assertNotIn("fps", metrics)
        self.assertIn("present interval", metrics)

    def test_latency_counts_attributed_samples_and_compares_like_statistics(self):
        # Latency samples are the attributed keypresses only; the pooled head p95 is read against the base's
        # per-run p95s, never its per-run medians.
        def latency(*values):
            return {"samples": [{"latency_ms": value} for value in values], "attributed": 0, "total": 0,
                    "coverage": 0.0}
        base = [kind_outcome(sustained_with(), latency=latency(10.0, 10.0, 30.0)),
                kind_outcome(sustained_with(), latency=latency(10.0, 10.0, 12.0))]
        head = [kind_outcome(sustained_with(), latency=latency(10.0, None, None, 25.0))]
        checks = {(check.metric, check.statistic): check for check in self.checks(flag_result("S2/default", base, head))}
        tail_check = checks[("latency", "p95")]
        self.assertEqual((tail_check.base_low, tail_check.base_high, tail_check.head_value, tail_check.flagged),
                         (12.0, 30.0, 25.0, False))
        self.assertEqual((tail_check.base_samples, tail_check.head_samples), (6, 2))
        self.assertEqual(checks[("latency", "median")].flagged, True)

    def test_the_report_prints_candidate_flags_only_when_kinds_were_checked(self):
        # The section lists each flag; with checks but no flag it says so; with no check (legacy) it is absent.
        base = [kind_outcome(transition_phase(40.0))]
        head = [kind_outcome(transition_phase(90.0))]
        checks = self.checks(flag_result("S11/release", base, head))
        document = perf_report.comparison_document([], [], [], [], [], flag_checks=checks)
        self.assertIn("### Candidate flags", document)
        self.assertIn("| S11/release | timed | image | completion | median | 40.00–40.00 | 90.00 | higher is worse |",
                      document)
        unflagged = self.checks(flag_result("S11/release", base, base))
        self.assertIn("None of the 1 checks", perf_report.comparison_document([], [], [], [], [], flag_checks=unflagged))
        self.assertNotIn("Candidate flags", perf_report.comparison_document([], [], [], [], [], flag_checks=None))
        self.assertNotIn("Candidate flags", perf_report.comparison_document([], [], [], [], [], flag_checks=[]))


class RunIdentityTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    def test_a_comparison_records_its_identity_for_perf_flags(self):
        # perf-flags.py binds each artifact to its run, refs, harness, settings and metric definitions.
        environ = {"GITHUB_RUN_ID": "77", "GITHUB_RUN_ATTEMPT": "2"}
        code, _gate, _calls, _plans, _work, out = self.compare(environ=environ)
        self.assertEqual(code, 0)
        identity_path = out / "run-identity.json"
        self.assertTrue(identity_path.is_file(), sorted(path.name for path in out.iterdir()))
        identity = json.loads(identity_path.read_text(encoding="utf-8"))
        self.assertEqual((identity["run_id"], identity["run_attempt"]), ("77", 2))
        self.assertEqual((identity["base_sha"], identity["head_sha"]), (self.SHAS["main"], self.SHAS["HEAD"]))
        self.assertEqual(identity["flag_metrics_version"], require(perf, "FLAG_METRICS_VERSION", self))
        self.assertEqual(set(identity), {"schema_version", "run_id", "run_attempt", "platform", "base_sha",
                                         "head_sha", "harness_hash", "settings", "flag_metrics_version", "sets",
                                         "capabilities", "guard_api"})
        # The harness's declared capabilities, so perf-flags validates each result as perf-compare did.
        self.assertEqual(identity["capabilities"],
                         {"latency_split_schema": None, "phase_kinds": None, "s10_attribution": None,
                          "echo_timeline_schema": None, "guard_correlation_schema": None})
        # Each set's final inventory: per side its blocked or failed status and the attempt directories it accepted,
        # so perf-flags binds results to accepted executions, never to whatever valid-looking files it finds.
        self.assertEqual(identity["sets"], [{"label": "S1/default", "dataset": "timed",
                                             "base": {"status": "", "accepted": ["01-base"]},
                                             "head": {"status": "", "accepted": ["02-head"]}}])

    def test_a_phase_kinds_comparison_records_its_capabilities(self):
        # A head whose list declares the split schema and phase kinds records both, and the attribution
        # capability it does not declare as null: the identity always carries every known key.
        _code, _gate, _calls, _plans, _work, out = self.compare(listing=perf_latency_tests.LIST_WITH_KINDS)
        identity = json.loads((out / "run-identity.json").read_text(encoding="utf-8"))
        self.assertEqual(identity.get("capabilities"),
                         {"latency_split_schema": 1, "phase_kinds": 1, "s10_attribution": None,
                          "echo_timeline_schema": None, "guard_correlation_schema": None})

    def test_an_attribution_comparison_records_its_capability(self):
        # A head whose list declares the S10 attribution schema records it, so perf-flags validates attribution
        # records under it instead of refusing them as undeclared.
        listing = {**perf_build_tests.LIST_JSON, "capabilities": {"latency_split_schema": 1, "phase_kinds": 1, "s10_attribution": 1}}
        _code, _gate, _calls, _plans, _work, out = self.compare(listing=listing)
        identity = json.loads((out / "run-identity.json").read_text(encoding="utf-8"))
        self.assertEqual(identity.get("capabilities"),
                         {"latency_split_schema": 1, "phase_kinds": 1, "s10_attribution": 1,
                          "echo_timeline_schema": None, "guard_correlation_schema": None})

    def test_an_echo_timeline_comparison_records_its_capability_and_plans_carry_it(self):
        # A head whose list declares the echo-timeline schema records it for perf-flags, and every plan carries it
        # with the plan's own scenario and variant, so each result is validated under it.
        capabilities = {"latency_split_schema": 1, "phase_kinds": 1, "s10_attribution": 1, "echo_timeline_schema": 1}
        _code, _gate, _calls, plans, _work, out = self.compare(listing={**perf_build_tests.LIST_JSON, "capabilities": capabilities})
        identity = json.loads((out / "run-identity.json").read_text(encoding="utf-8"))
        self.assertEqual(identity.get("capabilities"), {**capabilities, "guard_correlation_schema": None})
        self.assertEqual({plan.echo_timeline_schema for plan in plans}, {1})
        self.assertIn("echo_timeline_schema=plan.echo_timeline_schema", inspect.getsource(perf_execution.execute_run))
        self.assertIn("scope=(plan.scenario.id, plan.variant)", inspect.getsource(perf_execution.execute_run))

    def test_a_guard_correlation_comparison_records_its_capability_and_plans_carry_it(self):
        # A head whose list declares the guard-correlation contract records it for perf-flags, every plan carries
        # it, and each plan knows whether both sides were built with the take's cfg; execute_run validates under it.
        capabilities = {"latency_split_schema": 1, "phase_kinds": 1, "s10_attribution": 1,
                        "echo_timeline_schema": 1, "guard_correlation_schema": 1}
        _code, _gate, _calls, plans, _work, out = self.compare(listing={**perf_build_tests.LIST_JSON, "capabilities": capabilities})
        identity = json.loads((out / "run-identity.json").read_text(encoding="utf-8"))
        self.assertEqual(identity.get("capabilities"), capabilities)
        self.assertEqual({plan.guard_correlation_schema for plan in plans}, {1})
        self.assertTrue(all(isinstance(plan.guard_api, bool) for plan in plans))
        self.assertIn("guard_correlation_schema=plan.guard_correlation_schema", inspect.getsource(perf_execution.execute_run))
        for value in (True, 1.0, 2):
            with self.subTest(value=value), self.assertRaises(ValueError):
                perf_scenarios.parse_scenario_list(json.dumps(
                    {**perf_build_tests.LIST_JSON, "capabilities": {"latency_split_schema": 1, "guard_correlation_schema": value}}))

    def test_a_list_declares_the_echo_timeline_schema_strictly(self):
        # Only a known integer schema is read; a bool, a float or an unknown schema is refused, never legacy.
        listed = perf_scenarios.parse_scenario_list(json.dumps(
            {**perf_build_tests.LIST_JSON, "capabilities": {"latency_split_schema": 1, "echo_timeline_schema": 1}}))
        self.assertEqual([scenario.echo_timeline_schema for scenario in listed], [1, 1])
        self.assertEqual([scenario.echo_timeline_schema
                          for scenario in perf_scenarios.parse_scenario_list(json.dumps(perf_latency_tests.LIST_WITH_SPLIT))], [None, None])
        for value in (True, 1.0, 2, "1"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                perf_scenarios.parse_scenario_list(json.dumps(
                    {**perf_build_tests.LIST_JSON, "capabilities": {"latency_split_schema": 1, "echo_timeline_schema": value}}))

    def test_a_failed_side_lists_no_accepted_attempt(self):
        # A side that ends blocked or failed discards its valid attempts, and the inventory says so.
        valid = perf_outcomes_tests.make_outcome()
        # The base kept an outcome object, but its blocked status alone decides: it accepts nothing.
        result = perf_sets.SetResult("S1/default", "timed", perf_metrics.SideRuns([valid], blocked="exit 5"),
                                perf_metrics.SideRuns([valid]),
                                attempts=[("base", "/e/01-base", "valid", []), ("head", "/e/02-head", "valid", []),
                                          ("base", "/e/03-base", "launch", ["exit 5"])])
        self.assertEqual(require(perf, "run_inventory", self)([result]),
                         [{"label": "S1/default", "dataset": "timed", "base": {"status": "exit 5", "accepted": []},
                           "head": {"status": "", "accepted": ["02-head"]}}])

    def test_a_phase_kinds_comparison_prints_its_candidate_flags(self):
        # The comparison computes the flags of its own accepted runs: a head transition slower than every base run
        # is listed under Candidate flags in comparison.md.
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 60000, "scale_factor": 2.0}

        def runs_with(completion_ms):
            return lambda plan: perf_outcomes_tests.make_outcome(plan=plan, result=perf_result_tests.valid_result(
                monitor=monitor, phases=[transition_phase(completion_ms)]))
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=runs_with(40.0), head_run=runs_with(90.0))
        self.assertEqual(code, 0)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("### Candidate flags", document)
        self.assertIn("| S1/default | timed | image | completion | median | 40.00–40.00 | 90.00 |", document)


class PhaseKindCoverageTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    def test_an_image_whose_completion_improves_while_frames_fall_is_not_flagged(self):
        # macOS S11/release image: completion improves 222.870 -> 142.500 ms while presented frames fall 2 -> 1.
        # The completion falls, so nothing is flagged, and no frame rate is reported to suggest a regression.
        base = [kind_outcome(transition_phase(222.870, presented_frames=2, last_present_seq=2))]
        head = [kind_outcome(transition_phase(142.500, presented_frames=1, last_present_seq=1))]
        result = flag_result("S11/release", base, head)
        self.assertEqual([check.flagged for check in perf_comparison_rows.flag_checks([result])], [False])
        rows = perf_comparison_rows.comparison_rows("S11/release", result.base, result.head)
        self.assertEqual(perf_comparison_tests.row_for(rows, "image completion (ms)")[4], "-36.1%")
        self.assertFalse([row[1] for row in rows if "fps" in row[1]])

    def test_recovery_reports_activity_and_its_own_table_but_no_rates_or_flags(self):
        # S1/atlas-retry's recovery is a hold: activity counts and the dedicated recovery table only, with no
        # generic rate, interval, completion or candidate flag.
        def recovery_run():
            outcome = kind_outcome(perf_latency_tests.kind_phase("hold", name="recovery"))
            outcome.result["atlas_recovery"] = {"episodes": 8, "distinct_keys": 70, "records": perf_atlas_retry_tests.recovery_records()}
            return outcome
        base, head = side_with(recovery_run()), side_with(recovery_run())
        metrics = [row[1] for row in perf_comparison_rows.comparison_rows("S1/atlas-retry", base, head)
                   if row[1].startswith("recovery ")]
        self.assertEqual(metrics, ["recovery hold (s)", "recovery presented frames (count)",
                                   "recovery redraws requested (count)", "recovery CPU (s)"])
        self.assertEqual(perf_comparison_rows.flag_checks([perf_sets.SetResult("S1/atlas-retry", "counters", base, head)]), [])
        self.assertTrue(perf_attribution_report.atlas_recovery_rows("S1/atlas-retry", base, head))

    def test_an_older_harness_is_printed_in_its_own_unclassified_block(self):
        # A harness without phase kinds cannot be classified, so its rows leave the main table for a separate
        # block and never enter the candidate flags; a phase-kinds run stays in the main table.
        code, _gate, _calls, _plans, _work, out = self.compare()
        self.assertEqual(code, 0)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        heading = "### Unclassified (older harness)"
        self.assertIn(heading, document)
        main, _, unclassified = document.partition(heading)
        self.assertIn("| S1/default | workload presented frames (fps) |", unclassified)
        self.assertNotIn("workload presented frames (fps)", main)
        self.assertNotIn("Candidate flags", document)
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 60000, "scale_factor": 2.0}

        def runs_with(completion_ms):
            return lambda plan: perf_outcomes_tests.make_outcome(plan=plan, result=perf_result_tests.valid_result(
                monitor=monitor, phases=[transition_phase(completion_ms)]))
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=runs_with(40.0), head_run=runs_with(41.0))
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertNotIn(heading, document)
        self.assertIn("| S1/default | image completion (ms) |", document)

    def test_an_unclassified_set_contributes_no_candidate_flag(self):
        # A set with any unkinded run is reported only in the unclassified block, so none of its runs, kinded
        # or not, is read by kind: no check, however far its kinded runs moved.
        legacy = perf_outcomes_tests.make_outcome()
        base = [kind_outcome(transition_phase(40.0)), legacy]
        head = [kind_outcome(transition_phase(900.0))]
        result = flag_result("S11/release", base, head)
        self.assertTrue(perf_comparison_rows.unclassified_result(result))
        self.assertEqual(perf_comparison_rows.flag_checks([result]), [])

    def test_a_run_with_any_unkinded_phase_is_unclassified(self):
        # One phase without a kind makes the whole run unreadable by kind, even when its other phases have one;
        # a run whose every phase has a known kind is classified.
        legacy_phase = perf_result_tests.valid_result()["phases"][0]
        mixed = flag_result("S2/default", [kind_outcome(perf_latency_tests.kind_phase(), dict(legacy_phase, name="idle"))],
                            [kind_outcome(perf_latency_tests.kind_phase())])
        kinded = flag_result("S2/default", [kind_outcome(perf_latency_tests.kind_phase())], [kind_outcome(perf_latency_tests.kind_phase())])
        self.assertEqual((perf_comparison_rows.unclassified_result(mixed), perf_comparison_rows.unclassified_result(kinded)), (True, False))
