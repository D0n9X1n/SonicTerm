"""Perf attribution tests."""

from __future__ import annotations
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

import perf_attribution_report
import perf_attribution_schema
import perf_build_tests
import perf_harness_sources
import perf_latency_tests
import perf_memory
import perf_memory_tests
import perf_metrics
import perf_prebuilt_tests
import perf_result_schema
import perf_result_tests
import perf_scenarios
import perf_sets


def present_line(seq, resets, *, arming=1, pane=7, set_=False, sentinel=False, prompt=False,
                 admission="closed", epoch=None):
    """One parsed present line of pane 7."""
    return perf_memory.PresentLine("present", arming=arming, seq=seq, pane=pane, resets=resets,
                            epoch=resets if epoch is None else epoch, set=set_, sentinel=sentinel,
                            prompt=prompt, admission=admission)


def armed_outcome(lines, *, updates=3, seq_start=10, seq_end=None, end_resets=None, cached=2, presented=None,
                  counted=None, baseline_set=False, end_epoch=None):
    """A counters run of S10 armed from a closed baseline of 5 resets, with `lines` in its logs. `presented` and
    `counted` replace the phase's presented frames and its counters' presented delta, which otherwise equal the
    sequence range; `baseline_set` and `end_epoch` replace the baseline's set bit and the final epoch."""
    seq_end = seq_start + len(lines) if seq_end is None else seq_end
    fresh = seq_end - seq_start
    record = {"state": "armed", "arming": 1, "pane": 7, "updates": updates,
              "baseline": {"set": baseline_set, "epoch": 5, "resets": 5}, "seq_start": seq_start, "seq_end": seq_end,
              "end": {"set": False, "epoch": 5 + updates if end_epoch is None else end_epoch,
                      "resets": 5 + updates if end_resets is None else end_resets}}
    phase = {"name": "stream", "updates": updates, "presented_frames": fresh if presented is None else presented,
             "frame_counters": {"window": {"cached": cached, "presented": fresh if counted is None else counted}},
             "s10_attribution": record}
    return SimpleNamespace(result={"phases": [phase]}, present_lines=list(lines))


def complete_lines():
    """A complete S10/sync phase of 3 updates: a pre-workload present, each update once, then the sentinel."""
    return [present_line(11, 5), present_line(12, 6), present_line(13, 7), present_line(14, 8),
            present_line(15, 8, sentinel=True)]



