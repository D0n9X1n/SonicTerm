#!/usr/bin/env python3
"""Contracts of the performance comparison script, driven entirely by fakes.

No test builds, launches or signals a real process: the process table, the
`lsappinfo` and `footprint` commands, the gate's `run_step` and the clock are
replaced, so the suite runs unchanged on macOS, Windows and Linux.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "perf_compare", Path(__file__).with_name("perf-compare.py")
)
assert SPEC and SPEC.loader
perf = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = perf
SPEC.loader.exec_module(perf)


class StatisticsTests(unittest.TestCase):
    def test_median_of_odd_and_even_counts(self):
        # The median is the middle value, or the mean of the two middle values.
        self.assertEqual(perf.median([3.0, 1.0, 2.0]), 2.0)
        self.assertEqual(perf.median([4.0, 1.0, 3.0, 2.0]), 2.5)

    def test_nearest_rank_p95_takes_an_observed_sample(self):
        # Nearest rank is ceil(0.95 * n) and never interpolates between samples.
        self.assertEqual(perf.percentile_95([float(value) for value in range(1, 21)]), 19.0)
        self.assertEqual(perf.percentile_95([float(value) for value in range(1, 11)]), 10.0)
        self.assertEqual(perf.percentile_95([float(value) for value in range(1, 101)]), 95.0)
        self.assertEqual(perf.percentile_95([7.0]), 7.0)

    def test_empty_samples_have_no_statistic(self):
        # An empty sample set reports no figure instead of inventing a zero.
        with self.assertRaises(ValueError):
            perf.median([])
        with self.assertRaises(ValueError):
            perf.percentile_95([])

    def test_frame_metrics_pool_samples_and_spread_comes_from_runs(self):
        # Pooled median and p95 cover every frame; the noise floor is the per-run spread.
        summary = perf.frame_summary([[1.0, 2.0, 3.0], [10.0, 20.0, 30.0, 40.0]])
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
        summary = perf.frame_summary([[], [5.0, 7.0]])
        self.assertEqual(summary.runs, 1)
        self.assertEqual(summary.run_median_range, (6.0, 6.0))
        self.assertIsNone(perf.frame_summary([[], []]))

    def test_run_level_metrics_report_median_and_range(self):
        # Run-level metrics report the median and min-max of one value per run.
        summary = perf.run_summary([5.0, None, 3.0, 4.0])
        self.assertEqual((summary.median, summary.minimum, summary.maximum), (4.0, 3.0, 5.0))
        self.assertEqual(summary.runs, 3)
        self.assertIsNone(perf.run_summary([None, None]))


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
        schedule = perf.AbbaSchedule(target_runs=5, retry_limit=3)
        order = self.drive(schedule, {"base": [True] * 5, "head": [True] * 5})
        self.assertEqual(order, ["base", "head", "head", "base"] * 2 + ["base", "head"])
        self.assertTrue(schedule.complete)
        self.assertIsNone(schedule.failed_side)

    def test_invalid_run_is_retried_in_a_later_slot(self):
        # An invalid run does not count; its side gets another run in its next ABBA slot.
        schedule = perf.AbbaSchedule(target_runs=2, retry_limit=3)
        order = self.drive(schedule, {"base": [False, True, True], "head": [True, True]})
        self.assertEqual(order, ["base", "head", "head", "base", "base"])
        self.assertEqual(schedule.valid_runs, {"base": 2, "head": 2})
        self.assertEqual(schedule.invalid_runs, {"base": 1, "head": 0})

    def test_a_finished_side_gives_its_slots_to_the_other(self):
        # Once one side has its valid runs, the other keeps running until it has its own.
        schedule = perf.AbbaSchedule(target_runs=1, retry_limit=3)
        order = self.drive(schedule, {"base": [True], "head": [False, True]})
        self.assertEqual(order, ["base", "head", "head"])
        self.assertTrue(schedule.complete)

    def test_retries_stop_after_three_per_side(self):
        # A fourth invalid run on one side ends the scenario and names that side.
        schedule = perf.AbbaSchedule(target_runs=1, retry_limit=3)
        order = self.drive(schedule, {"base": [False] * 4, "head": [True]})
        self.assertEqual(order, ["base", "head", "base", "base", "base"])
        self.assertEqual(schedule.failed_side, "base")
        self.assertFalse(schedule.complete)
        self.assertIsNone(schedule.next_side())

    def test_retry_limit_counts_each_side_separately(self):
        # Three retries on each side still let both sides reach their valid runs.
        schedule = perf.AbbaSchedule(target_runs=2, retry_limit=3)
        self.drive(schedule, {"base": [False, False, False, True, True],
                              "head": [False, False, False, True, True]})
        self.assertTrue(schedule.complete)
        self.assertEqual(schedule.invalid_runs, {"base": 3, "head": 3})


def command(argv, stdout="", exit_code=0, stderr="", timed_out=False, unix_s=100.0):
    """Build one raw command record as the bounded runner reports it."""
    return perf.CommandRecord(unix_s=unix_s, argv=tuple(argv), exit_code=exit_code,
                              stdout=stdout, stderr=stderr, timed_out=timed_out)


FRONT_ARGV = ("lsappinfo", "front")


class FrontApplicationTests(unittest.TestCase):
    def classify(self, front, pid_record=None):
        """Classify one sample; record every `info` lookup so tests can see whether it ran."""
        lookups = []

        def lookup(asn):
            lookups.append(asn)
            if pid_record is None:
                raise AssertionError("no lookup expected")
            return pid_record
        return perf.classify_front(front, lookup), lookups

    def test_both_null_forms_mean_no_front_application(self):
        # Only `[ NULL ]` and a null ASN not followed by a hex digit mean no front application.
        for stdout in ("[ NULL ]", "  [ NULL ] \n", "ASN:0x0-0x0-NULL\n", "ASN:0x0-0x0:\n", "ASN:0x0-0x0"):
            with self.subTest(stdout=stdout):
                reading, lookups = self.classify(command(FRONT_ARGV, stdout))
                self.assertEqual(reading.kind, "none")
                self.assertEqual(lookups, [])

    def test_front_application_needs_a_successful_pid_lookup(self):
        # An ASN line followed by a `"pid"=<n>` lookup names the front application.
        asn = "ASN:0x0-0x6ba4b9e:"
        reading, lookups = self.classify(
            command(FRONT_ARGV, asn + "\n"),
            command(("lsappinfo", "info", "-only", "pid", asn), '"pid"=55181\n'))
        self.assertEqual((reading.kind, reading.pid), ("app", 55181))
        self.assertEqual(lookups, [asn])
        self.assertEqual(perf.front_pid_argv(asn), ("lsappinfo", "info", "-only", "pid", asn))

    def test_a_hex_digit_after_the_null_prefix_is_a_real_application(self):
        # `ASN:0x0-0x0a1:` is an application whose ASN starts with zero, not the null ASN.
        reading, lookups = self.classify(command(FRONT_ARGV, "ASN:0x0-0x0a1:\n"),
                                         command(("lsappinfo",), '"pid"=42\n'))
        self.assertEqual((reading.kind, reading.pid), ("app", 42))
        self.assertEqual(lookups, ["ASN:0x0-0x0a1:"])

    def test_a_low_half_without_0x_still_names_an_application(self):
        # GitHub's macOS runners print the ASN's low half without `0x` (`ASN:0x0-c00c:`); that is a real ASN.
        for asn, pid in (("ASN:0x0-c00c:", 4101), ("ASN:0x0-24024:", 10753)):
            with self.subTest(asn=asn):
                reading, lookups = self.classify(command(FRONT_ARGV, asn + "\n"),
                                                 command(("lsappinfo",), f'"pid"={pid}\n'))
                self.assertEqual((reading.kind, reading.pid), ("app", pid))
                self.assertEqual(lookups, [asn])

    def test_failed_samples_never_count_as_no_front_application(self):
        # Empty output, a nonzero exit, a timeout or other text invalidates the sample.
        cases = (command(FRONT_ARGV, ""),
                 command(FRONT_ARGV, "[ NULL ]", exit_code=1),
                 command(FRONT_ARGV, "ASN:0x0-0x0:", exit_code=None, timed_out=True),
                 command(FRONT_ARGV, "[NULL]"),
                 command(FRONT_ARGV, "no application\n"),
                 command(FRONT_ARGV, "ASN:0x0-0x6ba4b9e:\nASN:0x0-0x1:\n"))
        for record in cases:
            with self.subTest(record=record):
                reading, _lookups = self.classify(record)
                self.assertEqual(reading.kind, "failed")
                self.assertTrue(reading.detail)

    def test_a_failed_pid_lookup_fails_the_sample(self):
        # The lookup must exit 0 and print exactly one positive pid.
        asn_record = command(FRONT_ARGV, "ASN:0x0-0x6ba4b9e:\n")
        for lookup in (command(("lsappinfo",), "", exit_code=0),
                       command(("lsappinfo",), '"pid"=55181\n', exit_code=1),
                       command(("lsappinfo",), "", exit_code=None, timed_out=True),
                       command(("lsappinfo",), '"pid"=0\n'),
                       command(("lsappinfo",), '"pid"=[ NULL ]\n')):
            with self.subTest(lookup=lookup):
                reading, _lookups = self.classify(asn_record, lookup)
                self.assertEqual(reading.kind, "failed")
                self.assertEqual(len(reading.records), 2)


def reading(kind, pid=None):
    """Build a classified front sample without raw records."""
    return perf.FrontReading(kind, pid, "" if kind != "failed" else "unparseable", ())


class FocusTheftTests(unittest.TestCase):
    HARNESS = 900

    def test_activation_while_another_application_is_front_is_theft(self):
        # The harness becoming front right after another application was front is theft.
        verdict = perf.judge_focus([reading("app", 10), reading("app", self.HARNESS)], self.HARNESS)
        self.assertTrue(verdict.theft)
        self.assertTrue(verdict.problems)

    def test_activation_with_no_front_application_is_not_theft(self):
        # A host with no front application cannot have focus stolen.
        verdict = perf.judge_focus([reading("none"), reading("app", self.HARNESS),
                                    reading("app", self.HARNESS)], self.HARNESS)
        self.assertFalse(verdict.theft)
        self.assertEqual(verdict.problems, [])

    def test_other_applications_changing_focus_is_not_theft(self):
        # Focus moving between the user's own applications says nothing about the harness.
        verdict = perf.judge_focus([reading("app", 10), reading("app", 11), reading("none")], self.HARNESS)
        self.assertEqual(verdict.problems, [])

    def test_a_failed_sample_invalidates_the_run(self):
        # A failed sample is a problem of its own and never reads as no front application.
        verdict = perf.judge_focus([reading("app", 10), reading("failed"), reading("app", self.HARNESS)],
                                   self.HARNESS)
        self.assertFalse(verdict.theft)
        self.assertEqual(len(verdict.failed), 1)
        self.assertTrue(verdict.problems)

    def test_unknown_harness_pid_cannot_pass(self):
        # Without harness.pid no sample can be judged, so the focus check cannot pass.
        verdict = perf.judge_focus([reading("app", 10)], None)
        self.assertFalse(verdict.judged)
        self.assertFalse(verdict.passed)
        self.assertTrue(perf.judge_focus([reading("app", 10)], self.HARNESS).passed)

    def test_activation_without_a_user_session_is_noted_not_theft(self):
        # On a CI runner nobody's focus can be taken: activation after another application is only noted.
        verdict = perf.judge_focus([reading("app", 10), reading("app", self.HARNESS)], self.HARNESS,
                                   user_session=False)
        self.assertFalse(verdict.theft)
        self.assertEqual(verdict.problems, [])
        self.assertTrue(verdict.passed)
        self.assertEqual(len(verdict.notes), 1)
        self.assertIn(str(self.HARNESS), verdict.notes[0])

    def test_failed_samples_still_fail_without_a_user_session(self):
        # The CI exception covers activation only; a failed sample still invalidates the run.
        verdict = perf.judge_focus([reading("app", 10), reading("failed")], self.HARNESS, user_session=False)
        self.assertFalse(verdict.passed)

    def test_only_a_github_actions_runner_lacks_a_user_session(self):
        # GitHub sets GITHUB_ACTIONS=true on its runners; a developer's shell does not.
        self.assertFalse(perf.has_user_session({"GITHUB_ACTIONS": "true"}))
        self.assertTrue(perf.has_user_session({}))
        self.assertTrue(perf.has_user_session({"GITHUB_ACTIONS": "false"}))

    def test_every_raw_sample_is_appended_to_the_evidence_log(self):
        # The log keeps time, argv, exit status, stdout and stderr of both commands of a sample.
        with tempfile.TemporaryDirectory() as temporary:
            log_path = Path(temporary) / "front-samples.log"
            records = (command(FRONT_ARGV, "ASN:0x0-0x1:\n", unix_s=5.0),
                       command(("lsappinfo", "info"), "", exit_code=1, stderr="gone\n", unix_s=5.5))
            perf.append_front_samples(log_path, records)
            perf.append_front_samples(log_path, records[:1])
            lines = [json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines()]
        self.assertEqual(len(lines), 3)
        self.assertEqual(lines[1], {"unix_s": 5.5, "argv": ["lsappinfo", "info"], "exit_code": 1,
                                    "timed_out": False, "stdout": "", "stderr": "gone\n"})
        failed = perf.FrontReading("failed", None, "lookup failed", records)
        self.assertIn('"stderr": "gone\\n"', perf.describe_sample(failed))


class FakeProcess:
    """One mutable process-table entry; `survives_kill` models a process SIGKILL cannot end."""

    def __init__(self, pid, pgid, sid, start="1", command="sh", start_unix_s=1001.0,
                 unreadable=False, survives_kill=False):
        self.pid, self.pgid, self.sid, self.start = pid, pgid, sid, start
        self.command, self.start_unix_s = command, start_unix_s
        self.unreadable, self.survives_kill, self.alive = unreadable, survives_kill, True


class FakeTable:
    """A process table the cleanup code reads and signals; it records every signal sent."""

    def __init__(self, *processes):
        self.processes = {process.pid: process for process in processes}
        self.kills = []
        self.group_kills = []
        self.enumeration_fails = False
        # pid -> (read number, replacement): the pid names another process from that read on.
        self.replace_on_read = {}
        self.read_counts = {}

    def pids(self):
        if self.enumeration_fails:
            return None
        return sorted(pid for pid, process in self.processes.items() if process.alive)

    def session_of(self, pid):
        process = self.processes.get(pid)
        return process.sid if process is not None and process.alive else None

    def read(self, pid):
        self.read_counts[pid] = self.read_counts.get(pid, 0) + 1
        planned = self.replace_on_read.get(pid)
        if planned and planned[0] == self.read_counts[pid]:
            self.processes[pid] = planned[1]
        process = self.processes.get(pid)
        if process is None or not process.alive:
            return None
        if process.unreadable:
            raise perf.ProcessUnreadable(f"pid {pid} cannot be read")
        return perf.ProcessInfo(pid, process.pgid, process.sid, process.start,
                                process.start_unix_s, process.command)

    def kill_group(self, pgid):
        self.group_kills.append(pgid)
        for process in self.processes.values():
            if process.pgid == pgid and not process.survives_kill:
                process.alive = False
        return "sent"

    def kill(self, pid):
        self.kills.append(pid)
        process = self.processes.get(pid)
        if process is None or not process.alive:
            return "gone"
        if not process.survives_kill:
            process.alive = False
        return "sent"


class FakeClock:
    """A monotonic clock that only advances when the code under test sleeps."""

    def __init__(self, now=1000.0):
        self.now = now

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += max(seconds, 0.001)


LAUNCH_UNIX_S = 1000.0
RECORD_TEXT = '{"role": "0", "leader_pid": 500, "anchor_pid": 501, "tty": "/dev/ttys009"}'


def leader(**overrides):
    """The session leader: the scenario script, which leads its session and process group."""
    return FakeProcess(**{"pid": 500, "pgid": 500, "sid": 500, "start": "10.1", **overrides})


def anchor(**overrides):
    """The anchor: a sleeping member that keeps the leader's process group, so its id stays reserved."""
    return FakeProcess(**{"pid": 501, "pgid": 500, "sid": 500, "start": "10.2", "command": "sleep",
                          **overrides})


