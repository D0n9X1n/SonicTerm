#!/usr/bin/env python3
"""Tests for the MCP smoke's process custody: layered attribution, fail-closed cleanup, the watchdog, Windows jobs."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mcp_smoke_process as process_module  # noqa: E402

POSIX_ONLY = unittest.skipIf(os.name == "nt", "Unix sessions and signals")
WINDOWS_ONLY = unittest.skipUnless(os.name == "nt", "Windows Job Objects")
Row = process_module.Row

# A child that ignores SIGHUP and SIGTERM, so only SIGKILL ends it; it writes its pid atomically to argv[1].
# With argv[2] it waits for that file to appear and then execs /bin/sleep, keeping its pid and start time.
CHILD_SOURCE = (
    "import os, signal, sys, time\n"
    "for name in ('SIGHUP', 'SIGTERM'):\n"
    "    if hasattr(signal, name):\n"
    "        signal.signal(getattr(signal, name), signal.SIG_IGN)\n"
    "open(sys.argv[1] + '.tmp', 'w').write(str(os.getpid()))\n"
    "os.replace(sys.argv[1] + '.tmp', sys.argv[1])\n"
    "if len(sys.argv) > 2:\n"
    "    while not os.path.exists(sys.argv[2]):\n"
    "        time.sleep(0.02)\n"
    "    os.execv('/bin/sleep', ['sleep', '600'])\n"
    "time.sleep(600)\n"
)
# A root that starts that child in its own session (detached, as setsid does) and then lives until
# killed (argv[2] == 'stay') or exits at once (anything else).
ROOT_SOURCE = (
    "import subprocess, sys, time\n"
    "options = {} if sys.platform.startswith('win') else {'start_new_session': True}\n"
    "subprocess.Popen([sys.executable, '-c', sys.argv[1], *sys.argv[3:]], **options)\n"
    "if sys.argv[2] == 'stay':\n"
    "    time.sleep(600)\n"
)
# A root that starts /bin/sleep in its own session (no setsid: a system binary whose environment
# macOS hides), writes its pid to argv[1], and exits at once.
SLEEP_ROOT_SOURCE = (
    "import subprocess, sys\n"
    "sleeper = subprocess.Popen(['/bin/sleep', '600'])\n"
    "open(sys.argv[1] + '.tmp', 'w').write(str(sleeper.pid))\n"
    "import os; os.replace(sys.argv[1] + '.tmp', sys.argv[1])\n"
)


def wait_for_file(path: Path, timeout_s: float = 15.0) -> int:
    """Wait for a fixture's pid file and return the pid it holds."""
    end = time.monotonic() + timeout_s
    while not path.exists():
        if time.monotonic() > end:
            raise AssertionError(f"fixture never wrote {path}")
        time.sleep(0.02)
    return int(path.read_text())


def unix_alive(pid: int) -> bool:
    """Whether `pid` is a live, non-zombie process right now."""
    row = process_module.read_process_table().get(pid)
    return row is not None and not row.state.startswith("Z")


def force_kill(pids) -> None:
    """Test cleanup: kill fixture pids whatever happened, ignoring ones already gone."""
    for pid in pids:
        try:
            if os.name == "nt":
                subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True, timeout=30)
            else:
                os.kill(pid, signal.SIGKILL)
        except (OSError, subprocess.SubprocessError):
            pass


class FakeProcess:
    """A Popen stand-in: `returncode` None means running and unreaped."""

    def __init__(self, pid: int) -> None:
        self.pid = pid
        self.returncode = None

    def poll(self):
        return self.returncode

    def wait(self, timeout=None):
        if self.returncode is None:
            self.returncode = -9
        return self.returncode


class SignalRecorder:
    """Records every signal cleanup sends instead of sending it."""

    def __init__(self) -> None:
        self.sent: list[tuple[int, int]] = []

    def __call__(self, pid: int, number: int) -> None:
        self.sent.append((pid, number))


def row(ppid, start, sid, uid=501, state="S", marked=False):
    """A fake table row."""
    return Row(ppid, state, start, uid, sid, marked)