class S10AttributionCfgTests(unittest.TestCase):
    """`perf_s10_attribution_api` is an entry of the canonical harness cfg table."""

    def test_the_cfg_is_on_only_when_both_trees_define_both_methods(self):
        # The table's detection turns the entry on only when both trees define both App methods; a tree without
        # one (a base before the prerequisite) turns it off for both, and the atlas-retry entry is decided apart.
        table = perf_build_tests.REAL_GATE.HARNESS_API_CFGS
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)

            def tree(name, methods):
                built = root / name
                source = built / "crates/sonicterm-app/src/app/perf_present.rs"
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text("impl App {\n" + "".join(f"    pub fn {method}(&mut self) {{}}\n"
                                                         for method in methods) + "}\n", encoding="utf-8")
                return built
            both = ("arm_s10_attribution", "disarm_s10_attribution")
            cases = {"both": (both, both, ("perf_s10_attribution_api",)),
                     "base lacks disarm": (both, both[:1], ()),
                     "base predates both": (both, (), ())}
            for name, (head, base, expected) in cases.items():
                with self.subTest(name):
                    decided, _ = perf_harness_sources.harness_cfg_decision(
                        {"head": tree(f"{name}-head", head), "base": tree(f"{name}-base", base)}, table)
                    self.assertEqual(decided, expected)

    def test_a_list_declares_the_attribution_schema_strictly(self):
        # The schema is read from each side's own list; an unknown value, a non-integer or an extra key is
        # refused, and a list without it has none.
        listed = perf_scenarios.parse_scenario_list(json.dumps(
            {**perf_build_tests.LIST_JSON, "capabilities": {"latency_split_schema": 1, "s10_attribution": 1}}))
        self.assertEqual([scenario.attribution_schema for scenario in listed], [1, 1])
        self.assertEqual([scenario.attribution_schema
                          for scenario in perf_scenarios.parse_scenario_list(json.dumps(perf_latency_tests.LIST_WITH_SPLIT))], [None, None])
        for value in (2, True, 1.0, "1", None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                perf_scenarios.parse_scenario_list(json.dumps(
                    {**perf_build_tests.LIST_JSON, "capabilities": {"latency_split_schema": 1, "s10_attribution": value}}))
        with self.assertRaises(ValueError):
            perf_scenarios.parse_scenario_list(json.dumps({**perf_build_tests.LIST_JSON, "capabilities": {"s10_attribution": 1}}))




def attributed_result(record=None, cfg=True):
    """A valid result whose counted phase carries `record`, from a build whose cfg is `cfg`."""
    data = perf_result_tests.valid_result(s10_attribution_api=cfg)
    data["phases"][0] = {**data["phases"][0], "updates": 3}
    if record is not None:
        data["phases"][0]["s10_attribution"] = record
    return data


ARMED_RECORD = {"state": "armed", "arming": 1, "pane": 7, "updates": 3,
                "baseline": {"set": False, "epoch": 5, "resets": 5}, "seq_start": 10, "seq_end": 15,
                "end": {"set": False, "epoch": 8, "resets": 8}}


class S10AttributionResultTests(unittest.TestCase):
    """The result's recorded cfg and each counted phase's attribution record."""

    def problems(self, data, attribution_api=True, schema=1):
        return perf_attribution_schema.attribution_problems(data, attribution_api, schema)

    def test_a_record_must_match_the_comparisons_cfg_and_be_well_formed(self):
        # The recorded cfg must equal the comparison's; an armed record needs a build that calls the API, so a
        # side never inherits the other's; an unknown reason or a malformed armed record is refused.
        self.assertEqual(self.problems(attributed_result(ARMED_RECORD)), [])
        self.assertEqual(self.problems(attributed_result({"state": "unavailable", "reason": "not-armed"})), [])
        self.assertEqual(self.problems(attributed_result({"state": "unavailable", "reason": "api-disabled"},
                                                         cfg=False), attribution_api=False), [])
        refused = {
            "cfg mismatch": (attributed_result(ARMED_RECORD), False),
            "cfg missing": ({key: value for key, value in attributed_result().items()
                             if key != "s10_attribution_api"}, True),
            "armed without the API": (attributed_result(ARMED_RECORD, cfg=False), None),
            "unknown reason": (attributed_result({"state": "unavailable", "reason": "maybe"}), True),
            "malformed armed": (attributed_result({**ARMED_RECORD, "baseline": {"set": "no"}}), True),
            "other updates": (attributed_result({**ARMED_RECORD, "updates": 4}), True),
            "unknown state": (attributed_result({"state": "passed"}), True),
        }
        for name, (data, cfg) in refused.items():
            with self.subTest(name):
                self.assertTrue(self.problems(data, attribution_api=cfg), name)

    def test_a_harness_without_the_schema_is_not_checked(self):
        # An older harness records no attribution, so nothing about it is required.
        self.assertEqual(self.problems(perf_result_tests.valid_result(), schema=None), [])
        self.assertEqual(perf_result_schema.validate_result(perf_result_tests.valid_result(), perf_memory_tests.HARNESS_HASH, 0), [])


class S10AttributionLineTests(unittest.TestCase):
    """The App's `sonic::perf_present` lines, as the run log writes them."""

    def test_a_present_line_parses_and_a_malformed_one_keeps_its_problem(self):
        # The fields the App writes parse into a line; a missing field or an unknown admission is kept with its
        # problem, so the run reads incomplete; any other line is not an attribution line.
        line = ("2026-10-05T20:00:00.000001Z  WARN sonic::perf_present: perf_present kind=\"present\" arming=3 "
                "seq=41 window=\"main\" pane=7 resets=3 epoch=4 set=true sentinel=true prompt=false "
                "admission=\"credit\"")
        self.assertEqual(perf_memory.parse_present_line(line), perf_memory.PresentLine(
            "present", arming=3, seq=41, pane=7, resets=3, epoch=4, set=True, sentinel=True, prompt=False,
            admission="credit"))
        overflow = ("2026-10-05T20:00:00Z  WARN sonic::perf_present: perf_present kind=\"overflow\" arming=3 "
                    "seq=42 window=\"main\" pane=7 emitted=68")
        self.assertEqual(perf_memory.parse_present_line(overflow),
                         perf_memory.PresentLine("overflow", arming=3, seq=42, pane=7, emitted=68))
        self.assertIsNotNone(perf_memory.parse_present_line(line.replace("seq=41 ", "")).problem)
        self.assertIsNotNone(perf_memory.parse_present_line(line.replace("credit", "luck")).problem)
        self.assertIsNone(perf_memory.parse_present_line("2026-10-05T20:00:00Z  WARN sonic::render: perf_present"))


class S10AttributionAnalysisTests(unittest.TestCase):
    """Each counters run's classification, histogram and identity, and each side's verdict."""

    def test_a_complete_sync_run_attributes_every_present(self):
        # One pre-workload present, each update once, one sentinel present: the identity holds and no update
        # was shown twice.
        run = perf_attribution_report.attribution_run(armed_outcome(complete_lines()), synchronized=True)
        self.assertEqual(run.state, "complete", run.reasons)
        self.assertEqual((run.fresh, run.buckets, run.max_presents, run.never_shown), (5, (0, 3, 0, 0), 1, 0))
        self.assertEqual(run.non_update, {"pre-workload": 1, "sentinel": 1})
        self.assertTrue(run.identity_holds)
        self.assertEqual((run.cached, run.admissions), (2, {"closed": 5}))

    def test_every_gap_in_the_evidence_makes_a_run_incomplete(self):
        # A presented frame without a line, a repeated line, an overflow, a malformed line, an open update, a
        # fixture that played the wrong count, another pane's line and a decreasing epoch each make it incomplete.
        lines = complete_lines()
        cases = {
            "a frame with no line": armed_outcome(lines[:2] + lines[3:], seq_end=15),
            "a repeated line": armed_outcome(lines + [lines[-1]], seq_end=15),
            "overflow": armed_outcome(lines + [perf_memory.PresentLine("overflow", arming=1, seq=15, pane=7)]),
            "malformed": armed_outcome(lines + [perf_memory.PresentLine("present", problem="seq is None")]),
            "open update": armed_outcome(lines[:2] + [present_line(13, 6, set_=True, admission="timeout")]
                                         + lines[3:]),
            "wrong fixture count": armed_outcome(lines, end_resets=7),
            "another pane": armed_outcome(lines + [present_line(16, 8, pane=9)]),
            "epoch decreased": armed_outcome(lines[:2] + [present_line(13, 7, epoch=1)] + lines[3:]),
        }
        for name, outcome in cases.items():
            with self.subTest(name):
                run = perf_attribution_report.attribution_run(outcome, synchronized=True)
                self.assertEqual(run.state, "incomplete", name)
                self.assertTrue(run.reasons, name)

    def test_a_missing_line_is_named_even_when_the_identity_balances(self):
        # Frame 14 has no line and update 2 is shown twice at frame 13: the counts balance the identity, so only
        # the sequence check sees the frame without a line and the repeated one.
        lines = [present_line(11, 5), present_line(12, 6), present_line(13, 7), present_line(13, 7),
                 present_line(15, 8, sentinel=True)]
        run = perf_attribution_report.attribution_run(armed_outcome(lines, seq_end=15), synchronized=True)
        self.assertEqual(run.state, "incomplete")
        self.assertIn("1 presented frame(s) have no line and 1 have more than one", run.reasons)

    def test_an_unarmed_phase_is_unavailable_and_default_is_unsupported(self):
        # A disabled adapter, an App that armed nothing (the stub) and a missing record are unavailable, never
        # complete; S10/default reports marker classes only.
        for reason in ("api-disabled", "not-armed"):
            outcome = SimpleNamespace(result={"phases": [{"name": "stream", "updates": 3, "s10_attribution":
                                                          {"state": "unavailable", "reason": reason}}]},
                                      present_lines=[])
            self.assertEqual(perf_attribution_report.attribution_run(outcome, True), perf_attribution_report.AttributionRun("unavailable", [reason]))
        self.assertEqual(perf_attribution_report.attribution_run(SimpleNamespace(result=None, present_lines=[]), True).state,
                         "unavailable")
        default = perf_attribution_report.attribution_run(armed_outcome(complete_lines()), synchronized=False)
        self.assertEqual(default.state, "unsupported")
        self.assertEqual(default.non_update, {"stream": 4, "sentinel": 1})

    def test_the_verdict_needs_complete_runs_and_the_evidence_boundary(self):
        # Complete runs with every update shown at most once and an excess within ±1 of the timed median hold the
        # ratio, attributed under instrumentation only; a repeat asks for a reproduction; an incomplete or
        # unavailable run, a distant timed excess or no timed run leave the decision pending.
        complete = perf_attribution_report.attribution_run(armed_outcome(complete_lines()), True)
        verdict = perf_attribution_report.attribution_verdict([complete, complete], True, [2, 3])
        self.assertTrue(verdict.startswith("ratio holds"), verdict)
        self.assertIn("under instrumentation only", verdict)
        # Update 2 is shown at sequences 13 and 14, so the run is complete with one update presented twice.
        repeated = [present_line(11, 5), present_line(12, 6), present_line(13, 7), present_line(14, 7),
                    present_line(15, 8), present_line(16, 8, sentinel=True)]
        twice = perf_attribution_report.attribution_run(armed_outcome(repeated), True)
        self.assertEqual((twice.state, twice.max_presents), ("complete", 2), twice.reasons)
        self.assertTrue(perf_attribution_report.attribution_verdict([twice], True, [3]).startswith("repeat-presentation"))
        incomplete = perf_attribution_report.attribution_run(armed_outcome(complete_lines(), end_resets=7), True)
        unavailable = perf_attribution_report.AttributionRun("unavailable", ["not-armed"])
        self.assertTrue(perf_attribution_report.attribution_verdict([complete, incomplete], True, [2]).startswith("pending"))
        self.assertEqual(perf_attribution_report.attribution_verdict([unavailable], True, [2]), "unavailable: not-armed")
        self.assertTrue(perf_attribution_report.attribution_verdict([complete], True, [9]).startswith("pending"))
        self.assertTrue(perf_attribution_report.attribution_verdict([complete], True, []).startswith("pending"))
        self.assertTrue(perf_attribution_report.attribution_verdict([complete], False, [2]).startswith("unsupported"))

    def test_an_unequal_feature_comparison_gives_rows_but_no_verdict(self):
        # The report keeps each side's runs, but only equal feature sets give a #1598 verdict.
        counters = perf_sets.SetResult("S10/sync", "counters", perf_metrics.SideRuns([armed_outcome(complete_lines())]),
                                  perf_metrics.SideRuns([armed_outcome(complete_lines())]))
        timed = perf_sets.SetResult("S10/sync", "timed", perf_metrics.SideRuns([armed_outcome(complete_lines())]),
                               perf_metrics.SideRuns([armed_outcome(complete_lines())]))
        rows, evidence = perf_attribution_report.attribution_report("S10/sync", counters, timed, comparable=False)
        self.assertEqual([row[-1] for row in rows], ["no verdict: the two refs build different perf features"] * 2)
        rows, evidence = perf_attribution_report.attribution_report("S10/sync", counters, timed, comparable=True)
        self.assertTrue(all(row[-1].startswith("ratio holds") for row in rows), rows)
        self.assertEqual(evidence["head"]["timed_excess"], [2])



PRESENT_TEXT = ("2026-10-05T20:00:00.000001Z  WARN sonic::perf_present: perf_present kind=\"present\" arming=3 "
                "seq=41 window=\"main\" pane=7 resets=3 epoch=4 set=true sentinel=true prompt=false "
                "admission=\"credit\"")


class S10AttributionConsistencyTests(unittest.TestCase):
    """A record or lines that contradict themselves are never complete and never hold the ratio."""

    def test_the_reviewers_contradictions_are_incomplete(self):
        # An open baseline, a final epoch below the baseline, resets that decrease while epochs do not, and
        # presented frames or a counter delta that disagree with the sequence range each leave the run incomplete;
        # so does a final reading below a line's state.
        lines = complete_lines()
        decreasing = [present_line(11, 5), present_line(12, 6), present_line(13, 8, epoch=8),
                      present_line(14, 7, epoch=8), present_line(15, 8, sentinel=True)]
        cases = {
            "an open baseline": armed_outcome(lines, baseline_set=True),
            "a final epoch below the baseline": armed_outcome(lines, end_epoch=4),
            "resets decreasing while epochs do not": armed_outcome(decreasing),
            "presented frames beyond the sequence": armed_outcome(lines, presented=100),
            "a counter delta beyond the sequence": armed_outcome(lines, counted=7),
            "a final reading below a line": armed_outcome(lines, end_epoch=6),
        }
        for name, outcome in cases.items():
            with self.subTest(name):
                run = perf_attribution_report.attribution_run(outcome, synchronized=True)
                self.assertEqual(run.state, "incomplete", name)
                self.assertFalse(perf_attribution_report.attribution_verdict([run], True, [2]).startswith("ratio holds"), name)

    def test_a_contradictory_record_is_a_schema_failure(self):
        # Counts are unsigned 64-bit, the baseline is closed, the final reading is not below it and the sequence
        # does not end before it starts; any other record is a schema problem.
        self.assertEqual(perf_attribution_schema.attribution_problems(attributed_result(ARMED_RECORD), True, 1), [])
        cases = {
            "an open baseline": {**ARMED_RECORD, "baseline": {"set": True, "epoch": 5, "resets": 5}},
            "a final epoch below the baseline": {**ARMED_RECORD, "end": {"set": False, "epoch": 4, "resets": 8}},
            "final resets below the baseline": {**ARMED_RECORD, "end": {"set": False, "epoch": 8, "resets": 4}},
            "a negative count": {**ARMED_RECORD, "seq_start": -1},
            "a count beyond 64 bits": {**ARMED_RECORD, "arming": 2 ** 64},
            "an end before the start": {**ARMED_RECORD, "seq_end": 9},
            "a negative reading": {**ARMED_RECORD, "baseline": {"set": False, "epoch": -1, "resets": 5}},
        }
        for name, record in cases.items():
            with self.subTest(name):
                self.assertTrue(perf_attribution_schema.attribution_problems(attributed_result(record), True, 1), name)

    def test_a_line_must_name_the_main_window_once_per_field(self):
        # A line without the window, for another window, with a field written twice, conflicting or not, or with a
        # count no `u64` holds keeps its problem, so the run reads incomplete.
        self.assertIsNone(perf_memory.parse_present_line(PRESENT_TEXT).problem)
        changed = {"no window": PRESENT_TEXT.replace(' window="main"', ""),
                   "a child window": PRESENT_TEXT.replace('window="main"', 'window="child-1"'),
                   "a conflicting field": PRESENT_TEXT + " seq=999",
                   "a repeated field": PRESENT_TEXT + " seq=41",
                   "a count beyond 64 bits": PRESENT_TEXT.replace("seq=41", f"seq={2 ** 64}")}
        for name, line in changed.items():
            with self.subTest(name):
                self.assertIsNotNone(perf_memory.parse_present_line(line).problem, name)

    def test_s10_default_keeps_its_transport_complete_or_incomplete(self):
        # S10/default has no update identity, but a frame without its line still makes it incomplete, never a
        # sound control; only complete transport reads unsupported.
        lines = complete_lines()
        gap = perf_attribution_report.attribution_run(armed_outcome(lines[:2] + lines[3:], seq_end=15), synchronized=False)
        self.assertEqual(gap.state, "incomplete")
        self.assertTrue(perf_attribution_report.attribution_verdict([gap], False, [2]).startswith("pending"))
        sound = perf_attribution_report.attribution_run(armed_outcome(lines), synchronized=False)
        self.assertEqual(sound.state, "unsupported")
        self.assertTrue(perf_attribution_report.attribution_verdict([sound], False, [2]).startswith("unsupported"))


class S10AttributionPrebuiltTests(perf_prebuilt_tests.PrebuiltHarness, unittest.TestCase):
    """A prebuilt manifest's harness cfg decision is compared exactly."""

    def test_a_manifest_cfg_value_must_be_a_list_of_names(self):
        # A number, a boolean, a bare name, null or a per-side object is refused for its type, never compared
        # loosely, so `0` can never stand for an absent cfg.
        binaries, _digest = self.produce()
        for value in ([0], [True], "perf_atlas_retry_api", None, {"base": True, "head": True}):
            with self.subTest(value=value):
                changed = self.rewrite(binaries, lambda data, value=value: data.update(harness_cfgs=value))
                with self.assertRaisesRegex(ValueError, "harness_cfgs .* is not a list of strings"):
                    self.consume(binaries, changed)


CERTIFIED = {"state": "certified", "missing_terminal": 2, "missing_chrome": 0, "atlas_width": 512,
             "atlas_height": 512, "scale": 1.0}
