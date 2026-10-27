"""Perf windows tests."""

from __future__ import annotations
from perf_test_fixture import perf
import os
import unittest
from unittest import mock

import perf_focus
import perf_outcomes
import perf_processes
import perf_processes_tests
import perf_sessions
import perf_settings
import perf_watcher_tests


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
    # Resolve this runtime-only dependency after module initialization.
    import perf_watcher_tests
    return perf_processes_tests.FakeTable(perf_watcher_tests.harness_process(pgid=0, sid=0, command="perf_scenarios.exe", ppid=4),
                     perf_processes_tests.FakeProcess(PROGRAM_PID, 0, 0, start="21", command="perf_scenarios.exe",
                                 start_unix_s=1001.5, ppid=perf_watcher_tests.HARNESS_PID))


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
KERNEL_ROWS = [(4, 0, "System"), (700, 4, "explorer.exe"), (perf_watcher_tests.HARNESS_PID, 700, "perf_scenarios.exe"),
               (PROGRAM_PID, perf_watcher_tests.HARNESS_PID, "perf_scenarios.exe")]
KERNEL_CREATED = {4: filetime(10.0), 700: filetime(500.0), perf_watcher_tests.HARNESS_PID: filetime(1000.5),
                  PROGRAM_PID: filetime(1001.5)}


class WindowsProcessTableTests(unittest.TestCase):
    def kernel(self):
        return FakeKernel(KERNEL_ROWS, KERNEL_CREATED)

    def test_toolhelp_rows_and_creation_times_become_process_records(self):
        # The image name is the command, the Toolhelp parent is ppid, and the raw creation time is identity.
        # Resolve this runtime-only dependency after module initialization.
        import perf_watcher_tests
        kernel = self.kernel()
        table = perf_processes.WindowsProcessTable(kernel)
        self.assertEqual(table.pids(), [4, 700, perf_watcher_tests.HARNESS_PID, PROGRAM_PID])
        info = table.read(PROGRAM_PID)
        self.assertEqual((info.pid, info.ppid, info.command, info.pgid, info.sid),
                         (PROGRAM_PID, perf_watcher_tests.HARNESS_PID, "perf_scenarios.exe", 0, 0))
        self.assertEqual(info.start, str(filetime(1001.5)))
        self.assertAlmostEqual(info.start_unix_s, 1001.5, places=6)
        self.assertEqual(sorted(kernel.closed), sorted(kernel.handles))

    def test_gone_exited_and_unreadable_processes_read_apart(self):
        # A pid that is unlisted or has exited is gone; one the kernel refuses is unreadable, never gone.
        # Resolve this runtime-only dependency after module initialization.
        import perf_watcher_tests
        kernel = self.kernel()
        table = perf_processes.WindowsProcessTable(kernel)
        self.assertIsNone(table.read(12345))
        kernel.exited.add(PROGRAM_PID)
        self.assertIsNone(table.read(PROGRAM_PID))
        kernel.denied.add(4)
        with self.assertRaises(perf_sessions.ProcessUnreadable):
            table.read(4)
        kernel.snapshot_fails = True
        self.assertIsNone(table.pids())
        with self.assertRaises(perf_sessions.ProcessUnreadable):
            table.read(perf_watcher_tests.HARNESS_PID)
        self.assertEqual(sorted(kernel.closed), sorted(kernel.handles))

    def test_terminate_rechecks_the_creation_time_so_a_reused_pid_is_spared(self):
        # The pid is reopened and its creation time compared before TerminateProcess, so a reused pid lives.
        # Resolve this runtime-only dependency after module initialization.
        import perf_watcher_tests
        kernel = self.kernel()
        table = perf_processes.WindowsProcessTable(kernel)
        self.assertEqual(table.terminate(perf_watcher_tests.HARNESS_PID, str(filetime(999.0))), "stale")
        self.assertEqual(kernel.terminated, [])
        self.assertEqual(table.terminate(perf_watcher_tests.HARNESS_PID, str(filetime(1000.5))), "sent")
        self.assertEqual(kernel.terminated, [(perf_watcher_tests.HARNESS_PID, 124)])
        self.assertEqual(kernel.opened[-1][1], perf_processes.PROCESS_TERMINATE | perf_processes.PROCESS_QUERY_LIMITED_INFORMATION)
        self.assertEqual(table.terminate(12345, "1"), "gone")
        kernel.denied.add(4)
        self.assertEqual(table.terminate(4, str(filetime(10.0))), "refused")
        self.assertEqual(sorted(kernel.closed), sorted(kernel.handles))

    def test_windows_hosts_get_the_windows_table(self):
        # make_process_table serves _accept_harness_pid and other_instance_alive on every supported host.
        with mock.patch.object(perf.sys, "platform", "win32"):
            self.assertIsInstance(perf_processes.make_process_table(), perf_processes.WindowsProcessTable)