class TableTests(unittest.TestCase):
    def test_ps_rows_carry_start_time_only(self):
        # macOS identity is lstart alone, so exec keeps it; ps never supplies the marker.
        text = ("  123     1   501 Ss   Mon Nov  3 13:30:01 2026\n"
                "  124   123   501 S+   Mon Nov  3 13:30:02 2026\nnoise\n")
        table = process_module.parse_ps_table(text)
        self.assertEqual(table[123], Row(1, "Ss", "Mon Nov 3 13:30:01 2026", 501, -1, False))
        self.assertEqual(table[124].start, "Mon Nov 3 13:30:02 2026")

    def test_procargs2_yields_only_the_environment(self):
        # The token in argv is not a match; the environment entry is; exec-path padding is skipped.
        def buffer(argv, environment, padding=3, tail=b"\0"):
            body = b"/usr/bin/python3\0" + b"\0" * padding
            body += b"".join(argument + b"\0" for argument in argv)
            body += b"".join(entry + b"\0" for entry in environment)
            return len(argv).to_bytes(4, sys.byteorder) + body + tail
        marker = b"SONICTERM_SMOKE_RUN=abc"
        in_argv = buffer([b"python3", marker], [b"HOME=/h"])
        self.assertEqual(process_module.parse_procargs2(in_argv), [b"HOME=/h"])
        self.assertFalse(process_module.has_marker(process_module.parse_procargs2(in_argv), "abc"))
        in_environment = buffer([b"python3"], [b"HOME=/h", marker], padding=0)
        self.assertTrue(process_module.has_marker(process_module.parse_procargs2(in_environment), "abc"))
        self.assertFalse(process_module.has_marker(process_module.parse_procargs2(in_environment), "ab"))

    def test_procargs2_handles_truncated_buffers(self):
        # A buffer cut inside argv has no environment; one cut inside an entry drops that entry.
        argc = (2).to_bytes(4, sys.byteorder)
        self.assertIsNone(process_module.parse_procargs2(b"\x01"))
        self.assertIsNone(process_module.parse_procargs2(argc + b"/bin/x"))
        self.assertEqual(process_module.parse_procargs2(argc + b"/bin/x\0\0a\0SONICTERM_SMOKE_RUN=abc"), [])
        cut = argc + b"/bin/x\0a\0b\0HOME=/h\0SONICTERM_SMOKE_RUN=ab"
        self.assertEqual(process_module.parse_procargs2(cut), [b"HOME=/h"])
        self.assertFalse(process_module.has_marker(process_module.parse_procargs2(cut), "ab"))

    def test_linux_environ_matches_whole_entries_only(self):
        # The token as a substring of another entry, or in a cut-off last entry, is not a match.
        raw = b"HOME=/h\0OTHER=SONICTERM_SMOKE_RUN=abc\0SONICTERM_SMOKE_RUN=abcd\0"
        self.assertFalse(process_module.has_marker(process_module.parse_environ(raw), "abc"))
        self.assertTrue(process_module.has_marker(process_module.parse_environ(raw), "abcd"))
        self.assertFalse(process_module.has_marker(process_module.parse_environ(b"A=1\0SONICTERM_SMOKE_RUN=abc"), "abc"))

    def test_proc_stat_yields_session_and_start_time(self):
        # Linux identity is field 22 and the session field 6, after a name that may hold spaces and ')'.
        line = "42 (my (odd) cmd) S 7 40 41 " + " ".join(["0"] * 15) + " 9876 0 0"
        self.assertEqual(process_module.parse_proc_stat(line), (42, 7, "S", 41, "9876"))
        self.assertIsNone(process_module.parse_proc_stat("garbage"))

    def test_descendants_follow_parent_links_from_every_root(self):
        # A reparented child is reached from its own tracked identity, not only from the app.
        table = {10: row(1, "b", 10), 11: row(10, "c", 10), 12: row(11, "d", 10), 30: row(1, "g", 30),
                 31: row(30, "h", 30)}
        self.assertEqual(process_module.descendants(table, [10]), {11, 12})
        self.assertEqual(process_module.descendants(table, [10, 30]), {11, 12, 31})

    def test_identity_matching_rejects_reuse_and_zombies(self):
        # A pid counts as the tracked process only with the same start time, and a zombie is already dead.
        table = {7: row(1, "new", 7), 8: row(1, "old", 8, state="Z")}
        self.assertFalse(process_module.matches(table, 7, "old"))
        self.assertTrue(process_module.matches(table, 8, "old"))
        self.assertFalse(process_module.alive(table, 8, "old"))

    @POSIX_ONLY
    def test_this_host_table_has_sessions_identities_and_markers(self):
        # The live reader reports real session ids, stable start times, and the marker of a launched child.
        first = process_module.read_process_table()
        self.assertEqual(first[os.getpid()].sid, os.getsid(0))
        self.assertEqual(first[os.getpid()].start, process_module.read_process_table()[os.getpid()].start)
        token = "probe" + str(os.getpid())
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                                 env={**os.environ, process_module.MARKER_NAME: token})
        try:
            deadline = time.monotonic() + 10
            while True:
                table = process_module.read_process_table(token)
                if child.pid in table and table[child.pid].marked:
                    break
                self.assertLess(time.monotonic(), deadline, "the marker never became visible")
                time.sleep(0.05)
            self.assertFalse(table[os.getpid()].marked)
            self.assertFalse(process_module.read_process_table("other" + token)[child.pid].marked)
        finally:
            child.kill()
            child.wait()


