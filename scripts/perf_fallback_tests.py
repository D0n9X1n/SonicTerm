"""Perf fallback tests."""

from __future__ import annotations
import contextlib
import io
from pathlib import Path
import tempfile
import time
import unittest
from unittest import mock

import perf_arguments
import perf_build_policy
import perf_delivery
import perf_driver_tests
import perf_fallback_rows
import perf_font_diagnostics
import perf_home_tests
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_result_schema
import perf_result_tests
import perf_settings



# --- Font-fallback waits in a laps run ------------------------------------------------------

# A fixed Unix second; every fallback fixture time is an offset from it.
FALLBACK_EPOCH_S = 1_790_000_000


def fallback_stamp(offset_us: int) -> str:
    """The file layer's UTC stamp for FALLBACK_EPOCH_S plus `offset_us`, with microseconds."""
    whole_s, micros = divmod(offset_us, 1_000_000)
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(FALLBACK_EPOCH_S + whole_s)) + f".{micros:06d}Z"


def font_line(offset_us: int, fields: str, spans: str = "font_shape{loaded_font_id=3 iteration=0}: ") -> str:
    """One `font operation` debug line as the file layer writes it: stamp, level, span context, target, fields."""
    return f"{fallback_stamp(offset_us)} DEBUG {spans}render_timing: font operation {fields}"


def receive_enter(offset_us: int) -> str:
    return font_line(offset_us, 'operation="fallback_receive" phase="enter"')


def receive_return(offset_us: int, elapsed_ms: float) -> str:
    return font_line(offset_us, f'operation="fallback_receive" phase="return" outcome="ok" elapsed_ms={elapsed_ms}')


def dispatch_phase(name: str, dispatches, slow=None, count=None, start_s=0.0) -> dict:
    """A phase of `(begin_s, ms)` dispatches. Its slow list holds the SLOW_DISPATCH_LIMIT longest unless `slow`
    replaces it, and its dispatch_count is exact unless `count` replaces it."""
    records = [{"start_unix_s": FALLBACK_EPOCH_S + begin_s,
                "end_unix_s": FALLBACK_EPOCH_S + begin_s + duration_ms / 1000, "ms": duration_ms}
               for begin_s, duration_ms in dispatches]
    longest = sorted(records, key=lambda record: record["ms"], reverse=True)[:perf_result_schema.SLOW_DISPATCH_LIMIT]
    end_s = max([begin_s + duration_ms / 1000 for begin_s, duration_ms in dispatches] + [start_s]) + 1.0
    return dict(perf_result_tests.valid_result()["phases"][0], name=name, start_unix_s=FALLBACK_EPOCH_S + start_s,
                end_unix_s=FALLBACK_EPOCH_S + end_s, dispatch_ms=[duration_ms for _begin_s, duration_ms in dispatches],
                slow_dispatches=longest if slow is None else slow,
                dispatch_count=len(dispatches) if count is None else count)


# The counterexample: nineteen 1,900 ms dispatches, then one 2,000 ms dispatch, three seconds apart.
COUNTEREXAMPLE = [(1.0 + 3.0 * index, 1900.0 if index < 19 else 2000.0) for index in range(20)]


def counterexample_lines(dominated) -> list[str]:
    """A 1,700 ms fallback_receive wait inside each `dominated` dispatch, entered 0.1 s after it starts."""
    lines = []
    for index in dominated:
        begin_us = int(COUNTEREXAMPLE[index][0] * 1_000_000)
        lines += [receive_enter(begin_us + 100_000), receive_return(begin_us + 1_800_000, 1700.0)]
    return lines


def counterexample_result(**phase_options) -> dict:
    return perf_result_tests.valid_result(phases=[dispatch_phase("workload", COUNTEREXAMPLE, **phase_options)])


class LapsSelectionTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    """--laps-scenario restricts the separate laps set to named, selected variants; --laps-runs sets its runs."""

    def laps_plans(self, plans):
        return sorted((plan.scenario.id, plan.variant, plan.side) for plan in plans if plan.laps)

    def test_bad_combinations_are_usage_errors(self):
        # With --laps, --laps-runs without a laps selection or below 1, and either flag on a smoke or a
        # build-only run all stop at the command line.
        comparison = ["--base", "main", "--head", "HEAD"]
        for argv in (comparison + ["--laps", "--laps-scenario", "S1"], comparison + ["--laps-runs", "2"],
                     comparison + ["--laps-scenario", "S1", "--laps-runs", "0"],
                     ["--smoke", "--laps-scenario", "S1"], ["--smoke", "--laps-runs", "1"],
                     comparison + ["--build-only", "/tmp/x", "--require-base", "--laps-scenario", "S1"]):
            with self.subTest(argv=argv), self.assertRaises(SystemExit), \
                    contextlib.redirect_stderr(io.StringIO()):
                perf_arguments.parse_args(argv)
        # --laps-runs also sets the runs of a plain --laps set.
        self.assertEqual(perf_arguments.parse_args(comparison + ["--laps", "--laps-runs", "2"]).laps_runs, 2)

    def test_a_bare_id_means_default_and_only_named_variants_run_laps(self):
        # S10 names S10/default only; S10/sync and S1 run no laps, and the laps set takes --laps-runs per side.
        # On Windows S10 also replays its delivery first, which needs the real host this fixture stubs out;
        # the replay is not under test here, so it is turned off on every platform.
        with mock.patch.object(perf_delivery, "delivery_replayed", return_value=False):
            code, _gate, _calls, plans, _work, out = self.compare(
                scenarios=("S1", "S10", "S10/sync"), options=("--laps-scenario", "S10", "--laps-runs", "2"))
        self.assertEqual(code, perf_settings.EXIT_PASS)
        self.assertEqual(self.laps_plans(plans), [("S10", "default", "base")] * 2 + [("S10", "default", "head")] * 2)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S10/default | fallback_receive verdict | inconclusive", document)

    def test_unselected_or_unknown_selections_fail(self):
        # A variant the comparison does not select, or one the listing does not have, is refused.
        for value, message in (("S10", "not selected"), ("S10/sync", "not selected"), ("S9", "unknown"),
                               ("S1/nope", "unknown")):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, message):
                self.compare(scenarios=("S1",), options=("--laps-scenario", value))

    def test_the_run_cap_applies(self):
        # A capped variant's laps set takes min(--laps-runs, cap) under --short, and every run otherwise.
        # Resolve this runtime-only dependency after module initialization.
        import perf_variants_tests
        _code, _gate, _calls, plans, _work, _out = self.compare(
            listing=perf_variants_tests.CAPPED_LISTING, scenarios=("S4",), options=("--short", "--laps-scenario", "S4", "--laps-runs", "3"))
        self.assertEqual(self.laps_plans(plans), [("S4", "default", "base"), ("S4", "default", "head")])
        _code, _gate, _calls, plans, _work, _out = self.compare(
            listing=perf_variants_tests.CAPPED_LISTING, scenarios=("S4",), options=("--laps-scenario", "S4", "--laps-runs", "3"))
        self.assertEqual(len(self.laps_plans(plans)), 6)

    def test_the_command_records_the_flags(self):
        # The details block's invocation repeats the laps selection and its runs.
        args = perf_arguments.parse_args(["--base", "main", "--head", "HEAD", "--scenario", "S9", "--laps-scenario", "S9",
                                "--laps-runs", "2"])
        self.assertIn("--laps-scenario S9 --laps-runs 2", perf_build_policy.comparison_command(args))


