"""Gate execution tests."""

from __future__ import annotations
from gate_test_fixture import gate
import errno
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
from unittest import mock

import gate_cli
import gate_custody
import gate_execution
import gate_model
import gate_process
import gate_test_fixture


class RunnerTests(unittest.TestCase):
    """The runner keeps going after every kind of step failure and bounds every step."""

    def test_failure_hang_and_launch_error_each_let_later_steps_run(self):
        # Protect run-to-the-end: no step outcome stops the steps after it, and a hang
        # dies with its whole process tree at its own deadline.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            survivor = base / "grandchild-survived"
            grandchild = (
                "import pathlib, time; time.sleep(3); "
                f"pathlib.Path({str(survivor)!r}).write_text('alive', encoding='utf-8')"
            )
            hang = (
                "import subprocess, sys, time; "
                f"subprocess.Popen([sys.executable, '-c', {grandchild!r}]); "
                "print('hanging', flush=True); time.sleep(60)"
            )
            steps = [
                gate_test_fixture.python_step("fails", "import sys; print('failing', flush=True); sys.exit(3)"),
                gate_test_fixture.python_step("after-failure", gate_test_fixture.marker_code(base / "after-failure")),
                gate_test_fixture.python_step("hangs", hang, timeout_s=1),
                gate_test_fixture.python_step("after-timeout", gate_test_fixture.marker_code(base / "after-timeout")),
                gate_test_fixture.python_step("missing", "", argv=("sonicterm-no-such-program-for-gate-tests",)),
                gate_test_fixture.python_step("after-launch-error", gate_test_fixture.marker_code(base / "after-launch-error")),
            ]
            started = time.monotonic()
            report, console = gate_test_fixture.run_quietly(steps, root, base / "logs")
            elapsed = time.monotonic() - started
            # Outlast the grandchild's own sleep, so a survivor would have written its marker.
            time.sleep(3.5)

            self.assertEqual(
                [result.status for result in report.results],
                [gate_model.FAIL, gate_model.PASS, gate_model.TIMEOUT, gate_model.PASS, gate_model.LAUNCH, gate_model.PASS],
            )
            self.assertEqual(report.results[0].exit_code, 3)
            for name in ("after-failure", "after-timeout", "after-launch-error"):
                self.assertTrue((base / name).is_file(), f"{name} did not run")
            hung = report.results[2]
            self.assertGreaterEqual(hung.elapsed_s, 1)
            self.assertLess(hung.elapsed_s, 20)
            self.assertLess(elapsed, 60)
            self.assertIn("deadline of 1s reached; process tree killed", hung.detail)
            self.assertIn("hanging", hung.log_path.read_text(encoding="utf-8"))
            self.assertFalse(survivor.exists(), "the hung step's grandchild outlived its deadline")
            self.assertIn("cannot find", report.results[4].detail)
            self.assertEqual(report.exit_code, 1)
            # A non-passing step's log tail reaches the console.
            self.assertIn("    | failing", console)

    @gate_test_fixture.POSIX_ONLY
    def test_a_program_the_kernel_refuses_is_a_launch_error(self):
        # Protect the Popen failure path: an exec error is a verdict, never an exception.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            program = base / "not-a-program"
            program.write_bytes(b"\x00\x01\x02 not an executable format\n")
            program.chmod(0o755)
            steps = [
                gate_test_fixture.python_step("refused", "", argv=(str(program),)),
                gate_test_fixture.python_step("after", gate_test_fixture.marker_code(base / "after")),
            ]
            report, _console = gate_test_fixture.run_quietly(steps, root, base / "logs")

            self.assertEqual([result.status for result in report.results], [gate_model.LAUNCH, gate_model.PASS])
            self.assertIn("launch failed", report.results[0].detail)
            self.assertTrue((base / "after").is_file())

    def test_logs_and_summaries_record_every_step(self):
        # Protect the evidence a watcher reads: step logs and both summaries name every verdict.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            logs = base / "logs"
            steps = [
                gate_test_fixture.python_step("passes", "print('passing output')"),
                gate_test_fixture.python_step("fails", "import sys; sys.exit(5)"),
            ]
            report, console = gate_test_fixture.run_quietly(steps, root, logs)

            self.assertEqual(report.exit_code, 1)
            first = (logs / "01-passes.log").read_text(encoding="utf-8")
            self.assertIn("[local-gate] step=passes", first)
            self.assertIn("passing output", first)
            self.assertIn("result=PASS exit=0", first)
            self.assertIn("result=FAIL exit=5", (logs / "02-fails.log").read_text(encoding="utf-8"))
            self.assertIn("verdict=FAIL exit=1", (logs / "summary.txt").read_text(encoding="utf-8"))
            document = json.loads((logs / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(
                [(step["id"], step["status"], step["exit_code"]) for step in document["steps"]],
                [("passes", "PASS", 0), ("fails", "FAIL", 5)],
            )
            self.assertEqual(document["exit_code"], 1)
            self.assertIn("[local-gate] start passes", console)
            self.assertIn("[local-gate] finish fails result=FAIL exit=5", console)

    def test_step_environment_reaches_the_child(self):
        # Protect env-carried flags such as RUSTDOCFLAGS from being dropped at launch.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            seen = base / "seen"
            code = (
                "import os, pathlib; "
                f"pathlib.Path({str(seen)!r}).write_text(os.environ['LOCAL_GATE_PROBE'], encoding='utf-8')"
            )
            step = gate_test_fixture.python_step("env", code, env=(("LOCAL_GATE_PROBE", "-D warnings"),))
            report, _console = gate_test_fixture.run_quietly([step], root, base / "logs")

            self.assertEqual(report.exit_code, 0)
            self.assertEqual(seen.read_text(encoding="utf-8"), "-D warnings")

    @gate_test_fixture.POSIX_ONLY
    def test_a_process_the_leader_leaves_behind_is_killed_and_fails_the_step(self):
        # Protect the step boundary: a descendant with its output on DEVNULL holds no pipe, so
        # without the group check the step would pass while the process kept running.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            survivor = base / "leftover-survived"
            child = (
                "import pathlib, time; time.sleep(4); "
                f"pathlib.Path({str(survivor)!r}).write_text('alive', encoding='utf-8')"
            )
            leader = (
                "import subprocess, sys; "
                f"subprocess.Popen([sys.executable, '-c', {child!r}], "
                "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); "
                "print('leader exits', flush=True)"
            )
            steps = [
                gate_test_fixture.python_step("leaves-a-child", leader),
                gate_test_fixture.python_step("after-leftover", gate_test_fixture.marker_code(base / "after-leftover")),
            ]
            started = time.monotonic()
            report, _console = gate_test_fixture.run_quietly(steps, root, base / "logs")
            # Outlast the child's own sleep, even from a late start, so a survivor would have
            # written its marker.
            time.sleep(max(0.0, 5.5 - (time.monotonic() - started)))

            leaves = report.results[0]
            self.assertEqual(leaves.status, gate_model.FAIL)
            # The leader itself succeeded; only the process it left behind fails the step.
            self.assertEqual(leaves.exit_code, 0)
            self.assertEqual(leaves.leftover_processes, 1)
            self.assertIn("1 leftover process(es) outlived the leader", leaves.detail)
            # Killed after the grace period, well before the child's four-second sleep ends.
            self.assertLess(leaves.elapsed_s, 4)
            self.assertFalse(survivor.exists(), "the leftover process outlived its step")
            self.assertEqual(report.results[1].status, gate_model.PASS)
            self.assertEqual(report.exit_code, 1)
            self.assertIn("leftover_processes=1", leaves.log_path.read_text(encoding="utf-8"))
            summary = (base / "logs" / "summary.txt").read_text(encoding="utf-8")
            self.assertIn("1 leftover process(es)", summary)
            document = json.loads((base / "logs" / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(document["steps"][0]["leftover_processes"], 1)

    @gate_test_fixture.POSIX_ONLY
    def test_a_descendant_that_exits_within_the_grace_period_passes(self):
        # Protect ordinary steps from false failures: a short-lived descendant that finishes
        # soon after the leader exits leaves nothing behind. The grace period is POSIX-only; on
        # Windows the step returns at once, so the child could still hold the fixture's cwd.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            leader = (
                "import subprocess, sys; "
                "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(0.2)'], "
                "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)"
            )
            report, _console = gate_test_fixture.run_quietly([gate_test_fixture.python_step("short-child", leader)], root, base / "logs")

            self.assertEqual(report.results[0].status, gate_model.PASS, report.results[0].detail)
            self.assertEqual(report.results[0].leftover_processes, 0)
            self.assertEqual(report.exit_code, 0)

    @gate_test_fixture.POSIX_ONLY
    def test_the_leader_stays_unreaped_until_its_group_is_killed(self):
        # Protect the group id from reuse: with each exit watch this host offers, the leader's
        # exit is observed without reaping it, so while the group is settled and killed its pid
        # still reserves the group number. The no-watch fallback reaps first, as documented.
        watches = gate_custody.leader_watches()
        # Linux Python has os.waitid and every macOS Python has kqueue, so a CI host has a watch.
        self.assertTrue(watches)
        real_settle, real_killpg = gate_custody._settle_group, os.killpg
        leader = (
            "import subprocess, sys; "
            "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(10)'], "
            "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)"
        )
        for watch in (*watches, None):
            with self.subTest(watch=watch):
                seen = {"settle": [], "kill": []}

                def settle(process, *args):
                    # Popen sets returncode only when it reaps, so None means the leader is unreaped.
                    seen["settle"].append(process.returncode)
                    seen["process"] = process
                    return real_settle(process, *args)

                def killpg(pgid, signal_number):
                    if signal_number == signal.SIGKILL and "process" in seen:
                        seen["kill"].append((pgid == seen["process"].pid, seen["process"].returncode))
                    return real_killpg(pgid, signal_number)

                with tempfile.TemporaryDirectory() as directory:
                    base = Path(directory)
                    root = gate_test_fixture.fixture_repository(base)
                    with mock.patch.object(gate_custody, "_LEADER_WATCH", watch), \
                            mock.patch.object(gate_custody, "_settle_group", settle), \
                            mock.patch.object(gate.os, "killpg", killpg):
                        report, _console = gate_test_fixture.run_quietly(
                            [gate_test_fixture.python_step("leaves-a-child", leader)], root, base / "logs"
                        )

                result = report.results[0]
                self.assertEqual(result.status, gate_model.FAIL, result.detail)
                self.assertEqual(result.leftover_processes, 1)
                # The leader's own status is recorded once it is reaped.
                self.assertEqual(result.exit_code, 0)
                status = None if watch else 0
                self.assertEqual(seen["settle"], [status])
                self.assertEqual(seen["kill"], [(True, status)])

    def test_leader_watch_selection_prefers_waitid_then_kqueue(self):
        # Protect the selection logic on every host: waitid with WNOWAIT is preferred where
        # Python provides it, kqueue serves macOS builds that lack os.waitid but keep WNOWAIT,
        # and a host with neither, or Windows, falls back to Popen.wait.
        waitid = {name: 0 for name in gate_process._WAITID_NAMES}
        without_waitid = {name: 0 for name in gate_process._WAITID_NAMES if name != "waitid"}
        kqueue = {name: 0 for name in gate_process._KQUEUE_NAMES}
        cases = (
            ("posix", waitid, kqueue, ("waitid", "kqueue")),
            ("posix", without_waitid, kqueue, ("kqueue",)),
            ("posix", waitid, {}, ("waitid",)),
            ("posix", without_waitid, {}, ()),
            ("nt", waitid, kqueue, ()),
        )
        for name, os_names, select_names, expected in cases:
            with self.subTest(name=name, expected=expected):
                fake_os = types.SimpleNamespace(name=name, **os_names)
                fake_select = types.SimpleNamespace(**select_names)
                self.assertEqual(gate_custody.leader_watches(fake_os, fake_select), expected)
        # The runner uses the host's preferred watch, or None for the Popen.wait fallback.
        self.assertEqual(gate_custody._LEADER_WATCH, (gate_custody.leader_watches() or (None,))[0])

    @gate_test_fixture.POSIX_ONLY
    def test_each_exit_watch_sees_the_exit_without_reaping_the_leader(self):
        # Protect each exit watch directly: it reports a running leader as running and an exited
        # one as exited while leaving it unreaped, including a leader that is already a zombie
        # when the watch starts, which kqueue reports through ESRCH at registration.
        for watch in gate_custody.leader_watches():
            with self.subTest(watch=watch):
                process = subprocess.Popen(
                    [sys.executable, "-c", "import sys; sys.stdin.read()"],
                    stdin=subprocess.PIPE,
                    **gate_model.SMOKE_RUNNER.process_group_options(),
                )
                try:
                    self.assertFalse(gate_custody._await_leader(process, 0.2, watch))
                    process.stdin.close()
                    self.assertTrue(gate_custody._await_leader(process, 30, watch))
                    self.assertIsNone(process.returncode)
                    # Now a zombie: the watch still reports the exit and still does not reap it.
                    self.assertTrue(gate_custody._await_leader(process, 30, watch))
                    self.assertIsNone(process.returncode)
                    self.assertEqual(process.wait(timeout=10), 0)
                finally:
                    if process.returncode is None and process.poll() is None:
                        gate_model.SMOKE_RUNNER.terminate_process_tree(process)
                        process.wait(timeout=10)
                    process.stdin.close()

    def test_a_console_that_cannot_encode_a_log_tail_does_not_stop_the_run(self):
        # Protect the run from its own diagnostics: a failing step's tail with CJK text and
        # undecodable bytes is printed escaped to a strict cp1252 console, as on Windows,
        # later steps still run, both summaries are written, and the log keeps its bytes.
        payload = "中文 café\n".encode("utf-8") + b"\xff\xfe\n"
        code = f"import sys; sys.stdout.buffer.write({payload!r}); sys.stdout.flush(); sys.exit(1)"
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            buffer = io.BytesIO()
            console = io.TextIOWrapper(buffer, encoding="cp1252", errors="strict", newline="\n")
            steps = [
                gate_test_fixture.python_step("prints-cjk", code),
                gate_test_fixture.python_step("after-cjk", gate_test_fixture.marker_code(base / "after-cjk")),
            ]
            report = gate_cli.run_gate(steps, root, base / "logs", "test", console=console)
            console.flush()
            text = buffer.getvalue().decode("cp1252")

            self.assertEqual([result.status for result in report.results], [gate_model.FAIL, gate_model.PASS])
            self.assertTrue((base / "after-cjk").exists())
            self.assertTrue((base / "logs" / "summary.txt").is_file())
            self.assertTrue((base / "logs" / "summary.json").is_file())
            # The saved log keeps the step's exact bytes; only the console copy is escaped.
            self.assertIn(payload, report.results[0].log_path.read_bytes())
            self.assertIn("\\u4e2d\\u6587 café", text)
            self.assertIn("\\ufffd", text)

    def test_a_refused_group_kill_still_records_and_reaps_the_step(self):
        # Protect cleanup from macOS's EPERM: a group kill refused after the leader exited, on
        # Ctrl-C or at a deadline with the pipe still held, is recorded in the step's detail,
        # the leader is still reaped, a later step still runs, and both summaries are written.
        for case in ("interrupted after exit", "timed out with the pipe held"):
            with self.subTest(case=case):
                seen = []
                released = threading.Event()
                real_copy = gate_process._copy_output

                def refuse(process):
                    # Stand-in for macOS: only the zombie leader is left, so killpg is refused.
                    seen.append(process)
                    released.set()
                    raise PermissionError(errno.EPERM, os.strerror(errno.EPERM))

                def interrupt(process, deadline):
                    raise KeyboardInterrupt

                def held_copy(pipe, log):
                    # Stand-in for a descendant outside the group that holds the pipe past the deadline.
                    released.wait(30)
                    real_copy(pipe, log)

                with tempfile.TemporaryDirectory() as directory:
                    base = Path(directory)
                    root = gate_test_fixture.fixture_repository(base)
                    if case == "interrupted after exit":
                        patch = mock.patch.object(gate_custody, "_settle_group", interrupt)
                        steps = [gate_test_fixture.python_step("exits", "pass")]
                    else:
                        patch = mock.patch.object(gate_process, "_copy_output", held_copy)
                        steps = [gate_test_fixture.python_step("holds-pipe", "pass", timeout_s=1),
                                 gate_test_fixture.python_step("after", gate_test_fixture.marker_code(base / "after"))]
                    with (patch, mock.patch.object(gate_model, "WINDOWS_JOB", None),
                          mock.patch.object(gate_model.SMOKE_RUNNER, "terminate_process_tree", refuse)):
                        report, _console = gate_test_fixture.run_quietly(steps, root, base / "logs")

                    result = report.results[0]
                    self.assertIn("EPERM", result.detail)
                    # The leader exited 0 and the runner reaped it, so its status is real.
                    self.assertEqual(result.exit_code, 0)
                    self.assertEqual(seen[0].returncode, 0)
                    self.assertTrue((base / "logs" / "summary.txt").is_file())
                    self.assertTrue((base / "logs" / "summary.json").is_file())
                    if case == "interrupted after exit":
                        self.assertEqual(result.status, gate_model.INTERRUPTED)
                        self.assertEqual(report.exit_code, 130)
                    else:
                        self.assertEqual(result.status, gate_model.TIMEOUT)
                        self.assertEqual(report.results[1].status, gate_model.PASS)
                        self.assertTrue((base / "after").exists())

    @unittest.skipUnless(sys.platform == "darwin", "macOS refuses a group kill on a zombie-only group")
    def test_a_zombie_only_group_kill_is_refused_on_macos_and_the_step_is_still_reaped(self):
        # Protect the real macOS path: once the leader has exited, its unreaped zombie is the only
        # member, and killpg fails with EPERM, not ESRCH. A deadline reached while a child outside
        # the group holds the pipe then records the refusal, reaps the leader, and writes summaries.
        leader = subprocess.Popen([sys.executable, "-c", "pass"], **gate_model.SMOKE_RUNNER.process_group_options())
        try:
            self.assertTrue(gate_custody._await_leader(leader, 30, gate_custody._LEADER_WATCH))
            with self.assertRaises(PermissionError):
                os.killpg(leader.pid, signal.SIGKILL)
        finally:
            leader.wait(timeout=10)
        # The holder leaves the step's session but keeps the pipe for three seconds.
        holder = (
            "import subprocess, sys; "
            "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(3)'], start_new_session=True)"
        )
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            report, _console = gate_test_fixture.run_quietly([gate_test_fixture.python_step("holds-pipe", holder, timeout_s=1)], root, base / "logs")
            result = report.results[0]
            self.assertEqual(result.status, gate_model.TIMEOUT, result.detail)
            self.assertIn("EPERM", result.detail)
            self.assertEqual(result.exit_code, 0)
            self.assertTrue((base / "logs" / "summary.json").is_file())

    @gate_test_fixture.POSIX_ONLY
    def test_a_leader_another_reaper_collects_fails_without_a_status_or_a_signal(self):
        # Protect lost ownership: when another reaper collects the leader (here the kernel, with
        # SIGCHLD ignored), its exit status is unavailable, never Popen's synthetic 0, and the
        # runner sends no signal to its pid or group, which may already name other processes.
        sent = []
        real_killpg, real_kill = os.killpg, os.kill

        def killpg(pgid, signal_number):
            sent.append(("killpg", pgid, signal_number))
            return real_killpg(pgid, signal_number)

        def kill(pid, signal_number):
            # Signal 0 only checks that a pid exists; every other signal is recorded.
            if signal_number != 0:
                sent.append(("kill", pid, signal_number))
            return real_kill(pid, signal_number)

        with tempfile.TemporaryDirectory() as directory:
            step = gate_test_fixture.python_step("exits-7", "import sys; sys.exit(7)")
            previous = signal.signal(signal.SIGCHLD, signal.SIG_IGN)
            try:
                with mock.patch.object(gate.os, "killpg", killpg), mock.patch.object(gate.os, "kill", kill):
                    result = gate_execution.run_step(step, 1, Path(directory), Path(directory), dict(os.environ))
            finally:
                signal.signal(signal.SIGCHLD, previous)
        self.assertEqual(result.status, gate_model.FAIL, result.detail)
        self.assertIsNone(result.exit_code)
        self.assertIn("exit status unavailable", result.detail)
        self.assertEqual(sent, [])


class OutputLimitTests(unittest.TestCase):
    def test_limited_output_drains_and_fails_instead_of_accepting_truncation(self):
        # A child can finish writing beyond the cap without blocking its pipe or passing the gate.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            step = gate_test_fixture.python_step("flood", "import sys; sys.stdout.buffer.write(b'x' * 131072)")
            result = gate_execution.run_step(step, 1, root, root, os.environ, output_limit_bytes=1024)
            self.assertEqual(result.status, gate_model.FAIL)
            self.assertEqual(result.exit_code, 0)
            self.assertIn("output limit", result.detail)
            self.assertLess(result.log_path.stat().st_size, 4096)

    def test_output_at_the_limit_remains_complete(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            step = gate_test_fixture.python_step("exact", "import sys; sys.stdout.buffer.write(b'x' * 1024)")
            result = gate_execution.run_step(step, 1, root, root, os.environ, output_limit_bytes=1024)
            self.assertEqual(result.status, gate_model.PASS)
            self.assertIn(b'x' * 1024, result.log_path.read_bytes())