@POSIX_ONLY
class FakeCleanupTests(unittest.TestCase):
    OWN = {"own_pid": 100, "own_sid": 99, "own_uid": 501}

    def containment(self, tables, recorder, verify_s=0.3, root_pid=4242):
        """A containment over a FakeProcess whose reader returns `tables` in turn, raising Exceptions."""
        state = {"tables": list(tables), "tokens": []}

        def reader(token):
            state["tokens"].append(token)
            value = state["tables"].pop(0) if len(state["tables"]) > 1 else state["tables"][0]
            if isinstance(value, Exception):
                raise value
            return value
        contained = process_module.UnixContainment(FakeProcess(root_pid), reader, "tok", kill=recorder,
                                                   killpg=recorder, verify_s=verify_s, **self.OWN)
        return contained, state

    def test_a_reused_pid_is_never_killed(self):
        # The tracked child's pid now names an unrelated process in another session: no signal reaches it.
        recorder = SignalRecorder()
        contained, state = self.containment([{4242: row(1, "root", 4242), 5000: row(4242, "old", 5000)}], recorder)
        contained.walk()
        contained.process.returncode = 0
        state["tables"] = [{5000: row(1, "new", 5000)}]
        self.assertEqual(contained.cleanup(), [])
        self.assertEqual(recorder.sent, [])

    def test_an_unrelated_unmarked_process_is_never_signalled(self):
        # A same-user process in an unrecorded session without the marker is not ours.
        recorder = SignalRecorder()
        table = {4242: row(1, "root", 4242), 6000: row(1, "x", 6000), 6001: row(1, "y", 4242, uid=0)}
        contained, state = self.containment([table], recorder)
        contained.walk()
        contained.process.returncode = 0
        state["tables"] = [{6000: row(1, "x", 6000), 6001: row(1, "y", 4242, uid=0)}]
        self.assertEqual(contained.cleanup(), [])
        self.assertNotIn(6000, [pid for pid, _number in recorder.sent])
        self.assertNotIn(6001, [pid for pid, _number in recorder.sent])

    def test_a_session_whose_id_was_reused_is_dropped(self):
        # The recorded session's leader pid now names a different process: its members are not ours.
        recorder = SignalRecorder()
        contained, state = self.containment([{4242: row(1, "root", 4242), 5000: row(4242, "old", 5000)}], recorder)
        contained.walk()
        contained.process.returncode = 0
        state["tables"] = [{5000: row(1, "reused", 5000), 5001: row(5000, "member", 5000)}]
        self.assertEqual(contained.cleanup(), [])
        self.assertEqual(recorder.sent, [])

    def test_marker_and_session_members_are_killed_until_gone(self):
        # A marked orphan and an unmarked orphan that kept a recorded session are both ours and both die.
        recorder = SignalRecorder()
        contained, state = self.containment([{4242: row(1, "root", 4242)}], recorder)
        contained.walk()
        contained.process.returncode = 0
        living = {7000: row(1, "m", 7000, marked=True), 7001: row(1, "s", 4242)}
        state["tables"] = [living, living, living, {}]
        self.assertEqual(contained.cleanup(), [])
        self.assertIn((7000, signal.SIGKILL), recorder.sent)
        self.assertIn((7001, signal.SIGKILL), recorder.sent)
        self.assertIn("tok", state["tokens"])

    def test_a_marked_process_of_another_user_is_left_alone(self):
        # The marker attributes only this user's processes.
        recorder = SignalRecorder()
        contained, state = self.containment([{4242: row(1, "root", 4242)}], recorder)
        contained.walk()
        contained.process.returncode = 0
        state["tables"] = [{7000: row(1, "m", 7000, uid=0, marked=True)}]
        self.assertEqual(contained.cleanup(), [])
        self.assertEqual(recorder.sent, [])

    def test_a_failed_scan_fails_cleanup_but_still_kills_the_root(self):
        # An unknown outcome is a failure: the root and its group are killed and the problem names the table.
        recorder = SignalRecorder()
        contained, state = self.containment([{4242: row(1, "root", 4242)}], recorder)
        contained.walk()
        state["tables"] = [RuntimeError("ps failed")]
        problems = contained.cleanup()
        self.assertTrue(any("unreadable" in problem for problem in problems), problems)
        self.assertIn((4242, signal.SIGKILL), recorder.sent)
        self.assertEqual(contained.cleanup(), problems)

    def test_a_survivor_fails_cleanup_and_stays_registered(self):
        # A process that never disappears is reported, and its entry is not dropped.
        recorder = SignalRecorder()
        stuck = {4242: row(1, "root", 4242), 5000: row(4242, "old", 5000)}
        contained, _state = self.containment([stuck], recorder)
        contained.walk()
        owned = process_module.Owned()
        owned.entries.append(contained)
        problems = owned.cleanup()
        self.assertTrue(any("still alive" in problem for problem in problems), problems)
        self.assertEqual(owned.entries, [contained])

    def test_tracking_read_errors_make_readiness_fail(self):
        # A table that could not be read while tracking is reported, never read as "no children".
        contained, _state = self.containment([RuntimeError("ps failed")], SignalRecorder())
        self.assertFalse(contained.walk())
        with self.assertRaises(RuntimeError):
            contained.descendant_count()


