"""Perf echo tests."""

from __future__ import annotations
import hashlib
import inspect
import re
import unittest

import perf_echo_analysis
import perf_latency_schema
import perf_latency_tests
import perf_memory_tests
import perf_outcomes
import perf_sets
import perf_timeline_schema
import perf_timeline_tests


# One ordered S2/flood sample's seven parts, 10 ms in all, integer nanoseconds.
ECHO_BASE_PARTS = {"input_to_parse": 1_000_000, "parse_to_publication": 1_000_000,
                   "publication_to_frame_request": 2_000_000, "frame_request_to_entry": 2_000_000,
                   "entry_to_render": 1_000_000, "render": 2_000_000, "render_exit_to_credited_end": 1_000_000}
ECHO_HEAD_SHA, ECHO_BASE_SHA = "a" * 40, "b" * 40
ECHO_SETTINGS = {"short": True, "counters": True, "features": {"base": ["perf-counters"], "head": ["perf-counters"]},
                 "profile": {}}
ECHO_CAPABILITIES = {"latency_split_schema": 1, "phase_kinds": None, "s10_attribution": None,
                     "echo_timeline_schema": 1, "guard_correlation_schema": None}


def echo_identity(run_id, head=ECHO_HEAD_SHA, **overrides):
    """One workflow execution's run identity, as load_echo_execution reads it from run-identity.json."""
    identity = {"run_id": run_id, "run_attempt": 1, "platform": "macos", "head_sha": head,
                "base_sha": ECHO_BASE_SHA, "harness_hash": perf_memory_tests.HARNESS_HASH, "settings": ECHO_SETTINGS,
                "capabilities": ECHO_CAPABILITIES, "guard_api": None}
    identity.update(overrides)
    return identity


def control_head(name):
    """A control branch's own head: one per control, never the replay's."""
    return hashlib.sha1(name.encode()).hexdigest()


def echo_entry(flood=0, absent=0.0, tick=None, m3_complete=True, tick_qualified=True, **parts_ms):
    """An ordered schema-1 timeline for one credited S2/flood sample: ECHO_BASE_PARTS with `parts_ms` (keyword per
    part, milliseconds) replacing parts, `flood` services in its interval, the permit absent for `absent` ms of
    readiness to entry (present the rest), and its consumed tick delivered `tick` ms before the entry (default: at
    readiness; descriptive only). Every derived field is consistent, so it validates."""
    parts = dict(ECHO_BASE_PARTS)
    for name, milliseconds in parts_ms.items():
        parts[name] = round(milliseconds * 1_000_000)
    ready_to_entry = parts["frame_request_to_entry"]
    absent_ns = round(absent * 1_000_000)
    tick_to_entry = ready_to_entry if tick is None else round(tick * 1_000_000)
    tail = parts["render_exit_to_credited_end"]
    return perf_timeline_tests.timeline(parts_ns=parts, permit_present_ns=ready_to_entry - absent_ns, permit_absent_ns=absent_ns,
                    render_exit_to_dispatch_return_ns=tail, dispatch_return_to_credited_end_ns=0,
                    flood_services=flood, m3_complete=m3_complete, tick_qualified=tick_qualified,
                    tick_to_entry_ns=tick_to_entry if tick_qualified else None)


def echo_sample(entry):
    """A credited split sample carrying `entry`: its latency and split are the timeline's own sums, so the
    reader's parts-sum and split-parts rules hold; a non-ordered entry gets a 10 ms split."""
    parts = entry.get("parts_ns")
    if parts is None:
        return perf_timeline_tests.timeline_sample(entry)
    head_parts = [parts["input_to_parse"], parts["parse_to_publication"],
                  sum(parts[name] for name in perf_timeline_schema.TIMELINE_PART_NAMES[2:])]
    return perf_timeline_tests.timeline_sample(entry, latency_ms=sum(head_parts) / 1e6, split=dict(
        perf_latency_tests.split_sample()["split"], **{name: value / 1e6 for name, value in zip(perf_latency_schema.SPLIT_PARTS, head_parts)}))


def echo_run(entries, attributed=None):
    """One accepted run of samples carrying `entries`; it attributed every sample unless `attributed` says."""
    samples = [echo_sample(entry) for entry in entries]
    return perf_echo_analysis.EchoRun(samples, len(samples) if attributed is None else attributed, len(samples))


def echo_run_id(label):
    """A distinct positive decimal workflow run id derived from an execution's label, as GitHub Actions writes one."""
    return str(1000 + int(hashlib.sha1(label.encode()).hexdigest()[:8], 16))


def echo_execution(label, base_runs, head_runs, head=ECHO_HEAD_SHA, **identity):
    """One execution from per-run entry lists for each side, with a run identity derived from its label."""
    return perf_echo_analysis.EchoExecution(label, [echo_run(run) for run in base_runs], [echo_run(run) for run in head_runs],
                              identity=echo_identity(echo_run_id(label), head, **identity))