ACKED = perf.AckedSession("0", 500, 501, "10.1", "10.2")


class SessionRecordTests(unittest.TestCase):
    def test_documented_record_parses(self):
        # The scenario script's record names its role, leader, anchor and tty.
        record = perf.parse_session_record(RECORD_TEXT, "0")
        self.assertEqual(record, perf.SessionRecord("0", 500, 501, "/dev/ttys009"))

    def test_numeric_role_from_the_generated_script_parses(self):
        # The role script writes `"role":%s`, so the role is a bare number matching the file name.
        script_record = '{"role":0,"leader_pid":500,"anchor_pid":501,"tty":"none"}'
        self.assertEqual(perf.parse_session_record(script_record, "0"), perf.SessionRecord("0", 500, 501, "none"))
        for text in (script_record.replace('"role":0', '"role":1'), script_record.replace('"role":0', '"role":true'),
                     script_record.replace('"role":0', '"role":0.0')):
            with self.subTest(text=text), self.assertRaises(ValueError):
                perf.parse_session_record(text, "0")

    def test_malformed_records_are_refused(self):
        # A record that cannot name two distinct positive pids for its own role is refused.
        for text in ("not json", "[]", RECORD_TEXT.replace('"0"', '"1"'),
                     RECORD_TEXT.replace("501", "500"), RECORD_TEXT.replace("501", "true"),
                     RECORD_TEXT.replace("501", '"501"'), RECORD_TEXT.replace("500", "0"),
                     '{"role": "0", "leader_pid": 500, "tty": ""}'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                perf.parse_session_record(text, "0")


class SessionValidationTests(unittest.TestCase):
    RECORD = perf.SessionRecord("0", 500, 501, "/dev/ttys009")

    def test_live_anchor_in_the_leaders_session_and_group_is_acknowledged(self):
        # Validation stores both start times, which later identify the same processes.
        acked, problem = perf.validate_session(self.RECORD, FakeTable(leader(), anchor()), LAUNCH_UNIX_S)
        self.assertIsNone(problem)
        self.assertEqual(acked, ACKED)

    def test_dead_misplaced_or_older_processes_fail_validation(self):
        # Each case would let cleanup signal a process outside this run's session.
        cases = {
            "leader exited": (anchor(),),
            "anchor exited": (leader(),),
            "anchor in another session": (leader(), anchor(sid=777)),
            "anchor in its own job-control group": (leader(), anchor(pgid=501)),
            "leader leads no session": (leader(sid=400), anchor()),
            "anchor predates the launch": (leader(), anchor(start_unix_s=900.0)),
            "leader unreadable": (leader(unreadable=True), anchor()),
        }
        for name, processes in cases.items():
            with self.subTest(name):
                acked, problem = perf.validate_session(self.RECORD, FakeTable(*processes), LAUNCH_UNIX_S)
                self.assertIsNone(acked)
                self.assertTrue(problem)


class SessionCleanupTests(unittest.TestCase):
    def clean(self, table, sessions=(ACKED,)):
        clock = FakeClock()
        result = perf.cleanup_sessions(table, list(sessions), clock=clock, sleep=clock.sleep)
        return result, clock

    def test_crash_before_registration_signals_nothing(self):
        # With no registered session nothing is known about the run's shells, so nothing is signalled.
        table = FakeTable(FakeProcess(600, 600, 600))
        result, _clock = self.clean(table, sessions=())
        self.assertTrue(result.settled)
        self.assertEqual(table.kills, [])

    def test_leader_exit_with_a_surviving_same_session_child(self):
        # A child that outlived its leader, and lost its tty, is still found by its session id.
        table = FakeTable(anchor(), FakeProcess(510, 500, 500, start="11"), FakeProcess(600, 600, 600))
        result, _clock = self.clean(table)
        self.assertTrue(result.settled, result.problems)
        self.assertEqual(table.kills, [510, 501])
        self.assertEqual(result.problems, [])

    def test_separate_job_control_groups_are_cleaned_before_the_anchor(self):
        # Jobs in their own process groups are session members; the anchor is signalled last.
        table = FakeTable(leader(), anchor(), FakeProcess(520, 520, 500, start="12"),
                          FakeProcess(530, 530, 500, start="13"))
        result, _clock = self.clean(table)
        self.assertTrue(result.settled, result.problems)
        self.assertEqual(sorted(table.kills[:-1]), [500, 520, 530])
        self.assertEqual(table.kills[-1], 501)

    def test_reused_anchor_identity_is_never_signalled(self):
        # A live process at the anchor's pid with another start time is not the anchor.
        table = FakeTable(anchor(start="99.9", sid=777, pgid=777))
        result, _clock = self.clean(table)
        self.assertEqual(table.kills, [])
        self.assertTrue(result.settled)
        self.assertTrue(any("reused" in problem for problem in result.problems))

    def test_member_reused_between_listing_and_signal_is_not_signalled(self):
        # The recheck immediately before the signal catches a pid that now names another process.
        table = FakeTable(anchor(), FakeProcess(510, 500, 500, start="11"))
        table.replace_on_read[510] = (2, FakeProcess(510, 777, 777, start="12"))
        result, _clock = self.clean(table)
        self.assertNotIn(510, table.kills)
        self.assertEqual(table.kills, [501])
        self.assertTrue(result.settled, result.problems)

    def test_missing_anchor_with_surviving_members_signals_nothing(self):
        # Without the anchor the session id may belong to someone else, so members are only listed.
        table = FakeTable(FakeProcess(510, 500, 500, start="11"))
        result, _clock = self.clean(table)
        self.assertFalse(result.settled)
        self.assertEqual(table.kills, [])
        self.assertEqual([member.pid for member in result.survivors], [510])

    def test_deadline_kill_leaves_whole_sessions_that_cleanup_ends(self):
        # A harness killed at its deadline leaves every PTY session alive; cleanup ends them all.
        second = perf.AckedSession("1", 700, 701, "20.1", "20.2")
        table = FakeTable(leader(), anchor(), FakeProcess(510, 510, 500, start="11"),
                          FakeProcess(700, 700, 700, start="20.1"),
                          FakeProcess(701, 700, 700, start="20.2", command="sleep"))
        result, _clock = self.clean(table, sessions=(ACKED, second))
        self.assertTrue(result.settled, result.problems)
        self.assertEqual(sorted(table.kills), [500, 501, 510, 700, 701])
        self.assertLess(table.kills.index(510), table.kills.index(501))
        self.assertLess(table.kills.index(700), table.kills.index(701))

    def test_a_survivor_after_the_bound_keeps_the_anchor_and_fails(self):
        # A member SIGKILL cannot end leaves the anchor alive, so the session id stays reserved.
        table = FakeTable(anchor(), FakeProcess(510, 500, 500, start="11", survives_kill=True))
        result, clock = self.clean(table)
        self.assertFalse(result.settled)
        self.assertNotIn(501, table.kills)
        self.assertEqual([member.pid for member in result.survivors], [510])
        self.assertGreaterEqual(clock.now - 1000.0, perf.CLEANUP_BOUND_S)

    def test_incomplete_enumeration_signals_nothing(self):
        # A process list that cannot be read is never proof that a session is empty.
        table = FakeTable(anchor(), FakeProcess(510, 500, 500, start="11"))
        table.enumeration_fails = True
        result, _clock = self.clean(table)
        self.assertFalse(result.settled)
        self.assertEqual(table.kills, [])

    def test_unreadable_member_makes_the_enumeration_incomplete(self):
        # A session member whose identity cannot be read can be neither signalled nor ignored.
        table = FakeTable(anchor(), FakeProcess(510, 500, 500, start="11", unreadable=True))
        result, _clock = self.clean(table)
        self.assertFalse(result.settled)
        self.assertEqual(table.kills, [])


class ProcessTableDecodingTests(unittest.TestCase):
    @staticmethod
    def bsdinfo(status=2, pid=4321, ppid=1, pgid=4000, comm=b"perf_scenarios", start_s=1700000000, start_us=42):
        """Pack a synthetic 136-byte `proc_bsdinfo` record in the kernel's field order."""
        return perf.struct.pack(perf.BSDINFO_FORMAT, 0, status, 0, pid, ppid, 501, 20, 501, 20, 501, 20, 0,
                                comm, b"", 3, pgid, 0, 0, 0, 0, start_s, start_us)

    def test_bsdinfo_record_is_136_bytes_and_decodes_its_identity(self):
        # The layout must match the 136 bytes `proc_pidinfo(PROC_PIDTBSDINFO)` writes.
        self.assertEqual(perf.BSDINFO_SIZE, 136)
        info = perf.decode_bsdinfo(self.bsdinfo(), sid=4000)
        self.assertEqual((info.pid, info.ppid, info.pgid, info.sid, info.command),
                         (4321, 1, 4000, 4000, "perf_scenarios"))
        self.assertEqual(info.start, "1700000000.000042")
        self.assertAlmostEqual(info.start_unix_s, 1700000000.000042)

    def test_bsdinfo_zombie_reads_as_gone(self):
        # A zombie runs nothing and is no session member to signal.
        self.assertIsNone(perf.decode_bsdinfo(self.bsdinfo(status=5), sid=4000))

    def test_proc_stat_with_parentheses_in_the_command_name(self):
        # The command name may hold spaces and parentheses; fields follow the last `)`.
        line = "1234 (my (odd) cmd) S 1 1230 1200 34816 1230 4194304 0 0 0 0 0 0 0 0 20 0 1 0 5000 1 2\n"
        info = perf.parse_proc_stat(line, 1234, boot_unix_s=1000.0, ticks_per_s=100)
        self.assertEqual((info.pid, info.ppid, info.pgid, info.sid, info.command, info.start),
                         (1234, 1, 1230, 1200, "my (odd) cmd", "5000"))
        self.assertEqual(info.start_unix_s, 1050.0)

    def test_proc_stat_zombie_reads_as_gone_and_garbage_is_unreadable(self):
        # A zombie is gone; a record that cannot be parsed is never read as gone.
        zombie = "1234 (sh) Z 1 1230 1200 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5000 1 2\n"
        self.assertIsNone(perf.parse_proc_stat(zombie, 1234, 1000.0, 100))
        for garbage in ("", "1234 sh S 1", "1234 (sh) S 1 x 1200"):
            with self.subTest(garbage=garbage), self.assertRaises(perf.ProcessUnreadable):
                perf.parse_proc_stat(garbage, 1234, 1000.0, 100)


@unittest.skipUnless(sys.platform == "darwin" or sys.platform.startswith("linux"),
                     "the real process table exists on macOS and Linux")
class LiveProcessTableTests(unittest.TestCase):
    def test_reading_this_process_matches_the_kernel(self):
        # A read-only check of this test process proves the record offsets on the real host.
        table = perf.make_process_table()
        info = table.read(os.getpid())
        self.assertEqual((info.pid, info.ppid, info.pgid, info.sid),
                         (os.getpid(), os.getppid(), os.getpgid(0), os.getsid(0)))
        self.assertLess(abs(info.start_unix_s - perf.time.time()), 3600)
        self.assertIn(os.getpid(), table.pids())
        self.assertEqual(table.session_of(os.getpid()), os.getsid(0))

    def test_a_pid_above_every_hosts_limit_reads_as_gone(self):
        # Linux caps pids at 2**22 and macOS at 99999, so this pid cannot exist.
        table = perf.make_process_table()
        self.assertIsNone(table.read(2**22 + 1))
        self.assertIsNone(table.session_of(2**22 + 1))


STAMP = "2026-10-02T11:24:16.123456Z"
STAMP_UNIX_S = 1790940256.123456  # calendar.timegm of 2026-10-02T11:24:16Z, plus the fraction


def memory_line(stamp=STAMP, resident="123", renderer="456", session="789", prefix="memory: "):
    """One `memory snapshot` line as the file layer writes it."""
    return (f"{stamp}  INFO {prefix}memory snapshot process_private_committed_bytes=unsupported "
            f"process_resident_bytes={resident} process_virtual_bytes=unsupported "
            f"session_total_bytes={session} renderer_total_bytes={renderer} renderers=[main warm] "
            f"allocator_state=unsupported")


class MemoryLineTests(unittest.TestCase):
    def test_snapshot_line_parses_byte_counts_and_unsupported_values(self):
        # Process fields may read `unsupported`; extra fields are tolerated.
        sample = perf.parse_memory_line(memory_line(resident="unsupported"))
        self.assertIsNone(sample.process_resident_bytes)
        self.assertEqual((sample.renderer_total_bytes, sample.session_total_bytes), (456, 789))
        self.assertAlmostEqual(sample.unix_s, STAMP_UNIX_S, places=5)
        self.assertEqual(perf.parse_memory_line(memory_line()).process_resident_bytes, 123)

    def test_a_span_may_precede_the_target(self):
        # The line is found by its message, wherever a span puts the target.
        line = memory_line(prefix="run{scenario=S1}: memory: ")
        self.assertEqual(perf.parse_memory_line(line).renderer_total_bytes, 456)

    def test_other_memory_events_and_malformed_lines_are_ignored(self):
        # Other events share the `memory` target; required totals must be integers.
        for line in (f"{STAMP}  INFO memory: reclaimed renderer_total_bytes=5 session_total_bytes=1",
                     f"{STAMP} DEBUG memory: pane snapshot renderer_total_bytes=5",
                     memory_line(renderer="unsupported"), memory_line(session=""),
                     memory_line(stamp="yesterday"), ""):
            with self.subTest(line=line):
                self.assertIsNone(perf.parse_memory_line(line))

    def test_checkpoint_takes_the_latest_line_at_or_before_it(self):
        # A line written after the checkpoint never describes it.
        samples = [perf.MemorySample(unix_s, None, int(unix_s), 0) for unix_s in (10.0, 20.0, 30.0)]
        self.assertEqual(perf.memory_at(samples, 25.0).unix_s, 20.0)
        self.assertEqual(perf.memory_at(samples, 20.0).unix_s, 20.0)
        self.assertIsNone(perf.memory_at(samples, 5.0))

    def test_log_directory_reads_every_rolled_file_but_not_breadcrumbs(self):
        # Rolled daily files are all read; subdirectories such as breadcrumbs are not.
        with tempfile.TemporaryDirectory() as temporary:
            logs = Path(temporary)
            (logs / "sonicterm.log.2026-10-03").write_text(
                memory_line(stamp="2026-10-03T00:00:01Z", renderer="2") + "\n", encoding="utf-8")
            (logs / "sonicterm.log.2026-10-02").write_text(memory_line() + "\nnoise\n", encoding="utf-8")
            (logs / "breadcrumbs").mkdir()
            (logs / "breadcrumbs" / "breadcrumbs-1-2-3.log").write_text(memory_line(renderer="9"), encoding="utf-8")
            samples = perf.read_memory_samples(logs)
        self.assertEqual([sample.renderer_total_bytes for sample in samples], [456, 2])
        self.assertEqual(perf.read_memory_samples(Path(temporary) / "missing"), [])


class RenderTimingTests(unittest.TestCase):
    LINE = ("2026-10-02T11:24:16.5Z DEBUG render_timing: line=[render_timing] window=main "
            "total=4.20ms prepare=1.00ms present=3.00ms tail=0.20ms")

    def test_render_timing_field_parses_every_lap(self):
        # The fmt layer writes the text as the `line` field; every lap and the total are kept.
        sample = perf.parse_render_timing(self.LINE)
        self.assertEqual(sample.window, "main")
        self.assertEqual(sample.laps, {"total": 4.2, "prepare": 1.0, "present": 3.0, "tail": 0.2})

    def test_malformed_or_unrelated_lines_are_ignored(self):
        # A lap without a millisecond value would shift every later lap, so the line is dropped.
        for line in (self.LINE.replace("prepare=1.00ms", "prepare=fast"),
                     self.LINE.replace("window=main ", ""), memory_line(), ""):
            with self.subTest(line=line):
                self.assertIsNone(perf.parse_render_timing(line))


HARNESS_HASH = "ab" * 32


def valid_result(**overrides):
    """A result.json body that satisfies the schema; overrides replace top-level keys."""
    phase = {"name": "workload", "start_unix_s": 10.0, "end_unix_s": 70.0, "cpu_user_s": 1.5,
             "cpu_system_s": 0.5, "presented_frames": 120, "redraw_requested": 130,
             "dispatch_ms": [1.0, 2.0], "present_interval_ms": [16.6, 16.7], "allocations_per_frame": None}
    result = {"schema_version": 1, "managed": True, "harness_hash": HARNESS_HASH, "status": "valid",
              "exit_code": 0, "grid": {"columns": 250, "rows": 70}, "phases": [phase],
              "latency": None, "throughput": {"bytes": 1000, "seconds": 2.0}, "uncover_ms": None,
              "scrollback_rows_retained": None,
              "checkpoints": [{"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}],
              "finish_session_settled": True, "notes": []}
    result.update(overrides)
    return result


class ResultSchemaTests(unittest.TestCase):
    def test_valid_result_passes(self):
        # The documented schema, with extra keys tolerated, has no problem.
        self.assertEqual(perf.validate_result({**valid_result(), "scenario": "S1"}, HARNESS_HASH, 0), [])

    def test_unmanaged_result_or_another_harness_fails(self):
        # A standalone run or a result from another harness can never enter a comparison.
        for overrides in ({"managed": False}, {"managed": None}, {"harness_hash": "cd" * 32}):
            with self.subTest(overrides=overrides):
                self.assertTrue(perf.validate_result(valid_result(**overrides), HARNESS_HASH, 0))
        missing = valid_result()
        del missing["managed"]
        self.assertTrue(perf.validate_result(missing, HARNESS_HASH, 0))

    def test_schema_version_must_be_one(self):
        # Another version or a string version means the parser cannot trust any field.
        for version in (2, "1", None, True):
            with self.subTest(version=version):
                self.assertTrue(perf.validate_result(valid_result(schema_version=version), HARNESS_HASH, 0))
        self.assertTrue(perf.validate_result([], HARNESS_HASH, 0))

    def test_field_types_are_checked(self):
        # Wrong types fail instead of reading as zero or as a missing metric.
        broken_phase = dict(valid_result()["phases"][0], dispatch_ms=[1.0, "slow"])
        for overrides in ({"phases": {}}, {"phases": [broken_phase]},
                          {"phases": [dict(broken_phase, dispatch_ms=[], presented_frames=None)]},
                          {"checkpoints": [{"index": 0, "unix_s": 1.0}]},
                          {"finish_session_settled": "yes"}, {"exit_code": True},
                          {"latency": {"samples": [], "attributed": 0, "total": 0}},
                          {"throughput": {"bytes": 1}}, {"notes": "none"}, {"grid": None}):
            with self.subTest(overrides=overrides):
                self.assertTrue(perf.validate_result(valid_result(**overrides), HARNESS_HASH, 0))

    def test_partial_result_with_null_fields_passes(self):
        # A timed-out or early result writes null for what it never measured; that is not a schema failure.
        latency = {"samples": [{"inject_unix_s": 2.0, "latency_ms": None, "reason": "no-candidate"},
                               {"inject_unix_s": 3.0, "latency_ms": 7.5, "reason": None}],
                   "attributed": 1, "total": 2, "coverage": 0.5}
        early = valid_result(status="timeout", exit_code=4, grid=None, finish_session_settled=None,
                             latency=latency, invalid_reason=None, harness_pid=HARNESS_PID)
        self.assertEqual(perf.validate_result(early, HARNESS_HASH, 4), [])
        self.assertTrue(perf.validate_result(valid_result(finish_session_settled="yes"), HARNESS_HASH, 0))
        broken = dict(latency, samples=[{"latency_ms": "slow"}])
        self.assertTrue(perf.validate_result(valid_result(latency=broken), HARNESS_HASH, 0))

    def test_valid_result_ends_with_an_end_checkpoint(self):
        # Memory is reported for every checkpoint label, and a valid run always ends with `end`.
        other = [{"index": 0, "label": "idle", "unix_s": 50.0, "footprint_file": None}]
        self.assertTrue(perf.validate_result(valid_result(checkpoints=other), HARNESS_HASH, 0))
        invalid = valid_result(checkpoints=other, status="invalid", exit_code=3)
        self.assertEqual(perf.validate_result(invalid, HARNESS_HASH, 3), [])

    def test_recorded_exit_code_matches_the_process(self):
        # A result whose exit code disagrees with the process describes another run.
        self.assertTrue(perf.validate_result(valid_result(), HARNESS_HASH, 3))
        self.assertEqual(perf.validate_result(valid_result(), HARNESS_HASH, None), [])

    def test_occlusion_reason_marks_an_environmental_invalidation(self):
        # Only an occlusion is retried in the smoke; other invalidations are not environmental.
        self.assertTrue(perf.occlusion_invalidated(
            valid_result(status="invalid", notes=["unrequested native occlusion change"])))
        self.assertFalse(perf.occlusion_invalidated(
            valid_result(status="invalid", notes=["unexpected keyboard input"])))
        self.assertFalse(perf.occlusion_invalidated(valid_result(notes=["occlusion synthetic"])))


class HomeWriteTests(unittest.TestCase):
    SENTINEL_NS = 5_000_000_000
    HARNESS = 900

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.home = Path(self.temporary.name) / ".sonicterm"
        self.scratch = Path(self.temporary.name) / "scratch-run"

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, relative, data, mtime_ns=1_000_000_000):
        """Write a file under the temporary home with a chosen modification time."""
        target = self.home / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        os.utime(target, ns=(mtime_ns, mtime_ns))

    def violations(self, before, other_alive=False):
        after = perf.snapshot_home(self.home)
        return perf.home_violations(self.home, before, after, self.SENTINEL_NS, self.HARNESS,
                                    other_alive, [str(self.scratch)])

    def test_absent_home_is_recorded_and_never_created(self):
        # The check reads only; an absent directory stays absent and is no violation.
        self.assertIsNone(perf.snapshot_home(self.home))
        self.assertFalse(self.home.exists())
        self.assertEqual(self.violations(None), [])

    def test_created_home_or_changed_config_is_a_violation(self):
        # A write outside the attributable logs is a violation, including creating the directory.
        self.write("config.toml", b"x")
        self.assertTrue(self.violations(None))
        before = perf.snapshot_home(self.home)
        self.write("config.toml", b"changed", self.SENTINEL_NS + 1)
        self.assertEqual(len(self.violations(before)), 1)

    def test_ds_store_is_ignored(self):
        # Finder metadata is not a SonicTerm write.
        self.write("config.toml", b"x")
        before = perf.snapshot_home(self.home)
        self.write("logs/.DS_Store", b"finder", self.SENTINEL_NS + 1)
        self.assertEqual(self.violations(before), [])

    def test_breadcrumb_belongs_to_the_pid_in_its_name(self):
        # Another instance's breadcrumbs are its own; the harness starts no breadcrumb writer.
        self.write("config.toml", b"x")
        before = perf.snapshot_home(self.home)
        self.write("logs/breadcrumbs/breadcrumbs-20261002T112416-4242-0.log", b"crumb", self.SENTINEL_NS + 1)
        self.assertEqual(self.violations(before), [])
        self.write(f"logs/breadcrumbs/breadcrumbs-20261002T112416-{self.HARNESS}-1.log", b"crumb",
                   self.SENTINEL_NS + 1)
        self.assertEqual(len(self.violations(before)), 1)

    def test_log_growth_belongs_elsewhere_only_with_another_instance_and_without_this_run(self):
        # Both conditions must hold, and only the appended bytes are read.
        self.write("logs/sonicterm.log.2026-10-02", str(self.scratch).encode() + b"\n")
        before = perf.snapshot_home(self.home)
        self.write("logs/sonicterm.log.2026-10-02", str(self.scratch).encode() + b"\nother instance\n",
                   self.SENTINEL_NS + 1)
        self.assertEqual(self.violations(before, other_alive=True), [])
        self.assertTrue(self.violations(before, other_alive=False))
        self.write("logs/sonicterm.log.2026-10-02", str(self.scratch).encode() + b"\nopened "
                   + str(self.scratch).encode() + b"\n", self.SENTINEL_NS + 1)
        self.assertTrue(self.violations(before, other_alive=True))

    def test_a_shrunk_log_is_not_growth(self):
        # Rewriting a log is no append, so no other instance explains it.
        self.write("logs/sonicterm.log", b"long existing content\n")
        before = perf.snapshot_home(self.home)
        self.write("logs/sonicterm.log", b"short\n", self.SENTINEL_NS + 1)
        self.assertTrue(self.violations(before, other_alive=True))

    def test_removal_belongs_elsewhere_only_under_logs(self):
        # Another instance's retention may remove old logs; nothing else may disappear.
        self.write("logs/sonicterm.log.2026-09-01", b"old\n")
        self.write("themes/custom.toml", b"x")
        before = perf.snapshot_home(self.home)
        (self.home / "logs/sonicterm.log.2026-09-01").unlink()
        self.assertEqual(self.violations(before, other_alive=True), [])
        self.assertTrue(self.violations(before, other_alive=False))
        (self.home / "themes/custom.toml").unlink()
        self.assertEqual(len(self.violations(before, other_alive=True)), 1)

    def test_unchanged_file_newer_than_the_sentinel_is_a_candidate(self):
        # A write between the sentinel and the first snapshot still reads as newer than the sentinel.
        self.write("config.toml", b"x", self.SENTINEL_NS + 10)
        self.assertTrue(self.violations(perf.snapshot_home(self.home)))

    def test_another_instance_is_a_live_sonicterm_process_other_than_the_harness(self):
        # The harness itself, other programs and unreadable processes are not another instance.
        table = FakeTable(FakeProcess(self.HARNESS, 900, 900, command="sonicterm-mac"),
                          FakeProcess(901, 901, 901, command="zsh"),
                          FakeProcess(902, 902, 902, command="sonicterm-mac", unreadable=True))
        self.assertFalse(perf.other_instance_alive(table, self.HARNESS))
        table.processes[903] = FakeProcess(903, 903, 903, command="sonicterm-linux")
        self.assertTrue(perf.other_instance_alive(table, self.HARNESS))
        table.enumeration_fails = True
        self.assertFalse(perf.other_instance_alive(table, self.HARNESS))


SELECTION_TABLE = """[[example]]
name = "native_split_selection"
path = "examples/native_split_selection.rs"
harness = false
test = false
"""
HARNESS_TABLES = """[[example]]
name = "perf_scenarios"
path = "examples/perf_scenarios/main.rs"
test = true

[[example]]
name = "perf_scenarios_alloc"
path = "examples/perf_scenarios/alloc_main.rs"
test = false
"""
PACKAGE = '[package]\nname = "sonicterm-app"\n\n'
DEPENDENCIES = '\n[dev-dependencies]\ntempfile = "3"\n'
HEAD_MANIFEST = PACKAGE + SELECTION_TABLE + "\n" + HARNESS_TABLES + DEPENDENCIES
BASE_MANIFEST = PACKAGE + SELECTION_TABLE + DEPENDENCIES

try:
    import tomllib
except ImportError:  # Python before 3.11 has no TOML parser; the text checks still run.
    tomllib = None


class CargoOverlayTests(unittest.TestCase):
    def test_entries_are_the_two_harness_tables_in_fixed_order(self):
        # The hash and the overlay read exactly the two harness examples, never another example.
        entries = perf.harness_entries(HEAD_MANIFEST)
        self.assertLess(entries.index('name = "perf_scenarios"'), entries.index('name = "perf_scenarios_alloc"'))
        self.assertNotIn("native_split_selection", entries)
        self.assertNotIn("dev-dependencies", entries)

    def test_missing_entries_are_inserted_and_other_tables_kept(self):
        # A base without the harness gains the head's two tables; everything else is untouched.
        overlaid = perf.overlay_manifest(BASE_MANIFEST, perf.harness_entries(HEAD_MANIFEST))
        self.assertEqual(perf.harness_entries(overlaid), perf.harness_entries(HEAD_MANIFEST))
        self.assertTrue(overlaid.startswith(BASE_MANIFEST.rstrip("\n")))
        if tomllib is not None:
            parsed = tomllib.loads(overlaid)
            self.assertEqual([entry["name"] for entry in parsed["example"]],
                             ["native_split_selection", "perf_scenarios", "perf_scenarios_alloc"])
            self.assertEqual(parsed["dev-dependencies"], {"tempfile": "3"})

    def test_identical_entries_leave_the_manifest_unchanged(self):
        # The head's own manifest, and a base that already matches, are not rewritten.
        self.assertEqual(perf.overlay_manifest(HEAD_MANIFEST, perf.harness_entries(HEAD_MANIFEST)), HEAD_MANIFEST)

    def test_different_or_duplicated_entries_are_replaced(self):
        # Every stale harness table is removed, so each example is defined once, as at head.
        stale = HARNESS_TABLES.replace("test = true", "test = false")
        for base in (PACKAGE + stale + DEPENDENCIES, PACKAGE + stale + "\n" + stale + DEPENDENCIES):
            with self.subTest(base=base):
                overlaid = perf.overlay_manifest(base, perf.harness_entries(HEAD_MANIFEST))
                self.assertEqual(perf.harness_entries(overlaid), perf.harness_entries(HEAD_MANIFEST))
                self.assertEqual(overlaid.count('name = "perf_scenarios"\n'), 1)
                if tomllib is not None:
                    self.assertEqual(len(tomllib.loads(overlaid)["example"]), 2)

    def test_head_without_both_entries_is_refused(self):
        # A head whose harness is not declared cannot define the overlay.
        for manifest in (BASE_MANIFEST, HEAD_MANIFEST.replace('name = "perf_scenarios_alloc"', 'name = "other"')):
            with self.subTest(manifest=manifest), self.assertRaises(ValueError):
                perf.harness_entries(manifest)


class HarnessHashTests(unittest.TestCase):
    def tree(self, root, files):
        for relative, data in files.items():
            target = Path(root) / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        return Path(root)

    def test_hash_covers_sorted_paths_bytes_and_entries(self):
        # Creation order does not matter; any byte, name or entry change does.
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            files = {"main.rs": b"fn main() {}\n", "nested/record.rs": b"pub struct Record;\n"}
            one = self.tree(first, files)
            two = self.tree(second, dict(reversed(list(files.items()))))
            digest = perf.harness_hash(one, HARNESS_TABLES)
            self.assertEqual(digest, perf.harness_hash(two, HARNESS_TABLES))
            self.assertRegex(digest, r"^[0-9a-f]{64}$")
            self.assertNotEqual(digest, perf.harness_hash(one, HARNESS_TABLES + "# changed\n"))
            (two / "main.rs").write_bytes(b"fn main() { }\n")
            self.assertNotEqual(digest, perf.harness_hash(two, HARNESS_TABLES))
            (two / "main.rs").write_bytes(files["main.rs"])
            (two / "main.rs").rename(two / "main2.rs")
            self.assertNotEqual(digest, perf.harness_hash(two, HARNESS_TABLES))

    def test_path_and_content_boundaries_cannot_collide(self):
        # Length prefixes keep `ab` + `c` apart from `a` + `bc`.
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            self.assertNotEqual(perf.harness_hash(self.tree(first, {"ab": b"c"}), ""),
                                perf.harness_hash(self.tree(second, {"a": b"bc"}), ""))

    def test_empty_or_linked_harness_is_refused(self):
        # An empty directory or a symlink would make the two trees' hashes meaningless.
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(ValueError):
                perf.harness_hash(Path(temporary), HARNESS_TABLES)
            root = self.tree(temporary, {"main.rs": b"x"})
            try:
                (root / "link.rs").symlink_to(root / "main.rs")
            except (OSError, NotImplementedError):
                self.skipTest("this host cannot create a symlink")
            with self.assertRaises(ValueError):
                perf.harness_hash(root, HARNESS_TABLES)

    def test_overlay_refuses_to_target_its_own_source(self):
        # The overlay deletes its target's harness first, so a target that is the source would lose it.
        with tempfile.TemporaryDirectory() as head_root:
            head = self.tree(head_root, {f"{perf.HARNESS_DIRECTORY}/main.rs": b"head",
                                         perf.APP_MANIFEST: HEAD_MANIFEST.encode()})
            spellings = [head, head / "crates" / ".."]
            alias = Path(head_root + "-alias")
            try:
                alias.symlink_to(head, target_is_directory=True)
                spellings.append(alias)
            except (OSError, NotImplementedError):
                pass  # This host cannot create a symlink; the other spellings still run.
            try:
                for target in spellings:
                    with self.subTest(target=target), self.assertRaises(ValueError):
                        perf.overlay_harness(head, target)
            finally:
                if alias.is_symlink():
                    alias.unlink()
            self.assertEqual((head / perf.HARNESS_DIRECTORY / "main.rs").read_bytes(), b"head")

    def test_overlay_refuses_to_target_the_main_checkout(self):
        # The main checkout holds uncommitted work; ROOT is replaced so a broken guard deletes only a fixture.
        with tempfile.TemporaryDirectory() as head_root, tempfile.TemporaryDirectory() as main_root:
            head = self.tree(head_root, {f"{perf.HARNESS_DIRECTORY}/main.rs": b"head",
                                         perf.APP_MANIFEST: HEAD_MANIFEST.encode()})
            main = self.tree(main_root, {f"{perf.HARNESS_DIRECTORY}/main.rs": b"uncommitted",
                                         perf.APP_MANIFEST: HEAD_MANIFEST.encode()})
            with mock.patch.object(perf, "ROOT", main), self.assertRaises(ValueError):
                perf.overlay_harness(head, main / "crates" / "..")
            self.assertEqual((main / perf.HARNESS_DIRECTORY / "main.rs").read_bytes(), b"uncommitted")

    def test_overlay_makes_both_trees_hash_alike_and_leaves_src_alone(self):
        # The base gets the head's directory and entries; files under src/ are never touched.
        with tempfile.TemporaryDirectory() as head_root, tempfile.TemporaryDirectory() as base_root:
            harness = perf.HARNESS_DIRECTORY
            head = self.tree(head_root, {f"{harness}/main.rs": b"head", f"{harness}/alloc_main.rs": b"alloc",
                                         perf.APP_MANIFEST: HEAD_MANIFEST.encode()})
            base = self.tree(base_root, {f"{harness}/main.rs": b"old", f"{harness}/stale.rs": b"stale",
                                         perf.APP_MANIFEST: BASE_MANIFEST.encode(),
                                         "crates/sonicterm-app/src/lib.rs": b"base source"})
            perf.overlay_harness(head, base)
            self.assertEqual(perf.tree_harness_hash(base), perf.tree_harness_hash(head))
            self.assertFalse((base / harness / "stale.rs").exists())
            self.assertEqual((base / "crates/sonicterm-app/src/lib.rs").read_bytes(), b"base source")


LIST_JSON = {"schema_version": 1, "scenarios": [
    {"id": "S1", "variants": ["default"], "title": "Idle", "timeout_s": 120, "short_timeout_s": 30},
    {"id": "S10", "variants": ["default", "sync"], "title": "TUI", "timeout_s": 200, "short_timeout_s": 40}]}


class BuildAndListTests(unittest.TestCase):
    def test_build_command_is_locked_and_reports_artifacts(self):
        # Release builds for comparisons, debug for the smoke; the artifact message names the binary.
        self.assertEqual(perf.build_argv("perf_scenarios", release=True),
                         ("cargo", "build", "--locked", "--release", "-p", "sonicterm-app", "--example",
                          "perf_scenarios", "--message-format=json-render-diagnostics"))
        self.assertNotIn("--release", perf.build_argv("perf_scenarios", release=False))

    def test_executable_comes_from_the_examples_artifact_message(self):
        # Only the named example's artifact counts, whatever target directory Cargo chose.
        log = "\n".join([
            "[local-gate] step=build", "",
            '{"reason":"compiler-artifact","target":{"name":"sonicterm_app","kind":["lib"]},"executable":null}',
            "   Compiling sonicterm-app v1.3.8",
            '{"reason":"compiler-artifact","target":{"name":"perf_scenarios","kind":["example"]},'
            '"executable":"/t/release/examples/perf_scenarios","fresh":true}',
            '{"reason":"build-finished","success":true}'])
        self.assertEqual(perf.artifact_executable(log, "perf_scenarios"), Path("/t/release/examples/perf_scenarios"))
        self.assertIsNone(perf.artifact_executable(log, "perf_scenarios_alloc"))

    def test_scenario_list_parses_from_the_step_log(self):
        # The list is the JSON line among the launcher's header and footer.
        log = "[local-gate] step=list\n\n" + json.dumps(LIST_JSON) + "\n\n[local-gate] result=PASS exit=0\n"
        scenarios = perf.parse_scenario_list(log)
        self.assertEqual([scenario.id for scenario in scenarios], ["S1", "S10"])
        self.assertEqual(scenarios[1].variants, ("default", "sync"))
        self.assertEqual((scenarios[1].timeout_s, scenarios[1].short_timeout_s), (200, 40))

    def test_malformed_scenario_list_is_refused(self):
        # A wrong version, a missing field or no JSON at all cannot define the scenario set.
        broken = json.loads(json.dumps(LIST_JSON))
        del broken["scenarios"][0]["short_timeout_s"]
        for text in ("no json here", json.dumps({**LIST_JSON, "schema_version": 2}), json.dumps(broken),
                     json.dumps({"schema_version": 1, "scenarios": []})):
            with self.subTest(text=text), self.assertRaises(ValueError):
                perf.parse_scenario_list(text)

    def test_selection_expands_all_dedupes_and_keeps_the_given_order(self):
        # `all` is every default variant; named variants run only when asked; repeats are dropped.
        scenarios = perf.parse_scenario_list(json.dumps(LIST_JSON))
        self.assertEqual(perf.select_scenarios(["all", "S10/sync", "S1", "S10/default"], scenarios),
                         [("S1", "default"), ("S10", "default"), ("S10", "sync")])
        self.assertEqual(perf.select_scenarios(["S10/sync", "all"], scenarios),
                         [("S10", "sync"), ("S1", "default"), ("S10", "default")])
        for unknown in (["S99"], ["S1/sync"], []):
            with self.subTest(unknown=unknown), self.assertRaises(ValueError):
                perf.select_scenarios(unknown, scenarios)

    def test_run_command_environment_and_scratch(self):
        # The child drops NO_COLOR and RUST_LOG, keeps HOME, and gets a new scratch path.
        binary, scratch = Path("/b/perf_scenarios"), Path("/tmp/run")
        argv = perf.harness_argv(binary, "S10", "sync", HARNESS_HASH, scratch, short=True, laps=True)
        self.assertEqual(argv, (str(binary), "--run", "S10", "--variant", "sync", "--short", "--laps",
                                "--managed", "--harness-hash", HARNESS_HASH, str(scratch)))
        self.assertEqual(perf.harness_environment({"HOME": "/h", "NO_COLOR": "1", "RUST_LOG": "debug"}),
                         {"HOME": "/h"})
        with tempfile.TemporaryDirectory() as temporary:
            scratch = perf.new_scratch_path(Path(temporary), "S10", "sync")
            self.assertEqual(scratch.parent, Path(temporary))
            self.assertFalse(scratch.exists())

    def test_run_timeouts_add_a_margin_and_the_smoke_caps_at_100_s(self):
        # A run that reaches its bound is an unexpected harness exit, never a pass.
        scenario = perf.Scenario("S1", ("default",), "Idle", 120, 30)
        self.assertEqual(perf.run_timeout_s(scenario, smoke=False), 120 + perf.RUN_MARGIN_S)
        self.assertEqual(perf.run_timeout_s(scenario, smoke=True), min(30 + perf.RUN_MARGIN_S, 100))
        self.assertEqual(perf.run_timeout_s(perf.Scenario("S11", ("default",), "Image", 300, 90), smoke=True), 100)


class FakeGate:
    """Stands in for local-gate.py: records each step and answers it from a handler."""

    PASS, FAIL, TIMEOUT = "PASS", "FAIL", "TIMEOUT"

    def __init__(self, handler):
        self.handler = handler
        self.steps = []
        self.roots = []
        self.environs = []

    def Step(self, step_id, argv, hosts, timeout_s, evidence, prerequisites, ci_jobs):
        return SimpleNamespace(id=step_id, argv=tuple(argv), timeout_s=timeout_s)

    def run_step(self, step, index, root, log_dir, environ, output_limit_bytes=None):
        self.steps.append(step)
        self.roots.append(Path(root))
        self.environs.append(dict(environ))
        status, exit_code, output = self.handler(step)
        log_path = Path(log_dir) / f"{index:02d}-{step.id}.log"
        log_path.write_text(output, encoding="utf-8")
        return SimpleNamespace(id=step.id, status=status, exit_code=exit_code, log_path=log_path,
                               detail="", leftover_processes=0, elapsed_s=0.0)


HARNESS_PID = 900


def harness_process(**overrides):
    """The harness: run_step starts it in a new session, so it leads its session and group."""
    return FakeProcess(**{"pid": HARNESS_PID, "pgid": HARNESS_PID, "sid": HARNESS_PID, "start": "9",
                          "command": "perf_scenarios", "start_unix_s": 1000.5, **overrides})


class RunWatcherTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.scratch, self.evidence = root / "scratch", root / "evidence"
        (self.scratch / "sessions").mkdir(parents=True)
        (self.scratch / "checkpoints").mkdir()
        self.evidence.mkdir()
        self.table = FakeTable(leader(), anchor(), harness_process())
        self.footprint_answer = ("PASS", 0)
        self.gate = FakeGate(self.answer_footprint)

    def tearDown(self):
        self.temporary.cleanup()

    def answer_footprint(self, step):
        status, exit_code = self.footprint_answer
        if status == "PASS":
            Path(step.argv[-1]).write_text(json.dumps({"processes": [{"pid": HARNESS_PID, "footprint": 1234}]}),
                                           encoding="utf-8")
        return status, exit_code, "footprint: cannot attach\n" if status != "PASS" else ""

    def watcher(self, kill_at_go=False):
        context = perf.RunContext(self.scratch, self.evidence, LAUNCH_UNIX_S, self.table, self.gate, (), kill_at_go)
        return perf.RunWatcher(context)

    def write(self, relative, content=""):
        (self.scratch / relative).parent.mkdir(parents=True, exist_ok=True)
        (self.scratch / relative).write_text(content, encoding="utf-8")

    def test_sessions_wait_for_harness_pid_then_are_acknowledged(self):
        # Records are validated once harness.pid exists, then acknowledged with an empty file.
        watcher = self.watcher()
        self.write("sessions/0.json", RECORD_TEXT)
        watcher.poll()
        self.assertFalse((self.scratch / "acks" / "0").exists())
        self.write("harness.pid", f"{HARNESS_PID}\n")
        watcher.poll()
        self.assertEqual((self.scratch / "acks" / "0").read_bytes(), b"")
        self.assertEqual(watcher.acked, {"0": ACKED})
        self.assertEqual(watcher.problems, [])

    def test_invalid_record_is_a_problem_and_never_acknowledged(self):
        # A record whose anchor is outside the leader's session would let cleanup signal a stranger.
        self.table.processes[501].sid = 777
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", RECORD_TEXT)
        watcher.poll()
        self.assertFalse((self.scratch / "acks" / "0").exists())
        self.assertTrue(watcher.problems)

    def test_partial_record_waits_then_fails_the_final_scan(self):
        # A record still being written is retried; one that never parses fails the run.
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", RECORD_TEXT[:20])
        watcher.poll()
        self.assertEqual(watcher.problems, [])
        watcher.final_scan()
        self.assertTrue(watcher.problems)

    def test_checkpoint_footprint_path_comes_from_the_request_name(self):
        # The JSON path is derived from `<index>-<label>.request`, never from the harness's field.
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("checkpoints/2-end.request")
        watcher.poll()
        json_path = self.scratch / "checkpoints" / "2-end.json"
        self.assertEqual(self.gate.steps[0].argv,
                         ("/usr/bin/footprint", "-p", str(HARNESS_PID), "-j", str(json_path)))
        self.assertTrue((self.scratch / "checkpoints" / "2-end.done").exists())
        self.assertEqual(watcher.footprints["2-end"]["bytes"], 1234)
        watcher.poll()
        self.assertEqual(len(self.gate.steps), 1)

    def test_failed_or_timed_out_footprint_still_answers_the_request(self):
        # The harness never waits on a failed footprint, and the run stays valid without it.
        for answer in (("TIMEOUT", None), ("FAIL", 1)):
            with self.subTest(answer=answer):
                for leftover in (self.scratch / "checkpoints").iterdir():
                    leftover.unlink()
                self.footprint_answer = answer
                watcher = self.watcher()
                self.write("harness.pid", str(HARNESS_PID))
                self.write("checkpoints/1-idle.request")
                watcher.poll()
                self.assertTrue((self.scratch / "checkpoints" / "1-idle.done").exists())
                record = watcher.footprints["1-idle"]
                self.assertIsNone(record["bytes"])
                self.assertEqual((record["status"], record["exit_code"]), answer)
                self.assertIn("cannot attach", record["output"])
                self.assertEqual(watcher.problems, [])

    def test_unmatched_request_name_is_still_answered(self):
        # A request this script cannot name gets its done file and no footprint run.
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("checkpoints/odd name.request")
        watcher.poll()
        self.assertTrue((self.scratch / "checkpoints" / "odd name.done").exists())
        self.assertEqual(self.gate.steps, [])
        self.assertIsNone(watcher.footprints["odd name"]["bytes"])

    def test_deadline_case_group_kills_only_a_validated_harness_after_go(self):
        # The kill matches a run_step deadline: one group SIGKILL of this run's harness leader.
        watcher = self.watcher(kill_at_go=True)
        self.write("harness.pid", str(HARNESS_PID))
        watcher.poll()
        self.assertEqual(self.table.group_kills, [])
        self.write("go/0")
        watcher.poll()
        self.assertEqual(self.table.group_kills, [HARNESS_PID])
        self.assertTrue(watcher.deadline["sent"])

    def test_harness_name_may_be_cut_by_the_kernel(self):
        # The kernel keeps 15 (Linux) or 16 (macOS) bytes of a command name; it is checked against the launched binary.
        table = FakeTable(harness_process(command="perf_scenarios_a"))
        self.assertIsNone(perf.validate_harness_leader(table, HARNESS_PID, LAUNCH_UNIX_S, "perf_scenarios_alloc"))
        self.assertTrue(perf.validate_harness_leader(table, HARNESS_PID, LAUNCH_UNIX_S, "perf_scenarios_other"))
        self.assertTrue(perf.validate_harness_leader(FakeTable(harness_process(command="perf")), HARNESS_PID,
                                                     LAUNCH_UNIX_S, "perf_scenarios"))

    def test_deadline_case_never_signals_an_unvalidated_harness(self):
        # A pid that is not this run's live harness leader is never signalled.
        cases = {"another program": {"command": "zsh"}, "not a session leader": {"sid": 1},
                 "started before the launch": {"start_unix_s": 900.0}}
        for name, overrides in cases.items():
            with self.subTest(name):
                self.table = FakeTable(harness_process(**overrides))
                watcher = self.watcher(kill_at_go=True)
                self.write("harness.pid", str(HARNESS_PID))
                self.write("go/0")
                watcher.poll()
                self.assertEqual(self.table.group_kills, [])
                self.assertFalse(watcher.deadline["sent"])
                self.assertTrue(watcher.deadline["problem"])
        self.table = FakeTable()
        watcher = self.watcher(kill_at_go=True)
        watcher.poll()
        self.assertEqual(self.table.group_kills, [])


class FrontSamplerTests(unittest.TestCase):
    def test_each_sample_form_is_printed_once_with_its_raw_text(self):
        # Even a passing CI log shows what the runner's lsappinfo printed for each form.
        answers = {"front": ["ASN:0x0-0x1:\n", "ASN:0x0-0x1:\n", "[ NULL ]\n", ""]}

        def run(argv, timeout_s):
            if argv == perf.FRONT_ARGV:
                return command(argv, answers["front"].pop(0))
            return command(argv, '"pid"=55\n')
        printed = set()
        with tempfile.TemporaryDirectory() as temporary:
            log_path = Path(temporary) / "front-samples.log"
            sampler = perf.FrontSampler(log_path, run, printed)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                for _sample in range(4):
                    sampler.sample()
            logged = log_path.read_text(encoding="utf-8").splitlines()
        text = output.getvalue()
        self.assertEqual(text.count("form=front application"), 1)
        self.assertEqual(text.count("form=no front application"), 1)
        self.assertEqual(text.count("form=failed"), 1)
        self.assertIn("[ NULL ]", text)
        self.assertEqual(len(logged), 6)
        self.assertEqual([reading.kind for reading in sampler.readings], ["app", "app", "none", "failed"])
        self.assertEqual(printed, {"app", "none", "failed"})


IDLE_SCENARIO = perf.Scenario("S1", ("default",), "Idle", 120, 30)


def make_outcome(**overrides):
    """A run outcome that classifies as valid; overrides replace single facts."""
    plan = overrides.pop("plan", perf.RunPlan(IDLE_SCENARIO, "default", "head", Path("/b"), HARNESS_HASH))
    fields = dict(plan=plan, evidence=Path("/e"), status="PASS", exit_code=0, result=valid_result(),
                  schema_problems=[], not_exercised=False, focus=perf.FocusVerdict(False, (), [], True),
                  watcher_problems=[], cleanup=perf.CleanupResult(), home=[],
                  deadline={"sent": False, "problem": None, "pid": None}, memory=[], laps=[], footprints={},
                  harness_pid=HARNESS_PID)
    fields.update(overrides)
    return perf.RunOutcome(**fields)


class ClassificationTests(unittest.TestCase):
    def kind(self, **overrides):
        return perf.classify_outcome(make_outcome(**overrides))[0]

    def test_valid_run_needs_every_condition(self):
        # Exit 0, a valid result, focus judged and kept, sessions and cleanup settled.
        self.assertEqual(perf.classify_outcome(make_outcome()), ("valid", []))
        self.assertEqual(self.kind(result=valid_result(finish_session_settled=False)), "cleanup")
        self.assertEqual(self.kind(focus=perf.FocusVerdict(False, (), [], False)), "focus")

    def test_harness_exit_codes(self):
        # 2 refuses, 3 invalidates, 4 is the harness's timeout, 5 is blocked, others are unexpected.
        self.assertEqual(self.kind(exit_code=2, result=None), "refused")
        self.assertEqual(self.kind(exit_code=3, result=valid_result(status="invalid", exit_code=3,
                                                                    notes=["native occlusion change"])), "occluded")
        self.assertEqual(self.kind(exit_code=3, result=valid_result(status="invalid", exit_code=3,
                                                                    notes=["unexpected keyboard input"])), "invalid")
        self.assertEqual(self.kind(exit_code=4, result=None), "timeout")
        self.assertEqual(self.kind(status="TIMEOUT", exit_code=-9, result=None), "timeout")
        self.assertEqual(self.kind(exit_code=5, result=None), "blocked")
        self.assertEqual(self.kind(exit_code=101, result=None), "unexpected")

    def test_not_exercised_without_a_result_is_blocked(self):
        # NOT_EXERCISED with exit 0 and no result.json means the host cannot exercise the scenario.
        self.assertEqual(self.kind(result=None, not_exercised=True), "blocked")
        self.assertEqual(self.kind(result=None), "schema")

    def test_schema_problems_are_never_retried_as_invalid(self):
        # An unmanaged result, another harness's hash, or a missing result is a schema failure.
        self.assertEqual(self.kind(schema_problems=["managed is False, not true"]), "schema")
        self.assertEqual(self.kind(exit_code=3, result=valid_result(status="invalid", exit_code=3),
                                   schema_problems=["harness_hash differs"]), "schema")
        self.assertEqual(self.kind(result=valid_result(status="invalid")), "schema")

    def test_safety_failures_outrank_the_exit_code(self):
        # Cleanup, session records, home writes and focus are judged even when the harness passed.
        failed = perf.CleanupResult()
        failed.unresolved("members left")
        self.assertEqual(self.kind(cleanup=failed), "cleanup")
        self.assertEqual(self.kind(watcher_problems=["session 0 failed validation"]), "session")
        self.assertEqual(self.kind(home=["config.toml: added"]), "home")
        self.assertEqual(self.kind(focus=perf.FocusVerdict(True, (), ["focus theft"], True)), "focus")
        self.assertEqual(self.kind(exit_code=5, result=None, home=["config.toml: added"]), "home")

    def test_deadline_case_is_judged_by_cleanup_not_by_its_result(self):
        # A run ended at GO has no complete result; its kill, cleanup, focus and home writes decide.
        plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("/b"), HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": HARNESS_PID}
        self.assertEqual(self.kind(plan=plan, status="FAIL", exit_code=-9, result=None, deadline=killed), "valid")
        self.assertEqual(self.kind(plan=plan, status="FAIL", exit_code=-9, result=None, deadline=killed,
                                   home=["config.toml: added"]), "home")
        refused = {"sent": False, "problem": "harness pid 900 runs 'zsh'", "pid": HARNESS_PID}
        self.assertEqual(self.kind(plan=plan, deadline=refused), "deadline")
        self.assertEqual(self.kind(plan=plan, exit_code=5, result=None), "blocked")
        self.assertEqual(self.kind(plan=plan), "unexpected")

    def test_verdicts_per_mode(self):
        # The smoke retries only an occlusion; a comparison retries any invalid run and stops on schema.
        self.assertEqual([perf.smoke_verdict(kind) for kind in ("valid", "occluded", "blocked", "focus")],
                         ["pass", "retry", "blocked", "fail"])
        for kind in ("schema", "refused", "cleanup", "home", "session", "timeout", "unexpected", "invalid", "deadline"):
            with self.subTest(kind=kind):
                self.assertEqual(perf.smoke_verdict(kind), "fail")
        self.assertEqual([perf.compare_verdict(kind) for kind in ("valid", "focus", "occluded", "blocked",
                                                                  "schema", "refused")],
                         ["valid", "invalid", "invalid", "blocked", "stop", "stop"])


def wait_for(condition, timeout_s=10.0):
    """Poll a condition the watcher thread satisfies; fail the test if it never holds."""
    deadline = perf.time.monotonic() + timeout_s
    while not condition():
        if perf.time.monotonic() > deadline:
            raise AssertionError("condition never held")
        perf.time.sleep(0.01)


class ExecuteRunTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.temp_root = root / "tmp"
        self.temp_root.mkdir()
        self.home = root / "home" / ".sonicterm"
        self.table = FakeTable(leader(), anchor(), harness_process(), FakeProcess(510, 510, 500, start="11"))
        self.home_write = False

    def tearDown(self):
        self.temporary.cleanup()

    def front_run(self, argv, timeout_s):
        self.assertEqual(tuple(argv), perf.FRONT_ARGV)
        return command(argv, "[ NULL ]\n")

    def fake_harness(self, step, deadline):
        """Act as the harness: register a session, wait for its acknowledgement, then pass or be killed."""
        scratch = Path(step.argv[-1])
        self.assertFalse(scratch.exists())
        (scratch / "logs").mkdir(parents=True)
        (scratch / "logs" / "sonicterm.log.2026-10-02").write_text(memory_line() + "\n", encoding="utf-8")
        (scratch / "harness.pid").write_text(str(HARNESS_PID), encoding="utf-8")
        (scratch / "sessions").mkdir()
        (scratch / "sessions" / "0.json").write_text(RECORD_TEXT, encoding="utf-8")
        wait_for(lambda: (scratch / "acks" / "0").exists())
        if self.home_write:
            self.home.mkdir(parents=True)
            (self.home / "config.toml").write_text("x", encoding="utf-8")
        (scratch / "go").mkdir()
        (scratch / "go" / "0").write_bytes(b"")
        if deadline:
            wait_for(lambda: self.table.group_kills)
            return "FAIL", -9, "killed\n"
        (scratch / "result.json").write_text(json.dumps(valid_result()), encoding="utf-8")
        return "PASS", 0, "harness finished\n"

    def run_plan(self, deadline=False):
        gate = FakeGate(lambda step: self.fake_harness(step, deadline))
        host = perf.Host(gate, self.table, self.front_run, self.home, self.temp_root, set(), {"HOME": "/h"},
                         clock=lambda: LAUNCH_UNIX_S)
        plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("/b/perf_scenarios"), HARNESS_HASH, short=True,
                            smoke=True, kill_at_go=deadline)
        evidence = Path(self.temporary.name) / "evidence" / ("deadline" if deadline else "run")
        with contextlib.redirect_stdout(io.StringIO()):
            outcome = perf.execute_run(plan, host, evidence)
        return outcome, evidence, gate

    def test_managed_run_is_acknowledged_cleaned_and_recorded(self):
        # The whole run: acknowledgement before GO, cleanup after, and the evidence the brief lists.
        outcome, evidence, gate = self.run_plan()
        self.assertEqual(perf.classify_outcome(outcome), ("valid", []))
        self.assertEqual(gate.steps[0].timeout_s, perf.run_timeout_s(IDLE_SCENARIO, smoke=True))
        self.assertIn("--managed", gate.steps[0].argv)
        self.assertEqual(sorted(self.table.kills), [500, 501, 510])
        self.assertEqual(self.table.kills[-1], 501)
        self.assertEqual(len(outcome.memory), 1)
        for name in ("scratch/result.json", "scratch/logs/sonicterm.log.2026-10-02", "scratch/sessions/0.json",
                     "front-samples.log", "cleanup.json", "home-check.json", "outcome.json"):
            with self.subTest(name=name):
                self.assertTrue((evidence / name).is_file())
        self.assertEqual(list(self.temp_root.iterdir()), [])
        self.assertTrue(json.loads((evidence / "cleanup.json").read_text())["settled"])

    def test_deadline_case_kills_the_harness_group_and_cleanup_ends_the_sessions(self):
        # Killed at GO like a step deadline: no result, every PTY session still ended by cleanup.
        outcome, _evidence, _gate = self.run_plan(deadline=True)
        self.assertEqual(self.table.group_kills, [HARNESS_PID])
        self.assertEqual(sorted(self.table.kills), [500, 501, 510])
        self.assertIsNone(outcome.result)
        self.assertEqual(perf.classify_outcome(outcome)[0], "valid")

    def test_home_write_during_a_run_is_reported(self):
        # A write under the SonicTerm home invalidates the run and names the path.
        self.home_write = True
        outcome, evidence, _gate = self.run_plan()
        kind, reasons = perf.classify_outcome(outcome)
        self.assertEqual(kind, "home")
        self.assertTrue(any("config.toml" in reason or "created" in reason for reason in reasons))
        self.assertTrue(json.loads((evidence / "home-check.json").read_text())["violations"])


def timed_outcome(dispatch, uncover=None, latency=None, footprint=None, memory=True):
    """A valid timed run whose workload phase carries the given dispatch samples."""
    phase = dict(valid_result()["phases"][0], dispatch_ms=dispatch)
    result = valid_result(phases=[phase], uncover_ms=uncover, latency=latency)
    samples = [perf.MemorySample(60.0, 200 * 1048576, 100 * 1048576, 1)] if memory else []
    footprints = {"0-end": {"bytes": footprint}} if footprint is not None else {}
    return make_outcome(result=result, memory=samples, footprints=footprints)


def row_for(rows, metric):
    """Return the table row whose metric column matches, failing when it is missing."""
    matches = [row for row in rows if row[1] == metric]
    if len(matches) != 1:
        raise AssertionError(f"{metric!r} not found once in {[row[1] for row in rows]}")
    return matches[0]


class ComparisonTableTests(unittest.TestCase):
    def test_frame_rows_show_pooled_statistics_with_the_per_run_spread(self):
        # Each cell is the pooled figure followed by the min-max of per-run figures.
        base = perf.SideRuns([timed_outcome([1.0, 2.0]), timed_outcome([3.0, 4.0])])
        head = perf.SideRuns([timed_outcome([2.0, 3.0]), timed_outcome([4.0, 5.0])])
        rows = perf.comparison_rows("S1/default", base, head)
        self.assertEqual(row_for(rows, "workload dispatch median (ms)"),
                         ["S1/default", "workload dispatch median (ms)", "2.50 (1.50–3.50)", "3.50 (2.50–4.50)", "+40.0%"])
        self.assertEqual(row_for(rows, "workload dispatch p95 (ms)")[2:], ["4.00 (2.00–4.00)", "5.00 (3.00–5.00)", "+25.0%"])
        self.assertEqual(row_for(rows, "workload presented frames (fps)")[2:], ["2.00 (2.00–2.00)", "2.00 (2.00–2.00)", "+0.0%"])
        self.assertEqual(row_for(rows, "workload CPU (s)")[2], "2.00 (2.00–2.00)")
        self.assertEqual(row_for(rows, "end renderer_total_bytes (MiB)")[2], "100.00 (100.00–100.00)")

    def test_a_field_missing_at_base_prints_n_a(self):
        # A metric only the head reports has no baseline and no change.
        rows = perf.comparison_rows("S12/default", perf.SideRuns([timed_outcome([1.0])]),
                                    perf.SideRuns([timed_outcome([1.0], uncover=50.0)]))
        self.assertEqual(row_for(rows, "uncover (ms)")[2:], ["n/a", "50.00 (50.00–50.00)", "n/a"])

    def test_blocked_base_prints_blocked_with_its_error(self):
        # A base that cannot build or run the scenario names the error instead of numbers.
        rows = perf.comparison_rows("S1/default", perf.SideRuns(blocked="error[E0599]: no method `run_action`"),
                                    perf.SideRuns([timed_outcome([1.0])]))
        self.assertEqual(rows[0][1:3], ["status", "blocked: error[E0599]: no method `run_action`"])
        self.assertEqual(row_for(rows, "workload dispatch median (ms)")[2], "blocked")
        table = perf.render_table(rows)
        self.assertTrue(table.startswith("| Scenario | Metric (unit) | Baseline | PR | Change |\n| --- |"))

    def test_footprint_row_says_how_many_runs_have_it(self):
        # A footprint failure leaves that run out, and the cell counts the runs that have it.
        base = perf.SideRuns([timed_outcome([1.0], footprint=300 * 1048576), timed_outcome([1.0])])
        head = perf.SideRuns([timed_outcome([1.0], footprint=310 * 1048576), timed_outcome([1.0], footprint=320 * 1048576)])
        row = row_for(perf.comparison_rows("S1/default", base, head), "end footprint (MiB)")
        self.assertEqual(row[2], "300.00 (300.00–300.00), 1/2 runs")
        self.assertEqual(row[3], "315.00 (310.00–320.00), 2/2 runs")

    def test_memory_unavailable_in_a_short_run_is_n_a_not_a_failure(self):
        # A run with no memory line before a checkpoint has no memory figure for it.
        rows = perf.comparison_rows("S1/default", perf.SideRuns([timed_outcome([1.0], memory=False)]),
                                    perf.SideRuns([timed_outcome([1.0])]))
        self.assertEqual(row_for(rows, "end renderer_total_bytes (MiB)")[2], "n/a")

    def test_latency_acceptance_needs_80_percent_on_both_sides_within_10_points(self):
        # Coverage below 80% on a side, or sides more than 10 points apart, leaves acceptance open.
        self.assertTrue(perf.latency_acceptance((80, 100), (90, 100)))
        self.assertFalse(perf.latency_acceptance((79, 100), (85, 100)))
        self.assertFalse(perf.latency_acceptance((80, 100), (91, 100)))
        self.assertFalse(perf.latency_acceptance(None, (90, 100)))
        latency = {"samples": [5.0, 6.0], "attributed": 2, "total": 4, "coverage": 0.5}
        rows = perf.comparison_rows("S2/default", perf.SideRuns([timed_outcome([1.0], latency=latency)]),
                                    perf.SideRuns([timed_outcome([1.0], latency=latency)]))
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
        rows = perf.comparison_rows("S2/default", perf.SideRuns([timed_outcome([1.0], latency=latency)]),
                                    perf.SideRuns([timed_outcome([1.0], latency=latency)]))
        self.assertEqual(row_for(rows, "keypress-to-present latency median (ms)")[2], "5.00 (5.00–5.00)")
        self.assertEqual(row_for(rows, "latency attribution coverage (%)")[2], "66.7% (2/3)")

    def test_cells_escape_table_pipes(self):
        # An error text holding `|` cannot break the Markdown table.
        table = perf.render_table([["S1", "status", "blocked: a | b", "n/a", "n/a"]])
        self.assertIn("blocked: a \\| b", table)

    def test_laps_table_pools_each_lap_separately(self):
        # Laps runs have their own table: each window and lap, pooled with its per-run spread.
        def laps_run(present):
            samples = [perf.RenderTimingSample(1.0, "main", {"total": 4.0, "present": value}) for value in present]
            return make_outcome(laps=samples)
        rows = perf.laps_rows("S1/default", perf.SideRuns([laps_run([1.0, 3.0])]), perf.SideRuns([laps_run([2.0, 4.0])]))
        self.assertEqual(row_for(rows, "main present median (ms)")[2:], ["2.00 (2.00–2.00)", "3.00 (3.00–3.00)", "+50.0%"])
        self.assertEqual(row_for(rows, "main total p95 (ms)")[2], "4.00 (4.00–4.00)")


def outcome_of(kind):
    """Build a factory that answers a smoke plan with an outcome of the named kind."""
    def build(plan):
        killed = {"sent": True, "problem": None, "pid": HARNESS_PID}
        occlusion = valid_result(status="invalid", exit_code=3, notes=["native occlusion change"])
        if kind == "valid" and plan.kill_at_go:
            return make_outcome(plan=plan, status="FAIL", exit_code=-9, result=None, deadline=killed)
        if kind == "valid":
            return make_outcome(plan=plan)
        if kind == "occluded":
            return make_outcome(plan=plan, exit_code=3, result=occlusion)
        if kind == "blocked":
            return make_outcome(plan=plan, exit_code=5, result=None)
        if kind == "schema":
            return make_outcome(plan=plan, schema_problems=["managed is False, not true"])
        if kind == "theft":
            return make_outcome(plan=plan, focus=perf.FocusVerdict(True, (), ["focus theft"], True))
        if kind == "home":
            return make_outcome(plan=plan, home=["config.toml: added"])
        if kind == "survivor":
            cleanup = perf.CleanupResult()
            cleanup.unresolved("members left", [perf.ProcessInfo(510, 500, 500, "11", 1001.0, "sleep")])
            return make_outcome(plan=plan, cleanup=cleanup)
        if kind == "bound":
            return make_outcome(plan=plan, status="TIMEOUT", exit_code=-9, result=None)
        raise AssertionError(kind)
    return build


class SmokeTests(unittest.TestCase):
    SCENARIOS = {"S1": IDLE_SCENARIO, "S3": perf.Scenario("S3", ("default",), "Flood", 120, 30)}

    def run_cases(self, answers):
        """Drive the smoke's cases; each case name answers from its queue of outcome kinds."""
        calls = []

        def run_case(plan, evidence):
            name = plan.scenario.id + ("-deadline" if plan.kill_at_go else "")
            calls.append(name)
            self.assertTrue(plan.short and plan.smoke)
            queue = answers.get(name, ["valid"])
            return outcome_of(queue.pop(0) if len(queue) > 1 else queue[0])(plan)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code, reasons = perf.smoke_cases(self.SCENARIOS, Path("/b"), HARNESS_HASH, run_case, Path("/e"))
        return code, reasons, calls

    def test_three_valid_cases_pass(self):
        # S1, S3 and the deadline case each need one valid run; no timing is asserted.
        code, reasons, calls = self.run_cases({})
        self.assertEqual((code, reasons), (0, []))
        self.assertEqual(calls, ["S1", "S3", "S1-deadline"])

    def test_an_occlusion_is_retried_within_the_bound(self):
        # An environmental invalidation retries; the case passes once a run is valid.
        code, _reasons, calls = self.run_cases({"S1": ["occluded", "occluded", "valid"]})
        self.assertEqual(code, 0)
        self.assertEqual(calls.count("S1"), 3)

    def test_no_valid_exercised_run_after_the_retries_is_blocked(self):
        # Four occluded runs, or a scenario the harness cannot run, exit 3 after every case ran.
        code, reasons, calls = self.run_cases({"S1": ["occluded"]})
        self.assertEqual(code, 3)
        self.assertEqual(calls.count("S1"), perf.RETRY_LIMIT + 1)
        self.assertIn("S3", calls)
        self.assertTrue(reasons)
        self.assertEqual(self.run_cases({"S3": ["blocked"]})[0], 3)

    def test_safety_and_schema_failures_exit_1_at_once(self):
        # A schema, focus, cleanup or home failure, or a run that reached its bound, fails immediately.
        for kind in ("schema", "theft", "survivor", "home", "bound"):
            with self.subTest(kind=kind):
                code, reasons, calls = self.run_cases({"S1": [kind]})
                self.assertEqual(code, 1)
                self.assertEqual(calls, ["S1"])
                self.assertTrue(reasons)
        code, _reasons, calls = self.run_cases({"S1-deadline": ["survivor"]})
        self.assertEqual((code, calls), (1, ["S1", "S3", "S1-deadline"]))

    def test_deadline_case_occlusion_is_retried(self):
        # The deadline case can be occluded before GO like any other run.
        code, _reasons, calls = self.run_cases({"S1-deadline": ["occluded", "valid"]})
        self.assertEqual(code, 0)
        self.assertEqual(calls.count("S1-deadline"), 2)

    def test_a_listless_scenario_fails(self):
        # The smoke cannot pass without the scenarios it is defined by.
        with contextlib.redirect_stdout(io.StringIO()):
            code, _reasons = perf.smoke_cases({"S1": IDLE_SCENARIO}, Path("/b"), HARNESS_HASH, lambda plan, evidence: None,
                                              Path("/e"))
        self.assertEqual(code, 1)

    def test_evidence_directory_is_exported_kept_on_failure_and_removed_on_a_pass(self):
        # CI uploads the directory when the job fails, so a failure keeps it and prints its path.
        for code in (0, 1, 3):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as temporary:
                env_file = Path(temporary) / "github-env"
                seen = []

                def runner(evidence):
                    seen.append(evidence)
                    self.assertTrue(evidence.is_dir())
                    return code, ([] if code == 0 else ["reason"])
                output = io.StringIO()
                with mock.patch.object(perf.sys, "platform", "darwin"), contextlib.redirect_stdout(output), \
                        contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(perf.smoke_main({"GITHUB_ENV": str(env_file)}, runner), code)
                evidence = seen[0]
                self.assertTrue(evidence.name.startswith("sonicterm-perf-evidence-"))
                self.assertIn(f"SONICTERM_PERF_EVIDENCE_DIR={evidence}\n", env_file.read_text())
                self.assertIn(str(evidence), output.getvalue())
                self.assertEqual(evidence.exists(), code != 0)
                if evidence.exists():
                    perf.shutil.rmtree(evidence)

    def test_a_host_that_cannot_run_the_smoke_is_blocked(self):
        # The harness and lsappinfo exist only on macOS, so another host exits 3, never 0.
        with mock.patch.object(perf.sys, "platform", "linux"), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(perf.smoke_main({}, lambda evidence: self.fail("ran")), 3)


class RunSetTests(unittest.TestCase):
    def run_set(self, answers, runs=2, base_blocked=None):
        """Drive one run set; each side answers from its queue of outcome kinds, repeating the last."""
        calls = []
        plans = {side: perf.RunPlan(IDLE_SCENARIO, "default", side, Path(f"/{side}"), HARNESS_HASH) for side in perf.SIDES}

        def run_case(plan, evidence):
            calls.append(plan.side)
            queue = answers[plan.side]
            kind = queue.pop(0) if len(queue) > 1 else queue[0]
            if kind == "grid":
                return make_outcome(plan=plan, result=valid_result(grid={"cols": 200, "rows": 50}))
            return outcome_of(kind)(plan)
        with contextlib.redirect_stdout(io.StringIO()):
            result = perf.run_set("S1/default", plans, base_blocked, runs, run_case, Path("/e"))
        return result, calls

    def test_valid_runs_alternate_and_fill_both_sides(self):
        # Runs alternate A B B A until each side has its valid runs.
        result, calls = self.run_set({"base": ["valid"], "head": ["valid"]})
        self.assertEqual(calls, ["base", "head", "head", "base"])
        self.assertEqual((len(result.base.outcomes), len(result.head.outcomes)), (2, 2))
        self.assertEqual(perf.comparison_exit([result]), 0)

    def test_a_grid_that_differs_from_the_pair_is_invalid_and_retried(self):
        # Base and head must measure the same grid, or the pair compares different screens.
        result, calls = self.run_set({"base": ["valid"], "head": ["grid", "valid"]}, runs=1)
        self.assertEqual(calls, ["base", "head", "head"])
        self.assertEqual(result.attempts[1][2], "grid")
        self.assertEqual(len(result.head.outcomes), 1)

    def test_base_that_cannot_build_is_blocked_and_the_head_still_runs(self):
        # The base's build error is printed for the scenario; the head keeps its measured runs.
        result, calls = self.run_set({"base": ["valid"], "head": ["valid"]}, base_blocked="error[E0599]")
        self.assertEqual(calls, ["head", "head"])
        self.assertEqual(result.base.blocked, "error[E0599]")
        rows = perf.comparison_rows("S1/default", result.base, result.head)
        self.assertEqual(rows[0][2], "blocked: error[E0599]")
        self.assertEqual(perf.comparison_exit([result]), 0)

    def test_a_scenario_the_base_harness_cannot_run_retires_the_base(self):
        # Exit 5 at base blocks that side only.
        result, calls = self.run_set({"base": ["blocked"], "head": ["valid"]})
        self.assertEqual(calls, ["base", "head", "head"])
        self.assertTrue(result.base.blocked)
        self.assertEqual(len(result.head.outcomes), 2)

    def test_head_that_exhausts_its_retries_fails_the_comparison(self):
        # A fourth invalid head run fails the scenario, and the table says so.
        result, calls = self.run_set({"base": ["valid"], "head": ["theft"]}, runs=1)
        self.assertEqual(calls.count("head"), perf.RETRY_LIMIT + 1)
        self.assertIn("focus", result.head.failed)
        self.assertEqual(result.head.outcomes, [])
        self.assertTrue(perf.comparison_rows("S1/default", result.base, result.head)[0][3].startswith("failed: "))
        self.assertEqual(perf.comparison_exit([result]), 1)

    def test_base_that_exhausts_its_retries_is_blocked(self):
        # A base that cannot produce valid runs is reported blocked; the head is still measured.
        result, _calls = self.run_set({"base": ["theft"], "head": ["valid"]}, runs=1)
        self.assertIn("no 1 valid runs", result.base.blocked)
        self.assertIsNone(result.head.failed)
        self.assertEqual(len(result.head.outcomes), 1)

    def test_head_blocked_is_blocked_and_a_schema_failure_stops(self):
        # A head that cannot run the scenario exits 3; a schema failure stops the comparison at once.
        result, _calls = self.run_set({"base": ["valid"], "head": ["blocked"]})
        self.assertEqual(perf.comparison_exit([result]), 3)
        with self.assertRaises(perf.StopComparison) as raised:
            self.run_set({"base": ["valid"], "head": ["schema"]})
        self.assertIn("managed", str(raised.exception))


class FakeGit:
    """Answers `git worktree` commands by creating and removing directories."""

    def __init__(self, fail_add=False):
        self.calls = []
        self.fail_add = fail_add

    def __call__(self, argv):
        self.calls.append(tuple(argv))
        if tuple(argv[1:3]) == ("worktree", "add"):
            if self.fail_add:
                return command(argv, "", exit_code=128, stderr="fatal: invalid reference\n")
            Path(argv[-2]).mkdir(parents=True)
        elif tuple(argv[1:3]) == ("worktree", "remove"):
            perf.shutil.rmtree(argv[-1])
        return command(argv, "")


class WorktreeTests(unittest.TestCase):
    SHA = "78766228f0a0e6dd12bb0c6c4a251ea0eef0f13b"

    def test_each_side_gets_its_own_detached_worktree_even_for_one_sha(self):
        # Two refs with one SHA still get two trees, so the overlay never targets its own source.
        with tempfile.TemporaryDirectory() as temporary:
            git = FakeGit()
            worktrees = perf.Worktrees(git, Path(temporary) / "work")
            base, head = worktrees.create("base", self.SHA), worktrees.create("head", self.SHA)
            self.assertNotEqual(base, head)
            self.assertEqual(git.calls, [("git", "worktree", "add", "--detach", str(base), self.SHA),
                                         ("git", "worktree", "add", "--detach", str(head), self.SHA)])

    def test_existing_path_is_never_reused_and_only_created_trees_are_removed(self):
        # Removal touches only what this run created, with --force because the overlay changed it.
        with tempfile.TemporaryDirectory() as temporary:
            git = FakeGit()
            worktrees = perf.Worktrees(git, Path(temporary) / "work")
            (Path(temporary) / "work" / "base").mkdir(parents=True)
            with self.assertRaises(ValueError):
                worktrees.create("base", self.SHA)
            self.assertEqual(git.calls, [])
            head = worktrees.create("head", self.SHA)
            self.assertEqual(worktrees.remove(), [])
            self.assertEqual(git.calls[-1], ("git", "worktree", "remove", "--force", str(head)))
            self.assertEqual(sum(call[2] == "remove" for call in git.calls), 1)
            self.assertTrue((Path(temporary) / "work" / "base").is_dir())

    def test_failed_add_is_not_recorded_for_removal(self):
        # A worktree git refused to create is never removed.
        with tempfile.TemporaryDirectory() as temporary:
            git = FakeGit(fail_add=True)
            worktrees = perf.Worktrees(git, Path(temporary) / "work")
            with self.assertRaises(ValueError):
                worktrees.create("head", self.SHA)
            worktrees.remove()
            self.assertEqual([call[2] for call in git.calls], ["add"])

    def test_work_directory_is_under_the_ignored_target_tree(self):
        # Worktrees and target directories live under target/perf-compare/, which git ignores.
        self.assertEqual(perf.work_directory("stamp").parent, perf.ROOT / "target" / "perf-compare")


class HostBlockTests(unittest.TestCase):
    OUTPUTS = {
        "model": "Mac14,9\n", "cpu": "Apple M2 Pro\n", "memory": "34359738368\n",
        "os": "ProductName:\t\tmacOS\nProductVersion:\t\t26.6.2\nBuildVersion:\t\t25G123\n",
        "displays": ("Graphics/Displays:\n\n    Apple M2 Pro:\n\n      Chipset Model: Apple M2 Pro\n"
                     "      Displays:\n        Color LCD:\n          Resolution: 3024 x 1964 Retina\n"
                     "          UI Looks like: 1512 x 982 @ 120.00Hz\n          Main Display: Yes\n"),
        "power": "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; present: true\n",
        "power_settings": "System-wide power settings:\nCurrently in use:\n standby              1\n lowpowermode         0\n",
    }

    def test_host_block_names_machine_os_gpu_display_and_power(self):
        # The PR's host block names what the brief lists, from the raw command outputs.
        text = "\n".join(perf.host_block(self.OUTPUTS))
        for expected in ("Mac14,9", "Apple M2 Pro", "32 GiB", "macOS 26.6.2 (25G123)", "3024 x 1964 Retina",
                         "120.00Hz", "scale 2.00", "AC Power", "Low Power Mode off"):
            with self.subTest(expected=expected):
                self.assertIn(expected, text)

    def test_missing_outputs_read_unavailable(self):
        # A command that failed leaves its figure unavailable rather than guessed.
        text = "\n".join(perf.host_block({}))
        self.assertIn("unavailable", text)
        self.assertNotIn("None", text)

    def test_document_holds_the_tables_host_and_collapsed_details(self):
        # Laps and allocation tables appear only when those run sets ran.
        rows = [["S1/default", "status", "5 valid runs", "5 valid runs", ""]]
        document = perf.comparison_document(rows, [], [], ["- OS: macOS"], ["- Base: main 7876"])
        self.assertIn(perf.TABLE_HEADER, document)
        self.assertIn("### Host\n\n- OS: macOS", document)
        self.assertIn("<details><summary>", document)
        self.assertNotIn("Laps", document)
        self.assertIn("Laps", perf.comparison_document(rows, rows, [], [], []))


class CliTests(unittest.TestCase):
    def parse(self, *argv):
        with contextlib.redirect_stderr(io.StringIO()):
            return perf.parse_args(list(argv))

    def test_scenario_values_accumulate_in_order(self):
        # `--scenario all S10/sync` and repeated flags both reach the selection in the order given.
        args = self.parse("--base", "main", "--head", "HEAD", "--scenario", "S10/sync", "all", "--scenario", "S1")
        self.assertEqual(args.scenario, ["S10/sync", "all", "S1"])
        self.assertIsNone(self.parse("--base", "main", "--head", "HEAD").scenario)

    def test_invalid_combinations_are_refused(self):
        # The smoke takes no comparison option, a comparison needs both refs, and runs are positive.
        for argv in (("--smoke", "--base", "main"), ("--smoke", "--runs", "5"), ("--base", "main"), (),
                     ("--base", "main", "--head", "HEAD", "--runs", "0")):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                self.parse(*argv)
        self.assertTrue(self.parse("--smoke").smoke)

    def test_help_exits_zero(self):
        # `--help` is the one invocation that may run anywhere without building anything.
        output = io.StringIO()
        with contextlib.redirect_stdout(output), self.assertRaises(SystemExit) as raised:
            perf.parse_args(["--help"])
        self.assertEqual(raised.exception.code, 0)
        self.assertIn("--smoke", output.getvalue())

    def test_main_routes_the_smoke_and_blocks_comparisons_off_macos(self):
        # The smoke goes to its driver; a comparison on another host is BLOCKED, never a pass.
        with mock.patch.object(perf, "smoke_main", return_value=7) as smoke:
            self.assertEqual(perf.main(["--smoke"]), 7)
        smoke.assert_called_once()
        with mock.patch.object(perf.sys, "platform", "linux"), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(perf.main(["--base", "main", "--head", "HEAD"]), 3)


class CompareDriverTests(unittest.TestCase):
    SHAS = {"main": "1" * 40, "HEAD": "2" * 40}

    def compare(self, base_build="PASS"):
        """Drive the comparison with fake git, Cargo and runs; return the exit code, gate, git calls and paths."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        work, out = root / "work", root / "out"
        out.mkdir()
        git_calls = []

        def host_run(argv, timeout_s=perf.GIT_TIMEOUT_S):
            git_calls.append(tuple(argv))
            if tuple(argv[:2]) == ("git", "rev-parse"):
                return command(argv, self.SHAS[argv[-1].split("^")[0]] + "\n")
            if tuple(argv[:3]) == ("git", "worktree", "add"):
                tree = Path(argv[-2])
                manifest = HEAD_MANIFEST if tree.name == "head" else BASE_MANIFEST
                (tree / perf.APP_MANIFEST).parent.mkdir(parents=True)
                (tree / perf.APP_MANIFEST).write_text(manifest, encoding="utf-8")
                if tree.name == "head":
                    (tree / perf.HARNESS_DIRECTORY).mkdir(parents=True)
                    (tree / perf.HARNESS_DIRECTORY / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
            return command(argv, "")

        def answer(step):
            if step.id.startswith("build-"):
                side, example = step.id.split("-", 2)[1:]
                if side == "base" and base_build != "PASS":
                    return "FAIL", 101, "error[E0599]: no method named `run_action` found\n"
                artifact = {"reason": "compiler-artifact", "target": {"name": example, "kind": ["example"]},
                            "executable": f"/{side}/{example}"}
                return "PASS", 0, json.dumps(artifact) + "\n"
            return "PASS", 0, json.dumps(LIST_JSON) + "\n"
        gate = FakeGate(answer)
        args = perf.parse_args(["--base", "main", "--head", "HEAD", "--scenario", "S1", "--runs", "1"])
        plans = []

        def fake_run(plan, host, evidence):
            plans.append(plan)
            return display_run(60000)(plan)
        with mock.patch.object(perf, "production_host", return_value=None), \
                mock.patch.object(perf, "execute_run", side_effect=fake_run), \
                contextlib.redirect_stdout(io.StringIO()):
            code = perf._compare(args, gate, out, work, perf.Worktrees(host_run, work), host_run)
        return code, gate, git_calls, plans, work, out

    def test_comparison_builds_each_tree_with_its_own_target_and_writes_the_table(self):
        # Head first, one CARGO_TARGET_DIR per ref, the head's harness on both trees, runs alternating.
        code, gate, git_calls, plans, work, out = self.compare()
        self.assertEqual(code, 0)
        adds = [call for call in git_calls if call[1:3] == ("worktree", "add")]
        self.assertEqual(adds, [("git", "worktree", "add", "--detach", str(work / "head"), self.SHAS["HEAD"]),
                                ("git", "worktree", "add", "--detach", str(work / "base"), self.SHAS["main"])])
        self.assertEqual(gate.roots[:2], [work / "head", work / "base"])
        self.assertEqual([environ["CARGO_TARGET_DIR"] for environ in gate.environs[:2]],
                         [str(work / "target-head"), str(work / "target-base")])
        self.assertIn("--release", gate.steps[0].argv)
        self.assertEqual(perf.tree_harness_hash(work / "base"), perf.tree_harness_hash(work / "head"))
        self.assertEqual([plan.side for plan in plans], ["base", "head"])
        self.assertEqual({plan.binary for plan in plans}, {Path("/base/perf_scenarios"), Path("/head/perf_scenarios")})
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S1/default | status | 1 valid run | 1 valid run |", document)
        self.assertIn(self.SHAS["main"], document)
        self.assertIn(perf.tree_harness_hash(work / "head"), document)
        self.assertIn("- Measurement display: Built-in Display, 60 Hz, scale 2", document)

    def test_a_base_that_cannot_build_prints_blocked_with_the_error(self):
        # The head is still measured, and the table names the base's compiler error.
        code, _gate, _git_calls, plans, _work, out = self.compare(base_build="FAIL")
        self.assertEqual(code, 0)
        self.assertEqual([plan.side for plan in plans], ["head"])
        self.assertIn("blocked: base cannot build perf_scenarios: error[E0599]", (out / "comparison.md").read_text())


def display_run(refresh_mhz, scale=2.0, name="Built-in Display"):
    """A factory for a valid run measured on one display; a None rate means the rate was not reported."""
    def build(plan):
        monitor = {"name": name, "refresh_rate_millihertz": refresh_mhz, "scale_factor": scale}
        return make_outcome(plan=plan, result=valid_result(monitor=monitor))
    return build


def no_display(plan):
    """A valid run whose result reported no measurement display."""
    return make_outcome(plan=plan, result=valid_result(monitor=None))


class DisplayCheckTests(unittest.TestCase):
    def run_set(self, answers, runs=1, display=None):
        """Drive one run set whose answers are outcome factories; a side's last factory repeats."""
        calls = []
        plans = {side: perf.RunPlan(IDLE_SCENARIO, "default", side, Path(f"/{side}"), HARNESS_HASH)
                 for side in perf.SIDES}

        def run_case(plan, evidence):
            calls.append(plan.side)
            queue = answers[plan.side]
            return (queue.pop(0) if len(queue) > 1 else queue[0])(plan)
        with contextlib.redirect_stdout(io.StringIO()):
            result = perf.run_set("S1/default", plans, None, runs, run_case, Path("/e"), display=display)
        return result, calls

    def test_another_refresh_rate_or_scale_is_invalid_and_retried(self):
        # Every valid run must share the first valid run's refresh rate and scale; the reason names both displays.
        for other, named in ((display_run(75000), "Built-in Display, 75 Hz, scale 2"),
                             (display_run(60000, scale=1.0), "Built-in Display, 60 Hz, scale 1")):
            with self.subTest(named=named):
                result, calls = self.run_set({"base": [display_run(60000)], "head": [other, display_run(60000)]})
                self.assertEqual(calls, ["base", "head", "head"])
                _side, _evidence, kind, why = result.attempts[1]
                self.assertEqual(kind, "display")
                self.assertIn(named, why[0])
                self.assertIn("Built-in Display, 60 Hz, scale 2", why[0])
                self.assertEqual(perf.compare_verdict(kind), "invalid")
                self.assertEqual(len(result.head.outcomes), 1)

    def test_the_display_name_alone_does_not_matter(self):
        # Two displays that share a refresh rate and scale pace frames alike.
        result, _calls = self.run_set({"base": [display_run(60000, name="DELL U2720Q")],
                                       "head": [display_run(60000, name="LG HDR 4K")]})
        self.assertEqual([attempt[2] for attempt in result.attempts], ["valid", "valid"])

    def test_an_unknown_display_is_not_checked(self):
        # A run that reported no display, or no refresh rate, is accepted and its display reads unknown.
        for unknown in (no_display, display_run(None, scale=1.0)):
            with self.subTest(unknown=unknown):
                result, _calls = self.run_set({"base": [display_run(60000)], "head": [unknown]})
                self.assertEqual([attempt[2] for attempt in result.attempts], ["valid", "valid"])
        self.assertEqual(perf.describe_display(perf.display_of(valid_result(monitor=None))), "unknown")

    def test_the_reference_is_the_first_known_display_across_the_comparison(self):
        # One reference spans every run set of a comparison; a run that reported no display never sets it.
        display = perf.DisplayReference()
        self.run_set({"base": [no_display], "head": [display_run(60000)]}, display=display)
        self.assertEqual(display.monitor["refresh_rate_millihertz"], 60000)
        result, calls = self.run_set({"base": [display_run(75000), display_run(60000)],
                                      "head": [display_run(60000)]}, display=display)
        self.assertEqual(calls, ["base", "head", "base"])
        self.assertEqual(result.attempts[0][2], "display")

    def test_the_smoke_does_not_check_the_display(self):
        # The smoke checks schema, focus safety and cleanup only, so runs on different displays still pass.
        rates = iter([60000, 75000])

        def run_case(plan, evidence):
            if plan.kill_at_go:
                return outcome_of("valid")(plan)
            return display_run(next(rates))(plan)
        with contextlib.redirect_stdout(io.StringIO()):
            code, reasons = perf.smoke_cases(SmokeTests.SCENARIOS, Path("/b"), HARNESS_HASH, run_case, Path("/e"))
        self.assertEqual((code, reasons), (0, []))

    def test_monitor_types_are_checked_when_present(self):
        # The display may be absent or null; when present, each field has its documented type.
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 60000, "scale_factor": 2.0}
        for accepted in (valid_result(), valid_result(monitor=None), valid_result(monitor=monitor),
                         valid_result(monitor=dict(monitor, name=None, refresh_rate_millihertz=None))):
            with self.subTest(accepted=accepted.get("monitor", "absent")):
                self.assertEqual(perf.validate_result(accepted, HARNESS_HASH, 0), [])
        for broken in ("60 Hz", dict(monitor, name=5), dict(monitor, refresh_rate_millihertz="60000"),
                       dict(monitor, refresh_rate_millihertz=True), dict(monitor, refresh_rate_millihertz=60000.0),
                       dict(monitor, scale_factor=None), {"name": "x", "scale_factor": 2.0}):
            with self.subTest(broken=broken):
                self.assertTrue(perf.validate_result(valid_result(monitor=broken), HARNESS_HASH, 0))

    def test_host_block_names_the_measurement_display(self):
        # The display the valid runs shared is printed; with none known it reads unknown.
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 59940, "scale_factor": 2.0}
        self.assertIn("- Measurement display: Built-in Display, 59.94 Hz, scale 2",
                      perf.host_block(HostBlockTests.OUTPUTS, monitor))
        self.assertIn("- Measurement display: unknown", perf.host_block(HostBlockTests.OUTPUTS))
        unnamed = dict(monitor, name=None, refresh_rate_millihertz=75000, scale_factor=1.0)
        self.assertIn("- Measurement display: unnamed display, 75 Hz, scale 1", perf.host_block({}, unnamed))


if __name__ == "__main__":
    unittest.main(verbosity=2)