@POSIX_ONLY
class UnixTreeTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix="stmcpp-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, self.directory, True)
        self.pids: list[int] = []
        # Registered last so it runs first: the fixtures die before their directory is removed.
        self.addCleanup(lambda: force_kill(self.pids))

    def test_a_detached_child_of_a_root_that_exits_at_once_is_killed_without_waiting(self):
        # The reviewer's case: setsid'd SIGHUP-ignoring child, root gone immediately, cleanup at once.
        pid_file = self.directory / "child.pid"
        owned = process_module.Owned()
        contained = owned.launch([sys.executable, "-c", ROOT_SOURCE, CHILD_SOURCE, "exit", str(pid_file)])
        self.pids.append(contained.process.pid)
        child = wait_for_file(pid_file)
        self.pids.append(child)
        self.assertEqual(owned.cleanup(), [])
        self.assertFalse(unix_alive(child))
        self.assertEqual(owned.entries, [])

    def test_a_system_binary_orphan_is_killed_through_its_session(self):
        # /bin/sleep hides its environment on macOS; reparented after its parent exits, it keeps the session.
        pid_file = self.directory / "sleep.pid"
        owned = process_module.Owned()
        contained = owned.launch([sys.executable, "-c", SLEEP_ROOT_SOURCE, str(pid_file)])
        self.pids.append(contained.process.pid)
        sleeper = wait_for_file(pid_file)
        self.pids.append(sleeper)
        contained.process.wait(timeout=10)
        self.assertTrue(unix_alive(sleeper))
        self.assertEqual(owned.cleanup(), [])
        self.assertFalse(unix_alive(sleeper))

    def test_a_tracked_child_that_execs_sleep_is_still_killed(self):
        # Identity is the start time only, so after exec /bin/sleep the child still matches and dies.
        pid_file, go_file = self.directory / "child.pid", self.directory / "go"
        owned = process_module.Owned()
        contained = owned.launch([sys.executable, "-c", ROOT_SOURCE, CHILD_SOURCE, "stay", str(pid_file),
                                  str(go_file)])
        self.pids.append(contained.process.pid)
        child = wait_for_file(pid_file)
        self.pids.append(child)
        end = time.monotonic() + 5
        while child not in contained.known_pids():
            self.assertLess(time.monotonic(), end, "the tracker never saw the child")
            time.sleep(0.02)
        go_file.write_text("go")
        end = time.monotonic() + 10
        while "sleep" not in subprocess.run(["ps", "-o", "command=", "-p", str(child)], capture_output=True,
                                            text=True).stdout:
            self.assertLess(time.monotonic(), end, "the child never execed /bin/sleep")
            time.sleep(0.05)
        self.assertEqual(owned.cleanup(), [])
        self.assertFalse(unix_alive(child))

    def test_unrelated_processes_survive_cleanup(self):
        # A process in its own session without the marker, and one carrying another run's token, are not ours.
        bystander = subprocess.Popen(["/bin/sleep", "600"], start_new_session=True)
        other_run = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"], start_new_session=True,
                                     env={**os.environ, process_module.MARKER_NAME: "another-run"})
        self.pids.extend([bystander.pid, other_run.pid])
        try:
            owned = process_module.Owned()
            contained = owned.launch([sys.executable, "-c", "import time; time.sleep(600)"])
            self.pids.append(contained.process.pid)
            time.sleep(0.3)
            self.assertEqual(owned.cleanup(), [])
            self.assertIsNone(bystander.poll())
            self.assertIsNone(other_run.poll())
        finally:
            for process in (bystander, other_run):
                process.kill()
                process.wait()

    def test_a_process_with_the_token_only_in_argv_is_never_signalled(self):
        # The reviewer's repro: a sibling in another session, with the token as an argument but not in its environment.
        owned = process_module.Owned()
        contained = owned.launch([sys.executable, "-c", "import time; time.sleep(600)"])
        self.pids.append(contained.process.pid)
        sibling = subprocess.Popen(
            [sys.executable, "-c", "import time, sys; time.sleep(600)",
             f"{process_module.MARKER_NAME}={contained.token}"],
            start_new_session=True,
            env={key: value for key, value in os.environ.items() if key != process_module.MARKER_NAME})
        self.pids.append(sibling.pid)
        try:
            time.sleep(0.3)
            self.assertEqual(owned.cleanup(), [])
            self.assertIsNone(sibling.poll(), "a process carrying the token only in argv was killed")
        finally:
            sibling.kill()
            sibling.wait()

    def test_group_kill_alone_leaves_the_child_alive(self):
        # The control: killing only the root's process group, the old behavior, leaves the detached child.
        pid_file = self.directory / "child.pid"
        root = subprocess.Popen([sys.executable, "-c", ROOT_SOURCE, CHILD_SOURCE, "stay", str(pid_file)],
                                start_new_session=True)
        self.pids.append(root.pid)
        child = wait_for_file(pid_file)
        self.pids.append(child)
        os.killpg(root.pid, signal.SIGKILL)
        root.wait(timeout=10)
        time.sleep(0.2)
        self.assertTrue(unix_alive(child))

    def test_the_watchdog_exits_only_after_an_in_progress_cleanup_confirms_every_death(self):
        # The watchdog fires while the main thread is mid-cleanup: it waits on the lock and exits after.
        pid_file = self.directory / "child.pid"
        entered, release = threading.Event(), threading.Event()

        def reader(token):
            # The cleanup thread's reads pause once, so the watchdog fires while cleanup is in progress.
            if threading.current_thread().name == "cleanup" and not release.is_set():
                entered.set()
                release.wait(10)
            return process_module.read_process_table(token)
        owned = process_module.Owned(reader=reader)
        contained = owned.launch([sys.executable, "-c", ROOT_SOURCE, CHILD_SOURCE, "stay", str(pid_file)])
        root = contained.process
        self.pids.append(root.pid)
        child = wait_for_file(pid_file)
        self.pids.append(child)
        results, reports, exits = [], [], []

        def record_exit(code):
            exits.append((code, root.poll() is not None, unix_alive(child)))
        cleaner = threading.Thread(target=lambda: results.append(owned.cleanup()), name="cleanup")
        cleaner.start()
        self.assertTrue(entered.wait(10))
        watchdog = threading.Thread(target=owned.expire, args=(reports.append, record_exit))
        watchdog.start()
        time.sleep(0.3)
        self.assertEqual(exits, [], "the watchdog exited while cleanup was still running")
        release.set()
        cleaner.join(30)
        watchdog.join(30)
        self.assertEqual(results, [[]])
        self.assertEqual(reports, [[]])
        self.assertEqual(exits, [(process_module.DEADLINE_EXIT_CODE, True, False)])
        with self.assertRaises(RuntimeError):
            owned.launch([sys.executable, "-c", "pass"])