class FallbackLogTests(unittest.TestCase):
    """Each `font operation` record classifies once; only a malformed fallback_receive record is unparsed."""

    def test_a_production_sequence_classifies_every_record(self):
        # The span context, shape_impl, a fallback_receive pair and a worker's queue_wait and fallback_locator
        # interleave; every record classifies, nothing is unparsed and the pair matches.
        worker = 'font_shape{loaded_font_id=3 iteration=0}:font_request{request_id=7}: '
        lines = [
            font_line(0, 'operation="shape_impl" phase="enter"'),
            receive_enter(1_000),
            font_line(1_200, 'operation="queue_wait" phase="return" outcome="returned" elapsed_ms=0.2', worker),
            font_line(1_300, 'operation="fallback_locator" phase="enter"', worker),
            font_line(2_500, 'operation="fallback_locator" phase="return" outcome="ok" elapsed_ms=1.2', worker),
            receive_return(2_750, 1.75),
            font_line(3_000, 'operation="shape_impl" phase="return" outcome="ok" elapsed_ms=3.0'),
            font_line(3_100, 'operation="a_newer_operation" phase="enter"'),
            f"{fallback_stamp(3_200)} DEBUG render_timing: line=[render_timing] window=main draw=1.0ms",
        ]
        log = perf_font_diagnostics.parse_fallback_log(lines)
        self.assertEqual((log.unparsed, log.unmatched_enter, log.unmatched_return, log.entries), (0, 0, 0, 1))
        self.assertEqual([wait.elapsed_ms for wait in log.waits], [1.75])
        self.assertEqual(log.waits[0].end_unix_s, FALLBACK_EPOCH_S + 0.00275)
        self.assertEqual(log.other, {"shape_impl": 2, "queue_wait": 1, "fallback_locator": 2, "a_newer_operation": 1})

    def test_malformed_receive_records_are_unparsed(self):
        # A missing, non-numeric, NaN, infinite or negative elapsed_ms, a missing or unknown phase and an
        # unparseable stamp each add one unparsed record and no wait; a malformed shape_impl adds none.
        cases = {
            "no elapsed_ms": font_line(10, 'operation="fallback_receive" phase="return" outcome="ok"'),
            "NaN": receive_return(10, "NaN"), "inf": receive_return(10, "inf"), "negative": receive_return(10, -1.0),
            "text": receive_return(10, "soon"), "no phase": font_line(10, 'operation="fallback_receive"'),
            "unknown phase": font_line(10, 'operation="fallback_receive" phase="exit"'),
            "no stamp": "garbage DEBUG render_timing: font operation operation=\"fallback_receive\" phase=\"enter\"",
        }
        for name, line in cases.items():
            with self.subTest(name):
                log = perf_font_diagnostics.parse_fallback_log([line])
                self.assertEqual((log.unparsed, log.waits), (1, []))
        for line in (font_line(10, 'operation="shape_impl"'),
                     font_line(10, 'operation="shape_impl" phase="return" elapsed_ms=NaN')):
            self.assertEqual(perf_font_diagnostics.parse_fallback_log([line]).unparsed, 0)

    def test_pairing_counts_never_change_the_verdict(self):
        # An enter left open at the log's end and a return with no open enter are counted, and the supported
        # counterexample stays supported; an enter followed by another enter leaves the first unmatched.
        lines = [receive_return(100, 0.5)] + counterexample_lines(range(19)) + [receive_enter(70_000_000)]
        log = perf_font_diagnostics.parse_fallback_log(lines)
        self.assertEqual((log.unmatched_enter, log.unmatched_return), (1, 1))
        self.assertEqual(perf_fallback_rows.fallback_run_verdict(counterexample_result(), log).verdict, "supported")
        twice = perf_font_diagnostics.parse_fallback_log([receive_enter(1), receive_enter(2), receive_return(3, 0.001)])
        self.assertEqual((twice.unmatched_enter, twice.unmatched_return), (1, 0))

    def test_the_log_directory_is_read_in_file_order(self):
        # A run's logs are read file by file in name order, so a pair split across files still matches; a run
        # without a logs directory has no log at all.
        with tempfile.TemporaryDirectory() as temporary:
            logs = Path(temporary) / "logs"
            logs.mkdir()
            (logs / "sonicterm.log.1").write_text(receive_enter(1) + "\n", encoding="utf-8")
            (logs / "sonicterm.log.2").write_text(receive_return(5, 0.004) + "\n", encoding="utf-8")
            log = perf_font_diagnostics.read_fallback_log(logs)
            self.assertEqual((len(log.waits), log.unmatched_enter, log.unmatched_return), (1, 0, 0))
            self.assertIsNone(perf_font_diagnostics.read_fallback_log(Path(temporary) / "missing"))