class ProgramRecordTests(unittest.TestCase):
    def test_a_record_names_its_role_and_a_positive_pid(self):
        # The record must name the role its file is named for, a positive pid, and a string tty.
        self.assertEqual(perf_sessions.parse_program_record(PROGRAM_TEXT, "0"), perf_sessions.ProgramRecord("0", PROGRAM_PID, "none"))
        for text in (PROGRAM_TEXT.replace('"role": 0', '"role": 1'), PROGRAM_TEXT.replace("950", "0"),
                     '{"role": 0, "program_pid": 950}', "[]"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                perf_sessions.parse_program_record(text, "0")

    def test_a_valid_program_is_acknowledged(self):
        # A live harness child running the harness image, created after launch, is stored with its creation time.
        # Resolve this runtime-only dependency after module initialization.
        import perf_watcher_tests
        record = perf_sessions.parse_program_record(PROGRAM_TEXT, "0")
        acked, problem = perf_sessions.validate_program(record, program_table(), perf_watcher_tests.HARNESS_PID, perf_processes_tests.LAUNCH_UNIX_S,
                                               harness_command="PERF_SCENARIOS.EXE")
        self.assertEqual((acked, problem), (perf_sessions.AckedProgram("0", PROGRAM_PID, "21"), None))

    def test_a_wrong_parent_image_or_creation_time_is_refused(self):
        # Each check refuses a process that may not be this run's role program, naming what failed.
        # Resolve this runtime-only dependency after module initialization.
        import perf_watcher_tests
        record = perf_sessions.parse_program_record(PROGRAM_TEXT, "0")
        cases = {"parent": {"ppid": 4}, "image": {"command": "cmd.exe"}, "before": {"start_unix_s": 900.0}}
        for word, overrides in cases.items():
            with self.subTest(word):
                table = program_table()
                for name, value in overrides.items():
                    setattr(table.processes[PROGRAM_PID], name, value)
                acked, problem = perf_sessions.validate_program(record, table, perf_watcher_tests.HARNESS_PID, perf_processes_tests.LAUNCH_UNIX_S,
                                                       harness_command="perf_scenarios.exe")
                self.assertIsNone(acked)
                self.assertIn(word, problem)
        table = program_table()
        table.processes[PROGRAM_PID].alive = False
        self.assertIn("not alive", perf_sessions.validate_program(record, table, perf_watcher_tests.HARNESS_PID, perf_processes_tests.LAUNCH_UNIX_S,
                                                         harness_command="perf_scenarios.exe")[1])
        harness_record = perf_sessions.ProgramRecord("0", perf_watcher_tests.HARNESS_PID, "none")
        self.assertIsNone(perf_sessions.validate_program(harness_record, program_table(), perf_watcher_tests.HARNESS_PID, perf_processes_tests.LAUNCH_UNIX_S,
                                                harness_command="perf_scenarios.exe")[0])


class CustodyCleanupTests(unittest.TestCase):
    def test_verified_custody_settles_the_run(self):
        # The gate's rule: the job emptied, the bootstrap reaped, protocol and capture complete, no errors.
        self.assertTrue(perf_outcomes.custody_cleanup(custody()).passed)
        self.assertTrue(perf_outcomes.custody_cleanup(custody(active=2, cleanup="terminated")).passed)

    def test_unverified_or_missing_custody_is_unresolved(self):
        # Without proof that the job ended every process, cleanup cannot clear the run.
        cases = {"not empty": {"empty": False}, "bootstrap not reaped": {"bootstrap_reaped": False},
                 "protocol incomplete": {"protocol_complete": False}, "capture incomplete": {"capture_complete": False},
                 "errors": {"errors": ["cleanup accounting: OSError"]}}
        for name, overrides in cases.items():
            with self.subTest(name):
                self.assertFalse(perf_outcomes.custody_cleanup(custody(**overrides)).passed)
        self.assertFalse(perf_outcomes.custody_cleanup(None).passed)

    def test_leftovers_are_zero_only_for_a_verified_deadline_kill(self):
        # The deadline kill expects members alive at its moment; any other count is members that outlived the run.
        alive = custody(active=3, cleanup="terminated")
        self.assertEqual(perf_outcomes.windows_leftover_processes(alive, deadline_case=True), 0)
        self.assertEqual(perf_outcomes.windows_leftover_processes(alive, deadline_case=False), 3)
        unverified = custody(active=3, cleanup="terminated", empty=False)
        self.assertEqual(perf_outcomes.windows_leftover_processes(unverified, deadline_case=True), 3)
        self.assertEqual(perf_outcomes.windows_leftover_processes(custody(), deadline_case=False), 0)
        self.assertIsNone(perf_outcomes.windows_leftover_processes(custody(before_cleanup=None), deadline_case=False))
        self.assertIsNone(perf_outcomes.windows_leftover_processes(None, deadline_case=True))

    def test_a_forced_job_end_is_fail_for_the_harness_step(self):
        # The harness step is synthetic and strict, so run_step reports FAIL, not CLEANED_NOT_NATURAL,
        # when it had to end the job.
        gate = perf_settings.load_gate()
        raw = {"interrupted": False, "timed_out": False, "launch_failed": False, "errors": [], "exit_code": 0,
               "custody": custody(active=1, cleanup="terminated"), "natural": False}
        self.assertEqual(gate._phase_status(raw, gate.WindowsPolicy.STRICT), gate.FAIL)
        self.assertEqual(gate.Step("harness", ("x",), ("windows",), 1, "local", (), ()).windows_policy,
                         gate.WindowsPolicy.STRICT)


@unittest.skipUnless(os.name == "nt", "the real Windows process table exists only on Windows")
class LiveWindowsTests(unittest.TestCase):
    def test_reading_this_process_matches_the_kernel(self):
        # A read-only check of this interpreter proves the Toolhelp and GetProcessTimes bindings.
        table = perf_processes.make_process_table()
        info = table.read(os.getpid())
        self.assertTrue(info.command.lower().startswith("python"), info.command)
        self.assertEqual((info.pid, info.ppid), (os.getpid(), os.getppid()))
        self.assertTrue(int(info.start) > 0)
        self.assertLess(abs(info.start_unix_s - perf.time.time()), 3600)
        self.assertIn(os.getpid(), table.pids())
        self.assertIsNone(table.read(2**31 - 4))

    def test_the_foreground_sampler_returns_a_reading(self):
        # The real user32 binding returns one classified reading in the shared record schema.
        reading = perf_focus.sample_foreground()
        self.assertIn(reading.kind, ("app", "none", "failed"))
        self.assertEqual(reading.records[0].argv, ("GetForegroundWindow",))


# The reason the harness gives when a pane's program exits before the run finishes.
PANE_EXIT_REASON = "pane 2 exited (exit code 1) before the run finished"