def echo_replay(base, head, second_head=None):
    """A replay of two executions, each with two base runs of `base` and two head runs of `head` (the second
    execution's head is `second_head` when given)."""
    second = head if second_head is None else second_head
    return [echo_execution("replay 1", [base, base], [head, head]),
            echo_execution("replay 2", [base, base], [second, second])]


def echo_control(name, head):
    """A control's two executions on its own branch: the replay's base against `head`."""
    base = [echo_entry()] * 5
    return [echo_execution(f"{name} {index}", [base, base], [head, head], control_head(name)) for index in (1, 2)]


ECHO_BASE = [echo_entry()] * 5
ECHO_RENDER = [echo_entry(render=6.0)] * 5


class EchoAttributionTests(unittest.TestCase):
    def outcome(self, result):
        return (result.outcome, result.step)

    def attribute(self, replay, controls=None):
        """The verdict, failing by assertion instead of erroring when the analysis raises."""
        try:
            return perf_echo_analysis.echo_attribution(replay, controls)
        except (ZeroDivisionError, KeyError, IndexError, TypeError) as error:
            self.fail(f"the analysis raised {type(error).__name__}: {error}")

    def test_every_fixture_sample_satisfies_the_reader(self):
        # The fixtures stay real schema-1 evidence: echo_timeline_problems accepts each, so no test passes on
        # samples the loader would refuse.
        for entries in (ECHO_BASE, ECHO_RENDER, [echo_entry(flood=2, absent=1.5, tick=1.0, frame_request_to_entry=5.0)],
                        [echo_entry(m3_complete=False, tick_qualified=False)], [perf_timeline_tests.timeline("split-only")]):
            latency = perf_timeline_tests.timeline_latency([echo_sample(entry) for entry in entries])
            self.assertEqual(perf_timeline_schema.echo_timeline_problems(latency, 1, True), [], entries)

    def test_a_renderer_shift_screens_m1_and_stays_unconfirmed_without_controls(self):
        # +4 ms of render in every head sample, both executions: reproduced, render is the whole Δ_total, so M1;
        # main has no controls (ruling D1), so step 4 says so.
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER))
        self.assertEqual(self.outcome(result), ("M1 renderer work", 4))
        self.assertEqual(result.confirmations, {"M1": "screened, unconfirmed (no control runs)"})

    def test_step_one_reports_its_medians_in_milliseconds(self):
        # latency_ms is already milliseconds: the 10 ms base and 14 ms head read as such, not divided again.
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER))
        self.assertIn("- step 1, replay 1: head run medians 14.000 ms, 14.000 ms; base 10.000 ms, 10.000 ms: "
                      "reproduced", result.lines)

    def test_one_execution_is_evidence_unavailable(self):
        # §4 reads two executions; one is disclosed as unavailable, never read as a decision.
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER)[:1])
        self.assertEqual(self.outcome(result), ("inconclusive (evidence unavailable)", 0))
        self.assertIn("- 1 replay execution(s); §4 reads two", result.lines)

    def test_an_execution_without_identity_is_evidence_unavailable(self):
        # Evidence with no run identity cannot be shown to be a distinct execution of this comparison.
        replay = echo_replay(ECHO_BASE, ECHO_RENDER)
        replay[0].identity = {}
        result = self.attribute(replay)
        self.assertEqual(self.outcome(result), ("inconclusive (evidence unavailable)", 0))
        self.assertTrue(any(line.startswith("- replay 1: no execution identity") for line in result.lines))

    def test_one_workflow_run_read_twice_is_not_two_executions(self):
        # The same run id under two labels is one execution, whatever its directory was called.
        replay = echo_replay(ECHO_BASE, ECHO_RENDER)
        replay[1].identity = dict(replay[0].identity)
        result = self.attribute(replay)
        self.assertEqual(self.outcome(result), ("inconclusive (evidence unavailable)", 0))
        self.assertIn(f"- replay 2: workflow run {echo_run_id('replay 1')} is already read as replay 1", result.lines)

    def test_an_identity_that_names_no_workflow_execution_is_evidence_unavailable(self):
        # A blank, malformed or zero-padded run id, attempt 0, or a non-boolean guard_api names no workflow
        # execution; each is refused before uniqueness and pairing read it, while valid distinct ids pass.
        for key, value, expected in (("run_id", "", "run_id '' is not a workflow run id"),
                                     ("run_id", "12a", "run_id '12a' is not a workflow run id"),
                                     ("run_id", "0700", "run_id '0700' is not a workflow run id"),
                                     ("run_id", 700, "run_id 700 is not a workflow run id"),
                                     ("run_attempt", 0, "run_attempt 0 is not a workflow run attempt"),
                                     ("run_attempt", True, "run_attempt True is not a workflow run attempt"),
                                     ("guard_api", 1, "guard_api 1 is neither null nor a boolean")):
            with self.subTest(key=key, value=value):
                replay = echo_replay(ECHO_BASE, ECHO_RENDER)
                replay[1].identity[key] = value
                self.assertIn(f"replay 2: {expected}", perf_echo_analysis.echo_identity_problems(replay))
        self.assertEqual(perf_echo_analysis.echo_identity_problems(echo_replay(ECHO_BASE, ECHO_RENDER)), [])

    def test_guard_api_must_match_across_executions_and_controls(self):
        # guard_api changes what the harness instruments: a true/false or null/boolean mismatch between the
        # replay executions, or between a control and the replay, is not one comparison; a shared null is.
        for first, second in ((True, False), (None, True), (False, None)):
            with self.subTest(first=first, second=second):
                replay = echo_replay(ECHO_BASE, ECHO_RENDER)
                replay[0].identity["guard_api"], replay[1].identity["guard_api"] = first, second
                self.assertIn("replay 2: guard_api differs from replay 1's", perf_echo_analysis.echo_identity_problems(replay))
                controls = {"C2": echo_control("C2", [echo_entry(render=4.0)] * 5)}
                for execution in [*replay, *controls["C2"]]:
                    execution.identity["guard_api"] = first
                controls["C2"][1].identity["guard_api"] = second
                self.assertEqual(perf_echo_analysis.echo_identity_problems(replay, controls),
                                 ["C2 2: guard_api differs from replay 1's"])

    def test_replay_executions_must_measure_one_comparison(self):
        # Two executions of different heads, harnesses or platforms are not one replay.
        for key, value in (("head_sha", "c" * 40), ("harness_hash", "cd" * 32), ("platform", "windows"),
                           ("settings", dict(ECHO_SETTINGS, short=False))):
            with self.subTest(key=key):
                replay = echo_replay(ECHO_BASE, ECHO_RENDER)
                replay[1].identity[key] = value
                result = self.attribute(replay)
                self.assertEqual(self.outcome(result), ("inconclusive (evidence unavailable)", 0))
                self.assertIn(f"- replay 2: {key} differs from replay 1's", result.lines)

    def test_a_control_must_be_paired_with_the_replay(self):
        # A control is its own branch against the replay's base: another base, the replay's head, a head it shares
        # with another control, or two heads of its own are not paired evidence.
        cases = []
        controls = {"C2": echo_control("C2", [echo_entry(render=4.0)] * 5)}
        controls["C2"][0].identity["base_sha"] = "c" * 40
        cases.append((controls, "- C2 1: base_sha differs from replay 1's"))
        controls = {"C2": [echo_execution(f"C2 {index}", [ECHO_BASE] * 2, [ECHO_RENDER] * 2) for index in (1, 2)]}
        cases.append((controls, "- C2: its head is the replay's head, not a control branch"))
        controls = {"C1": echo_control("C1", ECHO_RENDER),
                    "C2": [echo_execution(f"C2 {index}", [ECHO_BASE] * 2, [ECHO_RENDER] * 2, control_head("C1"))
                           for index in (1, 2)]}
        cases.append((controls, "- C2: its head is also C1's"))
        controls = {"C2": echo_control("C2", ECHO_RENDER)}
        controls["C2"][1].identity["head_sha"] = "d" * 40
        cases.append((controls, "- C2: its executions measured 2 heads, not one"))
        for controls, expected in cases:
            with self.subTest(expected=expected):
                result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), controls)
                self.assertEqual(self.outcome(result), ("inconclusive (evidence unavailable)", 0))
                self.assertIn(expected, result.lines)

    def test_a_side_without_runs_or_credited_samples_is_evidence_unavailable(self):
        # An empty side and a run with no credited sample are named, not counted as zero.
        replay = echo_replay(ECHO_BASE, ECHO_RENDER)
        replay[0].head = []
        replay[1].base[0] = perf_echo_analysis.EchoRun([perf_latency_tests.split_sample(latency_ms=None, split_reason=perf_latency_schema.NOT_CREDITED)], 0, 1)
        result = self.attribute(replay)
        self.assertEqual(self.outcome(result), ("inconclusive (evidence unavailable)", 0))
        self.assertIn("- replay 1 head: no accepted S2/flood counters run", result.lines)
        self.assertIn("- replay 2 base run 1: no credited sample", result.lines)

    def test_a_blocked_side_is_named_as_such(self):
        # A side the comparison ended blocked accepted no run; its status is named, not read as an empty side.
        replay = echo_replay(ECHO_BASE, ECHO_RENDER)
        replay[0].head, replay[0].statuses = [], {"base": "", "head": "blocked"}
        result = self.attribute(replay)
        self.assertIn("- replay 1 head: the comparison ended 'blocked', so it accepted no run", result.lines)

    def test_not_reproduced_in_one_execution_is_inconclusive(self):
        # Every head run's median must exceed every base run's in both executions; a tie in the second is not.
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER, second_head=ECHO_BASE))
        self.assertEqual(self.outcome(result), ("inconclusive (not reproduced under instrumentation)", 1))
        self.assertTrue(any(line.endswith("not reproduced") for line in result.lines))

    def test_one_slow_head_run_is_not_reproduction(self):
        # One head run above every base run is not enough: the other head run at the base median fails step 1.
        replay = echo_replay(ECHO_BASE, ECHO_RENDER)
        replay[1].head[1] = echo_run(ECHO_BASE)
        self.assertEqual(self.outcome(self.attribute(replay)),
                         ("inconclusive (not reproduced under instrumentation)", 1))

    def test_an_unavailable_timeline_side_is_evidence_unavailable_not_zero(self):
        # A stub base records nothing: its credited latency still reproduces, but no part can be compared.
        stub = [perf_timeline_tests.timeline("unavailable", unavailable_reason="not-recorded")] * 5
        result = self.attribute(echo_replay(stub, ECHO_RENDER))
        self.assertEqual(self.outcome(result), ("inconclusive (evidence unavailable)", 2))
        self.assertIn("- step 2: timeline unavailable on replay 1 base, replay 2 base (recorded nothing)",
                      result.lines)

    def test_the_ordered_gate_is_exact_at_eighty_percent(self):
        # Four ordered samples of five on each side is exactly 80%: the analysis proceeds. Three of four is not.
        split_only = perf_timeline_tests.timeline("split-only")
        passing = self.attribute(echo_replay([echo_entry()] * 4 + [split_only],
                                             [echo_entry(render=6.0)] * 4 + [split_only]))
        self.assertEqual(self.outcome(passing), ("M1 renderer work", 4))
        failing = self.attribute(echo_replay([echo_entry()] * 3 + [split_only],
                                             [echo_entry(render=6.0)] * 3 + [split_only]))
        self.assertEqual(self.outcome(failing), ("inconclusive (population coverage)", 2))

    def test_the_ordered_gap_is_exact_at_ten_points(self):
        # 100% base against 90% head is a 10-point gap and passes; against 80% it is 20 points and fails.
        split_only = perf_timeline_tests.timeline("split-only")
        within = self.attribute(echo_replay([echo_entry()] * 10, [echo_entry(render=6.0)] * 9 + [split_only]))
        self.assertEqual(self.outcome(within), ("M1 renderer work", 4))
        beyond = self.attribute(echo_replay([echo_entry()] * 10, [echo_entry(render=6.0)] * 8 + [split_only] * 2))
        self.assertEqual(self.outcome(beyond), ("inconclusive (population coverage)", 2))

    def test_keypress_attribution_below_eighty_percent_fails_the_gate(self):
        # The existing keypress-attribution gate also applies: 3 of 4 samples attributed on a side fails it.
        replay = echo_replay(ECHO_BASE, ECHO_RENDER)
        for execution in replay:
            execution.base = [perf_echo_analysis.EchoRun(run.samples, 3, 4) for run in execution.base]
        self.assertEqual(self.outcome(self.attribute(replay)), ("inconclusive (population coverage)", 2))

    def test_a_median_rise_with_a_mean_fall_is_mean_and_median_disagree(self):
        # Head latencies 11, 11, 11, 9 and 8 ms: the median rises to 11 ms but the mean stays at 10 ms, so
        # Δ_total = 0 and the procedure stops; exactly zero is on the inconclusive side.
        head = [echo_entry(render=3.0)] * 3 + [echo_entry(render=1.0), echo_entry(render=0.0)]
        result = self.attribute(echo_replay(ECHO_BASE, head))
        self.assertEqual(self.outcome(result), ("inconclusive (mean and median disagree)", 2))

    def test_a_spread_shift_is_distributed(self):
        # +1 ms on three parts: each s_I is a third, so no part is primary.
        head = [echo_entry(render=3.0, entry_to_render=2.0, publication_to_frame_request=3.0)] * 5
        self.assertEqual(self.outcome(self.attribute(echo_replay(ECHO_BASE, head))),
                         ("distributed / inconclusive", 2))

    def test_a_share_of_exactly_one_half_is_primary(self):
        # +2 ms of render and +2 ms of entry_to_render: each s_I is exactly 0.5, so both are primary; render
        # screens M1 and entry_to_render, with no screen, is named.
        result = self.attribute(echo_replay(ECHO_BASE, [echo_entry(render=4.0, entry_to_render=3.0)] * 5))
        self.assertEqual(self.outcome(result), ("M1 renderer work", 4))
        self.assertIn("- step 3: primary entry_to_render has no mechanism screen", result.lines)

    def test_a_share_one_nanosecond_below_one_half_is_not_primary(self):
        # One nanosecond more of entry_to_render puts render's exact share below 0.5; only the unscreened part is
        # primary, so no mechanism is screened.
        head = [echo_entry(render=4.0, entry_to_render=3.000001)] * 5
        self.assertEqual(self.outcome(self.attribute(echo_replay(ECHO_BASE, head))),
                         ("inconclusive (no mechanism screened)", 3))

    def test_the_same_part_must_be_primary_in_both_executions(self):
        # Render is the whole Δ in the first execution and publication_to_frame_request in the second.
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER,
                                            second_head=[echo_entry(publication_to_frame_request=6.0)] * 5))
        self.assertEqual(self.outcome(result), ("distributed / inconclusive", 2))

    def test_a_part_primary_in_one_execution_only_is_not_primary(self):
        # Render is the whole Δ in the first execution but 40% in the second, with a rising median in both.
        second = [echo_entry(render=4.0, publication_to_frame_request=5.0)] * 5
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER, second_head=second))
        self.assertEqual(self.outcome(result), ("distributed / inconclusive", 2))

    def test_a_primary_part_whose_median_shift_disagrees_is_not_primary(self):
        # Render's mean rises by 1.4 ms (s_I above 0.5) while its median falls by 0.5 ms; the median shift must
        # share Δ_I's sign, so render is not primary. +1 ms of input_to_parse keeps the latency median rising.
        head = ([echo_entry(input_to_parse=2.0, render=1.5)] * 3
                + [echo_entry(input_to_parse=2.0, render=6.25)] * 2)
        self.assertEqual(self.outcome(self.attribute(echo_replay(ECHO_BASE, head))),
                         ("distributed / inconclusive", 2))

    def test_a_readiness_shift_screens_m2(self):
        result = self.attribute(echo_replay(ECHO_BASE, [echo_entry(publication_to_frame_request=6.0)] * 5))
        self.assertEqual(self.outcome(result), ("M2 upstream readiness", 4))

    def test_a_flood_rise_of_exactly_one_screens_m3(self):
        # frame_request_to_entry +4 ms with one flood service per head sample against none: the rise is exactly
        # 1.0, so M3 holds; the permit is never absent, so M4 does not.
        head = [echo_entry(flood=1, frame_request_to_entry=6.0)] * 5
        result = self.attribute(echo_replay(ECHO_BASE, head))
        self.assertEqual(self.outcome(result), ("M3 flood competition", 4))
        self.assertEqual(result.confirmations, {"M3": "screened, unconfirmed (no control runs)"})

    def test_a_flood_rise_below_one_is_not_m3(self):
        head = [echo_entry(flood=1, frame_request_to_entry=6.0)] * 4 + [echo_entry(frame_request_to_entry=6.0)]
        result = self.attribute(echo_replay(ECHO_BASE, head))
        self.assertEqual(self.outcome(result), ("inconclusive (no mechanism screened)", 3))
        self.assertTrue(any("rise +0.800 per sample" in line for line in result.lines))

    def test_a_permit_absence_of_exactly_half_screens_m4(self):
        # frame_request_to_entry +4 ms, of which the permit is absent for exactly 2 ms, 50% (v6 X3, v6.1 B2): M4.
        head = [echo_entry(frame_request_to_entry=6.0, absent=2.0)] * 5
        result = self.attribute(echo_replay(ECHO_BASE, head))
        self.assertEqual(self.outcome(result), ("M4 pacing", 4))

    def test_a_permit_absence_below_half_is_not_m4(self):
        head = [echo_entry(frame_request_to_entry=6.0, absent=1.999999)] * 5
        self.assertEqual(self.outcome(self.attribute(echo_replay(ECHO_BASE, head))),
                         ("inconclusive (no mechanism screened)", 3))

    def test_a_permit_held_throughout_and_replaced_is_not_pacing(self):
        # A permit held from readiness to entry and replaced by the later consumed tick: the tick arrived 2 ms after
        # readiness, but no permit was ever absent, so the wait is not pacing. tick_to_entry is descriptive only.
        head = [echo_entry(frame_request_to_entry=6.0, tick=4.0)] * 5
        result = self.attribute(echo_replay(ECHO_BASE, head))
        self.assertEqual(self.outcome(result), ("inconclusive (no mechanism screened)", 3))
        self.assertTrue(any("permit-absent Δ +0.000 ms of frame_request_to_entry Δ +4.000 ms" in line
                            for line in result.lines), result.lines)

    def test_a_permit_invalidated_then_reacquired_is_pacing(self):
        # The permit held at readiness is invalidated and a new tick is stored 3 ms later: 3 ms of absence in a 4 ms
        # rise screens M4, though that tick was delivered only 1 ms before entry.
        head = [echo_entry(frame_request_to_entry=6.0, absent=3.0, tick=1.0)] * 5
        result = self.attribute(echo_replay(ECHO_BASE, head))
        self.assertEqual(self.outcome(result), ("M4 pacing", 4))

    def test_m4_does_not_hold_when_m3_does(self):
        # Both the flood rise and the permit absence hold; M3's condition excludes M4, so only M3 is named.
        head = [echo_entry(flood=1, frame_request_to_entry=6.0, absent=2.0)] * 5
        result = self.attribute(echo_replay(ECHO_BASE, head))
        self.assertEqual(self.outcome(result), ("M3 flood competition", 4))
        self.assertIn("- step 3 M4, replay 1: M3's condition holds there, so M4 does not", result.lines)

    def test_m3_true_in_one_execution_excludes_m4_there(self):
        # The flood rise holds in the first execution only: M3 is not screened (not both), and M4 is not either,
        # because its condition that M3 is false fails in the first execution. With C4 given, nothing is attributed.
        head = [echo_entry(flood=1, frame_request_to_entry=6.0, absent=2.0)] * 5
        second = [echo_entry(frame_request_to_entry=6.0, absent=2.0)] * 5
        controls = {"C4": echo_control("C4", [echo_entry(frame_request_to_entry=4.0)] * 5)}
        result = self.attribute(echo_replay(ECHO_BASE, head, second_head=second), controls)
        self.assertEqual(self.outcome(result), ("inconclusive (no mechanism screened)", 3))
        self.assertEqual(result.confirmations, {})

    def test_m3_unavailable_in_one_execution_leaves_m4_unevaluated_there(self):
        # The first execution's flood intervals are too incomplete for M3; M4 cannot be evaluated there, so it is
        # not screened even though the second execution would screen it.
        incomplete = echo_entry(m3_complete=False)
        head_incomplete = echo_entry(m3_complete=False, frame_request_to_entry=6.0, absent=2.0)
        first = echo_execution("replay 1", [[echo_entry()] * 3 + [incomplete]] * 2,
                               [[echo_entry(frame_request_to_entry=6.0, absent=2.0)] * 3 + [head_incomplete]] * 2)
        second = echo_execution("replay 2", [ECHO_BASE] * 2, [[echo_entry(frame_request_to_entry=6.0, absent=2.0)] * 5] * 2)
        result = self.attribute([first, second])
        self.assertEqual(self.outcome(result), ("inconclusive (no mechanism screened)", 3))
        self.assertIn("- step 3 M4, replay 1: not evaluated: M3 is unavailable there, so its condition cannot be "
                      "shown not to hold", result.lines)

    def test_the_tick_qualified_gate_is_exact_at_eighty_percent_of_ordered(self):
        # M4 reads ordered ∩ tick_qualified: four of five per side is exactly 80% of ordered and M4 holds on the
        # qualified samples; three of four is below, so M4 is unavailable, never screened from fewer.
        unqualified = echo_entry(tick_qualified=False)
        head_unqualified = echo_entry(tick_qualified=False, frame_request_to_entry=6.0, absent=2.0)
        passing = self.attribute(echo_replay([echo_entry()] * 4 + [unqualified],
                                             [echo_entry(frame_request_to_entry=6.0, absent=2.0)] * 4 + [head_unqualified]))
        self.assertEqual(self.outcome(passing), ("M4 pacing", 4))
        failing = self.attribute(echo_replay([echo_entry()] * 3 + [unqualified],
                                             [echo_entry(frame_request_to_entry=6.0, absent=2.0)] * 3 + [head_unqualified]))
        self.assertEqual(self.outcome(failing), ("inconclusive (no mechanism screened)", 3))
        self.assertTrue(any("M4, replay 1: unavailable: tick-qualified 6/8 base" in line for line in failing.lines))

    def test_an_unavailable_m3_population_is_not_a_zero_and_blocks_m4(self):
        # Three of four samples with a complete flood interval is below §2's gate in both executions.
        incomplete = echo_entry(m3_complete=False)
        head_incomplete = echo_entry(m3_complete=False, frame_request_to_entry=6.0, absent=2.0)
        result = self.attribute(echo_replay([echo_entry()] * 3 + [incomplete],
                                            [echo_entry(frame_request_to_entry=6.0, absent=2.0)] * 3 + [head_incomplete]))
        self.assertEqual(self.outcome(result), ("inconclusive (no mechanism screened)", 3))
        self.assertTrue(any("M3, replay 1: unavailable: ordered ∩ m3_complete 6/8" in line for line in result.lines))

    def test_the_m3_gate_is_exact_at_eighty_percent_of_credited(self):
        head = [echo_entry(flood=1, frame_request_to_entry=6.0)] * 4 + [
            echo_entry(flood=1, m3_complete=False, frame_request_to_entry=6.0)]
        result = self.attribute(echo_replay([echo_entry()] * 4 + [echo_entry(m3_complete=False)], head))
        self.assertEqual(self.outcome(result), ("M3 flood competition", 4))

    def test_two_screened_mechanisms_are_combined_and_each_named(self):
        result = self.attribute(echo_replay(ECHO_BASE, [echo_entry(render=4.0,
                                                                   publication_to_frame_request=4.0)] * 5))
        self.assertEqual(self.outcome(result), ("combined (M1 renderer work, M2 upstream readiness)", 4))
        self.assertEqual(set(result.confirmations), {"M1", "M2"})

    def test_a_control_reducing_both_shifts_by_exactly_half_attributes_unverified(self):
        # C2 leaves +2 ms of render against the replay's +4 ms: R_C is exactly 0.5 for the part and the total
        # median shift, in both executions. A named control is not verified, so the attribution says so.
        controls = {"C2": echo_control("C2", [echo_entry(render=4.0)] * 5)}
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), controls)
        self.assertEqual(result.confirmations, {"M1": "attributed (C2, control identity unverified)"})
        self.assertIn("control identity is not verified", perf_echo_analysis.format_echo_attribution(result))

    def test_a_control_reducing_less_than_half_is_unconfirmed(self):
        controls = {"C2": echo_control("C2", [echo_entry(render=4.000001)] * 5)}
        self.assertEqual(self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), controls).confirmations,
                         {"M1": "screened, unconfirmed"})

    def test_a_control_must_hold_in_both_executions(self):
        controls = {"C2": echo_control("C2", [echo_entry(render=4.0)] * 5)}
        controls["C2"][1] = echo_execution("C2 2", [ECHO_BASE, ECHO_BASE], [ECHO_RENDER, ECHO_RENDER], control_head("C2"))
        self.assertEqual(self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), controls).confirmations,
                         {"M1": "screened, unconfirmed"})

    def test_a_control_must_reduce_the_total_median_shift_too(self):
        head = [echo_entry(render=5.5)] * 3 + [echo_entry(render=1.75)] * 2
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), {"C2": echo_control("C2", head)})
        self.assertEqual(result.confirmations, {"M1": "screened, unconfirmed"})
        self.assertTrue(any("R_C 50.0% for render, 12.5% for the total median shift" in line
                            for line in result.lines))

    def test_a_control_given_once_cannot_confirm(self):
        controls = {"C2": echo_control("C2", [echo_entry(render=4.0)] * 5)[:1]}
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), controls)
        self.assertEqual(result.confirmations, {"M1": "screened, unconfirmed"})
        self.assertIn("- step 4 C2: 1 execution(s), not two, so it cannot confirm", result.lines)

    def test_a_control_for_another_mechanism_leaves_this_one_unconfirmed(self):
        controls = {"C4": echo_control("C4", [echo_entry(render=4.0)] * 5)}
        self.assertEqual(self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), controls).confirmations,
                         {"M1": "screened, unconfirmed (no control runs)"})

    def test_c4_attributes_m4_on_the_ordered_population(self):
        # C4 removes the permit's absence: frame_request_to_entry +2 ms against the replay's +4 ms, so R_C is 0.5.
        replay = echo_replay(ECHO_BASE, [echo_entry(frame_request_to_entry=6.0, absent=2.0)] * 5)
        controls = {"C4": echo_control("C4", [echo_entry(frame_request_to_entry=4.0)] * 5)}
        self.assertEqual(self.attribute(replay, controls).confirmations,
                         {"M4": "attributed (C4, control identity unverified)"})

    def test_a_control_below_the_ordered_gate_cannot_confirm(self):
        # The control's head orders three of four samples (6 of 8 over its two runs), below §2's gate.
        head = [echo_entry(render=4.0)] * 3 + [perf_timeline_tests.timeline("split-only")]
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), {"C2": echo_control("C2", head)})
        self.assertEqual(result.confirmations, {"M1": "screened, unconfirmed"})
        self.assertTrue(any("ordered 10/10 base, 6/8 head, below §2's gates" in line for line in result.lines),
                        result.lines)

    def test_a_control_below_the_keypress_gate_cannot_confirm(self):
        # The control's head credits 5 of 50 keypresses (10%): its ordered coverage is full, but the keypress gate
        # fails, so its arithmetic is not evidence, however well it halves the shift.
        controls = {"C2": echo_control("C2", [echo_entry(render=4.0)] * 5)}
        for execution in controls["C2"]:
            execution.head = [perf_echo_analysis.EchoRun(run.samples, 5, 50) for run in execution.head]
        result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), controls)
        self.assertEqual(result.confirmations, {"M1": "screened, unconfirmed"})
        self.assertTrue(any("keypress attribution 10/10 base, 10/100 head" in line for line in result.lines),
                        result.lines)

    def test_a_control_without_evidence_is_named_never_a_crash(self):
        # An empty control side, all-unavailable timelines, and all split-only samples are each unavailable or
        # ungated, named, and never reach a mean of nothing.
        unavailable = [perf_timeline_tests.timeline("unavailable", unavailable_reason="not-recorded")] * 5
        cases = {"empty": None, "unavailable": unavailable, "split-only": [perf_timeline_tests.timeline("split-only")] * 5}
        for case, head in cases.items():
            with self.subTest(case=case):
                controls = {"C2": echo_control("C2", head or ECHO_BASE)}
                if head is None:
                    for execution in controls["C2"]:
                        execution.head = []
                result = self.attribute(echo_replay(ECHO_BASE, ECHO_RENDER), controls)
                self.assertEqual(result.confirmations, {"M1": "screened, unconfirmed"})
                expected = {"empty": "unavailable: C2 1 head: no accepted S2/flood counters run",
                            "unavailable": "unavailable: C2 1 head: no recorded timeline",
                            "split-only": "ordered 10/10 base, 0/10 head, below §2's gates"}[case]
                self.assertTrue(any(expected in line for line in result.lines), (expected, result.lines))

    # M3's head: three busy samples, frame_request_to_entry 7 ms with two flood services, and two quiet ones at
    # 3 ms with none. Mean rise 1.2 services and +3.4 ms of the part (the whole Δ).
    M3_HEAD = [echo_entry(flood=2, frame_request_to_entry=7.0)] * 3 + [
        echo_entry(flood=0, frame_request_to_entry=3.0)] * 2

    def test_m3_with_c5_and_the_stratified_check_is_supporting_evidence_only(self):
        controls = {"C5": echo_control("C5", [echo_entry(flood=2, frame_request_to_entry=4.0)] * 3
                                       + [echo_entry(frame_request_to_entry=3.0)] * 2)}
        result = self.attribute(echo_replay(ECHO_BASE, self.M3_HEAD), controls)
        self.assertEqual(self.outcome(result), ("M3 flood competition", 4))
        self.assertEqual(result.confirmations["M3"], "supporting evidence only (C5, a broad control, with the "
                                                     "stratified check; control identity unverified); "
                                                     "screened, unconfirmed")
        self.assertTrue(any("median 3.000 ms with no flood service against 7.000 ms with one or more: lower" in line
                            for line in result.lines))

    def test_m3_without_controls_still_reports_the_stratified_check(self):
        result = self.attribute(echo_replay(ECHO_BASE, self.M3_HEAD))
        self.assertEqual(result.confirmations.get("M3"), "screened, unconfirmed (no control runs)")
        self.assertEqual(sum("M3 stratified" in line for line in result.lines), 4)

    def test_m3_whose_stratified_check_fails_is_unconfirmed_even_with_c5(self):
        head = [echo_entry(flood=2, frame_request_to_entry=5.0)] * 3 + [
            echo_entry(flood=0, frame_request_to_entry=8.0)] * 2
        controls = {"C5": echo_control("C5", [echo_entry(flood=2, frame_request_to_entry=3.0)] * 5)}
        result = self.attribute(echo_replay(ECHO_BASE, head), controls)
        self.assertEqual(self.outcome(result), ("M3 flood competition", 4))
        self.assertEqual(result.confirmations.get("M3"), "screened, unconfirmed")
        self.assertTrue(any(line.endswith(": not lower") for line in result.lines))

    def test_equal_strata_medians_do_not_support_m3(self):
        head = [echo_entry(flood=2, frame_request_to_entry=5.0)] * 3 + [
            echo_entry(flood=0, frame_request_to_entry=5.0)] * 2
        controls = {"C5": echo_control("C5", [echo_entry(flood=2, frame_request_to_entry=3.5)] * 5)}
        result = self.attribute(echo_replay(ECHO_BASE, head), controls)
        self.assertEqual(self.outcome(result), ("M3 flood competition", 4))
        self.assertEqual(result.confirmations.get("M3"), "screened, unconfirmed")
        self.assertTrue(any("R_C 50.0% for frame_request_to_entry" in line for line in result.lines), result.lines)

    def test_an_empty_stratum_leaves_the_stratified_check_unavailable(self):
        head = [echo_entry(flood=1, frame_request_to_entry=6.0)] * 5
        controls = {"C5": echo_control("C5", [echo_entry(flood=1, frame_request_to_entry=4.0)] * 5)}
        result = self.attribute(echo_replay(ECHO_BASE, head), controls)
        self.assertEqual(self.outcome(result), ("M3 flood competition", 4))
        self.assertEqual(result.confirmations.get("M3"), "screened, unconfirmed")
        self.assertTrue(any("unavailable (0 with no flood service, 5 with one or more)" in line
                            for line in result.lines))

    def test_a_published_during_dispatch_shift_beyond_ten_points_is_named(self):
        during = perf_timeline_tests.timeline("during-dispatch")
        beyond = self.attribute(echo_replay([echo_entry()] * 10, [echo_entry(render=6.0)] * 8 + [during] * 2))
        self.assertTrue(any("population shift, replay 1" in line for line in beyond.lines))
        within = self.attribute(echo_replay([echo_entry()] * 10, [echo_entry(render=6.0)] * 9 + [during]))
        self.assertFalse(any("population shift" in line for line in within.lines))

    def test_every_classification_kind_perf_compare_writes_is_known(self):
        # The loader refuses any kind outside ECHO_CLASSIFICATION_KINDS, so every kind classify_outcome returns or
        # run_set assigns must be in it, or a real rejection would be refused as malformed.
        source = inspect.getsource(perf_outcomes.classify_outcome) + inspect.getsource(perf_sets.run_set)
        written = set(re.findall(r'return "([a-z-]+)", \[', source)) | set(re.findall(r'kind, why = "([a-z-]+)"',
                                                                                     source))
        self.assertTrue(written and written <= set(perf_echo_analysis.ECHO_CLASSIFICATION_KINDS),
                        sorted(written - set(perf_echo_analysis.ECHO_CLASSIFICATION_KINDS)))