class FallbackVerdictTests(unittest.TestCase):
    """A run is supported only when an examined slow dispatch's matched waits cover half of it."""

    def test_the_counterexample_is_supported(self):
        # Nineteen 1,900 ms dispatches dominated by fallback_receive beside one 2,000 ms dispatch with none:
        # p95 examines all twenty, coverage is complete and the dominated ones support the claim.
        verdict = perf_fallback_rows.fallback_run_verdict(counterexample_result(),
                                            perf_font_diagnostics.parse_fallback_log(counterexample_lines(range(19))))
        self.assertEqual((verdict.verdict, verdict.coverage), ("supported", "complete"))
        self.assertEqual((len(verdict.inside_ms), sum(verdict.inside_ms), verdict.outside_ms), (19, 19 * 1700.0, []))

    def test_truncated_waitless_or_absent_logs_are_inconclusive(self):
        # The counterexample without the records of its dominated dispatches (one wait left in a gap), a log
        # with no waits and no log at all are each inconclusive, never refuted.
        gap = [receive_enter(3_000_000), receive_return(3_500_000, 400.0)]
        verdict = perf_fallback_rows.fallback_run_verdict(counterexample_result(), perf_font_diagnostics.parse_fallback_log(gap))
        self.assertEqual((verdict.verdict, verdict.coverage, verdict.outside_ms), ("inconclusive", "complete", [400.0]))
        waitless = perf_font_diagnostics.parse_fallback_log([font_line(10, 'operation="shape_impl" phase="enter"')])
        self.assertEqual(perf_fallback_rows.fallback_run_verdict(counterexample_result(), waitless).verdict, "inconclusive")
        self.assertEqual(perf_fallback_rows.fallback_run_verdict(counterexample_result(), None).verdict, "inconclusive")
        unparsed = perf_font_diagnostics.parse_fallback_log([receive_return(10, "NaN")])
        self.assertEqual(perf_fallback_rows.fallback_run_verdict(counterexample_result(), unparsed).verdict, "inconclusive")

    def test_a_zero_length_dispatch_without_waits_is_inconclusive(self):
        # A recorded 0 ms dispatch with an empty log satisfies "matched >= half its duration" as 0 >= 0; support
        # needs matched waiting, so the run stays inconclusive, as every waitless log does.
        result = perf_result_tests.valid_result(phases=[dispatch_phase("workload", [(1.0, 0.0)])])
        verdict = perf_fallback_rows.fallback_run_verdict(result, perf_font_diagnostics.parse_fallback_log([]))
        self.assertEqual((verdict.verdict, verdict.inside_ms), ("inconclusive", []))

    def test_coverage_beyond_the_recorded_dispatches(self):
        # A hundred equal 1,900 ms dispatches leave 36 at or above p95 unrecorded: waits only in unrecorded ones are
        # inconclusive, while waits in a recorded one stay supported although coverage is incomplete. A phase
        # from a harness that records no slow dispatches has unavailable coverage.
        dispatches = [(1.0 + 3.0 * index, 1900.0) for index in range(100)]
        result = perf_result_tests.valid_result(phases=[dispatch_phase("workload", dispatches)])

        def waits_in(indices):
            lines = []
            for index in indices:
                begin_us = int(dispatches[index][0] * 1_000_000)
                lines += [receive_enter(begin_us + 100_000), receive_return(begin_us + 1_800_000, 1700.0)]
            return perf_font_diagnostics.parse_fallback_log(lines)

        unrecorded = perf_fallback_rows.fallback_run_verdict(result, waits_in(range(70, 100)))
        self.assertEqual((unrecorded.verdict, unrecorded.coverage), ("inconclusive", "incomplete"))
        recorded = perf_fallback_rows.fallback_run_verdict(result, waits_in([3]))
        self.assertEqual((recorded.verdict, recorded.coverage), ("supported", "incomplete"))
        older = perf_result_tests.valid_result(phases=[{key: value for key, value in dispatch_phase("workload", COUNTEREXAMPLE).items()
                                      if key not in ("slow_dispatches", "dispatch_count")}])
        verdict = perf_fallback_rows.fallback_run_verdict(older, perf_font_diagnostics.parse_fallback_log(counterexample_lines(range(19))))
        self.assertEqual((verdict.verdict, verdict.coverage), ("inconclusive", "unavailable"))

    def test_waits_never_match_another_run_or_phase(self):
        # One run's waits cover the other run's dispatches, so neither run, nor the side, is supported; a wait in
        # one phase's span never matches a dispatch another phase lists.
        dominated = perf_font_diagnostics.parse_fallback_log(counterexample_lines(range(19)))
        moved = perf_result_tests.valid_result(phases=[dispatch_phase("workload", [(begin_s + 1.5, duration_ms) for begin_s, duration_ms in COUNTEREXAMPLE],
                                                    start_s=0.0)])
        side = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=moved, fallback_log=dominated),
                                       perf_outcomes_tests.make_outcome(result=counterexample_result(),
                                                    fallback_log=perf_font_diagnostics.parse_fallback_log([]))])
        self.assertEqual(perf_fallback_rows.fallback_side_verdict(side), "inconclusive")
        other_phase = dispatch_phase("warmup", [(200.0, 10.0)], start_s=199.0)
        other_phase["slow_dispatches"] = dispatch_phase("workload", COUNTEREXAMPLE)["slow_dispatches"]
        empty = dispatch_phase("workload", [(1.0, 10.0)])
        crossed = perf_result_tests.valid_result(phases=[empty, other_phase])
        self.assertEqual(perf_fallback_rows.fallback_run_verdict(crossed, dominated).verdict, "inconclusive")

    def test_only_two_verdicts(self):
        # Whatever the input, a run and a side are only ever supported or inconclusive.
        self.assertEqual(perf_fallback_rows.FALLBACK_VERDICTS, ("supported", "inconclusive"))
        logs = [None, perf_font_diagnostics.parse_fallback_log([]), perf_font_diagnostics.parse_fallback_log(counterexample_lines(range(19))),
                perf_font_diagnostics.parse_fallback_log([receive_return(10, "NaN")])]
        results = [counterexample_result(), perf_result_tests.valid_result(), perf_result_tests.valid_result(phases=[]), {}]
        for result in results:
            for log in logs:
                self.assertIn(perf_fallback_rows.fallback_run_verdict(result, log).verdict, perf_fallback_rows.FALLBACK_VERDICTS)
        for side in (perf_metrics.SideRuns(), perf_metrics.SideRuns(blocked="no base"), perf_metrics.SideRuns(failed="crashed")):
            self.assertIn(perf_fallback_rows.fallback_side_verdict(side), perf_fallback_rows.FALLBACK_VERDICTS)

    def test_rows_report_waits_and_the_verdict_per_side(self):
        # Waits inside and outside examined slow dispatches, then each side's verdict with its coverage,
        # unparsed and unmatched counts beside it.
        base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=counterexample_result(),
                                                    fallback_log=perf_font_diagnostics.parse_fallback_log(counterexample_lines(range(19))))])
        head = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=counterexample_result(),
                                                    fallback_log=perf_font_diagnostics.parse_fallback_log([]))])
        cells = {row[1]: (row[2], row[3]) for row in perf_fallback_rows.fallback_rows("S9/default", base, head)}
        inside = cells["fallback_receive waits inside slow dispatches"]
        self.assertTrue(inside[0].startswith("19 waits, 32300.0 ms, max 1700.0 ms"), inside)
        self.assertTrue(inside[1].startswith("0 waits"), inside)
        verdict = cells["fallback_receive verdict"]
        self.assertTrue(verdict[0].startswith("supported"), verdict)
        self.assertTrue(verdict[1].startswith("inconclusive"), verdict)
        for fragment in ("coverage complete", "unparsed 0", "unmatched_enter 0", "unmatched_return 0"):
            self.assertIn(fragment, verdict[0])

    def test_slow_dispatches_are_validated(self):
        # A result that records slow dispatches must record them whole: each a start, an end and a duration,
        # and dispatch_count a non-negative integer.
        def problems(**fields):
            phase = dict(perf_result_tests.valid_result()["phases"][0], **fields)
            return perf_result_schema.validate_result(perf_result_tests.valid_result(phases=[phase]), perf_memory_tests.HARNESS_HASH, 0)

        self.assertEqual(problems(slow_dispatches=[{"start_unix_s": 1.0, "end_unix_s": 2.0, "ms": 1000.0}],
                                  dispatch_count=2), [])
        for fields in ({"slow_dispatches": "x"}, {"slow_dispatches": [{"start_unix_s": 1.0, "end_unix_s": 2.0}]},
                       {"dispatch_count": -1}, {"dispatch_count": 1.5}):
            with self.subTest(fields=fields):
                self.assertTrue(problems(**fields))


# An app source file defining the checkpoint-memory hook, and the manifests that declare its feature.
HOOK_SOURCE = ("impl App {\n    #[doc(hidden)]\n    pub fn __perf_checkpoint_memory(&mut self, index: usize) {}\n}\n")
HOOK_TABLE = "\n[features]\nperf-hook-checkpoint-memory = []\n"
HOOK_MANIFEST = perf_home_tests.HEAD_MANIFEST + HOOK_TABLE
