"""Perf processes tests."""

from __future__ import annotations
from perf_test_fixture import perf
import os
import sys
import unittest

import perf_processes
import perf_sessions
# Imported for its effect: pins perf_sessions.own_pid so no fixture pid can equal this process's pid.
import perf_own_pid_fixture  # noqa: F401


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
            raise perf_sessions.ProcessUnreadable(f"pid {pid} cannot be read")
        return perf_sessions.ProcessInfo(pid, process.pgid, process.sid, process.start,
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


ACKED = perf_sessions.AckedSession("0", 500, 501, "10.1", "10.2")


class SessionRecordTests(unittest.TestCase):
    def test_documented_record_parses(self):
        # The scenario script's record names its role, leader, anchor and tty.
        record = perf_sessions.parse_session_record(RECORD_TEXT, "0")
        self.assertEqual(record, perf_sessions.SessionRecord("0", 500, 501, "/dev/ttys009"))

    def test_numeric_role_from_the_generated_script_parses(self):
        # The role script writes `"role":%s`, so the role is a bare number matching the file name.
        script_record = '{"role":0,"leader_pid":500,"anchor_pid":501,"tty":"none"}'
        self.assertEqual(perf_sessions.parse_session_record(script_record, "0"), perf_sessions.SessionRecord("0", 500, 501, "none"))
        for text in (script_record.replace('"role":0', '"role":1'), script_record.replace('"role":0', '"role":true'),
                     script_record.replace('"role":0', '"role":0.0')):
            with self.subTest(text=text), self.assertRaises(ValueError):
                perf_sessions.parse_session_record(text, "0")

    def test_malformed_records_are_refused(self):
        # A record that cannot name two distinct positive pids for its own role is refused.
        for text in ("not json", "[]", RECORD_TEXT.replace('"0"', '"1"'),
                     RECORD_TEXT.replace("501", "500"), RECORD_TEXT.replace("501", "true"),
                     RECORD_TEXT.replace("501", '"501"'), RECORD_TEXT.replace("500", "0"),
                     '{"role": "0", "leader_pid": 500, "tty": ""}'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                perf_sessions.parse_session_record(text, "0")


class SessionValidationTests(unittest.TestCase):
    RECORD = perf_sessions.SessionRecord("0", 500, 501, "/dev/ttys009")

    def test_live_anchor_in_the_leaders_session_and_group_is_acknowledged(self):
        # Validation stores both start times, which later identify the same processes.
        acked, problem = perf_sessions.validate_session(self.RECORD, FakeTable(leader(), anchor()), LAUNCH_UNIX_S)
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
                acked, problem = perf_sessions.validate_session(self.RECORD, FakeTable(*processes), LAUNCH_UNIX_S)
                self.assertIsNone(acked)
                self.assertTrue(problem)


class SessionCleanupTests(unittest.TestCase):
    def clean(self, table, sessions=(ACKED,)):
        clock = FakeClock()
        result = perf_sessions.cleanup_sessions(table, list(sessions), clock=clock, sleep=clock.sleep)
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
        second = perf_sessions.AckedSession("1", 700, 701, "20.1", "20.2")
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
        self.assertGreaterEqual(clock.now - 1000.0, perf_sessions.CLEANUP_BOUND_S)

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
        return perf.struct.pack(perf_processes.BSDINFO_FORMAT, 0, status, 0, pid, ppid, 501, 20, 501, 20, 501, 20, 0,
                                comm, b"", 3, pgid, 0, 0, 0, 0, start_s, start_us)

    def test_bsdinfo_record_is_136_bytes_and_decodes_its_identity(self):
        # The layout must match the 136 bytes `proc_pidinfo(PROC_PIDTBSDINFO)` writes.
        self.assertEqual(perf_processes.BSDINFO_SIZE, 136)
        info = perf_processes.decode_bsdinfo(self.bsdinfo(), sid=4000)
        self.assertEqual((info.pid, info.ppid, info.pgid, info.sid, info.command),
                         (4321, 1, 4000, 4000, "perf_scenarios"))
        self.assertEqual(info.start, "1700000000.000042")
        self.assertAlmostEqual(info.start_unix_s, 1700000000.000042)

    def test_bsdinfo_zombie_reads_as_gone(self):
        # A zombie runs nothing and is no session member to signal.
        self.assertIsNone(perf_processes.decode_bsdinfo(self.bsdinfo(status=5), sid=4000))

    def test_proc_stat_with_parentheses_in_the_command_name(self):
        # The command name may hold spaces and parentheses; fields follow the last `)`.
        line = "1234 (my (odd) cmd) S 1 1230 1200 34816 1230 4194304 0 0 0 0 0 0 0 0 20 0 1 0 5000 1 2\n"
        info = perf_processes.parse_proc_stat(line, 1234, boot_unix_s=1000.0, ticks_per_s=100)
        self.assertEqual((info.pid, info.ppid, info.pgid, info.sid, info.command, info.start),
                         (1234, 1, 1230, 1200, "my (odd) cmd", "5000"))
        self.assertEqual(info.start_unix_s, 1050.0)

    def test_proc_stat_zombie_reads_as_gone_and_garbage_is_unreadable(self):
        # A zombie is gone; a record that cannot be parsed is never read as gone.
        zombie = "1234 (sh) Z 1 1230 1200 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5000 1 2\n"
        self.assertIsNone(perf_processes.parse_proc_stat(zombie, 1234, 1000.0, 100))
        for garbage in ("", "1234 sh S 1", "1234 (sh) S 1 x 1200"):
            with self.subTest(garbage=garbage), self.assertRaises(perf_sessions.ProcessUnreadable):
                perf_processes.parse_proc_stat(garbage, 1234, 1000.0, 100)


@unittest.skipUnless(sys.platform == "darwin" or sys.platform.startswith("linux"),
                     "the real process table exists on macOS and Linux")
class LiveProcessTableTests(unittest.TestCase):
    def test_reading_this_process_matches_the_kernel(self):
        # A read-only check of this test process proves the record offsets on the real host.
        table = perf_processes.make_process_table()
        info = table.read(os.getpid())
        self.assertEqual((info.pid, info.ppid, info.pgid, info.sid),
                         (os.getpid(), os.getppid(), os.getpgid(0), os.getsid(0)))
        self.assertLess(abs(info.start_unix_s - perf.time.time()), 3600)
        self.assertIn(os.getpid(), table.pids())
        self.assertEqual(table.session_of(os.getpid()), os.getsid(0))

    def test_a_pid_above_every_hosts_limit_reads_as_gone(self):
        # Linux caps pids at 2**22 and macOS at 99999, so this pid cannot exist.
        table = perf_processes.make_process_table()
        self.assertIsNone(table.read(2**22 + 1))
        self.assertIsNone(table.session_of(2**22 + 1))


STAMP = "2026-10-02T11:24:16.123456Z"
STAMP_UNIX_S = 1790940256.123456  # calendar.timegm of 2026-10-02T11:24:16Z, plus the fraction