def windows_alive(pid: int) -> bool:
    """Whether `pid` names a running process (Windows): open it and test its handle without waiting."""
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel32.OpenProcess(0x00100000, False, pid)
    if not handle:
        return False
    try:
        # WAIT_TIMEOUT means the process has not exited.
        return kernel32.WaitForSingleObject(handle, 0) == 0x102
    finally:
        kernel32.CloseHandle(handle)


class LaunchFlagTests(unittest.TestCase):
    def test_a_windows_launch_never_starts_a_new_process_group(self):
        # A new process group makes the app and every pane program ignore Ctrl+C, so the smoke's
        # interrupt step could never pass; the launch must not ask for one.
        source = Path(process_module.__file__).read_text(encoding="utf-8")
        launch = source[source.index("def launch("):]
        launch = launch[:launch.index("\n    def ", 1)]
        self.assertNotIn("| subprocess.CREATE_NEW_PROCESS_GROUP", launch)
        self.assertIn("CREATE_SUSPENDED", launch)


@WINDOWS_ONLY
class WindowsJobTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix="stmcpp-"))
        self.addCleanup(shutil.rmtree, self.directory, True)
        self.pids: list[int] = []
        # Registered last so it runs first: the fixtures die before their directory is removed.
        self.addCleanup(lambda: force_kill(self.pids))

    def test_the_job_kills_the_whole_tree_and_reports_it_empty(self):
        # A grandchild started after the resume is in the job, and cleanup confirms ActiveProcesses == 0.
        pid_file = self.directory / "child.pid"
        owned = process_module.Owned()
        contained = owned.launch([sys.executable, "-c", ROOT_SOURCE, CHILD_SOURCE, "stay", str(pid_file)])
        self.pids.append(contained.process.pid)
        child = wait_for_file(pid_file)
        self.pids.append(child)
        self.assertGreaterEqual(contained.descendant_count(), 1)
        self.assertEqual(owned.cleanup(), [])
        self.assertIsNotNone(contained.process.poll())
        self.assertFalse(windows_alive(child))
        self.assertEqual(owned.entries, [])

    def test_a_childless_root_reports_no_descendants(self):
        # Readiness reads job accounting: the root alone is not a child.
        owned = process_module.Owned()
        contained = owned.launch([sys.executable, "-c", "import time; time.sleep(600)"])
        self.pids.append(contained.process.pid)
        time.sleep(0.5)
        self.assertEqual(contained.descendant_count(), 0)
        self.assertEqual(owned.cleanup(), [])
        self.assertFalse(windows_alive(contained.process.pid))


if __name__ == "__main__":
    unittest.main(verbosity=2)
