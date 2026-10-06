#!/usr/bin/env python3
"""Contracts of the performance comparison script, driven entirely by fakes.

No test builds, launches or signals a real process: the process table, the
`lsappinfo` and `footprint` commands, the gate's `run_step` and the clock are
replaced, so the suite runs unchanged on macOS, Windows and Linux.
"""

from __future__ import annotations

import contextlib
import copy
import dataclasses
import hashlib
import importlib.util
import inspect
import io
import json
import os
import re
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import time
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
        # Only `[ NULL ]` and a null ASN followed by a character that is not a hex digit mean no front application.
        for stdout in ("[ NULL ]", "  [ NULL ] \n", "ASN:0x0-0x0-NULL\n", "ASN:0x0-0x0:\n"):
            with self.subTest(stdout=stdout):
                reading, lookups = self.classify(command(FRONT_ARGV, stdout))
                self.assertEqual(reading.kind, "none")
                self.assertEqual(lookups, [])

    def test_a_cut_off_null_asn_fails_the_sample(self):
        # A bare `ASN:0x0-0x0` is cut-off output, not a null form. It fails the sample, so it never stands
        # for no front application before a harness activation.
        reading, lookups = self.classify(command(FRONT_ARGV, "ASN:0x0-0x0\n"))
        self.assertEqual(reading.kind, "failed")
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

    def test_a_low_half_without_0x_is_looked_up_with_0x(self):
        # The macos-14 runner prints `ASN:0x0-c00c:` and its lookup of that spelling prints nothing,
        # so the fake answers only the `0x` spelling, as the lookup must ask for it.
        for printed, spelled in (("ASN:0x0-c00c:", "ASN:0x0-0xc00c:"), ("ASN:0x0-24024:", "ASN:0x0-0x24024:")):
            with self.subTest(printed=printed):
                lookups = []

                def lookup(asn, spelled=spelled):
                    lookups.append(asn)
                    return command(perf.front_pid_argv(asn), '"pid"=4242\n' if asn == spelled else "")
                reading = perf.classify_front(command(FRONT_ARGV, printed + "\n"), lookup)
                self.assertEqual((reading.kind, reading.pid), ("app", 4242))
                self.assertEqual(lookups, [spelled])

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

    def test_only_a_github_hosted_runner_lacks_a_user_session(self):
        # The smoke and a comparison on a GitHub-hosted runner have no user; GITHUB_ACTIONS=true alone does not
        # prove that, because a self-hosted runner may have one, and a desk run stays strict.
        hosted = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
        self.assertFalse(perf.has_user_session(hosted))
        for environ in ({"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "self-hosted"}, {"GITHUB_ACTIONS": "true"},
                        {"RUNNER_ENVIRONMENT": "github-hosted"},
                        {"GITHUB_ACTIONS": "false", "RUNNER_ENVIRONMENT": "github-hosted"}, {}):
            with self.subTest(environ=environ):
                self.assertTrue(perf.has_user_session(environ))

    def test_the_log_names_the_focus_rule_and_the_runner(self):
        # The first CI log shows that RUNNER_ENVIRONMENT reached the run, and which focus rule judged it.
        hosted = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
        line = perf.focus_rule_line(hosted)
        self.assertIn("GITHUB_ACTIONS=true RUNNER_ENVIRONMENT=github-hosted", line)
        self.assertIn("a GitHub-hosted runner has no user session", line)
        for environ in (dict(hosted, RUNNER_ENVIRONMENT="self-hosted"), {}):
            with self.subTest(environ=environ):
                self.assertIn("strict", perf.focus_rule_line(environ))
        self.assertIn("RUNNER_ENVIRONMENT=unset", perf.focus_rule_line({}))

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
                 unreadable=False, survives_kill=False, ppid=0):
        self.pid, self.pgid, self.sid, self.start, self.ppid = pid, pgid, sid, start, ppid
        self.command, self.start_unix_s = command, start_unix_s
        self.unreadable, self.survives_kill, self.alive = unreadable, survives_kill, True


class FakeTable:
    """A process table the cleanup code reads and signals; it records every signal sent."""

    def __init__(self, *processes):
        self.processes = {process.pid: process for process in processes}
        self.kills = []
        self.group_kills = []
        # (pid, start) of every Windows TerminateProcess the table accepted.
        self.terminations = []
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
                                process.start_unix_s, process.command, process.ppid)

    def terminate(self, pid, start):
        process = self.processes.get(pid)
        if process is None or not process.alive:
            return "gone"
        if process.start != start:
            return "stale"
        self.terminations.append((pid, start))
        process.alive = False
        return "sent"

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


def memory_line(stamp=STAMP, resident="123", renderer="456", session="789", prefix="memory: ",
                grid=""):
    """One `memory snapshot` line as the file layer writes it; `grid` adds the grid fields."""
    return (f"{stamp}  INFO {prefix}memory snapshot process_private_committed_bytes=unsupported "
            f"process_resident_bytes={resident} process_virtual_bytes=unsupported "
            f"session_total_bytes={session} {grid}renderer_total_bytes={renderer} renderers=[main warm] "
            f"allocator_state=unsupported")


GRID_FIELDS = ("grid_visible_bytes=3000000 grid_history_bytes=6000000 grid_alternate_bytes=0 "
               "panes_total=3 panes_sampled=3 panes_contended=0 ")


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

    def test_grid_fields_parse_and_give_bytes_per_pane(self):
        # Visible + history + alternate over the panes sampled is the per-pane grid figure.
        sample = perf.parse_memory_line(memory_line(grid=GRID_FIELDS))
        self.assertEqual((sample.grid_visible_bytes, sample.grid_history_bytes,
                          sample.grid_alternate_bytes, sample.panes_sampled), (3000000, 6000000, 0, 3))
        self.assertEqual(sample.grid_bytes_per_pane(), 3000000)

    def test_missing_grid_fields_or_no_sampled_pane_give_no_per_pane_figure(self):
        # An older line without the fields, a malformed field, or zero sampled panes reads unavailable.
        for grid in ("", GRID_FIELDS.replace("grid_history_bytes=6000000", "grid_history_bytes=x"),
                     GRID_FIELDS.replace("panes_sampled=3", "panes_sampled=0")):
            with self.subTest(grid=grid):
                sample = perf.parse_memory_line(memory_line(grid=grid))
                self.assertIsNotNone(sample, "the required totals still parse")
                self.assertIsNone(sample.grid_bytes_per_pane())

    def test_each_renderers_glyph_atlas_facts_parse_in_order_with_every_fit_label(self):
        # The renderer breakdown carries six glyph atlas facts per renderer under its identity; every fit
        # label parses, an older line without them gives no facts, and an unknown fit label is not a fact.
        def facts(dim, fit):
            return (f"glyph_atlas_dim={dim} glyph_atlas_packed_pixels=4000 glyph_atlas_growths=1 "
                    f"glyph_atlas_evictions=0 glyph_atlas_fit={fit} glyph_atlas_max_tile=25x16")
        for fit in ("256", "512", "1024", "2048", "no_headroom", "does_not_fit", "evicted"):
            with self.subTest(fit=fit):
                line = memory_line().replace(
                    "renderers=[main warm]",
                    f'renderers="main[1] total=9/1 {facts(512, fit)}; warm[0] total=1/1 {facts(256, "256")}"')
                atlases = perf.parse_memory_line(line).glyph_atlases
                # Each renderer's facts carry its `role[label]` identity from the breakdown.
                self.assertEqual(atlases, (perf.GlyphAtlasFacts("main[1]", 512, 4000, 1, 0, fit, (25, 16)),
                                           perf.GlyphAtlasFacts("warm[0]", 256, 4000, 1, 0, "256", (25, 16))))
        self.assertEqual(perf.parse_memory_line(memory_line()).glyph_atlases, ())
        unknown = memory_line().replace("renderers=[main warm]", f"renderers={facts(512, 'tiny')}")
        self.assertEqual(perf.parse_memory_line(unknown).glyph_atlases, ())

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


def without_presenter(result):
    """`result` with its presenter record removed, for a test of a run that recorded none."""
    result.pop("presenter", None)
    return result


# The presenter a macOS run on the hardware path records: wgpu, not degraded, never Windows GDI.
MACOS_PRESENTER = {"software_render_mode": "auto", "software_rendering": False, "software_render_degraded": False,
                   "windows_gdi": False}


def valid_result(**overrides):
    """A result.json body that satisfies the schema; overrides replace top-level keys. It carries the presenter a
    valid macOS result must record; a test of a run that recorded none removes it with `without_presenter`."""
    phase = {"name": "workload", "start_unix_s": 10.0, "end_unix_s": 70.0, "cpu_user_s": 1.5,
             "cpu_system_s": 0.5, "presented_frames": 120, "redraw_requested": 130,
             "dispatch_ms": [1.0, 2.0], "present_interval_ms": [16.6, 16.7], "allocations_per_frame": None}
    result = {"schema_version": 1, "managed": True, "harness_hash": HARNESS_HASH, "status": "valid",
              "exit_code": 0, "grid": {"columns": 250, "rows": 70}, "phases": [phase],
              "latency": None, "throughput": {"bytes": 1000, "seconds": 2.0}, "uncover_ms": None,
              "scrollback_rows_retained": None,
              "checkpoints": [{"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}],
              "finish_session_settled": True, "notes": [], "presenter": dict(MACOS_PRESENTER)}
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
        # 1 s later: a filesystem stores file times at its own resolution, 100 ns on NTFS and 1 s on HFS+.
        self.write("config.toml", b"x", self.SENTINEL_NS + 1_000_000_000)
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

    def test_the_home_follows_the_app_home_then_userprofile(self):
        # The App reads HOME, then USERPROFILE (sonicterm-cfg dirs_home); Git Bash sets HOME on Windows.
        self.assertEqual(perf.sonicterm_home({"HOME": "/h", "USERPROFILE": "C:/u"}), Path("/h") / ".sonicterm")
        self.assertEqual(perf.sonicterm_home({"USERPROFILE": "C:/u"}), Path("C:/u") / ".sonicterm")
        self.assertEqual(perf.sonicterm_home({}), Path.home() / ".sonicterm")

    def test_the_windows_binary_counts_as_another_instance(self):
        # An installed SonicTerm on Windows runs as sonicterm-windows.exe and may write the shared home.
        table = FakeTable(FakeProcess(904, 0, 0, command="sonicterm-windows.exe"))
        self.assertTrue(perf.other_instance_alive(table, self.HARNESS))


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

    def test_the_gate_s_reviewed_builds_run_the_same_commands(self):
        # The gate owns the steps perf-compare runs; their commands are the builds this script describes.
        for example in perf.HARNESS_EXAMPLES:
            for side in ("head", "base"):
                self.assertEqual(REAL_GATE.PERF_BUILDS[f"build-{side}-{example}"].argv,
                                 perf.build_argv(example, release=True))
        self.assertEqual(REAL_GATE.PERF_BUILDS["build-perf_scenarios"].argv,
                         perf.build_argv(perf.HARNESS_EXAMPLE, release=False))

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
        # The bound is run_step's deadline; a run that reaches it is unresolved cleanup, never a pass or a retry.
        scenario = perf.Scenario("S1", ("default",), "Idle", 120, 30)
        self.assertEqual(perf.run_timeout_s(scenario, smoke=False), 120 + perf.RUN_MARGIN_S)
        self.assertEqual(perf.run_timeout_s(scenario, smoke=True), min(30 + perf.RUN_MARGIN_S, 100))
        self.assertEqual(perf.run_timeout_s(perf.Scenario("S11", ("default",), "Image", 300, 90), smoke=True), 100)

    def test_a_short_comparison_run_gets_the_short_bound_without_the_smoke_cap(self):
        # A --short comparison run uses the scenario's short timeout plus the margin; only the smoke is capped.
        scenario = perf.Scenario("S11", ("default",), "Image", 300, 90)
        self.assertEqual(perf.run_timeout_s(scenario, smoke=False, short=True), 90 + perf.RUN_MARGIN_S)
        self.assertEqual(perf.run_timeout_s(scenario, smoke=False, short=False), 300 + perf.RUN_MARGIN_S)


REAL_GATE = perf.load_gate()


class FakeGate:
    """Stands in for local-gate.py: records each step and answers it from a handler."""

    PASS, FAIL, TIMEOUT = "PASS", "FAIL", "TIMEOUT"

    @property
    def PERF_BUILDS(self):
        """The real gate's reviewed build steps, which perf-compare runs as they are."""
        return REAL_GATE.PERF_BUILDS

    @property
    def PERF_COUNTER_BUILDS(self):
        """The real gate's reviewed builds for a tree that declares perf-counters."""
        return REAL_GATE.PERF_COUNTER_BUILDS

    @property
    def PERF_FEATURE_BUILDS(self):
        """The real gate's reviewed comparison builds, keyed by the perf features a tree declares."""
        return REAL_GATE.PERF_FEATURE_BUILDS

    def __init__(self, handler):
        self.handler = handler
        self.steps = []
        self.roots = []
        self.environs = []

    def __getattr__(self, name):
        """Every other gate fact (the harness cfg table, its catalogs and flag composition) is the real gate's."""
        return getattr(REAL_GATE, name)

    def Step(self, step_id, argv, hosts, timeout_s, evidence, prerequisites, ci_jobs):
        return SimpleNamespace(id=step_id, argv=tuple(argv), hosts=tuple(hosts), timeout_s=timeout_s)

    def run_step(self, step, index, root, log_dir, environ, output_limit_bytes=None):
        self.steps.append(step)
        self.roots.append(Path(root))
        self.environs.append(dict(environ))
        status, exit_code, output = self.handler(step)
        log_path = Path(log_dir) / f"{index:02d}-{step.id}.log"
        log_path.write_text(output, encoding="utf-8")
        return SimpleNamespace(id=step.id, status=status, exit_code=exit_code, log_path=log_path,
                               detail="", leftover_processes=getattr(self, "leftover", 0), elapsed_s=0.0,
                               custody=getattr(self, "custody", None))


HARNESS_PID = 900


def write_atlas_retry_api(tree: Path, skip: str | None = None) -> None:
    """Write the four methods `perf_atlas_retry_api` needs into their owning crates of `tree`, except `skip`."""
    for entry in REAL_GATE.HARNESS_API_CFGS:
        for crate, name in entry.methods:
            if name == skip:
                continue
            source = tree / crate / "atlas_retry_api.rs"
            source.parent.mkdir(parents=True, exist_ok=True)
            with source.open("a", encoding="utf-8") as stream:
                stream.write(f"impl Api {{\n    pub fn {name}(&self) {{}}\n}}\n")


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

    def watcher(self, kill_at_go=False, platform=None, harness_command=None):
        # Only a test of another host names one, so the rest keep RunContext's defaults.
        extra = {key: value for key, value in (("platform", platform), ("harness_command", harness_command))
                 if value is not None}
        context = perf.RunContext(self.scratch, self.evidence, LAUNCH_UNIX_S, self.table, self.gate, (), kill_at_go,
                                  **extra)
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

    def test_a_rejected_record_with_a_valid_anchor_is_cleaned_after_the_run(self):
        # The final scan revalidates a record rejected during the run; its live anchor lets cleanup end the session.
        self.table = FakeTable(anchor(), harness_process(), FakeProcess(510, 500, 500, start="11"))
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", RECORD_TEXT)
        watcher.poll()
        self.assertIn("0", watcher.rejected)
        watcher.final_scan()
        sessions = watcher.sessions_for_cleanup()
        self.assertEqual([session.anchor_pid for session in sessions], [501])
        clock = FakeClock()
        result = perf.cleanup_sessions(self.table, sessions, unanchored=list(watcher.unanchored.values()),
                                       clock=clock, sleep=clock.sleep)
        self.assertTrue(result.settled, result.problems)
        self.assertEqual(self.table.kills, [510, 501])

    def test_members_without_a_valid_anchor_are_listed_and_never_signalled(self):
        # With the anchor gone the session id may name another session, so its members are only listed.
        self.table = FakeTable(harness_process(), FakeProcess(510, 500, 500, start="11"))
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", RECORD_TEXT)
        watcher.poll()
        watcher.final_scan()
        self.assertEqual(watcher.sessions_for_cleanup(), [])
        clock = FakeClock()
        result = perf.cleanup_sessions(self.table, [], unanchored=list(watcher.unanchored.values()),
                                       clock=clock, sleep=clock.sleep)
        self.assertFalse(result.settled)
        self.assertEqual([member.pid for member in result.survivors], [510])
        self.assertEqual(self.table.kills, [])

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
        # A footprint that failed, or was killed at its bound and reaped, still answers; the run stays valid without it.
        for answer in (("TIMEOUT", -9), ("FAIL", 1)):
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

    def test_done_waits_until_the_footprint_process_is_reaped(self):
        # `.done` tells the harness footprint has finished; one whose exit was never collected withholds it.
        self.footprint_answer = ("TIMEOUT", None)
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("checkpoints/1-idle.request")
        watcher.poll()
        self.assertFalse((self.scratch / "checkpoints" / "1-idle.done").exists())
        self.assertFalse(watcher.footprints["1-idle"]["reaped"])
        watcher.poll()
        self.assertFalse((self.scratch / "checkpoints" / "1-idle.done").exists())
        self.assertEqual(len(self.gate.steps), 1)

    def test_footprint_ends_well_inside_the_harness_checkpoint_wait(self):
        # The harness waits CHECKPOINT_WAIT for `.done` and then measures again, so footprint and its reap end first.
        source = (perf.ROOT / "crates/sonicterm-app/examples/perf_scenarios/probe.rs").read_text(encoding="utf-8")
        match = re.search(r"const CHECKPOINT_WAIT: Duration = Duration::from_secs\((\d+)\);", source)
        self.assertIsNotNone(match, "probe.rs no longer declares CHECKPOINT_WAIT in whole seconds")
        self.assertLessEqual(perf.FOOTPRINT_TIMEOUT_S, 40)
        self.assertLess(perf.FOOTPRINT_TIMEOUT_S + perf.RUN_STEP_REAP_BOUND_S + perf.WATCH_INTERVAL_S, int(match[1]))

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

    def test_deadline_kill_rechecks_the_accepted_harness_identity(self):
        # The accepted pid and start token are rechecked just before the signal; a pid that now names another
        # perf_scenarios leader, started after the launch, is never signalled.
        watcher = self.watcher(kill_at_go=True)
        self.write("harness.pid", str(HARNESS_PID))
        watcher.poll()
        self.assertEqual(watcher.harness_pid, HARNESS_PID)
        self.table.processes[HARNESS_PID] = harness_process(start="10", start_unix_s=1002.0)
        self.write("go/0")
        watcher.poll()
        self.assertEqual(self.table.group_kills, [])
        self.assertFalse(watcher.deadline["sent"])
        self.assertIn("identity", watcher.deadline["problem"])

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

    def test_windows_role_program_is_acknowledged_once_validated(self):
        # A role program's record is acknowledged only when its process is the harness's child, running its image.
        self.table = program_table()
        watcher = self.watcher(platform="win32", harness_command="perf_scenarios.exe")
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", PROGRAM_TEXT)
        watcher.poll()
        self.assertEqual((self.scratch / "acks" / "0").read_bytes(), b"")
        self.assertEqual(watcher.programs, {"0": perf.AckedProgram("0", PROGRAM_PID, "21")})
        self.assertEqual(watcher.problems, [])
        # A record naming a process the harness did not start is refused and never acknowledged.
        self.table.processes[PROGRAM_PID + 1] = FakeProcess(PROGRAM_PID + 1, 0, 0, start="22",
                                                             command="perf_scenarios.exe", ppid=4)
        self.write("sessions/1.json", PROGRAM_TEXT.replace('"role": 0', '"role": 1').replace(
            str(PROGRAM_PID), str(PROGRAM_PID + 1)))
        watcher.poll()
        self.assertFalse((self.scratch / "acks" / "1").exists())
        self.assertTrue(any("parent" in problem for problem in watcher.problems), watcher.problems)

    def test_windows_deadline_kill_terminates_only_a_validated_harness(self):
        # On Windows the accepted harness is ended by TerminateProcess after its image and creation time
        # are rechecked; the job ends the rest, and no session or group check applies.
        self.table = program_table()
        watcher = self.watcher(kill_at_go=True, platform="win32", harness_command="perf_scenarios.exe")
        self.write("harness.pid", str(HARNESS_PID))
        watcher.poll()
        self.write("go/0")
        watcher.poll()
        self.assertEqual(self.table.terminations, [(HARNESS_PID, "9")])
        self.assertEqual(self.table.group_kills, [])
        self.assertTrue(watcher.deadline["sent"])
        cases = {"another image": {"command": "cmd.exe"}, "a reused pid": {"start": "10"},
                 "started before the launch": {"start_unix_s": 900.0}}
        for name, overrides in cases.items():
            with self.subTest(name):
                self.table = program_table()
                (self.scratch / "go" / "0").unlink(missing_ok=True)
                watcher = self.watcher(kill_at_go=True, platform="win32", harness_command="perf_scenarios.exe")
                self.write("harness.pid", str(HARNESS_PID))
                watcher.poll()
                self.table.processes[HARNESS_PID] = harness_process(
                    **{"pgid": 0, "sid": 0, "command": "perf_scenarios.exe", **overrides})
                self.write("go/0")
                watcher.poll()
                self.assertEqual(self.table.terminations, [])
                self.assertFalse(watcher.deadline["sent"])
                self.assertTrue(watcher.deadline["problem"])

    def test_off_macos_a_checkpoint_is_done_without_footprint(self):
        # footprint exists only on macOS, so elsewhere the harness's checkpoint wait ends at once.
        self.table = program_table()
        watcher = self.watcher(platform="win32", harness_command="perf_scenarios.exe")
        self.write("harness.pid", str(HARNESS_PID))
        self.write("checkpoints/1-end.request")
        watcher.poll()
        self.assertTrue((self.scratch / "checkpoints" / "1-end.done").exists())
        self.assertEqual(self.gate.steps, [])
        self.assertIn("no footprint on this host", watcher.footprints["1-end"]["detail"])


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
# The reason the harness writes (waits.rs first_present_missing_reason) when a window that opened on a hidden
# Space presented no frame within 10 s: no redraw and no native occlusion event.
STARTUP_OCCLUSION_REASON = (
    "no frame presented within 10 s of the window opening (0 RedrawRequested; no native occlusion event "
    "arrived), so the run is treated as a suspected occlusion; the likely cause is a full-screen app on its "
    "display, which keeps the window on a hidden Space")
# What every run_step deadline or interruption reason says: its count of the harness's group proves nothing.
UNCOUNTED_STOP = ("so either the harness's process group was not counted or a process outside it held the output "
                  "open; either way the run's cleanup is unresolved")


# The App's adapter line from a Windows run on the hosted runner's software adapter.
WINDOWS_ADAPTER_LINE = ("2026-10-02T11:24:16.123456Z  INFO sonicterm_gpu::recovery_context: wgpu adapter selected "
                        "backend=Dx12 name=Microsoft Basic Render Driver driver=10.0.26100.9278 device_type=Cpu "
                        "software_rendering=true device_memory_policy=MemoryUsage")


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

    def test_a_windows_run_must_report_its_adapter(self):
        # A Windows run proves its wgpu path only with the App's `wgpu adapter selected` or `reused` line, so a
        # run without it is not valid, even with a presenter; macOS logs no adapter and stays valid.
        wgpu = perf.RunPlan(IDLE_SCENARIO, "wgpu", "head", Path("/b"), HARNESS_HASH)
        self.assertNotEqual(self.kind(plan=wgpu, platform="win32", renderer=None), "valid")
        kind, reasons = perf.classify_outcome(make_outcome(
            platform="win32", renderer=None, result=valid_result(presenter=WGPU_PRESENTER)))
        self.assertEqual(kind, "adapter")
        self.assertTrue(any("wgpu adapter selected" in reason for reason in reasons), reasons)
        self.assertEqual(self.kind(platform="darwin", renderer=None), "valid")

    def test_schema_failures_stop_a_timed_out_or_blocked_run(self):
        # A harness timeout (exit 4) or block (exit 5) whose result is unmanaged, or another harness's, is a
        # schema failure, which stops a comparison; without one each keeps its kind. A run_step deadline is
        # unresolved cleanup either way, decided before the schema check, since run_step never counted the group.
        problems = ["managed is False, not true"]
        # Each case: how the run ended, the result's status, and the kind with and without the schema problem.
        cases = {
            "harness timeout": (dict(status="FAIL", exit_code=perf.HARNESS_TIMEOUT), "timeout", "schema", "timeout"),
            "harness block": (dict(status="FAIL", exit_code=perf.HARNESS_BLOCKED), "blocked", "schema", "blocked"),
            "run_step deadline": (dict(status="TIMEOUT", exit_code=-9), "timeout", "cleanup", "cleanup"),
        }
        for name, (launch, status, with_problem, without_problem) in cases.items():
            with self.subTest(name):
                result = valid_result(status=status, exit_code=launch["exit_code"], finish_session_settled=True)
                kind, reasons = perf.classify_outcome(make_outcome(result=result, schema_problems=problems, **launch))
                self.assertEqual(kind, with_problem)
                if with_problem == "schema":
                    self.assertEqual(reasons, problems)
                self.assertEqual(perf.compare_verdict(kind), "stop")
                self.assertEqual(perf.classify_outcome(make_outcome(result=result, **launch))[0], without_problem)

    def test_a_launcher_failure_with_exit_0_is_never_valid(self):
        # run_step can report FAIL with exit 0 (a lost leader, a failed final log write), so only PASS can be valid.
        kind, reasons = perf.classify_outcome(make_outcome(status="FAIL"))
        self.assertNotEqual(kind, "valid")
        self.assertTrue(reasons)
        self.assertEqual(perf.smoke_verdict(kind), "fail")

    def test_process_group_survivors_fail_the_smoke_and_stop_a_comparison(self):
        # Members that outlived the harness, or could not be counted, are a safety failure like unresolved cleanup.
        for leftover in (2, None):
            with self.subTest(leftover=leftover):
                outcome = make_outcome(status="FAIL", leftover_processes=leftover,
                                       step_detail="2 leftover process(es) outlived the leader by 2s")
                kind, reasons = perf.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any("process group" in reason for reason in reasons))
                self.assertEqual(perf.smoke_verdict(kind), "fail")
                self.assertEqual(perf.compare_verdict(kind), "stop")

    def test_an_unsettled_teardown_fails_before_any_occlusion_retry(self):
        # finish_session did not settle, so the run fails at once instead of being retried, in the smoke and a comparison.
        unsettled = valid_result(status="invalid", exit_code=3, finish_session_settled=False,
                                 invalid_reason="unrequested native occlusion change")
        deadline_plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("/b"), HARNESS_HASH, kill_at_go=True)
        cases = {"occlusion": make_outcome(exit_code=3, result=unsettled),
                 "occlusion before GO": make_outcome(plan=deadline_plan, exit_code=3, result=unsettled),
                 "other invalidation": make_outcome(exit_code=3, result=dict(unsettled, invalid_reason="unexpected input")),
                 "harness timeout": make_outcome(exit_code=4, result=dict(unsettled, status="timeout", exit_code=4)),
                 "startup occlusion": make_outcome(exit_code=3, result=dict(
                     unsettled, invalid_reason=STARTUP_OCCLUSION_REASON))}
        for name, outcome in cases.items():
            with self.subTest(name):
                kind, reasons = perf.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any("finish_session" in reason for reason in reasons))
                self.assertEqual((perf.smoke_verdict(kind), perf.compare_verdict(kind)), ("fail", "stop"))

    def test_startup_occlusion_is_an_occlusion_the_smoke_and_a_comparison_retry(self):
        # No frame within 10 s of the window opening ends Startup as invalid (exit 3) with a suspected occlusion,
        # the harness's own reason; a full-screen app hiding the window is environmental, so the run is retried.
        startup = valid_result(status="invalid", exit_code=3, finish_session_settled=True,
                               invalid_reason=STARTUP_OCCLUSION_REASON)
        kind, _reasons = perf.classify_outcome(make_outcome(exit_code=3, result=startup))
        self.assertEqual(kind, "occluded")
        self.assertEqual((perf.smoke_verdict(kind), perf.compare_verdict(kind)), ("retry", "invalid"))

    def test_the_deadline_case_passes_only_on_its_own_collected_sigkill(self):
        # Signal acceptance proves no termination: only run_step's own reap of the harness, exit -SIGKILL, passes.
        plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("/b"), HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": HARNESS_PID}
        self.assertEqual(self.kind(plan=plan, status="FAIL", exit_code=-9, result=None, deadline=killed), "valid")
        for status, exit_code in (("TIMEOUT", -9), ("TIMEOUT", None), ("FAIL", None), ("FAIL", 0), ("FAIL", -15)):
            with self.subTest(status=status, exit_code=exit_code):
                kind = self.kind(plan=plan, status=status, exit_code=exit_code, result=None, deadline=killed)
                self.assertNotEqual(kind, "valid")
                self.assertEqual(perf.smoke_verdict(kind), "fail")

    def test_an_uncollected_harness_exit_is_unresolved_cleanup(self):
        # Without a collected exit run_step never counted the harness's group, so its leftover count is no measurement.
        deadline_plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("/b"), HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": HARNESS_PID}
        cases = {"run_step deadline": make_outcome(status="TIMEOUT", exit_code=None, result=None),
                 "leader taken by another reaper": make_outcome(status="FAIL", exit_code=None, result=None),
                 "interrupted": make_outcome(status="INTERRUPTED", exit_code=None, result=None),
                 "deadline kill never reaped": make_outcome(plan=deadline_plan, status="TIMEOUT", exit_code=None,
                                                            result=None, deadline=killed)}
        for name, outcome in cases.items():
            with self.subTest(name):
                kind, reasons = perf.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any("collected" in reason for reason in reasons))
                self.assertEqual((perf.smoke_verdict(kind), perf.compare_verdict(kind)), ("fail", "stop"))
        # A launch failure started no process, so there is nothing to collect.
        self.assertNotEqual(self.kind(status="LAUNCH", exit_code=None, result=None), "cleanup")

    def test_a_run_step_deadline_is_unresolved_cleanup_even_when_collected(self):
        # At the deadline either the harness hung, so its group was killed uncounted, or it exited with its group
        # counted empty while a process outside the group held the output open. Neither a collected exit nor settled
        # anchor cleanup measures what outlived the harness: the smoke fails, a comparison stops, and the reason
        # carries run_step's detail, which names a pipe holder.
        settled_anchors = perf.CleanupResult()
        self.assertTrue(settled_anchors.passed)
        deadline = f"deadline of {perf.run_timeout_s(IDLE_SCENARIO, smoke=False)}s reached"
        hang = f"{deadline}; process tree killed"
        pipe = (f"{deadline}; the group kill was refused with EPERM, so only the leader was killed or reaped; "
                "a descendant outside the process group still holds the output pipe")
        cases = {"no result": (-9, None, hang),
                 "a settled result": (-9, valid_result(status="timeout", exit_code=4, finish_session_settled=True), hang),
                 "an unsettled result": (-9, valid_result(status="timeout", exit_code=4, finish_session_settled=False),
                                         hang),
                 "a pipe held outside the counted group": (0, None, pipe)}
        for name, (exit_code, result, detail) in cases.items():
            with self.subTest(name):
                outcome = make_outcome(status="TIMEOUT", exit_code=exit_code, result=result, cleanup=settled_anchors,
                                       step_detail=detail)
                kind, reasons = perf.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any(detail in reason and UNCOUNTED_STOP in reason for reason in reasons), reasons)
                self.assertEqual((perf.smoke_verdict(kind), perf.compare_verdict(kind)), ("fail", "stop"))

    def test_an_interrupted_run_is_unresolved_cleanup_even_when_collected(self):
        # Ctrl-C in run_step's wait kills and reaps the harness without counting its group, as its deadline does.
        # Whatever exit it collected, the smoke fails and a comparison stops instead of starting its next attempt.
        detail = "interrupted; process tree killed"
        occluded = valid_result(status="invalid", exit_code=3, invalid_reason=STARTUP_OCCLUSION_REASON)
        cases = {"killed (-9)": (-9, None),
                 "already exited 0": (0, valid_result()),
                 "already exited with an occlusion (3)": (3, occluded),
                 "already exited blocked (5)": (5, None)}
        for name, (exit_code, result) in cases.items():
            with self.subTest(name):
                outcome = make_outcome(status="INTERRUPTED", exit_code=exit_code, result=result, step_detail=detail)
                kind, reasons = perf.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any(detail in reason and UNCOUNTED_STOP in reason for reason in reasons), reasons)
                self.assertEqual((perf.smoke_verdict(kind), perf.compare_verdict(kind)), ("fail", "stop"))

    def test_the_harness_reason_leads_an_occlusion_or_invalidation(self):
        # The harness names the cause in invalid_reason; its notes are fixed text about the measurement, so the
        # reason comes first and the notes follow, before GO in the deadline case too.
        notes = ["A measurement ends when the dispatch returns, not at scanout."]
        checkpoint = ("checkpoint 0-end got no .done within 60 s, so its footprint is missing and the run stopped "
                      "before the next phase")
        deadline_plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("/b"), HARNESS_HASH, kill_at_go=True)
        cases = {"startup occlusion": (None, STARTUP_OCCLUSION_REASON, "occluded"),
                 "occlusion before GO": (deadline_plan, STARTUP_OCCLUSION_REASON, "occluded"),
                 "checkpoint invalidation": (None, checkpoint, "invalid")}
        for name, (plan, reason, expected) in cases.items():
            with self.subTest(name):
                result = valid_result(status="invalid", exit_code=3, invalid_reason=reason, notes=notes)
                overrides = {"plan": plan} if plan else {}
                outcome = make_outcome(status="FAIL", exit_code=3, result=result, **overrides)
                self.assertEqual(perf.classify_outcome(outcome), (expected, [reason] + notes))

    def test_the_harness_deadline_with_a_counted_group_stays_a_retryable_timeout(self):
        # Exit 4 is the harness's own deadline: run_step saw the harness exit and counted its group empty,
        # so a comparison retries the run; the smoke fails it, as it fails every timeout.
        result = valid_result(status="timeout", exit_code=perf.HARNESS_TIMEOUT, finish_session_settled=True)
        outcome = make_outcome(status="FAIL", exit_code=perf.HARNESS_TIMEOUT, result=result, leftover_processes=0)
        kind, reasons = perf.classify_outcome(outcome)
        self.assertEqual(kind, "timeout")
        self.assertIn("the harness timed out (exit 4)", reasons)
        self.assertEqual((perf.smoke_verdict(kind), perf.compare_verdict(kind)), ("fail", "invalid"))

    def test_a_reported_teardown_failure_is_fatal_on_every_exit_path(self):
        # An unsettled finish_session may have left sessions behind, so no retryable reason or exit code hides it.
        # A run_step deadline is cleanup before any result is read, so its own test covers that path.
        unsettled = valid_result(status="invalid", exit_code=3, finish_session_settled=False)
        theft = perf.FocusVerdict(True, (), ["focus theft"], True)
        cases = {
            "exit 3 with a session problem": make_outcome(exit_code=3, result=unsettled,
                                                          watcher_problems=["session 1 failed validation"]),
            "exit 3 with a home write": make_outcome(exit_code=3, result=unsettled, home=["config.toml: added"]),
            "exit 3 with focus theft": make_outcome(exit_code=3, result=unsettled, focus=theft),
            "exit 3 with a font error": make_outcome(exit_code=3, result=unsettled,
                                                     font_errors=["Unable to load the configured primary font"]),
            "exit 4": make_outcome(status="FAIL", exit_code=4, result=dict(unsettled, status="timeout", exit_code=4)),
            "exit 5": make_outcome(status="FAIL", exit_code=5, result=dict(unsettled, status="blocked", exit_code=5)),
        }
        for name, outcome in cases.items():
            with self.subTest(name):
                kind, reasons = perf.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any("finish_session" in reason for reason in reasons))
                self.assertEqual((perf.smoke_verdict(kind), perf.compare_verdict(kind)), ("fail", "stop"))

    def test_the_deliberate_kill_with_proven_cleanup_is_exempt(self):
        # The deadline case ends the harness before its teardown; run_step's reap and the anchors prove its cleanup.
        plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("/b"), HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": HARNESS_PID}
        partial = valid_result(status="invalid", exit_code=3, finish_session_settled=False)
        self.assertEqual(self.kind(plan=plan, status="FAIL", exit_code=-9, result=partial, deadline=killed), "valid")

    def test_fatal_outcomes_are_decided_before_any_retryable_reason(self):
        # A retry would rerun on a host that may hold this run's processes, or trust an untrusted result.
        theft = perf.FocusVerdict(True, (), ["focus theft"], True)
        unmanaged = valid_result(status="timeout", exit_code=4, managed=False)
        cases = {
            "unmanaged result and a home write": (
                "schema", make_outcome(schema_problems=["managed is False, not true"], home=["config.toml: added"])),
            "exit 0 without result.json and a home write": (
                "schema", make_outcome(result=None, home=["config.toml: added"])),
            "refusal and focus theft": ("refused", make_outcome(status="FAIL", exit_code=2, result=None, focus=theft)),
            "unmanaged exit-4 result and focus theft": (
                "schema", make_outcome(status="FAIL", exit_code=4, result=unmanaged,
                                       schema_problems=["managed is False, not true"], focus=theft)),
        }
        for name, (expected, outcome) in cases.items():
            with self.subTest(name):
                kind, _reasons = perf.classify_outcome(outcome)
                self.assertEqual(kind, expected)
                self.assertEqual(perf.compare_verdict(kind), "stop")

    def test_harness_exit_codes(self):
        # 2 refuses, 3 invalidates, 4 is the harness's timeout, 5 is blocked, others are unexpected; run_step's
        # own deadline is no harness exit code but unresolved cleanup.
        self.assertEqual(self.kind(exit_code=2, result=None), "refused")
        self.assertEqual(self.kind(exit_code=3, result=valid_result(status="invalid", exit_code=3,
                                                                    notes=["native occlusion change"])), "occluded")
        self.assertEqual(self.kind(exit_code=3, result=valid_result(status="invalid", exit_code=3,
                                                                    notes=["unexpected keyboard input"])), "invalid")
        self.assertEqual(self.kind(exit_code=4, result=None), "timeout")
        self.assertEqual(self.kind(status="TIMEOUT", exit_code=-9, result=None), "cleanup")
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

    def test_windows_deadline_case_is_valid_only_with_verified_custody(self):
        # TerminateProcess ends the harness with 124; the job's members alive at that moment are expected
        # only when the job's custody proves they were all ended.
        plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("/b"), HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": HARNESS_PID}
        verified = custody(active=3, cleanup="terminated")

        def windows(custody_record, exit_code=124):
            return make_outcome(
                plan=plan, status="FAIL", exit_code=exit_code, result=None, deadline=killed,
                cleanup=perf.custody_cleanup(custody_record), custody=custody_record,
                deadline_exit_code=perf.deadline_exit_code("win32"),
                leftover_processes=perf.windows_leftover_processes(custody_record, deadline_case=True))
        self.assertEqual(perf.classify_outcome(windows(verified)), ("valid", []))
        self.assertNotEqual(perf.classify_outcome(windows(verified, exit_code=-9))[0], "valid")
        self.assertEqual(perf.classify_outcome(windows(custody(active=3, cleanup="terminated", empty=False)))[0],
                         "cleanup")
        self.assertEqual((perf.deadline_exit_code("win32"), perf.deadline_exit_code("darwin")), (124, -9))

    def test_windows_members_outliving_a_run_without_a_deadline_are_cleanup(self):
        # A job run_step had to end held members past the harness's exit, so the run is cleanup, never launcher.
        record = custody(active=2, cleanup="terminated")
        outcome = make_outcome(status="FAIL", exit_code=0, cleanup=perf.custody_cleanup(record), custody=record,
                               leftover_processes=perf.windows_leftover_processes(record, deadline_case=False))
        kind, reasons = perf.classify_outcome(outcome)
        self.assertEqual(kind, "cleanup")
        self.assertTrue(any("2 member(s)" in reason and "job" in reason for reason in reasons), reasons)


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
        self.skip_ack = False
        self.font_error = False
        self.progress = False
        self.write_result = True
        # The result.json the harness writes when it passes; None writes valid_result().
        self.result_body = None
        self.deadline_answer = ("FAIL", -9, "killed\n")
        # When set, the harness never exits by itself, and run_step's deadline reports this.
        self.hang_answer = None
        self.front_pids = []
        self.source_root = Path(self.temporary.name) / "tree"
        self.source_root.mkdir()

    def tearDown(self):
        self.temporary.cleanup()

    def front_run(self, argv, timeout_s):
        """Answer lsappinfo: no front application, or each sample's front pid in turn; None fails the lookup."""
        if not self.front_pids:
            self.assertEqual(tuple(argv), perf.FRONT_ARGV)
            return command(argv, "[ NULL ]\n")
        if tuple(argv) == perf.FRONT_ARGV:
            return command(argv, "ASN:0x0-0x1:\n")
        pid = self.front_pids.pop(0) if len(self.front_pids) > 1 else self.front_pids[0]
        if pid is None:
            return command(argv, "", exit_code=1, stderr="lookup failed\n")
        return command(argv, f'"pid"={pid}\n')

    def fake_harness(self, step, deadline):
        """Act as the harness: register a session, wait for its acknowledgement, then pass or be killed."""
        scratch = Path(step.argv[-1])
        self.assertFalse(scratch.exists())
        (scratch / "logs").mkdir(parents=True)
        log = memory_line() + "\n"
        if getattr(self, "windows_run", False):
            # A Windows run's App logs the adapter it selected, which the comparison requires there.
            log += WINDOWS_ADAPTER_LINE + "\n"
        (scratch / "logs" / "sonicterm.log.2026-10-02").write_text(log, encoding="utf-8")
        (scratch / "harness.pid").write_text(str(HARNESS_PID), encoding="utf-8")
        (scratch / "sessions").mkdir()
        (scratch / "sessions" / "0.json").write_text(getattr(self, "session_text", RECORD_TEXT), encoding="utf-8")
        if not self.skip_ack:
            wait_for(lambda: (scratch / "acks" / "0").exists())
        if self.home_write:
            self.home.mkdir(parents=True)
            (self.home / "config.toml").write_text("x", encoding="utf-8")
        (scratch / "go").mkdir()
        (scratch / "go" / "0").write_bytes(b"")
        if self.progress:
            # Shaped like a valid result, so a test can show it is never read as one.
            (scratch / "progress.json").write_text(json.dumps(valid_result()), encoding="utf-8")
        if self.hang_answer is not None:
            return self.hang_answer
        if deadline:
            wait_for(lambda: self.table.group_kills)
            return self.deadline_answer
        if self.write_result:
            # A Windows run also records how it presented, which a valid Windows result must carry.
            default = valid_result(presenter=WGPU_PRESENTER) if getattr(self, "windows_run", False) else valid_result()
            body = self.result_body if self.result_body is not None else default
            (scratch / "result.json").write_text(json.dumps(body), encoding="utf-8")
        output = "harness finished\n"
        if self.font_error:
            output = ('E config: Unable to load the configured primary font "Rec Mono St.Helens" (weight=Regular, '
                      'stretch=Normal, style=Normal). Fallback fonts are being used instead\n') + output
        return "PASS", 0, output

    def run_plan(self, deadline=False, smoke=True, environ=None, name=None, short=None, counters=False, side="head"):
        gate = FakeGate(lambda step: self.fake_harness(step, deadline))
        host = perf.Host(gate, self.table, self.front_run, self.home, self.temp_root, set(),
                         environ or {"HOME": "/h"}, clock=lambda: LAUNCH_UNIX_S)
        # Only a counters run names the field, so every other plan is built exactly as before.
        plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke" if smoke else side, Path("/b/perf_scenarios"),
                            HARNESS_HASH, short=smoke if short is None else short, smoke=smoke,
                            kill_at_go=deadline, source_root=self.source_root,
                            **({"counters": True} if counters else {}))
        evidence = Path(self.temporary.name) / "evidence" / (name or ("deadline" if deadline else "run"))
        with contextlib.redirect_stdout(io.StringIO()):
            outcome = perf.execute_run(plan, host, evidence)
        return outcome, evidence, gate

    def test_a_counters_run_passes_counters_and_reads_a_result_with_the_gate_on(self):
        # The harness gets --counters, and a result that reports the gate forced on passes the schema.
        self.result_body = counters_result()
        outcome, _evidence, gate = self.run_plan(smoke=False, counters=True)
        self.assertIn("--counters", gate.steps[0].argv)
        self.assertEqual(outcome.schema_problems, [])

    def test_a_base_counters_run_may_lack_a_field_its_tree_does_not_report(self):
        # The base's App is older, so a contract field it never had is not a schema failure on that side.
        result = counters_result()
        del result["phases"][0]["frame_counters"]["app"]["native_request_redraw_unregistered"]
        self.result_body = result
        outcome, _evidence, _gate = self.run_plan(smoke=False, counters=True, side="base")
        self.assertEqual(outcome.schema_problems, [])

    def test_a_head_counters_run_must_report_every_field(self):
        # The head's counters are the contract under test, so a field it leaves out is a schema failure.
        result = counters_result()
        del result["phases"][0]["frame_counters"]["app"]["native_request_redraw_unregistered"]
        self.result_body = result
        outcome, _evidence, _gate = self.run_plan(smoke=False, counters=True, side="head")
        self.assertTrue(any("native_request_redraw_unregistered" in problem for problem in outcome.schema_problems))

    def test_a_counters_run_whose_harness_ignored_the_flag_is_a_schema_failure(self):
        # A harness that ignored --counters measured nothing, so the run stops the comparison instead of a row of 0.
        outcome, _evidence, _gate = self.run_plan(smoke=False, counters=True)
        self.assertEqual(perf.classify_outcome(outcome)[0], "schema")
        self.assertTrue(any("--counters" in problem for problem in outcome.schema_problems), outcome.schema_problems)

    def test_a_short_comparison_run_passes_short_to_the_harness_and_its_bound(self):
        # A comparison run planned short gets `--short` and the short deadline, not the full scenario's.
        _outcome, _evidence, gate = self.run_plan(smoke=False, short=True)
        self.assertIn("--short", gate.steps[0].argv)
        self.assertEqual(gate.steps[0].timeout_s, perf.run_timeout_s(IDLE_SCENARIO, smoke=False, short=True))

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

    def test_a_session_without_a_valid_anchor_leaves_the_run_unresolved(self):
        # The run's cleanup lists the anchorless session's members in its evidence and signals none of them.
        self.table = FakeTable(harness_process(), FakeProcess(510, 500, 500, start="11"))
        self.skip_ack = True
        outcome, evidence, _gate = self.run_plan()
        self.assertEqual(perf.classify_outcome(outcome)[0], "cleanup")
        self.assertEqual(self.table.kills, [])
        survivors = json.loads((evidence / "cleanup.json").read_text(encoding="utf-8"))["survivors"]
        self.assertEqual([member["pid"] for member in survivors], [510])

    def test_the_harness_runs_in_its_source_tree_and_logs_into_the_evidence(self):
        # asset_dir() finds development assets from the working directory, so a run's cwd is the tree that built it.
        _outcome, evidence, gate = self.run_plan()
        self.assertEqual(gate.roots, [self.source_root])
        self.assertTrue((evidence / "01-harness.log").is_file())

    def test_a_primary_font_that_failed_to_load_invalidates_the_run(self):
        # The App fell back to another font, so the run measured another renderer; the smoke fails at once.
        self.font_error = True
        outcome, _evidence, _gate = self.run_plan()
        kind, reasons = perf.classify_outcome(outcome)
        self.assertEqual(kind, "font")
        self.assertIn("Rec Mono St.Helens", reasons[0])
        self.assertEqual(perf.smoke_verdict(kind), "fail")

    def test_an_unresolved_home_check_invalidates_the_run(self):
        # A home check that could not finish cannot clear the run of a write, so the run is invalid.
        unresolved = perf.HomeSnapshotUnresolved("more than 200000 entries under the home")
        with mock.patch.object(perf, "snapshot_home", side_effect=unresolved):
            outcome, evidence, _gate = self.run_plan()
        kind, reasons = perf.classify_outcome(outcome)
        self.assertEqual(kind, "home")
        self.assertTrue(any("unresolved" in reason for reason in reasons))
        self.assertTrue(json.loads((evidence / "home-check.json").read_text(encoding="utf-8"))["unresolved"])

    def test_progress_from_a_killed_run_is_kept_as_evidence(self):
        # A run ended at its deadline keeps its earlier phases as evidence; they are never its result.
        self.progress = True
        outcome, evidence, _gate = self.run_plan(deadline=True)
        self.assertTrue((evidence / "scratch" / "progress.json").is_file())
        self.assertIsNone(outcome.result)
        self.assertEqual(perf.classify_outcome(outcome)[0], "valid")

    def test_progress_never_stands_in_for_a_missing_result(self):
        # Without result.json a run classifies as before, even when progress.json looks like a valid result.
        self.progress, self.write_result = True, False
        outcome, evidence, _gate = self.run_plan()
        self.assertTrue((evidence / "scratch" / "progress.json").is_file())
        self.assertIsNone(outcome.result)
        self.assertEqual(perf.classify_outcome(outcome), ("schema", ["exit 0 without result.json"]))

    def test_a_deadline_kill_run_step_never_collected_fails(self):
        # kill_group was accepted, but run_step's bounded reap never collected the harness: cleanup is unresolved.
        self.deadline_answer = ("TIMEOUT", None, "deadline reached; the leader was never reaped\n")
        outcome, _evidence, _gate = self.run_plan(deadline=True)
        self.assertTrue(outcome.deadline["sent"])
        kind, _reasons = perf.classify_outcome(outcome)
        self.assertEqual(kind, "cleanup")
        self.assertEqual(perf.smoke_verdict(kind), "fail")

    def test_a_run_step_deadline_after_settled_anchor_cleanup_stops_a_comparison(self):
        # The harness hangs until run_step's deadline kills and collects it. The anchor cleanup settles, but the
        # group was never counted, so the run is unresolved cleanup that stops a comparison, not a retried timeout.
        self.hang_answer = ("TIMEOUT", -9, "")
        outcome, evidence, _gate = self.run_plan(smoke=False)
        self.assertTrue(outcome.cleanup.passed)
        self.assertTrue(json.loads((evidence / "cleanup.json").read_text())["settled"])
        kind, reasons = perf.classify_outcome(outcome)
        self.assertEqual(kind, "cleanup")
        self.assertTrue(any(UNCOUNTED_STOP in reason for reason in reasons), reasons)
        self.assertEqual(perf.compare_verdict(kind), "stop")

    def test_focus_exception_covers_only_a_github_hosted_runner(self):
        # The harness becoming front after another app is theft, except on a GitHub-hosted runner, in the smoke and
        # in a comparison, where the evidence records the activation; a failed sample fails even there.
        hosted = {"HOME": "/h", "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
        self_hosted = dict(hosted, RUNNER_ENVIRONMENT="self-hosted")
        cases = {"github-hosted smoke": (hosted, True, [10, HARNESS_PID], "valid"),
                 "github-hosted comparison": (hosted, False, [10, HARNESS_PID], "valid"),
                 "self-hosted smoke": (self_hosted, True, [10, HARNESS_PID], "focus"),
                 "self-hosted comparison": (self_hosted, False, [10, HARNESS_PID], "focus"),
                 "github-hosted smoke with a failed sample": (hosted, True, [None], "focus"),
                 "github-hosted comparison with a failed sample": (hosted, False, [None], "focus")}
        for name, (environ, smoke, front_pids, expected) in cases.items():
            with self.subTest(name):
                self.table = FakeTable(leader(), anchor(), harness_process(), FakeProcess(510, 510, 500, start="11"))
                self.front_pids = list(front_pids)
                outcome, evidence, _gate = self.run_plan(smoke=smoke, environ=environ, name=name.replace(" ", "-"))
                self.assertEqual(perf.classify_outcome(outcome)[0], expected)
                notes = json.loads((evidence / "outcome.json").read_text(encoding="utf-8"))["focus_notes"]
                self.assertEqual(bool(notes), name in ("github-hosted smoke", "github-hosted comparison"))

    def test_home_write_during_a_run_is_reported(self):
        # A write under the SonicTerm home invalidates the run and names the path.
        self.home_write = True
        outcome, evidence, _gate = self.run_plan()
        kind, reasons = perf.classify_outcome(outcome)
        self.assertEqual(kind, "home")
        self.assertTrue(any("config.toml" in reason or "created" in reason for reason in reasons))
        self.assertTrue(json.loads((evidence / "home-check.json").read_text())["violations"])

    def test_windows_run_is_settled_by_job_custody(self):
        # On Windows the gate's job, not anchor cleanup, proves teardown; cleanup.json keeps its counts and members.
        self.windows_run = True
        self.table = program_table()
        self.session_text = PROGRAM_TEXT
        record = custody(active=0, members=[member(HARNESS_PID, "perf_scenarios.exe")])

        def run(name, custody_record):
            gate = FakeGate(lambda step: self.fake_harness(step, False))
            gate.custody = custody_record
            host = perf.Host(gate, self.table, self.front_run, self.home, self.temp_root, set(), {"HOME": "/h"},
                             clock=lambda: LAUNCH_UNIX_S, platform="win32")
            plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("C:/b/perf_scenarios.exe"), HARNESS_HASH,
                                short=True, smoke=True, source_root=self.source_root)
            evidence = Path(self.temporary.name) / "evidence" / name
            with contextlib.redirect_stdout(io.StringIO()):
                return perf.execute_run(plan, host, evidence), evidence, gate
        outcome, evidence, gate = run("windows", record)
        self.assertEqual(perf.classify_outcome(outcome), ("valid", []))
        self.assertEqual((self.table.kills, self.table.group_kills), ([], []))
        self.assertEqual(gate.steps[0].hosts, ("windows",))
        recorded = json.loads((evidence / "cleanup.json").read_text())
        self.assertEqual(recorded["cleanup"], "none")
        self.assertEqual(recorded["before_cleanup"]["active_processes"], 0)
        self.assertEqual(recorded["before_cleanup"]["members"]["processes"][0]["pid"], HARNESS_PID)
        self.assertEqual([program["role"] for program in recorded["programs"]], ["0"])
        # Without a custody record the teardown is unproven, so the run fails as cleanup.
        outcome, _evidence, _gate = run("windows-no-custody", None)
        self.assertEqual(perf.classify_outcome(outcome)[0], "cleanup")

    def test_windows_foreground_changes_are_listed_and_invalidate_a_desk_run(self):
        # On Windows the foreground window replaces lsappinfo; a change at a desk invalidates the run and is listed.
        self.windows_run = True
        self.table = program_table()
        self.session_text = PROGRAM_TEXT
        readings = []

        def front_sample():
            reading = foreground(1, 100 if not readings else 200)
            readings.append(reading)
            return reading

        def handler(step):
            # The run lasts until the sampler has seen the change, so the test does not depend on its timing.
            wait_for(lambda: len(readings) >= 2)
            return self.fake_harness(step, False)
        gate = FakeGate(handler)
        gate.custody = custody(active=0)
        host = perf.Host(gate, self.table, self.front_run, self.home, self.temp_root, set(), {"HOME": "/h"},
                         clock=lambda: LAUNCH_UNIX_S, platform="win32", front_sample=front_sample)
        plan = perf.RunPlan(IDLE_SCENARIO, "default", "smoke", Path("C:/b/perf_scenarios.exe"), HARNESS_HASH,
                            short=True, smoke=True, source_root=self.source_root)
        evidence = Path(self.temporary.name) / "evidence" / "foreground"
        with contextlib.redirect_stdout(io.StringIO()):
            outcome = perf.execute_run(plan, host, evidence)
        self.assertEqual(perf.classify_outcome(outcome)[0], "focus")
        recorded = json.loads((evidence / "outcome.json").read_text())
        self.assertEqual([(change["from_pid"], change["to_pid"]) for change in recorded["foreground_changes"]],
                         [(100, 200)])
        samples = (evidence / "front-samples.log").read_text().splitlines()
        self.assertTrue(samples)
        self.assertTrue(all(json.loads(line)["argv"] == ["GetForegroundWindow"] for line in samples))


def timed_outcome(dispatch, uncover=None, latency=None, footprint=None, memory=True):
    """A valid timed run whose workload phase carries the given dispatch samples.

    Its harness samples memory at checkpoints, and `memory` gives its `end` checkpoint one complete
    tagged sample.
    """
    phase = dict(valid_result()["phases"][0], dispatch_ms=dispatch)
    result = valid_result(phases=[phase], uncover_ms=uncover, latency=latency, checkpoint_memory="supported")
    samples = [perf.MemorySample(60.0, 200 * 1048576, 100 * 1048576, 1, checkpoint_index=0,
                                 checkpoint_label="end", checkpoint_attempt=1,
                                 checkpoint_complete=True)] if memory else []
    footprints = {"0-end": {"bytes": footprint}} if footprint is not None else {}
    return make_outcome(result=result, memory=samples, footprints=footprints)


def row_for(rows, metric):
    """Return the table row whose metric column matches, failing when it is missing."""
    matches = [row for row in rows if row[1] == metric]
    if len(matches) != 1:
        raise AssertionError(f"{metric!r} not found once in {[row[1] for row in rows]}")
    return matches[0]


class ComparisonTableTests(unittest.TestCase):
    def test_presented_frames_per_update_divides_by_each_runs_recorded_updates(self):
        # S10's stream phase records the updates its workload played. The row divides each run's presented
        # frames by that run's own count, never a constant: runs of 300 and 1,200 updates give 0.5 and 1.0.
        # A base whose harness predates the field reads n/a with no change; a phase without the field (any
        # non-S10 phase) gets no row; a malformed count is a result problem.
        def stream_outcome(presented, updates):
            phase = dict(valid_result()["phases"][0], name="stream", presented_frames=presented)
            if updates is not None:
                phase["updates"] = updates
            return make_outcome(result=valid_result(phases=[phase]))

        metric = "stream presented frames per update (ratio)"
        head = perf.SideRuns([stream_outcome(150, 300), stream_outcome(1200, 1200)])
        for label in ("S10/default", "S10/sync"):
            with self.subTest(label=label):
                older = perf.SideRuns([stream_outcome(300, None)])
                self.assertEqual(row_for(perf.comparison_rows(label, older, head), metric)[2:],
                                 ["n/a", "0.75 (0.50–1.00)", "n/a"])
                recorded = perf.SideRuns([stream_outcome(600, 300)])
                self.assertEqual(row_for(perf.comparison_rows(label, recorded, head), metric)[2:],
                                 ["2.00 (2.00–2.00)", "0.75 (0.50–1.00)", perf.percent_change(2.0, 0.75)])
        rows = perf.comparison_rows("S1/default", perf.SideRuns([timed_outcome([1.0])]),
                                    perf.SideRuns([timed_outcome([1.0])]))
        self.assertFalse([row for row in rows if "per update" in row[1]], rows)
        for bad in (0, -3, 1.5, "300"):
            with self.subTest(updates=bad):
                phase = dict(valid_result()["phases"][0], name="stream", updates=bad)
                problems = perf.validate_result(valid_result(phases=[phase]), HARNESS_HASH, 0)
                self.assertTrue(any("updates" in problem for problem in problems), problems)

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
        # A run whose checkpoint sample never reached the log has no memory figure for it.
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
        if kind == "valid" and plan.variant in ("gdi", "wgpu"):
            # A presenter variant is valid only with its presenter record and a logged adapter, as on Windows.
            presenter = dict(WGPU_PRESENTER, windows_gdi=plan.variant == "gdi")
            return make_outcome(plan=plan, platform="win32", renderer=HARDWARE_RENDERER,
                                result=valid_result(presenter=presenter))
        if kind == "valid":
            return make_outcome(plan=plan)
        if kind == "occluded":
            return make_outcome(plan=plan, exit_code=3, result=occlusion)
        if kind == "startup":
            return make_outcome(plan=plan, exit_code=3, result=valid_result(
                status="invalid", exit_code=3, invalid_reason=STARTUP_OCCLUSION_REASON))
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
        if kind == "unmanaged theft":
            # An unmanaged exit-4 result that also reports focus theft, as the review reproduced it.
            unmanaged = valid_result(status="timeout", exit_code=4, managed=False)
            return make_outcome(plan=plan, status="FAIL", exit_code=4, result=unmanaged,
                                schema_problems=["managed is False, not true"],
                                focus=perf.FocusVerdict(True, (), ["focus theft"], True))
        if kind == "bound":
            return make_outcome(plan=plan, status="TIMEOUT", exit_code=-9, result=None)
        if kind == "interrupted":
            # Ctrl-C in run_step's wait: it killed and reaped the harness without counting its group.
            return make_outcome(plan=plan, status="INTERRUPTED", exit_code=-9, result=None,
                                step_detail="interrupted; process tree killed")
        if kind == "pane":
            return make_outcome(plan=plan, exit_code=3, result=valid_result(
                status="invalid", exit_code=3, invalid_reason=PANE_EXIT_REASON))
        if kind == "invalid":
            return make_outcome(plan=plan, exit_code=3, result=valid_result(
                status="invalid", exit_code=3, invalid_reason="a phase ran past its bound"))
        if kind == "degraded":
            presenter = dict(WGPU_PRESENTER, software_rendering=True, software_render_degraded=True)
            return make_outcome(plan=plan, platform="win32", renderer=HARDWARE_RENDERER,
                                result=valid_result(presenter=presenter))
        raise AssertionError(kind)
    return build


class SmokeTests(unittest.TestCase):
    SCENARIOS = {"S1": IDLE_SCENARIO, "S3": perf.Scenario("S3", ("default",), "Flood", 120, 30)}

    def run_cases(self, answers, host_platform="darwin", scenarios=None):
        """Drive the smoke's cases on `host_platform`; each case name answers from its queue of outcome kinds."""
        calls = []

        def run_case(plan, evidence):
            name = (plan.scenario.id + ("" if plan.variant == "default" else f"/{plan.variant}")
                    + ("-deadline" if plan.kill_at_go else ""))
            calls.append(name)
            self.assertTrue(plan.short and plan.smoke)
            queue = answers.get(name, ["valid"])
            return outcome_of(queue.pop(0) if len(queue) > 1 else queue[0])(plan)
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed), contextlib.redirect_stderr(io.StringIO()):
            code, reasons = perf.smoke_cases(scenarios or self.SCENARIOS, Path("/b"), HARNESS_HASH, run_case,
                                             Path("/e"), host_platform=host_platform)
        # What the smoke printed, for tests that check the lines a CI log shows.
        self.printed = printed.getvalue()
        return code, reasons, calls

    def test_the_attempt_line_and_blocked_summary_name_a_suspected_occlusion(self):
        # The CI log shows these lines, so each names the harness's reason, not only its fixed notes.
        code, reasons, _calls = self.run_cases({"S1": ["startup"]})
        self.assertEqual(code, 3)
        self.assertIn(f"[perf-smoke] S1 attempt 1: occluded: {STARTUP_OCCLUSION_REASON}", self.printed)
        self.assertTrue(reasons[0].startswith(f"S1: no valid run after {perf.RETRY_LIMIT} retries: "
                                              f"{STARTUP_OCCLUSION_REASON}"), reasons)

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

    def test_startup_occlusion_after_the_retries_is_blocked(self):
        # A window that never reaches the screen is retried within the bound, then the smoke reports BLOCKED.
        code, _reasons, calls = self.run_cases({"S1": ["startup"]})
        self.assertEqual(code, 3)
        self.assertEqual(calls.count("S1"), perf.RETRY_LIMIT + 1)

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

    def test_gate_problem_ignores_leader_watches_on_windows(self):
        # On Windows the gate's job owns every process, so no process-group id needs to stay reserved.
        gate = SimpleNamespace(sigchld_problem=lambda: None, leader_watches=lambda: ())
        self.assertIsNone(perf.gate_problem(gate, os_name="nt"))
        self.assertIsNotNone(perf.gate_problem(gate, os_name="posix"))

    def test_the_smoke_runs_on_windows(self):
        # Windows is a supported host, so the smoke runs there rather than reporting BLOCKED.
        with mock.patch.object(perf.sys, "platform", "win32"), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(perf.smoke_main({}, lambda evidence: (0, [])), 0)

    def test_passing_smoke_prints_members_before_removing_evidence(self):
        # A passing smoke deletes its evidence, so the gate log keeps each attempt's job members instead.
        many = [member(1000 + index, f"worker{index}.exe") for index in range(20)]

        def run_case(plan, evidence):
            if plan.kill_at_go:
                record = custody(active=20, cleanup="terminated", members=many)
                return make_outcome(plan=plan, status="FAIL", exit_code=124, result=None,
                                    deadline={"sent": True, "problem": None, "pid": HARNESS_PID},
                                    custody=record, deadline_exit_code=124,
                                    cleanup=perf.custody_cleanup(record),
                                    leftover_processes=perf.windows_leftover_processes(record, deadline_case=True))
            record = custody(active=0, members=[member(HARNESS_PID, "perf_scenarios.exe"),
                                               member(PROGRAM_PID, "conhost.exe")])
            return make_outcome(plan=plan, custody=record, cleanup=perf.custody_cleanup(record))

        def runner(evidence):
            return perf.smoke_cases(self.SCENARIOS, Path("/b"), HARNESS_HASH, run_case, evidence,
                                    cases=perf.SMOKE_CASES)
        printed = io.StringIO()
        with mock.patch.object(perf.sys, "platform", "win32"), contextlib.redirect_stdout(printed), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(perf.smoke_main({}, runner), 0)
        text = printed.getvalue()
        self.assertIn(f"[perf-smoke] S1 attempt 1 members: {HARNESS_PID} perf_scenarios.exe 130000000000; "
                      f"{PROGRAM_PID} conhost.exe 130000000000", text)
        deadline_line = next(line for line in text.splitlines() if line.startswith("[perf-smoke] S1-deadline attempt 1 members:"))
        self.assertEqual(deadline_line.count(".exe"), 16)
        self.assertTrue(deadline_line.endswith("; and 4 more"), deadline_line)
        self.assertLess(text.index(" members: "), text.index("removed"))
        evidence = Path(re.search(r"evidence=(\S+)", text)[1])
        self.assertFalse(evidence.exists())

    def test_synthetic_steps_name_the_current_host(self):
        # local-gate selects and labels steps by host, so a Windows run's steps say windows.
        self.assertEqual(perf.gate_hosts("win32"), ("windows",))
        self.assertEqual(perf.gate_hosts("darwin"), ("macos",))
        with tempfile.TemporaryDirectory() as temporary:
            gate = FakeGate(lambda step: ("PASS", 0, json.dumps(LIST_JSON) + "\n"))
            with mock.patch.object(perf.sys, "platform", "win32"):
                perf.list_scenarios(gate, Path("C:/b/perf_scenarios.exe"), Path(temporary), 1)
        self.assertEqual(gate.steps[0].hosts, ("windows",))

    WINDOWS_SCENARIOS = {"S1": perf.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 120, 30),
                         "S3": perf.Scenario("S3", ("default",), "Flood", 120, 30)}

    def test_the_case_list_names_each_hosts_cases_and_expected_kinds(self):
        # Windows adds the wgpu presenter and a role program's exit; macOS keeps its three cases.
        def listed(platform_name):
            return [(case.name, case.expected) for case in perf.smoke_case_list(platform_name)]
        darwin = [("S1", "valid"), ("S3", "valid"), ("S1-deadline", "valid")]
        self.assertEqual(listed("darwin"), darwin)
        self.assertEqual(listed("win32"), darwin + [("S1/wgpu", "valid"), ("S1/role-exit", "invalid")])
        # Each case's variant must be one the harness lists, or the smoke fails before any run.
        code, reasons, calls = self.run_cases({}, host_platform="win32")
        self.assertEqual((code, calls), (1, []))
        self.assertTrue(any("S1/wgpu" in reason for reason in reasons), reasons)
        code, reasons, calls = self.run_cases({"S1/role-exit": ["pane"]}, "win32", self.WINDOWS_SCENARIOS)
        self.assertEqual((code, reasons), (0, []))
        self.assertEqual(calls, ["S1", "S3", "S1-deadline", "S1/wgpu", "S1/role-exit"])

    def test_a_blocked_wgpu_run_blocks_the_smoke(self):
        # A degraded wgpu presenter cannot measure the wgpu path, so the smoke exits 3, naming the case.
        code, reasons, _calls = self.run_cases({"S1/wgpu": ["degraded"], "S1/role-exit": ["pane"]}, "win32",
                                               self.WINDOWS_SCENARIOS)
        self.assertEqual(code, 3)
        self.assertTrue(any(reason.startswith("S1/wgpu") for reason in reasons), reasons)

    def test_role_exit_passes_only_as_invalid_naming_the_pane(self):
        # A role program that exits must end the run invalid with a reason that names its pane.
        for answer in ("valid", "invalid"):
            with self.subTest(answer=answer):
                code, reasons, _calls = self.run_cases({"S1/role-exit": [answer]}, "win32", self.WINDOWS_SCENARIOS)
                self.assertEqual(code, 1)
                self.assertTrue(any(reason.startswith("S1/role-exit") for reason in reasons), reasons)


class RunSetTests(unittest.TestCase):
    def run_set(self, answers, runs=2, base_blocked=None, variant="default"):
        """Drive one run set; each side answers from its queue of outcome kinds or factories, repeating the last."""
        calls = []
        plans = {side: perf.RunPlan(IDLE_SCENARIO, variant, side, Path(f"/{side}"), HARNESS_HASH) for side in perf.SIDES}

        def run_case(plan, evidence):
            calls.append(plan.side)
            queue = answers[plan.side]
            kind = queue.pop(0) if len(queue) > 1 else queue[0]
            if callable(kind):
                return kind(plan)
            if kind == "grid":
                return make_outcome(plan=plan, result=valid_result(grid={"cols": 200, "rows": 50}))
            return outcome_of(kind)(plan)
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            result = perf.run_set("S1/default", plans, base_blocked, runs, run_case, Path("/e"))
        # What the comparison printed, for tests that check its run lines.
        self.printed = printed.getvalue()
        return result, calls

    def test_a_run_line_names_a_suspected_occlusion(self):
        # A retried run's line names the harness's reason first, so the log says why the run was retried.
        self.run_set({"base": ["startup", "valid"], "head": ["valid"]}, runs=1)
        self.assertIn(f"[perf-compare] S1/default timed base run 1: occluded: {STARTUP_OCCLUSION_REASON}", self.printed)

    def test_an_interrupted_run_stops_the_comparison(self):
        # Someone who pressed Ctrl-C wants the comparison to stop, and run_step never counted the harness's group.
        with self.assertRaises(perf.StopComparison) as raised:
            self.run_set({"base": ["interrupted", "valid"], "head": ["valid"]})
        self.assertIn(UNCOUNTED_STOP, str(raised.exception))

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

    def test_each_attempt_persists_its_final_classification(self):
        # outcome.json is written before the set's grid check, so a run the set rejects after a valid outcome
        # still reads valid there. The set writes each attempt's final kind and reasons beside it, which perf-flags
        # binds a set's accepted attempts to; an attempt without a directory gets none.
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        plans = {side: perf.RunPlan(IDLE_SCENARIO, "default", side, Path(f"/{side}"), HARNESS_HASH)
                 for side in perf.SIDES}
        answers = {"base": ["valid"], "head": ["grid", "valid"]}

        def run_case(plan, evidence):
            evidence.mkdir(parents=True)
            queue = answers[plan.side]
            kind = queue.pop(0) if len(queue) > 1 else queue[0]
            if kind == "grid":
                return make_outcome(plan=plan, result=valid_result(grid={"cols": 200, "rows": 50}))
            return outcome_of(kind)(plan)
        with contextlib.redirect_stdout(io.StringIO()):
            result = perf.run_set("S1/default", plans, None, 1, run_case, root)
        paths = {Path(evidence).name: Path(evidence) / perf.CLASSIFICATION_FILE
                 for _side, evidence, _kind, _why in result.attempts}
        self.assertTrue(all(path.is_file() for path in paths.values()), "every attempt's classification is written")
        written = {name: json.loads(path.read_text()) for name, path in paths.items()}
        self.assertEqual(written["01-base"], {"side": "base", "kind": "valid", "reasons": []})
        self.assertEqual((written["02-head"]["kind"], written["02-head"]["side"]), ("grid", "head"))
        self.assertEqual(written["02-head"]["reasons"], list(result.attempts[1][3]))
        self.assertEqual(written["03-head"]["kind"], "valid")
        with contextlib.redirect_stdout(io.StringIO()):
            bare = perf.run_set("S1/default", plans, None, 1, lambda plan, _evidence: outcome_of("valid")(plan),
                                root / "absent")
        self.assertFalse(any((Path(evidence) / perf.CLASSIFICATION_FILE).exists()
                             for _side, evidence, _kind, _why in bare.attempts))

    def test_host_platform_names_macos_and_windows_and_keeps_other_hosts(self):
        # perf-flags and run-identity.json key evidence by these names, so the two mapped hosts and the
        # pass-through for any other host are pinned.
        self.assertEqual(perf.host_platform("darwin"), "macos")
        self.assertEqual(perf.host_platform("win32"), "windows")
        self.assertEqual(perf.host_platform("linux"), "linux")

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

    def test_a_schema_failure_behind_focus_theft_stops_the_comparison(self):
        # The first attempt is unmanaged and also reports theft; it stops at once instead of being retried to exit 0.
        with self.assertRaises(perf.StopComparison) as raised:
            self.run_set({"base": ["unmanaged theft", "valid"], "head": ["valid"]})
        self.assertIn("managed", str(raised.exception))

    def test_a_run_step_deadline_stops_the_comparison(self):
        # run_step's deadline killed and collected the harness, but never counted its group: no retry, a stop.
        with self.assertRaises(perf.StopComparison) as raised:
            self.run_set({"base": ["bound", "valid"], "head": ["valid"]})
        self.assertIn(UNCOUNTED_STOP, str(raised.exception))

    def test_head_blocked_is_blocked_and_a_schema_failure_stops(self):
        # A head that cannot run the scenario exits 3; a schema failure stops the comparison at once.
        result, _calls = self.run_set({"base": ["valid"], "head": ["blocked"]})
        self.assertEqual(perf.comparison_exit([result]), 3)
        with self.assertRaises(perf.StopComparison) as raised:
            self.run_set({"base": ["valid"], "head": ["schema"]})
        self.assertIn("managed", str(raised.exception))

    def test_a_renderer_or_presenter_mismatch_invalidates_the_pair(self):
        # Both sides must draw through the same adapter and presenter, or the pair compares two renderers.
        other_adapter = dict(HARDWARE_RENDERER, name="Intel(R) Arc(TM) A770 Graphics")
        cases = {"renderer": (windows_run(), windows_run(renderer=other_adapter)),
                 "presenter": (windows_run(), windows_run(software_render_mode="gpu")),
                 # A degraded macOS run beside one on the hardware path compares two presenters too.
                 "macos presenter": (macos_run(), macos_run(software_render_degraded=True))}
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
            result = valid_result(grid={"cols": 250, "rows": 70}, presenter=WGPU_PRESENTER)
            return make_outcome(plan=plan, platform="win32", renderer=None, result=result)
        result, calls = self.run_set({"base": [windows_run()], "head": [no_adapter, windows_run()]}, runs=1)
        self.assertEqual(calls, ["base", "head", "head"])
        self.assertEqual(result.attempts[1][2], "adapter")
        self.assertEqual(len(result.head.outcomes), 1)

    def test_a_gdi_variant_without_gdi_is_blocked(self):
        # The gdi variant measures the GDI presenter, so a run that did not present through GDI cannot count.
        result, calls = self.run_set({"base": [windows_run()], "head": [windows_run()]}, runs=1, variant="gdi")
        self.assertEqual(calls, ["base", "head"])
        self.assertIn("GDI", result.head.blocked)
        self.assertEqual(perf.comparison_exit([result]), perf.EXIT_BLOCKED)

    def test_a_default_windows_pair_on_hardware_is_valid_and_names_wgpu(self):
        # A hardware adapter that is not degraded measures the wgpu path, and the table says so.
        result, _calls = self.run_set({"base": [windows_run()], "head": [windows_run()]}, runs=1)
        self.assertEqual((len(result.base.outcomes), len(result.head.outcomes)), (1, 1))
        presenter_rows = [row for row in perf.comparison_rows("S1/default", result.base, result.head)
                          if row[1] == "presenter"]
        self.assertEqual(len(presenter_rows), 1)
        self.assertTrue(all("wgpu" in cell for cell in presenter_rows[0][2:4]), presenter_rows)

    def test_a_windows_pair_runs_at_any_shared_grid(self):
        # A Windows window opens at a grid that depends on the display and its scale, so any grid is
        # measured; a pair must share one, so 281x58 beside 250x70 is not a valid pair.
        wide = windows_run(grid={"cols": 281, "rows": 58})
        result, calls = self.run_set({"base": [wide], "head": [wide]}, runs=1)
        self.assertEqual(calls, ["base", "head"])
        self.assertEqual((len(result.base.outcomes), len(result.head.outcomes)), (1, 1))
        configured = windows_run(grid={"cols": 250, "rows": 70})
        result, calls = self.run_set({"base": [wide], "head": [configured, wide]}, runs=1)
        self.assertEqual(calls, ["base", "head", "head"])
        self.assertEqual(result.attempts[1][2], "grid")
        self.assertEqual(len(result.head.outcomes), 1)

    def test_the_table_records_each_sides_grid(self):
        # The grid is no longer fixed, so a row beside the presenter names each side's grids; a side
        # whose runs reported none has no row.
        def grid_run(cols, rows):
            return make_outcome(result=valid_result(grid={"cols": cols, "rows": rows}))
        base = perf.SideRuns([grid_run(281, 58)])
        head = perf.SideRuns([grid_run(281, 58), grid_run(250, 70)])
        rows = [row for row in perf.comparison_rows("S1/default", base, head) if row[1] == "grid"]
        self.assertEqual(rows, [["S1/default", "grid", "281x58", "281x58; 250x70", ""]])
        bare = perf.SideRuns([make_outcome(result=valid_result(grid=None))])
        self.assertFalse(any(row[1] == "grid" for row in perf.comparison_rows("S1/default", bare, bare)))


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

    WINDOWS_VALUES = {"cpu": "Intel(R) Core(TM) i7-1185G7 @ 3.00GHz", "manufacturer": "LENOVO",
                      "product": "20XW0026US", "os_name": "Windows 10 Enterprise", "os_version": "24H2",
                      "os_build": "26100", "os_ubr": 4061}
    POWERCFG = "Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)\n"

    def windows_outputs(self, values, powercfg_exit=0, memory=34359738368, conhost="10.0.26100.4061"):
        """Read the Windows host through a fake registry, memory status, powercfg and file version."""
        registry = {perf.WINDOWS_REGISTRY_VALUES[name]: value for name, value in values.items()}
        argvs = []

        def host_run(argv, timeout_s):
            argvs.append(tuple(argv))
            return command(argv, self.POWERCFG if powercfg_exit == 0 else "", exit_code=powercfg_exit)
        outputs = perf.windows_host_outputs(host_run, registry=lambda key, name: registry.get((key, name)),
                                            memory=lambda: memory, file_version=lambda path: conhost)
        return outputs, argvs

    def test_the_windows_host_block_reads_registry_memory_power_and_conhost(self):
        # The Windows block names machine, OS build, adapter, display, power plan and console host version.
        outputs, argvs = self.windows_outputs(self.WINDOWS_VALUES)
        self.assertEqual(argvs, [perf.POWERCFG_ARGV])
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 60000, "scale_factor": 1.5}
        text = "\n".join(perf.host_block_windows(outputs, monitor, HARDWARE_RENDERER))
        for expected in ("LENOVO 20XW0026US", "Intel(R) Core(TM) i7-1185G7 @ 3.00GHz", "32 GiB",
                         "Windows 10 Enterprise 24H2 (build 26100.4061)", "NVIDIA GeForce RTX 4070",
                         "Built-in Display, 60 Hz, scale 1.5", "Balanced", "conhost.exe 10.0.26100.4061"):
            with self.subTest(expected=expected):
                self.assertIn(expected, text)

    def test_missing_windows_values_read_unavailable(self):
        # A registry value, memory status, power plan or version that could not be read is unavailable, never guessed.
        outputs, _argvs = self.windows_outputs({}, powercfg_exit=1, memory=None, conhost=None)
        text = "\n".join(perf.host_block_windows(outputs, None, None))
        self.assertIn("unavailable", text)
        self.assertNotIn("None", text)

    def test_adapter_lines_split_at_the_known_keys(self):
        # Adapter names and drivers hold spaces, so a line is split at the App's field names, not at spaces.
        measured = ("2026-10-02T11:24:16.123456Z  INFO sonicterm_gpu::recovery_context: wgpu adapter selected "
                    "backend=Dx12 name=Microsoft Basic Render Driver driver=10.0.26100.9278 device_type=Cpu "
                    "software_rendering=true device_memory_policy=MemoryUsage")
        self.assertEqual(perf.parse_adapter_line(measured), {
            "event": "selected", "backend": "Dx12", "name": "Microsoft Basic Render Driver",
            "driver": "10.0.26100.9278", "device_type": "Cpu", "software_rendering": True})
        hardware = ("2026-10-02T11:24:17Z  INFO sonicterm_gpu::core: wgpu adapter reused backend=Dx12 "
                    "name=NVIDIA GeForce RTX 4070 driver=NVIDIA 560.94 device_type=DiscreteGpu "
                    "software_rendering=false")
        self.assertEqual(perf.parse_adapter_line(hardware), {
            "event": "reused", "backend": "Dx12", "name": "NVIDIA GeForce RTX 4070", "driver": "NVIDIA 560.94",
            "device_type": "DiscreteGpu", "software_rendering": False})
        self.assertIsNone(perf.parse_adapter_line(memory_line()))
        self.assertIsNone(perf.parse_adapter_line("wgpu adapter selected backend=Dx12 name=cut off"))


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

    def test_short_is_a_comparison_option_the_smoke_refuses(self):
        # `--short` shortens a comparison's runs; the smoke is always short and takes no comparison option.
        self.assertTrue(self.parse("--base", "main", "--head", "HEAD", "--short").short)
        self.assertFalse(self.parse("--base", "main", "--head", "HEAD").short)
        with self.assertRaises(SystemExit):
            self.parse("--smoke", "--short")

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

    def test_a_legacy_console_encoding_still_prints_the_counters_table(self):
        # Windows runners give Python a cp1252 console, which cannot encode the `≤` of a histogram bucket bound.
        # After use_utf8_output, printing the table succeeds and the bytes are UTF-8.
        script = ("import importlib.util, sys\n"
                  f"spec = importlib.util.spec_from_file_location('perf_compare', {str(SPEC.origin)!r})\n"
                  "module = importlib.util.module_from_spec(spec)\n"
                  "sys.modules[spec.name] = module\n"
                  "spec.loader.exec_module(module)\n"
                  "module.use_utf8_output()\n"
                  "print('p95 ≤5000 us', flush=True)\n")
        environment = {**os.environ, "PYTHONIOENCODING": "cp1252", "PYTHONUTF8": "0"}
        completed = subprocess.run([sys.executable, "-c", script], capture_output=True, env=environment, check=False)
        self.assertEqual(completed.returncode, 0, completed.stderr.decode("utf-8", "replace"))
        self.assertIn("p95 ≤5000 us", completed.stdout.decode("utf-8"))

    def test_main_switches_the_console_to_utf8_before_anything_prints(self):
        # Every path through main prints; the switch must come first, so no message can hit the legacy encoding.
        calls = []
        with mock.patch.object(perf, "use_utf8_output", side_effect=lambda: calls.append("utf8")), \
                mock.patch.object(perf, "smoke_main", side_effect=lambda _environ: calls.append("smoke") or 0):
            self.assertEqual(perf.main(["--smoke"]), 0)
        self.assertEqual(calls, ["utf8", "smoke"])

    def test_comparisons_are_not_blocked_on_windows(self):
        # On Windows a comparison reaches the gate checks instead of reporting BLOCKED.
        gate = SimpleNamespace(sigchld_problem=lambda: "stop here", leader_watches=lambda: ())
        with mock.patch.object(perf.sys, "platform", "win32"), mock.patch.object(perf, "load_gate", return_value=gate), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(perf.main(["--base", "main", "--head", "HEAD"]), perf.EXIT_FAIL)


RUSTC_VV = ("rustc 1.99.0 (abcdef 2026-09-01)\nbinary: rustc\ncommit-hash: abcdef\n"
            "host: aarch64-apple-darwin\nrelease: 1.99.0\nLLVM version: 21.1.0\n")


class CompareHarness:
    """Drives `_compare` with fake git, Cargo and runs; shared by the driver, strict-base and prebuilt tests."""

    SHAS = {"main": "1" * 40, "HEAD": "2" * 40}

    def compare(self, base_build="PASS", assets=("base", "head"), options=(), environ=None,
                head_manifest=HEAD_MANIFEST, listing=None, scenarios=("S1",), base_manifest=BASE_MANIFEST,
                logging_api=("base", "head"), build_status="PASS", list_fail=(), base_run=None,
                head_build="PASS", real_binaries=False, toolchain=None, hook_trees=(), head_run=None,
                trim_trees=(), counters_run=None, api_trees=("base", "head")):
        """Drive the comparison with fake git, Cargo and runs; return the exit code, gate, git calls and paths.

        A counters run answers with the gate on; what the comparison printed is kept in `self.printed`.
        `list_fail` names the sides whose `--list` fails; `base_run`, when given, answers every base run,
        and `head_run` every head run outside the counters set.
        `real_binaries` writes each build's executable under the test's directory, so build-only can copy
        it; `toolchain` replaces the `rustc -vV` text, and a `--build-only` or `--prebuilt` run takes no
        `--scenario` or `--runs`. `trim_trees` names the trees whose app source defines the trim hook method;
        `hook_trees` names the trees whose app source defines the
        checkpoint-memory hook method; `api_trees` the trees that define S1/atlas-retry's App and renderer
        methods (by default both, as two current trees do).
        """
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        work, out = root / "work", root / "out"
        out.mkdir()
        git_calls = []

        def host_run(argv, timeout_s=perf.GIT_TIMEOUT_S):
            git_calls.append(tuple(argv))
            if tuple(argv) == ("rustc", "-vV"):
                return command(argv, toolchain or RUSTC_VV)
            if tuple(argv) == ("cargo", "-V"):
                return command(argv, "cargo 1.99.0 (abcdef 2026-09-01)\n")
            if tuple(argv[:2]) == ("git", "rev-parse"):
                return command(argv, self.SHAS[argv[-1].split("^")[0]] + "\n")
            if tuple(argv[:3]) == ("git", "worktree", "add"):
                tree = Path(argv[-2])
                manifest = head_manifest if tree.name == "head" else base_manifest
                (tree / perf.APP_MANIFEST).parent.mkdir(parents=True)
                (tree / perf.APP_MANIFEST).write_text(manifest, encoding="utf-8")
                # Each tree's logging crate, with the filtered init only for the trees in `logging_api`.
                (tree / LOGGING_LIB).parent.mkdir(parents=True)
                (tree / LOGGING_LIB).write_text(LOGGING_WITH_FILTER if tree.name in logging_api
                                                else LOGGING_WITHOUT_FILTER, encoding="utf-8")
                if tree.name in hook_trees:
                    hook = tree / perf.APP_SOURCE_DIRECTORY / "app" / "memory_snapshot.rs"
                    hook.parent.mkdir(parents=True, exist_ok=True)
                    hook.write_text(HOOK_SOURCE, encoding="utf-8")
                if tree.name in api_trees:
                    write_atlas_retry_api(tree)
                if tree.name in trim_trees:
                    trim = tree / perf.APP_SOURCE_DIRECTORY / "app" / "retention.rs"
                    trim.parent.mkdir(parents=True, exist_ok=True)
                    trim.write_text(TRIM_SOURCE, encoding="utf-8")
                if tree.name in assets:
                    font = tree / "assets" / "fonts" / "RecMonoSt.Helens-Regular.ttf"
                    font.parent.mkdir(parents=True)
                    font.write_bytes(b"font")
                if tree.name == "head":
                    (tree / perf.HARNESS_DIRECTORY).mkdir(parents=True)
                    (tree / perf.HARNESS_DIRECTORY / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
            return command(argv, "")

        def answer(step):
            if step.id.startswith("build-"):
                side, example = step.id.split("-", 2)[1:]
                if (side == "base" and base_build != "PASS") or (side == "head" and head_build != "PASS"):
                    return "FAIL", 101, "error[E0599]: no method named `run_action` found\n"
                executable = f"/{side}/{example}"
                if real_binaries:
                    # A file that exists and is executable, under a directory with no `assets` of its own.
                    built = root / "cargo" / side / perf.executable_name(example)
                    built.parent.mkdir(parents=True, exist_ok=True)
                    built.write_bytes(f"{side} {example} binary".encode("utf-8"))
                    built.chmod(0o755)
                    executable = str(built)
                artifact = {"reason": "compiler-artifact", "target": {"name": example, "kind": ["example"]},
                            "executable": executable}
                return build_status, 0, json.dumps(artifact) + "\n"
            # A listing step's argv[0] is the binary, whose path names its side.
            if any(f"/{side}/" in step.argv[0].replace(os.sep, "/") for side in list_fail):
                return "FAIL", 1, "dyld: Library not loaded: libcairo.2.dylib\n"
            return "PASS", 0, json.dumps(listing or LIST_JSON) + "\n"
        gate = FakeGate(answer)
        selection = () if "--build-only" in options else ("--scenario", *scenarios, "--runs", "1")
        args = perf.parse_args(["--base", "main", "--head", "HEAD", *selection, *options])
        plans = []
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 60000, "scale_factor": 2.0}

        def fake_run(plan, host, evidence):
            plans.append(plan)
            # When: a test supplies counters runs, they answer every counters plan of either side.
            if counters_run is not None and getattr(plan, "counters", False):
                return counters_run(plan)
            if base_run is not None and plan.side == "base":
                return base_run(plan)
            if head_run is not None and plan.side == "head" and not getattr(plan, "counters", False):
                return head_run(plan)
            if getattr(plan, "counters", False):
                return make_outcome(plan=plan, result=counters_result({"window.attempts": 5}, monitor=monitor))
            return display_run(60000)(plan)
        printed = io.StringIO()
        # An installed Linux package's assets would win over the worktree's, so this host has none.
        with mock.patch.object(perf, "LINUX_SHARED_ASSETS", root / "no-installed-assets"), \
                mock.patch.object(perf, "production_host", return_value=None), \
                mock.patch.object(perf, "execute_run", side_effect=fake_run), \
                mock.patch.dict(os.environ, environ or {}), \
                contextlib.redirect_stdout(printed):
            code = perf._compare(args, gate, out, work, perf.Worktrees(host_run, work), host_run)
        self.printed = printed.getvalue()
        return code, gate, git_calls, plans, work, out


class CompareDriverTests(CompareHarness, unittest.TestCase):
    def test_the_counters_set_runs_on_the_head_only_after_the_timed_set(self):
        # Only the head declares perf-counters: it builds with it and runs a head-only counters set after the
        # timed set, and the base and change cells read n/a.
        code, gate, _calls, plans, _work, out = self.compare(options=("--counters", "--counters-runs", "2"),
                                                             head_manifest=COUNTERS_MANIFEST)
        self.assertEqual(code, 0)
        self.assertEqual([(plan.side, plan.counters) for plan in plans],
                         [("base", False), ("head", False), ("head", True), ("head", True)])
        builds = {step.id: step.argv for step in gate.steps if step.id.startswith("build-")}
        self.assertIn("perf-counters", builds["build-head-perf_scenarios"])
        self.assertNotIn("--features", builds["build-base-perf_scenarios"])
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S1/default |  | status | n/a | 2 valid runs |  |", document)
        self.assertIn("| S1/default | workload | window.attempts (count) | n/a | 5 (5–5) | n/a |", document)
        self.assertIn("- Built with `--features perf-counters`: head", document)
        self.assertIn("--counters --counters-runs 2", document)

    def test_the_counters_set_runs_on_both_sides_when_the_base_declares_the_feature(self):
        # Both refs build with the feature, the counters set runs base and head runs after the timed set, and the
        # table compares them like the timed table.
        code, gate, _calls, plans, _work, out = self.compare(options=("--counters", "--counters-runs", "2"),
                                                             head_manifest=COUNTERS_MANIFEST,
                                                             base_manifest=BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, 0)
        self.assertEqual([(plan.side, plan.counters) for plan in plans[:2]], [("base", False), ("head", False)])
        self.assertEqual(sorted(plan.side for plan in plans[2:] if plan.counters), ["base", "base", "head", "head"])
        self.assertEqual(len(plans), 6)
        builds = {step.id: step.argv for step in gate.steps if step.id.startswith("build-")}
        self.assertIn("perf-counters", builds["build-base-perf_scenarios"])
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S1/default |  | status | 2 valid runs | 2 valid runs |  |", document)
        self.assertIn("| S1/default | workload | window.attempts (count) | 5 (5–5) | 5 (5–5) | +0.0% |", document)
        self.assertIn("- Built with `--features perf-counters`: base, head", document)

    def test_the_counters_run_count_defaults_to_runs(self):
        # Without --counters-runs the counters set takes --runs.
        _code, _gate, _calls, plans, _work, _out = self.compare(options=("--counters",),
                                                                head_manifest=COUNTERS_MANIFEST)
        self.assertEqual(sum(1 for plan in plans if plan.counters), 1)

    def test_a_base_with_the_feature_but_no_filtered_logging_init_runs_head_only(self):
        # The 4d19e855 case: the base declares perf-counters but lacks the logging API the head's harness calls,
        # so it builds without the feature and the counters set runs on the head only, base n/a.
        code, gate, _calls, plans, _work, out = self.compare(options=("--counters", "--counters-runs", "2"),
                                                             head_manifest=COUNTERS_MANIFEST,
                                                             base_manifest=BASE_COUNTERS_MANIFEST,
                                                             logging_api=("head",))
        self.assertEqual(code, 0)
        builds = {step.id: step.argv for step in gate.steps if step.id.startswith("build-")}
        self.assertNotIn("perf-counters", builds["build-base-perf_scenarios"])
        self.assertIn("perf-counters", builds["build-head-perf_scenarios"])
        self.assertEqual([plan.side for plan in plans if plan.counters], ["head", "head"])
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S1/default | workload | window.attempts (count) | n/a | 5 (5–5) | n/a |", document)
        self.assertIn("- Built with `--features perf-counters`: head\n", document)

    def test_a_head_without_the_feature_skips_the_counters_set_and_says_so(self):
        # No head build gets --features, no counters run is planned, and both the log and the table say why.
        code, gate, _calls, plans, _work, out = self.compare(options=("--counters",))
        self.assertEqual(code, 0)
        self.assertFalse(any(plan.counters for plan in plans))
        self.assertFalse(any("--features" in step.argv for step in gate.steps))
        self.assertIn("counters: head does not support them", self.printed)
        self.assertIn("counters: head does not support them", (out / "comparison.md").read_text(encoding="utf-8"))

    def test_the_overhead_table_covers_s2_and_s3_only(self):
        # The head's counters runs against its timed runs, for S2 here and never for S1.
        listing = {"schema_version": 1, "scenarios": LIST_JSON["scenarios"] + [
            {"id": "S2", "variants": ["default"], "title": "Typing", "timeout_s": 120, "short_timeout_s": 30}]}
        _code, _gate, _calls, _plans, _work, out = self.compare(
            options=("--counters",), head_manifest=COUNTERS_MANIFEST, listing=listing, scenarios=("S1", "S2"))
        document = (out / "comparison.md").read_text(encoding="utf-8")
        overhead = document.split("### Counters overhead", 1)[1].split("###", 1)[0]
        self.assertIn("counters-on vs counters-off on the head; sequential sets, not interleaved", overhead)
        self.assertIn("| S2/default | status | 1 valid run | 1 valid run |", overhead)
        self.assertNotIn("S1/default", overhead)

    def test_a_build_whose_compiler_helper_was_cleaned_still_builds(self):
        # On Windows a finished build whose linker helper the gate cleaned ends CLEANED_NOT_NATURAL, exit 0;
        # its binary is used, and each build is the gate's reviewed step.
        code, gate, _git_calls, plans, _work, _out = self.compare(build_status="CLEANED_NOT_NATURAL")
        self.assertEqual(code, 0)
        # Both fixture trees define S1/atlas-retry's methods, so both build the plain set with that cfg on.
        plain = gate.PERF_CFG_BUILDS[(("perf_atlas_retry_api",), ())]
        self.assertIs(gate.steps[0], plain["build-head-perf_scenarios"])
        self.assertIs(gate.steps[1], plain["build-base-perf_scenarios"])
        self.assertEqual({plan.binary for plan in plans}, {Path("/base/perf_scenarios"), Path("/head/perf_scenarios")})

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

    def test_a_short_comparison_plans_short_runs_and_says_so(self):
        # `--short` reaches every run, and the table's details say the runs were short and how it was invoked.
        _code, _gate, _git_calls, plans, _work, out = self.compare(options=("--short",))
        self.assertTrue(plans and all(plan.short for plan in plans))
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("--runs 1 --short", document)
        self.assertIn("- Run length: short", document)
        _code, _gate, _git_calls, plans, _work, out = self.compare()
        self.assertFalse(any(plan.short for plan in plans))
        self.assertIn("- Run length: full", (out / "comparison.md").read_text(encoding="utf-8"))

    def test_the_details_record_release_profile_overrides(self):
        # A CI run that relaxes the release profile to fit its time budget says so beside the table.
        overrides = {"CARGO_PROFILE_RELEASE_LTO": "off", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16"}
        with mock.patch.dict(os.environ, {}, clear=False):
            for name in list(os.environ):
                if name.startswith("CARGO_PROFILE_RELEASE_"):
                    del os.environ[name]
            _code, _gate, _git_calls, _plans, _work, out = self.compare(environ=overrides)
            document = (out / "comparison.md").read_text(encoding="utf-8")
            self.assertIn("- Release profile overrides (both refs): CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 "
                          "CARGO_PROFILE_RELEASE_LTO=off", document)
            _code, _gate, _git_calls, _plans, _work, out = self.compare()
            self.assertIn("- Release profile overrides (both refs): none",
                          (out / "comparison.md").read_text(encoding="utf-8"))

    def test_each_side_runs_in_its_own_worktree(self):
        # A run's cwd is the worktree that built its binary, so asset_dir() finds that ref's own assets.
        _code, _gate, _git_calls, plans, work, _out = self.compare()
        self.assertEqual({(plan.side, plan.source_root) for plan in plans},
                         {("base", work / "base"), ("head", work / "head")})

    def test_a_tree_without_its_own_assets_cannot_run(self):
        # A base without its assets is blocked with the reason; a head without them fails the comparison.
        code, _gate, _git_calls, plans, _work, out = self.compare(assets=("head",))
        self.assertEqual(code, 0)
        self.assertEqual([plan.side for plan in plans], ["head"])
        self.assertIn("blocked: base cannot run perf_scenarios", (out / "comparison.md").read_text())
        with self.assertRaises(ValueError):
            self.compare(assets=("base",))

    def test_a_base_that_cannot_build_prints_blocked_with_the_error(self):
        # The head is still measured, and the table names the base's compiler error.
        code, _gate, _git_calls, plans, _work, out = self.compare(base_build="FAIL")
        self.assertEqual(code, 0)
        self.assertEqual([plan.side for plan in plans], ["head"])
        self.assertIn("blocked: base cannot build perf_scenarios: error[E0599]", (out / "comparison.md").read_text())



class StrictBaseTests(CompareHarness, unittest.TestCase):
    """`--require-base`: a CI comparison whose base cannot build, list or measure fails instead of passing."""

    STRICT = ("--require-base",)

    def test_a_base_build_failure_raises_like_the_heads(self):
        # Under --require-base the base's compiler error ends the comparison before any run.
        with self.assertRaisesRegex(ValueError, "the base cannot build perf_scenarios: error\\[E0599\\]"):
            self.compare(base_build="FAIL", options=self.STRICT)

    def test_a_base_asset_problem_raises_like_the_heads(self):
        # A base whose own assets are missing would measure another font, so the strict comparison stops.
        with self.assertRaisesRegex(ValueError, "the base cannot run perf_scenarios"):
            self.compare(assets=("head",), options=self.STRICT)

    def test_a_base_that_cannot_list_raises(self):
        # Both binaries must load and list their scenarios before anything is measured.
        with self.assertRaisesRegex(ValueError, "--list"):
            self.compare(list_fail=("base",), options=self.STRICT)

    def test_a_base_blocked_at_runtime_fails_and_marks_the_table_incomplete(self):
        # A base retired by an exit-5 run leaves a head-only table: exit 1, and comparison.md says so first.
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=outcome_of("blocked"), options=self.STRICT)
        self.assertEqual(code, perf.EXIT_FAIL)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertTrue(document.startswith("**Incomplete comparison:**"), document[:200])
        self.assertIn("S1/default timed base", document.split("\n", 1)[0])

    def test_a_base_out_of_retries_fails(self):
        # A base that never produces a valid run is retired by the schedule; strict mode fails on it.
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=outcome_of("invalid"), options=self.STRICT)
        self.assertEqual(code, perf.EXIT_FAIL)
        self.assertTrue((out / "comparison.md").read_text(encoding="utf-8").startswith("**Incomplete comparison:**"))

    def test_both_sides_blocked_fail_rather_than_report_blocked(self):
        # Without the flag a blocked head exits 3; under it every missing side is a failure.
        blocked = perf.SetResult("S1/default", "timed", perf.SideRuns(blocked="exit 5"),
                                 perf.SideRuns(blocked="exit 5"), target_runs=5)
        self.assertEqual(perf.comparison_exit([blocked], require_base=False), perf.EXIT_BLOCKED)
        self.assertEqual(perf.comparison_exit([blocked], require_base=True), perf.EXIT_FAIL)
        self.assertEqual(len(perf.strict_problems([blocked])), 2)

    def test_a_side_short_of_its_runs_is_a_problem(self):
        # Valid runs below the set's target are incomplete even when nothing was marked blocked.
        short = perf.SetResult("S1/default", "timed", perf.SideRuns(outcomes=[object()] * 5),
                               perf.SideRuns(outcomes=[object()] * 4), target_runs=5)
        self.assertEqual(perf.strict_problems([short]), ["S1/default timed head: 4 of 5 valid runs"])

    def test_an_empty_result_set_fails(self):
        # A strict comparison that ran no set measured nothing, so it cannot pass; a lenient one is unchanged.
        self.assertEqual(perf.strict_problems([]), ["no scenario set ran"])
        self.assertEqual(perf.comparison_exit([], require_base=True), perf.EXIT_FAIL)
        self.assertEqual(perf.comparison_exit([], require_base=False), perf.EXIT_PASS)

    def test_a_set_without_a_positive_target_fails(self):
        # A set whose target was never set (0) would pass with no runs at all; each side is named.
        unset = perf.SetResult("S1/default", "timed", perf.SideRuns(), perf.SideRuns())
        self.assertEqual(perf.strict_problems([unset]), ["S1/default timed base: target 0 is not positive",
                                                         "S1/default timed head: target 0 is not positive"])
        self.assertEqual(perf.comparison_exit([unset], require_base=True), perf.EXIT_FAIL)

    def test_more_valid_runs_than_the_target_fails(self):
        # Counts must be exact: six base runs against a target of five is not the comparison that was planned.
        extra = perf.SetResult("S1/default", "timed", perf.SideRuns(outcomes=[object()] * 6),
                               perf.SideRuns(outcomes=[object()] * 5), target_runs=5)
        self.assertEqual(perf.strict_problems([extra]), ["S1/default timed base: 6 of 5 valid runs"])
        self.assertEqual(perf.comparison_exit([extra], require_base=True), perf.EXIT_FAIL)

    def test_the_counters_exception_survives_exact_counts(self):
        # A head-only counters set (base without perf-counters) still passes when the head has exactly its runs.
        head_only = perf.SetResult("S1/default", "counters", perf.SideRuns(blocked=perf.COUNTERS_HEAD_ONLY),
                                   perf.SideRuns(outcomes=[object()] * 2), target_runs=2)
        self.assertEqual(perf.strict_problems([head_only]), [])

    def test_a_counters_set_whose_base_has_no_feature_still_passes_with_n_a(self):
        # The one allowed gap: a base without perf-counters runs no counters set, and its cells read n/a.
        code, _gate, _calls, _plans, _work, out = self.compare(
            options=self.STRICT + ("--counters", "--counters-runs", "2"), head_manifest=COUNTERS_MANIFEST)
        self.assertEqual(code, perf.EXIT_PASS)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertNotIn("Incomplete comparison", document)
        self.assertIn("| S1/default | workload | window.attempts (count) | n/a | 5 (5–5) | n/a |", document)
        self.assertIn("--require-base", document)

    def test_without_the_flag_a_blocked_base_still_passes(self):
        # A local comparison stays lenient: the head is measured and the base reads blocked, exit 0.
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=outcome_of("blocked"))
        self.assertEqual(code, perf.EXIT_PASS)
        self.assertNotIn("Incomplete comparison", (out / "comparison.md").read_text(encoding="utf-8"))



# The producer's GitHub context, which the consumer's own binding flags and environment must match.
PRODUCER_ENV = {"GITHUB_RUN_ID": "37118041050", "GITHUB_RUN_ATTEMPT": "1", "ImageOS": "macos14",
                "ImageVersion": "20260928.1", "CARGO_PROFILE_RELEASE_LTO": "off",
                "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16"}


class PrebuiltHarness(CompareHarness):
    """Produces binaries with `--build-only`, then consumes them with `--prebuilt` in a fresh comparison."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.shared = Path(temporary.name)

    def environment(self, **overrides):
        """The producer's environment with `overrides`; any other CARGO_PROFILE_RELEASE_* is cleared."""
        environ = {name: value for name, value in os.environ.items() if not name.startswith("CARGO_PROFILE_RELEASE_")}
        environ.update(PRODUCER_ENV)
        environ.update(overrides)
        return environ

    def produce(self, options=(), **kwargs):
        """Run `--build-only` into a shared directory; return it and the manifest digest the run printed."""
        binaries = self.shared / "perf-binaries"
        environ = kwargs.pop("environ", None) or self.environment()
        with mock.patch.dict(os.environ, environ, clear=True):
            code, *_rest = self.compare(options=("--require-base", "--build-only", str(binaries), *options),
                                        real_binaries=True, **kwargs)
        self.assertEqual(code, perf.EXIT_PASS)
        printed = re.search(r"^manifest_sha256=([0-9a-f]{64})$", self.printed, re.M)
        self.assertIsNotNone(printed, self.printed)
        return binaries, printed.group(1)

    def consume(self, binaries, digest, run_id="37118041050", attempt="1", options=(), environ=None, **kwargs):
        """Run a strict comparison on the produced binaries; return what `compare` returns."""
        binding = ("--prebuilt", str(binaries), "--prebuilt-run-id", run_id, "--prebuilt-attempt", attempt,
                   "--prebuilt-manifest-sha256", digest)
        with mock.patch.dict(os.environ, environ or self.environment(), clear=True):
            return self.compare(options=("--require-base", *binding, *options), **kwargs)

    @staticmethod
    def rewrite(binaries, change):
        """Apply `change` to the manifest, write it back and return its new digest, so rule 2 still passes."""
        manifest = binaries / "manifest.json"
        data = json.loads(manifest.read_text(encoding="utf-8"))
        change(data)
        manifest.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        return hashlib.sha256(manifest.read_bytes()).hexdigest()


class BuildOnlyTests(PrebuiltHarness, unittest.TestCase):
    """`--build-only DIR`: the producer builds both refs once and refuses every comparison option."""

    def test_comparison_options_are_rejected(self):
        # A producer measures nothing, so a run option would be silently ignored; each is a usage error.
        for extra in (("--scenario", "S1"), ("--runs", "5"), ("--short",), ("--laps",), ("--counters",),
                      ("--counters-runs", "2"), ("--keep",), ("--out", "/tmp/out"),
                      ("--prebuilt", "/tmp/binaries")):
            with self.subTest(extra=extra), self.assertRaises(SystemExit):
                self.parse("--base", "main", "--head", "HEAD", "--require-base", "--build-only", "/tmp/b", *extra)
        with self.assertRaises(SystemExit):
            # A producer is strict: a manifest always holds both sides.
            self.parse("--base", "main", "--head", "HEAD", "--build-only", "/tmp/b")
        self.assertTrue(self.parse("--base", "main", "--head", "HEAD", "--require-base", "--build-only", "/tmp/b",
                                   "--alloc").alloc)

    def test_prebuilt_needs_every_binding_flag(self):
        # A consumer binds the artifact to the run, the producer's attempt and the published digest.
        binding = ("--prebuilt-run-id", "1", "--prebuilt-attempt", "1", "--prebuilt-manifest-sha256", "a" * 64)
        for index in range(0, len(binding), 2):
            partial = binding[:index] + binding[index + 2:]
            with self.subTest(missing=binding[index]), self.assertRaises(SystemExit):
                self.parse("--base", "main", "--head", "HEAD", "--prebuilt", "/tmp/b", *partial)
        with self.assertRaises(SystemExit):
            self.parse("--base", "main", "--head", "HEAD", *binding)
        with self.assertRaises(SystemExit):
            self.parse("--base", "main", "--head", "HEAD", "--prebuilt", "/tmp/b", *binding[:4],
                       "--prebuilt-manifest-sha256", "not-hex")
        self.assertEqual(self.parse("--base", "main", "--head", "HEAD", "--prebuilt", "/tmp/b", *binding)
                         .prebuilt_attempt, "1")

    def parse(self, *argv):
        with contextlib.redirect_stderr(io.StringIO()):
            return perf.parse_args(list(argv))

    def test_a_head_build_failure_fails(self):
        # The head builds first and its error ends the producer.
        with self.assertRaisesRegex(ValueError, "the head cannot build"):
            self.produce(head_build="FAIL")

    def test_a_base_build_failure_fails(self):
        # A producer never publishes a manifest without the base.
        with self.assertRaisesRegex(ValueError, "the base cannot build"):
            self.produce(base_build="FAIL")

    def test_a_producer_whose_binary_cannot_list_fails(self):
        # The producer lists both binaries before publishing them.
        with self.assertRaisesRegex(ValueError, "--list"):
            self.produce(list_fail=("head",))

    def test_alloc_adds_the_alloc_example(self):
        # `--alloc` builds and publishes perf_scenarios_alloc on both sides.
        binaries, _digest = self.produce(options=("--alloc",))
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        for side in perf.SIDES:
            self.assertEqual(sorted(manifest["binaries"][side]), sorted([perf.ALLOC_EXAMPLE, perf.HARNESS_EXAMPLE]))


class PrebuiltManifestTests(PrebuiltHarness, unittest.TestCase):
    """What the producer's manifest.json records, and the digest it prints for the workflow."""

    def test_every_field_is_recorded(self):
        # The manifest binds the binaries to the run, both SHAs, the harness, toolchain, profile and image.
        binaries, digest = self.produce(head_manifest=COUNTERS_MANIFEST)
        manifest_path = binaries / "manifest.json"
        self.assertEqual(hashlib.sha256(manifest_path.read_bytes()).hexdigest(), digest)
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        self.assertEqual(manifest["schema_version"], 1)
        self.assertEqual((manifest["run_id"], manifest["run_attempt"]), ("37118041050", "1"))
        self.assertEqual((manifest["base_sha"], manifest["head_sha"]), (self.SHAS["main"], self.SHAS["HEAD"]))
        self.assertRegex(manifest["harness_hash"], r"^[0-9a-f]{64}$")
        for side in perf.SIDES:
            entry = manifest["binaries"][side][perf.HARNESS_EXAMPLE]
            self.assertEqual(entry["path"], f"{side}/{perf.executable_name(perf.HARNESS_EXAMPLE)}")
            self.assertEqual(entry["sha256"], hashlib.sha256((binaries / entry["path"]).read_bytes()).hexdigest())
            self.assertTrue(os.access(binaries / entry["path"], os.X_OK))
        self.assertEqual(manifest["target"], "aarch64-apple-darwin")
        self.assertEqual(manifest["toolchain"], {"rustc": RUSTC_VV.strip(),
                                                 "cargo": "cargo 1.99.0 (abcdef 2026-09-01)"})
        self.assertEqual(manifest["profile"]["overrides"],
                         "CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 CARGO_PROFILE_RELEASE_LTO=off")
        self.assertEqual(manifest["profile"]["lto"], {"base": "off", "head": "off"})
        self.assertEqual(manifest["features"], {"base": [], "head": [perf.COUNTERS_FEATURE]})
        self.assertEqual(manifest["runner_image"], {"os": "macos14", "version": "20260928.1"})

    def test_the_lto_setting_is_read_from_the_tree_without_an_override(self):
        # Without CARGO_PROFILE_RELEASE_LTO, a release build takes the tree's own [profile.release] lto.
        manifest_text = '[workspace]\n\n[profile.release]\nopt-level = 3\nlto = "fat"\n\n[profile.dev]\nlto = false\n'
        with tempfile.TemporaryDirectory() as temp:
            (Path(temp) / "Cargo.toml").write_text(manifest_text, encoding="utf-8")
            self.assertEqual(perf.release_lto(Path(temp), {}), "fat")
            self.assertEqual(perf.release_lto(Path(temp), {"CARGO_PROFILE_RELEASE_LTO": "off"}), "off")
            self.assertEqual(perf.release_lto(Path(temp) / "missing", {}), "unset")

    def test_the_producer_publishes_no_assets(self):
        # Only executables and the manifest move; each ref's assets come from its own tree on the consumer.
        binaries, _digest = self.produce()
        published = sorted(path.relative_to(binaries).as_posix() for path in binaries.rglob("*")
                           if path.is_file() and "build-logs" not in path.parts)
        executable = perf.executable_name(perf.HARNESS_EXAMPLE)
        self.assertEqual(published, [f"base/{executable}", f"head/{executable}", "manifest.json"])


class PrebuiltRefusalTests(PrebuiltHarness, unittest.TestCase):
    """A consumer refuses any artifact that disagrees with its own job, one rule at a time."""

    def refused(self, pattern, binaries, digest, **kwargs):
        """Assert the consumer refuses the binaries with a message matching `pattern`."""
        with self.assertRaisesRegex(ValueError, "refusing the prebuilt binaries: .*" + pattern):
            self.consume(binaries, digest, **kwargs)

    def test_rule_1_a_missing_directory_manifest_or_wrong_schema(self):
        # A download that left nothing, a directory without a manifest, or another schema version.
        binaries, digest = self.produce()
        self.refused("no directory", self.shared / "absent", digest)
        digest = self.rewrite(binaries, lambda data: data.update(schema_version=2))
        self.refused("schema", binaries, digest)
        (binaries / "manifest.json").unlink()
        self.refused("manifest", binaries, digest)

    def test_rule_2_a_manifest_whose_digest_differs(self):
        # The manifest must be byte for byte the one the producer job published.
        binaries, _digest = self.produce()
        self.refused("sha256", binaries, "0" * 64)

    def test_rule_3_another_run_or_attempt(self):
        # A stale artifact from another run, or another producer attempt, is not this job's producer.
        binaries, digest = self.produce()
        self.refused("run", binaries, digest, run_id="37000000000")
        self.refused("attempt", binaries, digest, attempt="2")

    def test_rule_4_either_sha_differs(self):
        # The producer built other commits than this shard resolved.
        for side in perf.SIDES:
            with self.subTest(side=side):
                binaries, _digest = self.produce()
                digest = self.rewrite(binaries, lambda data, side=side: data.update({f"{side}_sha": "9" * 40}))
                self.refused(f"{side} SHA", binaries, digest)
                shutil.rmtree(binaries)

    def test_rule_5_the_harness_hash_differs(self):
        # The binaries were built from another overlaid harness.
        binaries, _digest = self.produce()
        digest = self.rewrite(binaries, lambda data: data.update(harness_hash="f" * 64))
        self.refused("harness", binaries, digest)

    def test_rule_6_the_features_differ(self):
        # Each side's cargo features must be the ones this job derives from that side's tree.
        for side in perf.SIDES:
            with self.subTest(side=side):
                binaries, _digest = self.produce()
                digest = self.rewrite(binaries, lambda data, side=side: data["features"].update(
                    {side: [perf.COUNTERS_FEATURE]}))
                self.refused(f"features", binaries, digest)
                shutil.rmtree(binaries)

    def test_rule_7_target_toolchain_or_image_differs(self):
        # A rolled-over toolchain or runner image fails closed rather than mixing builds and runs.
        binaries, digest = self.produce()
        self.refused("toolchain", binaries, digest, toolchain=RUSTC_VV.replace("1.99.0", "1.100.0"))
        self.refused("target", binaries, digest, toolchain=RUSTC_VV.replace("aarch64", "x86_64"))
        self.refused("runner image", binaries, digest, environ=self.environment(ImageVersion="20261001.2"))

    def test_rule_8_the_profile_differs(self):
        # A consumer whose profile step set other overrides would describe binaries it did not get.
        binaries, digest = self.produce()
        self.refused("profile", binaries, digest, environ=self.environment(CARGO_PROFILE_RELEASE_LTO="thin"))

    def test_rule_9_a_binary_is_missing_linked_unexecutable_misdigested_or_absent(self):
        # Every published file is checked before it is copied, on each side.
        executable = perf.executable_name(perf.HARNESS_EXAMPLE)
        damages = {
            "missing": lambda path: path.unlink(),
            "symlink": lambda path: (path.rename(path.with_name("real")), path.symlink_to("real")),
            "digest": lambda path: path.write_bytes(b"tampered"),
        }
        if os.name != "nt":
            damages["executable"] = lambda path: path.chmod(0o644)
        for side in perf.SIDES:
            for damage, apply in damages.items():
                with self.subTest(side=side, damage=damage):
                    binaries, digest = self.produce()
                    try:
                        apply(binaries / side / executable)
                    except OSError:
                        # When: a Windows runner without symlink privilege cannot create the link, only it skips.
                        shutil.rmtree(binaries)
                        self.skipTest("this host cannot create a symlink")
                    self.refused(f"{side} {perf.HARNESS_EXAMPLE}.*{damage}", binaries, digest)
                    shutil.rmtree(binaries)
        binaries, digest = self.produce()
        # A selected set that needs an example the producer never built.
        self.refused(f"base {perf.ALLOC_EXAMPLE}", binaries, digest, options=("--alloc",))

    def test_rule_10_a_side_that_cannot_list_or_has_an_asset_problem(self):
        # The copied binary must load and list from that side's tree, which must hold its own assets.
        for side in perf.SIDES:
            other = "head" if side == "base" else "base"
            with self.subTest(side=side, problem="list"):
                binaries, digest = self.produce()
                self.refused("--list", binaries, digest, list_fail=(side,))
                shutil.rmtree(binaries)
            with self.subTest(side=side, problem="assets"):
                binaries, digest = self.produce()
                self.refused(f"{side} cannot run", binaries, digest, assets=(other,))
                shutil.rmtree(binaries)


class PrebuiltRerunTests(PrebuiltHarness, unittest.TestCase):
    """GitHub reruns: which producer attempt a consumer may accept."""

    def test_a_failed_job_rerun_accepts_the_successful_producer_attempt(self):
        # "Re-run failed jobs" reruns a shard at attempt 2 but reuses the producer of attempt 1.
        binaries, digest = self.produce()
        code, *_rest = self.consume(binaries, digest, attempt="1",
                                    environ=self.environment(GITHUB_RUN_ATTEMPT="2"))
        self.assertEqual(code, perf.EXIT_PASS)

    def test_a_full_rerun_refuses_the_earlier_attempt(self):
        # "Re-run all jobs" gets a new producer at attempt 2; attempt 1's artifact is no longer its producer.
        binaries, digest = self.produce()
        with self.assertRaisesRegex(ValueError, "attempt"):
            self.consume(binaries, digest, attempt="2", environ=self.environment(GITHUB_RUN_ATTEMPT="2"))

    def test_a_stale_artifact_from_another_run_is_refused(self):
        # An artifact left from another workflow run names that run's id.
        binaries, digest = self.produce(environ=self.environment(GITHUB_RUN_ID="37000000000"))
        with self.assertRaisesRegex(ValueError, "run"):
            self.consume(binaries, digest)


class PrebuiltCompareTests(PrebuiltHarness, unittest.TestCase):
    """A prebuilt comparison measures exactly as a building one does, without Cargo."""

    def test_no_cargo_build_and_each_side_runs_in_its_own_tree(self):
        # The consumer runs no build step; its runs use the copied binaries with each ref's worktree as cwd.
        binaries, digest = self.produce()
        code, gate, calls, plans, work, out = self.consume(binaries, digest)
        self.assertEqual(code, perf.EXIT_PASS)
        self.assertFalse([step.id for step in gate.steps if step.id.startswith("build-")])
        self.assertFalse([call for call in calls if call[:2] == ("cargo", "build")])
        executable = perf.executable_name(perf.HARNESS_EXAMPLE)
        self.assertEqual({(plan.side, plan.binary, plan.source_root) for plan in plans},
                         {(side, work / "prebuilt" / side / executable, work / side) for side in perf.SIDES})
        details = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn(f"- Builds: prebuilt by run 37118041050 attempt 1, manifest sha256 `{digest}`", details)

    def test_the_tables_equal_a_building_comparisons(self):
        # Only the builds' origin differs: every table row matches a comparison that built both refs itself.
        binaries, digest = self.produce(head_manifest=COUNTERS_MANIFEST)
        options = ("--counters", "--counters-runs", "2")
        _code, _gate, _calls, _plans, _work, prebuilt_out = self.consume(binaries, digest, options=options,
                                                                         head_manifest=COUNTERS_MANIFEST)
        with mock.patch.dict(os.environ, self.environment(), clear=True):
            _code, _gate, _calls, _plans, _work, built_out = self.compare(
                options=("--require-base", *options), head_manifest=COUNTERS_MANIFEST)

        def tables(out):
            return (out / "comparison.md").read_text(encoding="utf-8").split("### Host", 1)[0]
        self.assertEqual(tables(prebuilt_out), tables(built_out))

    def test_the_copied_binaries_have_no_assets_sibling(self):
        # An `assets` beside the executable would win over the tree's, so the copy directory never has one,
        # even when the producer's directory gained one.
        binaries, digest = self.produce()
        (binaries / "head" / "assets" / "fonts").mkdir(parents=True)
        code, _gate, _calls, _plans, work, _out = self.consume(binaries, digest)
        self.assertEqual(code, perf.EXIT_PASS)
        for side in perf.SIDES:
            self.assertFalse(os.path.lexists(work / "prebuilt" / side / "assets"))

    def test_timing_marks_are_written_in_order(self):
        # timing.json carries the run, attempt, job, shard and ordered marks the critical-path report reads.
        binaries, digest = self.produce()
        environ = self.environment(GITHUB_RUN_ATTEMPT="2", PERF_JOB_NAME="macOS before/after comparison (S7)",
                                   PERF_SHARD="S7")
        _code, _gate, _calls, _plans, _work, out = self.consume(binaries, digest, environ=environ)
        timing = json.loads((out / "timing.json").read_text(encoding="utf-8"))
        self.assertEqual((timing["run_id"], timing["run_attempt"], timing["job"], timing["shard"]),
                         ("37118041050", "2", "macOS before/after comparison (S7)", "S7"))
        marks = [timing["marks"][name] for name in perf.TIMING_MARKS]
        self.assertEqual(marks, sorted(marks))


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

    def test_another_display_is_rejected_even_at_the_same_rate_and_scale(self):
        # Display identity is a known difference too: runs on two named displays are not one measurement display.
        result, calls = self.run_set({"base": [display_run(60000, name="DELL U2720Q")],
                                      "head": [display_run(60000, name="LG HDR 4K"),
                                               display_run(60000, name="DELL U2720Q")]})
        self.assertEqual(calls, ["base", "head", "head"])
        _side, _evidence, kind, why = result.attempts[1]
        self.assertEqual(kind, "display")
        self.assertIn("LG HDR 4K", why[0])
        self.assertIn("DELL U2720Q", why[0])

    def test_an_unknown_rate_does_not_hide_a_known_difference(self):
        # Only what a run did not report goes unchecked: a known scale or name must still match the reference.
        result, calls = self.run_set({"base": [display_run(60000)],
                                      "head": [display_run(None, scale=1.0), display_run(60000)]})
        self.assertEqual(calls, ["base", "head", "head"])
        _side, _evidence, kind, why = result.attempts[1]
        self.assertEqual(kind, "display")
        self.assertIn("Built-in Display, unknown Hz, scale 1", why[0])
        for unchecked in (no_display, display_run(None, scale=2.0)):
            with self.subTest(unchecked=unchecked):
                result, _calls = self.run_set({"base": [display_run(60000)], "head": [unchecked]})
                self.assertEqual([attempt[2] for attempt in result.attempts], ["valid", "valid"])
        self.assertEqual(perf.describe_display(perf.display_of(valid_result(monitor=None))), "unknown")

    def test_a_reference_learns_a_rate_it_did_not_have(self):
        # A first run with no reported rate sets the reference; the next reported rate is learned, then enforced.
        display = perf.DisplayReference()
        self.run_set({"base": [display_run(None)], "head": [display_run(60000)]}, display=display)
        self.assertEqual(display.monitor["refresh_rate_millihertz"], 60000)
        result, _calls = self.run_set({"base": [display_run(75000), display_run(60000)],
                                       "head": [display_run(60000)]}, display=display)
        self.assertEqual(result.attempts[0][2], "display")

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
            code, reasons = perf.smoke_cases(SmokeTests.SCENARIOS, Path("/b"), HARNESS_HASH, run_case, Path("/e"),
                                             host_platform="darwin")
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


class AssetCheckTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(os.path.realpath(temporary.name))
        self.tree = self.root / "checkout" / "target" / "perf-compare" / "work" / "head"
        self.tree.mkdir(parents=True)
        self.binary = self.root / "cargo-target" / "release" / "examples" / "perf_scenarios"
        self.binary.parent.mkdir(parents=True)

    def fonts(self, assets):
        """Give an assets directory the tracked font the harness configures."""
        (assets / "fonts").mkdir(parents=True)
        (assets / "fonts" / "RecMonoSt.Helens-Regular.ttf").write_bytes(b"font")

    def problem(self):
        return perf.asset_problem(self.binary, self.tree, platform="darwin")

    def test_the_trees_own_assets_with_fonts_pass(self):
        # A run started in the tree resolves that tree's assets and its tracked fonts.
        self.fonts(self.tree / "assets")
        self.assertIsNone(self.problem())

    def test_assets_found_only_above_the_tree_or_nowhere_are_refused(self):
        # An ancestor's assets would load another tree's fonts, and none at all falls back to another font.
        self.assertIn("assets", self.problem())
        self.fonts(self.root / "checkout" / "assets")
        self.assertIn(str(self.root / "checkout" / "assets"), self.problem())

    def test_assets_without_fonts_are_refused(self):
        # The App loads the configured font from assets/fonts.
        (self.tree / "assets").mkdir()
        self.assertIn("fonts", self.problem())

    def test_packaged_assets_beside_the_binary_take_precedence_and_are_refused(self):
        # asset_dir() prefers assets beside the executable, which are not the tree's own.
        self.fonts(self.tree / "assets")
        self.fonts(self.binary.parent / "assets")
        self.assertIn(str(self.binary.parent / "assets"), self.problem())


class SmokeSetupTests(unittest.TestCase):
    def test_a_checkout_without_assets_fails_before_any_run(self):
        # The smoke runs from the repository root; without its assets the App would load another font.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact = {"reason": "compiler-artifact", "target": {"name": "perf_scenarios", "kind": ["example"]},
                        "executable": str(root / "debug" / "perf_scenarios")}
            gate = FakeGate(lambda step: ("PASS", 0, json.dumps(artifact) + "\n"))
            gate.sigchld_problem = lambda: None
            gate.leader_watches = lambda: ("kqueue",)
            evidence = root / "evidence"
            evidence.mkdir()
            with mock.patch.object(perf, "load_gate", return_value=gate), \
                    mock.patch.object(perf, "ROOT", root / "checkout"):
                code, reasons = perf.run_smoke(evidence)
        self.assertEqual(code, perf.EXIT_FAIL)
        self.assertTrue(any("assets" in reason for reason in reasons), reasons)
        self.assertEqual(len(gate.steps), 1)


class HomeSymlinkTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.home, self.elsewhere = root / ".sonicterm", root / "dot-config"
        self.home.mkdir()
        self.elsewhere.mkdir()
        # Later than every file and link the test creates, so only a changed entry or target is a candidate.
        self.sentinel_ns = perf.time.time_ns() + 10 ** 15

    def link(self, relative, target, directory=False):
        """Create a symlink under the home, or skip on a host that cannot create one."""
        path = self.home / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            path.symlink_to(target, target_is_directory=directory)
        except (OSError, NotImplementedError):
            self.skipTest("this host cannot create a symlink")
        return path

    def write(self, path, data):
        """Write a file with an old modification time."""
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        os.utime(path, ns=(1_000_000_000, 1_000_000_000))

    def violations(self, before):
        return perf.home_violations(self.home, before, perf.snapshot_home(self.home), self.sentinel_ns,
                                    HARNESS_PID, False, ["/scratch-run"])

    def test_a_write_through_a_linked_file_is_a_candidate(self):
        # The main config may be a symlink into a dot-config tree; a write to its target is a write to the home.
        target = self.elsewhere / "sonicterm.toml"
        self.write(target, b"font = 1\n")
        self.link("sonicterm.toml", target)
        before = perf.snapshot_home(self.home)
        self.write(target, b"font = 2, changed\n")
        self.assertEqual(self.violations(before), ["sonicterm.toml: changed"])

    def test_the_sentinel_comparison_uses_the_link_target(self):
        # A target written after the sentinel is a candidate, however old the link itself is.
        target = self.elsewhere / "apollo.toml"
        self.write(target, b"x")
        self.link("themes/apollo.toml", target)
        # 1 s later: a filesystem stores file times at its own resolution, 100 ns on NTFS and 1 s on HFS+.
        later_ns = self.sentinel_ns + 1_000_000_000
        os.utime(target, ns=(later_ns, later_ns))
        self.assertEqual(self.violations(perf.snapshot_home(self.home)), ["themes/apollo.toml: newer than the sentinel"])

    def test_a_linked_directory_is_walked(self):
        # ~/.sonicterm/logs may point elsewhere; a file written there is a file written under the home.
        (self.elsewhere / "logs").mkdir()
        self.link("logs", self.elsewhere / "logs", directory=True)
        before = perf.snapshot_home(self.home)
        self.write(self.elsewhere / "logs" / "crash-1.log", b"crash")
        self.assertEqual(self.violations(before), ["logs/crash-1.log: added"])

    def test_a_dangling_link_records_only_its_target_text(self):
        # A link to nothing has no size or time to compare; retargeting it is still a change.
        link = self.link("keymaps/sonicterm-macos.toml", self.elsewhere / "missing-a.toml")
        before = perf.snapshot_home(self.home)
        self.assertEqual(before["keymaps/sonicterm-macos.toml"], (None, None, os.readlink(link)))
        link.unlink()
        self.link("keymaps/sonicterm-macos.toml", self.elsewhere / "missing-b.toml")
        self.assertEqual(self.violations(before), ["keymaps/sonicterm-macos.toml: changed"])

    def test_a_link_cycle_is_recorded_and_its_directory_walked_once(self):
        # A link back into the home is recorded, and the real directory it names is not walked again.
        self.write(self.home / "themes" / "apollo.toml", b"x")
        self.link("themes/loop", self.home / "themes", directory=True)
        self.assertEqual(sorted(perf.snapshot_home(self.home)), ["themes/apollo.toml", "themes/loop"])

    def test_a_walk_past_its_bound_is_unresolved(self):
        # The walk is bounded; reaching the bound makes the check unresolved instead of skipping the rest.
        for index in range(5):
            self.write(self.home / "logs" / f"sonicterm.log.{index}", b"x")
        with self.assertRaises(perf.HomeSnapshotUnresolved):
            perf.snapshot_home(self.home, max_entries=3)
        self.assertEqual(len(perf.snapshot_home(self.home, max_entries=6)), 5)

    @unittest.skipIf(os.name == "nt" or (hasattr(os, "geteuid") and os.geteuid() == 0),
                     "needs POSIX permissions as a non-root user")
    def test_an_unreadable_link_target_is_unresolved(self):
        # A target that cannot be read could hide a write, so the check is unresolved, never skipped.
        locked = self.elsewhere / "locked"
        self.write(locked / "sonicterm.toml", b"x")
        self.link("sonicterm.toml", locked / "sonicterm.toml")
        locked.chmod(0)
        self.addCleanup(locked.chmod, 0o700)
        with self.assertRaises(perf.HomeSnapshotUnresolved):
            perf.snapshot_home(self.home)


# --- perf.yml as a job graph: a YAML reader, an `if:` evaluator and a push simulator ---------------

PERF_WORKFLOW = Path(__file__).resolve().parent.parent / ".github" / "workflows" / "perf.yml"
# Every perf.yml job's eligibility: a tag push, or a `perf` pull request whose event is not another label.
ELIGIBILITY = ("github.event_name == 'push' || (contains(github.event.pull_request.labels.*.name, 'perf') && "
               "(github.event.action != 'labeled' || github.event.label.name == 'perf'))")
PERF_JOBS = ("perf-build-macos", "compare-macos", "compare-windows", "perf-result")
COMPARISON_JOBS = ("compare-macos", "compare-windows")
REF_STEP = "Choose the refs and the run length"
PROFILE_STEP = "Relax the release profile for a pull request"
BUILD_STEP = "Build both refs once"
PACKAGE_STEP = "Package the binaries"
UPLOAD_BINARIES_STEP = "Upload the binaries"
BUILD_EVIDENCE_STEP = "Upload the build evidence"
PLAN_STEP = "Check the producer's refs"
DOWNLOAD_STEP = "Download the binaries"
UNPACK_STEP = "Unpack the binaries"
COMPARE_STEP = "Compare the base and the head"
SUMMARY_STEP = "Publish the table in the job summary"
EVIDENCE_STEP = "Upload the comparison evidence"
RESULT_STEP = "Require every comparison job to succeed"
# Every scenario set a comparison measures: each runs in exactly one shard per platform. The named variants
# run beside their scenario's default, since a bare ID selects only the default; S1/atlas-retry runs only
# its counters set.
ALL_SCENARIOS = ["S1", "S1/atlas-retry", "S2", "S2/flood", "S3", "S4", "S5", "S6", "S6/flood", "S6/selection-drag",
                 "S7", "S8", "S9", "S10", "S10/sync", "S11", "S12"]
# The variants only one platform's shards add: S11/release on both (capped at 1), the presenter controls on Windows.
PLATFORM_SCENARIOS = {"macOS": ["S11/release"], "Windows": ["S11/release", "S11/gdi", "S11/wgpu"]}
JOB_RESULTS = ("success", "failure", "cancelled", "skipped", "")

_YAML_ENTRY = re.compile(r"(?P<key>[A-Za-z0-9_.-]+)\s*:(?:\s+(?P<value>.*))?$")


def _indent(line):
    """The number of leading spaces of a YAML line."""
    return len(line) - len(line.lstrip(" "))


def _strip_yaml_comment(text):
    """Drop a `#` comment that follows whitespace outside quotes."""
    quote = None
    for position, char in enumerate(text):
        if quote:
            if char == quote:
                quote = None
        elif char in "'\"" and (position == 0 or text[position - 1].isspace()):
            quote = char
        elif char == "#" and (position == 0 or text[position - 1].isspace()):
            return text[:position].rstrip()
    return text.strip()


def _flow_items(text):
    """Split a flow sequence's inside on commas outside quotes."""
    items, current, quote = [], "", None
    for char in text:
        if quote and char == quote:
            quote = None
        elif not quote and char in "'\"":
            quote = char
        if char == "," and not quote:
            items.append(current.strip())
            current = ""
        else:
            current += char
    if current.strip():
        items.append(current.strip())
    return items


def _yaml_scalar(text):
    """Decode a plain, quoted or flow-sequence scalar; plain scalars stay strings."""
    text = _strip_yaml_comment(text)
    if text.startswith('"'):
        return json.loads(text)
    if text.startswith("'"):
        return text[1:-1].replace("''", "'")
    if text.startswith("["):
        return [_yaml_scalar(item) for item in _flow_items(text[1:-1])]
    return text


class WorkflowYaml:
    """The YAML subset the workflows use: block mappings and sequences, plain, quoted and flow scalars, `|` and `>-`.

    PyYAML is not on every runner that runs these tests, and the supply-chain checker already confines the
    workflows to this directly auditable grammar; anything else raises instead of being guessed at.
    """

    def __init__(self, text):
        self.lines = text.splitlines()
        self.index = 0

    @classmethod
    def parse(cls, text):
        """Parse a whole document; a line left unread is an error."""
        reader = cls(text)
        document = reader.node(0)
        reader.skip_blank()
        if reader.index != len(reader.lines):
            raise ValueError(f"unparsed line {reader.index + 1}: {reader.lines[reader.index]!r}")
        return document

    def skip_blank(self):
        """Skip blank and comment lines between nodes (never inside a block scalar)."""
        while self.index < len(self.lines) and (not self.lines[self.index].strip()
                                                or self.lines[self.index].lstrip().startswith("#")):
            self.index += 1

    def node(self, minimum):
        """Read the mapping or sequence that starts at or deeper than `minimum`, or "" when there is none."""
        self.skip_blank()
        if self.index >= len(self.lines) or _indent(self.lines[self.index]) < minimum:
            return ""
        level = _indent(self.lines[self.index])
        text = self.lines[self.index].strip()
        return self.sequence(level) if text == "-" or text.startswith("- ") else self.mapping(level)

    def mapping(self, level):
        """Read `key: value` lines at exactly `level`."""
        result = {}
        while True:
            self.skip_blank()
            if self.index >= len(self.lines) or _indent(self.lines[self.index]) < level:
                return result
            line = self.lines[self.index]
            text = line.strip()
            match = _YAML_ENTRY.match(text)
            if _indent(line) > level or text.startswith("- ") or match is None:
                raise ValueError(f"line {self.index + 1} is not a mapping entry at indent {level}: {line!r}")
            self.index += 1
            key = match.group("key")
            if key in result:
                raise ValueError(f"duplicate key {key!r} at line {self.index}")
            result[key] = self.value(match.group("value") or "", level)

    def value(self, raw, level):
        """Read an entry's value: a block scalar, an inline scalar or a nested node."""
        raw = _strip_yaml_comment(raw)
        if raw in ("|", "|-", ">", ">-"):
            return self.block(raw, level)
        if raw:
            return _yaml_scalar(raw)
        return self.node(level + 1)

    def sequence(self, level):
        """Read `- item` lines at exactly `level`; an item whose line holds a key is a mapping at that key's column."""
        items = []
        while True:
            self.skip_blank()
            if self.index >= len(self.lines) or _indent(self.lines[self.index]) < level:
                return items
            line = self.lines[self.index]
            text = line.strip()
            if _indent(line) > level:
                raise ValueError(f"line {self.index + 1} is deeper than its sequence: {line!r}")
            if not (text == "-" or text.startswith("- ")):
                return items
            rest = text[1:].lstrip()
            column = _indent(line) + len(text) - len(rest)
            if not rest:
                self.index += 1
                items.append(self.node(level + 1))
            elif _YAML_ENTRY.match(rest) and not rest.startswith(("'", '"')):
                # The mapping's first key shares the dash's line: re-read the line as that key at its column.
                self.lines[self.index] = " " * column + rest
                items.append(self.mapping(column))
            else:
                self.index += 1
                items.append(_yaml_scalar(rest))

    def block(self, style, level):
        """Read a literal (`|`) or folded (`>`) block deeper than `level`; `-` drops the final newline."""
        collected = []
        while self.index < len(self.lines):
            line = self.lines[self.index]
            if line.strip() and _indent(line) <= level:
                break
            collected.append(line)
            self.index += 1
        while collected and not collected[-1].strip():
            collected.pop()
        if not collected:
            return ""
        content = min(_indent(line) for line in collected if line.strip())
        body = [line[content:] if line.strip() else "" for line in collected]
        ending = "" if style.endswith("-") else "\n"
        if style.startswith("|"):
            return "\n".join(body) + ending
        return " ".join(line.strip() for line in body if line.strip()) + ending


def load_perf_workflow():
    """perf.yml, parsed."""
    return WorkflowYaml.parse(PERF_WORKFLOW.read_text(encoding="utf-8"))


def job_step(workflow, job_id, name):
    """The one step of a job with this name."""
    found = [step for step in workflow["jobs"][job_id]["steps"] if step.get("name") == name]
    if len(found) != 1:
        raise AssertionError(f"{job_id} has {len(found)} steps named {name!r}")
    return found[0]


class UnsupportedExpression(ValueError):
    """An expression form the simulator does not model; a test never guesses at its meaning."""


_EXPRESSION_TOKEN = re.compile(r"\s*(?:(?P<string>'(?:[^']|'')*')|(?P<operator>==|!=|&&|\|\||[!(),])"
                               r"|(?P<name>[A-Za-z_][A-Za-z0-9_.*-]*))")
STATUS_FUNCTIONS = ("always", "success", "failure", "cancelled")
# What an `if:` may read (eligibility reads the pull request's action and labels); a `${{ }}` value may
# also read the rest of github, needs, matrix and runner.temp.
IF_REFERENCES = (re.compile(r"steps\.[A-Za-z0-9_-]+\.outputs\.[A-Za-z0-9_-]+"), re.compile(r"runner\.os"),
                 re.compile(r"github\.event_name"), re.compile(r"github\.event\.action"),
                 re.compile(r"github\.event\.label\.name"),
                 re.compile(r"github\.event\.pull_request\.labels\.\*\.name"))
VALUE_REFERENCES = IF_REFERENCES + (
    re.compile(r"github\.[A-Za-z0-9_.]+"), re.compile(r"matrix\.[A-Za-z0-9_]+"), re.compile(r"runner\.temp"),
    re.compile(r"needs\.[A-Za-z0-9_-]+\.(?:result|outputs\.[A-Za-z0-9_-]+)"))


class _ExpressionParser:
    """Recursive descent over `||`, `&&`, `!`, `==`, `!=`, parentheses, string literals, calls and references."""

    def __init__(self, text):
        self.tokens = []
        position, text = 0, text.strip()
        while position < len(text):
            match = _EXPRESSION_TOKEN.match(text, position)
            if match is None or match.end() == position:
                raise UnsupportedExpression(f"cannot read {text[position:]!r}")
            self.tokens.append((match.lastgroup, match.group(match.lastgroup)))
            position = match.end()
        self.position = 0

    def peek(self):
        return self.tokens[self.position] if self.position < len(self.tokens) else (None, None)

    def take(self, expected=None):
        token = self.peek()
        if token[0] is None or (expected is not None and token != ("operator", expected)):
            raise UnsupportedExpression(f"expected {expected or 'a token'}, found {token[1]!r}")
        self.position += 1
        return token

    def parse(self):
        tree = self.disjunction()
        if self.position != len(self.tokens):
            raise UnsupportedExpression(f"trailing {self.tokens[self.position:]}")
        return tree

    def disjunction(self):
        tree = self.conjunction()
        while self.peek() == ("operator", "||"):
            self.take()
            tree = ("or", tree, self.conjunction())
        return tree

    def conjunction(self):
        tree = self.unary()
        while self.peek() == ("operator", "&&"):
            self.take()
            tree = ("and", tree, self.unary())
        return tree

    def unary(self):
        if self.peek() == ("operator", "!"):
            self.take()
            return ("not", self.unary())
        tree = self.primary()
        if self.peek() in (("operator", "=="), ("operator", "!=")):
            operator = self.take()[1]
            tree = ("eq" if operator == "==" else "ne", tree, self.primary())
        return tree

    def primary(self):
        kind, value = self.take()
        if (kind, value) == ("operator", "("):
            tree = self.disjunction()
            self.take(")")
            return tree
        if kind == "string":
            return ("literal", value[1:-1].replace("''", "'"))
        if kind != "name":
            raise UnsupportedExpression(f"unexpected {value!r}")
        if self.peek() != ("operator", "("):
            return ("reference", value)
        self.take("(")
        arguments = []
        if self.peek() != ("operator", ")"):
            arguments.append(self.disjunction())
            while self.peek() == ("operator", ","):
                self.take()
                arguments.append(self.disjunction())
        self.take(")")
        return ("call", value, arguments)


def _truthy(value):
    """GitHub's coercion for the values modelled here: a non-empty string or True."""
    return bool(value)


def _uses_status(tree):
    """Whether an expression calls a status function, which suppresses the implicit `success() &&`."""
    if tree[0] == "call":
        return tree[1] in STATUS_FUNCTIONS or any(_uses_status(argument) for argument in tree[2])
    return any(_uses_status(part) for part in tree[1:] if isinstance(part, tuple))


def evaluate_expression(tree, context, references):
    """Evaluate a parsed expression, short-circuiting as GitHub does; unmodelled forms raise."""
    kind = tree[0]
    if kind == "or":
        left = evaluate_expression(tree[1], context, references)
        return left if _truthy(left) else evaluate_expression(tree[2], context, references)
    if kind == "and":
        left = evaluate_expression(tree[1], context, references)
        return evaluate_expression(tree[2], context, references) if _truthy(left) else left
    if kind == "not":
        return not _truthy(evaluate_expression(tree[1], context, references))
    if kind in ("eq", "ne"):
        # GitHub compares strings case-insensitively.
        same = (str(evaluate_expression(tree[1], context, references)).lower()
                == str(evaluate_expression(tree[2], context, references)).lower())
        return same if kind == "eq" else not same
    if kind == "literal":
        return tree[1]
    if kind == "call":
        if tree[1] in ("always", "success") and not tree[2]:
            return context["status"][tree[1]]
        arguments = [evaluate_expression(argument, context, references) for argument in tree[2]]
        if tree[1] == "contains" and len(arguments) == 2:
            # An array contains an equal item; a string contains a substring; both case-insensitively.
            needle = str(arguments[1]).lower()
            if isinstance(arguments[0], list):
                return any(str(item).lower() == needle for item in arguments[0])
            return needle in str(arguments[0]).lower()
        if tree[1] == "format" and arguments:
            result = str(arguments[0])
            for index, argument in enumerate(arguments[1:]):
                result = result.replace("{" + str(index) + "}", str(argument))
            return result
        raise UnsupportedExpression(f"{tree[1]}() is not modelled")
    if not any(pattern.fullmatch(tree[1]) for pattern in references):
        raise UnsupportedExpression(f"{tree[1]} is not modelled here")
    return _lookup(context, tree[1].split("."))


def _lookup(value, parts):
    """Follow a dotted path; `*` maps the rest of the path over a list; a missing key reads as ""."""
    for position, part in enumerate(parts):
        if part == "*":
            items = value if isinstance(value, list) else []
            return [_lookup(item, parts[position + 1:]) for item in items]
        value = value.get(part, "") if isinstance(value, dict) else ""
    return value


def condition_holds(text, context, default_status):
    """Evaluate an `if:`; one without a status function is `success() && (...)`, as GitHub reads it."""
    if text in (None, ""):
        return default_status
    text = text.strip()
    if text.startswith("${{") and text.endswith("}}"):
        text = text[3:-2]
    tree = _ExpressionParser(text).parse()
    if not _uses_status(tree):
        tree = ("and", ("call", "success", []), tree)
    status = {"always": True, "success": default_status}
    return _truthy(evaluate_expression(tree, dict(context, status=status), IF_REFERENCES))


def substitute(text, context):
    """Replace every `${{ }}` in a value with its evaluation."""
    def replace(match):
        value = evaluate_expression(_ExpressionParser(match.group(1)).parse(), context, VALUE_REFERENCES)
        if isinstance(value, bool):
            return "true" if value else "false"
        return str(value)
    return re.sub(r"\$\{\{(.*?)\}\}", replace, text, flags=re.S)


def read_key_values(path):
    """The `name=value` lines a step appended to GITHUB_OUTPUT or GITHUB_ENV."""
    values = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if "=" in line:
            name, value = line.split("=", 1)
            values[name] = value
    return values


# A fake command that records its argv under RUNNER_TEMP and succeeds.
RECORD_ARGV = '#!/usr/bin/env bash\nprintf \'%s\\n\' "$@" >"$RUNNER_TEMP/argv"\n'
# A fake command that must never run: reaching it fails the step.
UNEXPECTED_COMMAND = '#!/usr/bin/env bash\necho "unexpected $(basename "$0") $*" >&2\nexit 97\n'


# Windows skips every test that runs a workflow step through `run_bash_step`: a bare `bash` there can
# resolve to the WSL launcher rather than Git Bash, and the extensionless fake tools are not executable,
# so the step exits without output. macOS and Ubuntu CI run these tests.
def run_bash_step(script, environ, fakes=None):
    """Run a workflow `run:` block as GitHub does (`bash --noprofile --norc -eo pipefail`).

    `fakes` maps command names to scripts placed first on PATH. Returns the exit code, GITHUB_OUTPUT,
    GITHUB_ENV, stderr, the step summary and the argv a RECORD_ARGV fake saw.
    """
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        fake_bin = root / "bin"
        fake_bin.mkdir()
        for name, body in (fakes or {}).items():
            (fake_bin / name).write_text(body, encoding="utf-8")
            (fake_bin / name).chmod(0o755)
        script_path, outputs, env_file, summary = root / "step.sh", root / "outputs", root / "env", root / "summary"
        script_path.write_text(script, encoding="utf-8")
        for record in (outputs, env_file, summary):
            record.touch()
        full_environ = dict(os.environ, RUNNER_TEMP=str(root), GITHUB_OUTPUT=str(outputs), GITHUB_ENV=str(env_file),
                            GITHUB_STEP_SUMMARY=str(summary), PATH=f"{fake_bin}{os.pathsep}{os.environ['PATH']}")
        full_environ.update(environ)
        completed = subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", str(script_path)],
                                   env=full_environ, capture_output=True, text=True, timeout=30, check=False)
        argv_file = root / "argv"
        return SimpleNamespace(code=completed.returncode, outputs=read_key_values(outputs),
                               env=read_key_values(env_file), stderr=completed.stderr, root=root,
                               summary=summary.read_text(encoding="utf-8"),
                               argv=argv_file.read_text(encoding="utf-8").splitlines() if argv_file.exists() else [])


class PushSimulator:
    """Runs perf.yml's jobs for one event (a tag push by default) in file order; bash steps really run.

    Checkout, the toolchain and the cache restore succeed; an upload records its artifact name and a
    download fails unless that name was uploaded; pwsh steps succeed without running. Git is FAKE_GIT in
    `git_mode`; python, tar and shasum fail if any step reaches them, since a first release builds nothing.
    """

    RUNNER_OS = {"perf-build-macos": "macOS", "compare-macos": "macOS", "compare-windows": "Windows",
                 "perf-result": "Linux"}

    def __init__(self, workflow, root, git_mode, github=None):
        self.workflow, self.root, self.git_mode = workflow, root, git_mode
        self.fake_bin = root / "bin"
        self.fake_bin.mkdir()
        for name, body in (("git", FAKE_GIT), ("python3", UNEXPECTED_COMMAND), ("python", UNEXPECTED_COMMAND),
                           ("tar", UNEXPECTED_COMMAND), ("shasum", UNEXPECTED_COMMAND),
                           ("brew", "#!/usr/bin/env bash\nexit 0\n")):
            (self.fake_bin / name).write_text(body, encoding="utf-8")
            (self.fake_bin / name).chmod(0o755)
        self.github = github or {"event_name": "push", "run_id": "7", "run_attempt": "1", "sha": "tag-commit",
                                 "ref_name": "v1.4.0", "workspace": str(root / "workspace"), "event": {}}
        self.results, self.outputs, self.traces, self.artifacts, self.names = {}, {}, {}, {}, {}

    def run(self):
        """Run every job in file order; each job's needs have finished before it starts."""
        for job_id in self.workflow["jobs"]:
            self.run_job(job_id)
        return self

    def run_job(self, job_id):
        """Decide a job's `if:` from its needs' results, then run each matrix entry; any failed entry fails it."""
        job = self.workflow["jobs"][job_id]
        needs = job.get("needs", [])
        needs = [needs] if isinstance(needs, str) else needs
        context = {"github": self.github,
                   "needs": {name: {"result": self.results[name], "outputs": self.outputs[name]} for name in needs}}
        upstream_succeeded = all(self.results[name] == "success" for name in needs)
        # A matrix job's name reads its entry; every other job's name is the check's name, even when skipped.
        if "strategy" not in job:
            self.names[job_id] = substitute(job["name"], context)
        if not condition_holds(job.get("if"), context, upstream_succeeded):
            self.results[job_id], self.outputs[job_id], self.traces[job_id] = "skipped", {}, []
            return
        entries = (job.get("strategy") or {}).get("matrix", {}).get("include") or [{}]
        states, traces, outputs = [], [], {}
        for entry in entries:
            state, outputs, trace = self.run_entry(job_id, job, dict(context, matrix=entry))
            states.append(state)
            traces.append(trace)
        self.results[job_id] = "failure" if "failure" in states else "success"
        self.outputs[job_id], self.traces[job_id] = outputs, traces

    def run_entry(self, job_id, job, context):
        """Run one matrix entry's steps; return its result, its job outputs and (name, state, outputs) per step."""
        workspace = Path(tempfile.mkdtemp(dir=self.root))
        temp = workspace / "runner-temp"
        temp.mkdir()
        context = dict(context, runner={"os": self.RUNNER_OS[job_id], "temp": str(temp)})
        env = {}
        for scope in (self.workflow.get("env") or {}, job.get("env") or {}):
            env.update({name: substitute(value, context) for name, value in scope.items()})
        steps, trace, failed = {}, [], False
        for step in job["steps"]:
            name = step.get("name") or step["uses"].split("@")[0]
            step_context = dict(context, steps=steps)
            if not condition_holds(step.get("if"), step_context, not failed):
                trace.append((name, "skipped", {}))
                if "id" in step:
                    steps[step["id"]] = {"outputs": {}}
                continue
            succeeded, outputs = self.execute(step, step_context, env, workspace, temp)
            trace.append((name, "success" if succeeded else "failure", outputs))
            if "id" in step:
                steps[step["id"]] = {"outputs": outputs}
            failed = failed or not succeeded
        output_context = dict(context, steps=steps)
        outputs = {name: substitute(value, output_context) for name, value in (job.get("outputs") or {}).items()}
        return ("failure" if failed else "success"), outputs, trace

    def execute(self, step, context, env, workspace, temp):
        """Run one step; return whether it succeeded and its outputs."""
        if "uses" in step:
            action = step["uses"].split("@")[0]
            arguments = step.get("with") or {}
            if action == "actions/upload-artifact":
                self.artifacts[substitute(arguments["name"], context)] = True
                return True, {}
            if action == "actions/download-artifact":
                return substitute(arguments["name"], context) in self.artifacts, {}
            if action in ("actions/checkout", "dtolnay/rust-toolchain", "actions/cache/restore"):
                return True, {}
            raise AssertionError(f"the simulator does not model {action}")
        if step.get("shell") == "pwsh":
            return True, {}
        outputs, env_file, summary, script = temp / "output", temp / "env", temp / "summary", temp / "step.sh"
        for record in (outputs, env_file):
            record.write_text("", encoding="utf-8")
        summary.touch()
        script.write_text(substitute(step["run"], context), encoding="utf-8")
        environ = dict(env, **{name: substitute(value, context) for name, value in (step.get("env") or {}).items()})
        environ.update(PATH=f"{self.fake_bin}{os.pathsep}{os.environ['PATH']}", HOME=os.environ.get("HOME", ""),
                       GITHUB_OUTPUT=str(outputs), GITHUB_ENV=str(env_file), GITHUB_STEP_SUMMARY=str(summary),
                       RUNNER_TEMP=str(temp), RUNNER_OS=context["runner"]["os"], GITHUB_RUN_ID=self.github["run_id"],
                       GITHUB_RUN_ATTEMPT=self.github["run_attempt"], GITHUB_SHA=self.github["sha"],
                       FAKE_GIT_MODE=self.git_mode)
        completed = subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", str(script)], cwd=workspace,
                                   env=environ, capture_output=True, text=True, timeout=30, check=False)
        env.update(read_key_values(env_file))
        return completed.returncode == 0, read_key_values(outputs)


class WorkflowModelTests(unittest.TestCase):
    """The reader and evaluator the workflow tests rely on: they model only what perf.yml uses, and raise otherwise."""

    def test_the_reader_handles_the_forms_perf_yml_uses(self):
        # Nested mappings, a dash line that opens a mapping, flow lists, comments, `|` and `>-` blocks.
        document = WorkflowYaml.parse("on:\n  push:\n    tags: [\"v[0-9]*\"]  # tags\njobs:\n  one:\n"
                                      "    needs: [a, b]\n    if: >-\n      x ==\n      'y'\n    steps:\n"
                                      "      - name: Step\n        run: |\n          echo 1  # kept\n\n"
                                      "          echo 2\n      - uses: actions/checkout@abc # v1\n")
        self.assertEqual(document["on"], {"push": {"tags": ["v[0-9]*"]}})
        job = document["jobs"]["one"]
        self.assertEqual((job["needs"], job["if"]), (["a", "b"], "x == 'y'"))
        self.assertEqual(job["steps"], [{"name": "Step", "run": "echo 1  # kept\n\necho 2\n"},
                                        {"uses": "actions/checkout@abc"}])
        with self.assertRaises(ValueError):
            WorkflowYaml.parse("a: 1\n   b: 2\n")

    def test_the_evaluator_short_circuits_and_refuses_what_it_does_not_model(self):
        # A push makes the eligibility true without reading the pull request's labels; anything else raises.
        context = {"github": {"event_name": "push"}}
        self.assertTrue(condition_holds(ELIGIBILITY, context, True))
        self.assertFalse(condition_holds(ELIGIBILITY, context, False))
        self.assertTrue(condition_holds(f"always() && ({ELIGIBILITY})", context, False))
        labelled = {"github": pull_request_github("labeled", ["perf"], "perf")["github"]}
        self.assertTrue(condition_holds(ELIGIBILITY, labelled, True))
        with self.assertRaises(UnsupportedExpression):
            condition_holds("fromJSON('[]')", context, True)
        with self.assertRaises(UnsupportedExpression):
            condition_holds("needs.build.result == 'success'", context, True)
        with self.assertRaises(UnsupportedExpression):
            condition_holds("failure()", context, True)

    def test_the_whole_workflow_parses(self):
        # The four jobs, in the order their needs require.
        self.assertEqual(list(load_perf_workflow()["jobs"]), list(PERF_JOBS))


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash steps with fake tools")
class FirstReleaseWorkflowTests(unittest.TestCase):
    """A first release resolves no base: every job succeeds without building, transferring or comparing."""

    def simulate(self, workflow):
        with tempfile.TemporaryDirectory() as temp:
            return PushSimulator(workflow, Path(temp), "first").run()

    @staticmethod
    def states(simulator, job_id):
        """Each step's states across a job's matrix entries."""
        found = {}
        for trace in simulator.traces[job_id]:
            for name, state, _outputs in trace:
                found.setdefault(name, set()).add(state)
        return found

    def test_the_first_release_path_skips_every_build_transfer_and_comparison(self):
        # The producer resolves base='' and publishes no manifest; each macOS shard plans prebuilt=false; nothing
        # downloads or compares, and perf-result's real step passes on three successes.
        simulator = self.simulate(load_perf_workflow())
        self.assertEqual(simulator.results, {job: "success" for job in PERF_JOBS})
        producer_outputs = simulator.outputs["perf-build-macos"]
        self.assertEqual((producer_outputs["base"], producer_outputs["manifest_sha256"]), ("", ""))
        producer = self.states(simulator, "perf-build-macos")
        self.assertEqual(producer[REF_STEP], {"success"})
        for name in (BUILD_STEP, PACKAGE_STEP, UPLOAD_BINARIES_STEP, BUILD_EVIDENCE_STEP):
            self.assertEqual(producer[name], {"skipped"}, name)
        macos = self.states(simulator, "compare-macos")
        self.assertEqual((macos[REF_STEP], macos[PLAN_STEP]), ({"success"}, {"success"}))
        for name in (DOWNLOAD_STEP, UNPACK_STEP, COMPARE_STEP, SUMMARY_STEP, EVIDENCE_STEP):
            self.assertEqual(macos[name], {"skipped"}, name)
        plans = [outputs for trace in simulator.traces["compare-macos"]
                 for name, _state, outputs in trace if name == PLAN_STEP]
        self.assertEqual(plans, [{"prebuilt": "false"}] * len(simulator.traces["compare-macos"]))
        windows = self.states(simulator, "compare-windows")
        for name in (COMPARE_STEP, SUMMARY_STEP, EVIDENCE_STEP):
            self.assertEqual(windows[name], {"skipped"}, name)
        self.assertEqual(self.states(simulator, "perf-result")[RESULT_STEP], {"success"})
        self.assertEqual(simulator.artifacts, {})

    def test_without_the_download_guard_a_first_release_fails(self):
        # The mutation the simulator must catch: an unguarded download reaches an artifact nobody uploaded.
        workflow = copy.deepcopy(load_perf_workflow())
        del job_step(workflow, "compare-macos", DOWNLOAD_STEP)["if"]
        simulator = self.simulate(workflow)
        self.assertEqual(self.states(simulator, "compare-macos")[DOWNLOAD_STEP], {"failure"})
        self.assertEqual(simulator.results["compare-macos"], "failure")
        self.assertEqual(simulator.results["perf-result"], "failure")


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class PlanStepTests(unittest.TestCase):
    """compare-macos's gate: a shard measures the producer's binaries only for the refs it resolved itself."""

    def plan(self, producer_base, producer_head, manifest, base, head):
        """Run the plan step with its env evaluated from the producer's outputs and this shard's ref step."""
        step = job_step(load_perf_workflow(), "compare-macos", PLAN_STEP)
        context = {"needs": {"perf-build-macos": {"result": "success", "outputs": {
                       "base": producer_base, "head": producer_head, "manifest_sha256": manifest, "attempt": "1"}}},
                   "steps": {"refs": {"outputs": {"base": base, "head": head}}}, "github": {}}
        environ = {name: substitute(value, context) for name, value in step["env"].items()}
        return run_bash_step(step["run"], environ)

    def test_a_base_or_head_mismatch_fails(self):
        # A producer that resolved other refs (a moved merge base, say) built binaries this shard must not measure.
        for producer_base, producer_head in (("a" * 40, "c" * 40), ("b" * 40, "d" * 40), ("", "c" * 40)):
            with self.subTest(base=producer_base, head=producer_head):
                result = self.plan(producer_base, producer_head, "e" * 64, "b" * 40, "c" * 40)
                self.assertEqual(result.code, 1)
                self.assertIn("producer resolved", result.stderr)
                self.assertNotIn("prebuilt", result.outputs)

    def test_a_base_without_a_manifest_fails(self):
        # Agreeing refs with no manifest mean the producer skipped its build: nothing to measure.
        result = self.plan("b" * 40, "c" * 40, "", "b" * 40, "c" * 40)
        self.assertEqual(result.code, 1)
        self.assertIn("no manifest", result.stderr)

    def test_agreement_chooses_prebuilt(self):
        # Matching refs with a manifest measure the producer's binaries; a first release (no base) skips.
        self.assertEqual(self.plan("b" * 40, "c" * 40, "e" * 64, "b" * 40, "c" * 40).outputs, {"prebuilt": "true"})
        self.assertEqual(self.plan("", "", "", "", "").outputs, {"prebuilt": "false"})


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class PerfResultJobTests(unittest.TestCase):
    """The one stably named check: it passes only when the producer and both comparison jobs succeeded."""

    def job(self):
        return load_perf_workflow()["jobs"]["perf-result"]

    def test_the_job_is_one_inline_step_with_no_checkout(self):
        # Nothing from the pull request's tree runs here, so the head cannot change what the check accepts.
        job = self.job()
        self.assertEqual(" ".join(job["name"].split()),
                         f"${{{{ ({ELIGIBILITY}) && '{RESULT_NAME}' || '{RESULT_NOT_RUN}' }}}}")
        self.assertEqual(job["runs-on"], "ubuntu-latest")
        self.assertEqual(job["needs"], ["perf-build-macos", "compare-macos", "compare-windows"])
        self.assertEqual(" ".join(job["if"].split()), f"always() && ({ELIGIBILITY})")
        self.assertEqual(len(job["steps"]), 1)
        step = job["steps"][0]
        self.assertNotIn("uses", step)
        self.assertEqual(step["shell"], "bash")
        self.assertEqual(step["env"], {"PRODUCER": "${{ needs.perf-build-macos.result }}",
                                       "MACOS": "${{ needs.compare-macos.result }}",
                                       "WINDOWS": "${{ needs.compare-windows.result }}"})
        for forbidden in ("scripts/", ".github/", "checkout"):
            self.assertNotIn(forbidden, step["run"])

    def accepted(self, script):
        """The combinations of the three results the script exits 0 for."""
        accepted = []
        for producer in JOB_RESULTS:
            for macos in JOB_RESULTS:
                for windows in JOB_RESULTS:
                    result = run_bash_step(script, {"PRODUCER": producer, "MACOS": macos, "WINDOWS": windows})
                    if result.code == 0:
                        accepted.append((producer, macos, windows))
        return accepted

    def test_only_three_successes_pass(self):
        # All 125 combinations of success, failure, cancelled, skipped and empty; a `|| true` mutation passes more.
        script = self.job()["steps"][0]["run"]
        self.assertEqual(self.accepted(script), [("success", "success", "success")])
        mutated = script.replace('= "success" ]', '= "success" ] || true', 1)
        self.assertNotEqual(mutated, script)
        self.assertGreater(len(self.accepted(mutated)), 1)


class EligibilityTests(unittest.TestCase):
    """Every job carries the same eligibility, so another label's run skips them all and reports nothing."""

    def test_every_job_has_the_eligibility_verbatim(self):
        # compare-macos also needs the producer's success, implicitly: it must not run after a failed producer.
        jobs = load_perf_workflow()["jobs"]
        for job_id in ("perf-build-macos", "compare-macos", "compare-windows"):
            self.assertEqual(" ".join(jobs[job_id]["if"].split()), ELIGIBILITY, job_id)
        self.assertNotIn("always()", jobs["compare-macos"]["if"])
        self.assertEqual(jobs["compare-macos"]["needs"], "perf-build-macos")
        self.assertEqual(" ".join(jobs["perf-result"]["if"].split()), f"always() && ({ELIGIBILITY})")

    def test_no_job_keeps_its_own_concurrency_group(self):
        # The workflow-level group cancels a whole superseded eligible run, so per-job groups would be redundant.
        for job_id, job in load_perf_workflow()["jobs"].items():
            self.assertNotIn("concurrency", job, job_id)


def pull_request_github(action, labels, label=None, run_id="70"):
    """A pull_request event's context: its action, the labels the PR carries and the label just added."""
    event = {"action": action, "pull_request": {"number": 1575, "labels": [{"name": name} for name in labels]}}
    if label is not None:
        event["label"] = {"name": label}
    return {"github": {"event_name": "pull_request", "event": event, "run_id": run_id, "ref_name": "1575/merge",
                       "run_attempt": "1", "sha": "merge-commit", "workspace": "/w"}}


def push_github(run_id="71"):
    """A tag push's context."""
    return {"github": {"event_name": "push", "event": {}, "run_id": run_id, "ref_name": "v1.4.0",
                       "run_attempt": "1", "sha": "tag-commit", "workspace": "/w"}}


RESULT_NAME = "Performance comparison result"
RESULT_NOT_RUN = "Performance comparison result (not run)"
# (context, eligible): a tag push; perf added; a push while perf is set; another label on a perf PR; a PR
# without perf; perf added to a PR that also carries bug.
ELIGIBILITY_CASES = (
    (push_github(), True),
    (pull_request_github("labeled", ["perf"], "perf"), True),
    (pull_request_github("synchronize", ["perf", "bug"]), True),
    (pull_request_github("labeled", ["perf", "bug"], "bug"), False),
    (pull_request_github("opened", ["bug"]), False),
    (pull_request_github("synchronize", []), False),
)


class ResultIdentityTests(unittest.TestCase):
    """Only an eligible run publishes the real result name, and only eligible runs share a concurrency group."""

    def test_only_an_eligible_run_publishes_the_result_name(self):
        # An ineligible run's skipped result job reads "(not run)", so it can never hide an eligible run's check.
        workflow = load_perf_workflow()
        job = workflow["jobs"]["perf-result"]
        for context, eligible in ELIGIBILITY_CASES:
            with self.subTest(event=context["github"]["event"].get("action", "push"), eligible=eligible):
                self.assertEqual(substitute(job["name"], context), RESULT_NAME if eligible else RESULT_NOT_RUN)
                self.assertEqual(condition_holds(job["if"], context, False), eligible)

    def concurrency(self, context):
        """The workflow-level group and cancel-in-progress for one event."""
        block = load_perf_workflow()["concurrency"]
        return substitute(block["group"], context), substitute(block["cancel-in-progress"], context)

    def test_eligible_runs_share_a_group_and_ineligible_runs_cancel_nothing(self):
        # Two eligible runs of one PR share a cancelling group; each ineligible run has a group of its own.
        first = self.concurrency(pull_request_github("labeled", ["perf"], "perf", run_id="70"))
        newer = self.concurrency(pull_request_github("synchronize", ["perf"], run_id="80"))
        self.assertEqual(first, ("perf-comparison-1575", "true"))
        self.assertEqual(newer, first)
        ineligible = [self.concurrency(pull_request_github("labeled", ["perf", "bug"], "bug", run_id=str(run_id)))
                      for run_id in (81, 82)]
        self.assertEqual([group for group, _cancel in ineligible], ["perf-ineligible-81", "perf-ineligible-82"])
        self.assertNotIn(first[0], [group for group, _cancel in ineligible])
        self.assertEqual(self.concurrency(push_github()), ("perf-comparison-v1.4.0", "false"))

    def test_a_skipped_duplicate_run_is_all_skipped_under_its_own_name(self):
        # Another label on a perf PR starts a run whose every job skips; its result check reads "(not run)".
        with tempfile.TemporaryDirectory() as temp:
            context = pull_request_github("labeled", ["perf", "bug"], "bug")
            simulator = PushSimulator(load_perf_workflow(), Path(temp), "first", github=context["github"]).run()
        self.assertEqual(simulator.results, {job: "skipped" for job in PERF_JOBS})
        self.assertEqual(simulator.names["perf-result"], RESULT_NOT_RUN)
        self.assertEqual(self.concurrency(context)[0], "perf-ineligible-70")

    @unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the result job's bash step")
    def test_a_superseded_eligible_run_never_reports_success(self):
        # A newer eligible run cancels this one: its result job still runs (always()), under the real name, and fails.
        workflow = load_perf_workflow()
        job = workflow["jobs"]["perf-result"]
        context = pull_request_github("labeled", ["perf"], "perf")
        self.assertTrue(condition_holds(job["if"], context, False))
        self.assertEqual(substitute(job["name"], context), RESULT_NAME)
        for results in (("cancelled", "cancelled", "cancelled"), ("success", "cancelled", "success"),
                        ("success", "skipped", "cancelled")):
            with self.subTest(results=results):
                environ = dict(zip(("PRODUCER", "MACOS", "WINDOWS"), results))
                self.assertEqual(run_bash_step(job["steps"][0]["run"], environ).code, 1)


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class ProfileStepTests(unittest.TestCase):
    """The producer and both comparison jobs set the same release profile, which the manifest then binds."""

    def test_every_job_sets_the_same_profile(self):
        # Byte-for-byte the same step, writing the same GITHUB_ENV on a pull request and nothing on a push.
        workflow = load_perf_workflow()
        steps = [job_step(workflow, job_id, PROFILE_STEP) for job_id in ("perf-build-macos", *COMPARISON_JOBS)]
        self.assertEqual(steps[1:], steps[:1] * 2)
        for event, expected in (("pull_request", {"CARGO_PROFILE_RELEASE_LTO": "off",
                                                  "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16"}), ("push", None)):
            with self.subTest(event=event):
                holds = condition_holds(steps[0]["if"], {"github": {"event_name": event}}, True)
                self.assertEqual(holds, expected is not None)
                if expected:
                    self.assertEqual(run_bash_step(steps[0]["run"], {}).env, expected)


def comparison_context(job_id, run_length, attempt="2", manifest="d" * 64):
    """A pull request's context for a comparison step: the producer of `attempt` published `manifest`."""
    platform_name = "Windows" if job_id == "compare-windows" else "macOS"
    return {"github": {"event_name": "pull_request", "run_id": "7", "run_attempt": "3"},
            "matrix": {"platform": platform_name, "shard": "S2-S10sync", "scenarios": "S2 S10/sync"},
            "steps": {"refs": {"outputs": {"base": "b" * 40, "head": "c" * 40, "length": run_length}},
                      "plan": {"outputs": {"prebuilt": "true"}}},
            "needs": {"perf-build-macos": {"result": "success",
                                           "outputs": {"attempt": attempt, "manifest_sha256": manifest}}},
            "runner": {"os": platform_name, "temp": "/runner/temp"}}


def compare_argv(job_id, run_length):
    """Run a comparison job's compare step with fake pythons; return the argv perf-compare.py received."""
    step = job_step(load_perf_workflow(), job_id, COMPARE_STEP)
    context = comparison_context(job_id, run_length)
    environ = {name: substitute(value, context) for name, value in step["env"].items()}
    result = run_bash_step(step["run"], dict(environ, GITHUB_RUN_ID="7"),
                           fakes={"python3": RECORD_ARGV, "python": RECORD_ARGV})
    if result.code != 0:
        raise AssertionError(result.stderr)
    return [argument.replace(str(result.root), "$RUNNER_TEMP") for argument in result.argv]


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with a fake python3")
class CountersWorkflowTests(unittest.TestCase):
    """Each comparison job's step: which run and counters options each mode passes."""

    def test_a_pull_request_runs_two_counters_runs_and_a_release_its_full_count(self):
        # The PR budget allows two counters runs per scenario; a release takes --runs. Both are strict.
        for job_id in COMPARISON_JOBS:
            with self.subTest(job=job_id):
                pull_request = compare_argv(job_id, "short")
                self.assertIn("--short", pull_request)
                self.assertIn("--counters", pull_request)
                self.assertEqual(pull_request[pull_request.index("--counters-runs") + 1], "2")
                release = compare_argv(job_id, "full")
                self.assertIn("--counters", release)
                self.assertNotIn("--counters-runs", release)
                self.assertNotIn("--short", release)
                for argv in (pull_request, release):
                    self.assertIn("--require-base", argv)
                    self.assertEqual(argv[argv.index("--runs") + 1], "5")
                    self.assertEqual(argv[argv.index("--scenario") + 1:argv.index("--scenario") + 3],
                                     ["S2", "S10/sync"])


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class PrebuiltDownloadTests(unittest.TestCase):
    """compare-macos measures exactly the producer attempt's artifact, bound by run, attempt and digest."""

    def test_the_download_names_the_producers_attempt(self):
        # The consumer's name uses the producer's recorded attempt, so a failed-job rerun still finds attempt n.
        workflow = load_perf_workflow()
        context = comparison_context("compare-macos", "short", attempt="2")
        download = job_step(workflow, "compare-macos", DOWNLOAD_STEP)
        self.assertEqual(download["uses"], "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c")
        self.assertEqual(substitute(download["with"]["name"], context), "perf-binaries-macOS-7-2")
        upload = job_step(workflow, "perf-build-macos", UPLOAD_BINARIES_STEP)
        producer = {"github": {"run_id": "7", "run_attempt": "2"}, "runner": {"temp": "/t"}}
        self.assertEqual(substitute(upload["with"]["name"], producer), "perf-binaries-macOS-7-2")
        self.assertEqual((upload["with"]["retention-days"], upload["with"]["if-no-files-found"]), ("1", "error"))

    def test_every_artifact_step_is_gated_on_the_plan(self):
        # Download, unpack and compare run only when the plan chose prebuilt; summary and evidence also on failure.
        workflow = load_perf_workflow()
        for name in (DOWNLOAD_STEP, UNPACK_STEP, COMPARE_STEP):
            self.assertEqual(job_step(workflow, "compare-macos", name)["if"], "steps.plan.outputs.prebuilt == 'true'")
        for name in (SUMMARY_STEP, EVIDENCE_STEP):
            self.assertEqual(job_step(workflow, "compare-macos", name)["if"],
                             "always() && steps.plan.outputs.prebuilt == 'true'")

    def test_the_compare_step_binds_the_producer(self):
        # --require-base plus the prebuilt directory, this run's id, the producer's attempt and its manifest digest.
        argv = compare_argv("compare-macos", "short")
        self.assertIn("--require-base", argv)
        self.assertEqual(argv[argv.index("--prebuilt") + 1], "$RUNNER_TEMP/perf-binaries")
        self.assertEqual(argv[argv.index("--prebuilt-run-id") + 1], "7")
        self.assertEqual(argv[argv.index("--prebuilt-attempt") + 1], "2")
        self.assertEqual(argv[argv.index("--prebuilt-manifest-sha256") + 1], "d" * 64)
        self.assertNotIn("--prebuilt", compare_argv("compare-windows", "short"))


# The producer's fake perf-compare: it writes manifest.json under --build-only and prints a digest, the file's
# own unless FAKE_DIGEST says otherwise.
FAKE_PRODUCER = """#!/usr/bin/env bash
printf '%s\\n' "$@" >"$RUNNER_TEMP/argv"
while [ $# -gt 0 ]; do
  if [ "$1" = "--build-only" ]; then directory=$2; fi
  shift
done
mkdir -p "$directory"
printf '{"schema_version": 1}\\n' >"$directory/manifest.json"
if [ "$FAKE_DIGEST" = "wrong" ]; then
  echo "manifest_sha256=$(printf '0%.0s' $(seq 64))"
else
  echo "manifest_sha256=$(shasum -a 256 "$directory/manifest.json" | cut -d ' ' -f 1)"
fi
"""


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None or shutil.which("shasum") is None,
                 "runs the producer's bash step with fake tools")
class ProducerBuildStepTests(unittest.TestCase):
    """perf-build-macos publishes the digest of the manifest file it built, never just what the script printed."""

    def build(self, digest_mode):
        step = job_step(load_perf_workflow(), "perf-build-macos", BUILD_STEP)
        context = {"steps": {"refs": {"outputs": {"base": "b" * 40, "head": "c" * 40}}}, "github": {}}
        environ = {name: substitute(value, context) for name, value in step["env"].items()}
        return run_bash_step(step["run"], dict(environ, FAKE_DIGEST=digest_mode), fakes={"python3": FAKE_PRODUCER})

    def test_the_output_is_the_manifest_files_digest(self):
        # A strict build-only run of both refs, whose printed digest matches manifest.json.
        result = self.build("right")
        self.assertEqual(result.code, 0, result.stderr)
        self.assertRegex(result.outputs["manifest_sha256"], r"^[0-9a-f]{64}$")
        self.assertIn("--require-base", result.argv)
        self.assertEqual(result.argv[result.argv.index("--build-only") + 1], f"{result.root}/perf-binaries")

    def test_a_printed_digest_that_differs_from_the_file_fails(self):
        # The workflow recomputes the digest; a disagreement publishes nothing.
        result = self.build("wrong")
        self.assertNotEqual(result.code, 0)
        self.assertNotIn("manifest_sha256", result.outputs)


class EvidenceArtifactTests(unittest.TestCase):
    """The CI comparison's artifact is the only copy of its evidence once the runner is gone."""

    EVIDENCE_ROOT = "${{ runner.temp }}/perf-comparison"

    def upload_patterns(self, job_id):
        """Return a comparison job's evidence `path:` lines, each as (excluded, pattern relative to the evidence root)."""
        step = job_step(load_perf_workflow(), job_id, EVIDENCE_STEP)
        patterns = []
        for entry in step["with"]["path"].splitlines():
            entry = entry.strip()
            excluded = entry.startswith("!")
            entry = entry.lstrip("!")
            self.assertTrue(entry.startswith(self.EVIDENCE_ROOT), entry)
            patterns.append((excluded, entry[len(self.EVIDENCE_ROOT):].lstrip("/")))
        return patterns

    @staticmethod
    def matches(relative, pattern):
        """Match a path relative to the evidence root against an upload glob, `**` spanning directories."""
        if not pattern:
            return True
        expression = re.escape(pattern).replace(r"\*\*", "\0").replace(r"\*", "[^/]*").replace("\0", ".*")
        return re.fullmatch(f"{expression}(/.*)?", relative) is not None

    def test_every_evidence_name_carries_its_attempt(self):
        # A rerun's evidence never shares a name with the attempt before it.
        workflow = load_perf_workflow()
        for job_id, name in (("compare-macos", EVIDENCE_STEP), ("compare-windows", EVIDENCE_STEP),
                             ("perf-build-macos", BUILD_EVIDENCE_STEP)):
            with self.subTest(job=job_id):
                self.assertTrue(job_step(workflow, job_id, name)["with"]["name"].endswith("-${{ github.run_attempt }}"))

    def test_the_artifact_keeps_every_record_a_run_copies_from_its_scratch(self):
        # The workflow uploads the evidence tree; excluding a run's kept scratch would drop result.json,
        # progress.json, the App's logs and the checkpoint footprints, the raw data behind the CI-only table.
        with tempfile.TemporaryDirectory() as temp:
            scratch, evidence = Path(temp) / "scratch", Path(temp) / "evidence"
            for name in ("result.json", "progress.json", "harness.pid", "logs/sonicterm.log.2026-10-02",
                         "sessions/0.json", "acks/0", "checkpoints/0-end.json", "go/0", "workload/fixture.bin"):
                (scratch / name).parent.mkdir(parents=True, exist_ok=True)
                (scratch / name).write_text("x", encoding="utf-8")
            run = evidence / "runs" / "S1-default" / "timed" / "01-base"
            run.mkdir(parents=True)
            perf._keep_scratch(scratch, run / "scratch")
            for name in ("comparison.md", "timing.json", "runs/S1-default/timed/01-base/outcome.json",
                         "runs/S1-default/timed/01-base/01-harness.log"):
                (evidence / name).parent.mkdir(parents=True, exist_ok=True)
                (evidence / name).write_text("x", encoding="utf-8")
            for job_id in COMPARISON_JOBS:
                patterns = self.upload_patterns(job_id)
                archived = set()
                for file in evidence.rglob("*"):
                    if file.is_file():
                        relative = file.relative_to(evidence).as_posix()
                        included = any(self.matches(relative, pattern) for excluded, pattern in patterns
                                       if not excluded)
                        dropped = any(self.matches(relative, pattern) for excluded, pattern in patterns if excluded)
                        if included and not dropped:
                            archived.add(relative)
                kept = "runs/S1-default/timed/01-base/scratch/"
                for name in ("result.json", "progress.json", "logs/sonicterm.log.2026-10-02",
                             "checkpoints/0-end.json", "sessions/0.json"):
                    self.assertIn(kept + name, archived)
                for name in ("comparison.md", "timing.json", "runs/S1-default/timed/01-base/outcome.json",
                             "runs/S1-default/timed/01-base/01-harness.log"):
                    self.assertIn(name, archived)
                self.assertFalse(any("workload" in name for name in archived), archived)


FAKE_GIT = """#!/usr/bin/env bash
# Answers the release-ref step's Git calls for one mode: previous, first, root, broken or unreadable.
mode=$FAKE_GIT_MODE
case "$1" in
  rev-list)
    # `rev-list --parents -n 1 <sha>` prints the commit, then its parents.
    case "$mode" in
      root) echo tag-commit ;;
      unreadable) echo "fatal: unable to read commit object" >&2; exit 128 ;;
      *) echo "tag-commit parent-commit" ;;
    esac ;;
  rev-parse)
    if [[ "$*" == *"{commit}"* ]]; then echo base-commit; exit 0; fi
    if [ "$mode" = root ]; then exit 1; fi
    if [ "$mode" = unreadable ]; then echo "fatal: unable to read commit object" >&2; exit 128; fi
    echo parent-commit ;;
  tag)
    case "$mode" in
      previous) echo v1.3.8 ;;
      broken) echo "fatal: unable to read tree: object missing" >&2; exit 128 ;;
    esac ;;
  describe)
    if [ "$mode" = previous ]; then echo v1.3.8; else echo "fatal: No names found" >&2; exit 128; fi ;;
  *) echo "unexpected git $*" >&2; exit 2 ;;
esac
"""


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with a fake git")
class ReleaseRefSelectionTests(unittest.TestCase):
    """The release mode's ref step: only a real first release may skip the comparison."""

    def step_script(self):
        """Return the `run:` block of the producer's ref-selection step."""
        return job_step(load_perf_workflow(), "perf-build-macos", REF_STEP)["run"]

    def test_every_job_resolves_its_refs_with_the_same_step(self):
        # A shard recomputes the refs itself and compares them with the producer's, so the steps must agree.
        workflow = load_perf_workflow()
        steps = [job_step(workflow, job_id, REF_STEP) for job_id in ("perf-build-macos", *COMPARISON_JOBS)]
        self.assertEqual(steps[1:], steps[:1] * 2)
        self.assertEqual(steps[0]["id"], "refs")

    def run_step(self, mode):
        """Run the step as a tag push with a fake git; return its exit code, outputs and summary."""
        result = run_bash_step(self.step_script(), {"EVENT": "push", "GITHUB_SHA": "tag-commit",
                                                    "FAKE_GIT_MODE": mode}, fakes={"git": FAKE_GIT})
        outputs = "".join(f"{name}={value}\n" for name, value in result.outputs.items())
        return result.code, outputs, result.summary

    def test_a_release_with_an_earlier_tag_compares_against_it(self):
        # The previous release tag's commit is the base, the tag the head, at full length.
        code, outputs, _summary = self.run_step("previous")
        self.assertEqual(code, 0)
        self.assertEqual(outputs.split(), ["base=base-commit", "head=tag-commit", "length=full"])

    def test_only_a_real_first_release_skips_the_comparison(self):
        # An empty tag list, or a root commit, leaves `base` empty and says why in the summary.
        for mode, reason in (("first", "No earlier release tag"), ("root", "no parent")):
            with self.subTest(mode=mode):
                code, outputs, summary = self.run_step(mode)
                self.assertEqual((code, outputs.strip()), (0, "base="))
                self.assertIn(reason, summary)

    def test_a_failed_parent_lookup_fails_the_job(self):
        # An unreadable tagged commit is not a root commit: the step fails and writes no `base`.
        code, outputs, summary = self.run_step("unreadable")
        self.assertNotEqual(code, 0)
        self.assertNotIn("base=", outputs)
        self.assertNotIn("no parent", summary)

    def test_a_failed_tag_lookup_fails_the_job(self):
        # A Git error is not a first release: the step fails and writes no `base`, so nothing is skipped silently.
        code, outputs, summary = self.run_step("broken")
        self.assertNotEqual(code, 0)
        self.assertNotIn("base=", outputs)
        self.assertNotIn("No earlier release tag", summary)


class WindowsComparisonLegTests(unittest.TestCase):
    """perf.yml compares on Windows too: every scenario once per platform, on a runner with Cairo and Git Bash."""

    def matrix(self, job_id):
        return load_perf_workflow()["jobs"][job_id]["strategy"]["matrix"]["include"]

    def test_every_scenario_runs_once_per_platform(self):
        # Each platform's shards partition the common scenario sets plus its own S11 variants; Windows keeps five
        # shards.
        for job_id, platform_name, runner, counts in (("compare-macos", "macOS", "macos-14", (3, 4, 5)),
                                                       ("compare-windows", "Windows", "windows-latest", (5,))):
            with self.subTest(job=job_id):
                entries = self.matrix(job_id)
                self.assertIn(len(entries), counts)
                self.assertEqual({(entry["platform"], entry["runner"]) for entry in entries},
                                 {(platform_name, runner)})
                scenarios = [scenario for entry in entries for scenario in entry["scenarios"].split()]
                self.assertEqual(sorted(scenarios), sorted(ALL_SCENARIOS + PLATFORM_SCENARIOS[platform_name]))
                # The S11 variants join the existing S4-S5-S11 shard; no shard is renamed.
                shard = next(entry for entry in entries if entry["shard"] == "S4-S5-S11")
                platform_variants = PLATFORM_SCENARIOS[platform_name]
                self.assertEqual(shard["scenarios"].split()[:3], ["S4", "S5", "S11"])
                self.assertEqual(shard["scenarios"].split()[3:3 + len(platform_variants)], platform_variants)
                self.assertEqual(len({entry["shard"] for entry in entries}), len(entries))

    def test_the_atlas_retry_variant_runs_in_the_shard_its_projection_names(self):
        # The projection in perf.yml places S1/atlas-retry where measured slack absorbs it: macOS S4-S5-S11, since
        # macOS S1-S3-S6-S8-S12 was the zero-slack critical path, and Windows S1-S3-S6-S8-S12, since Windows
        # S4-S5-S11 was critical once.
        for job_id, shard_name in (("compare-macos", "S4-S5-S11"), ("compare-windows", "S1-S3-S6-S8-S12")):
            with self.subTest(job=job_id):
                holders = [entry["shard"] for entry in self.matrix(job_id)
                           if "S1/atlas-retry" in entry["scenarios"].split()]
                self.assertEqual(holders, [shard_name])

    def test_only_the_s9_s10_shards_run_s9_laps(self):
        # Every matrix entry carries a laps field: S9 on the S9-S10 shard of each platform, empty elsewhere; each
        # comparison step passes the laps flags only when the field is set, and no step sets timeout-minutes.
        for job_id in ("compare-macos", "compare-windows"):
            with self.subTest(job=job_id):
                entries = self.matrix(job_id)
                self.assertEqual({entry["shard"]: entry["laps"] for entry in entries if entry["laps"]},
                                 {"S9-S10": "S9"})
                self.assertTrue(all("laps" in entry for entry in entries))
                step = next(step for step in load_perf_workflow()["jobs"][job_id]["steps"]
                            if step.get("name") == "Compare the base and the head")
                self.assertEqual(step["env"]["LAPS"], "${{ matrix.laps }}")
                self.assertIn('if [ -n "$LAPS" ]; then\n  run_flags+=(--laps-scenario "$LAPS" --laps-runs 2)\nfi',
                              step["run"])
        self.assertNotIn("timeout-minutes", (Path(__file__).resolve().parent.parent / ".github" / "workflows"
                                             / "perf.yml").read_text(encoding="utf-8"))

    def test_windows_legs_get_cairo_bash_and_their_own_names(self):
        # Windows builds need Cairo from vcpkg, the shared step scripts need bash, and the two platforms'
        # shards must not share an artifact name.
        workflow = load_perf_workflow()
        windows = workflow["jobs"]["compare-windows"]
        cairo = job_step(workflow, "compare-windows", "Install Cairo for Windows")
        self.assertEqual((cairo["shell"], cairo["run"]), ("pwsh", ".\\scripts\\setup-windows-cairo.ps1"))
        self.assertEqual(windows["defaults"]["run"]["shell"], "bash")
        for job_id in COMPARISON_JOBS:
            job = workflow["jobs"][job_id]
            self.assertEqual(job["runs-on"], "${{ matrix.runner }}")
            self.assertIn("-${{ matrix.platform }}-${{ matrix.shard }}-",
                          job_step(workflow, job_id, EVIDENCE_STEP)["with"]["name"])
            self.assertEqual(job["strategy"]["fail-fast"], "false")


# The result.json frame-counter contract, written out here so a test fails when the script drifts from it:
# each section's integer counts, then its histograms (the suffix names the unit).
COUNTER_CONTRACT = {
    "window": (("attempts", "presented", "cached", "settled", "retry", "surface_retry", "stopped", "failed",
                "contention_parser", "contention_images", "defer_timeout", "defer_contention", "defer_sync",
                "defer_streaming", "stream_clock_exempt", "display_link_ticks", "display_link_admissions", "display_link_fallbacks",
                "contention_retry_armed", "dirt_ack_dropped", "native_request_redraw",
                "user_request_redraw", "redraw_requested"),
               ("present_interval_ms", "handler_ms", "flush_to_redraw_ms")),
    "app": (("wake_init", "wake_poll", "wake_wait_cancelled", "wake_resume_time", "wake_user", "ui_parser_locks",
             "fg_probe_calls", "fg_probe_panes", "fg_worker_probes", "fg_worker_panes", "fg_results_stale",
             "native_request_redraw_unregistered"),
            ("about_to_wait_ms", "user_event_ms", "new_events_ms", "ui_parser_wait_us", "fg_probe_us",
             "fg_worker_probe_us")),
    "vt": (("parse_bytes", "batches", "flushes", "flushes_untargeted", "flushes_coalesced",
            "flushes_suppressed", "sync_timeouts"),
           ("parser_lock_wait_us", "parser_lock_hold_us", "parse_us")),
    "renderer": (("vertex_bytes", "index_bytes", "damage_permille_sum", "damaged_frames",
                  "damage_waste_permille_sum", "software_frames",
                  "gpu_frames", "row_cache_hits", "row_cache_misses", "shape_requests", "full_frames",
                  "partial_frames", "partial_fallbacks", "row_cells_hashed",
                  # row_cache_invalidate_us is summed microseconds as a plain count, not a histogram.
                  "row_cache_invalidate_visits", "row_cache_invalidate_us", "recolor_glyphs_visited",
                  # font_fallback_applies is supporting evidence; a base older than the counter shows n/a.
                  "font_fallback_applies",
                  "shape_ns", "raster_ns", "raster_calls", "raster_tiles", "font_generation_applies",
                  "font_prepare_ns", "font_generation_prepare_ns", "render_attempts", "render_attempts_presented",
                  "render_attempt_ns", "render_attempt_shape_ns", "render_attempt_raster_ns",
                  "render_attempt_shape_requests", "render_attempt_raster_calls", "render_attempt_raster_tiles",
                  "apply_attempts", "apply_attempts_presented", "apply_attempt_ns", "apply_attempt_shape_ns",
                  "apply_attempt_raster_ns", "apply_attempt_shape_requests", "apply_attempt_raster_calls",
                  "apply_attempt_raster_tiles",
                  # Glyph atlas growths and growths no frame presented; a base older than them shows n/a.
                  "glyph_atlas_growths", "atlas_growth_abandoned",
                  # Title and chrome-run cache reuses and prepares; a base older than them shows n/a.
                  "tab_title_reuses", "tab_title_prepares", "chrome_run_reuses", "chrome_run_prepares"),
                 ("assembly_us", "atlas_growth_to_present_ms")),
}
CONTRACT_FIELD_COUNT = sum(len(counts) + len(histograms) for counts, histograms in COUNTER_CONTRACT.values())
MILLISECOND_BOUNDS = [4, 7, 9, 12, 17, 25, 34, 50, 100]
MICROSECOND_BOUNDS = [10, 50, 100, 500, 1000, 5000]


def frame_counters(values=None):
    """One phase's complete frame_counters object; `values` maps `section.field` to a count or (buckets, sum_us).

    Every histogram's sum is `sum_us`, an exact integer in microseconds whatever the histogram's unit.
    """
    values = values or {}
    sections = {}
    for section, (counts, histograms) in COUNTER_CONTRACT.items():
        body = {name: values.get(f"{section}.{name}", 0) for name in counts}
        for name in histograms:
            unit = name.rsplit("_", 1)[1]
            bounds = MILLISECOND_BOUNDS if unit == "ms" else MICROSECOND_BOUNDS
            buckets, total_us = values.get(f"{section}.{name}", ([0] * (len(bounds) + 1), 0))
            body[name] = {"unit": unit, "bounds": list(bounds), "counts": list(buckets), "sum_us": total_us}
        sections[section] = body
    return sections


def counters_result(values=None, **overrides):
    """A valid result of a run with the gate forced on, every phase carrying frame_counters(values)."""
    result = valid_result(**overrides)
    result["frame_counters"] = "on"
    for phase in result["phases"]:
        phase["frame_counters"] = frame_counters(values)
    return result


FEATURES_TABLE = "\n[features]\nperf-counters = []\n"
COUNTERS_MANIFEST = HEAD_MANIFEST + FEATURES_TABLE
BASE_COUNTERS_MANIFEST = BASE_MANIFEST + FEATURES_TABLE
# The logging crate's source, with and without the filtered init the counters harness calls.
LOGGING_LIB = "crates/sonicterm-logging/src/lib.rs"
LOGGING_WITH_FILTER = ("pub fn init_in(cfg: &LoggingConfig, dir: &Path) -> io::Result<LoggingGuard> {}\n"
                       "pub fn init_in_with_filter(dir: &Path, filter: &str) -> io::Result<LoggingGuard> {}\n")
LOGGING_WITHOUT_FILTER = "pub fn init_in(cfg: &LoggingConfig, dir: &Path) -> io::Result<LoggingGuard> {}\n"
# Stands for a key the test deletes instead of setting.
MISSING = object()


class FrameCounterSchemaTests(unittest.TestCase):
    def check(self, result, counters=True):
        return perf.validate_result(result, HARNESS_HASH, 0, counters=counters)

    def test_a_result_without_the_key_is_unsupported(self):
        # Older harnesses write no frame_counters, which reads as a binary built without the feature.
        self.assertEqual(perf.frame_counter_state(valid_result()), "unsupported")
        for state in ("off", "unsupported"):
            with self.subTest(state=state):
                self.assertEqual(self.check(valid_result(frame_counters=state), counters=False), [])
        self.assertEqual(self.check(counters_result()), [])

    def test_the_state_is_one_of_three_values(self):
        # A misspelled or null state is a schema failure, never read as a run without counters.
        for state in ("ON", "maybe", None, True, 1):
            with self.subTest(state=state):
                self.assertTrue(self.check(valid_result(frame_counters=state), counters=False))

    def test_with_the_gate_on_every_phase_carries_every_field_with_its_type(self):
        # A missing, partial or mistyped object would read as zeros, so each one is a schema failure.
        def broken(section, name, value):
            counters = frame_counters()
            if value is MISSING:
                del counters[section][name]
            else:
                counters[section][name] = value
            return counters

        histogram = frame_counters()["window"]["handler_ms"]
        cases = {
            "a phase without frame_counters": MISSING,
            "frame_counters that is not an object": [],
            "a missing section": {key: body for key, body in frame_counters().items() if key != "vt"},
            "a missing count": broken("window", "attempts", MISSING),
            "a missing full-plan frame count": broken("renderer", "full_frames", MISSING),
            "a missing row-cache invalidation visit count": broken("renderer", "row_cache_invalidate_visits", MISSING),
            "a missing row-cache invalidation time": broken("renderer", "row_cache_invalidate_us", MISSING),
            "a missing recolor visit count": broken("renderer", "recolor_glyphs_visited", MISSING),
            "a missing assembly histogram": broken("renderer", "assembly_us", MISSING),
            "a missing fallback-apply count": broken("renderer", "font_fallback_applies", MISSING),
            "a negative fallback-apply count": broken("renderer", "font_fallback_applies", -1),
            "a negative recolor visit count": broken("renderer", "recolor_glyphs_visited", -1),
            "a fractional invalidation visit count": broken("renderer", "row_cache_invalidate_visits", 0.5),
            # The invalidation time is a plain integer of microseconds, never a histogram object.
            "a histogram for the invalidation time": broken("renderer", "row_cache_invalidate_us",
                                                            frame_counters()["renderer"]["assembly_us"]),
            "a count for the assembly histogram": broken("renderer", "assembly_us", 12),
            "the assembly histogram in ms": broken("renderer", "assembly_us", dict(
                frame_counters()["renderer"]["assembly_us"], unit="ms", bounds=MILLISECOND_BOUNDS, counts=[0] * 10)),
            "a missing unregistered-window count": broken("app", "native_request_redraw_unregistered", MISSING),
            "a missing histogram": broken("app", "fg_worker_probe_us", MISSING),
            "a negative count": broken("window", "attempts", -1),
            "a fractional count": broken("window", "attempts", 1.5),
            "a boolean count": broken("vt", "batches", True),
            "a histogram for a count": broken("window", "attempts", histogram),
            "a count for a histogram": broken("window", "handler_ms", 3),
            "too few buckets": broken("window", "handler_ms", dict(histogram, counts=[0] * 9)),
            "a negative bucket": broken("window", "handler_ms", dict(histogram, counts=[-1] + [0] * 9)),
            "a fractional bucket": broken("window", "handler_ms", dict(histogram, counts=[0.5] + [0] * 9)),
            "the wrong unit": broken("window", "handler_ms", dict(histogram, unit="us")),
            "the other unit's bounds": broken("window", "handler_ms", dict(histogram, bounds=MICROSECOND_BOUNDS,
                                                                            counts=[0] * 7)),
            "a negative sum_us": broken("window", "handler_ms", dict(histogram, sum_us=-1)),
            "a missing sum_us": broken("window", "handler_ms", {key: item for key, item in histogram.items()
                                                                if key != "sum_us"}),
            "a string sum_us": broken("window", "handler_ms", dict(histogram, sum_us="0")),
            "a fractional sum_us": broken("window", "handler_ms", dict(histogram, sum_us=1.5)),
            "a boolean sum_us": broken("window", "handler_ms", dict(histogram, sum_us=True)),
            # The retired key: the contract replaced `sum` in the unit with `sum_us`.
            "the retired sum beside sum_us": broken("window", "handler_ms", dict(histogram, sum=0)),
            "the retired sum alone": broken("window", "handler_ms", {"sum": 0, **{
                key: item for key, item in histogram.items() if key != "sum_us"}}),
        }
        for case, counters in cases.items():
            with self.subTest(case):
                result = counters_result()
                if counters is MISSING:
                    del result["phases"][0]["frame_counters"]
                else:
                    result["phases"][0]["frame_counters"] = counters
                problems = self.check(result)
                self.assertTrue(any("frame_counters" in problem for problem in problems), problems)

    def test_only_a_run_with_the_gate_on_carries_phase_counters(self):
        # With the gate off, or without the feature, a phase object of counters is a contract break.
        for state in ("off", "unsupported", MISSING):
            with self.subTest(state=state):
                result = counters_result()
                if state is MISSING:
                    del result["frame_counters"]
                else:
                    result["frame_counters"] = state
                self.assertTrue(self.check(result, counters=False))

    def test_a_base_may_lack_a_newer_field_but_types_still_count(self):
        # An older base reports the contract it had: a missing field or section is not a failure there, but a
        # field it does report must have its type. The head's counters must be whole.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["renderer"]["full_frames"]
        self.assertEqual(perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        self.assertTrue(any("full_frames" in problem for problem in self.check(lacking)))
        no_section = counters_result()
        del no_section["phases"][0]["frame_counters"]["renderer"]
        # A base built before the four row-cache, recolor and assembly fields joined the contract lacks them all.
        older = counters_result()
        for name in ("row_cache_invalidate_visits", "row_cache_invalidate_us", "recolor_glyphs_visited", "assembly_us"):
            del older["phases"][0]["frame_counters"]["renderer"][name]
        self.assertEqual(perf.validate_result(older, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        self.assertTrue(self.check(older))
        self.assertEqual(perf.validate_result(no_section, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        # A base built before the fallback-apply count joined the contract lacks only that field: n/a, not a failure.
        before_fallback = counters_result()
        del before_fallback["phases"][0]["frame_counters"]["renderer"]["font_fallback_applies"]
        self.assertEqual(
            perf.validate_result(before_fallback, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        self.assertTrue(any("font_fallback_applies" in problem for problem in self.check(before_fallback)))
        mistyped = counters_result({"window.attempts": -1})
        self.assertTrue(perf.validate_result(mistyped, HARNESS_HASH, 0, counters=True, partial_counters=True))

    def test_partial_counters_skip_an_absent_key_but_never_a_present_null(self):
        # The partial rule is about keys an older contract never had. A key that is present with a null or
        # wrongly typed value is malformed, at the section and at the field level, on any side.
        def base_problems(edit):
            result = counters_result()
            edit(result["phases"][0]["frame_counters"])
            return perf.validate_result(result, HARNESS_HASH, 0, counters=True, partial_counters=True)

        self.assertEqual(base_problems(lambda counters: counters.pop("renderer")), [])
        self.assertEqual(base_problems(lambda counters: counters["renderer"].pop("full_frames")), [])
        cases = {"a null section": lambda counters: counters.update(renderer=None),
                 "a list section": lambda counters: counters.update(renderer=[]),
                 "a null count": lambda counters: counters["renderer"].update(full_frames=None),
                 "a null histogram": lambda counters: counters["renderer"].update(assembly_us=None)}
        for case, edit in cases.items():
            with self.subTest(case):
                problems = base_problems(edit)
                self.assertTrue(any("renderer" in problem for problem in problems), problems)

    def test_the_state_must_match_whether_the_run_passed_counters(self):
        # A counters run whose harness ignored the flag measured nothing; a plain run must not pay the gate's cost.
        for state in ("off", "unsupported"):
            with self.subTest(state=state):
                problems = self.check(valid_result(frame_counters=state), counters=True)
                self.assertTrue(any("--counters" in problem for problem in problems), problems)
        self.assertTrue(self.check(counters_result(), counters=False))
        # A run that ended before its first phase still reports the gate it forced on.
        early = counters_result(status="invalid", exit_code=3, phases=[], grid=None)
        self.assertEqual(perf.validate_result(early, HARNESS_HASH, 3, counters=True), [])


class CounterBuildTests(unittest.TestCase):
    def tree(self, manifest, logging_source):
        """A worktree with the app manifest and, unless None, the logging crate's lib.rs."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / perf.APP_MANIFEST).parent.mkdir(parents=True)
        (root / perf.APP_MANIFEST).write_text(manifest, encoding="utf-8")
        if logging_source is not None:
            (root / LOGGING_LIB).parent.mkdir(parents=True)
            (root / LOGGING_LIB).write_text(logging_source, encoding="utf-8")
        return root

    def test_support_needs_the_feature_and_the_filtered_logging_init(self):
        # The head's counters harness calls sonicterm_logging::init_in_with_filter, so a tree that declares the
        # feature without that function (4d19e855, d957b88f) cannot build the overlaid harness with it.
        self.assertTrue(perf.tree_supports_counters(self.tree(COUNTERS_MANIFEST, LOGGING_WITH_FILTER)))
        unsupported = {
            "the feature without the function": (COUNTERS_MANIFEST, LOGGING_WITHOUT_FILTER),
            "the function only in a comment": (COUNTERS_MANIFEST, "// pub fn init_in_with_filter(dir: &Path)\n"),
            "the feature without the logging crate": (COUNTERS_MANIFEST, None),
            "the function without the feature": (BASE_MANIFEST, LOGGING_WITH_FILTER),
            "neither": (BASE_MANIFEST, LOGGING_WITHOUT_FILTER),
        }
        for case, (manifest, logging_source) in unsupported.items():
            with self.subTest(case):
                self.assertFalse(perf.tree_supports_counters(self.tree(manifest, logging_source)))

    def test_a_tree_supports_counters_only_when_its_features_table_declares_the_key(self):
        # A comment, another table or a value naming the feature is not a declaration.
        declared = (FEATURES_TABLE,
                    '[features]\ndefault = []\nperf-counters = ["sonicterm-gpu/perf-counters"]  # the gate\n',
                    '[features]\n"perf-counters" = []\n', COUNTERS_MANIFEST)
        absent = (BASE_MANIFEST, HEAD_MANIFEST, "[features]\ndefault = []\n", "[features]\n# perf-counters = []\n",
                  "[package.metadata]\nperf-counters = []\n", "# [features]\n# perf-counters = []\n",
                  '[features]\ndefault = ["perf-counters"]\n')
        for manifest in declared:
            with self.subTest(manifest=manifest):
                self.assertTrue(perf.declares_perf_counters(manifest))
        for manifest in absent:
            with self.subTest(manifest=manifest):
                self.assertFalse(perf.declares_perf_counters(manifest))

    def test_the_build_adds_the_feature_only_when_asked(self):
        # The feature pair is appended to the locked build; the plain build is unchanged.
        plain = perf.build_argv("perf_scenarios", release=True)
        self.assertEqual(perf.build_argv("perf_scenarios", release=True, counters=False), plain)
        self.assertEqual(perf.build_argv("perf_scenarios", release=True, counters=True),
                         plain + ("--features", "perf-counters"))

    def test_only_a_counters_run_passes_counters_to_the_harness(self):
        # The harness forces the gate on only for a run started with --counters.
        argv = perf.harness_argv(Path("/b"), "S1", "default", HARNESS_HASH, Path("/s"), counters=True)
        self.assertIn("--counters", argv)
        self.assertNotIn("--counters", perf.harness_argv(Path("/b"), "S1", "default", HARNESS_HASH, Path("/s")))


class CounterCliTests(unittest.TestCase):
    def parse(self, *argv):
        with contextlib.redirect_stderr(io.StringIO()):
            return perf.parse_args(list(argv))

    def test_counters_and_their_run_count_parse(self):
        # --counters-runs is optional; without it the counters set takes --runs.
        args = self.parse("--base", "main", "--head", "HEAD", "--counters", "--counters-runs", "2")
        self.assertEqual((args.counters, args.counters_runs), (True, 2))
        args = self.parse("--base", "main", "--head", "HEAD")
        self.assertEqual((args.counters, args.counters_runs), (False, None))

    def test_the_smoke_and_a_bare_run_count_are_refused(self):
        # The smoke takes no comparison option; a counters run count without --counters has nothing to count.
        for argv in (("--smoke", "--counters"), ("--smoke", "--counters-runs", "2"),
                     ("--base", "main", "--head", "HEAD", "--counters-runs", "2"),
                     ("--base", "main", "--head", "HEAD", "--counters", "--counters-runs", "0")):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                self.parse(*argv)


def counters_side(*values_per_run):
    """The head's valid counters runs, one per values map."""
    return perf.SideRuns(outcomes=[make_outcome(result=counters_result(values)) for values in values_per_run])


class CounterTableTests(unittest.TestCase):
    def test_a_counters_run_whose_frames_disagree_with_its_presenter_is_noted(self):
        # result.json records the presenter on Windows and macOS: GDI frames count as software_frames, wgpu frames
        # as gpu_frames. A run whose counts contradict its record is named in a note, never passed silently; a
        # consistent run, or one that recorded no presenter (an older base), adds nothing.
        gdi = dict(WGPU_PRESENTER, software_render_degraded=True, windows_gdi=True)
        consistent = perf.SideRuns(outcomes=[
            make_outcome(result=counters_result({"renderer.software_frames": 40}, presenter=gdi)),
            make_outcome(result=counters_result({"renderer.gpu_frames": 40}, presenter=WGPU_PRESENTER)),
            make_outcome(result=counters_result({"renderer.gpu_frames": 40})),
            make_outcome(result=without_presenter(counters_result({"renderer.software_frames": 40})))])
        self.assertEqual(perf.presenter_counter_notes("S1/default", "head", consistent), [])
        wrong = perf.SideRuns(outcomes=[
            make_outcome(result=counters_result({"renderer.gpu_frames": 3, "renderer.software_frames": 37},
                                                presenter=gdi)),
            make_outcome(result=counters_result({"renderer.software_frames": 5}, presenter=WGPU_PRESENTER))])
        notes = perf.presenter_counter_notes("S1/default", "head", wrong)
        self.assertEqual(len(notes), 2, notes)
        self.assertIn("S1/default head run 1", notes[0])
        self.assertIn("GDI", notes[0])
        self.assertIn("3 gpu_frames", notes[0])
        self.assertIn("S1/default head run 2", notes[1])
        self.assertIn("5 software_frames", notes[1])

    def test_counts_are_medians_and_histograms_bucket_bounds(self):
        # Counts are medians across runs; a histogram's p95 and max are bucket bounds, overflow included, and
        # its mean is the pooled sum_us over the event count, in the histogram's unit (sum_us is always
        # microseconds). A field 0 in every run is left out and counted.
        side = counters_side(
            {"window.attempts": 4, "window.handler_ms": ([0, 0, 0, 5, 5, 0, 0, 0, 0, 0], 130_000)},
            {"window.attempts": 6, "app.wake_user": 3, "window.handler_ms": ([0, 0, 0, 4, 5, 0, 0, 0, 0, 1], 170_000),
             "vt.parse_us": ([0, 0, 0, 1, 0, 0, 1], 6050)})
        head_only = perf.SideRuns(blocked=perf.COUNTERS_HEAD_ONLY)
        rows, omitted = perf.counter_rows("S1/default", head_only, side)
        self.assertEqual(rows, [
            ["S1/default", "", "status", "n/a", "2 valid runs", ""],
            ["S1/default", "workload", "window.attempts (count)", "n/a", "5 (4–6)", "n/a"],
            ["S1/default", "workload", "window.handler_ms (ms)", "n/a",
             "p95 ≤17 ms, max >100 ms, mean 15.00 ms (20 events)", "n/a"],
            ["S1/default", "workload", "app.wake_user (count)", "n/a", "1.5 (0–3)", "n/a"],
            ["S1/default", "workload", "vt.parse_us (us)", "n/a",
             "p95 >5000 us, max >5000 us, mean 3025.00 us (2 events)", "n/a"],
            ["S1/default", "workload", "renderer attempt split (pooled)", "n/a", "no render attempts (2/2 runs)", ""]])
        self.assertEqual(omitted, CONTRACT_FIELD_COUNT - 4)

    def test_the_s11_release_reshow_phase_prints_full_frames_max_assembly_and_presented(self):
        # S11/release's re-show is read from the generic per-phase counters: its phase is named reshow, and the
        # table prints that phase's full frames, the max assembly time and the presented frames on both sides.
        def reshow_side(full_frames, presented, assembly):
            result = counters_result({"renderer.full_frames": full_frames, "window.presented": presented,
                                      "renderer.assembly_us": assembly})
            result["phases"][0]["name"] = "reshow"
            return perf.SideRuns(outcomes=[make_outcome(result=result)])
        base = reshow_side(2, 3, ([0, 0, 0, 2, 1, 0, 0], 2600))
        head = reshow_side(1, 3, ([0, 0, 0, 3, 0, 0, 0], 900))
        rows, _omitted = perf.counter_rows("S11/release", base, head)
        self.assertIn(["S11/release", "reshow", "renderer.full_frames (count)", "2 (2–2)", "1 (1–1)", "-50.0%"], rows)
        self.assertIn(["S11/release", "reshow", "window.presented (count)", "3 (3–3)", "3 (3–3)", "+0.0%"], rows)
        self.assertIn(["S11/release", "reshow", "renderer.assembly_us (us)",
                       "p95 ≤1000 us, max ≤1000 us, mean 866.67 us (3 events)",
                       "p95 ≤500 us, max ≤500 us, mean 300.00 us (3 events)", "-65.4%"], rows)

    def test_an_integer_microsecond_field_is_labelled_a_summed_duration(self):
        # An integer field named *_us holds summed microseconds, so it reads (us, summed), never (count); its change
        # still compares the medians. A plain count keeps (count).
        base = counters_side({"renderer.row_cache_invalidate_us": 1000, "renderer.row_cache_hits": 10})
        head = counters_side({"renderer.row_cache_invalidate_us": 1250, "renderer.row_cache_hits": 12})
        rows, _omitted = perf.counter_rows("S2/default", base, head)
        self.assertIn(["S2/default", "workload", "renderer.row_cache_invalidate_us (us, summed)", "1000 (1000–1000)",
                       "1250 (1250–1250)", "+25.0%"], rows)
        self.assertIn(["S2/default", "workload", "renderer.row_cache_hits (count)", "10 (10–10)", "12 (12–12)",
                       "+20.0%"], rows)

    def test_a_head_only_set_is_never_compared(self):
        # Without base counters every base and change cell is n/a; a head without valid runs has a status row only.
        self.assertIn("Change", perf.COUNTERS_HEADER)
        head_only = perf.SideRuns(blocked=perf.COUNTERS_HEAD_ONLY)
        rows, omitted = perf.counter_rows("S1/default", head_only, perf.SideRuns(failed="focus: theft"))
        self.assertEqual((rows, omitted), ([["S1/default", "", "status", "n/a", "failed: focus: theft", ""]], 0))

    def test_both_sides_compare_and_a_field_the_base_lacks_reads_n_a(self):
        # A count's change compares medians and a histogram's compares means; a newer field the base's contract
        # lacks reads n/a there, with no change.
        base_result = counters_result({"window.attempts": 4, "window.handler_ms": ([0, 0, 0, 2, 0, 0, 0, 0, 0, 0], 20_000)})
        del base_result["phases"][0]["frame_counters"]["renderer"]["full_frames"]
        base = perf.SideRuns(outcomes=[make_outcome(result=base_result)])
        head = counters_side({"window.attempts": 6, "renderer.full_frames": 3,
                              "window.handler_ms": ([0, 0, 0, 2, 0, 0, 0, 0, 0, 0], 24_000)})
        rows, omitted = perf.counter_rows("S1/default", base, head)
        self.assertEqual(rows, [
            ["S1/default", "", "status", "1 valid run", "1 valid run", ""],
            ["S1/default", "workload", "window.attempts (count)", "4 (4–4)", "6 (6–6)", "+50.0%"],
            ["S1/default", "workload", "window.handler_ms (ms)", "p95 ≤12 ms, max ≤12 ms, mean 10.00 ms (2 events)",
             "p95 ≤12 ms, max ≤12 ms, mean 12.00 ms (2 events)", "+20.0%"],
            ["S1/default", "workload", "renderer.full_frames (count)", "n/a", "3 (3–3)", "n/a"],
            # Both sides carry the attempt fields and drew none, which reads as one explicit row.
            ["S1/default", "workload", "renderer attempt split (pooled)", "no render attempts (1/1 runs)",
             "no render attempts (1/1 runs)", ""]])
        self.assertEqual(omitted, CONTRACT_FIELD_COUNT - 3)

    def test_foreground_worker_fields_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # The foreground worker's counters joined the app section: a head must report them, a base built before
        # them reads n/a, and a base that supports them and saw nothing prints a real 0.
        worker_fields = ("fg_worker_probes", "fg_worker_panes", "fg_results_stale", "fg_worker_probe_us")
        lacking = counters_result()
        for name in worker_fields:
            del lacking["phases"][0]["frame_counters"]["app"][name]
        self.assertEqual(perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        problems = perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)
        for name in worker_fields:
            with self.subTest(name=name):
                self.assertTrue(any(f"app.{name}" in problem for problem in problems), problems)
        head = counters_side({"app.fg_worker_probes": 2, "app.fg_worker_panes": 3})
        older_base = perf.SideRuns(outcomes=[make_outcome(result=lacking)])
        rows, _omitted = perf.counter_rows("S2/flood", older_base, head)
        cells = {row[2]: (row[3], row[4]) for row in rows[1:]}
        self.assertEqual(cells["app.fg_worker_probes (count)"], ("n/a", "2 (2–2)"))
        self.assertEqual(cells["app.fg_worker_panes (count)"], ("n/a", "3 (3–3)"))
        supported_zero = perf.SideRuns(outcomes=[make_outcome(result=counters_result())])
        rows, _omitted = perf.counter_rows("S2/flood", supported_zero, head)
        cells = {row[2]: (row[3], row[4]) for row in rows[1:]}
        self.assertEqual(cells["app.fg_worker_probes (count)"], ("0 (0–0)", "2 (2–2)"))

    def test_nanosecond_fields_are_labelled_and_shown_in_microseconds_without_rounding_the_figure(self):
        # A _ns field is summed nanoseconds: its cell shows microseconds with two decimals, while the change
        # compares the exact nanosecond medians, so 1,999 ns and 2,999 ns read 2.00 and 3.00 us and +50.0%.
        base = counters_side({"renderer.shape_ns": 1999})
        head = counters_side({"renderer.shape_ns": 2999})
        rows, _omitted = perf.counter_rows("S9/default", base, head)
        cells = {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}
        self.assertEqual(cells["renderer.shape_ns (us, summed from ns)"], ("2.00 (2.00–2.00)", "3.00 (3.00–3.00)",
                                                                          "+50.0%"))

    def test_the_attempt_split_pools_raw_totals_so_its_shares_add_up(self):
        # Three runs (attempt, shape, raster ns) = (100, 90, 0), (100, 0, 90), (1000, 500, 500): field-by-field
        # medians would give (100, 90, 90), a negative remainder. Pooling gives (1200, 590, 590): 49.2%, 49.2%
        # and 1.7% for the other 20 ns, with per-attempt means over the 3 pooled attempts.
        def run(attempt_ns, shape_ns, raster_ns):
            return {"renderer.apply_attempts": 1, "renderer.apply_attempt_ns": attempt_ns,
                    "renderer.apply_attempt_shape_ns": shape_ns, "renderer.apply_attempt_raster_ns": raster_ns,
                    "renderer.apply_attempt_shape_requests": 4, "renderer.apply_attempt_raster_calls": 2,
                    "renderer.render_attempts": 1, "renderer.render_attempt_ns": attempt_ns}
        head = counters_side(run(100, 90, 0), run(100, 0, 90), run(1000, 500, 500))
        text, attempts = perf.attempt_split(perf._counter_phases(head)["workload"], "apply_")
        self.assertEqual(attempts, 3)
        self.assertEqual(text, "3 attempts (3/3 runs): shaping 49.2%, rasterizing 49.2%, other 1.7%; per attempt "
                               "0.40 us, 4.0 shape requests, 2.0 raster calls, 0.0 tiles")
        # The two splits join the counters table after the phase's fields, once each.
        rows, _omitted = perf.counter_rows("S9/default", counters_side(), head)
        names = [row[2] for row in rows if row[2].startswith("renderer attempt split")]
        self.assertEqual(names, ["renderer attempt split: every attempt (pooled)",
                                 "renderer attempt split: fallback apply attempts (pooled)"])

    def test_the_attempt_split_reports_no_attempts_excludes_incomplete_runs_and_reads_n_a_on_an_older_base(self):
        # Zero apply attempts reads "no apply attempts", never 0%. A run missing any field of the class is left
        # out of every field of it, and the scope names how many runs counted. A base built before the counters
        # reads n/a for the split, and a phase where no side drew an attempt adds no split rows.
        quiet = counters_side({"renderer.render_attempts": 2, "renderer.render_attempt_ns": 50})
        phases = perf._counter_phases(quiet)["workload"]
        self.assertEqual(perf.attempt_split(phases, "apply_"), ("no apply attempts (1/1 runs)", 0))
        complete = counters_result({"renderer.apply_attempts": 1, "renderer.apply_attempt_ns": 10})
        lacking = counters_result({"renderer.apply_attempts": 5, "renderer.apply_attempt_ns": 999})
        del lacking["phases"][0]["frame_counters"]["renderer"]["apply_attempt_raster_tiles"]
        side = perf.SideRuns(outcomes=[make_outcome(result=complete), make_outcome(result=lacking)])
        text, attempts = perf.attempt_split(perf._counter_phases(side)["workload"], "apply_")
        self.assertEqual(attempts, 1)
        self.assertTrue(text.startswith("1 attempts (1/2 runs): "), text)
        older = counters_result()
        for name in list(older["phases"][0]["frame_counters"]["renderer"]):
            if "attempt" in name:
                del older["phases"][0]["frame_counters"]["renderer"][name]
        rows, _omitted = perf.counter_rows("S9/default", perf.SideRuns(outcomes=[make_outcome(result=older)]), quiet)
        split = {row[2]: (row[3], row[4]) for row in rows if row[2].startswith("renderer attempt split")}
        self.assertEqual(split["renderer attempt split: fallback apply attempts (pooled)"],
                         ("n/a", "no apply attempts (1/1 runs)"))
        # A phase where neither side drew an attempt reads as one explicit row, never omitted.
        rows, _omitted = perf.counter_rows("S9/default", counters_side({}), counters_side({}))
        split = [row for row in rows if row[2].startswith("renderer attempt split")]
        self.assertEqual([row[2:5] for row in split], [["renderer attempt split (pooled)",
                                                        "no render attempts (1/1 runs)",
                                                        "no render attempts (1/1 runs)"]])

    def test_the_harness_attempt_fixture_pools_to_exact_shares_and_lists_each_run(self):
        # The fixture is written by the harness from cumulative totals through its own delta and serializer
        # (counters_tests.rs). Run 1's (1999, 999, 999) -> (2999, 1499, 1499) ns arrives as (1000, 500, 500),
        # so pooling with run 2 gives (4000, 1500, 2000): 37.5%, 50.0% and 12.5%, never a negative remainder.
        fixture = json.loads(ATTEMPT_FIXTURE.read_text(encoding="utf-8"))
        def run(renderer):
            result = counters_result()
            result["phases"][0]["frame_counters"]["renderer"].update(renderer)
            return make_outcome(result=result)
        head = perf.SideRuns(outcomes=[run(entry["renderer"]) for entry in fixture["runs"]])
        text, attempts = perf.attempt_split(perf._counter_phases(head)["workload"], "apply_")
        self.assertEqual(attempts, 4)
        self.assertEqual(text, "4 attempts (2/2 runs): shaping 37.5%, rasterizing 50.0%, other 12.5%; per attempt "
                               "1.00 us, 2.0 shape requests, 1.5 raster calls, 1.0 tiles")
        rows, _omitted = perf.counter_rows("S9/default", counters_side(), head)
        cells = {row[2]: row[4] for row in rows[1:]}
        self.assertEqual(cells["renderer.apply_attempt_ns (us, summed from ns)"], "2.00 (1.00–3.00)")
        details = perf.attempt_split_details("S9/default", counters_side(), head)
        self.assertEqual(len(details), 2)
        self.assertTrue(details[0].startswith("- S9/default head run 1 workload: every attempt 1 attempts"), details)
        self.assertIn("shaping 50.0%, rasterizing 50.0%, other 0.0%", details[0])
        # A run of a phase with attempts is listed even when it applied nothing, so both pooled rows' whole
        # populations can be read run by run; a phase with only ordinary attempts is listed too.
        quiet = counters_result({"renderer.render_attempts": 2, "renderer.render_attempt_ns": 50})
        mixed = perf.SideRuns(outcomes=[*head.outcomes, make_outcome(result=quiet)])
        details = perf.attempt_split_details("S9/default", counters_side({}), mixed)
        self.assertEqual(len(details), 4, details)
        self.assertIn("head run 3 workload: every attempt 2 attempts (1/1 runs)", details[-1])
        self.assertIn("fallback apply attempts no apply attempts (1/1 runs)", details[-1])
        ordinary = perf.attempt_split_details("S9/default", counters_side({}), perf.SideRuns(
            outcomes=[make_outcome(result=quiet)]))
        self.assertEqual(len(ordinary), 2, ordinary)
        self.assertIn("head run 1 workload: every attempt 2 attempts", ordinary[-1])

    def test_fallback_applies_read_n_a_on_an_older_base_and_a_real_zero_on_a_supporting_one(self):
        # font_fallback_applies joined the renderer section: a base built before it reads n/a with no change, a
        # supporting base that applied nothing prints a real 0, and the head's value is compared against that 0.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["renderer"]["font_fallback_applies"]
        head = counters_side({"renderer.font_fallback_applies": 2})
        older_base = perf.SideRuns(outcomes=[make_outcome(result=lacking)])
        rows, _omitted = perf.counter_rows("S9/default", older_base, head)
        cells = {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}
        self.assertEqual(cells["renderer.font_fallback_applies (count)"], ("n/a", "2 (2–2)", "n/a"))
        supported_zero = perf.SideRuns(outcomes=[make_outcome(result=counters_result())])
        rows, _omitted = perf.counter_rows("S9/default", supported_zero, head)
        cells = {row[2]: (row[3], row[4]) for row in rows[1:]}
        self.assertEqual(cells["renderer.font_fallback_applies (count)"], ("0 (0–0)", "2 (2–2)"))

    def test_dropped_acknowledgements_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # dirt_ack_dropped joined the window section: a head must report it, a base built before it reads n/a
        # with no change shown, a supporting base that dropped nothing prints a real 0, and a gate-off run
        # carries no phase counters, so it is never checked for the field.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["window"]["dirt_ack_dropped"]
        problems = perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("window.dirt_ack_dropped" in problem for problem in problems), problems)
        self.assertEqual(perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"window.dirt_ack_dropped": 3})

        def dropped_cells(base):
            rows, _omitted = perf.counter_rows("S3/default", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["window.dirt_ack_dropped (count)"]

        self.assertEqual(dropped_cells(perf.SideRuns(outcomes=[make_outcome(result=lacking)])),
                         ("n/a", "3 (3–3)", "n/a"))
        self.assertEqual(dropped_cells(perf.SideRuns(outcomes=[make_outcome(result=counters_result())])),
                         ("0 (0–0)", "3 (3–3)", perf.percent_change(0, 3)))
        gate_off = valid_result(frame_counters="off")
        self.assertEqual(perf.validate_result(gate_off, HARNESS_HASH, 0, counters=False), [])

    def test_stream_clock_exemptions_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # stream_clock_exempt joined the window section: a head must report it, a base built before it reads n/a
        # with no change shown, a supporting base that exempted nothing prints a real 0, and a gate-off run
        # carries no phase counters, so it is never checked for the field.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["window"]["stream_clock_exempt"]
        problems = perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("window.stream_clock_exempt" in problem for problem in problems), problems)
        self.assertEqual(perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"window.stream_clock_exempt": 5})

        def exempt_cells(base):
            rows, _omitted = perf.counter_rows("S2/default", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["window.stream_clock_exempt (count)"]

        self.assertEqual(exempt_cells(perf.SideRuns(outcomes=[make_outcome(result=lacking)])),
                         ("n/a", "5 (5–5)", "n/a"))
        self.assertEqual(exempt_cells(perf.SideRuns(outcomes=[make_outcome(result=counters_result())])),
                         ("0 (0–0)", "5 (5–5)", perf.percent_change(0, 5)))
        gate_off = valid_result(frame_counters="off")
        self.assertEqual(perf.validate_result(gate_off, HARNESS_HASH, 0, counters=False), [])

    def test_display_link_counts_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # The three display-link counts joined the window section: a head must report each, a base built before
        # them reads n/a with no change shown, a supporting base whose link never ran prints a real 0, and a
        # gate-off run carries no phase counters, so it is never checked for them.
        for name in ("display_link_ticks", "display_link_admissions", "display_link_fallbacks"):
            with self.subTest(name=name):
                lacking = counters_result()
                del lacking["phases"][0]["frame_counters"]["window"][name]
                problems = perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)
                self.assertTrue(any(f"window.{name}" in problem for problem in problems), problems)
                self.assertEqual(
                    perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
                head = counters_side({f"window.{name}": 4})

                def link_cells(base):
                    rows, _omitted = perf.counter_rows("S4/default", base, head)
                    return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}[f"window.{name} (count)"]

                self.assertEqual(link_cells(perf.SideRuns(outcomes=[make_outcome(result=lacking)])),
                                 ("n/a", "4 (4–4)", "n/a"))
                self.assertEqual(link_cells(perf.SideRuns(outcomes=[make_outcome(result=counters_result())])),
                                 ("0 (0–0)", "4 (4–4)", perf.percent_change(0, 4)))
        gate_off = valid_result(frame_counters="off")
        self.assertEqual(perf.validate_result(gate_off, HARNESS_HASH, 0, counters=False), [])

    def test_damage_waste_is_required_on_the_head_and_n_a_on_an_older_base(self):
        # damage_waste_permille_sum joined the renderer section: a head must report it, a base built before it
        # reads n/a with no change shown, a supporting base that wasted nothing prints a real 0, and a gate-off
        # run carries no phase counters, so it is never checked for the field.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["renderer"]["damage_waste_permille_sum"]
        problems = perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("renderer.damage_waste_permille_sum" in problem for problem in problems), problems)
        self.assertEqual(perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"renderer.damage_waste_permille_sum": 40})

        def waste_cells(base):
            rows, _omitted = perf.counter_rows("S6/sweep", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["renderer.damage_waste_permille_sum (count)"]

        self.assertEqual(waste_cells(perf.SideRuns(outcomes=[make_outcome(result=lacking)])),
                         ("n/a", "40 (40–40)", "n/a"))
        self.assertEqual(waste_cells(perf.SideRuns(outcomes=[make_outcome(result=counters_result())])),
                         ("0 (0–0)", "40 (40–40)", perf.percent_change(0, 40)))
        gate_off = valid_result(frame_counters="off")
        self.assertEqual(perf.validate_result(gate_off, HARNESS_HASH, 0, counters=False), [])

    def test_partial_counters_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # partial_frames, partial_fallbacks and row_cells_hashed joined the renderer section: a head must
        # report each, a base built before them reads n/a with no change shown, a supporting base that
        # counted none prints a real 0, and a gate-off run carries no phase counters, so it is never checked.
        for name in ("partial_frames", "partial_fallbacks", "row_cells_hashed"):
            with self.subTest(name=name):
                lacking = counters_result()
                del lacking["phases"][0]["frame_counters"]["renderer"][name]
                problems = perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)
                self.assertTrue(any(f"renderer.{name}" in problem for problem in problems), problems)
                self.assertEqual(
                    perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
                head = counters_side({f"renderer.{name}": 12})

                def partial_cells(base):
                    rows, _omitted = perf.counter_rows("S2/typing", base, head)
                    return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}[f"renderer.{name} (count)"]

                self.assertEqual(partial_cells(perf.SideRuns(outcomes=[make_outcome(result=lacking)])),
                                 ("n/a", "12 (12–12)", "n/a"))
                self.assertEqual(partial_cells(perf.SideRuns(outcomes=[make_outcome(result=counters_result())])),
                                 ("0 (0–0)", "12 (12–12)", perf.percent_change(0, 12)))
        gate_off = valid_result(frame_counters="off")
        self.assertEqual(perf.validate_result(gate_off, HARNESS_HASH, 0, counters=False), [])

    def test_suppressed_flushes_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # flushes_suppressed joined the vt section: a head's typing phase must report it, a base built before it
        # reads n/a with no change shown, a supporting base that coalesced nothing prints a real 0, and a run
        # with the gate off carries no phase counters, so it is never checked for the field.
        typing = counters_result()
        typing["phases"][0]["name"] = "typing"
        del typing["phases"][0]["frame_counters"]["vt"]["flushes_suppressed"]
        problems = perf.validate_result(typing, HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("vt.flushes_suppressed" in problem for problem in problems), problems)
        self.assertEqual(perf.validate_result(typing, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        def typing_side(values):
            result = counters_result(values)
            result["phases"][0]["name"] = "typing"
            return perf.SideRuns(outcomes=[make_outcome(result=result)])

        def suppressed_row(base):
            rows, _omitted = perf.counter_rows("S5/default", base, head)
            return [row for row in rows[1:] if row[2] == "vt.flushes_suppressed (count)"]

        # Both sides measure the same typing phase, so only the field's presence can differ.
        head = typing_side({"vt.flushes": 9, "vt.flushes_suppressed": 4})
        older_base = perf.SideRuns(outcomes=[make_outcome(result=typing)])
        self.assertEqual(suppressed_row(older_base),
                         [["S5/default", "typing", "vt.flushes_suppressed (count)", "n/a", "4 (4–4)", "n/a"]])
        self.assertEqual(suppressed_row(typing_side({})),
                         [["S5/default", "typing", "vt.flushes_suppressed (count)", "0 (0–0)", "4 (4–4)",
                           perf.percent_change(0, 4)]])
        gate_off = valid_result(frame_counters="off")
        self.assertEqual(perf.validate_result(gate_off, HARNESS_HASH, 0, counters=False), [])

    def test_sync_deferrals_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # defer_sync joined the window section: a head must report it, a base built before it reads n/a with no
        # change shown, a supporting base that never held a frame prints a real 0, and a gate-off run carries no
        # phase counters, so it is never checked for the field.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["window"]["defer_sync"]
        problems = perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("window.defer_sync" in problem for problem in problems), problems)
        self.assertEqual(perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"window.defer_sync": 6})

        def held_cells(base):
            rows, _omitted = perf.counter_rows("S10/sync", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["window.defer_sync (count)"]

        self.assertEqual(held_cells(perf.SideRuns(outcomes=[make_outcome(result=lacking)])),
                         ("n/a", "6 (6–6)", "n/a"))
        self.assertEqual(held_cells(perf.SideRuns(outcomes=[make_outcome(result=counters_result())])),
                         ("0 (0–0)", "6 (6–6)", perf.percent_change(0, 6)))
        gate_off = valid_result(frame_counters="off")
        self.assertEqual(perf.validate_result(gate_off, HARNESS_HASH, 0, counters=False), [])

    def test_sync_timeouts_are_required_on_the_head_and_n_a_on_an_older_base(self):
        # sync_timeouts joined the vt section: a head must report it, a base built before it reads n/a with no
        # change shown, a supporting base whose updates all ended in time prints a real 0, and a gate-off run
        # carries no phase counters, so it is never checked for the field.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["vt"]["sync_timeouts"]
        problems = perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("vt.sync_timeouts" in problem for problem in problems), problems)
        self.assertEqual(perf.validate_result(lacking, HARNESS_HASH, 0, counters=True, partial_counters=True), [])
        head = counters_side({"vt.sync_timeouts": 2})

        def timeout_cells(base):
            rows, _omitted = perf.counter_rows("S10/sync", base, head)
            return {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}["vt.sync_timeouts (count)"]

        self.assertEqual(timeout_cells(perf.SideRuns(outcomes=[make_outcome(result=lacking)])),
                         ("n/a", "2 (2–2)", "n/a"))
        self.assertEqual(timeout_cells(perf.SideRuns(outcomes=[make_outcome(result=counters_result())])),
                         ("0 (0–0)", "2 (2–2)", perf.percent_change(0, 2)))
        gate_off = valid_result(frame_counters="off")
        self.assertEqual(perf.validate_result(gate_off, HARNESS_HASH, 0, counters=False), [])

    def test_event_loop_probe_work_falling_to_zero_shows_against_a_base_that_probed(self):
        # The event-loop probe fields stay in the contract with real zeros on a head that moved probing to the
        # worker, so a base that probed on the event loop shows that work falling to 0 beside the worker's rows.
        base = perf.SideRuns(outcomes=[make_outcome(result=counters_result(
            {"app.fg_probe_calls": 7, "app.fg_probe_panes": 7,
             "app.fg_probe_us": ([0, 5, 2, 0, 0, 0, 0], 700)}))])
        head = counters_side({"app.fg_worker_probes": 7, "app.fg_worker_panes": 7})
        rows, _omitted = perf.counter_rows("S2/default", base, head)
        cells = {row[2]: (row[3], row[4], row[5]) for row in rows[1:]}
        self.assertEqual(cells["app.fg_probe_calls (count)"], ("7 (7–7)", "0 (0–0)", perf.percent_change(7, 0)))
        self.assertEqual(cells["app.fg_probe_panes (count)"][:2], ("7 (7–7)", "0 (0–0)"))
        self.assertEqual(cells["app.fg_probe_us (us)"][1], "no events")
        self.assertEqual(cells["app.fg_worker_probes (count)"][:2], ("0 (0–0)", "7 (7–7)"))
        # A head that left the legacy fields out breaks the contract it still declares.
        lacking = counters_result()
        del lacking["phases"][0]["frame_counters"]["app"]["fg_probe_calls"]
        self.assertTrue(any("app.fg_probe_calls" in problem
                            for problem in perf.validate_result(lacking, HARNESS_HASH, 0, counters=True)))

    def test_overhead_covers_s2_and_s3_only(self):
        # Typing and the output flood are where the counters' own cost would show.
        self.assertEqual([perf.overhead_applies(label) for label in ("S2/default", "S2/flood", "S3/default",
                                                                     "S1/default", "S10/sync")],
                         [True, True, True, False, False])

    def test_the_document_carries_both_tables_after_the_timed_table(self):
        # Each table appears only when it has rows, with its note.
        rows = [["S2/default", "status", "1 valid run", "1 valid run", ""]]
        document = perf.comparison_document(rows, [], [], [], [], counter_rows=[["S2/default", "", "status", "n/a",
                                                                               "1 valid run", ""]],
                                            counters_note="Counters that were 0 in every run are left out: 49 here.",
                                            overhead_rows=rows)
        for expected in ("### Frame counters", perf.COUNTERS_HEADER, "left out: 49 here.", "### Counters overhead",
                         perf.OVERHEAD_HEADER, "counters-on vs counters-off on the head; sequential sets, not interleaved"):
            with self.subTest(expected=expected):
                self.assertIn(expected, document)
        self.assertLess(document.index(perf.TABLE_HEADER), document.index("### Frame counters"))
        plain = perf.comparison_document(rows, [], [], [], [])
        self.assertNotIn("Frame counters", plain)
        self.assertNotIn("Counters overhead", plain)


PROGRAM_PID = 950
# The record workload::program_session_json writes; the role is an integer, as serde_json prints it.
PROGRAM_TEXT = '{"program_pid": 950, "role": 0, "tty": "none"}'
# Windows creation times in this suite: raw FILETIME values, 100 ns since 1601.
FILETIME_EPOCH_OFFSET_S = 11_644_473_600


def filetime(unix_s):
    """The FILETIME value of a Unix time, as GetProcessTimes reports a creation time."""
    return int(round((unix_s + FILETIME_EPOCH_OFFSET_S) * 10_000_000))


def program_table():
    """A Windows-shaped table: the harness and its role program, with no sessions or process groups."""
    return FakeTable(harness_process(pgid=0, sid=0, command="perf_scenarios.exe", ppid=4),
                     FakeProcess(PROGRAM_PID, 0, 0, start="21", command="perf_scenarios.exe",
                                 start_unix_s=1001.5, ppid=HARNESS_PID))


def custody(active=0, cleanup="none", members=None, **overrides):
    """A windows-process-job custody record: verified unless an override says otherwise."""
    before = {"active_processes": active, "total_processes": max(active, 1)}
    if members is not None:
        before["members"] = {"count": len(members), "processes": members}
    record = {"before_cleanup": before, "after_cleanup": {"active_processes": 0, "total_processes": 1},
              "cleanup": cleanup, "empty": True, "bootstrap_reaped": True, "protocol_complete": True,
              "capture_complete": True, "errors": []}
    record.update(overrides)
    return record


def member(pid, image):
    """One listed job member as Job.members() records it."""
    return {"pid": pid, "image": image, "created": 130000000000}


class FakeKernel:
    """The kernel32 calls WindowsProcessTable makes, answered from Toolhelp rows and creation times."""

    def __init__(self, rows, created):
        self.rows, self.created = list(rows), dict(created)
        self.exited, self.denied, self.snapshot_fails = set(), set(), False
        self.handles, self.opened, self.closed, self.terminated = {}, [], [], []

    def snapshot(self):
        if self.snapshot_fails:
            raise OSError("CreateToolhelp32Snapshot failed")
        return list(self.rows)

    def open_process(self, access, pid):
        if pid in self.denied:
            raise PermissionError(f"OpenProcess({pid}) was denied")
        if pid not in self.created:
            return None
        handle = 1000 + len(self.opened)
        self.handles[handle] = pid
        self.opened.append((pid, access))
        return handle

    def creation_time(self, handle):
        return self.created[self.handles[handle]]

    def is_alive(self, handle):
        return self.handles[handle] not in self.exited

    def terminate(self, handle, exit_code):
        self.terminated.append((self.handles[handle], exit_code))
        return True

    def close(self, handle):
        self.closed.append(handle)


# Toolhelp rows: (pid, parent pid, image name).
KERNEL_ROWS = [(4, 0, "System"), (700, 4, "explorer.exe"), (HARNESS_PID, 700, "perf_scenarios.exe"),
               (PROGRAM_PID, HARNESS_PID, "perf_scenarios.exe")]
KERNEL_CREATED = {4: filetime(10.0), 700: filetime(500.0), HARNESS_PID: filetime(1000.5),
                  PROGRAM_PID: filetime(1001.5)}


class WindowsProcessTableTests(unittest.TestCase):
    def kernel(self):
        return FakeKernel(KERNEL_ROWS, KERNEL_CREATED)

    def test_toolhelp_rows_and_creation_times_become_process_records(self):
        # The image name is the command, the Toolhelp parent is ppid, and the raw creation time is identity.
        kernel = self.kernel()
        table = perf.WindowsProcessTable(kernel)
        self.assertEqual(table.pids(), [4, 700, HARNESS_PID, PROGRAM_PID])
        info = table.read(PROGRAM_PID)
        self.assertEqual((info.pid, info.ppid, info.command, info.pgid, info.sid),
                         (PROGRAM_PID, HARNESS_PID, "perf_scenarios.exe", 0, 0))
        self.assertEqual(info.start, str(filetime(1001.5)))
        self.assertAlmostEqual(info.start_unix_s, 1001.5, places=6)
        self.assertEqual(sorted(kernel.closed), sorted(kernel.handles))

    def test_gone_exited_and_unreadable_processes_read_apart(self):
        # A pid that is unlisted or has exited is gone; one the kernel refuses is unreadable, never gone.
        kernel = self.kernel()
        table = perf.WindowsProcessTable(kernel)
        self.assertIsNone(table.read(12345))
        kernel.exited.add(PROGRAM_PID)
        self.assertIsNone(table.read(PROGRAM_PID))
        kernel.denied.add(4)
        with self.assertRaises(perf.ProcessUnreadable):
            table.read(4)
        kernel.snapshot_fails = True
        self.assertIsNone(table.pids())
        with self.assertRaises(perf.ProcessUnreadable):
            table.read(HARNESS_PID)
        self.assertEqual(sorted(kernel.closed), sorted(kernel.handles))

    def test_terminate_rechecks_the_creation_time_so_a_reused_pid_is_spared(self):
        # The pid is reopened and its creation time compared before TerminateProcess, so a reused pid lives.
        kernel = self.kernel()
        table = perf.WindowsProcessTable(kernel)
        self.assertEqual(table.terminate(HARNESS_PID, str(filetime(999.0))), "stale")
        self.assertEqual(kernel.terminated, [])
        self.assertEqual(table.terminate(HARNESS_PID, str(filetime(1000.5))), "sent")
        self.assertEqual(kernel.terminated, [(HARNESS_PID, 124)])
        self.assertEqual(kernel.opened[-1][1], perf.PROCESS_TERMINATE | perf.PROCESS_QUERY_LIMITED_INFORMATION)
        self.assertEqual(table.terminate(12345, "1"), "gone")
        kernel.denied.add(4)
        self.assertEqual(table.terminate(4, str(filetime(10.0))), "refused")
        self.assertEqual(sorted(kernel.closed), sorted(kernel.handles))

    def test_windows_hosts_get_the_windows_table(self):
        # make_process_table serves _accept_harness_pid and other_instance_alive on every supported host.
        with mock.patch.object(perf.sys, "platform", "win32"):
            self.assertIsInstance(perf.make_process_table(), perf.WindowsProcessTable)


class ProgramRecordTests(unittest.TestCase):
    def test_a_record_names_its_role_and_a_positive_pid(self):
        # The record must name the role its file is named for, a positive pid, and a string tty.
        self.assertEqual(perf.parse_program_record(PROGRAM_TEXT, "0"), perf.ProgramRecord("0", PROGRAM_PID, "none"))
        for text in (PROGRAM_TEXT.replace('"role": 0', '"role": 1'), PROGRAM_TEXT.replace("950", "0"),
                     '{"role": 0, "program_pid": 950}', "[]"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                perf.parse_program_record(text, "0")

    def test_a_valid_program_is_acknowledged(self):
        # A live harness child running the harness image, created after launch, is stored with its creation time.
        record = perf.parse_program_record(PROGRAM_TEXT, "0")
        acked, problem = perf.validate_program(record, program_table(), HARNESS_PID, LAUNCH_UNIX_S,
                                               harness_command="PERF_SCENARIOS.EXE")
        self.assertEqual((acked, problem), (perf.AckedProgram("0", PROGRAM_PID, "21"), None))

    def test_a_wrong_parent_image_or_creation_time_is_refused(self):
        # Each check refuses a process that may not be this run's role program, naming what failed.
        record = perf.parse_program_record(PROGRAM_TEXT, "0")
        cases = {"parent": {"ppid": 4}, "image": {"command": "cmd.exe"}, "before": {"start_unix_s": 900.0}}
        for word, overrides in cases.items():
            with self.subTest(word):
                table = program_table()
                for name, value in overrides.items():
                    setattr(table.processes[PROGRAM_PID], name, value)
                acked, problem = perf.validate_program(record, table, HARNESS_PID, LAUNCH_UNIX_S,
                                                       harness_command="perf_scenarios.exe")
                self.assertIsNone(acked)
                self.assertIn(word, problem)
        table = program_table()
        table.processes[PROGRAM_PID].alive = False
        self.assertIn("not alive", perf.validate_program(record, table, HARNESS_PID, LAUNCH_UNIX_S,
                                                         harness_command="perf_scenarios.exe")[1])
        harness_record = perf.ProgramRecord("0", HARNESS_PID, "none")
        self.assertIsNone(perf.validate_program(harness_record, program_table(), HARNESS_PID, LAUNCH_UNIX_S,
                                                harness_command="perf_scenarios.exe")[0])


class CustodyCleanupTests(unittest.TestCase):
    def test_verified_custody_settles_the_run(self):
        # The gate's rule: the job emptied, the bootstrap reaped, protocol and capture complete, no errors.
        self.assertTrue(perf.custody_cleanup(custody()).passed)
        self.assertTrue(perf.custody_cleanup(custody(active=2, cleanup="terminated")).passed)

    def test_unverified_or_missing_custody_is_unresolved(self):
        # Without proof that the job ended every process, cleanup cannot clear the run.
        cases = {"not empty": {"empty": False}, "bootstrap not reaped": {"bootstrap_reaped": False},
                 "protocol incomplete": {"protocol_complete": False}, "capture incomplete": {"capture_complete": False},
                 "errors": {"errors": ["cleanup accounting: OSError"]}}
        for name, overrides in cases.items():
            with self.subTest(name):
                self.assertFalse(perf.custody_cleanup(custody(**overrides)).passed)
        self.assertFalse(perf.custody_cleanup(None).passed)

    def test_leftovers_are_zero_only_for_a_verified_deadline_kill(self):
        # The deadline kill expects members alive at its moment; any other count is members that outlived the run.
        alive = custody(active=3, cleanup="terminated")
        self.assertEqual(perf.windows_leftover_processes(alive, deadline_case=True), 0)
        self.assertEqual(perf.windows_leftover_processes(alive, deadline_case=False), 3)
        unverified = custody(active=3, cleanup="terminated", empty=False)
        self.assertEqual(perf.windows_leftover_processes(unverified, deadline_case=True), 3)
        self.assertEqual(perf.windows_leftover_processes(custody(), deadline_case=False), 0)
        self.assertIsNone(perf.windows_leftover_processes(custody(before_cleanup=None), deadline_case=False))
        self.assertIsNone(perf.windows_leftover_processes(None, deadline_case=True))

    def test_a_forced_job_end_is_fail_for_the_harness_step(self):
        # The harness step is synthetic and strict, so run_step reports FAIL, not CLEANED_NOT_NATURAL,
        # when it had to end the job.
        gate = perf.load_gate()
        raw = {"interrupted": False, "timed_out": False, "launch_failed": False, "errors": [], "exit_code": 0,
               "custody": custody(active=1, cleanup="terminated"), "natural": False}
        self.assertEqual(gate._phase_status(raw, gate.WindowsPolicy.STRICT), gate.FAIL)
        self.assertEqual(gate.Step("harness", ("x",), ("windows",), 1, "local", (), ()).windows_policy,
                         gate.WindowsPolicy.STRICT)


@unittest.skipUnless(os.name == "nt", "the real Windows process table exists only on Windows")
class LiveWindowsTests(unittest.TestCase):
    def test_reading_this_process_matches_the_kernel(self):
        # A read-only check of this interpreter proves the Toolhelp and GetProcessTimes bindings.
        table = perf.make_process_table()
        info = table.read(os.getpid())
        self.assertTrue(info.command.lower().startswith("python"), info.command)
        self.assertEqual((info.pid, info.ppid), (os.getpid(), os.getppid()))
        self.assertTrue(int(info.start) > 0)
        self.assertLess(abs(info.start_unix_s - perf.time.time()), 3600)
        self.assertIn(os.getpid(), table.pids())
        self.assertIsNone(table.read(2**31 - 4))

    def test_the_foreground_sampler_returns_a_reading(self):
        # The real user32 binding returns one classified reading in the shared record schema.
        reading = perf.sample_foreground()
        self.assertIn(reading.kind, ("app", "none", "failed"))
        self.assertEqual(reading.records[0].argv, ("GetForegroundWindow",))


# The reason the harness gives when a pane's program exits before the run finishes.
PANE_EXIT_REASON = "pane 2 exited (exit code 1) before the run finished"
# A hardware adapter as parse_adapter_line reads it, and a wgpu presenter that is not degraded.
HARDWARE_RENDERER = {"event": "selected", "backend": "Dx12", "name": "NVIDIA GeForce RTX 4070",
                     "driver": "32.0.15.6094", "device_type": "DiscreteGpu", "software_rendering": False}
WGPU_PRESENTER = {"software_render_mode": "auto", "software_rendering": False, "software_render_degraded": False,
                  "windows_gdi": False}


def macos_run(**presenter):
    """A factory for a valid macOS run, which logs no adapter; overrides change the presenter's fields."""
    def build(plan):
        result = valid_result(presenter=dict(WGPU_PRESENTER, **presenter))
        return make_outcome(plan=plan, platform="darwin", result=result)
    return build


def windows_run(renderer=None, grid=None, **presenter):
    """A factory for a valid Windows run on one adapter and presenter; overrides change the presenter's fields."""
    def build(plan):
        result = valid_result(grid=grid or {"cols": 250, "rows": 70}, presenter=dict(WGPU_PRESENTER, **presenter))
        return make_outcome(plan=plan, platform="win32", renderer=renderer or HARDWARE_RENDERER, result=result)
    return build


def foreground(hwnd, pid, error=None):
    """One classified foreground sample, as sample_foreground records it."""
    return perf.classify_foreground(hwnd, pid, error, unix_s=LAUNCH_UNIX_S)


class ForegroundTests(unittest.TestCase):
    def test_null_window_pid_zero_and_api_errors_classify_apart(self):
        # No foreground window is `none`; a window without a pid, or a failed call, is a failed sample.
        self.assertEqual(foreground(0, 0).kind, "none")
        self.assertEqual(foreground(0x1234, 0).kind, "failed")
        self.assertEqual(foreground(0x1234, 77, error="GetWindowThreadProcessId failed: 5").kind, "failed")
        reading = foreground(0x1234, 77)
        self.assertEqual((reading.kind, reading.pid), ("app", 77))

    def test_each_sample_keeps_the_front_samples_schema(self):
        # front-samples.log holds one record schema on every host, so a Windows sample is a CommandRecord too.
        record = foreground(0x1234, 77).records[0]
        self.assertEqual(record.argv, ("GetForegroundWindow",))
        self.assertEqual(set(record.as_json()), {"unix_s", "argv", "exit_code", "timed_out", "stdout", "stderr"})
        self.assertEqual((record.unix_s, record.exit_code), (LAUNCH_UNIX_S, 0))
        self.assertEqual(foreground(0x1234, 0, error="denied").records[0].exit_code, 1)
        with tempfile.TemporaryDirectory() as temporary:
            log_path = Path(temporary) / "front-samples.log"
            sampler = perf.FrontSampler(log_path, None, set(), sample=lambda: foreground(0x1234, 77))
            with contextlib.redirect_stdout(io.StringIO()):
                sampler.sample()
            self.assertEqual(json.loads(log_path.read_text())["argv"], ["GetForegroundWindow"])

    def test_another_app_taking_the_foreground_invalidates_a_desk_run(self):
        # At a desk any foreground change during the run may have moved the user's focus, so the run is invalid.
        verdict = perf.judge_foreground([foreground(1, 100), foreground(2, 200)], user_session=True)
        self.assertFalse(verdict.passed)
        self.assertEqual([(change["from_pid"], change["to_pid"]) for change in verdict.changes], [(100, 200)])

    def test_a_null_reading_between_the_same_app_is_bridged(self):
        # A moment with no foreground window is no change when the same application comes back.
        verdict = perf.judge_foreground([foreground(1, 100), foreground(0, 0), foreground(1, 100)],
                                        user_session=True)
        self.assertTrue(verdict.passed)
        self.assertEqual(verdict.changes, [])

    def test_the_harness_becoming_foreground_is_a_change(self):
        # Launch must not take the foreground, so the harness taking it counts like any other application.
        verdict = perf.judge_foreground([foreground(0, 0), foreground(1, 100), foreground(0, 0),
                                         foreground(3, HARNESS_PID)], user_session=True)
        self.assertFalse(verdict.passed)
        self.assertEqual([change["to_pid"] for change in verdict.changes], [HARNESS_PID])

    def test_without_a_user_session_a_change_is_only_noted(self):
        # A GitHub-hosted runner has no user focus to take, so a change is recorded, not failed.
        verdict = perf.judge_foreground([foreground(1, 100), foreground(2, 200)], user_session=False)
        self.assertTrue(verdict.passed)
        self.assertEqual(len(verdict.changes), 1)
        self.assertTrue(verdict.notes)
        failed = perf.judge_foreground([foreground(1, 100), foreground(2, 0)], user_session=False)
        self.assertFalse(failed.passed)


def checkpoint_outcome(platform_name, labels, **overrides):
    """A valid run on `platform_name` whose result has one checkpoint per label."""
    checkpoints = [{"index": index, "label": label, "unix_s": 70.0, "footprint_file": None}
                   for index, label in enumerate(labels)]
    result = valid_result(checkpoints=checkpoints, uncover_ms=overrides.pop("uncover_ms", None))
    return make_outcome(platform=platform_name, result=result, **overrides)


class WindowsTableTests(unittest.TestCase):
    def rows(self, label, outcome):
        """The table rows of one scenario whose sides each hold `outcome`, keyed by metric."""
        sides = [perf.SideRuns(outcomes=[outcome]) for _ in perf.SIDES]
        return {row[1]: row for row in perf.comparison_rows(label, *sides)}

    def test_s12_on_windows_has_n_a_occlusion_rows(self):
        # Windows reports no occlusion, so S12's uncover and covered-memory rows say n/a and why.
        rows = self.rows("S12/default", checkpoint_outcome("win32", ["settled", "covered", "end"]))
        for metric in ("uncover (ms)", "memory released while covered (MiB)"):
            with self.subTest(metric=metric):
                self.assertEqual(rows[metric][2:], ["n/a", "n/a", "Windows reports no occlusion"])
        self.assertNotIn("uncover (ms)", self.rows("S1/default", checkpoint_outcome("win32", ["end"])))

    def test_every_windows_checkpoint_has_an_n_a_footprint_row(self):
        # Windows has no footprint tool, so each checkpoint's footprint row says n/a and why.
        rows = self.rows("S12/default", checkpoint_outcome("win32", ["settled", "end"]))
        for label in ("settled", "end"):
            with self.subTest(label=label):
                self.assertEqual(rows[f"{label} footprint (MiB)"][2:], ["n/a", "n/a", "Windows has no `footprint`"])

    def test_a_macos_comparison_keeps_its_measured_rows(self):
        # macOS measures uncover and footprint, so its rows keep their figures and no Windows note appears; its
        # presenter row names wgpu, so a reader sees the run stayed on the hardware path.
        outcome = checkpoint_outcome("darwin", ["end"], uncover_ms=120.0,
                                     footprints={"1-end": {"bytes": 64 * perf.MIB}})
        rows = self.rows("S12/default", outcome)
        self.assertTrue(rows["uncover (ms)"][2].startswith("120.00"), rows["uncover (ms)"])
        self.assertTrue(rows["end footprint (MiB)"][2].startswith("64.00"), rows["end footprint (MiB)"])
        self.assertFalse(any("Windows" in cell for row in rows.values() for cell in row))
        self.assertEqual(rows["presenter"][2:4], ["wgpu", "wgpu"])

    def test_synthetic_occlusion_on_windows_is_a_schema_failure(self):
        # Windows reports no occlusion, so a synthetic one there means the harness did what it must not.
        result = valid_result(synthetic_occlusion=True)
        self.assertTrue(perf.validate_result(result, HARNESS_HASH, 0, platform_name="win32"))
        self.assertEqual(perf.validate_result(result, HARNESS_HASH, 0), [])

    def test_presenter_types_are_checked_when_present(self):
        # The presenter object is optional; when present each field has its type, and the schema stays 1.
        self.assertEqual(perf.validate_result(valid_result(presenter=WGPU_PRESENTER), HARNESS_HASH, 0), [])
        self.assertEqual(perf.validate_result(valid_result(presenter=dict(WGPU_PRESENTER, software_render_mode=None)),
                                              HARNESS_HASH, 0), [])
        for broken in ("gdi", dict(WGPU_PRESENTER, windows_gdi="no"), {"software_rendering": False}):
            with self.subTest(broken=broken):
                self.assertTrue(perf.validate_result(valid_result(presenter=broken), HARNESS_HASH, 0))

    def test_a_valid_windows_or_macos_result_must_carry_its_presenter(self):
        # Every Windows and macOS run records how it presented, so a valid win32 or darwin result without one is a
        # schema problem; a run that ended early need not have one.
        for platform in ("win32", "darwin"):
            with self.subTest(platform=platform):
                missing = without_presenter(valid_result())
                problems = perf.validate_result(missing, HARNESS_HASH, 0, platform_name=platform)
                self.assertTrue(any("presenter" in problem for problem in problems), problems)
                self.assertEqual(perf.validate_result(valid_result(presenter=WGPU_PRESENTER), HARNESS_HASH, 0,
                                                      platform_name=platform), [])
                invalid = without_presenter(valid_result(status="invalid", exit_code=3))
                self.assertEqual(perf.validate_result(invalid, HARNESS_HASH, 3, platform_name=platform), [])

    def test_a_macos_presenter_reads_wgpu_or_wgpu_degraded(self):
        # macOS never presents through GDI, so its row names wgpu and says when the degrade path was taken.
        for degraded, expected in ((False, "wgpu"), (True, "wgpu, degraded")):
            with self.subTest(degraded=degraded):
                result = valid_result(presenter=dict(WGPU_PRESENTER, software_render_degraded=degraded))
                self.assertEqual(perf.presenter_text(make_outcome(platform="darwin", result=result)), expected)

    def test_a_gdi_or_wgpu_run_without_its_presenter_record_is_blocked(self):
        # gdi and wgpu exist to measure one presenter, so a run that recorded none proves neither; the
        # default variant names no presenter and needs none.
        for variant in ("gdi", "wgpu"):
            with self.subTest(variant=variant):
                plan = perf.RunPlan(IDLE_SCENARIO, variant, "head", Path("/b"), HARNESS_HASH)
                reason = perf.presenter_blocked(make_outcome(plan=plan, result=without_presenter(valid_result())))
                self.assertIsNotNone(reason)
                self.assertIn("presenter", reason)
        self.assertIsNone(perf.presenter_blocked(make_outcome(result=without_presenter(valid_result()))))
        self.assertIsNone(perf.presenter_blocked(make_outcome()))


def delivery_record(**overrides):
    """A `delivery.json` record of one S10/sync replay whose every check passed."""
    record = {"schema_version": 2, "scenario": "S10", "variant": "sync", "bytes_kept": 4096,
              "checks": [{"name": "sync brackets", "ok": True, "detail": "enclosed 300, empty pair ahead 0, absent 0",
                          "unseen": 0, "brackets": 600, "unseen_markers": []}]}
    record.update(overrides)
    return record


def unseen_check(markers=("line 214 of 99999",), brackets=0, extra=0, **overrides):
    """A failed schema 2 `sync brackets` check whose frames `markers` (plus `extra` unlisted ones) never painted."""
    unseen = len(markers) + extra
    check = {"name": "sync brackets", "ok": False,
             "detail": f"enclosed 0, empty pair ahead 0, absent {300 - unseen}, never painted {unseen}",
             "unseen": unseen, "brackets": brackets, "unseen_markers": list(markers)}
    check.update(overrides)
    return check


def unseen_record(variant="default", **check_overrides):
    """A schema 2 S10 record whose only failed check is one retryable missing frame."""
    return delivery_record(variant=variant, checks=[unseen_check(**check_overrides)])


class DeliveryResultTests(unittest.TestCase):
    def write(self, directory, record):
        """Write `record` as the replay's `delivery.json` and return the scratch path."""
        path = Path(directory) / "delivery.json"
        path.write_text(record if isinstance(record, str) else json.dumps(record), encoding="utf-8")
        return Path(directory)

    def test_a_passed_replay_becomes_one_row_per_check(self):
        # Each check of the replay is a table row; both sides share one replay, so both cells hold its detail.
        with tempfile.TemporaryDirectory() as directory:
            record, problem = perf.read_delivery(self.write(directory, delivery_record()), "S10", "sync")
        self.assertIsNone(problem)
        rows = perf.delivery_rows("S10/sync", record, problem)
        self.assertEqual(rows, [["S10/sync", "delivery: sync brackets", "enclosed 300, empty pair ahead 0, absent 0",
                                 "enclosed 300, empty pair ahead 0, absent 0", perf.DELIVERY_NOTE]])

    def test_a_failed_check_blocks_the_scenario_and_names_it(self):
        # A failed check is the scenario's blocked reason, naming the check and its detail.
        failed = delivery_record(checks=[
            delivery_record()["checks"][0],
            {"name": "delivered lines", "ok": False, "detail": "240960 delivered, 240961 planned"}])
        with tempfile.TemporaryDirectory() as directory:
            record, problem = perf.read_delivery(self.write(directory, failed), "S10", "sync")
        self.assertEqual(problem, "delivery check failed: delivered lines: 240960 delivered, 240961 planned")
        rows = perf.delivery_rows("S10/sync", record, problem)
        self.assertEqual(rows[1][4], "blocked: " + problem)

    def test_a_missing_or_unreadable_record_blocks_the_scenario(self):
        # No file, broken JSON, another schema, another scenario and no checks each block with a reason.
        cases = {
            "missing": None,
            "broken": "{not json",
            "schema": delivery_record(schema_version=3),
            "scenario": delivery_record(scenario="S9"),
            "variant": delivery_record(variant="default"),
            "no checks": delivery_record(checks=[]),
            "check shape": delivery_record(checks=[{"name": "sync brackets", "ok": "yes", "detail": ""}]),
        }
        for name, content in cases.items():
            with self.subTest(case=name), tempfile.TemporaryDirectory() as directory:
                scratch = Path(directory) if content is None else self.write(directory, content)
                record, problem = perf.read_delivery(scratch, "S10", "sync")
                self.assertIsNone(record)
                self.assertTrue(problem and problem.startswith("delivery.json"), problem)
                self.assertEqual(perf.delivery_rows("S10/sync", record, problem),
                                 [["S10/sync", "delivery", "blocked", "blocked", "blocked: " + problem]])

    def test_a_malformed_schema_2_record_is_never_trusted(self):
        # A schema 2 record is admitted or retried only when every structured field is typed and in range and
        # the detail is the exact classification whose numbers agree with them; anything else blocks.
        broken_fields = delivery_record(checks=[dict(delivery_record()["checks"][0], unseen="0")])
        no_brackets = dict(delivery_record()["checks"][0])
        del no_brackets["brackets"]
        cases = {
            "a non-classification detail with a valid suffix": unseen_record(
                detail="not a frame classification, never painted 1"),
            "a passing record whose unseen count is a string": broken_fields,
            "a passing record with no bracket count": delivery_record(checks=[no_brackets]),
            "a passing record with a negative bracket count": delivery_record(checks=[
                dict(delivery_record()["checks"][0], brackets=-1)]),
            "a passing record whose markers are not a list": delivery_record(checks=[
                dict(delivery_record()["checks"][0], unseen_markers="none")]),
            "a passing verdict the counts contradict": unseen_record(ok=True),
            "a failing verdict the counts contradict": delivery_record(checks=[
                dict(delivery_record()["checks"][0], ok=False)]),
            "enclosed frames with no bracket": unseen_record(
                detail="enclosed 2, empty pair ahead 0, absent 297, never painted 1"),
            "no sync brackets check for S10": delivery_record(checks=[
                {"name": "completion", "ok": True, "detail": "arrived"}]),
            "schema 2.0": delivery_record(schema_version=2.0),
            "schema true": delivery_record(schema_version=True),
            "a bytes_kept that is not a count": delivery_record(bytes_kept="4096"),
        }
        for name, content in cases.items():
            with self.subTest(case=name), tempfile.TemporaryDirectory() as directory:
                variant = content["variant"]
                record, problem = perf.read_delivery(self.write(directory, content), "S10", variant)
                self.assertIsNone(record)
                self.assertTrue(problem and problem.startswith("delivery.json"), problem)

    def test_a_version_1_record_is_still_read(self):
        # A harness from before the structured frame fields writes schema 1; its checks still fill the table.
        legacy = delivery_record(schema_version=1, checks=[
            {"name": "sync brackets", "ok": True, "detail": "enclosed 300, empty pair ahead 0, absent 0"}])
        with tempfile.TemporaryDirectory() as directory:
            record, problem = perf.read_delivery(self.write(directory, legacy), "S10", "sync")
        self.assertIsNone(problem)
        self.assertEqual(record["schema_version"], 1)

    def test_the_replay_command_line_names_capture_delivery(self):
        # The replay is the harness's `--capture-delivery` mode, with the scenario, variant and run length.
        argv = perf.capture_delivery_argv(Path("C:/build/perf_scenarios.exe"), "S10", "sync",
                                          Path("C:/tmp/replay"), short=True)
        self.assertEqual(argv, (str(Path("C:/build/perf_scenarios.exe")), "--run", "S10", "--variant", "sync",
                                "--short", "--capture-delivery", str(Path("C:/tmp/replay"))))
        self.assertNotIn("--short", perf.capture_delivery_argv(Path("h"), "S3", "default", Path("s")))

    def test_only_windows_replays_the_delivered_scenarios(self):
        # S3, S9, S10 and S11 get a replay on Windows; no scenario does on macOS.
        for scenario_id in ("S3", "S9", "S10", "S11"):
            self.assertTrue(perf.delivery_replayed(scenario_id, "win32"))
            self.assertFalse(perf.delivery_replayed(scenario_id, "darwin"))
        self.assertFalse(perf.delivery_replayed("S1", "win32"))


VERIFIED = object()


class DeliveryReplayTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.temp_root, self.evidence = root / "temp", root / "evidence"
        self.temp_root.mkdir()
        self.evidence.mkdir()

    def tearDown(self):
        self.temporary.cleanup()

    def replay_attempts(self, attempts, variant="sync", platform_name="win32", evidence=None):
        """Run one S10 replay whose harness answers its Nth attempt from `attempts[N - 1]`.

        Each attempt is a dict: `record` (None writes nothing), `status`, `exit_code`, `custody` (verified unless
        given; None records none), `leftover`, and `text`, the delivered text the harness keeps beside its record.
        A replay that asks for more attempts than given fails the test.
        """
        pending = list(attempts)
        gate = FakeGate(None)

        def answer(step):
            if not pending:
                raise AssertionError(f"attempt {len(gate.steps)} was not expected")
            attempt = pending.pop(0)
            scratch = Path(step.argv[-1])
            scratch.mkdir(parents=True, exist_ok=True)
            if attempt.get("text") is not None:
                # Bytes, not write_text: Windows would turn each LF into CR LF, and the record's bytes_kept
                # counts the delivered bytes exactly.
                (scratch / "delivery.txt").write_bytes(attempt["text"].encode("utf-8"))
            if attempt.get("record") is not None:
                (scratch / "delivery.json").write_text(json.dumps(attempt["record"]), encoding="utf-8")
            # FakeGate reads custody and leftover after the handler answers, so each attempt sets its own.
            custody_record = attempt.get("custody", VERIFIED)
            gate.custody = custody() if custody_record is VERIFIED else custody_record
            gate.leftover = attempt.get("leftover", 0)
            return attempt.get("status", "PASS"), attempt.get("exit_code", 0), f"replay log {len(gate.steps)}\n"
        gate.handler = answer
        result = perf.run_delivery_replay(gate, Path("h.exe"), "S10", variant, evidence or self.evidence, 3,
                                          short=True, timeout_s=60, temp_root=self.temp_root,
                                          environ={"NO_COLOR": "1"}, platform_name=platform_name)
        return gate, result

    def replay(self, record, status="PASS", exit_code=0, custody_record=VERIFIED, platform_name="win32",
               leftover=0):
        """Run one S10/sync replay whose single attempt writes `record` (None writes nothing) and ends as given."""
        attempt = {"record": record, "status": status, "exit_code": exit_code, "custody": custody_record,
                   "leftover": leftover}
        return self.replay_attempts([attempt], platform_name=platform_name)

    @staticmethod
    def retryable(variant="default", **check_overrides):
        """An attempt that missed frame markers: exit 5, a schema 2 record and the delivered text it states."""
        text = "partial frames\n"
        record = unseen_record(variant, **check_overrides)
        record["bytes_kept"] = len(text.encode("utf-8"))
        return {"record": record, "status": "FAIL", "exit_code": 5, "text": text}

    @staticmethod
    def passed(variant="default"):
        """An attempt whose every check passed, with kept delivered text."""
        brackets = 600 if variant == "sync" else 0
        check = {"name": "sync brackets", "ok": True, "detail": "enclosed 0, empty pair ahead 0, absent 300",
                 "unseen": 0, "brackets": brackets, "unseen_markers": []}
        return {"record": delivery_record(variant=variant, checks=[check]), "status": "PASS", "exit_code": 0,
                "text": "every frame\n"}

    def test_a_replay_whose_job_custody_is_unverified_stops_the_comparison(self):
        # Processes the replay may have left would disturb every later run, as a measured run's would,
        # so an unverified or missing custody stops the lifecycle instead of blocking one scenario.
        for custody_record in (custody(empty=False, errors=["3 members alive"]), None):
            with self.subTest(custody=custody_record),                     self.assertRaisesRegex(perf.StopComparison, "delivery replay S10/sync: unresolved cleanup"):
                self.replay(delivery_record(), custody_record=custody_record)
        self.assertFalse(any(self.temp_root.iterdir()))

    def test_a_timed_out_replay_with_verified_custody_only_blocks_its_scenario(self):
        # A deadline whose job was emptied is the scenario's problem, not the comparison's.
        _gate, (record, problem, _note) = self.replay(None, "TIMEOUT", 124)
        self.assertIsNone(record)
        self.assertIn("TIMEOUT, exit 124", problem)

    def test_a_posix_replay_that_left_processes_stops_the_comparison(self):
        # Without a job, the gate's leftover count proves teardown; an unknown count proves nothing.
        for leftover in (2, None):
            with self.subTest(leftover=leftover),                     self.assertRaisesRegex(perf.StopComparison, "delivery replay S10/sync"):
                self.replay(delivery_record(), platform_name="darwin", leftover=leftover)
        _gate, (_record, problem, _note) = self.replay(delivery_record(), platform_name="darwin")
        self.assertIsNone(problem)

    def test_the_smoke_fails_at_once_when_its_replay_stops(self):
        # A smoke replay with unproven teardown fails the smoke before any case runs.
        scenarios = dict(SmokeTests.SCENARIOS, S1=perf.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 300, 80),
                         S10=perf.Scenario("S10", ("default", "sync"), "Redraw", 300, 240))
        cases = []

        def replay(scenario, variant, evidence):
            raise perf.StopComparison("delivery replay S10/sync: unresolved cleanup: job not empty")
        with contextlib.redirect_stdout(io.StringIO()):
            code, reasons = perf.smoke_cases(scenarios, Path("/b"), HARNESS_HASH,
                                             lambda plan, evidence: cases.append(plan), self.evidence,
                                             host_platform="win32", replay=replay)
        self.assertEqual(code, perf.EXIT_FAIL)
        self.assertEqual(reasons, ["delivery replay S10/sync: unresolved cleanup: job not empty"])
        self.assertEqual(cases, [])

    def test_a_passed_replay_keeps_its_record_as_evidence_and_removes_its_scratch(self):
        # The replay runs `--capture-delivery` in a fresh scratch under the temp root, without NO_COLOR;
        # its record is copied into the evidence and the scratch is removed.
        gate, (record, problem, note) = self.replay(delivery_record())
        self.assertIsNone(problem)
        self.assertEqual(record["scenario"], "S10")
        # Even a first-attempt pass states how many attempts it took.
        self.assertEqual(note, perf.DELIVERY_NOTE + "; passed on attempt 1 of 3")
        argv = gate.steps[0].argv
        self.assertEqual(argv[:-1], ("h.exe", "--run", "S10", "--variant", "sync", "--short", "--capture-delivery"))
        self.assertEqual(Path(argv[-1]).parent, self.temp_root)
        self.assertFalse(Path(argv[-1]).exists())
        self.assertEqual(gate.steps[0].timeout_s, 60)
        self.assertNotIn("NO_COLOR", gate.environs[0])
        self.assertTrue((self.evidence / "delivery-S10-sync-attempt1.json").exists())
        self.assertEqual(len(gate.steps), 1)

    def test_a_replay_with_no_record_names_its_step_status(self):
        # A harness that wrote nothing blocks the scenario with the step's status and exit code.
        _gate, (record, problem, _note) = self.replay(None, "FAIL", 1)
        self.assertIsNone(record)
        self.assertIn("FAIL", problem)
        self.assertIn("exit 1", problem)

    def test_a_failed_check_keeps_the_record_and_its_reason(self):
        # A failed check exits as blocked (5); the record stays so the table shows every check.
        failed = delivery_record(schema_version=1,
                                 checks=[{"name": "sync brackets", "ok": False, "detail": "absent 300"}])
        _gate, (record, problem, _note) = self.replay(failed, "FAIL", 5)
        self.assertIsNotNone(record)
        self.assertEqual(problem, "delivery check failed: sync brackets: absent 300")

    def test_a_record_that_disagrees_with_the_exit_blocks(self):
        # A passing record from a harness that did not exit 0 is not trusted.
        _gate, (_record, problem, _note) = self.replay(delivery_record(), "FAIL", 5)
        self.assertIn("passed every check, but the replay ended FAIL, exit 5", problem)

    def test_blocked_sets_block_both_sides_of_every_set(self):
        # A scenario whose delivery is blocked runs no set; each set reports both sides blocked.
        results = perf.blocked_set_results("S10/sync", "delivery check failed: x", ("timed", "laps"))
        self.assertEqual([result.set_name for result in results], ["timed", "laps"])
        for result in results:
            self.assertEqual((result.base.blocked, result.head.blocked),
                             ("delivery check failed: x", "delivery check failed: x"))
        self.assertEqual(perf.comparison_exit(results), perf.EXIT_BLOCKED)

    def test_the_windows_smoke_replays_s10_sync_and_a_failed_replay_blocks_it(self):
        # On Windows the smoke replays S10/sync before its runs; a blocked replay makes it exit 3, and
        # macOS never replays.
        scenarios = dict(SmokeTests.SCENARIOS, S1=perf.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 300, 80),
                         S10=perf.Scenario("S10", ("default", "sync"), "Redraw", 300, 240))
        replays = []

        def replay(scenario, variant, evidence):
            replays.append((scenario.id, variant))
            return perf.DeliveryOutcome(None, "delivery.json is missing", None)

        def run_case(plan, evidence):
            kind = "invalid" if plan.variant == "role-exit" else "valid"
            return outcome_of(kind)(plan)
        for platform_name, expected_replays, expected_code in (("win32", [("S10", "sync")], perf.EXIT_BLOCKED),
                                                               ("darwin", [], perf.EXIT_PASS)):
            replays.clear()
            with self.subTest(platform=platform_name), contextlib.redirect_stdout(io.StringIO()), \
                    mock.patch.object(perf, "case_verdict", lambda case, kind, reasons: (
                        "pass" if kind == "valid" or case.expected == kind else "fail", list(reasons))):
                code, reasons = perf.smoke_cases(scenarios, Path("/b"), HARNESS_HASH, run_case, self.evidence,
                                                 host_platform=platform_name, replay=replay)
            self.assertEqual(replays, expected_replays)
            self.assertEqual(code, expected_code)
            if expected_code == perf.EXIT_BLOCKED:
                self.assertIn("S10/sync delivery: not exercised: delivery.json is missing", reasons)


    def test_one_missing_frame_is_retried_and_a_passing_attempt_admits_the_scenario(self):
        # A replay that never found one frame marker (exit 5, schema 2, no bracket in `default`) is retried in a
        # fresh scratch; the passing second attempt admits S10, and the note discloses attempt 1 and its marker.
        gate, outcome = self.replay_attempts([self.retryable(), self.passed()], variant="default")
        self.assertIsNone(outcome.problem)
        self.assertTrue(outcome.record["checks"][0]["ok"])
        self.assertEqual(outcome.note, "untimed ConPTY replay by the head build, shared by both sides; passed on "
                                       "attempt 2 of 3; attempt 1: enclosed 0, empty pair ahead 0, absent 299, "
                                       "never painted 1 (marker `line 214 of 99999`)")
        self.assertEqual(perf.delivery_rows("S10/default", *outcome)[0][4], outcome.note)
        # Each attempt has its own scratch, removed afterwards, its own step and the same outer deadline.
        scratches = [Path(step.argv[-1]) for step in gate.steps]
        self.assertEqual(len(set(scratches)), 2)
        self.assertFalse(any(path.exists() for path in scratches))
        self.assertEqual([step.id for step in gate.steps],
                         ["delivery-S10-default-attempt1", "delivery-S10-default-attempt2"])
        self.assertEqual([step.timeout_s for step in gate.steps], [60, 60])
        # Both attempts keep their record, delivered text and log.
        self.assertEqual((self.evidence / "delivery-S10-default-attempt1.txt").read_text(encoding="utf-8"),
                         "partial frames\n")
        first = json.loads((self.evidence / "delivery-S10-default-attempt1.json").read_text(encoding="utf-8"))
        self.assertEqual(first["checks"][0]["unseen_markers"], ["line 214 of 99999"])
        self.assertTrue((self.evidence / "delivery-S10-default-attempt2.json").is_file())
        self.assertTrue((self.evidence / "delivery-S10-default-attempt2.txt").is_file())
        self.assertEqual(sorted(path.name for path in self.evidence.glob("*.log")),
                         ["03-delivery-S10-default-attempt1.log", "03-delivery-S10-default-attempt2.log"])

    def test_three_retryable_attempts_block_and_disclose_every_detail(self):
        # The retry is bounded: a third missing-frame attempt blocks S10, and the note gives all three details.
        markers = ("line 214 of 99999", "Uptime frame 160", "line 3 of 99999")
        gate, outcome = self.replay_attempts([self.retryable(markers=(marker,)) for marker in markers],
                                             variant="default")
        self.assertEqual(len(gate.steps), 3)
        self.assertEqual(outcome.problem, "delivery check failed: sync brackets: enclosed 0, empty pair ahead 0, "
                                          "absent 299, never painted 1")
        self.assertIn("; blocked on attempt 3 of 3; no attempts left", outcome.note)
        for attempt, marker in enumerate(markers, start=1):
            self.assertIn(f"attempt {attempt}: enclosed 0, empty pair ahead 0, absent 299, never painted 1 "
                          f"(marker `{marker}`)", outcome.note)
        self.assertEqual(perf.delivery_rows("S10/default", *outcome)[0][4],
                         f"blocked: {outcome.problem}; {outcome.note}")

    def test_sync_retries_with_brackets_and_lists_at_most_eight_markers(self):
        # `sync` accepts any bracket placement, so a missing frame with brackets is retried there; a long list
        # of missing markers is named up to the harness's eight, then counted.
        markers = tuple(f"line {number} of 99999" for number in range(1, 9))
        _gate, outcome = self.replay_attempts([self.retryable("sync", markers=markers, brackets=37, extra=3),
                                               self.passed("sync")])
        self.assertIsNone(outcome.problem)
        self.assertIn("never painted 11 (markers `line 1 of 99999`, `line 2 of 99999`, `line 3 of 99999`, "
                      "`line 4 of 99999`, `line 5 of 99999`, `line 6 of 99999`, `line 7 of 99999`, "
                      "`line 8 of 99999`, and 3 more)", outcome.note)

    def test_every_other_failure_blocks_on_its_first_attempt(self):
        # Only one missing-frame shape is retried; everything else blocks where it happens, as before.
        sentinel = {"name": "completion", "ok": False,
                    "detail": "the sentinel did not arrive before the scenario's timeout"}
        no_brackets = unseen_check()
        del no_brackets["brackets"]
        legacy = {"name": "sync brackets", "ok": False,
                  "detail": "enclosed 0, empty pair ahead 0, absent 299, never painted 1"}
        cases = {
            "default with an unseen frame and brackets": self.retryable(brackets=4),
            "default with brackets only": self.retryable(markers=(), brackets=4,
                                                         detail="enclosed 0, empty pair ahead 0, absent 300"),
            "another failing check": {"record": delivery_record(variant="default", checks=[sentinel]),
                                      "status": "FAIL", "exit_code": 5},
            "two failing checks": {"record": delivery_record(variant="default", checks=[unseen_check(), sentinel]),
                                   "status": "FAIL", "exit_code": 5},
            "a detail that disagrees": self.retryable(detail="enclosed 0, empty pair ahead 0, absent 298, "
                                                             "never painted 2"),
            "a detail with no count": self.retryable(detail="absent 300"),
            "an unseen count that is not a number": self.retryable(unseen="1"),
            "a zero unseen count": self.retryable(markers=(), detail="enclosed 0, empty pair ahead 0, absent 300"),
            "markers that disagree with the count": self.retryable(unseen_markers=[]),
            "markers that are not strings": self.retryable(unseen_markers=[214]),
            "no bracket count": {"record": delivery_record(variant="default", checks=[no_brackets]),
                                 "status": "FAIL", "exit_code": 5},
            "a version 1 record": {"record": delivery_record(schema_version=1, variant="default", checks=[legacy]),
                                   "status": "FAIL", "exit_code": 5},
            "exit 1": dict(self.retryable(), exit_code=1),
            "a timeout": dict(self.retryable(), status="TIMEOUT", exit_code=124),
            "a crash after the record": dict(self.retryable(), exit_code=3221225477),
            "a launch failure": dict(self.retryable(), status="LAUNCH", exit_code=None),
            "a missing record": {"record": None, "status": "FAIL", "exit_code": 5},
            "a passing step with a failed record": dict(self.retryable(), status="PASS", exit_code=0),
            "a non-classification detail with a valid suffix": self.retryable(
                detail="not a frame classification, never painted 1"),
            "schema 2.0": dict(self.retryable(), record=dict(self.retryable()["record"], schema_version=2.0)),
            "schema true": dict(self.retryable(), record=dict(self.retryable()["record"], schema_version=True)),
            "no delivered text": dict(self.retryable(), text=None),
            "delivered text shorter than the record states": dict(self.retryable(), text="partial"),
        }
        for name, attempt in cases.items():
            with self.subTest(case=name):
                for stale_file in self.evidence.iterdir():
                    stale_file.unlink()
                gate, outcome = self.replay_attempts([attempt], variant="default")
                self.assertEqual(len(gate.steps), 1)
                self.assertIsNotNone(outcome.problem)
                self.assertEqual(outcome.note, perf.DELIVERY_NOTE + "; blocked on attempt 1 of 3; not retryable")

    def test_a_failed_record_whose_replay_did_not_end_blocked_names_how_it_ended(self):
        # A later attempt that writes the same failed record but crashes, times out or exits other than 5 is
        # not retried, and its blocked reason names the step's real end, not only the record's failed check.
        for status, exit_code in (("FAIL", 3221225477), ("TIMEOUT", 124), ("FAIL", 1)):
            with self.subTest(status=status, exit_code=exit_code):
                for stale_file in self.evidence.iterdir():
                    stale_file.unlink()
                gate, outcome = self.replay_attempts(
                    [self.retryable(), dict(self.retryable(), status=status, exit_code=exit_code)], variant="default")
                self.assertEqual(len(gate.steps), 2)
                self.assertIn("never painted 1", outcome.problem)
                self.assertIn(f"the replay ended {status}, exit {exit_code}", outcome.problem)
                self.assertTrue(outcome.note.startswith(
                    perf.DELIVERY_NOTE + "; blocked on attempt 2 of 3; not retryable; attempt 1: "))

    def test_a_retryable_attempt_then_another_failure_blocks_with_the_second_reason(self):
        # A non-retryable failure after a retryable one blocks at once with its own reason, disclosing attempt 1.
        gate, outcome = self.replay_attempts([self.retryable(), {"record": None, "status": "FAIL", "exit_code": 1}],
                                             variant="default")
        self.assertEqual(len(gate.steps), 2)
        self.assertIsNone(outcome.record)
        self.assertIn("delivery.json is missing", outcome.problem)
        self.assertIn("the replay ended FAIL, exit 1", outcome.problem)
        self.assertEqual(outcome.note, perf.DELIVERY_NOTE + "; blocked on attempt 2 of 3; not retryable; attempt 1: "
                                       "enclosed 0, empty pair ahead 0, absent 299, never painted 1 "
                                       "(marker `line 214 of 99999`)")
        self.assertEqual(perf.delivery_rows("S10/default", *outcome),
                         [["S10/default", "delivery", "blocked", "blocked",
                           f"blocked: {outcome.problem}; {outcome.note}"]])

    def test_unproven_teardown_on_any_attempt_stops_the_comparison(self):
        # A retry never hides unproven teardown: whichever attempt leaves it raises StopComparison.
        unverified = custody(empty=False, errors=["2 members alive"])
        for failing_attempt in (1, 2):
            attempts = [self.retryable(), self.retryable()]
            attempts[failing_attempt - 1] = dict(attempts[failing_attempt - 1], custody=unverified)
            with self.subTest(attempt=failing_attempt), \
                    self.assertRaisesRegex(perf.StopComparison, "delivery replay S10/default: unresolved cleanup"):
                self.replay_attempts(attempts, variant="default")

    def test_the_smoke_applies_the_same_retry_rule(self):
        # The Windows smoke reaches the replay through run_delivery_replay too, so one missing frame is retried
        # there, a pass on attempt 2 lets the smoke pass, and three missing frames block it with every detail.
        scenarios = dict(SmokeTests.SCENARIOS, S1=perf.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 300, 80),
                         S10=perf.Scenario("S10", ("default", "sync"), "Redraw", 300, 240))

        def run_case(plan, evidence):
            kind = "invalid" if plan.variant == "role-exit" else "valid"
            return outcome_of(kind)(plan)
        sequences = {
            "retried": ([self.retryable("sync"), self.passed("sync")], perf.EXIT_PASS),
            "exhausted": ([self.retryable("sync")] * 3, perf.EXIT_BLOCKED),
        }
        for name, (attempts, expected_code) in sequences.items():
            for stale_file in self.evidence.iterdir():
                stale_file.unlink()
            output = io.StringIO()

            def replay(scenario, variant, evidence, attempts=attempts):
                return self.replay_attempts(attempts, variant=variant)[1]
            with self.subTest(sequence=name), contextlib.redirect_stdout(output), \
                    mock.patch.object(perf, "case_verdict", lambda case, kind, reasons: (
                        "pass" if kind == "valid" or case.expected == kind else "fail", list(reasons))):
                code, reasons = perf.smoke_cases(scenarios, Path("/b"), HARNESS_HASH, run_case, self.evidence,
                                                 host_platform="win32", replay=replay)
            self.assertEqual(code, expected_code, reasons)
            if expected_code == perf.EXIT_PASS:
                self.assertIn("passed on attempt 2 of 3", output.getvalue())
            else:
                self.assertTrue(any("blocked on attempt 3 of 3" in reason and "attempt 3: " in reason
                                    for reason in reasons), reasons)


    def test_a_passing_record_with_malformed_fields_is_not_admitted(self):
        # Validation runs before admission too: a passing step whose record has broken fields blocks S10.
        broken = self.passed()
        broken["record"]["checks"][0]["unseen"] = None
        gate, outcome = self.replay_attempts([broken], variant="default")
        self.assertEqual(len(gate.steps), 1)
        self.assertIsNone(outcome.record)
        self.assertIn("malformed", outcome.problem)

    def test_a_retry_needs_its_delivered_text_within_the_cap(self):
        # A retried attempt's classified bytes are its only evidence, so a missing text file blocks instead of
        # retrying, and so does text past the harness's kept-output cap.
        gate, outcome = self.replay_attempts([dict(self.retryable(), text=None)], variant="default")
        self.assertEqual(len(gate.steps), 1)
        self.assertIn("delivery-S10-default-attempt1.txt is missing", outcome.problem)
        self.assertEqual(outcome.note, perf.DELIVERY_NOTE + "; blocked on attempt 1 of 3; not retryable")
        for stale_file in self.evidence.iterdir():
            stale_file.unlink()
        with mock.patch.object(perf, "DELIVERY_TEXT_LIMIT_BYTES", 4):
            gate, outcome = self.replay_attempts([self.retryable()], variant="default")
        self.assertEqual(len(gate.steps), 1)
        self.assertIn("passes the 4-byte cap", outcome.problem)

    def test_a_retried_smoke_pass_keeps_and_flags_its_evidence(self):
        # smoke_main deletes a passing smoke's evidence, but not when a replay was retried: then every attempt's
        # record, text and log stay, and the job is told to upload them. An unretried pass still removes them.
        scenarios = dict(SmokeTests.SCENARIOS, S1=perf.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 300, 80),
                         S10=perf.Scenario("S10", ("default", "sync"), "Redraw", 300, 240))

        def run_case(plan, evidence):
            kind = "invalid" if plan.variant == "role-exit" else "valid"
            return outcome_of(kind)(plan)
        for name, attempts in (("retried", [self.retryable("sync"), self.passed("sync")]),
                               ("first try", [self.passed("sync")])):
            kept = []

            def runner(evidence, attempts=attempts):
                kept.append(evidence)

                def replay(scenario, variant, replay_evidence):
                    return self.replay_attempts(attempts, variant=variant, evidence=replay_evidence)[1]
                return perf.smoke_cases(scenarios, Path("/b"), HARNESS_HASH, run_case, evidence,
                                        host_platform="win32", replay=replay)
            env_file = self.evidence / f"github-env-{name.replace(' ', '-')}"
            output = io.StringIO()
            with self.subTest(sequence=name), mock.patch.object(perf.sys, "platform", "win32"), \
                    contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()), \
                    mock.patch.object(perf, "case_verdict", lambda case, kind, reasons: (
                        "pass" if kind == "valid" or case.expected == kind else "fail", list(reasons))):
                self.assertEqual(perf.smoke_main({"GITHUB_ENV": str(env_file)}, runner), perf.EXIT_PASS)
                exported = env_file.read_text(encoding="utf-8")
                if name == "retried":
                    self.assertTrue(kept[0].is_dir())
                    self.assertEqual(sorted(path.name for path in kept[0].iterdir()),
                                     ["03-delivery-S10-sync-attempt1.log", "03-delivery-S10-sync-attempt2.log",
                                      "delivery-S10-sync-attempt1.json", "delivery-S10-sync-attempt1.txt",
                                      "delivery-S10-sync-attempt2.json", "delivery-S10-sync-attempt2.txt"])
                    self.assertIn(f"{perf.REPLAY_RETRIED_ENV}=1\n", exported)
                    self.assertIn("a delivery replay was retried", output.getvalue())
                    perf.shutil.rmtree(kept[0])
                else:
                    self.assertFalse(kept[0].exists())
                    self.assertNotIn(perf.REPLAY_RETRIED_ENV, exported)

    def test_ci_uploads_the_windows_smoke_evidence_on_a_retried_pass(self):
        # A retried pass is green, so the Windows upload step must also run when smoke_main flags a retry.
        workflow = (Path(__file__).resolve().parent.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8")
        step = workflow[workflow.index("      - name: Upload Windows perf scenario smoke evidence\n"):]
        condition = step.splitlines()[1]
        self.assertEqual(condition, "        if: ${{ (failure() || env.%s == '1') && "
                                    "env.SONICTERM_PERF_EVIDENCE_DIR != '' }}" % perf.REPLAY_RETRIED_ENV)


# A tree that declares only perf-frame-texture, and one that declares both perf features.
FRAME_TEXTURE_TABLE = "\n[features]\nperf-frame-texture = []\n"
BOTH_FEATURES_TABLE = "\n[features]\nperf-counters = []\nperf-frame-texture = []\n"
FRAME_MANIFEST = HEAD_MANIFEST + FRAME_TEXTURE_TABLE
BOTH_MANIFEST = HEAD_MANIFEST + BOTH_FEATURES_TABLE
# A listing whose S4 caps its default variant at one short run; S4 has no Windows delivery replay.
CAPPED_LISTING = {"schema_version": 1, "scenarios": [
    {"id": "S4", "variants": ["default"], "title": "Stream", "timeout_s": 120, "short_timeout_s": 30,
     "run_caps": {"default": 1}}]}


class RunCapListTests(unittest.TestCase):
    """`--list` may cap a variant's short-mode runs; anything but a positive count per listed variant is refused."""

    def entry(self, **extra):
        return {"schema_version": 1, "scenarios": [
            {"id": "S11", "variants": ["default", "release"], "title": "Image", "timeout_s": 420,
             "short_timeout_s": 300, **extra}]}

    def test_absent_caps_leave_the_scenario_uncapped(self):
        # Without `run_caps` no variant is capped, as before.
        scenario = perf.parse_scenario_list(json.dumps(self.entry()))[0]
        self.assertEqual(scenario.run_caps, ())
        self.assertIsNone(scenario.cap("release"))

    def test_caps_are_read_per_variant(self):
        # Each listed variant's cap is kept; an unlisted variant stays uncapped.
        scenario = perf.parse_scenario_list(json.dumps(self.entry(run_caps={"release": 1})))[0]
        self.assertEqual((scenario.cap("release"), scenario.cap("default")), (1, None))

    def test_malformed_caps_are_refused(self):
        # Zero, a negative or non-integer count, an unknown variant and a non-object all fail the listing.
        for caps in ({"release": 0}, {"release": -1}, {"release": "1"}, {"release": True}, {"release": 1.5},
                     {"gdi": 1}, [["release", 1]], "release"):
            with self.subTest(caps=caps), self.assertRaisesRegex(ValueError, "malformed scenario entry"):
                perf.parse_scenario_list(json.dumps(self.entry(run_caps=caps)))

    def test_capped_runs_apply_in_short_mode_only(self):
        # min(requested, cap) under --short; a cap above the request keeps the request; release mode is uncapped.
        scenario = perf.parse_scenario_list(json.dumps(self.entry(run_caps={"release": 2})))[0]
        self.assertEqual(perf.capped_runs(scenario, "release", 5, short=True), 2)
        self.assertEqual(perf.capped_runs(scenario, "release", 1, short=True), 1)
        self.assertEqual(perf.capped_runs(scenario, "release", 5, short=False), 5)
        self.assertEqual(perf.capped_runs(scenario, "default", 5, short=True), 5)


class RunCapCompareTests(CompareHarness, unittest.TestCase):
    """A capped variant runs min(requested, cap) valid runs per side in every set, and the table says so."""

    def capped(self, *options, **kwargs):
        return self.compare(listing=CAPPED_LISTING, scenarios=("S4",), options=("--runs", "5", *options), **kwargs)

    def per_side(self, plans, counters=False):
        return [sum(1 for plan in plans if plan.side == side and plan.counters == counters) for side in perf.SIDES]

    def test_no_metadata_leaves_the_runs_unchanged(self):
        # The plain listing has no caps: five valid runs per side under --short, as before.
        _code, _gate, _calls, plans, _work, _out = self.compare(options=("--runs", "5", "--short"))
        self.assertEqual(self.per_side(plans), [5, 5])

    def test_the_cap_applies_to_runs_and_counters_runs(self):
        # --runs 5 and --counters-runs 2 both become one run per side.
        code, _gate, _calls, plans, _work, _out = self.capped(
            "--short", "--counters", "--counters-runs", "2", head_manifest=COUNTERS_MANIFEST,
            base_manifest=BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, perf.EXIT_PASS)
        self.assertEqual(self.per_side(plans), [1, 1])
        self.assertEqual(self.per_side(plans, counters=True), [1, 1])

    def test_the_laps_and_alloc_sets_are_capped(self):
        # Every set of a capped variant takes the cap, laps and alloc included.
        code, _gate, _calls, plans, _work, _out = self.capped("--short", "--laps", "--alloc")
        self.assertEqual(code, perf.EXIT_PASS)
        for side in perf.SIDES:
            self.assertEqual(sum(1 for plan in plans if plan.side == side and plan.laps), 1, side)
            self.assertEqual(sum(1 for plan in plans if plan.side == side
                                 and plan.binary.name.startswith(perf.ALLOC_EXAMPLE)), 1, side)

    def test_release_mode_ignores_the_cap(self):
        # A release comparison runs every requested run.
        _code, _gate, _calls, plans, _work, _out = self.compare(
            listing=CAPPED_LISTING, scenarios=("S4",), options=("--runs", "3"))
        self.assertEqual(self.per_side(plans), [3, 3])

    def test_capped_rows_and_the_document_disclose_the_cap(self):
        # Each capped row is labelled `(runs N of M)` and comparison.md lists the capped variants.
        _code, _gate, _calls, _plans, _work, out = self.capped("--short")
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S4/default (runs 1 of 5) | status | 1 valid run | 1 valid run |", document)
        self.assertIn("Capped variants (--short): S4/default timed 1 of 5 runs.", document)
        _code, _gate, _calls, _plans, _work, out = self.compare(options=("--runs", "5", "--short"))
        self.assertNotIn("Capped variants", (out / "comparison.md").read_text(encoding="utf-8"))


def memory_sample(unix_s, renderer_bytes=16 * 1024 * 1024):
    return perf.MemorySample(unix_s, 100 * 1024 * 1024, renderer_bytes, 0)


class MemoryFreshnessTests(unittest.TestCase):
    """A checkpoint with fresh_after_unix_s reads memory only from a sample taken at or after that time."""

    def test_absent_metadata_keeps_the_latest_earlier_sample(self):
        samples = [memory_sample(10.0), memory_sample(40.0)]
        self.assertEqual(perf.memory_at(samples, 50.0).unix_s, 40.0)

    def test_a_sample_before_the_threshold_reads_unavailable(self):
        # The latest sample at or before the checkpoint predates freshness, so the reading is unavailable.
        samples = [memory_sample(10.0), memory_sample(40.0)]
        self.assertIsNone(perf.memory_at(samples, 80.0, fresh_after_unix_s=45.0))

    def test_samples_exactly_at_and_after_the_threshold_are_used(self):
        for sample_s in (45.0, 70.0):
            with self.subTest(sample_s=sample_s):
                samples = [memory_sample(10.0), memory_sample(sample_s)]
                self.assertEqual(perf.memory_at(samples, 80.0, fresh_after_unix_s=45.0).unix_s, sample_s)

    def test_non_finite_or_negative_values_are_schema_problems(self):
        # fresh_after_unix_s and frame_texture_bytes must be finite and non-negative; anything else fails the schema.
        base = {"index": 0, "label": "released", "unix_s": 80.0, "footprint_file": None}
        for key, value in (("fresh_after_unix_s", float("nan")), ("fresh_after_unix_s", float("inf")),
                           ("fresh_after_unix_s", -1.0), ("fresh_after_unix_s", "45"),
                           ("fresh_after_unix_s", True), ("frame_texture_bytes", -4),
                           ("frame_texture_bytes", 4.5), ("frame_texture_bytes", True)):
            with self.subTest(key=key, value=value):
                checkpoint = dict(base, **{key: value})
                self.assertFalse(perf._checkpoint_ok(checkpoint))
                problems = perf.validate_result(valid_result(checkpoints=[checkpoint]), HARNESS_HASH, 0)
                self.assertTrue(any("checkpoints" in problem for problem in problems), problems)
        self.assertTrue(perf._checkpoint_ok(dict(base, fresh_after_unix_s=45.0, frame_texture_bytes=4)))

    def test_run_metrics_report_a_released_reading_only_when_fresh(self):
        # The released row appears only when its checkpoint's own sample is fresh; a tagged sample taken
        # before fresh_after_unix_s reads unavailable.
        point = {"index": 1, "label": "released", "unix_s": 80.0, "footprint_file": None,
                 "fresh_after_unix_s": 45.0}
        tag = {"checkpoint_index": 1, "checkpoint_label": "released", "checkpoint_attempt": 1,
               "checkpoint_complete": True}
        result = valid_result(checkpoints=[point], checkpoint_memory="supported")
        stale = make_outcome(result=result, memory=[dataclasses.replace(memory_sample(40.0), **tag)])
        fresh = make_outcome(result=result, memory=[dataclasses.replace(memory_sample(46.0), **tag)])
        self.assertNotIn(("released renderer_total_bytes", "MiB", "run"), perf.run_metrics(stale))
        self.assertIn(("released renderer_total_bytes", "MiB", "run"), perf.run_metrics(fresh))


class GridBytesPerPaneRowTests(unittest.TestCase):
    """A checkpoint whose own tagged sample carries grid fields gets a grid-bytes-per-pane row, read through the
    authoritative-sample rule; one without the fields gets none, and a harness without the hook reads n/a."""

    def test_the_row_is_the_per_pane_grid_figure_in_mebibytes(self):
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}
        supported = valid_result(checkpoints=[point], checkpoint_memory="supported")
        with_grid = perf.MemorySample(60.0, None, 1, 0, checkpoint_index=0, checkpoint_label="end",
                                      checkpoint_attempt=1, checkpoint_complete=True,
                                      grid_visible_bytes=1048576, grid_history_bytes=2 * 1048576,
                                      grid_alternate_bytes=1048576, panes_sampled=2)
        metrics = perf.run_metrics(make_outcome(result=supported, memory=[with_grid]))
        self.assertEqual(metrics[("end grid bytes per pane", "MiB", "run")], 2.0)
        without = perf.run_metrics(make_outcome(result=supported, memory=[tagged_sample(0, 1, True)]))
        self.assertNotIn(("end grid bytes per pane", "MiB", "run"), without)
        # An untagged periodic sample with grid fields is never substituted for a checkpoint's own reading.
        periodic = perf.run_metrics(make_outcome(result=valid_result(checkpoints=[point]),
                                                 memory=[perf.MemorySample(60.0, None, 1, 0,
                                                                           grid_visible_bytes=1048576,
                                                                           grid_history_bytes=0,
                                                                           grid_alternate_bytes=0,
                                                                           panes_sampled=1)]))
        self.assertIsInstance(periodic[("end grid bytes per pane", "MiB", "run")], perf.NotAvailable)


class FrameTextureRowTests(unittest.TestCase):
    """The end checkpoint's frame_texture_bytes becomes its own row, n/a on a base that never reports it."""

    def test_the_row_comes_from_the_checkpoint_not_from_memory(self):
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None, "frame_texture_bytes": 4}
        head = perf.SideRuns(outcomes=[make_outcome(result=valid_result(checkpoints=[point]))])
        base = perf.SideRuns(outcomes=[make_outcome()])
        self.assertEqual(perf.run_metrics(head.outcomes[0])[("end frame_texture_bytes", "B", "run")], 4)
        row = row_for(perf.comparison_rows("S11/gdi", base, head), "end frame_texture_bytes (B)")
        self.assertEqual(row[2:4], ["n/a", "4.00 (4.00–4.00)"])


class FrameTextureFeatureTests(PrebuiltHarness, unittest.TestCase):
    """perf-frame-texture reaches only a tree that declares it, in every build path, and binds a prebuilt artifact."""

    def build_argv(self, gate, side):
        return next(step.argv for step in gate.steps if step.id == f"build-{side}-perf_scenarios")

    def test_a_building_comparison_passes_the_feature_only_to_a_declaring_tree(self):
        # The head declares it and builds with it; the base does not and builds without it. Windows per-shard
        # builds go through the same build_sides catalog.
        _code, gate, _calls, _plans, _work, out = self.compare(head_manifest=FRAME_MANIFEST)
        self.assertIn("perf-frame-texture", self.build_argv(gate, "head"))
        self.assertNotIn("--features", self.build_argv(gate, "base"))
        self.assertIn("- Built with `--features perf-frame-texture`: head",
                      (out / "comparison.md").read_text(encoding="utf-8"))
        _code, gate, _calls, _plans, _work, _out = self.compare(head_manifest=BOTH_MANIFEST)
        head = self.build_argv(gate, "head")
        self.assertEqual(head[head.index("--features") + 1], "perf-counters,perf-frame-texture")

    def test_build_only_records_the_feature_in_the_manifest(self):
        binaries, _digest = self.produce(head_manifest=FRAME_MANIFEST)
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["features"], {"base": [], "head": [perf.FRAME_TEXTURE_FEATURE]})

    def test_load_prebuilt_refuses_a_feature_mismatch(self):
        # A consumer whose head tree does not declare the feature refuses binaries built with it.
        binaries, digest = self.produce(head_manifest=FRAME_MANIFEST)
        with self.assertRaisesRegex(ValueError, "refusing the prebuilt binaries: features"):
            self.consume(binaries, digest)
        code, *_rest = self.consume(binaries, digest, head_manifest=FRAME_MANIFEST)
        self.assertEqual(code, perf.EXIT_PASS)

    def test_the_gate_reviews_one_build_per_feature_set(self):
        # Every feature combination has the gate's own reviewed steps; the plain and counters sets are the old ones.
        # Five features give 32 ordered subsets, each with its four side/example build steps. The catalog is
        # checked as data: no subset is compiled here.
        catalog = REAL_GATE.PERF_FEATURE_BUILDS
        counters, texture, hook, echo, trim = perf.PERF_FEATURES
        self.assertEqual((echo, trim), ("perf-echo-trace", "perf-hook-trim"))
        subsets = [()]
        for feature in perf.PERF_FEATURES:
            subsets += [subset + (feature,) for subset in subsets]
        self.assertEqual(set(catalog), set(subsets))
        self.assertEqual(len(catalog), 32)
        self.assertIn((counters, texture, hook, echo, trim), catalog)
        self.assertIn((counters, hook, echo), catalog, "a base without the trim hook keeps its own step")
        self.assertEqual(sum(len(steps) for steps in catalog.values()), 128)
        for features, steps in catalog.items():
            for step in steps.values():
                self.assertTrue(REAL_GATE._reviewed_step(step))
                if features:
                    self.assertEqual(step.argv[step.argv.index("--features") + 1], ",".join(features))
                else:
                    self.assertNotIn("--features", step.argv)
        for step_id, step in catalog[()].items():
            self.assertIs(step, REAL_GATE.PERF_BUILDS[step_id])
        for step_id, step in catalog[("perf-counters",)].items():
            self.assertIs(step, REAL_GATE.PERF_COUNTER_BUILDS[step_id])



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
    longest = sorted(records, key=lambda record: record["ms"], reverse=True)[:perf.SLOW_DISPATCH_LIMIT]
    end_s = max([begin_s + duration_ms / 1000 for begin_s, duration_ms in dispatches] + [start_s]) + 1.0
    return dict(valid_result()["phases"][0], name=name, start_unix_s=FALLBACK_EPOCH_S + start_s,
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
    return valid_result(phases=[dispatch_phase("workload", COUNTEREXAMPLE, **phase_options)])


class LapsSelectionTests(CompareHarness, unittest.TestCase):
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
                perf.parse_args(argv)
        # --laps-runs also sets the runs of a plain --laps set.
        self.assertEqual(perf.parse_args(comparison + ["--laps", "--laps-runs", "2"]).laps_runs, 2)

    def test_a_bare_id_means_default_and_only_named_variants_run_laps(self):
        # S10 names S10/default only; S10/sync and S1 run no laps, and the laps set takes --laps-runs per side.
        # On Windows S10 also replays its delivery first, which needs the real host this fixture stubs out;
        # the replay is not under test here, so it is turned off on every platform.
        with mock.patch.object(perf, "delivery_replayed", return_value=False):
            code, _gate, _calls, plans, _work, out = self.compare(
                scenarios=("S1", "S10", "S10/sync"), options=("--laps-scenario", "S10", "--laps-runs", "2"))
        self.assertEqual(code, perf.EXIT_PASS)
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
        _code, _gate, _calls, plans, _work, _out = self.compare(
            listing=CAPPED_LISTING, scenarios=("S4",), options=("--short", "--laps-scenario", "S4", "--laps-runs", "3"))
        self.assertEqual(self.laps_plans(plans), [("S4", "default", "base"), ("S4", "default", "head")])
        _code, _gate, _calls, plans, _work, _out = self.compare(
            listing=CAPPED_LISTING, scenarios=("S4",), options=("--laps-scenario", "S4", "--laps-runs", "3"))
        self.assertEqual(len(self.laps_plans(plans)), 6)

    def test_the_command_records_the_flags(self):
        # The details block's invocation repeats the laps selection and its runs.
        args = perf.parse_args(["--base", "main", "--head", "HEAD", "--scenario", "S9", "--laps-scenario", "S9",
                                "--laps-runs", "2"])
        self.assertIn("--laps-scenario S9 --laps-runs 2", perf.comparison_command(args))


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
        log = perf.parse_fallback_log(lines)
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
                log = perf.parse_fallback_log([line])
                self.assertEqual((log.unparsed, log.waits), (1, []))
        for line in (font_line(10, 'operation="shape_impl"'),
                     font_line(10, 'operation="shape_impl" phase="return" elapsed_ms=NaN')):
            self.assertEqual(perf.parse_fallback_log([line]).unparsed, 0)

    def test_pairing_counts_never_change_the_verdict(self):
        # An enter left open at the log's end and a return with no open enter are counted, and the supported
        # counterexample stays supported; an enter followed by another enter leaves the first unmatched.
        lines = [receive_return(100, 0.5)] + counterexample_lines(range(19)) + [receive_enter(70_000_000)]
        log = perf.parse_fallback_log(lines)
        self.assertEqual((log.unmatched_enter, log.unmatched_return), (1, 1))
        self.assertEqual(perf.fallback_run_verdict(counterexample_result(), log).verdict, "supported")
        twice = perf.parse_fallback_log([receive_enter(1), receive_enter(2), receive_return(3, 0.001)])
        self.assertEqual((twice.unmatched_enter, twice.unmatched_return), (1, 0))

    def test_the_log_directory_is_read_in_file_order(self):
        # A run's logs are read file by file in name order, so a pair split across files still matches; a run
        # without a logs directory has no log at all.
        with tempfile.TemporaryDirectory() as temporary:
            logs = Path(temporary) / "logs"
            logs.mkdir()
            (logs / "sonicterm.log.1").write_text(receive_enter(1) + "\n", encoding="utf-8")
            (logs / "sonicterm.log.2").write_text(receive_return(5, 0.004) + "\n", encoding="utf-8")
            log = perf.read_fallback_log(logs)
            self.assertEqual((len(log.waits), log.unmatched_enter, log.unmatched_return), (1, 0, 0))
            self.assertIsNone(perf.read_fallback_log(Path(temporary) / "missing"))


class FallbackVerdictTests(unittest.TestCase):
    """A run is supported only when an examined slow dispatch's matched waits cover half of it."""

    def test_the_counterexample_is_supported(self):
        # Nineteen 1,900 ms dispatches dominated by fallback_receive beside one 2,000 ms dispatch with none:
        # p95 examines all twenty, coverage is complete and the dominated ones support the claim.
        verdict = perf.fallback_run_verdict(counterexample_result(),
                                            perf.parse_fallback_log(counterexample_lines(range(19))))
        self.assertEqual((verdict.verdict, verdict.coverage), ("supported", "complete"))
        self.assertEqual((len(verdict.inside_ms), sum(verdict.inside_ms), verdict.outside_ms), (19, 19 * 1700.0, []))

    def test_truncated_waitless_or_absent_logs_are_inconclusive(self):
        # The counterexample without the records of its dominated dispatches (one wait left in a gap), a log
        # with no waits and no log at all are each inconclusive, never refuted.
        gap = [receive_enter(3_000_000), receive_return(3_500_000, 400.0)]
        verdict = perf.fallback_run_verdict(counterexample_result(), perf.parse_fallback_log(gap))
        self.assertEqual((verdict.verdict, verdict.coverage, verdict.outside_ms), ("inconclusive", "complete", [400.0]))
        waitless = perf.parse_fallback_log([font_line(10, 'operation="shape_impl" phase="enter"')])
        self.assertEqual(perf.fallback_run_verdict(counterexample_result(), waitless).verdict, "inconclusive")
        self.assertEqual(perf.fallback_run_verdict(counterexample_result(), None).verdict, "inconclusive")
        unparsed = perf.parse_fallback_log([receive_return(10, "NaN")])
        self.assertEqual(perf.fallback_run_verdict(counterexample_result(), unparsed).verdict, "inconclusive")

    def test_a_zero_length_dispatch_without_waits_is_inconclusive(self):
        # A recorded 0 ms dispatch with an empty log satisfies "matched >= half its duration" as 0 >= 0; support
        # needs matched waiting, so the run stays inconclusive, as every waitless log does.
        result = valid_result(phases=[dispatch_phase("workload", [(1.0, 0.0)])])
        verdict = perf.fallback_run_verdict(result, perf.parse_fallback_log([]))
        self.assertEqual((verdict.verdict, verdict.inside_ms), ("inconclusive", []))

    def test_coverage_beyond_the_recorded_dispatches(self):
        # A hundred equal 1,900 ms dispatches leave 36 at or above p95 unrecorded: waits only in unrecorded ones are
        # inconclusive, while waits in a recorded one stay supported although coverage is incomplete. A phase
        # from a harness that records no slow dispatches has unavailable coverage.
        dispatches = [(1.0 + 3.0 * index, 1900.0) for index in range(100)]
        result = valid_result(phases=[dispatch_phase("workload", dispatches)])

        def waits_in(indices):
            lines = []
            for index in indices:
                begin_us = int(dispatches[index][0] * 1_000_000)
                lines += [receive_enter(begin_us + 100_000), receive_return(begin_us + 1_800_000, 1700.0)]
            return perf.parse_fallback_log(lines)

        unrecorded = perf.fallback_run_verdict(result, waits_in(range(70, 100)))
        self.assertEqual((unrecorded.verdict, unrecorded.coverage), ("inconclusive", "incomplete"))
        recorded = perf.fallback_run_verdict(result, waits_in([3]))
        self.assertEqual((recorded.verdict, recorded.coverage), ("supported", "incomplete"))
        older = valid_result(phases=[{key: value for key, value in dispatch_phase("workload", COUNTEREXAMPLE).items()
                                      if key not in ("slow_dispatches", "dispatch_count")}])
        verdict = perf.fallback_run_verdict(older, perf.parse_fallback_log(counterexample_lines(range(19))))
        self.assertEqual((verdict.verdict, verdict.coverage), ("inconclusive", "unavailable"))

    def test_waits_never_match_another_run_or_phase(self):
        # One run's waits cover the other run's dispatches, so neither run, nor the side, is supported; a wait in
        # one phase's span never matches a dispatch another phase lists.
        dominated = perf.parse_fallback_log(counterexample_lines(range(19)))
        moved = valid_result(phases=[dispatch_phase("workload", [(begin_s + 1.5, duration_ms) for begin_s, duration_ms in COUNTEREXAMPLE],
                                                    start_s=0.0)])
        side = perf.SideRuns(outcomes=[make_outcome(result=moved, fallback_log=dominated),
                                       make_outcome(result=counterexample_result(),
                                                    fallback_log=perf.parse_fallback_log([]))])
        self.assertEqual(perf.fallback_side_verdict(side), "inconclusive")
        other_phase = dispatch_phase("warmup", [(200.0, 10.0)], start_s=199.0)
        other_phase["slow_dispatches"] = dispatch_phase("workload", COUNTEREXAMPLE)["slow_dispatches"]
        empty = dispatch_phase("workload", [(1.0, 10.0)])
        crossed = valid_result(phases=[empty, other_phase])
        self.assertEqual(perf.fallback_run_verdict(crossed, dominated).verdict, "inconclusive")

    def test_only_two_verdicts(self):
        # Whatever the input, a run and a side are only ever supported or inconclusive.
        self.assertEqual(perf.FALLBACK_VERDICTS, ("supported", "inconclusive"))
        logs = [None, perf.parse_fallback_log([]), perf.parse_fallback_log(counterexample_lines(range(19))),
                perf.parse_fallback_log([receive_return(10, "NaN")])]
        results = [counterexample_result(), valid_result(), valid_result(phases=[]), {}]
        for result in results:
            for log in logs:
                self.assertIn(perf.fallback_run_verdict(result, log).verdict, perf.FALLBACK_VERDICTS)
        for side in (perf.SideRuns(), perf.SideRuns(blocked="no base"), perf.SideRuns(failed="crashed")):
            self.assertIn(perf.fallback_side_verdict(side), perf.FALLBACK_VERDICTS)

    def test_rows_report_waits_and_the_verdict_per_side(self):
        # Waits inside and outside examined slow dispatches, then each side's verdict with its coverage,
        # unparsed and unmatched counts beside it.
        base = perf.SideRuns(outcomes=[make_outcome(result=counterexample_result(),
                                                    fallback_log=perf.parse_fallback_log(counterexample_lines(range(19))))])
        head = perf.SideRuns(outcomes=[make_outcome(result=counterexample_result(),
                                                    fallback_log=perf.parse_fallback_log([]))])
        cells = {row[1]: (row[2], row[3]) for row in perf.fallback_rows("S9/default", base, head)}
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
            phase = dict(valid_result()["phases"][0], **fields)
            return perf.validate_result(valid_result(phases=[phase]), HARNESS_HASH, 0)

        self.assertEqual(problems(slow_dispatches=[{"start_unix_s": 1.0, "end_unix_s": 2.0, "ms": 1000.0}],
                                  dispatch_count=2), [])
        for fields in ({"slow_dispatches": "x"}, {"slow_dispatches": [{"start_unix_s": 1.0, "end_unix_s": 2.0}]},
                       {"dispatch_count": -1}, {"dispatch_count": 1.5}):
            with self.subTest(fields=fields):
                self.assertTrue(problems(**fields))


# An app source file defining the checkpoint-memory hook, and the manifests that declare its feature.
HOOK_SOURCE = ("impl App {\n    #[doc(hidden)]\n    pub fn __perf_checkpoint_memory(&mut self, index: usize) {}\n}\n")
HOOK_TABLE = "\n[features]\nperf-hook-checkpoint-memory = []\n"
HOOK_MANIFEST = HEAD_MANIFEST + HOOK_TABLE
# An app source file defining the trim hook, and manifests that declare it: alone, and with every other feature.
TRIM_SOURCE = ("impl App {\n    #[doc(hidden)]\n    pub fn __trim_covered_now(&mut self, window_id: WindowId) {}\n}\n")
TRIM_MANIFEST = HEAD_MANIFEST + "\n[features]\nperf-hook-trim = []\n"
ALL_FEATURES_TABLE = ("\n[features]\nperf-counters = []\nperf-frame-texture = []\nperf-hook-checkpoint-memory = []\n"
                      "perf-echo-trace = [\"perf-counters\"]\n")
FOUR_FEATURES_MANIFEST = HEAD_MANIFEST + ALL_FEATURES_TABLE
FIVE_FEATURES_MANIFEST = FOUR_FEATURES_MANIFEST + "perf-hook-trim = []\n"
COUNTERS_HOOK_MANIFEST = HEAD_MANIFEST + "\n[features]\nperf-counters = []\nperf-hook-checkpoint-memory = []\n"


# Golden runs the harness's own test writes: `result.json` from the production serializer and the
# `memory` lines the App's hook logged, keyed by build ("supported" or "unsupported") and case.
CHECKPOINT_FIXTURE = Path(__file__).with_name("perf-compare_checkpoint_fixture.json")
# Two runs' renderer attempt deltas, written by the harness test that computes them.
ATTEMPT_FIXTURE = Path(__file__).with_name("perf-compare_attempt_fixture.json")


def fixture_run(build, case):
    """One golden run's result and its memory samples, parsed as a real run's logs are."""
    run = json.loads(CHECKPOINT_FIXTURE.read_text(encoding="utf-8"))[build][case]
    samples = sorted(filter(None, map(perf.parse_memory_line, run["logs"])), key=lambda sample: sample.unix_s)
    result = run["result"]
    # The golden runs come from the harness's headless probe, which has no renderer and so records no presenter;
    # a real macOS run records one, and these tests read checkpoints, not the presenter.
    if result.get("presenter") is None:
        result["presenter"] = dict(MACOS_PRESENTER)
    return result, samples


def fixture_outcome(build, case, plan=None):
    """A valid run outcome carrying a golden run's result and memory samples."""
    result, samples = fixture_run(build, case)
    return make_outcome(**({"plan": plan} if plan is not None else {}), result=result, memory=samples)


def tagged_sample(index, attempt, complete, renderer_mib=16, unix_s=60.0):
    """A memory line tagged with checkpoint `index`, attempt `attempt`."""
    return perf.MemorySample(unix_s, 100 * 1048576, renderer_mib * 1048576, 0, checkpoint_index=index,
                             checkpoint_label="end", checkpoint_attempt=attempt, checkpoint_complete=complete)


class CheckpointMemoryTests(unittest.TestCase):
    """A checkpoint reads its own tagged sample: the complete one with the highest attempt, else the last partial."""

    LINE = ("2026-10-03T00:00:01Z  INFO memory: memory snapshot process_resident_bytes=10 "
            "session_total_bytes=3 panes_total=2 panes_sampled=1 panes_contended=1 renderer_total_bytes=7 "
            "checkpoint_index=2 checkpoint_label=\"end\" checkpoint_attempt=3 checkpoint_complete=false")

    def test_a_tagged_line_parses_its_four_tags_and_pane_counts(self):
        # The label comes back without the log layer's quotes; a periodic line carries no tags.
        sample = perf.parse_memory_line(self.LINE)
        self.assertEqual((sample.checkpoint_index, sample.checkpoint_label, sample.checkpoint_attempt,
                          sample.checkpoint_complete), (2, "end", 3, False))
        self.assertEqual((sample.panes_total, sample.panes_sampled, sample.panes_contended), (2, 1, 1))
        periodic = perf.parse_memory_line(memory_line())
        self.assertEqual((periodic.checkpoint_index, periodic.checkpoint_complete), (None, None))

    def test_a_complete_attempt_wins_over_an_earlier_partial_one(self):
        reading = perf.checkpoint_memory([tagged_sample(0, 1, False, 9), tagged_sample(0, 2, True, 16)], 0)
        self.assertEqual((reading.sample.checkpoint_attempt, reading.partial), (2, False))

    def test_all_partial_reads_the_highest_attempt_marked_partial(self):
        samples = [tagged_sample(0, attempt, False, attempt) for attempt in (1, 3, 2)]
        reading = perf.checkpoint_memory(samples, 0)
        self.assertEqual((reading.sample.checkpoint_attempt, reading.partial), (3, True))

    def test_a_repeated_line_counts_once(self):
        # The same reading twice, even at another timestamp, is one reading, not a conflict.
        reading = perf.checkpoint_memory([tagged_sample(0, 1, True), tagged_sample(0, 1, True, unix_s=61.0)], 0)
        self.assertIsNone(reading.problem)
        self.assertEqual(reading.sample.checkpoint_attempt, 1)

    def test_two_complete_samples_of_one_attempt_with_different_totals_conflict(self):
        reading = perf.checkpoint_memory([tagged_sample(0, 1, True, 16), tagged_sample(0, 1, True, 17)], 0)
        self.assertEqual((reading.sample, reading.problem), (None, "conflicting samples"))
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}
        result = valid_result(checkpoints=[point], checkpoint_memory="supported")
        side = perf.SideRuns([make_outcome(result=result, memory=[tagged_sample(0, 1, True, 16),
                                                                  tagged_sample(0, 1, True, 17)])])
        rows = perf.comparison_rows("S1/default", side, side)
        self.assertEqual(row_for(rows, "end renderer_total_bytes (MiB)")[2], "n/a: conflicting samples")

    def test_a_periodic_sample_is_never_a_checkpoint_reading(self):
        # An older untagged sample before the checkpoint is not substituted for the missing tagged one.
        self.assertIsNone(perf.checkpoint_memory([memory_sample(50.0)], 0))
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}
        outcome = make_outcome(result=valid_result(checkpoints=[point], checkpoint_memory="supported"),
                               memory=[memory_sample(50.0)])
        self.assertNotIn(("end renderer_total_bytes", "MiB", "run"), perf.run_metrics(outcome))

    def test_each_index_reads_its_own_samples(self):
        samples = [tagged_sample(1, 1, True, 11), tagged_sample(2, 1, True, 22)]
        self.assertEqual(perf.checkpoint_memory(samples, 1).sample.renderer_total_bytes, 11 * 1048576)
        self.assertEqual(perf.checkpoint_memory(samples, 2).sample.renderer_total_bytes, 22 * 1048576)
        self.assertIsNone(perf.checkpoint_memory(samples, 0))

    def test_a_partial_reading_counts_and_its_cell_says_so(self):
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None, "sampling": "exhausted",
                 "attempts": 5, "last_attempt_complete": False}
        result = valid_result(checkpoints=[point], checkpoint_memory="supported")
        self.assertEqual(perf.validate_result(result, HARNESS_HASH, 0), [])
        side = perf.SideRuns([make_outcome(result=result, memory=[tagged_sample(0, 5, False, 16)])])
        rows = perf.comparison_rows("S1/default", side, side)
        self.assertEqual(row_for(rows, "end renderer_total_bytes (MiB)")[2], "16.00 (16.00–16.00), 1 partial")

    def test_a_side_without_the_hook_reads_n_a_unsupported(self):
        # The base's harness has no hook, so its checkpoint rows say so; the head's tagged sample reads.
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}
        base = perf.SideRuns([make_outcome(result=valid_result(checkpoints=[point],
                                                               checkpoint_memory="unsupported"),
                                           memory=[memory_sample(60.0)])])
        old_base = perf.SideRuns([make_outcome(result=valid_result(checkpoints=[point]))])
        head = perf.SideRuns([make_outcome(result=valid_result(checkpoints=[point], checkpoint_memory="supported"),
                                           memory=[tagged_sample(0, 1, True, 16)])])
        for side in (base, old_base):
            row = row_for(perf.comparison_rows("S1/default", side, head), "end renderer_total_bytes (MiB)")
            self.assertEqual(row[2:5], ["n/a: unsupported", "16.00 (16.00–16.00)", "n/a"])

    def test_an_unknown_checkpoint_memory_or_sampling_state_is_a_schema_problem(self):
        self.assertTrue(perf.validate_result(valid_result(checkpoint_memory="maybe"), HARNESS_HASH, 0))
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None, "sampling": "late"}
        self.assertTrue(perf.validate_result(valid_result(checkpoints=[point]), HARNESS_HASH, 0))


def atlas_facts(renderer="main[1]", dim=512, packed=4000, growths=1, evictions=0, fit="512", tile=(25, 16)):
    """One renderer's glyph atlas facts as a memory line reports them."""
    return perf.GlyphAtlasFacts(renderer, dim, packed, growths, evictions, fit, tile)


def atlas_outcome(facts, phase_growths=None, checkpoint_unix_s=70.0, readings=None):
    """A valid run whose `end` checkpoint sample reports `facts`; `phase_growths` gives each phase's counted
    `renderer.glyph_atlas_growths` delta, one phase per entry ending at 20 s steps, or no counters when None;
    `readings` is the checkpoint's `atlas_readings` list, absent when None (a harness that predates it)."""
    point = {"index": 0, "label": "end", "unix_s": checkpoint_unix_s, "footprint_file": None}
    if readings is not None:
        point["atlas_readings"] = readings
    phases = valid_result()["phases"]
    if phase_growths is not None:
        phases = [{**phases[0], "name": f"phase{number}", "start_unix_s": 20.0 * number,
                   "end_unix_s": 20.0 * number + 10.0,
                   "frame_counters": {"renderer": {"glyph_atlas_growths": growths}}}
                  for number, growths in enumerate(phase_growths)]
    sample = dataclasses.replace(tagged_sample(0, 1, True), glyph_atlases=tuple(facts))
    return make_outcome(result=valid_result(checkpoints=[point], checkpoint_memory="supported", phases=phases),
                        memory=[sample])


class GlyphAtlasReportTests(unittest.TestCase):
    """The memory line's per-renderer glyph atlas facts become per-checkpoint report rows."""

    def test_each_checkpoint_reports_every_renderers_atlas_facts_and_a_base_without_them_reads_n_a(self):
        # A base built before the facts reads n/a; the head's two runs show each fact per renderer, a
        # categorical fit that differs between runs lists each outcome with its run count, and numeric
        # facts compare medians only when both sides have them.
        base = perf.SideRuns([make_outcome(result=valid_result(checkpoint_memory="supported"),
                                           memory=[tagged_sample(0, 1, True)])])
        head = perf.SideRuns([
            atlas_outcome([atlas_facts(fit="no_headroom", growths=2, evictions=0),
                           atlas_facts("warm[0]", dim=256, packed=0, growths=0, fit="256", tile=(0, 0))]),
            atlas_outcome([atlas_facts(fit="evicted", growths=2, evictions=7),
                           atlas_facts("warm[0]", dim=256, packed=0, growths=0, fit="256", tile=(0, 0))])])
        rows = perf.comparison_rows("S9/default", base, head)
        expected = {
            "end main[1] glyph_atlas_dim (px)": "512",
            "end main[1] glyph_atlas_packed_pixels (px)": "4000",
            "end main[1] glyph_atlas_fit (outcome)": "evicted ×1; no_headroom ×1",
            "end main[1] glyph_atlas_growths (count)": "2",
            "end main[1] glyph_atlas_evictions (count)": "0 ×1; 7 ×1",
            "end main[1] glyph_atlas_max_tile (px)": "25x16",
            "end warm[0] glyph_atlas_fit (outcome)": "256",
        }
        for metric, head_cell in expected.items():
            with self.subTest(metric=metric):
                # A numeric fact's change is n/a without a base figure; a categorical fact has no change.
                change = "" if "_fit " in metric or "_max_tile " in metric else "n/a"
                self.assertEqual(row_for(rows, metric)[2:5], ["n/a", head_cell, change])
        both = perf.comparison_rows("S9/default", perf.SideRuns([atlas_outcome([atlas_facts(dim=256)])]),
                                    perf.SideRuns([atlas_outcome([atlas_facts(dim=512, fit="does_not_fit")])]))
        self.assertEqual(row_for(both, "end main[1] glyph_atlas_dim (px)")[2:5], ["256", "512", "+100.0%"])
        self.assertEqual(row_for(both, "end main[1] glyph_atlas_fit (outcome)")[2:5], ["512", "does_not_fit", ""])

    def test_a_run_without_atlas_facts_adds_no_atlas_rows(self):
        # Neither side reports facts, so the table keeps its existing rows only.
        side = perf.SideRuns([make_outcome(result=valid_result(checkpoint_memory="supported"),
                                           memory=[tagged_sample(0, 1, True)])])
        rows = perf.comparison_rows("S1/default", side, side)
        self.assertFalse([row for row in rows if "glyph_atlas" in row[1]], rows)

    def test_rows_aggregate_each_window_by_its_logical_identity_across_runs_and_sides(self):
        # A native window id (an object address on macOS, an HWND on Windows) differs in every run, so rows
        # group the run's main window as `main` through the checkpoint reading's `main_window`, keep the warm
        # pool slot, and list each run's native id as evidence; a run without a reading never guesses `main`.
        def run(native, dim):
            reading = {"attempt": 1, "main_window": native, "counted_glyph_atlas_growths": None,
                       "closed_glyph_atlas_growths": None}
            return atlas_outcome([atlas_facts(f"visible[{native}]", dim=dim),
                                  atlas_facts("warm[0]", dim=256, fit="256")], readings=[reading])
        base = perf.SideRuns([run("WindowId(0x1a)", 256), run("WindowId(0x2b)", 256)])
        head = perf.SideRuns([run("WindowId(0x3c)", 512), run("WindowId(0x4d)", 512)])
        rows = perf.comparison_rows("S9/default", base, head)
        self.assertEqual(row_for(rows, "end main glyph_atlas_dim (px)")[2:5], ["256", "512", "+100.0%"])
        self.assertEqual(row_for(rows, "end main renderer_native_id (id)")[2:5],
                         ["WindowId(0x1a) ×1; WindowId(0x2b) ×1", "WindowId(0x3c) ×1; WindowId(0x4d) ×1", ""])
        self.assertEqual(row_for(rows, "end warm[0] glyph_atlas_dim (px)")[2:4], ["256", "256"])
        self.assertFalse([row for row in rows if "WindowId(" in row[1]], "a native id never keys a row")
        unread = perf.SideRuns([atlas_outcome([atlas_facts("visible[WindowId(0x5e)]", dim=512)])])
        unread_rows = perf.comparison_rows("S9/default", unread, unread)
        self.assertEqual(row_for(unread_rows, "end visible#1 glyph_atlas_dim (px)")[2:4], ["512", "512"])

    def test_reconciliation_compares_each_windows_snapshot_and_counted_growths_at_the_same_sample(self):
        # Each live window's snapshot growths must equal the growths its own counters recorded at the same
        # sample. Dropped counts and duplicates that startup growth would mask under an inequality both read
        # as mismatches; a closed window's counted growths are listed apart, so retiring one is consistent.
        main = "WindowId(0x1a)"

        def run(snapshot, counted, phase_growths, closed=0):
            reading = {"attempt": 1, "main_window": main, "counted_glyph_atlas_growths": counted,
                       "closed_glyph_atlas_growths": closed}
            return atlas_outcome([atlas_facts(f"visible[{main}]", growths=snapshot),
                                  atlas_facts("warm[0]", growths=0)], phase_growths, readings=[reading])
        # Every growth went uncounted: the phases count 0, which an inequality would call consistent.
        missing = run(2, {main: 0}, [0])
        # Two startup growths outside any phase, then one phase growth counted twice: the phases count 2,
        # below the snapshot's 3, while the window's own lifetime count is 4.
        duplicated = run(3, {main: 4}, [2])
        # A closed window grew 3 times; the phases count 4 across both windows, the snapshot holds only main.
        retired = run(1, {main: 1}, [4], closed=3)
        rows = perf.glyph_atlas_reconciliation_rows(
            "S9/default", perf.SideRuns([missing, duplicated]), perf.SideRuns([retired]))
        self.assertEqual(rows, [
            ["S9/default", "end", "glyph_atlas_growths, snapshot/counted",
             "mismatch in 2 of 2 runs: main 2/0; main 3/4", "consistent: main 1/1, closed 3", ""]])

    def test_reconciliation_without_a_per_window_reading_is_inconclusive_with_both_figures(self):
        # Without a same-sample per-window count only the summed figures exist, and an inequality between them
        # proves nothing either way, so the run is inconclusive and shows both; so is a visible window the
        # reading did not count. Without counters, facts or a base there is nothing to reconcile.
        no_reading = atlas_outcome([atlas_facts(growths=3)], [1, 2])
        uncounted = atlas_outcome(
            [atlas_facts("visible[WindowId(0x1a)]", growths=2)], [2],
            readings=[{"attempt": 1, "main_window": "WindowId(0x1a)", "counted_glyph_atlas_growths": {},
                       "closed_glyph_atlas_growths": 0}])
        rows = perf.glyph_atlas_reconciliation_rows(
            "S9/default", perf.SideRuns([no_reading]), perf.SideRuns([uncounted]))
        self.assertEqual(rows, [
            ["S9/default", "end", "glyph_atlas_growths, snapshot/counted",
             "inconclusive in 1 of 1 runs: snapshot 3, counted 3", "inconclusive in 1 of 1 runs: main 2/none", ""]])
        no_counters = atlas_outcome([atlas_facts(growths=2)])
        no_facts = atlas_outcome([], [1])
        self.assertEqual(perf.glyph_atlas_reconciliation_rows(
            "S9/default", perf.SideRuns([no_counters]), perf.SideRuns([no_facts])), [])
        head_only = perf.SideRuns(blocked=perf.COUNTERS_HEAD_ONLY)
        self.assertEqual(perf.glyph_atlas_reconciliation_rows("S9/default", head_only, perf.SideRuns([no_reading])),
                         [["S9/default", "end", "glyph_atlas_growths, snapshot/counted", "n/a",
                           "inconclusive in 1 of 1 runs: snapshot 3, counted 3", ""]])

    def test_a_malformed_atlas_reading_is_a_schema_problem(self):
        # A reading names its attempt, the main window's label or null, and per-window and closed counts or
        # null; anything else would let the report compare a figure the harness never recorded.
        good = {"attempt": 1, "main_window": "WindowId(1)", "counted_glyph_atlas_growths": {"WindowId(1)": 2},
                "closed_glyph_atlas_growths": 0}
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None, "atlas_readings": [good]}
        self.assertEqual(perf.validate_result(valid_result(checkpoints=[point]), HARNESS_HASH, 0), [])
        for bad in ({**good, "attempt": "1"}, {**good, "main_window": 3},
                    {**good, "counted_glyph_atlas_growths": {"WindowId(1)": -1}},
                    {**good, "closed_glyph_atlas_growths": 1.5}, "reading"):
            with self.subTest(bad=bad):
                self.assertTrue(perf.validate_result(
                    valid_result(checkpoints=[{**point, "atlas_readings": [bad]}]), HARNESS_HASH, 0))


class CheckpointFixtureTests(unittest.TestCase):
    """The harness's golden runs read as the harness meant them: exhausted, partial, or unsupported."""

    # Each late-exhaustion case and the attempts its record and authoritative sample must carry.
    LATE_CASES = {"eight-attempts": 8, "stale-clock": 1, "single-at-600": 1, "deadline-500": 5,
                  "deadline-610": 5}

    def test_every_fixture_line_parses_as_a_memory_sample(self):
        # The golden lines are what the harness's App wrote; each must survive `parse_memory_line`, so a
        # fixture whose lines the parser drops cannot pass by reading fewer samples.
        fixture = json.loads(CHECKPOINT_FIXTURE.read_text(encoding="utf-8"))
        for build, cases in fixture.items():
            for case, run in cases.items():
                for line in run["logs"]:
                    with self.subTest(build=build, case=case, line=line[:60]):
                        self.assertIn(perf.MEMORY_MARKER, line)
                        self.assertIsNotNone(perf.parse_memory_line(line))

    def test_late_exhaustion_reads_the_last_partial_attempt(self):
        # Each case's result passes the schema and records exhausted sampling with its attempt count
        # and a partial last attempt; the authoritative sample is that attempt, marked partial, never
        # the untagged periodic line logged before it, and its cells count as partial.
        for case, attempts in self.LATE_CASES.items():
            with self.subTest(case=case):
                result, samples = fixture_run("supported", case)
                self.assertEqual(perf.validate_result(result, HARNESS_HASH, 0), [])
                self.assertEqual(result["checkpoint_memory"], "supported")
                point = result["checkpoints"][0]
                self.assertEqual((point["sampling"], point["attempts"], point["last_attempt_complete"]),
                                 ("exhausted", attempts, False))
                self.assertTrue(any(sample.checkpoint_index is None for sample in samples))
                reading = perf.checkpoint_memory(samples, 0)
                self.assertEqual((reading.sample.checkpoint_attempt, reading.partial, reading.problem),
                                 (attempts, True, None))
                metrics = perf.run_metrics(fixture_outcome("supported", case))
                self.assertIsInstance(metrics[("end renderer_total_bytes", "MiB", "run")], perf.PartialValue)
                side = perf.SideRuns([fixture_outcome("supported", case)])
                row = row_for(perf.comparison_rows("S1/default", side, side), "end renderer_total_bytes (MiB)")
                self.assertTrue(row[3].endswith(", 1 partial"), row)

    def test_a_build_without_the_hook_reads_n_a_unsupported_beside_a_hooked_head(self):
        # The unsupported run's result comes from the no-feature build's serializer: its checkpoint has
        # no sampling record, and its rows read `n/a: unsupported` though a periodic sample was logged.
        result, samples = fixture_run("unsupported", "unsupported")
        self.assertEqual(perf.validate_result(result, HARNESS_HASH, 0), [])
        self.assertEqual(result["checkpoint_memory"], "unsupported")
        self.assertNotIn("sampling", result["checkpoints"][0])
        self.assertTrue(samples)
        base = perf.SideRuns([fixture_outcome("unsupported", "unsupported")])
        head = perf.SideRuns([fixture_outcome("supported", "deadline-610")])
        row = row_for(perf.comparison_rows("S1/default", base, head), "end renderer_total_bytes (MiB)")
        self.assertEqual(row[2], "n/a: unsupported")
        self.assertTrue(row[3].endswith(", 1 partial"), row)


class CheckpointConflictTests(unittest.TestCase):
    """Every complete attempt is checked for conflicts before the authoritative sample is chosen."""

    def test_a_later_attempt_does_not_hide_an_earlier_conflict(self):
        # Attempt 1 has two complete samples with different totals (16 and 17 MiB); attempt 2 has one
        # complete sample (18 MiB). Choosing attempt 2 would hide the conflict, so the index conflicts.
        samples = [tagged_sample(0, 1, True, 16), tagged_sample(0, 1, True, 17), tagged_sample(0, 2, True, 18)]
        reading = perf.checkpoint_memory(samples, 0)
        self.assertEqual((reading.sample, reading.problem), (None, "conflicting samples"))

    def test_identical_complete_duplicates_still_count_once(self):
        # The same reading twice in attempt 1, then attempt 2: no conflict, attempt 2 wins.
        samples = [tagged_sample(0, 1, True, 16), tagged_sample(0, 1, True, 16, unix_s=61.0),
                   tagged_sample(0, 2, True, 18)]
        reading = perf.checkpoint_memory(samples, 0)
        self.assertIsNone(reading.problem)
        self.assertEqual(reading.sample.checkpoint_attempt, 2)


class HookSourceScanTests(unittest.TestCase):
    """Hook detection matches only code: comments and string literals of every kind are blanked."""

    METHOD = "pub fn __perf_checkpoint_memory(&mut self) {}"

    def detects(self, source):
        return bool(perf.HOOK_METHODS[perf.CHECKPOINT_MEMORY_FEATURE].search(perf.rust_code_only(source)))

    def test_the_method_only_in_comments_or_literals_is_not_detected(self):
        cases = {
            "block comment": f"/*\n{self.METHOD}\n*/\nfn other() {{}}\n",
            "nested block comment": f"/* outer /* inner */\n{self.METHOD}\n*/\n",
            "multi-line string": f'const TEXT: &str = "first line\n{self.METHOD}\n";\n',
            "raw string": f'const TEXT: &str = r#"\n{self.METHOD}\n"#;\n',
            "byte string": f'const TEXT: &[u8] = b"\n{self.METHOD}\n";\n',
            "doc line": f"/// {self.METHOD}\nfn other() {{}}\n",
            "line comment": f"// {self.METHOD}\nfn other() {{}}\n",
        }
        for name, source in cases.items():
            with self.subTest(name=name):
                self.assertFalse(self.detects(source), source)

    def test_the_real_method_is_detected(self):
        source = f"impl App {{\n    #[doc(hidden)]\n    {self.METHOD}\n}}\n"
        self.assertTrue(self.detects(source))
        # Text after a closed comment or string on an earlier line is code again.
        self.assertTrue(self.detects(f'/* note */\nconst TEXT: &str = "\\"done";\n{self.METHOD}\n'))

    def test_the_blanker_keeps_code_lifetimes_and_line_breaks(self):
        # Every position is kept, a quote inside a character literal does not open a string, and a
        # lifetime is code.
        source = 'let quote = \'"\'; fn keep<\'a>(text: &\'a str) {} // tail\nlet done = 1;'
        blanked = perf.rust_code_only(source)
        self.assertEqual(len(blanked), len(source))
        self.assertEqual(blanked.count("\n"), source.count("\n"))
        self.assertIn("fn keep<'a>(text: &'a str) {}", blanked)
        self.assertIn("let done = 1;", blanked)
        self.assertNotIn("tail", blanked)


class CheckpointMemoryFeatureTests(PrebuiltHarness, unittest.TestCase):
    """perf-hook-checkpoint-memory reaches only a tree that defines the hook, through the gate's reviewed steps."""

    def build_step(self, gate, side):
        return next(step for step in gate.steps if step.id == f"build-{side}-perf_scenarios")

    def test_a_tree_needs_the_declaration_and_the_method(self):
        # The feature alone, or the method only in a comment, does not make a tree hook-capable.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / perf.APP_MANIFEST).parent.mkdir(parents=True)
            (root / perf.APP_MANIFEST).write_text(HOOK_MANIFEST, encoding="utf-8")
            hook = root / perf.APP_SOURCE_DIRECTORY / "app" / "memory_snapshot.rs"
            hook.parent.mkdir(parents=True)
            method = perf.HOOK_METHODS[perf.CHECKPOINT_MEMORY_FEATURE]
            self.assertFalse(perf.tree_supports_hook(root, perf.CHECKPOINT_MEMORY_FEATURE, method))
            hook.write_text("// pub fn __perf_checkpoint_memory is coming\n", encoding="utf-8")
            self.assertFalse(perf.tree_supports_hook(root, perf.CHECKPOINT_MEMORY_FEATURE, method))
            hook.write_text(HOOK_SOURCE, encoding="utf-8")
            self.assertTrue(perf.tree_supports_hook(root, perf.CHECKPOINT_MEMORY_FEATURE, method))
            (root / perf.APP_MANIFEST).write_text(HEAD_MANIFEST, encoding="utf-8")
            self.assertFalse(perf.tree_supports_hook(root, perf.CHECKPOINT_MEMORY_FEATURE, method))

    def test_the_head_builds_with_the_hook_and_the_base_without_it(self):
        # Each side's build step is the gate's own catalog entry for exactly the features it supports.
        # Each side's run is the golden run its build writes, so the base's `unsupported` comes from
        # the no-feature serializer and the head's `supported` from the hooked one.
        code, gate, _calls, _plans, _work, out = self.compare(
            head_manifest=COUNTERS_HOOK_MANIFEST, hook_trees=("head",),
            base_run=lambda plan: fixture_outcome("unsupported", "unsupported", plan),
            head_run=lambda plan: fixture_outcome("supported", "deadline-610", plan))
        self.assertEqual(code, perf.EXIT_PASS)
        # Both fixture trees define S1/atlas-retry's methods, so each side builds its features with that cfg on.
        catalog = {features: steps for (cfgs, features), steps in REAL_GATE.PERF_CFG_BUILDS.items()
                   if cfgs == ("perf_atlas_retry_api",)}
        hooked = ("perf-counters", perf.CHECKPOINT_MEMORY_FEATURE)
        self.assertIs(self.build_step(gate, "head"), catalog[hooked]["build-head-perf_scenarios"])
        self.assertIs(self.build_step(gate, "base"), catalog[()]["build-base-perf_scenarios"])
        self.assertEqual(REAL_GATE.PERF_BUILD_CATALOG[("build-head-perf_scenarios", hooked)].argv,
                         self.build_step(gate, "head").argv)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("- Built with `--features perf-hook-checkpoint-memory`: head", document)
        row = next(line for line in document.splitlines() if "end renderer_total_bytes (MiB)" in line)
        cells = [cell.strip() for cell in row.strip("|").split("|")]
        self.assertEqual(cells[2], "n/a: unsupported", row)
        self.assertTrue(cells[3].endswith(", 1 partial"), row)

    def test_a_declaration_without_the_method_builds_without_the_feature(self):
        _code, gate, *_rest = self.compare(head_manifest=HOOK_MANIFEST)
        self.assertNotIn("--features", self.build_step(gate, "head").argv)

    def test_the_manifest_records_each_sides_features_and_a_mismatch_fails(self):
        binaries, digest = self.produce(head_manifest=HOOK_MANIFEST, hook_trees=("head",))
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["features"], {"base": [], "head": [perf.CHECKPOINT_MEMORY_FEATURE]})
        with self.assertRaisesRegex(ValueError, "refusing the prebuilt binaries: features"):
            self.consume(binaries, digest, head_manifest=HOOK_MANIFEST)
        code, *_rest = self.consume(binaries, digest, head_manifest=HOOK_MANIFEST, hook_trees=("head",))
        self.assertEqual(code, perf.EXIT_PASS)

    def test_the_gate_rejects_a_lookalike_build_step(self):
        # A copy of a reviewed step with the same id and argv is not the gate's own step.
        hooked = ("perf-counters", perf.CHECKPOINT_MEMORY_FEATURE)
        step = REAL_GATE.PERF_FEATURE_BUILDS[hooked]["build-head-perf_scenarios"]
        self.assertTrue(REAL_GATE._reviewed_step(step))
        self.assertFalse(REAL_GATE._reviewed_step(copy.copy(step)))

    def test_both_scripts_list_the_same_perf_features(self):
        self.assertEqual(perf.PERF_FEATURES, REAL_GATE.PERF_FEATURES)



CELL_MIB = 1048576


def cell_layout_run(name, grid_bytes, resident_bytes, workflow_run="1", panes=(3, 3, 0), supported=True):
    """A head S12 run whose `end` checkpoint read `grid_bytes` of grid and `resident_bytes` resident."""
    total, sampled, contended = panes
    sample = perf.MemorySample(60.0, resident_bytes, 16 * CELL_MIB, 0, checkpoint_index=2, checkpoint_label="end",
                               checkpoint_attempt=1, checkpoint_complete=True, panes_total=total,
                               panes_sampled=sampled, panes_contended=contended, grid_visible_bytes=grid_bytes,
                               grid_history_bytes=0, grid_alternate_bytes=0)
    result = {"checkpoint_memory": "supported" if supported else "unsupported",
              "checkpoints": [{"index": 2, "label": "end", "sampling": "complete", "attempts": 1,
                               "last_attempt_complete": True}]}
    return perf.CellLayoutRun(name, workflow_run, result, [sample] if supported else [])


def cell_layout_runs(pairs, prefix="run"):
    """Five (or however many) runs from (grid MiB, resident MiB) pairs."""
    return [cell_layout_run(f"{prefix}{index}", int(grid * CELL_MIB), int(resident * CELL_MIB))
            for index, (grid, resident) in enumerate(pairs)]


PASSING = [(30, 100)] * 5
FAILING = [(6, 100)] * 5


class CellLayoutBoundaryTests(unittest.TestCase):
    """Both clauses are inclusive and exact: 10G = 3R passes the share clause, one byte less fails; G = 24 MiB
    passes the size clause (8 MiB saved), one byte less fails."""

    def platform(self, grid_bytes, resident_bytes):
        runs = [cell_layout_run(f"run{index}", grid_bytes, resident_bytes) for index in range(5)]
        return perf.cell_layout_platform("macOS", runs)

    def test_the_share_clause_is_inclusive_and_exact(self):
        # 10G = 3R exactly passes; with 3R % 10 == 1, G = (3R - 1) / 10 is the nearest integer pair that fails.
        resident = 100 * CELL_MIB
        self.assertNotIn("share", self.platform(3 * resident // 10, resident).failed_clauses)
        odd_resident = 100 * CELL_MIB + 7
        self.assertEqual(3 * odd_resident % 10, 1)
        self.assertIn("share", self.platform((3 * odd_resident - 1) // 10, odd_resident).failed_clauses)

    def test_the_size_clause_is_inclusive_and_exact(self):
        self.assertNotIn("size", self.platform(24 * CELL_MIB, 50 * CELL_MIB).failed_clauses)
        self.assertIn("size", self.platform(24 * CELL_MIB - 1, 50 * CELL_MIB).failed_clauses)


class CellLayoutAggregationTests(unittest.TestCase):
    """The platform statistic is the median of per-run ratios, never a ratio of medians, and one outlier does
    not move it."""

    def test_the_median_of_ratios_not_the_ratio_of_medians(self):
        # Per-run shares 30%, 15%, 15%, 1%, 1%: median 15% fails, although median G / median R = 30 / 100 passes.
        judged = perf.cell_layout_platform("macOS", cell_layout_runs([(30, 100), (30, 200), (30, 200),
                                                                      (1, 100), (1, 100)]))
        self.assertIn("share", judged.failed_clauses)
        self.assertFalse(judged.passes)

    def test_an_outlier_does_not_change_the_result(self):
        steady = perf.cell_layout_platform("macOS", cell_layout_runs(FAILING))
        outlier = perf.cell_layout_platform("macOS", cell_layout_runs([(60, 100)] + FAILING[1:]))
        self.assertEqual((steady.passes, steady.share), (outlier.passes, outlier.share))


class CellLayoutOutcomeTests(unittest.TestCase):
    """Go needs one platform passing both clauses; NoGo needs both platforms decidable and failing; anything
    missing is Inconclusive and names the gap, never a NoGo."""

    def test_both_clauses_must_hold_on_the_same_platform(self):
        share_only = cell_layout_runs([(6, 10)] * 5)       # 60% share, 2 MiB saving
        size_only = cell_layout_runs([(30, 1000)] * 5)     # 3% share, 10 MiB saving
        self.assertEqual(perf.cell_layout_decision({"macOS": share_only, "Windows": size_only}).outcome, "NoGo")
        both = cell_layout_runs(PASSING)
        self.assertEqual(perf.cell_layout_decision({"macOS": both, "Windows": size_only}).outcome, "Go")

    def test_missing_runs_are_inconclusive_unless_the_other_platform_passes(self):
        decision = perf.cell_layout_decision({"macOS": cell_layout_runs(FAILING[:4]),
                                              "Windows": cell_layout_runs(FAILING)})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertIn("macOS: 4 valid runs, need exactly 5", decision.reasons)
        other_passes = perf.cell_layout_decision({"macOS": cell_layout_runs(FAILING[:4]),
                                                  "Windows": cell_layout_runs(PASSING)})
        self.assertEqual(other_passes.outcome, "Go")

    def test_runs_from_two_workflow_runs_are_never_pooled(self):
        mixed = cell_layout_runs(FAILING[:3]) + [cell_layout_run(f"late{index}", 6 * CELL_MIB, 100 * CELL_MIB,
                                                                 workflow_run="2") for index in range(3)]
        decision = perf.cell_layout_decision({"macOS": mixed, "Windows": cell_layout_runs(FAILING)})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertTrue(any("2 workflow runs" in reason for reason in decision.reasons))

    def test_invalid_inputs_name_their_reason(self):
        cases = {"panes_contended=1": dict(panes=(3, 3, 1)), "panes_sampled=2": dict(panes=(3, 2, 0)),
                 "panes_total=4": dict(panes=(4, 4, 0))}
        for reason, overrides in cases.items():
            run = cell_layout_run("bad", 6 * CELL_MIB, 100 * CELL_MIB, **overrides)
            self.assertEqual(perf.cell_layout_reading(run).problem, reason)
        self.assertEqual(perf.cell_layout_reading(cell_layout_run("zero", 6 * CELL_MIB, 0)).problem,
                         "process_resident_bytes=0")

    def test_unsupported_resident_and_a_missing_grid_field_are_inconclusive_not_nogo(self):
        unsupported = cell_layout_run("unsupported", 6 * CELL_MIB, None)
        self.assertEqual(perf.cell_layout_reading(unsupported).problem, "process_resident_bytes unsupported")
        base = cell_layout_run("no-history", 6 * CELL_MIB, 100 * CELL_MIB)
        sample = base.memory[0]
        no_history = perf.CellLayoutRun("no-history", "1", base.result, [perf.MemorySample(
            sample.unix_s, sample.process_resident_bytes, sample.renderer_total_bytes, 0, checkpoint_index=2,
            checkpoint_label="end", checkpoint_attempt=1, checkpoint_complete=True, panes_total=3, panes_sampled=3,
            panes_contended=0, grid_visible_bytes=6 * CELL_MIB, grid_history_bytes=None, grid_alternate_bytes=0)])
        self.assertEqual(perf.cell_layout_reading(no_history).problem, "missing grid_history_bytes")
        for bad in (unsupported, no_history):
            decision = perf.cell_layout_decision({"macOS": cell_layout_runs(FAILING[:4]) + [bad],
                                                  "Windows": cell_layout_runs(FAILING)})
            self.assertEqual(decision.outcome, "Inconclusive")
            self.assertTrue(any(bad.name in reason for reason in decision.reasons))

    def test_a_rejected_replacement_is_named_and_not_counted(self):
        focus = perf.CellLayoutRun("focus", "1", cell_layout_run("focus", 6 * CELL_MIB, 100 * CELL_MIB).result, [],
                                   classification="focus")
        judged = perf.cell_layout_platform("macOS", cell_layout_runs(FAILING) + [focus])
        self.assertFalse(judged.passes)
        self.assertEqual(sum(reading.problem is None for reading in judged.readings), 5)
        self.assertIn("macOS focus: run classified focus", judged.reasons)

    def test_the_same_run_counted_five_times_is_not_five_runs(self):
        same = cell_layout_run("only", 30 * CELL_MIB, 100 * CELL_MIB)
        decision = perf.cell_layout_decision({"macOS": [same] * 5, "Windows": cell_layout_runs(FAILING)})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertTrue(any("runs repeated" in reason for reason in decision.reasons))

    def test_platforms_from_two_workflow_runs_never_make_a_nogo(self):
        windows = [cell_layout_run(f"win{index}", 6 * CELL_MIB, 100 * CELL_MIB, workflow_run="2")
                   for index in range(5)]
        decision = perf.cell_layout_decision({"macOS": cell_layout_runs(FAILING), "Windows": windows})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertTrue(any("different workflow runs" in reason for reason in decision.reasons))
        # A platform whose own five runs pass still decides a go.
        passing = perf.cell_layout_decision({"macOS": cell_layout_runs(PASSING), "Windows": windows})
        self.assertEqual(passing.outcome, "Go")


class CellLayoutHeadOnlyTests(unittest.TestCase):
    """The decision reads the head alone: a head without checkpoint sampling is Inconclusive, never NoGo."""

    def test_unsupported_head_sampling_is_inconclusive(self):
        unsupported = [cell_layout_run(f"run{index}", 0, 0, supported=False) for index in range(5)]
        decision = perf.cell_layout_decision({"macOS": unsupported, "Windows": unsupported})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertIn("checkpoint sampling unsupported", decision.reasons[0])


def cell_line(index, label, attempt, complete, grid_history_bytes, resident_bytes=100 * CELL_MIB):
    """A tagged memory line as the App writes it, for checkpoint `index`."""
    return memory_line(resident=str(resident_bytes), grid=(
        f"grid_visible_bytes=0 grid_history_bytes={grid_history_bytes} grid_alternate_bytes=0 panes_total=3 "
        f"panes_sampled=3 panes_contended=0 checkpoint_index={index} checkpoint_label=\"{label}\" "
        f"checkpoint_attempt={attempt} checkpoint_complete={'true' if complete else 'false'} "))


class CellLayoutParsedLineTests(unittest.TestCase):
    """Retried, exhausted, repeated, conflicting and earlier-checkpoint samples reach the decision correctly, from
    parsed log lines, each beside four other valid runs."""

    def run_from(self, name, lines, sampling="complete"):
        memory = [sample for sample in map(perf.parse_memory_line, lines) if sample]
        result = {"checkpoint_memory": "supported",
                  "checkpoints": [{"index": 0, "label": "settled", "sampling": "complete"},
                                  {"index": 1, "label": "covered", "sampling": "complete"},
                                  {"index": 2, "label": "end", "sampling": sampling}]}
        return perf.CellLayoutRun(name, "1", result, memory)

    def decide(self, candidate):
        """The decision with `candidate` as macOS's fifth run and five failing Windows runs."""
        others = [self.run_from(f"ok{index}", [cell_line(2, "end", 1, True, 7)]) for index in range(4)]
        decision = perf.cell_layout_decision({"macOS": others + [candidate], "Windows": cell_layout_runs(FAILING)})
        valid = sum(reading.problem is None for reading in decision.platforms["macOS"].readings)
        return decision, valid

    def test_a_retried_end_uses_its_complete_attempt(self):
        decision, valid = self.decide(self.run_from(
            "retried", [cell_line(2, "end", 1, False, 1), cell_line(2, "end", 2, True, 7)]))
        self.assertEqual((decision.outcome, valid), ("NoGo", 5))
        # The candidate contributes attempt 2's grid bytes, not attempt 1's.
        candidate = decision.platforms["macOS"].readings[-1]
        self.assertEqual((candidate.name, candidate.grid_bytes, candidate.resident_bytes),
                         ("retried", 7, 100 * CELL_MIB))

    def test_exhausted_sampling_is_inconclusive_not_nogo(self):
        decision, valid = self.decide(self.run_from(
            "exhausted", [cell_line(2, "end", attempt, False, 7) for attempt in range(1, 9)], sampling="exhausted"))
        self.assertEqual((decision.outcome, valid), ("Inconclusive", 4))
        self.assertTrue(any("exhausted: partial" in reason for reason in decision.reasons))

    def test_a_repeated_end_line_counts_once(self):
        decision, valid = self.decide(self.run_from("repeated", [cell_line(2, "end", 1, True, 7)] * 2))
        self.assertEqual((decision.outcome, valid), ("NoGo", 5))

    def test_conflicting_end_samples_make_the_run_invalid(self):
        decision, valid = self.decide(self.run_from(
            "conflict", [cell_line(2, "end", 1, True, 7), cell_line(2, "end", 1, True, 8)]))
        self.assertEqual((decision.outcome, valid), ("Inconclusive", 4))
        self.assertTrue(any("conflict: conflicting samples" in reason for reason in decision.reasons))

    def test_only_the_end_checkpoint_is_read(self):
        # Huge settled and covered samples would pass both clauses; only end's small one may count.
        lines = [cell_line(0, "settled", 1, True, 90 * CELL_MIB), cell_line(1, "covered", 1, True, 90 * CELL_MIB),
                 cell_line(2, "end", 1, True, 7)]
        decision, valid = self.decide(self.run_from("earlier", lines))
        self.assertEqual((decision.outcome, valid), ("NoGo", 5))
        self.assertEqual(decision.platforms["macOS"].readings[-1].grid_bytes, 7)


CELL_HEAD = "a" * 40


def write_cell_artifact(root, platform, runs, shard="S1-S3-S6-S8-S12", head=CELL_HEAD, run_id="1"):
    """A `perf-comparison-*` artifact holding head S12 runs; each run is (name, outcome dict or raw text)."""
    artifact = root / f"perf-comparison-1586-{head}-{platform}-{shard}-1"
    (artifact / "runs" / "S12-default" / "timed").mkdir(parents=True)
    (artifact / "timing.json").write_text(json.dumps({"run_id": run_id, "shard": shard}), encoding="utf-8")
    for name, outcome in runs:
        run_dir = artifact / "runs" / "S12-default" / "timed" / name
        (run_dir / "scratch").mkdir(parents=True)
        text = outcome if isinstance(outcome, str) else json.dumps(outcome)
        (run_dir / "outcome.json").write_text(text, encoding="utf-8")
        (run_dir / "scratch" / "result.json").write_text(json.dumps(
            {"checkpoint_memory": "supported", "checkpoints": [{"index": 2, "label": "end", "sampling": "complete"}]}),
            encoding="utf-8")
        (run_dir / "01-harness.log").write_text(
            "\x1b[2m" + cell_line(2, "end", 1, True, 7) + "\x1b[0m\n", encoding="utf-8")
    return artifact


HEAD_OUTCOME = {"kind": "valid", "side": "head", "scenario": "S12", "variant": "default"}


class CellLayoutArtifactTests(unittest.TestCase):
    """The reader takes only the requested workflow run's S1-S3-S6-S8-S12 head runs, names evidence from anything
    else, and turns unreadable files into named invalid runs instead of failing."""

    def test_the_reader_keeps_provenance_and_names_foreign_evidence(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            write_cell_artifact(root, "macOS", [(f"0{index}-head", HEAD_OUTCOME) for index in range(1, 6)])
            write_cell_artifact(root, "Windows", [("01-head", HEAD_OUTCOME)], shard="S7")
            write_cell_artifact(root, "Windows", [("01-head", HEAD_OUTCOME), ("02-head", HEAD_OUTCOME)], run_id="2")
            runs = perf.read_cell_layout_runs(root, "1", CELL_HEAD)
            self.assertEqual(len(runs["macOS"]), 5)
            self.assertTrue(all(run.problem is None for run in runs["macOS"]))
            self.assertEqual(perf.cell_layout_reading(runs["macOS"][0]).grid_bytes, 7)
            # The S7 artifact is not read; the other workflow run's is read but named.
            self.assertEqual(len(runs["Windows"]), 2)
            self.assertTrue(all("workflow run 2" in run.problem for run in runs["Windows"]))

    def test_another_head_side_scenario_or_unreadable_file_is_a_named_invalid_run(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            write_cell_artifact(root, "macOS", [("01-head", dict(HEAD_OUTCOME, side="base")),
                                                ("02-head", dict(HEAD_OUTCOME, scenario="S7")),
                                                ("03-head", "{not json"),
                                                ("04-head", "[1, 2]")])
            write_cell_artifact(root, "Windows", [("01-head", HEAD_OUTCOME)], head="b" * 40)
            runs = perf.read_cell_layout_runs(root, "1", CELL_HEAD)
            problems = [run.problem for run in runs["macOS"]]
            self.assertIn("side=base", problems[0])
            self.assertIn("scenario=S7", problems[1])
            self.assertIn("outcome.json unreadable", problems[2])
            self.assertEqual(problems[3], "outcome.json is not an object")
            self.assertIn("measured head " + "b" * 40, runs["Windows"][0].problem)
            decision = perf.cell_layout_decision(runs)
            self.assertEqual(decision.outcome, "Inconclusive")
            # Every rejected run is named: the four macOS runs and the Windows run of the other head.
            for run in runs["macOS"] + runs["Windows"]:
                self.assertTrue(any(run.name in reason for reason in decision.reasons), run.name)


class CellLayoutDamagedEvidenceTests(unittest.TestCase):
    """Damaged result, timing and log evidence becomes a named invalid run; collection and the other platform's
    decision continue."""

    def test_damaged_files_are_named_and_a_passing_platform_still_decides(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            macos = write_cell_artifact(root, "macOS", [(f"0{index}-head", HEAD_OUTCOME) for index in range(1, 6)])
            timed = macos / "runs" / "S12-default" / "timed"
            (timed / "01-head" / "scratch" / "result.json").write_text(
                json.dumps({"checkpoint_memory": "supported", "checkpoints": 1}), encoding="utf-8")
            (timed / "02-head" / "scratch" / "result.json").write_text("[1]", encoding="utf-8")
            (timed / "03-head" / "scratch" / "result.json").write_text("{broken", encoding="utf-8")
            (timed / "04-head" / "01-harness.log").write_text(
                cell_line(2, "end", 1, True, 7).replace(STAMP[:10], "2026-99-03"), encoding="utf-8")
            unreadable = timed / "05-head" / "01-harness.log"
            original_read_text = Path.read_text

            def refuse_one_log(path, *args, **kwargs):
                # A portable unreadable file: chmod cannot make one on Windows, so this one read is refused.
                if path == unreadable:
                    raise PermissionError(13, "Permission denied", str(path))
                return original_read_text(path, *args, **kwargs)

            with mock.patch.object(Path, "read_text", refuse_one_log):
                runs = perf.read_cell_layout_runs(root, "1", CELL_HEAD)
            problems = [perf.cell_layout_reading(run).problem for run in runs["macOS"]]
            self.assertEqual(problems[0], "result.json checkpoints is not a list")
            self.assertEqual(problems[1], "result.json is not an object")
            self.assertIn("result.json unreadable", problems[2])
            self.assertIn("memory log unreadable", problems[3])
            self.assertIn("memory log unreadable", problems[4])
            # Windows' own five passing runs still decide a go.
            windows = [cell_layout_run(f"win{index}", 30 * CELL_MIB, 100 * CELL_MIB, workflow_run="1")
                       for index in range(5)]
            windows = [perf.CellLayoutRun(run.name, run.workflow_run, run.result, run.memory, head_sha=CELL_HEAD)
                       for run in windows]
            self.assertEqual(perf.cell_layout_decision({"macOS": runs["macOS"], "Windows": windows}).outcome, "Go")

    def test_a_damaged_timing_file_names_every_run_of_its_artifact(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            artifact = write_cell_artifact(root, "macOS", [("01-head", HEAD_OUTCOME)])
            (artifact / "timing.json").write_text("{broken", encoding="utf-8")
            broken = perf.read_cell_layout_runs(root, "1", CELL_HEAD)["macOS"][0]
            self.assertIn("timing.json unreadable", broken.problem)
            (artifact / "timing.json").write_text("[1]", encoding="utf-8")
            self.assertEqual(perf.read_cell_layout_runs(root, "1", CELL_HEAD)["macOS"][0].problem,
                             "timing.json is not an object")


class CellLayoutUndecodableEvidenceTests(unittest.TestCase):
    """JSON that fails to decode for any reason, and an unavailable download, leave named or missing evidence instead
    of an exception."""

    def test_an_over_long_integer_and_deep_nesting_are_named(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            artifact = write_cell_artifact(root, "macOS", [("01-head", HEAD_OUTCOME), ("02-head", HEAD_OUTCOME)])
            timed = artifact / "runs" / "S12-default" / "timed"
            # Python refuses to convert an integer string past its default 4,300-digit limit (ValueError).
            (timed / "01-head" / "scratch" / "result.json").write_text('{"n": ' + "9" * 5000 + "}", encoding="utf-8")
            first = perf.read_cell_layout_runs(root, "1", CELL_HEAD)["macOS"][0]
            self.assertIn("result.json unreadable", first.problem)
            # Whether deep nesting overflows depends on the interpreter, so the RecursionError is injected.
            with mock.patch.object(perf.json, "loads", side_effect=RecursionError("maximum recursion depth")):
                nested = perf.read_cell_layout_runs(root, "1", CELL_HEAD)["macOS"]
            self.assertTrue(all("unreadable: maximum recursion depth" in run.problem for run in nested))

    def test_an_unavailable_download_is_missing_evidence(self):
        runs = perf.read_cell_layout_runs(Path(tempfile.gettempdir()) / "no-such-download-1586", "1", CELL_HEAD)
        self.assertEqual(runs, {"macOS": [], "Windows": []})
        self.assertEqual(perf.cell_layout_decision(runs).outcome, "Inconclusive")

class RowGlyphCacheReportTests(unittest.TestCase):
    """The row glyph cache's checkpoint field and the derived counter rows its evaluation reads."""

    LINE = ("2026-10-03T00:00:01Z  INFO memory: memory snapshot process_resident_bytes=10 "
            "session_total_bytes=3 renderer_total_bytes=7 renderer_row_glyph_cache_bytes=5 "
            "checkpoint_index=0 checkpoint_label=\"end\" checkpoint_attempt=1 checkpoint_complete=true")

    def test_the_checkpoint_field_parses_and_an_older_line_lacks_it(self):
        # The field is optional: a line from an older build reads None and still parses.
        self.assertEqual(perf.parse_memory_line(self.LINE).renderer_row_glyph_cache_bytes, 5)
        self.assertIsNone(perf.parse_memory_line(memory_line()).renderer_row_glyph_cache_bytes)

    def test_complete_samples_differing_only_in_row_cache_bytes_conflict(self):
        # The field joins the totals a reading compares, so two complete samples of one attempt that agree
        # on every other total but differ in row-cache bytes are conflicting samples, not one reading.
        first = dataclasses.replace(tagged_sample(0, 1, True), renderer_row_glyph_cache_bytes=10)
        second = dataclasses.replace(tagged_sample(0, 1, True), renderer_row_glyph_cache_bytes=11)
        self.assertEqual(perf.checkpoint_memory([first, second], 0).problem, "conflicting samples")
        self.assertIsNone(perf.checkpoint_memory([first, first], 0).problem)

    def test_the_checkpoint_row_is_reported_n_a_on_an_older_base_and_partial_when_partial(self):
        # A base whose lines lack the field reads n/a for that row while its other totals still compare;
        # a reading from a partial attempt is marked partial, as renderer_total_bytes is.
        def outcome(cache_bytes, complete=True):
            out = timed_outcome([1.0])
            sample = dataclasses.replace(out.memory[0], renderer_row_glyph_cache_bytes=cache_bytes,
                                         checkpoint_complete=complete)
            return dataclasses.replace(out, memory=[sample])
        rows = perf.comparison_rows("S3/default", perf.SideRuns([outcome(None)]),
                                    perf.SideRuns([outcome(64 * 1048576)]))
        self.assertEqual(row_for(rows, "end renderer_row_glyph_cache_bytes (MiB)")[2:4],
                         ["n/a", "64.00 (64.00–64.00)"])
        self.assertEqual(row_for(rows, "end renderer_total_bytes (MiB)")[2], "100.00 (100.00–100.00)")
        partial = perf.comparison_rows("S3/default", perf.SideRuns([outcome(None)]),
                                       perf.SideRuns([outcome(64 * 1048576, complete=False)]))
        self.assertIn("partial", row_for(partial, "end renderer_row_glyph_cache_bytes (MiB)")[3])

    def test_derived_rows_name_their_denominators_and_pool_as_labelled(self):
        # The hit ratio pools every counters run; the assembly mean is exact per run (sum_us / samples)
        # with the pooled mean; shape requests per drawn frame and the fallback ratio are context rows.
        base = counters_side(
            {"renderer.row_cache_hits": 30, "renderer.row_cache_misses": 70, "renderer.shape_requests": 40,
             "renderer.gpu_frames": 10, "renderer.assembly_us": ([0, 0, 4, 0, 0, 0, 0], 400)},
            {"renderer.row_cache_hits": 10, "renderer.row_cache_misses": 90, "renderer.shape_requests": 60,
             "renderer.gpu_frames": 10, "renderer.assembly_us": ([0, 0, 2, 0, 0, 0, 0], 300)})
        head = counters_side(
            {"renderer.row_cache_hits": 90, "renderer.row_cache_misses": 10, "renderer.shape_requests": 10,
             "renderer.gpu_frames": 10, "renderer.partial_frames": 3, "renderer.partial_fallbacks": 1,
             "renderer.assembly_us": ([0, 4, 0, 0, 0, 0, 0], 200)})
        rows, _omitted = perf.counter_rows("S3/default", base, head)
        derived = {row[2]: row for row in rows}
        hit = derived["row-cache hit ratio = hits / (hits + misses), counters runs pooled"]
        self.assertEqual(hit[3:], ["0.200 (40/200, 2/2 runs)", "0.900 (90/100, 1/1 runs)", "+350.0%"])
        assembly = derived["assembly mean per counters run = assembly_sum_us / Σ assembly_buckets, per run"]
        self.assertEqual(assembly[3], "run 1 100.00 us, run 2 150.00 us; pooled 116.67 us (2/2 runs)")
        self.assertEqual(assembly[4], "run 1 50.00 us; pooled 50.00 us (1/1 runs)")
        shapes = derived["shape+measure requests per drawn frame = shape_requests / (gpu_frames + "
                         "software_frames), context only"]
        self.assertEqual(shapes[3:5], ["5.000 (100/20, 2/2 runs)", "1.000 (10/10, 1/1 runs)"])
        fallback = derived["partial fallback ratio = partial_fallbacks / (partial_frames + partial_fallbacks), "
                           "context only"]
        self.assertEqual(fallback[3:], ["n/a (denominator 0, 2/2 runs)", "0.250 (1/4, 1/1 runs)", "n/a"])

    def test_cache_reuse_rows_are_pooled_per_assembly_and_a_base_without_them_reads_na(self):
        # The title and chrome-run reuse rows divide the pooled reuses by the pooled assembly samples; a
        # base that predates the counters carries no field and reads n/a, while the head's figure stands.
        older = counters_result({"renderer.assembly_us": ([0, 0, 4, 0, 0, 0, 0], 400)})
        for phase in older["phases"]:
            # A base built before the counters existed reports none of them.
            for name in ("tab_title_reuses", "tab_title_prepares", "chrome_run_reuses", "chrome_run_prepares"):
                del phase["frame_counters"]["renderer"][name]
        base = perf.SideRuns(outcomes=[make_outcome(result=older)])
        head = counters_side(
            {"renderer.tab_title_reuses": 9, "renderer.chrome_run_reuses": 10,
             "renderer.assembly_us": ([0, 3, 0, 0, 0, 0, 0], 300)},
            {"renderer.tab_title_reuses": 3, "renderer.chrome_run_reuses": 0,
             "renderer.assembly_us": ([0, 1, 0, 0, 0, 0, 0], 100)})
        rows, _omitted = perf.counter_rows("S8/default", base, head)
        derived = {row[2]: row for row in rows}
        titles = derived["tab-title reuses per assembly = tab_title_reuses / Σ assembly_buckets, "
                         "counters runs pooled"]
        self.assertEqual(titles[4], "3.000 (12/4, 2/2 runs)")
        self.assertTrue(titles[3].startswith("n/a"), titles[3])
        runs = derived["chrome-run reuses per assembly = chrome_run_reuses / Σ assembly_buckets, "
                       "counters runs pooled"]
        self.assertEqual(runs[4], "2.500 (10/4, 2/2 runs)")
        idle = counters_side({"renderer.tab_title_reuses": 0, "renderer.assembly_us": ([0] * 7, 0)})
        rows, _omitted = perf.counter_rows("S8/default", idle, idle)
        idle_row = {row[2]: row for row in rows}.get(
            "tab-title reuses per assembly = tab_title_reuses / Σ assembly_buckets, counters runs pooled")
        self.assertIsNone(idle_row, "a phase that assembled nothing prints no reuse row")

    def test_assembly_means_keep_every_runs_position(self):
        # A run that assembled nothing and a run whose result lacks the histogram keep their positions
        # and read n/a; the pooled mean is still the sum over every sampled run's events.
        missing = counters_result({"renderer.assembly_us": ([0, 2, 0, 0, 0, 0, 0], 100)})
        del missing["phases"][0]["frame_counters"]["renderer"]["assembly_us"]
        side = perf.SideRuns(outcomes=[
            make_outcome(result=counters_result({"renderer.row_cache_hits": 1})),
            make_outcome(result=counters_result({"renderer.assembly_us": ([0, 4, 0, 0, 0, 0, 0], 200)})),
            make_outcome(result=missing),
            make_outcome(result=counters_result({"renderer.assembly_us": ([0, 1, 0, 0, 0, 0, 0], 100)}))])
        rows, _omitted = perf.counter_rows("S3/default", perf.SideRuns(blocked=perf.COUNTERS_HEAD_ONLY), side)
        assembly = [row for row in rows if row[2].startswith("assembly mean per counters run")]
        self.assertEqual(assembly[0][4], "run 1 n/a (no assembly), run 2 50.00 us, run 3 n/a (no histogram), "
                                         "run 4 100.00 us; pooled 60.00 us (2/4 runs)")
        quiet = perf.SideRuns(outcomes=[make_outcome(result=counters_result({}))])
        self.assertEqual(perf._assembly_means(perf._counter_phases(quiet)["workload"]),
                         ("run 1 n/a (no assembly); pooled n/a (0/1 runs)", None))

    def test_a_side_without_the_fields_reads_n_a_and_all_zero_rows_are_left_out(self):
        # A base lacking the counters reads n/a with no change; a phase where no side has a denominator
        # prints no derived row at all.
        older = counters_result({})
        del older["phases"][0]["frame_counters"]["renderer"]["row_cache_hits"]
        base = perf.SideRuns(outcomes=[make_outcome(result=older)])
        head = counters_side({"renderer.row_cache_hits": 3, "renderer.row_cache_misses": 1})
        rows, _omitted = perf.counter_rows("S7/default", base, head)
        hit = [row for row in rows if row[2].startswith("row-cache hit ratio")]
        self.assertEqual(hit[0][3:], ["n/a", "0.750 (3/4, 1/1 runs)", "n/a"])
        quiet, _omitted = perf.counter_rows("S1/default", counters_side({}), counters_side({}))
        self.assertFalse([row for row in quiet if "=" in row[2]])


def split_sample(parts=(2.0, 3.0, 5.0), **overrides):
    """A credited sample split into `parts` (ms), whose latency is their sum; overrides replace sample keys."""
    split = {"input_to_parse_ms": parts[0], "parse_to_publication_ms": parts[1],
             "publication_to_present_ms": parts[2], "delivery_lag_us": 40.0, "delivery": "sent",
             "coalesced": False, "sync_open": False, "echo_generation": 3}
    sample = {"inject_unix_s": 1.0, "latency_ms": sum(parts), "attributed": True, "reason": "credited",
              "split": split, "split_reason": "split"}
    sample.update(overrides)
    return sample


def reason_sample(split_reason, latency_ms=8.0):
    """A sample with no split: credited when `latency_ms` is a number, else uncredited."""
    return {"inject_unix_s": 2.0, "latency_ms": latency_ms, "attributed": latency_ms is not None,
            "reason": "credited" if latency_ms is not None else "no-candidate", "split": None,
            "split_reason": split_reason}


def split_latency(samples):
    """A schema-1 latency object whose aggregates are recomputed from `samples`, as the harness writes it."""
    credited = [sample for sample in samples if sample["latency_ms"] is not None]
    reasons = {}
    for sample in credited:
        reasons[sample["split_reason"]] = reasons.get(sample["split_reason"], 0) + 1
    split_count = reasons.get("split", 0)
    return {"samples": samples, "attributed": len(credited), "total": len(samples),
            "coverage": len(credited) / len(samples) if samples else 0.0, "split_schema": 1,
            "split_count": split_count, "split_reasons": reasons,
            "split_coverage": split_count / len(credited) if credited else None}


def split_problems(latency, schema=1):
    """What validate_result reports for a result carrying `latency`, under the harness's split schema."""
    return perf.validate_result(valid_result(latency=latency), HARNESS_HASH, 0, latency_split_schema=schema)


LIST_WITH_SPLIT = {**LIST_JSON, "capabilities": {"latency_split_schema": 1}}


def split_side(*samples_per_run):
    """A side's valid counters runs, each carrying one latency object built from its samples."""
    return perf.SideRuns(outcomes=[make_outcome(result=counters_result(latency=split_latency(list(samples))))
                                   for samples in samples_per_run])


LIST_WITH_KINDS = {**LIST_JSON, "capabilities": {"latency_split_schema": 1, "phase_kinds": 1}}
# Marks a field kind_phase drops.
DROP = object()


def kind_phase(kind="sustained", **fields):
    """One result.json phase from a phase-kinds harness: valid_result's phase with its kind and presentation
    fields (120 presented frames, the first 1 ms and the last 59 s into the phase, 10 of 130 redraws not
    presenting); `fields` replace keys, and DROP removes one."""
    phase = dict(valid_result()["phases"][0], kind=kind, first_present_ms=1.0, last_present_ms=59_000.0,
                 first_present_seq=1, last_present_seq=120, nonpresenting_redraws=10)
    phase.update(fields)
    return {key: value for key, value in phase.items() if value is not DROP}


class PhaseKindTests(CompareHarness, unittest.TestCase):
    def kind_problems(self, phases, schema=1):
        """What validate_result reports for a result carrying `phases`, under the harness's phase-kinds schema;
        a validator that does not take the schema fails the test rather than erroring."""
        try:
            return perf.validate_result(valid_result(phases=phases), HARNESS_HASH, 0, phase_kinds=schema)
        except TypeError as error:
            # When: validate_result has no phase_kinds parameter, the capability is not validated at all.
            self.fail(f"validate_result takes no phase_kinds: {error}")

    def test_scenario_list_reads_the_phase_kinds_capability(self):
        # A harness that classifies its phases lists phase_kinds 1 beside the split schema; every scenario carries
        # it into its plans. A list with only the split schema is a harness that predates phase kinds.
        try:
            capable = perf.parse_scenario_list(json.dumps(LIST_WITH_KINDS))
            legacy = perf.parse_scenario_list(json.dumps(LIST_WITH_SPLIT))
            kinds = ([scenario.phase_kinds for scenario in capable], [scenario.phase_kinds for scenario in legacy])
        except (ValueError, AttributeError) as error:
            # When: the list was refused or carries no phase_kinds, the capability is not read.
            self.fail(f"the phase_kinds capability is not read: {error}")
        self.assertEqual(kinds, ([1, 1], [None, None]))
        self.assertEqual([scenario.latency_split_schema for scenario in capable], [1, 1])

    def test_an_unknown_or_incomplete_capability_map_is_refused(self):
        # An unknown phase_kinds schema, a non-integer value, an unknown key or a map without the split schema
        # cannot be validated, so the list is refused rather than read as legacy.
        for capabilities in ({"latency_split_schema": 1, "phase_kinds": 2}, {"latency_split_schema": 1, "phase_kinds": True},
                             {"latency_split_schema": 1, "phase_kinds": 1.0}, {"phase_kinds": 1},
                             {"latency_split_schema": 1, "phase_kinds": 1, "other": 1}):
            with self.subTest(capabilities=capabilities), self.assertRaises(ValueError):
                perf.parse_scenario_list(json.dumps({**LIST_JSON, "capabilities": capabilities}))

    def test_a_phase_kinds_harness_must_classify_and_trace_every_phase(self):
        # Valid phases of each kind pass; each rule broken alone is a schema failure naming it, never a fallback.
        valid = [kind_phase(), kind_phase("transition", endpoint="sentinel-parsed", completion_ms=40.0),
                 kind_phase("transition", endpoint="sentinel-parsed", completion_missing="expired"),
                 kind_phase("transition", endpoint="sentinel-parsed", completion_missing="incomplete"),
                 kind_phase("hold"),
                 kind_phase("hold", presented_frames=0, first_present_ms=DROP, last_present_ms=DROP,
                            first_present_seq=DROP, last_present_seq=DROP, present_missing="no-presentation")]
        self.assertEqual(self.kind_problems(valid), [])
        transition = {"kind": "transition", "endpoint": "sentinel-parsed", "completion_ms": 40.0}
        no_frames = {"presented_frames": 0, "first_present_ms": DROP, "last_present_ms": DROP,
                     "first_present_seq": DROP, "last_present_seq": DROP}
        cases = {
            "unknown kind": ({"kind": "burst"}, "kind 'burst' is not one of"),
            "no kind": ({"kind": DROP}, "kind None is not one of"),
            "no endpoint": ({**transition, "endpoint": DROP}, "endpoint None is not a transition endpoint"),
            "unknown endpoint": ({**transition, "endpoint": "soon"}, "endpoint 'soon' is not a transition endpoint"),
            "both completions": ({**transition, "completion_missing": "expired"}, "exactly one of completion_ms"),
            "no completion": ({**transition, "completion_ms": DROP}, "exactly one of completion_ms"),
            "negative completion": ({**transition, "completion_ms": -1.0}, "completion_ms -1.0 is not"),
            "infinite completion": ({**transition, "completion_ms": float("inf")}, "completion_ms inf is not"),
            "reason of another endpoint": ({**transition, "completion_ms": DROP, "completion_missing": "not-presented"},
                                           "'not-presented' is not a reason for sentinel-parsed"),
            "hold with a completion": ({"kind": "hold", "completion_ms": 3.0}, "hold phase but carries completion_ms"),
            "sustained with an endpoint": ({"endpoint": "sentinel-parsed"}, "sustained phase but carries endpoint"),
            "frames without a first": ({"first_present_ms": DROP}, "does not record its first and last presentation"),
            "frames with a missing reason": ({"present_missing": "no-presentation"},
                                             "does not record its first and last presentation"),
            "first after last": ({"first_present_ms": 60_000.0}, "first presentation follows its last"),
            "first count after last": ({"first_present_seq": 121}, "first presentation follows its last"),
            "negative offset": ({"first_present_ms": -1.0}, "are not finite non-negative"),
            "fractional count": ({"last_present_seq": 120.5}, "are not finite non-negative"),
            "no frames but a first": ({"presented_frames": 0}, "presented no frame"),
            "no frames without the reason": (no_frames, "presented no frame"),
            "no frames, another reason": ({**no_frames, "present_missing": "late"}, "presented no frame"),
            "non-presenting above redraws": ({"nonpresenting_redraws": 131}, "nonpresenting_redraws 131"),
            "no non-presenting count": ({"nonpresenting_redraws": DROP}, "nonpresenting_redraws None"),
        }
        for name, (fields, expected) in cases.items():
            with self.subTest(case=name):
                problems = self.kind_problems([kind_phase(**fields)])
                self.assertTrue(any(expected in problem for problem in problems), problems)

    def test_a_legacy_harness_keeps_the_old_phase_contract(self):
        # Without the capability a phase carries no kind or presentation fields and none is required.
        self.assertEqual(self.kind_problems(valid_result()["phases"], schema=None), [])

    def test_plans_carry_the_head_lists_phase_kinds_into_validation(self):
        # Both sides run the head's overlaid harness, so the head's list decides both plans' phase_kinds, and
        # every result is validated under it.
        try:
            _code, _gate, _calls, plans, _work, _out = self.compare(listing=LIST_WITH_KINDS)
        except ValueError as error:
            # When: the head's list was refused, no plan carries its capability.
            self.fail(f"the head's phase_kinds list was refused: {error}")
        self.assertEqual({getattr(plan, "phase_kinds", None) for plan in plans}, {1})
        # The harness fakes runs above validate_result, so the real run's call is checked in its source.
        self.assertIn("phase_kinds=plan.phase_kinds", inspect.getsource(perf.execute_run))


class LatencySplitTests(CompareHarness, unittest.TestCase):
    def test_scenario_list_reads_the_latency_split_capability(self):
        # A harness that writes the split fields lists the schema; every scenario carries it into its plans.
        scenarios = perf.parse_scenario_list(json.dumps(LIST_WITH_SPLIT))
        self.assertEqual([scenario.latency_split_schema for scenario in scenarios], [1, 1])

    def test_a_list_without_capabilities_is_a_legacy_harness(self):
        # A head that predates the split lists no capabilities, and its results keep the old contract.
        scenarios = perf.parse_scenario_list(json.dumps(LIST_JSON))
        self.assertEqual([scenario.latency_split_schema for scenario in scenarios], [None, None])

    def test_an_unknown_split_capability_is_refused(self):
        # An unknown schema, a non-object, a non-integer value (True and 1.0 included) or an extra key cannot be
        # validated, so the list is refused rather than read as legacy.
        for capabilities in ({"latency_split_schema": 2}, [], "1", {"latency_split_schema": True},
                             {"latency_split_schema": 1.0}, {"latency_split_schema": 1, "other": 1}, {}):
            with self.subTest(capabilities=capabilities), self.assertRaises(ValueError):
                perf.parse_scenario_list(json.dumps({**LIST_JSON, "capabilities": capabilities}))

    def test_an_older_head_with_a_matching_hash_and_unsplit_latency_is_accepted(self):
        # With no declared schema the latency object is read as before; split fields, even malformed, are not
        # required and not trusted.
        legacy = {"samples": [{"inject_unix_s": 2.0, "latency_ms": 7.5, "attributed": True, "reason": "credited"}],
                  "attributed": 1, "total": 1, "coverage": 1.0}
        self.assertEqual(split_problems(legacy, schema=None), [])
        self.assertEqual(split_problems({**legacy, "split_count": "many"}, schema=None), [])

    def test_a_schema_1_harness_without_the_discriminator_fails(self):
        # A harness that declares schema 1 must say so in every latency object.
        latency = split_latency([split_sample()])
        self.assertEqual(split_problems(latency), [])
        del latency["split_schema"]
        problems = split_problems(latency)
        self.assertTrue(any("split_schema" in problem for problem in problems), problems)
        self.assertEqual(split_problems(latency, schema=None), [])

    def test_a_schema_1_harness_missing_split_fields_fails(self):
        # Every sample, every report field, and an empty list's zero aggregates are required.
        no_reason = split_latency([split_sample(), reason_sample("unsupported")])
        for sample in no_reason["samples"]:
            del sample["split_reason"]
        mixed = split_latency([split_sample(), reason_sample("unsupported")])
        del mixed["samples"][1]["split_reason"]
        empty_with_counts = {**split_latency([]), "attributed": 2, "split_count": 1, "split_reasons": {"split": 1},
                             "split_coverage": 0.5}
        cases = {"every sample": no_reason, "one sample": mixed, "empty with counts": empty_with_counts}
        for field_name in ("split_count", "split_reasons", "split_coverage"):
            missing = split_latency([split_sample()])
            del missing[field_name]
            cases[f"no {field_name}"] = missing
        for name, latency in cases.items():
            with self.subTest(name):
                self.assertTrue(split_problems(latency))

    def test_an_overlaid_base_without_the_feature_still_requires_schema_1(self):
        # Both sides run the head's overlaid harness, so the head's list decides both plans' schema; a base
        # result without the discriminator is then refused like a head result.
        code, _gate, _calls, plans, _work, _out = self.compare(listing=LIST_WITH_SPLIT)
        self.assertEqual(code, 0)
        self.assertEqual({plan.side for plan in plans}, {"base", "head"})
        self.assertEqual({plan.latency_split_schema for plan in plans}, {1})
        base_plan = next(plan for plan in plans if plan.side == "base")
        unmarked = split_latency([reason_sample("unsupported")])
        del unmarked["split_schema"]
        self.assertTrue(perf.validate_result(valid_result(latency=unmarked), HARNESS_HASH, 0, partial_counters=True,
                                             latency_split_schema=base_plan.latency_split_schema))

    def test_split_validation_rejects_each_rule(self):
        # One valid object (a split, an unsupported and an uncredited sample); each rule broken alone is refused.
        def base():
            return split_latency([split_sample(), reason_sample("unsupported"), reason_sample("not-credited", None)])
        self.assertEqual(split_problems(base()), [])

        def sample_change(position, **changes):
            def change(latency):
                latency["samples"][position].update(changes)
            return change

        def split_change(**changes):
            def change(latency):
                latency["samples"][0]["split"].update(changes)
            return change

        def report_change(**changes):
            def change(latency):
                latency.update(changes)
            return change

        rules = {
            "unknown reason": sample_change(1, split_reason="made-up"),
            "credited reads not-credited": sample_change(1, split_reason="not-credited"),
            "uncredited reads a reason": sample_change(2, split_reason="unsupported"),
            "negative latency": sample_change(1, latency_ms=-1.0),
            "string latency": sample_change(1, latency_ms="slow"),
            "split for another reason": sample_change(1, split=dict(split_sample()["split"])),
            "no split for split": sample_change(0, split=None),
            "negative part": split_change(input_to_parse_ms=-0.5),
            "infinite part": split_change(parse_to_publication_ms=float("inf")),
            "string lag": split_change(delivery_lag_us="slow"),
            "refused delivery": split_change(delivery="refused"),
            "coalesced not a boolean": split_change(coalesced=0),
            "sync_open not a boolean": split_change(sync_open=None),
            "negative generation": split_change(echo_generation=-1),
            "fractional generation": split_change(echo_generation=1.5),
            "boolean generation": split_change(echo_generation=True),
            "parts off the latency": split_change(publication_to_present_ms=5.01),
            "attributed": report_change(attributed=3),
            "reasons": report_change(split_reasons={"split": 1, "unsupported": 1, "not-credited": 1}),
            "boolean reason count": report_change(split_reasons={"split": True, "unsupported": 1}),
            "split count": report_change(split_count=2),
            "coverage": report_change(split_coverage=1.0),
            "null coverage when credited": report_change(split_coverage=None),
        }
        for name, change in rules.items():
            with self.subTest(name):
                latency = base()
                change(latency)
                self.assertTrue(split_problems(latency))
        # Serialized parts are rounded per part, so a sum within 0.001 ms of the latency is accepted.
        within = base()
        within["samples"][0]["split"]["publication_to_present_ms"] = 5.0009
        self.assertEqual(split_problems(within), [])

    def test_a_schema_1_report_must_carry_every_nullable_key(self):
        # A key the schema allows to be null must still be present: dropping it is an incomplete contract, not
        # a null. Explicit nulls in the same places stay valid.
        def without(latency, key, position=None):
            if position is None:
                del latency[key]
            else:
                del latency["samples"][position][key]
            return latency
        self.assertEqual(split_problems(split_latency([])), [])
        self.assertEqual(split_problems(split_latency([reason_sample("not-credited", None),
                                                       reason_sample("unsupported")])), [])
        cases = {
            "empty report without split_coverage": without(split_latency([]), "split_coverage"),
            "uncredited sample without split": without(split_latency([reason_sample("not-credited", None)]),
                                                       "split", 0),
            "uncredited sample without latency_ms": without(split_latency([reason_sample("not-credited", None)]),
                                                            "latency_ms", 0),
            "unsupported credited sample without split": without(split_latency([reason_sample("unsupported")]),
                                                                  "split", 0),
        }
        for name, latency in cases.items():
            with self.subTest(name):
                problems = split_problems(latency)
                self.assertTrue(any("missing" in problem for problem in problems), problems)

    def test_split_validation_rejects_fabricated_aggregates(self):
        # Mutually consistent aggregates that the samples do not support are refused: no sample is split.
        fabricated = split_latency([reason_sample("unsupported")])
        fabricated.update(split_count=10, split_reasons={"split": 10}, split_coverage=1.0, attributed=10)
        problems = split_problems(fabricated)
        for field_name in ("attributed", "split_reasons", "split_count", "split_coverage"):
            self.assertTrue(any(problem.startswith(f"latency {field_name}") for problem in problems), field_name)

    def test_an_empty_split_report_validates(self):
        # No samples: zero counts, no reasons and null coverage.
        self.assertEqual(split_problems(split_latency([])), [])
        self.assertEqual(split_latency([])["split_coverage"], None)

    def test_split_rows_appear_only_for_s2_default(self):
        # The split covers S2/default's typing phase alone, so no other variant gets rows.
        side = split_side([split_sample(), split_sample((1.0, 1.0, 1.0)), reason_sample("unsupported")])
        rows = perf.split_rows("S2/default", side, side, 1)
        self.assertEqual([row[2] for row in rows], [row_label for row_label, _ in perf.SPLIT_ROWS])
        self.assertTrue(all(row[:2] == ["S2/default", "typing"] for row in rows))
        by_label = {row[2]: row for row in rows}
        self.assertEqual(by_label["split input to parse, median (ms)"][4], "1.500 ms (2 splits)")
        self.assertEqual(by_label["split coverage (%)"][4], "66.7% (2/3)")
        self.assertEqual(by_label["split coverage (%)"][5], "+0.0%")
        self.assertIn("split 2, unsupported 1", by_label["split reasons (credited samples)"][4])
        self.assertIn("suppressed 0, coalesced 0, sync_open 0", by_label["split reasons (credited samples)"][4])
        for label in ("S2/flood", "S1/default", "S10/sync"):
            with self.subTest(label):
                self.assertEqual(perf.split_rows(label, side, side, 1), [])

    def test_split_rows_read_unsupported_on_a_base_without_the_feature(self):
        # A base built without perf-echo-trace credits samples but splits none: its cells read n/a (unsupported)
        # and nothing is compared; a head-only set reads n/a.
        base = split_side([reason_sample("unsupported"), reason_sample("unsupported")])
        head = split_side([split_sample(), reason_sample("arm-gate-off")])
        rows = perf.split_rows("S2/default", base, head, 1)
        timing = [row for row in rows if row[2] != "split reasons (credited samples)"]
        self.assertTrue(all(row[3] == "n/a (unsupported)" for row in timing), timing)
        self.assertTrue(all(row[5] == "n/a" for row in rows))
        self.assertEqual(rows[0][4], "2.000 ms (1 splits)")
        head_only = perf.split_rows("S2/default", perf.SideRuns(blocked=perf.COUNTERS_HEAD_ONLY), head, 1)
        self.assertTrue(all(row[3] == "n/a" for row in head_only))

    def test_split_coverage_reads_zero_unsupported_or_unavailable(self):
        # Credited, supported samples with no split are 0% coverage, a number; only a build whose every credited
        # sample reads unsupported is n/a (unsupported); no credited sample at all is unavailable. Timing cells
        # without a split read n/a (no split), and an empty reason set is never taken as an unsupported build.
        def cells(*samples):
            rows = perf.split_rows("S2/default", split_side(list(samples)), split_side([split_sample()]), 1)
            return {row[2]: row[3] for row in rows}
        zero = cells(reason_sample("no-appearance-observed"), reason_sample("no-appearance-observed"))
        self.assertEqual(zero["split coverage (%)"], "0.0% (0/2)")
        self.assertEqual(zero["split input to parse, median (ms)"], "n/a (no split)")
        unsupported = cells(reason_sample("unsupported"), reason_sample("unsupported"))
        self.assertEqual(unsupported["split coverage (%)"], "n/a (unsupported)")
        self.assertEqual(unsupported["split input to parse, median (ms)"], "n/a (unsupported)")
        nothing = cells(reason_sample("not-credited", None))
        self.assertEqual(nothing["split coverage (%)"], "unavailable")
        self.assertEqual(nothing["split input to parse, median (ms)"], "n/a (no split)")

    def test_split_rows_are_absent_for_a_legacy_head(self):
        # A head harness without the capability has no split to tabulate, whatever its samples say.
        side = split_side([split_sample()])
        self.assertEqual(perf.split_rows("S2/default", side, side, None), [])

    def test_tree_features_adds_echo_trace_only_with_counters(self):
        # perf-echo-trace implies perf-counters: a tree builds with it only when it declares it and supports
        # counters, and a build passes it after the other features.
        echo_table = "\n[features]\nperf-counters = []\nperf-echo-trace = [\"perf-counters\"]\n"

        def tree(manifest, logging_source):
            temporary = tempfile.TemporaryDirectory()
            self.addCleanup(temporary.cleanup)
            root = Path(temporary.name)
            (root / perf.APP_MANIFEST).parent.mkdir(parents=True)
            (root / perf.APP_MANIFEST).write_text(manifest, encoding="utf-8")
            (root / LOGGING_LIB).parent.mkdir(parents=True)
            (root / LOGGING_LIB).write_text(logging_source, encoding="utf-8")
            return root
        self.assertEqual(perf.tree_features(tree(HEAD_MANIFEST + echo_table, LOGGING_WITH_FILTER)),
                         ("perf-counters", "perf-echo-trace"))
        self.assertEqual(perf.tree_features(tree(HEAD_MANIFEST + echo_table, LOGGING_WITHOUT_FILTER)), ())
        self.assertEqual(perf.tree_features(tree(COUNTERS_MANIFEST, LOGGING_WITH_FILTER)), ("perf-counters",))
        self.assertEqual(perf.build_argv("perf_scenarios", True, features=("perf-counters", "perf-echo-trace"))[-1],
                         "perf-counters,perf-echo-trace")

    def test_the_split_reasons_match_the_harness(self):
        # The harness's precedence table is the reason set this script accepts, in the same order.
        record = (Path(__file__).parent.parent / "crates" / "sonicterm-app" / "examples" / "perf_scenarios"
                  / "record.rs").read_text(encoding="utf-8")
        table = record.split("pub(crate) const SPLIT_REASONS: [&str; 22] = [", 1)[1].split("];", 1)[0]
        listed = [name.strip().strip('"') for name in table.split(",") if name.strip()]
        listed = ["split" if name == "SPLIT" else name for name in listed]
        self.assertEqual(tuple(listed), perf.SPLIT_REASONS)



class TrimHookFeatureTests(PrebuiltHarness, unittest.TestCase):
    """perf-hook-trim reaches only a tree that defines the trim hook, through the gate's reviewed steps."""

    def build_step(self, gate, side):
        return next(step for step in gate.steps if step.id == f"build-{side}-perf_scenarios")

    def test_a_tree_needs_the_declaration_and_the_trim_method(self):
        # The feature alone, or the method only in a comment, does not make a tree trim-capable.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / perf.APP_MANIFEST).parent.mkdir(parents=True)
            (root / perf.APP_MANIFEST).write_text(TRIM_MANIFEST, encoding="utf-8")
            source = root / perf.APP_SOURCE_DIRECTORY / "app" / "retention.rs"
            source.parent.mkdir(parents=True)
            method = perf.HOOK_METHODS[perf.TRIM_HOOK_FEATURE]
            self.assertFalse(perf.tree_supports_hook(root, perf.TRIM_HOOK_FEATURE, method))
            source.write_text("// pub fn __trim_covered_now is coming\n", encoding="utf-8")
            self.assertFalse(perf.tree_supports_hook(root, perf.TRIM_HOOK_FEATURE, method))
            source.write_text(TRIM_SOURCE, encoding="utf-8")
            self.assertTrue(perf.tree_supports_hook(root, perf.TRIM_HOOK_FEATURE, method))
            (root / perf.APP_MANIFEST).write_text(HEAD_MANIFEST, encoding="utf-8")
            self.assertFalse(perf.tree_supports_hook(root, perf.TRIM_HOOK_FEATURE, method))

    def test_a_five_feature_head_and_a_four_feature_base_each_build_their_own_catalog_step(self):
        # The head declares every feature and defines both hooks; the base predates the trim hook. Each side's
        # step is the gate's catalog entry for exactly its tuple, and both run one harness hash.
        code, gate, _calls, plans, _work, out = self.compare(
            head_manifest=FIVE_FEATURES_MANIFEST, base_manifest=FOUR_FEATURES_MANIFEST,
            hook_trees=("head", "base"), trim_trees=("head",))
        self.assertEqual(code, perf.EXIT_PASS)
        # Both fixture trees define S1/atlas-retry's methods, so each side builds its features with that cfg on.
        catalog = {features: steps for (cfgs, features), steps in REAL_GATE.PERF_CFG_BUILDS.items()
                   if cfgs == ("perf_atlas_retry_api",)}
        five = perf.PERF_FEATURES
        four = tuple(feature for feature in five if feature != perf.TRIM_HOOK_FEATURE)
        self.assertIs(self.build_step(gate, "head"), catalog[five]["build-head-perf_scenarios"])
        self.assertIs(self.build_step(gate, "base"), catalog[four]["build-base-perf_scenarios"])
        self.assertEqual(self.build_step(gate, "head").argv[-1], ",".join(five))
        self.assertEqual(len({plan.harness_hash for plan in plans}), 1, "both sides run one overlaid harness")
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("- Built with `--features perf-hook-trim`: head", document)

    def test_a_trim_declaration_without_the_method_builds_without_the_feature(self):
        _code, gate, *_rest = self.compare(head_manifest=TRIM_MANIFEST)
        self.assertNotIn("--features", self.build_step(gate, "head").argv)

    def test_the_manifest_records_the_trim_feature_and_a_mismatch_fails(self):
        binaries, digest = self.produce(head_manifest=TRIM_MANIFEST, trim_trees=("head",))
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["features"], {"base": [], "head": [perf.TRIM_HOOK_FEATURE]})
        with self.assertRaisesRegex(ValueError, "refusing the prebuilt binaries: features"):
            self.consume(binaries, digest, head_manifest=TRIM_MANIFEST)
        code, *_rest = self.consume(binaries, digest, head_manifest=TRIM_MANIFEST, trim_trees=("head",))
        self.assertEqual(code, perf.EXIT_PASS)

    def test_the_gate_rejects_a_lookalike_trim_build_step(self):
        # A copy of a reviewed step with the same id and argv is not the gate's own step.
        step = REAL_GATE.PERF_FEATURE_BUILDS[perf.PERF_FEATURES]["build-head-perf_scenarios"]
        self.assertTrue(REAL_GATE._reviewed_step(step))
        self.assertFalse(REAL_GATE._reviewed_step(copy.copy(step)))

    def test_both_scripts_list_the_same_five_perf_features(self):
        self.assertEqual(perf.PERF_FEATURES, REAL_GATE.PERF_FEATURES)
        self.assertEqual(perf.PERF_FEATURES[-1], perf.TRIM_HOOK_FEATURE)


class TrimHookResultTests(unittest.TestCase):
    """result.json's `hooks.trim`: optional for an older harness, one known outcome when present."""

    def test_a_result_with_or_without_hooks_validates(self):
        # An older harness writes no `hooks`; this harness writes one of four outcomes.
        self.assertEqual(perf.validate_result(valid_result(), HARNESS_HASH, 0), [])
        for outcome in ("not-reached", "unsupported", "skipped", "trimmed"):
            with self.subTest(outcome=outcome):
                # A trimmed hook names its trim number; every other outcome carries none.
                after_hook = 1 if outcome == "trimmed" else None
                result = valid_result(hooks={"trim": outcome}, trim_seq_after_hook=after_hook)
                self.assertEqual(perf.validate_result(result, HARNESS_HASH, 0), [])

    def test_an_unknown_trim_outcome_or_shape_is_refused(self):
        for hooks in ({"trim": "maybe"}, {"trim": None}, {}, {"trim": 0}, [], "unsupported", None):
            with self.subTest(hooks=hooks):
                self.assertTrue(perf.validate_result(valid_result(hooks=hooks), HARNESS_HASH, 0))

    def test_an_unsupported_trim_keeps_checkpoint_memory_numeric(self):
        # An unsupported trim is an untrimmed baseline: the supported golden run's checkpoint memory stays a
        # figure, exactly as it reads without `hooks`; nothing reads n/a and nothing reads zero.
        result, samples = fixture_run("supported", "deadline-610")
        baseline = perf.run_metrics(make_outcome(result=result, memory=samples))
        hooked = dict(result, hooks={"trim": "unsupported"})
        metrics = perf.run_metrics(make_outcome(result=hooked, memory=samples))
        self.assertEqual(metrics, baseline)
        memory = {key: value for key, value in metrics.items()
                  if key[0].endswith(("renderer_total_bytes", "process_resident_bytes"))}
        self.assertTrue(memory, "the golden run has checkpoint memory")
        for key, value in memory.items():
            with self.subTest(key=key):
                self.assertIsInstance(value, float)
                self.assertNotIsInstance(value, perf.NotAvailable)
        # The golden run has no renderer, so its renderer reading is a real 0; resident memory is never 0.
        resident = [value for key, value in memory.items() if key[0].endswith("process_resident_bytes")]
        self.assertTrue(resident and all(value > 0 for value in resident), resident)



def trim_run(platform_trim="trimmed", after_hook=2, **overrides):
    """A short S12 trim-experiment result whose `covered` checkpoint is index 0."""
    covered = {"name": "covered", "start_unix_s": 10.0, "end_unix_s": 15.0, "cpu_user_s": 0.2,
               "cpu_system_s": 0.1, "presented_frames": 1, "redraw_requested": 2,
               "dispatch_ms": [1.0], "present_interval_ms": [16.6], "allocations_per_frame": None}
    fields = dict(scenario="S12", short=True, trim_experiment="s12-short-trim", hooks={"trim": platform_trim},
                  trim_seq_after_hook=after_hook, phases=[covered], checkpoint_memory="supported",
                  checkpoints=[{"index": 0, "label": "covered", "unix_s": 15.0, "footprint_file": None}])
    fields.update(overrides)
    return valid_result(**fields)


def covered_sample(trim_seq=None, trimmed=None, trim_source=None, renderer_mib=16):
    """A complete `covered` checkpoint sample with the given trim tags."""
    return perf.MemorySample(15.0, 100 * 1048576, renderer_mib * 1048576, 0, checkpoint_index=0,
                             checkpoint_label="covered", checkpoint_attempt=1, checkpoint_complete=True,
                             trimmed=trimmed, trim_source=trim_source, trim_seq=trim_seq)


# One trimmed renderer entry exactly as the App's breakdown writes it. The App's own test asserts this text
# byte for byte, so a rename on either side fails one of the two suites.
TRIMMED_RENDERER_ENTRY = ("visible[WindowId(1)] glyph=512/5 image=256/2 row_glyph=64/4 row_quad=32/3 software=1024/1 vertex=272/1 row_ink=48/40 frame_scratch=96/2 chrome_cache=24/3 total=2328/61 glyph_atlas_dim=512 glyph_atlas_packed_pixels=4000 glyph_atlas_growths=1 glyph_atlas_evictions=0 glyph_atlas_fit=512 glyph_atlas_max_tile=25x16 renderer_trimmed=true renderer_gpu_released_requested_bytes=8388608")


def renderer_entries(*trimmed_states):
    """Renderer entries in the App's format, one per state, each with its own window id."""
    return "; ".join(
        TRIMMED_RENDERER_ENTRY.replace("WindowId(1)", f"WindowId({index + 1})")
        .replace("renderer_trimmed=true", f"renderer_trimmed={str(state).lower()}")
        for index, state in enumerate(trimmed_states))


def formatted_covered_line(entries, trim_tags=""):
    """A covered checkpoint `memory snapshot` line in the production field order, with `entries` inside the
    renderer breakdown and `trim_tags` appended after the checkpoint tags (empty for a build without the hook)."""
    return (f"{STAMP}  INFO memory: memory snapshot process_private_committed_bytes=unsupported "
            f"process_resident_bytes=104857600 process_virtual_bytes=unsupported session_total_bytes=0 "
            f"panes_total=1 panes_sampled=1 panes_contended=0 renderer_total_bytes=16777216 live_renderers=2 "
            f"renderers={entries} allocator_state=unsupported checkpoint_index=0 checkpoint_label=\"covered\" "
            f"checkpoint_attempt=1 checkpoint_complete=true{trim_tags}")


class TrimExperimentTests(unittest.TestCase):
    """The short S12 trim experiment: covered-row rules, the Windows validator and the covered activity rows."""

    COVERED = ("covered renderer_total_bytes", "MiB", "run")

    def covered_metric(self, result, sample):
        return perf.run_metrics(make_outcome(result=result, memory=[sample]))[self.COVERED]

    def test_a_tagged_line_parses_its_three_trim_tags(self):
        # The trim tags come back typed; a line without them reads None for each.
        line = (CheckpointMemoryTests.LINE + ' trimmed=true trim_source="hook" trim_seq=3')
        sample = perf.parse_memory_line(line)
        self.assertEqual((sample.trimmed, sample.trim_source, sample.trim_seq), (True, "hook", 3))
        plain = perf.parse_memory_line(CheckpointMemoryTests.LINE)
        self.assertEqual((plain.trimmed, plain.trim_source, plain.trim_seq), (None, None, None))

    def test_top_level_trim_tags_never_read_a_renderer_entry(self):
        # Two renderer entries disagree with the line's own tag in both directions; the parser reads only the
        # top-level tag, and the renderer entries still parse as glyph atlas facts.
        for top, entries in ((True, (False, True)), (False, (True, True))):
            with self.subTest(top=top):
                tags = f' trimmed={str(top).lower()} trim_source="hook" trim_seq=2'
                sample = perf.parse_memory_line(formatted_covered_line(renderer_entries(*entries), tags))
                self.assertEqual((sample.trimmed, sample.trim_source, sample.trim_seq), (top, "hook", 2))
                self.assertEqual(len(sample.glyph_atlases), 2)

    def test_a_feature_off_line_with_renderer_entries_is_an_untrimmed_baseline(self):
        # A build without the hook writes renderer entries but no checkpoint trim tags: every tag parses as None,
        # and an unsupported side's covered reading stays a figure, never n/a: schema.
        sample = perf.parse_memory_line(formatted_covered_line(renderer_entries(True, False)))
        self.assertEqual((sample.trimmed, sample.trim_source, sample.trim_seq), (None, None, None))
        base = trim_run("unsupported", None)
        self.assertEqual(self.covered_metric(base, sample), 16.0)

    def test_a_malformed_trim_tag_is_a_schema_problem_never_an_absent_one(self):
        # A present but unreadable trim tag is recorded as malformed rather than read as absent, so an
        # unsupported side carrying one reads n/a: schema, and a trimmed side does too. A line with no trim
        # tags at all keeps the numeric unsupported baseline.
        base = trim_run("unsupported", None)
        head = trim_run("trimmed", 2)
        for tags, malformed in ((" trimmed=bogus", ("trimmed",)), (" trim_seq=-1", ("trim_seq",)),
                                (' trimmed=true trim_source="hook" trim_seq=x2', ("trim_seq",))):
            with self.subTest(tags=tags):
                sample = perf.parse_memory_line(formatted_covered_line(renderer_entries(True), tags))
                self.assertEqual(sample.malformed_trim_tags, malformed)
                self.assertEqual(self.covered_metric(base, sample), perf.NotAvailable("schema"))
                self.assertEqual(self.covered_metric(head, sample), perf.NotAvailable("schema"))
        absent = perf.parse_memory_line(formatted_covered_line(renderer_entries(True)))
        self.assertEqual(absent.malformed_trim_tags, ())
        self.assertEqual(self.covered_metric(base, absent), 16.0)

    def test_a_supported_experiment_that_did_not_trim_is_never_credited(self):
        # Skipped, unreached or unrecorded hooks read n/a with their reason, never as an ordinary trimmed
        # reading; the raw covered figure stays on its own row, never replaced by zero.
        raw_key = ("covered renderer_total_bytes, uncredited trim", "MiB", "run")
        cases = (("skipped", "trim skipped"), ("not-reached", "trim not reached"), (None, "trim not recorded"))
        for outcome, reason in cases:
            with self.subTest(outcome=outcome):
                hooks = {"trim": outcome} if outcome else None
                result = trim_run(after_hook=None, hooks=hooks)
                metrics = perf.run_metrics(make_outcome(result=result, memory=[covered_sample(renderer_mib=12)]))
                self.assertEqual(metrics[self.COVERED], perf.NotAvailable(reason))
                self.assertIn(raw_key, metrics, "the raw covered figure keeps its own row")
                self.assertEqual(metrics[raw_key], 12.0)

    def test_a_credited_trim_needs_its_state_source_and_number(self):
        # A trimmed hook counts only a sample that is trimmed, names hook or scheduler, and is at least the
        # hook's number; a sample after the trim whose window reads untrimmed is stale, and a missing state or a
        # source that names no trim is a schema problem.
        head = trim_run("trimmed", 2)
        for source in ("hook", "scheduler"):
            with self.subTest(source=source):
                self.assertEqual(self.covered_metric(head, covered_sample(2, True, source, 4)), 4.0)
        self.assertEqual(self.covered_metric(head, covered_sample(2, False, "hook")), perf.NotAvailable("stale"))
        for trimmed, source in ((None, "hook"), (True, "none"), (True, None), (True, "bogus")):
            with self.subTest(trimmed=trimmed, source=source):
                value = self.covered_metric(head, covered_sample(2, trimmed, source))
                self.assertEqual(value, perf.NotAvailable("schema"))

    def test_the_hook_outcome_and_its_number_must_agree(self):
        # A trimmed hook needs a positive number; any other outcome carries none; an experiment records its hook.
        # The results carry an `end` checkpoint, so each one's only problem is the one asserted.
        points = [{"index": 0, "label": "covered", "unix_s": 15.0, "footprint_file": None},
                  {"index": 1, "label": "end", "unix_s": 20.0, "footprint_file": None}]
        self.assertEqual(perf.validate_result(trim_run("trimmed", 2, checkpoints=points), HARNESS_HASH, 0), [])
        self.assertEqual(perf.validate_result(trim_run("unsupported", None, checkpoints=points), HARNESS_HASH, 0), [])
        expected = {("trimmed", None): "hooks.trim is trimmed but trim_seq_after_hook is not a positive integer",
                    ("unsupported", 2): "hooks.trim is 'unsupported' but trim_seq_after_hook is 2, not null",
                    ("skipped", 1): "hooks.trim is 'skipped' but trim_seq_after_hook is 1, not null",
                    ("not-reached", 3): "hooks.trim is 'not-reached' but trim_seq_after_hook is 3, not null"}
        for (outcome, after_hook), problem in expected.items():
            with self.subTest(outcome=outcome, after_hook=after_hook):
                result = trim_run(outcome, after_hook, checkpoints=points)
                self.assertEqual(perf.validate_result(result, HARNESS_HASH, 0), [problem])
        unrecorded = trim_run(after_hook=None, checkpoints=points)
        del unrecorded["hooks"]
        self.assertEqual(perf.validate_result(unrecorded, HARNESS_HASH, 0),
                         ["a trim experiment result records no hooks"])

    def test_covered_rows_follow_the_trim_rules(self):
        # An unsupported base without tags is an untrimmed baseline; a hooked sample at or after the hook's trim
        # number compares; one before it, or without a number, is stale; tags on an unsupported side are a schema
        # problem.
        base = trim_run("unsupported", None)
        self.assertEqual(self.covered_metric(base, covered_sample()), 16.0)
        head = trim_run("trimmed", 2)
        self.assertEqual(self.covered_metric(head, covered_sample(2, True, "hook", 4)), 4.0)
        self.assertEqual(self.covered_metric(head, covered_sample(3, True, "hook", 4)), 4.0)
        for stale in (covered_sample(1, True, "hook"), covered_sample(0, False, "none"), covered_sample()):
            with self.subTest(trim_seq=stale.trim_seq):
                self.assertEqual(self.covered_metric(head, stale), perf.NotAvailable("stale"))
        tagged = covered_sample(0, False, "none")
        self.assertEqual(self.covered_metric(base, tagged), perf.NotAvailable("schema"))

    def test_the_trim_rules_leave_other_checkpoints_and_runs_alone(self):
        # Pins: a partial authoritative sample still reads partial; an unsupported checkpoint hook still reads
        # n/a: unsupported; a result outside the experiment ignores its tags.
        point = {"index": 0, "label": "covered", "unix_s": 15.0, "footprint_file": None,
                 "sampling": "exhausted", "attempts": 1, "last_attempt_complete": False}
        partial = perf.MemorySample(15.0, 100 * 1048576, 8 * 1048576, 0, checkpoint_index=0,
                                    checkpoint_label="covered", checkpoint_attempt=1, checkpoint_complete=False,
                                    trimmed=True, trim_source="hook", trim_seq=2)
        value = self.covered_metric(trim_run(checkpoints=[point]), partial)
        self.assertIsInstance(value, perf.PartialValue)
        unsupported = self.covered_metric(trim_run(checkpoint_memory="unsupported"), covered_sample())
        self.assertEqual(unsupported, perf.NotAvailable("unsupported"))
        outside = trim_run("unsupported", None, trim_experiment=None)
        self.assertEqual(self.covered_metric(outside, covered_sample(0, False, "none")), 16.0)

    def test_windows_accepts_synthetic_occlusion_only_for_the_trim_experiment(self):
        # The one valid combination: win32, synthetic occlusion, S12, short and the experiment name.
        points = [{"index": 0, "label": "covered", "unix_s": 15.0, "footprint_file": None},
                  {"index": 1, "label": "end", "unix_s": 20.0, "footprint_file": None}]
        good = trim_run(synthetic_occlusion=True, presenter=dict(WGPU_PRESENTER), checkpoints=points)
        self.assertEqual(perf.validate_result(good, HARNESS_HASH, 0, platform_name="win32"), [])
        for change in ({"scenario": "S1"}, {"short": False}, {"trim_experiment": None}):
            with self.subTest(change=change):
                broken = dict(good, **change)
                self.assertTrue(perf.validate_result(broken, HARNESS_HASH, 0, platform_name="win32"))
        self.assertEqual(perf.validate_result(good, HARNESS_HASH, 0, platform_name="darwin"), [])
        for bad in ({"trim_experiment": "other"}, {"trim_seq_after_hook": 0}, {"trim_seq_after_hook": "2"}):
            with self.subTest(bad=bad):
                self.assertTrue(perf.validate_result(dict(good, **bad), HARNESS_HASH, 0))

    def test_the_covered_phase_reports_activity_not_rates(self):
        # On both hosts the experiment's covered phase prints wall, presented frames and redraws as counts and
        # CPU, never a frame rate or a present interval; the stored interval samples are untouched.
        for platform_name in ("darwin", "win32"):
            with self.subTest(platform=platform_name):
                result = trim_run()
                metrics = perf.run_metrics(make_outcome(result=result))
                self.assertNotIn(("covered presented frames", "fps", "run"), metrics)
                self.assertNotIn(("covered present interval", "ms", "frame"), metrics)
                self.assertEqual(metrics[("covered wall", "s", "run")], 5.0)
                self.assertEqual(metrics[("covered presented frames", "count", "run")], 1)
                self.assertEqual(metrics[("covered redraws requested", "count", "run")], 2)
                self.assertAlmostEqual(metrics[("covered CPU", "s", "run")], 0.3)
                self.assertEqual(result["phases"][0]["present_interval_ms"], [16.6])

    def test_without_the_experiment_the_covered_phase_keeps_its_rates(self):
        # Pin: the same result without `trim_experiment` keeps today's rate and interval rows.
        metrics = perf.run_metrics(make_outcome(result=trim_run(trim_experiment=None)))
        self.assertIn(("covered presented frames", "fps", "run"), metrics)
        self.assertIn(("covered present interval", "ms", "frame"), metrics)


# A listing whose S1 also offers the counters-only atlas-retry variant.
ATLAS_RETRY_LIST = {"schema_version": 1, "scenarios": [
    {"id": "S1", "variants": ["default", "atlas-retry"], "title": "Idle", "timeout_s": 120, "short_timeout_s": 30,
     "run_caps": {"atlas-retry": 2}}]}


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


def recovery_result(recovery=MISSING, **overrides):
    """A valid S1/atlas-retry counters result carrying `recovery` (the well-formed block unless given)."""
    result = counters_result(**overrides)
    result.setdefault("scenario", "S1")
    result.setdefault("variant", "atlas-retry")
    if recovery is MISSING:
        recovery = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
    if recovery is not None:
        result["atlas_recovery"] = recovery
    return result


class AtlasRetryVariantTests(CompareHarness, unittest.TestCase):
    def test_only_the_counters_set_runs_a_counters_only_variant(self):
        # S1/atlas-retry measures its injected episodes through counters, so the timed, laps and alloc sets are
        # dropped for it; every other variant keeps every set it was given.
        sets = [("timed", "a", False, False, 5), ("laps", "a", True, False, 2),
                ("counters", "a", False, True, 2), ("alloc", "b", False, False, 5)]
        self.assertEqual(perf.variant_sets("S1", "default", sets), sets)
        self.assertEqual(perf.variant_sets("S3", "atlas-retry", sets), sets)
        self.assertEqual(perf.variant_sets("S1", "atlas-retry", sets), [sets[2]])

    def test_a_counters_only_variant_is_refused_when_no_counters_set_runs(self):
        # Selecting the variant without a counters set would measure nothing, so the selection is refused,
        # naming the variant; ordinary selections are never refused.
        self.assertIsNone(perf.counters_only_problem([("S1", "default")], False))
        self.assertIsNone(perf.counters_only_problem([("S1", "atlas-retry")], True))
        problem = perf.counters_only_problem([("S1", "default"), ("S1", "atlas-retry")], False)
        self.assertIn("S1/atlas-retry", problem)
        self.assertIn("--counters", problem)

    def test_the_comparison_refuses_the_variant_without_counters(self):
        # The refusal reaches the driver before any run: nothing is planned, and compare_main's ValueError
        # handling turns it into a failed comparison.
        with self.assertRaisesRegex(ValueError, "S1/atlas-retry runs only in the counters set"):
            self.compare(listing=ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",), head_manifest=COUNTERS_MANIFEST)

    def test_the_comparison_refuses_the_variant_when_the_head_has_no_counters(self):
        # --counters with a head that does not declare perf-counters builds no counters set, so it is refused too.
        with self.assertRaisesRegex(ValueError, "S1/atlas-retry runs only in the counters set"):
            self.compare(listing=ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",), options=("--counters",))

    def test_a_counters_only_comparison_runs_counter_plans_and_passes_strictly(self):
        # Selected with --counters on both sides, the variant runs counters plans only and its complete
        # counter-only results leave no strict problem, so --require-base passes.
        code, _gate, _calls, plans, _work, out = self.compare(
            listing=ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",),
            options=("--counters", "--counters-runs", "2", "--require-base"),
            head_manifest=COUNTERS_MANIFEST, base_manifest=BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, 0, self.printed)
        self.assertTrue(plans)
        self.assertTrue(all(plan.counters for plan in plans))
        self.assertEqual(sorted(plan.side for plan in plans), ["base", "base", "head", "head"])
        self.assertTrue((out / "comparison.md").is_file())
        results = [perf.SetResult("S1/atlas-retry", "counters",
                                  perf.SideRuns([make_outcome(result=recovery_result())] * 2),
                                  perf.SideRuns([make_outcome(result=recovery_result())] * 2), target_runs=2)]
        self.assertEqual(perf.strict_problems(results), [])

    def test_a_well_formed_recovery_block_validates(self):
        # The harness's block for a valid counters run passes; a timed-out run may omit it.
        self.assertIsNone(perf.atlas_recovery_problem(recovery_result()))
        self.assertIsNone(perf.atlas_recovery_problem(recovery_result(None, status="timeout")))
        self.assertIsNone(perf.atlas_recovery_problem(counters_result(scenario="S1", variant="default")))

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
                self.assertIsNotNone(perf.atlas_recovery_problem(data))

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
                self.assertIsNotNone(perf.atlas_recovery_problem(recovery_result(recovery)))
        self.assertIsNone(perf.atlas_recovery_problem(recovery_result(good)))

    def test_validate_result_reports_a_recovery_problem(self):
        # The recovery check is part of the schema check, so a bad block refuses the run.
        broken = recovery_result({"episodes": 8, "distinct_keys": 70, "records": []})
        problems = perf.validate_result(broken, HARNESS_HASH, 0, counters=True)
        self.assertTrue(any("atlas_recovery" in problem for problem in problems), problems)
        self.assertFalse(any("atlas_recovery" in problem
                             for problem in perf.validate_result(recovery_result(), HARNESS_HASH, 0, counters=True)))

    def test_the_recovery_table_sums_each_frame_over_accepted_runs(self):
        # Each frame's cell sums misses, hits, shapes and attempts over every episode of every valid run; the
        # first row counts runs, episodes and distinct keys, and the last totals A-D.
        reused = recovery_records({index: {"misses": 0, "hits": 70, "shapes": 0} for index in range(1, 32, 4)})
        base = perf.SideRuns([make_outcome(result=recovery_result())] * 2)
        head = perf.SideRuns([make_outcome(result=recovery_result({"episodes": 8, "distinct_keys": 70,
                                                                   "records": reused}))] * 2)
        rows = perf.atlas_recovery_rows("S1/atlas-retry", base, head)
        self.assertEqual(rows[0], ["runs", "2 runs, 16 episodes, distinct keys 70",
                                   "2 runs, 16 episodes, distinct keys 70"])
        self.assertEqual([row[0] for row in rows], ["runs", "A", "B", "C", "D", "A-D total"])
        self.assertEqual(rows[2], ["B", "misses 1120, hits 0, shapes 1120, attempts 16",
                                   "misses 0, hits 1120, shapes 0, attempts 16"])
        self.assertEqual(rows[5][1], "misses 1120, hits 2240, shapes 1120, attempts 64")
        document = perf.comparison_document([], [], [], [], [], recovery_rows=rows)
        self.assertIn("### Atlas retry recovery", document)
        self.assertIn("| B | misses 1120, hits 0, shapes 1120, attempts 16 |", document)

    def test_no_recovery_table_without_recovery_runs(self):
        # Another variant, or a side with no accepted recovery runs on either side, adds no table, and the
        # document has no recovery section.
        runs = perf.SideRuns([make_outcome(result=recovery_result())])
        self.assertEqual(perf.atlas_recovery_rows("S1/default", runs, runs), [])
        empty = perf.SideRuns([make_outcome(result=counters_result())])
        self.assertEqual(perf.atlas_recovery_rows("S1/atlas-retry", empty, perf.SideRuns(blocked="exit 5")), [])
        head_only = perf.atlas_recovery_rows("S1/atlas-retry", perf.SideRuns(blocked="exit 5"), runs)
        self.assertEqual(head_only[1][1], "n/a")
        self.assertNotIn("Atlas retry recovery", perf.comparison_document([], [], [], [], []))



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
        self.assertEqual(perf.validate_result(valid_result(phases=[kind_phase()]), HARNESS_HASH, 0, phase_kinds=1),
                         [])
        for expected, fields in cases.items():
            with self.subTest(case=expected):
                capable = perf.validate_result(valid_result(phases=[kind_phase(**fields)]), HARNESS_HASH, 0,
                                               phase_kinds=1)
                self.assertTrue(any(expected in problem for problem in capable), capable)
                legacy_phase = dict(valid_result()["phases"][0], **fields)
                legacy = perf.validate_result(valid_result(phases=[legacy_phase]), HARNESS_HASH, 0)
                self.assertFalse(any(expected in problem and "phase-kinds" in problem for problem in legacy), legacy)


class CountersOnlyKindTests(CompareHarness, unittest.TestCase):
    def counters_only(self, listing, selected, phases, **overrides):
        """comparison.md of a counters-only comparison whose every run carries `phases` with the gate on."""
        def counters_run(plan):
            result = counters_result(phases=[dict(phase) for phase in phases], **overrides)
            return make_outcome(plan=plan, result=result)
        code, _gate, _calls, _plans, _work, out = self.compare(
            listing=listing, scenarios=(selected,), options=("--counters", "--counters-runs", "2"),
            head_manifest=COUNTERS_MANIFEST, base_manifest=BASE_COUNTERS_MANIFEST, counters_run=counters_run)
        self.assertEqual(code, 0, self.printed)
        return (out / "comparison.md").read_text(encoding="utf-8")

    def test_atlas_retry_reports_recovery_activity_beside_its_recovery_table(self):
        # S1/atlas-retry's recovery hold reports its elapsed hold and CPU from the counters runs, and the recovery
        # table stays.
        recovery = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
        listing = {**ATLAS_RETRY_LIST, "capabilities": {"latency_split_schema": 1, "phase_kinds": 1}}
        document = self.counters_only(listing, "S1/atlas-retry", [kind_phase("hold", name="recovery")],
                                      scenario="S1", variant="atlas-retry", atlas_recovery=recovery)
        section = document.partition("### Counters-only workloads")[2].partition("\n### ")[0]
        self.assertIn("recovery hold (s)", section)
        self.assertIn("recovery CPU (s)", section)
        self.assertNotIn("recovery presented frames (fps)", section)
        self.assertIn("### Atlas retry recovery", document)

    def test_an_older_harness_counters_only_variant_goes_to_the_unclassified_block(self):
        # Counters-only runs of a harness without phase kinds are kept as recorded in the unclassified block.
        recovery = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
        document = self.counters_only(ATLAS_RETRY_LIST, "S1/atlas-retry", [valid_result()["phases"][0]],
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




def kind_outcome(*phases, latency=None, throughput=None, evidence="/e/01-head"):
    """A valid run outcome of a phase-kinds harness carrying `phases` (kind_phase bodies)."""
    result = valid_result(phases=list(phases), latency=latency, throughput=throughput)
    return make_outcome(result=result, evidence=Path(evidence))


def transition_phase(completion=None, missing=None, **fields):
    """A transition phase reaching its endpoint `completion` ms after its start, or missing it for `missing`."""
    ending = {"completion_ms": completion} if missing is None else {"completion_missing": missing}
    return kind_phase("transition", name="image", endpoint="image-registered-then-presented", **ending, **fields)


def side_with(*outcomes):
    return perf.SideRuns(list(outcomes))


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
        rows = perf.comparison_rows("S11/release", base, head)
        completion = row_for(rows, "image completion (ms)")
        self.assertEqual(completion[2:], [
            "40.00 (40.00–40.00), 1/2 runs, single observation; 01-base 40.00, 04-base n/a: expired",
            "55.00 (50.00–60.00), 2/2 runs; 02-head 50.00, 03-head 60.00", "+37.5%"])
        self.assertEqual(row_for(rows, "image first present (ms)")[2], "1.00 (1.00–1.00)")
        self.assertEqual(row_for(rows, "image presented frames (count)")[3], "120.00 (120.00–120.00)")
        self.assertEqual(row_for(rows, "image non-presenting redraws (count)")[3], "10.00 (10.00–10.00)")
        metrics = [row[1] for row in rows]
        self.assertFalse([metric for metric in metrics if "fps" in metric or "interval" in metric], metrics)

    def test_a_hold_reports_activity_counts_only(self):
        # A hold is idle by design: its seconds, presented frames, redraws requested and CPU, no rate or interval.
        hold = side_with(kind_outcome(kind_phase("hold", name="idle")))
        metrics = [row[1] for row in perf.comparison_rows("S1/default", hold, hold)]
        idle = [metric for metric in metrics if metric.startswith("idle ")]
        self.assertEqual(idle, ["idle hold (s)", "idle presented frames (count)",
                                "idle redraws requested (count)", "idle CPU (s)"])

    def test_a_sustained_phase_and_a_legacy_phase_keep_their_rows(self):
        # Sustained phases and phases of a harness that predates kinds report what they reported before.
        for phase in (kind_phase("sustained"), valid_result()["phases"][0]):
            with self.subTest(kind=phase.get("kind")):
                side = side_with(kind_outcome(phase))
                metrics = [row[1] for row in perf.comparison_rows("S2/default", side, side)]
                self.assertIn("workload presented frames (fps)", metrics)
                self.assertIn("workload present interval p95 (ms)", metrics)
                self.assertNotIn("workload completion (ms)", metrics)


def flag_result(label, base_outcomes, head_outcomes, set_name="timed"):
    return perf.SetResult(label, set_name, side_with(*base_outcomes), side_with(*head_outcomes))


def sustained_with(intervals=(16.6, 16.7), frames=120, wall_s=60.0):
    return kind_phase("sustained", present_interval_ms=list(intervals), presented_frames=frames,
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
        busy = [kind_outcome(kind_phase("hold", name="idle", presented_frames=5000))]
        quiet = [kind_outcome(kind_phase("hold", name="idle", presented_frames=1))]
        self.assertEqual(self.checks(flag_result("S1/default", quiet, busy)), [])
        legacy = [make_outcome()]
        self.assertEqual(self.checks(flag_result("S2/default", legacy, legacy)), [])
        base = [kind_outcome(sustained_with(frames=120))]
        head = [kind_outcome(sustained_with(frames=60))]
        directions = dict(require(perf, "FLAG_DIRECTIONS", self))
        del directions["fps"]
        with mock.patch.object(perf, "FLAG_DIRECTIONS", directions):
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
        document = perf.comparison_document([], [], [], [], [], flag_checks=checks)
        self.assertIn("### Candidate flags", document)
        self.assertIn("| S11/release | timed | image | completion | median | 40.00–40.00 | 90.00 | higher is worse |",
                      document)
        unflagged = self.checks(flag_result("S11/release", base, base))
        self.assertIn("None of the 1 checks", perf.comparison_document([], [], [], [], [], flag_checks=unflagged))
        self.assertNotIn("Candidate flags", perf.comparison_document([], [], [], [], [], flag_checks=None))
        self.assertNotIn("Candidate flags", perf.comparison_document([], [], [], [], [], flag_checks=[]))


class RunIdentityTests(CompareHarness, unittest.TestCase):
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
                                         "capabilities"})
        # The harness's declared capabilities, so perf-flags validates each result as perf-compare did.
        self.assertEqual(identity["capabilities"], {"latency_split_schema": None, "phase_kinds": None})
        # Each set's final inventory: per side its blocked or failed status and the attempt directories it accepted,
        # so perf-flags binds results to accepted executions, never to whatever valid-looking files it finds.
        self.assertEqual(identity["sets"], [{"label": "S1/default", "dataset": "timed",
                                             "base": {"status": "", "accepted": ["01-base"]},
                                             "head": {"status": "", "accepted": ["02-head"]}}])

    def test_a_phase_kinds_comparison_records_its_capabilities(self):
        # A head whose list declares the split schema and phase kinds records both.
        _code, _gate, _calls, _plans, _work, out = self.compare(listing=LIST_WITH_KINDS)
        identity = json.loads((out / "run-identity.json").read_text(encoding="utf-8"))
        self.assertEqual(identity.get("capabilities"), {"latency_split_schema": 1, "phase_kinds": 1})

    def test_a_failed_side_lists_no_accepted_attempt(self):
        # A side that ends blocked or failed discards its valid attempts, and the inventory says so.
        valid = make_outcome()
        # The base kept an outcome object, but its blocked status alone decides: it accepts nothing.
        result = perf.SetResult("S1/default", "timed", perf.SideRuns([valid], blocked="exit 5"),
                                perf.SideRuns([valid]),
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
            return lambda plan: make_outcome(plan=plan, result=valid_result(
                monitor=monitor, phases=[transition_phase(completion_ms)]))
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=runs_with(40.0), head_run=runs_with(90.0))
        self.assertEqual(code, 0)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("### Candidate flags", document)
        self.assertIn("| S1/default | timed | image | completion | median | 40.00–40.00 | 90.00 |", document)


class PhaseKindCoverageTests(CompareHarness, unittest.TestCase):
    def test_an_image_whose_completion_improves_while_frames_fall_is_not_flagged(self):
        # macOS S11/release image: completion improves 222.870 -> 142.500 ms while presented frames fall 2 -> 1.
        # The completion falls, so nothing is flagged, and no frame rate is reported to suggest a regression.
        base = [kind_outcome(transition_phase(222.870, presented_frames=2, last_present_seq=2))]
        head = [kind_outcome(transition_phase(142.500, presented_frames=1, last_present_seq=1))]
        result = flag_result("S11/release", base, head)
        self.assertEqual([check.flagged for check in perf.flag_checks([result])], [False])
        rows = perf.comparison_rows("S11/release", result.base, result.head)
        self.assertEqual(row_for(rows, "image completion (ms)")[4], "-36.1%")
        self.assertFalse([row[1] for row in rows if "fps" in row[1]])

    def test_recovery_reports_activity_and_its_own_table_but_no_rates_or_flags(self):
        # S1/atlas-retry's recovery is a hold: activity counts and the dedicated recovery table only, with no
        # generic rate, interval, completion or candidate flag.
        def recovery_run():
            outcome = kind_outcome(kind_phase("hold", name="recovery"))
            outcome.result["atlas_recovery"] = {"episodes": 8, "distinct_keys": 70, "records": recovery_records()}
            return outcome
        base, head = side_with(recovery_run()), side_with(recovery_run())
        metrics = [row[1] for row in perf.comparison_rows("S1/atlas-retry", base, head)
                   if row[1].startswith("recovery ")]
        self.assertEqual(metrics, ["recovery hold (s)", "recovery presented frames (count)",
                                   "recovery redraws requested (count)", "recovery CPU (s)"])
        self.assertEqual(perf.flag_checks([perf.SetResult("S1/atlas-retry", "counters", base, head)]), [])
        self.assertTrue(perf.atlas_recovery_rows("S1/atlas-retry", base, head))

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
            return lambda plan: make_outcome(plan=plan, result=valid_result(
                monitor=monitor, phases=[transition_phase(completion_ms)]))
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=runs_with(40.0), head_run=runs_with(41.0))
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertNotIn(heading, document)
        self.assertIn("| S1/default | image completion (ms) |", document)

    def test_an_unclassified_set_contributes_no_candidate_flag(self):
        # A set with any unkinded run is reported only in the unclassified block, so none of its runs, kinded
        # or not, is read by kind: no check, however far its kinded runs moved.
        legacy = make_outcome()
        base = [kind_outcome(transition_phase(40.0)), legacy]
        head = [kind_outcome(transition_phase(900.0))]
        result = flag_result("S11/release", base, head)
        self.assertTrue(perf.unclassified_result(result))
        self.assertEqual(perf.flag_checks([result]), [])

    def test_a_run_with_any_unkinded_phase_is_unclassified(self):
        # One phase without a kind makes the whole run unreadable by kind, even when its other phases have one;
        # a run whose every phase has a known kind is classified.
        legacy_phase = valid_result()["phases"][0]
        mixed = flag_result("S2/default", [kind_outcome(kind_phase(), dict(legacy_phase, name="idle"))],
                            [kind_outcome(kind_phase())])
        kinded = flag_result("S2/default", [kind_outcome(kind_phase())], [kind_outcome(kind_phase())])
        self.assertEqual((perf.unclassified_result(mixed), perf.unclassified_result(kinded)), (True, False))


class HarnessCfgDetectionTests(unittest.TestCase):
    """One harness cfg decision for both sides, from each tree's owning crates."""

    def test_a_cfg_is_on_only_when_both_trees_define_every_method_in_code(self):
        # Every method in both trees turns the cfg on; one missing from either tree, or present only in a comment,
        # a string or a unit-test file, or in another crate than its owner, turns it off for both.
        table = REAL_GATE.HARNESS_API_CFGS
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)

            def tree(name, skip=None, extra=None):
                built = root / name
                write_atlas_retry_api(built, skip=skip)
                if extra is not None:
                    relative, body = extra
                    (built / relative).parent.mkdir(parents=True, exist_ok=True)
                    (built / relative).write_text(body, encoding="utf-8")
                return built
            full = tree("full")
            cases = {
                "both": (full, tree("both"), ("perf_atlas_retry_api",)),
                "base lacks one": (full, tree("lacks", skip="last_missing_chrome"), ()),
                "head lacks one": (tree("head-lacks", skip="font_fallback_notice_id"), full, ()),
                "a comment": (full, tree("comment", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/notes.rs", "// pub fn last_missing_chrome(\n")), ()),
                "a string": (full, tree("string", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/notes.rs", 'const NAME: &str = "\npub fn last_missing_chrome(";\n')),
                    ()),
                "a unit-test file": (full, tree("tests", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/core_tests.rs", "pub fn last_missing_chrome() {}\n")), ()),
                "another crate": (full, tree("crate", "last_missing_chrome", (
                    "crates/sonicterm-app/src/other.rs", "pub fn last_missing_chrome() {}\n")), ()),
            }
            for name, (head, base, expected) in cases.items():
                with self.subTest(name):
                    decided, _per_tree = perf.harness_cfg_decision({"head": head, "base": base}, table)
                    self.assertEqual(decided, expected)


class AtlasRetryAvailabilityTests(CompareHarness, unittest.TestCase):
    """S1/atlas-retry is a declared diagnostic: unavailable on both sides when its harness cfg is off."""

    def test_a_previous_release_base_leaves_the_ordinary_sets_and_marks_atlas_retry_unavailable(self):
        # A base without the methods (as v1.3.8) turns the cfg off for both sides: S1/default still runs both its
        # sets strictly, and S1/atlas-retry runs nothing, with its reason in the table and the details.
        code, gate, _calls, plans, _work, out = self.compare(
            listing=ATLAS_RETRY_LIST, scenarios=("S1", "S1/atlas-retry"), api_trees=("head",),
            options=("--counters", "--counters-runs", "1", "--require-base"),
            head_manifest=COUNTERS_MANIFEST, base_manifest=BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, perf.EXIT_PASS, self.printed)
        self.assertTrue(plans)
        self.assertEqual({plan.variant for plan in plans}, {"default"}, "atlas-retry is never planned")
        self.assertTrue(all(step.harness_cfgs == () for step in gate.steps if step.id.startswith("build-")))
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("unavailable on both sides: perf_atlas_retry_api is off because the base tree lacks", document)
        self.assertIn("| S1/atlas-retry | counters set | unavailable | unavailable |", document)

    def test_two_current_trees_schedule_atlas_retry_and_need_its_runs(self):
        # Both trees have the methods, so the cfg is on and S1/atlas-retry runs its counters plans, needing every
        # valid run under --require-base.
        code, gate, _calls, plans, _work, _out = self.compare(
            listing=ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",),
            options=("--counters", "--counters-runs", "2", "--require-base"),
            head_manifest=COUNTERS_MANIFEST, base_manifest=BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, perf.EXIT_PASS, self.printed)
        self.assertEqual(sorted(plan.side for plan in plans), ["base", "base", "head", "head"])
        self.assertTrue(all(step.harness_cfgs == ("perf_atlas_retry_api",)
                            for step in gate.steps if step.id.startswith("build-")))
        short = [perf.SetResult("S1/atlas-retry", "counters", perf.SideRuns([make_outcome()]),
                                perf.SideRuns([]), target_runs=1)]
        self.assertTrue(perf.strict_problems(short), "a missing atlas-retry run is still a strict problem")

    def test_a_selection_of_only_unavailable_diagnostics_does_not_pass(self):
        # With the cfg off, selecting only S1/atlas-retry leaves nothing runnable: blocked without --require-base
        # and failed with it, never passed.
        for options, expected in ((("--counters",), perf.EXIT_BLOCKED),
                                  (("--counters", "--require-base"), perf.EXIT_FAIL)):
            with self.subTest(options=options):
                code, *_rest = self.compare(listing=ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",),
                                            api_trees=("head",), options=options,
                                            head_manifest=COUNTERS_MANIFEST, base_manifest=BASE_COUNTERS_MANIFEST)
                self.assertEqual(code, expected, self.printed)
        unavailable = [perf.SetResult("S1/atlas-retry", "counters", perf.SideRuns(blocked="off"),
                                      perf.SideRuns(blocked="off"), unavailable="off")]
        self.assertEqual(perf.strict_problems(unavailable), ["no runnable scenario set: S1/atlas-retry off"])

    def test_unexpected_blocking_stays_a_strict_problem(self):
        # Only a set declared unavailable is excused; any other blocked side still fails --require-base.
        blocked = [perf.SetResult("S1/default", "timed", perf.SideRuns(blocked="the base cannot build"),
                                  perf.SideRuns([make_outcome()]), target_runs=1)]
        self.assertEqual(perf.comparison_exit(blocked, require_base=True), perf.EXIT_FAIL)


class HarnessCfgPrebuiltTests(PrebuiltHarness, unittest.TestCase):
    """The harness cfg decision and the composed flags are part of the build identity a consumer checks."""

    def test_load_prebuilt_refuses_another_cfg_decision_or_other_flags(self):
        # The producer records the decision and the flags; a manifest naming other ones is refused.
        binaries, digest = self.produce()
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["harness_cfgs"], ["perf_atlas_retry_api"])
        self.assertEqual(manifest["rustflags"], ["--check-cfg", "cfg(perf_atlas_retry_api)", "--cfg",
                                                 "perf_atlas_retry_api"])
        for key, value, message in (("harness_cfgs", [], "harness cfgs"),
                                    ("rustflags", ["--check-cfg", "cfg(perf_atlas_retry_api)"], "compiler flags")):
            with self.subTest(key=key):
                changed = self.rewrite(binaries, lambda data, key=key, value=value: data.update({key: value}))
                with self.assertRaisesRegex(ValueError, f"refusing the prebuilt binaries: {message}"):
                    self.consume(binaries, changed)
                self.rewrite(binaries, lambda data, key=key: data.update({key: manifest[key]}))


class PreviousReleaseCheckTests(unittest.TestCase):
    """The head's harness overlaid on the previous release tag must still build."""

    HEAD, PARENT, BASE = "a" * 40, "b" * 40, "c" * 40

    def check(self, *, shallow="false", tags="v1.3.8\n", status="PASS", base_api=False):
        """Run the check against a fake repository; return its exit code, the gate and the git calls."""
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        root = Path(scratch.name)
        head = root / "head"
        (head / perf.APP_MANIFEST).parent.mkdir(parents=True)
        (head / perf.APP_MANIFEST).write_text(HEAD_MANIFEST, encoding="utf-8")
        (head / perf.HARNESS_DIRECTORY).mkdir(parents=True)
        (head / perf.HARNESS_DIRECTORY / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
        write_atlas_retry_api(head)
        calls = []

        def host_run(argv, timeout_s=perf.GIT_TIMEOUT_S):
            calls.append(tuple(argv))
            words = tuple(argv[3:]) if tuple(argv[:2]) == ("git", "-C") else tuple(argv[1:])
            answers = {("rev-parse", "--is-shallow-repository"): shallow,
                       ("rev-parse", "--verify", "HEAD^{commit}"): self.HEAD,
                       ("rev-list", "--parents", "-n", "1", self.HEAD): f"{self.HEAD} {self.PARENT}",
                       ("tag", "--list", "v[0-9]*", "--merged", self.PARENT): tags,
                       ("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*", self.PARENT): "v1.3.8",
                       ("rev-parse", "--verify", "v1.3.8^{commit}"): self.BASE}
            if words[:2] == ("worktree", "add"):
                tree = Path(words[-2])
                (tree / perf.APP_MANIFEST).parent.mkdir(parents=True)
                (tree / perf.APP_MANIFEST).write_text(BASE_MANIFEST, encoding="utf-8")
                if base_api:
                    write_atlas_retry_api(tree)
                return command(argv, "")
            if words[:2] == ("worktree", "remove"):
                shutil.rmtree(words[-1], ignore_errors=True)
                return command(argv, "")
            return command(argv, answers.get(words, "") + "\n")
        gate = FakeGate(lambda step: (status, 0 if status == "PASS" else 101,
                                      "" if status == "PASS" else "error[E0599]: no method named `x`\n"))
        work = root / "work"
        code = perf.check_previous_release(gate, host_run, root / "out", work, {}, head_root=head)
        self.assertFalse(work.exists(), "the check removes only, and all, the scratch state it created")
        return code, gate, calls

    def test_the_previous_tag_is_checked_with_the_derived_cfgs_in_an_isolated_target(self):
        # The previous tag of HEAD's parent is checked with the base's features and the decision (off for a base
        # without the methods, on for one with them), in the check's own target directory.
        code, gate, calls = self.check()
        self.assertEqual(code, perf.EXIT_PASS)
        self.assertIs(gate.steps[0], REAL_GATE.PREVIOUS_RELEASE_CHECKS[((), ())])
        self.assertTrue(gate.environs[0]["CARGO_TARGET_DIR"].endswith("work/target"))
        self.assertIn(("git", "worktree", "add", "--detach", str(gate.roots[0]), self.BASE), calls)
        code, gate, _calls = self.check(base_api=True)
        self.assertIs(gate.steps[0], REAL_GATE.PREVIOUS_RELEASE_CHECKS[(("perf_atlas_retry_api",), ())])

    def test_a_failed_check_fails_and_a_shallow_clone_is_refused(self):
        # A harness that does not build on the tag fails the step; a shallow checkout cannot select the tag.
        self.assertEqual(self.check(status="FAIL")[0], perf.EXIT_FAIL)
        with self.assertRaisesRegex(ValueError, "shallow"):
            self.check(shallow="true")

    def test_no_earlier_tag_has_nothing_to_check(self):
        # Before the first release there is no tag to build against, so nothing is built.
        code, gate, _calls = self.check(tags="")
        self.assertEqual((code, gate.steps), (perf.EXIT_PASS, []))

    def test_the_mode_takes_only_out(self):
        # The check is its own gate step; a comparison option with it is a usage error.
        self.assertTrue(perf.parse_args(["--check-previous-release"]).check_previous_release)
        for extra in (["--base", "main"], ["--smoke"], ["--counters"], ["--keep"]):
            with self.subTest(extra=extra), self.assertRaises(SystemExit), \
                    contextlib.redirect_stderr(io.StringIO()):
                perf.parse_args(["--check-previous-release", *extra])


if __name__ == "__main__":
    unittest.main(verbosity=2)
