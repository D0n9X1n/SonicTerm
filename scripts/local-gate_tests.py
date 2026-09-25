#!/usr/bin/env python3
"""Runner, parity, and gate-script tests for scripts/local-gate.py.

The runner tests inject failing, hanging, non-launchable, and process-leaking
steps into the real runner and require every later step to run, the hang to die
with its process tree at its deadline, a leftover process to be killed and fail
its step, and a dirty tree to be reported and left unchanged. The parity tests
tie the step table to ci.yml, CLAUDE.md, and both Development-and-Release wiki
files; each also proves that a one-sided edit fails it, and the parser and
classifier tests mutate the complete workflow, so an unfamiliar form raises and
a parser that silently finds nothing cannot pass.

SONICTERM_GATE_ROOT selects the repository these tests read; it defaults to
this script's repository.
"""

from __future__ import annotations

import dataclasses
import errno
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import types
import unittest
from unittest import mock

ROOT = Path(
    os.environ.get("SONICTERM_GATE_ROOT") or Path(__file__).resolve().parent.parent
).resolve()
_SPEC = importlib.util.spec_from_file_location("local_gate", ROOT / "scripts" / "local-gate.py")
assert _SPEC is not None and _SPEC.loader is not None
gate = importlib.util.module_from_spec(_SPEC)
# Registered before execution: dataclasses resolve string annotations through sys.modules.
sys.modules[_SPEC.name] = gate
_SPEC.loader.exec_module(gate)

WORKFLOW = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")

# Fake tools are extensionless /bin/sh scripts found through PATH, which only a
# POSIX host resolves reliably; macOS and Ubuntu CI run these tests.
POSIX_ONLY = unittest.skipIf(os.name == "nt", "fake shell tools need a POSIX host")


def python_step(step_id: str, code: str, timeout_s: int = 30, **fields):
    """Return an injected step that runs Python code under the real runner."""
    values = dict(
        argv=(sys.executable, "-c", code),
        hosts=gate.HOSTS,
        timeout_s=timeout_s,
        evidence="local",
        prerequisites=("rust",),
        ci_jobs=(),
    )
    values.update(fields)
    return gate.Step(step_id, **values)


# Fixture Git commands take well under a second; the bound keeps a hung
# credential helper, lock, or filesystem from stalling the suite.
FIXTURE_GIT_TIMEOUT_S = 60


def _reap_killed(process: subprocess.Popen[bytes]) -> None:
    """Collect a killed fixture process, closing its pipes if a stray descendant still holds them."""
    try:
        process.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        for pipe in (process.stdout, process.stderr):
            if pipe is not None:
                pipe.close()
        process.wait(timeout=10)


def git(root: Path, *args: str, timeout: float = FIXTURE_GIT_TIMEOUT_S) -> subprocess.CompletedProcess[bytes]:
    """Run git in a fixture repository under a deadline, and fail the test on a git error.

    Git starts as the root of its own process tree, so a timeout kills and reaps the
    whole tree instead of leaving a hung helper behind.
    """
    command = ["git", "-C", str(root), *args]
    process = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        **gate.SMOKE_RUNNER.process_group_options(),
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        gate.SMOKE_RUNNER.terminate_process_tree(process)
        _reap_killed(process)
        raise
    if process.returncode != 0:
        raise subprocess.CalledProcessError(process.returncode, command, stdout, stderr)
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def commit(root: Path, message: str = "fixture") -> None:
    """Commit a fixture repository's index, isolated from user hooks and signing."""
    git(
        root,
        "-c", "user.name=fixture",
        "-c", "user.email=fixture@example.invalid",
        "-c", "commit.gpgsign=false",
        "-c", f"core.hooksPath={root.parent / 'no-hooks'}",
        "commit", "-q", "--no-verify", "-m", message,
    )


def fixture_repository(directory: Path) -> Path:
    """Create a repository with one committed file, isolated from user hooks and signing."""
    root = directory / "repository"
    root.mkdir()
    git(root, "init", "-q")
    git(root, "config", "core.autocrlf", "false")
    (root / "tracked.txt").write_bytes(b"committed\n")
    git(root, "add", "tracked.txt")
    commit(root)
    return root


def tree_bytes(root: Path) -> dict[str, bytes]:
    """Capture every working-tree file outside .git, so a test can prove nothing changed."""
    files = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if relative.parts[0] == ".git" or not path.is_file():
            continue
        files[relative.as_posix()] = path.read_bytes()
    return files


def run_quietly(steps, root: Path, log_dir: Path):
    """Run the real gate with its console captured, returning the report and the console text."""
    console = io.StringIO()
    report = gate.run_gate(steps, root, log_dir, "test", console=console)
    return report, console.getvalue()


def marker_code(target: Path) -> str:
    """Return Python code that proves a step ran by writing a marker file."""
    return f"import pathlib; pathlib.Path({str(target)!r}).write_text('ran')"


class RunnerTests(unittest.TestCase):
    """The runner keeps going after every kind of step failure and bounds every step."""

    def test_failure_hang_and_launch_error_each_let_later_steps_run(self):
        # Protect run-to-the-end: no step outcome stops the steps after it, and a hang
        # dies with its whole process tree at its own deadline.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            survivor = base / "grandchild-survived"
            grandchild = (
                "import pathlib, time; time.sleep(3); "
                f"pathlib.Path({str(survivor)!r}).write_text('alive')"
            )
            hang = (
                "import subprocess, sys, time; "
                f"subprocess.Popen([sys.executable, '-c', {grandchild!r}]); "
                "print('hanging', flush=True); time.sleep(60)"
            )
            steps = [
                python_step("fails", "import sys; print('failing', flush=True); sys.exit(3)"),
                python_step("after-failure", marker_code(base / "after-failure")),
                python_step("hangs", hang, timeout_s=1),
                python_step("after-timeout", marker_code(base / "after-timeout")),
                python_step("missing", "", argv=("sonicterm-no-such-program-for-gate-tests",)),
                python_step("after-launch-error", marker_code(base / "after-launch-error")),
            ]
            started = time.monotonic()
            report, console = run_quietly(steps, root, base / "logs")
            elapsed = time.monotonic() - started
            # Outlast the grandchild's own sleep, so a survivor would have written its marker.
            time.sleep(3.5)

            self.assertEqual(
                [result.status for result in report.results],
                [gate.FAIL, gate.PASS, gate.TIMEOUT, gate.PASS, gate.LAUNCH, gate.PASS],
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

    @POSIX_ONLY
    def test_a_program_the_kernel_refuses_is_a_launch_error(self):
        # Protect the Popen failure path: an exec error is a verdict, never an exception.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            program = base / "not-a-program"
            program.write_bytes(b"\x00\x01\x02 not an executable format\n")
            program.chmod(0o755)
            steps = [
                python_step("refused", "", argv=(str(program),)),
                python_step("after", marker_code(base / "after")),
            ]
            report, _console = run_quietly(steps, root, base / "logs")

            self.assertEqual([result.status for result in report.results], [gate.LAUNCH, gate.PASS])
            self.assertIn("launch failed", report.results[0].detail)
            self.assertTrue((base / "after").is_file())

    def test_logs_and_summaries_record_every_step(self):
        # Protect the evidence a watcher reads: step logs and both summaries name every verdict.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            logs = base / "logs"
            steps = [
                python_step("passes", "print('passing output')"),
                python_step("fails", "import sys; sys.exit(5)"),
            ]
            report, console = run_quietly(steps, root, logs)

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
            root = fixture_repository(base)
            seen = base / "seen"
            code = (
                "import os, pathlib; "
                f"pathlib.Path({str(seen)!r}).write_text(os.environ['LOCAL_GATE_PROBE'])"
            )
            step = python_step("env", code, env=(("LOCAL_GATE_PROBE", "-D warnings"),))
            report, _console = run_quietly([step], root, base / "logs")

            self.assertEqual(report.exit_code, 0)
            self.assertEqual(seen.read_text(encoding="utf-8"), "-D warnings")

    @POSIX_ONLY
    def test_a_process_the_leader_leaves_behind_is_killed_and_fails_the_step(self):
        # Protect the step boundary: a descendant with its output on DEVNULL holds no pipe, so
        # without the group check the step would pass while the process kept running.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            survivor = base / "leftover-survived"
            child = (
                "import pathlib, time; time.sleep(4); "
                f"pathlib.Path({str(survivor)!r}).write_text('alive')"
            )
            leader = (
                "import subprocess, sys; "
                f"subprocess.Popen([sys.executable, '-c', {child!r}], "
                "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); "
                "print('leader exits', flush=True)"
            )
            steps = [
                python_step("leaves-a-child", leader),
                python_step("after-leftover", marker_code(base / "after-leftover")),
            ]
            started = time.monotonic()
            report, _console = run_quietly(steps, root, base / "logs")
            # Outlast the child's own sleep, even from a late start, so a survivor would have
            # written its marker.
            time.sleep(max(0.0, 5.5 - (time.monotonic() - started)))

            leaves = report.results[0]
            self.assertEqual(leaves.status, gate.FAIL)
            # The leader itself succeeded; only the process it left behind fails the step.
            self.assertEqual(leaves.exit_code, 0)
            self.assertEqual(leaves.leftover_processes, 1)
            self.assertIn("1 leftover process(es) outlived the leader", leaves.detail)
            # Killed after the grace period, well before the child's four-second sleep ends.
            self.assertLess(leaves.elapsed_s, 4)
            self.assertFalse(survivor.exists(), "the leftover process outlived its step")
            self.assertEqual(report.results[1].status, gate.PASS)
            self.assertEqual(report.exit_code, 1)
            self.assertIn("leftover_processes=1", leaves.log_path.read_text(encoding="utf-8"))
            summary = (base / "logs" / "summary.txt").read_text(encoding="utf-8")
            self.assertIn("1 leftover process(es)", summary)
            document = json.loads((base / "logs" / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(document["steps"][0]["leftover_processes"], 1)

    @POSIX_ONLY
    def test_a_descendant_that_exits_within_the_grace_period_passes(self):
        # Protect ordinary steps from false failures: a short-lived descendant that finishes
        # soon after the leader exits leaves nothing behind. The grace period is POSIX-only; on
        # Windows the step returns at once, so the child could still hold the fixture's cwd.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            leader = (
                "import subprocess, sys; "
                "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(0.2)'], "
                "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)"
            )
            report, _console = run_quietly([python_step("short-child", leader)], root, base / "logs")

            self.assertEqual(report.results[0].status, gate.PASS, report.results[0].detail)
            self.assertEqual(report.results[0].leftover_processes, 0)
            self.assertEqual(report.exit_code, 0)

    @POSIX_ONLY
    def test_the_leader_stays_unreaped_until_its_group_is_killed(self):
        # Protect the group id from reuse: with each exit watch this host offers, the leader's
        # exit is observed without reaping it, so while the group is settled and killed its pid
        # still reserves the group number. The no-watch fallback reaps first, as documented.
        watches = gate.leader_watches()
        # Linux Python has os.waitid and every macOS Python has kqueue, so a CI host has a watch.
        self.assertTrue(watches)
        real_settle, real_killpg = gate._settle_group, os.killpg
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
                    root = fixture_repository(base)
                    with mock.patch.object(gate, "_LEADER_WATCH", watch), \
                            mock.patch.object(gate, "_settle_group", settle), \
                            mock.patch.object(gate.os, "killpg", killpg):
                        report, _console = run_quietly(
                            [python_step("leaves-a-child", leader)], root, base / "logs"
                        )

                result = report.results[0]
                self.assertEqual(result.status, gate.FAIL, result.detail)
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
        waitid = {name: 0 for name in gate._WAITID_NAMES}
        without_waitid = {name: 0 for name in gate._WAITID_NAMES if name != "waitid"}
        kqueue = {name: 0 for name in gate._KQUEUE_NAMES}
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
                self.assertEqual(gate.leader_watches(fake_os, fake_select), expected)
        # The runner uses the host's preferred watch, or None for the Popen.wait fallback.
        self.assertEqual(gate._LEADER_WATCH, (gate.leader_watches() or (None,))[0])

    @POSIX_ONLY
    def test_each_exit_watch_sees_the_exit_without_reaping_the_leader(self):
        # Protect each exit watch directly: it reports a running leader as running and an exited
        # one as exited while leaving it unreaped, including a leader that is already a zombie
        # when the watch starts, which kqueue reports through ESRCH at registration.
        for watch in gate.leader_watches():
            with self.subTest(watch=watch):
                process = subprocess.Popen(
                    [sys.executable, "-c", "import sys; sys.stdin.read()"],
                    stdin=subprocess.PIPE,
                    **gate.SMOKE_RUNNER.process_group_options(),
                )
                try:
                    self.assertFalse(gate._await_leader(process, 0.2, watch))
                    process.stdin.close()
                    self.assertTrue(gate._await_leader(process, 30, watch))
                    self.assertIsNone(process.returncode)
                    # Now a zombie: the watch still reports the exit and still does not reap it.
                    self.assertTrue(gate._await_leader(process, 30, watch))
                    self.assertIsNone(process.returncode)
                    self.assertEqual(process.wait(timeout=10), 0)
                finally:
                    if process.returncode is None and process.poll() is None:
                        gate.SMOKE_RUNNER.terminate_process_tree(process)
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
            root = fixture_repository(base)
            buffer = io.BytesIO()
            console = io.TextIOWrapper(buffer, encoding="cp1252", errors="strict", newline="\n")
            steps = [
                python_step("prints-cjk", code),
                python_step("after-cjk", marker_code(base / "after-cjk")),
            ]
            report = gate.run_gate(steps, root, base / "logs", "test", console=console)
            console.flush()
            text = buffer.getvalue().decode("cp1252")

            self.assertEqual([result.status for result in report.results], [gate.FAIL, gate.PASS])
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
                real_copy = gate._copy_output

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
                    root = fixture_repository(base)
                    if case == "interrupted after exit":
                        patch = mock.patch.object(gate, "_settle_group", interrupt)
                        steps = [python_step("exits", "pass")]
                    else:
                        patch = mock.patch.object(gate, "_copy_output", held_copy)
                        steps = [python_step("holds-pipe", "pass", timeout_s=1),
                                 python_step("after", marker_code(base / "after"))]
                    with patch, mock.patch.object(gate.SMOKE_RUNNER, "terminate_process_tree", refuse):
                        report, _console = run_quietly(steps, root, base / "logs")

                    result = report.results[0]
                    self.assertIn("EPERM", result.detail)
                    # The leader exited 0 and the runner reaped it, so its status is real.
                    self.assertEqual(result.exit_code, 0)
                    self.assertEqual(seen[0].returncode, 0)
                    self.assertTrue((base / "logs" / "summary.txt").is_file())
                    self.assertTrue((base / "logs" / "summary.json").is_file())
                    if case == "interrupted after exit":
                        self.assertEqual(result.status, gate.INTERRUPTED)
                        self.assertEqual(report.exit_code, 130)
                    else:
                        self.assertEqual(result.status, gate.TIMEOUT)
                        self.assertEqual(report.results[1].status, gate.PASS)
                        self.assertTrue((base / "after").exists())

    @unittest.skipUnless(sys.platform == "darwin", "macOS refuses a group kill on a zombie-only group")
    def test_a_zombie_only_group_kill_is_refused_on_macos_and_the_step_is_still_reaped(self):
        # Protect the real macOS path: once the leader has exited, its unreaped zombie is the only
        # member, and killpg fails with EPERM, not ESRCH. A deadline reached while a child outside
        # the group holds the pipe then records the refusal, reaps the leader, and writes summaries.
        leader = subprocess.Popen([sys.executable, "-c", "pass"], **gate.SMOKE_RUNNER.process_group_options())
        try:
            self.assertTrue(gate._await_leader(leader, 30, gate._LEADER_WATCH))
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
            root = fixture_repository(base)
            report, _console = run_quietly([python_step("holds-pipe", holder, timeout_s=1)], root, base / "logs")
            result = report.results[0]
            self.assertEqual(result.status, gate.TIMEOUT, result.detail)
            self.assertIn("EPERM", result.detail)
            self.assertEqual(result.exit_code, 0)
            self.assertTrue((base / "logs" / "summary.json").is_file())

    @POSIX_ONLY
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
            step = python_step("exits-7", "import sys; sys.exit(7)")
            previous = signal.signal(signal.SIGCHLD, signal.SIG_IGN)
            try:
                with mock.patch.object(gate.os, "killpg", killpg), mock.patch.object(gate.os, "kill", kill):
                    result = gate.run_step(step, 1, Path(directory), Path(directory), dict(os.environ))
            finally:
                signal.signal(signal.SIGCHLD, previous)
        self.assertEqual(result.status, gate.FAIL, result.detail)
        self.assertIsNone(result.exit_code)
        self.assertIn("exit status unavailable", result.detail)
        self.assertEqual(sent, [])


class GitStateTests(unittest.TestCase):
    """The runner reports Git state around the run and never cleans the tree."""

    def test_a_dirty_tree_is_reported_as_pre_existing_and_left_unchanged(self):
        # Protect contributors' uncommitted work: dirt present before the run is reported, not
        # blamed on the run, and every byte, status line, and index entry survives it.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            (root / "tracked.txt").write_bytes(b"edited before the run\n")
            (root / "untracked.txt").write_bytes(b"untracked\n")
            (root / "staged.txt").write_bytes(b"staged\n")
            git(root, "add", "staged.txt")
            tree = tree_bytes(root)
            status = git(root, "status", "--porcelain=v1", "--untracked-files=all").stdout
            index = git(root, "ls-files", "--stage").stdout

            report, console = run_quietly([python_step("passes", "pass")], root, base / "logs")

            self.assertEqual(report.exit_code, 0)
            self.assertEqual(report.changes, ())
            self.assertEqual(
                {path for path, _code, _fingerprint in report.before.entries},
                {"tracked.txt", "untracked.txt", "staged.txt"},
            )
            self.assertEqual(tree_bytes(root), tree)
            self.assertEqual(
                git(root, "status", "--porcelain=v1", "--untracked-files=all").stdout, status
            )
            self.assertEqual(git(root, "ls-files", "--stage").stdout, index)
            self.assertIn("pre-existing changes, present before the run and not caused by it (3)", console)
            self.assertIn("the run left tracked and untracked state unchanged", console)

    def test_a_change_made_during_the_run_fails_the_gate_and_is_kept(self):
        # Protect the verdict's meaning: a run that edited the tree it tested fails, and the
        # edit, including a further edit to an already-dirty file, stays for review.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            (root / "tracked.txt").write_bytes(b"edited before the run\n")
            code = (
                "import pathlib; "
                "pathlib.Path('tracked.txt').write_bytes(b'edited by the step\\n'); "
                "pathlib.Path('made-by-step.txt').write_bytes(b'new\\n')"
            )
            report, console = run_quietly([python_step("edits", code)], root, base / "logs")

            self.assertEqual(report.results[0].status, gate.PASS)
            self.assertEqual(report.exit_code, 1)
            changes = "\n".join(report.changes)
            self.assertRegex(changes, r"tracked\.txt: M file 0[0-7]{3} sha256:\S+ -> M file 0[0-7]{3} sha256:")
            self.assertIn("made-by-step.txt: clean -> ??", changes)
            self.assertEqual((root / "tracked.txt").read_bytes(), b"edited by the step\n")
            self.assertTrue((root / "made-by-step.txt").is_file())
            self.assertIn("nothing was reverted", console)
            self.assertIn("verdict=FAIL", console)

    def test_a_mode_change_to_a_dirty_file_fails_the_gate(self):
        # Protect the snapshot from a chmod that Git's status cannot show: the file is already
        # modified, so its porcelain line stays ` M` and only the fingerprint's mode changes.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            tracked = root / "tracked.txt"
            tracked.write_bytes(b"edited before the run\n")
            code = (
                "import os, stat; "
                "os.chmod('tracked.txt', stat.S_IMODE(os.stat('tracked.txt').st_mode) ^ stat.S_IWUSR)"
            )
            try:
                report, _console = run_quietly([python_step("chmods", code)], root, base / "logs")
            finally:
                # Restore write access so the temporary directory can be removed on every host.
                tracked.chmod(0o644)

            self.assertEqual(report.results[0].status, gate.PASS)
            self.assertEqual(report.exit_code, 1)
            self.assertEqual(len(report.changes), 1, report.changes)
            self.assertRegex(report.changes[0], r"^tracked\.txt: M file 0[0-7]{3} .* -> M file 0[0-7]{3} ")

    def test_a_tracked_edit_fails_the_gate_wherever_the_logs_go(self):
        # Protect the snapshot from --log-dir: only the runner's own untracked logs and summaries
        # are left out, so an edit under a tracked log directory still fails the gate.
        for name, relative in (
            ("outside the tree", None),
            ("untracked directory", Path("logs")),
            ("tracked source directory", Path("src")),
            ("under a tracked directory", Path("src") / "logs"),
        ):
            with self.subTest(log_dir=name), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = fixture_repository(base)
                (root / "src").mkdir()
                (root / "src" / "module.txt").write_bytes(b"committed\n")
                git(root, "add", "src/module.txt")
                commit(root, "source")
                logs = base / "logs" if relative is None else root / relative

                # A passing run leaves logs behind; neither they nor the next run's own logs
                # count as a change.
                clean, _console = run_quietly([python_step("passes", "pass")], root, logs)
                self.assertEqual((clean.exit_code, clean.changes), (0, ()))

                code = (
                    "import pathlib; "
                    "pathlib.Path('src/module.txt').write_bytes(b'edited by the step\\n'); "
                    "pathlib.Path('src/new.txt').write_bytes(b'new\\n')"
                )
                report, _console = run_quietly([python_step("edits", code)], root, logs)
                self.assertEqual(report.results[0].status, gate.PASS)
                self.assertEqual(report.exit_code, 1)
                changes = "\n".join(report.changes)
                self.assertIn("src/module.txt: clean -> M file", changes)
                self.assertIn("src/new.txt: clean -> ?? file", changes)

    def test_a_tracked_or_aliased_runner_output_is_refused_before_any_step(self):
        # Protect the final audit: the summaries are written after the last snapshot, so an
        # output path that Git tracks, or a link to tracked content, is refused before a step
        # runs instead of being overwritten under a PASS that reports unchanged state.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            (root / "logs").mkdir()
            (root / "logs" / "summary.txt").write_bytes(b"tracked summary\n")
            git(root, "add", "logs/summary.txt")
            commit(root, "track a summary")
            cases = {"tracked summary": root / "logs"}
            if os.name != "nt":
                # Links need no privilege on POSIX; they sit outside the tree and alias tracked.txt.
                symlinked = base / "symlinked"
                symlinked.mkdir()
                (symlinked / "summary.txt").symlink_to(root / "tracked.txt")
                hard = base / "hard"
                hard.mkdir()
                os.link(root / "tracked.txt", hard / "summary.json")
                cases.update({"symlink to tracked content": symlinked, "hard link to tracked content": hard})
            marker = base / "step-ran"
            for case, log_dir in cases.items():
                with self.subTest(case=case):
                    with self.assertRaisesRegex(ValueError, "runner output"):
                        run_quietly([python_step("marks", marker_code(marker))], root, log_dir)
                    self.assertFalse(marker.exists(), "a step ran before the refusal")
                    self.assertEqual((root / "tracked.txt").read_bytes(), b"committed\n")
                    self.assertEqual((root / "logs" / "summary.txt").read_bytes(), b"tracked summary\n")
            # The command line refuses the same log directory with exit 2 before any step runs.
            completed = subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "local-gate.py"), "--root", str(root),
                 "--log-dir", str(root / "logs"), "--step", "no-raw-exit"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60, check=False,
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn(b"is tracked by Git", completed.stderr)
            self.assertEqual((root / "logs" / "summary.txt").read_bytes(), b"tracked summary\n")

    @POSIX_ONLY
    def test_a_link_that_appears_at_an_output_during_the_run_fails_it(self):
        # Protect the final audit from links a step plants mid-run: a symlink or hard link at a
        # summary or a later log, or a replaced log directory, is never written through, tracked
        # content stays intact, no tail is read through a link or a replaced directory, and the
        # run fails; a replaced directory also stops the run before later steps and summaries.
        for case in ("summary symlink", "summary hard link", "next log symlink", "own log symlink",
                     "replaced log directory"):
            with self.subTest(case=case):
                with tempfile.TemporaryDirectory() as directory:
                    base = Path(directory)
                    root = fixture_repository(base)
                    (root / "tracked-logs").mkdir()
                    (root / "tracked-logs" / "summary.txt").write_bytes(b"tracked summary\n")
                    # Unrelated tracked content under the log name of the replaced-directory step.
                    (root / "tracked-logs" / "01-moves.log").write_bytes(b"unrelated tracked log\n")
                    git(root, "add", "tracked-logs/summary.txt", "tracked-logs/01-moves.log")
                    commit(root, "track a summary")
                    logs = base / "logs"
                    tracked = str(root / "tracked.txt")
                    if case == "summary symlink":
                        target = logs / "summary.txt"
                        steps = [python_step("plants", f"import os; os.symlink({tracked!r}, {str(target)!r})")]
                    elif case == "summary hard link":
                        target = logs / "summary.json"
                        steps = [python_step("plants", f"import os; os.link({tracked!r}, {str(target)!r})")]
                    elif case == "next log symlink":
                        target = logs / "02-second.log"
                        steps = [python_step("plants", f"import os; os.symlink({tracked!r}, {str(target)!r})"),
                                 python_step("second", "pass")]
                    elif case == "own log symlink":
                        target = logs / "01-swaps.log"
                        code = (f"import os, sys; os.unlink({str(target)!r}); "
                                f"os.symlink({tracked!r}, {str(target)!r}); sys.exit(1)")
                        steps = [python_step("swaps", code)]
                    else:
                        # The step fails after the swap, so its tail would be read through the new directory.
                        target = logs
                        code = (f"import os, sys; os.rename({str(logs)!r}, {str(base / 'logs-moved')!r}); "
                                f"os.symlink({str(root / 'tracked-logs')!r}, {str(logs)!r}); sys.exit(1)")
                        steps = [python_step("moves", code), python_step("after", marker_code(base / "after"))]
                    report, console = run_quietly(steps, root, logs)

                    self.assertEqual((root / "tracked.txt").read_bytes(), b"committed\n")
                    self.assertEqual((root / "tracked-logs" / "summary.txt").read_bytes(), b"tracked summary\n")
                    self.assertNotIn("committed", console)
                    self.assertNotIn("unrelated tracked log", console)
                    tracked_log = (root / "tracked-logs" / "01-moves.log").read_bytes()
                    self.assertEqual(tracked_log, b"unrelated tracked log\n")
                    self.assertEqual(report.exit_code, 1)
                    self.assertIn(str(target), "\n".join(report.output_problems))
                    if case in ("summary symlink", "summary hard link", "next log symlink"):
                        # The runner replaced the link with its own file instead of writing through it.
                        self.assertFalse(target.is_symlink())
                        self.assertEqual(os.lstat(target).st_nlink, 1)
                    if case == "replaced log directory":
                        # The run stopped: the later step did not run and no summary was written anywhere.
                        self.assertEqual([result.status for result in report.results], [gate.FAIL])
                        self.assertFalse((base / "after").exists())
                        self.assertFalse((base / "logs-moved" / "summary.txt").exists())
                        for where in (base / "logs-moved", root / "tracked-logs"):
                            self.assertFalse((where / "summary.json").exists())

    @POSIX_ONLY
    def test_a_hung_fixture_git_is_killed_at_its_deadline(self):
        # Protect the suite from a hung Git: the fixture helper kills and reaps Git's whole
        # tree at its deadline instead of waiting on it forever.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            survivor = base / "background-survived"
            fake_git = f"#!/bin/sh\n(sleep 2; echo alive > '{survivor}') &\nexec sleep 30\n"
            bin_dir = _write_tools(base, {"git": fake_git})
            path = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")
            started = time.monotonic()
            with mock.patch.dict(os.environ, {"PATH": path}):
                with self.assertRaises(subprocess.TimeoutExpired):
                    git(base, "status", timeout=0.5)
            self.assertLess(time.monotonic() - started, 10)
            # Outlast the background child's sleep, so a survivor would have written its marker.
            time.sleep(2.5)
            self.assertFalse(survivor.exists(), "the hung git's background child outlived the kill")

    def test_outside_a_git_work_tree_state_is_unavailable_not_fatal(self):
        # Protect exported trees: without Git the run still completes and says what it lacks.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve() / "plain"
            root.mkdir()
            ceiling = {"GIT_CEILING_DIRECTORIES": str(root.parent)}
            with mock.patch.dict(os.environ, ceiling):
                report, console = run_quietly(
                    [python_step("passes", "pass")], root, Path(directory) / "logs"
                )

            self.assertFalse(report.before.available)
            self.assertEqual(report.exit_code, 0)
            self.assertIn("Git state unavailable", console)


class TableTests(unittest.TestCase):
    """The step table is well formed and selects the right steps per host."""

    def test_every_step_is_well_formed(self):
        # Protect the invariants the runner, the renderer, and the parity checks rely on.
        ids = [step.id for step in gate.STEPS]
        self.assertEqual(len(ids), len(set(ids)))
        commands = [gate.command_text(step) for step in gate.STEPS]
        self.assertEqual(len(commands), len(set(commands)))
        for step in gate.STEPS:
            with self.subTest(step=step.id):
                self.assertRegex(step.id, r"^[a-z][a-z0-9-]*$")
                self.assertTrue(step.argv)
                self.assertTrue(step.hosts)
                self.assertLessEqual(set(step.hosts), set(gate.HOSTS))
                self.assertEqual(len(step.hosts), len(set(step.hosts)))
                self.assertIn(step.evidence, gate.EVIDENCE_CLASSES)
                self.assertGreater(step.timeout_s, 0)
                self.assertTrue(step.prerequisites)
                self.assertLessEqual(set(step.prerequisites), set(gate.PREREQUISITES))
                self.assertIn(step.shell, (None, "pwsh"))
                if step.evidence == "optional":
                    self.assertEqual(step.ci_jobs, ())
                for job in step.ci_jobs:
                    self.assertIn(gate.job_host(job), step.hosts)

    def test_host_selection_keeps_table_order_and_opt_in_classes(self):
        # Protect the default gate: each host runs its local steps in table order, release
        # steps only on request, and the doctests after the build they reuse.
        macos = [step.id for step in gate.select_steps("macos")]
        self.assertEqual(
            macos,
            [step.id for step in gate.STEPS if "macos" in step.hosts and step.evidence == "local"],
        )
        self.assertNotIn("release-macos", macos)
        self.assertIn("release-macos", [step.id for step in gate.select_steps("macos", release=True)])
        self.assertEqual(macos.index("doctests"), macos.index("workspace-crates") + 1)
        windows = [step.id for step in gate.select_steps("windows")]
        self.assertIn("msi-validator-tests", windows)
        self.assertIn("windows-warp-allocator", windows)
        self.assertNotIn("logic-coverage", windows)
        self.assertEqual(
            [step.id for step in gate.select_steps("linux", ids=["doctests", "fmt"])],
            ["fmt", "doctests"],
        )
        with self.assertRaises(ValueError):
            gate.select_steps("macos", ids=["no-such-step"])
        with self.assertRaises(ValueError):
            gate.select_steps("macos", ids=["msi-validator-tests"])

    def test_doctest_step_compiles_workspace_doctests_without_an_exemption(self):
        # Protect the first-party doctest from going uncompiled: the step builds every workspace
        # doctest in each platform's workspace-test job, and no manifest opts a library out.
        step = next(step for step in gate.STEPS if step.id == "doctests")
        self.assertEqual(gate.command_text(step), "cargo test --workspace --doc --no-fail-fast")
        self.assertEqual(step.evidence, "local")
        self.assertEqual(set(step.ci_jobs), {"macos-core", "windows-tests", "linux-core"})
        logging = (ROOT / "crates" / "sonicterm-logging" / "src" / "lib.rs").read_text(encoding="utf-8")
        self.assertIn("//! ```no_run", logging)
        for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
            if manifest.parent.name == "sonicterm-winit":
                continue
            with self.subTest(manifest=manifest.parent.name):
                self.assertNotRegex(manifest.read_text(encoding="utf-8"), r"(?m)^doctest\s*=\s*false")

    def test_pty_close_baseline_is_local_on_every_desktop(self):
        # The ignored native baseline is selected explicitly on every host, never hidden in CI-only evidence.
        matches = [step for step in gate.STEPS if step.id == "pty-close-baseline"]
        self.assertEqual(len(matches), 1)
        step = matches[0]
        self.assertEqual(
            gate.command_text(step),
            "cargo test -p sonicterm-app --lib pty_close_baseline -- --ignored --nocapture",
        )
        self.assertEqual(step.hosts, gate.HOSTS)
        self.assertEqual(step.evidence, "local")
        self.assertEqual(step.prerequisites, ("rust", "native"))
        self.assertEqual(step.ci_jobs, ("macos-core", "windows-tests", "linux-core"))
        self.assertEqual(step.timeout_s, 1200)
        for host in gate.HOSTS:
            self.assertIn(step, gate.select_steps(host))

    def test_command_text_matches_ci_spelling(self):
        # Protect verbatim parity: env prefixes and PowerShell paths render exactly as ci.yml
        # spells them, while the runner still launches the PowerShell script through pwsh.
        by_id = {step.id: step for step in gate.STEPS}
        self.assertEqual(
            gate.command_text(by_id["doc"]),
            'RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps',
        )
        self.assertEqual(
            gate.command_text(by_id["msi-validator-tests"]),
            ".\\scripts\\validate-windows-msi_tests.ps1",
        )
        self.assertEqual(
            gate.launch_argv(by_id["msi-validator-tests"]),
            ("pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-File",
             ".\\scripts\\validate-windows-msi_tests.ps1"),
        )

    def test_host_detection(self):
        # Protect step selection on each desktop host, including Git Bash's MSYS Python.
        for system, host in (
            ("Darwin", "macos"),
            ("Linux", "linux"),
            ("Windows", "windows"),
            ("MSYS_NT-10.0-26100", "windows"),
            ("FreeBSD", None),
        ):
            with self.subTest(system=system):
                self.assertEqual(gate.detect_host(system), host)

    def test_windows_target_is_an_optional_macos_aid_and_never_a_ci_gate(self):
        # Protect the Windows-target check from becoming a CI gate or a default local step.
        step = next(step for step in gate.STEPS if step.id == "windows-target")
        self.assertEqual(step.evidence, "optional")
        self.assertEqual(step.ci_jobs, ())
        self.assertEqual(step.hosts, ("macos",))
        self.assertEqual(gate.command_text(step), "bash scripts/check-windows-target.sh")
        self.assertIn("win-target", step.prerequisites)
        self.assertNotIn(
            "windows-target", [step.id for step in gate.select_steps("macos", release=True)]
        )
        self.assertIn("windows-target", [step.id for step in gate.select_steps("macos", optional=True)])
        self.assertNotIn("check-windows-target.sh", WORKFLOW)


class CiParityTests(unittest.TestCase):
    """The table and the reasoned CI-only list account for every ci.yml gate invocation."""

    def test_the_parser_reads_every_ci_job(self):
        # Protect parity from passing vacuously: the scan must find the jobs and commands it checks.
        jobs = gate.ci_job_commands(WORKFLOW)
        self.assertEqual(
            set(jobs),
            {
                "macos-core", "macos-coverage", "macos-smoke", "macos",
                "windows-native", "windows-checks", "windows-tests", "windows-smoke", "windows",
                "linux-core", "linux-packages", "linux",
            },
        )
        commands = {job: [command for _label, command in pairs] for job, pairs in jobs.items()}
        for job in ("macos-core", "windows-checks", "linux-core"):
            self.assertIn("cargo fmt --all --check", commands[job])
        # A multi-command block and a PowerShell continuation both split into their commands.
        self.assertIn("bash scripts/test-wiki-publish.sh", commands["linux-core"])
        self.assertIn(
            'RUSTDOCFLAGS="-D warnings" cargo doc -p sonicterm-resource --all-features --no-deps',
            commands["linux-core"],
        )
        self.assertTrue(any(
            command.startswith("python scripts/native-smoke-runner.py --timeout-seconds 120 ")
            and command.endswith("-- --nocapture")
            for command in commands["windows-tests"]
        ))

    def test_the_shipped_workflow_matches_the_table(self):
        # Protect the real repository: every table step runs verbatim where it says, and every
        # ci.yml gate invocation is a table step or a reasoned CI-only entry.
        self.assertEqual(gate.ci_parity_problems(WORKFLOW), [])
        self.assertEqual(gate.ci_only_problems(ROOT, WORKFLOW), [])

    def test_editing_a_ci_gate_step_alone_fails_parity(self):
        # Protect against a ci.yml gate command drifting from the table.
        old = "run: cargo clippy --workspace --all-targets -- -D warnings"
        self.assertEqual(WORKFLOW.count(old), 3)
        mutated = WORKFLOW.replace(old, old + " -W clippy::pedantic", 1)
        problems = gate.ci_parity_problems(mutated)
        self.assertTrue(any("`cargo clippy --workspace --all-targets -- -D warnings` is missing "
                            "from CI job macos-core" in problem for problem in problems), problems)
        self.assertTrue(any("neither a table step nor on the CI-only list" in problem
                            for problem in problems), problems)

    def test_a_new_ci_gate_invocation_must_be_tabled_or_allow_listed(self):
        # Protect against a new CI-only gate that a local run would never execute.
        anchor = "      - name: Clippy\n"
        self.assertEqual(WORKFLOW.count(anchor), 3)
        added = (
            "      - name: New gate\n"
            "        timeout-minutes: 2\n"
            "        run: bash scripts/check-something-new.sh\n\n"
        )
        problems = gate.ci_parity_problems(WORKFLOW.replace(anchor, added + anchor, 1))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("`bash scripts/check-something-new.sh`, which is neither a table step", problems[0])

    def test_a_table_step_must_run_in_exactly_the_jobs_it_names(self):
        # Protect the table's CI-jobs column from naming a job that does not run the step, or
        # omitting one that does.
        fmt = next(step for step in gate.STEPS if step.id == "fmt")
        for replacement, expected in (
            (dataclasses.replace(fmt, ci_jobs=fmt.ci_jobs + ("windows-tests",)),
             "`cargo fmt --all --check` is missing from CI job windows-tests"),
            (dataclasses.replace(fmt, ci_jobs=("macos-core", "windows-checks")),
             "runs table step fmt, but the table does not name linux-core"),
        ):
            steps = tuple(replacement if step.id == "fmt" else step for step in gate.STEPS)
            problems = gate.ci_parity_problems(WORKFLOW, steps=steps)
            self.assertTrue(any(expected in problem for problem in problems), problems)

    def test_a_ci_only_cargo_test_must_rerun_a_local_target(self):
        # Protect the evidence-rerun distinction: an allow-listed cargo test that no local step
        # runs is a missing local test, whatever the entry calls itself.
        missing = gate.CiOnly(
            "evidence-rerun",
            "cargo test -p sonicterm-gpu --test no_such_target -- --nocapture",
            ("macos-core",),
            "fixture",
        )
        problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(missing,))
        self.assertTrue(any("tests/no_such_target.rs does not exist" in problem
                            for problem in problems), problems)
        rerun = next(entry for entry in gate.CI_ONLY if entry.kind == "evidence-rerun")
        problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(dataclasses.replace(rerun, kind="setup"),))
        self.assertTrue(any("missing local test" in problem for problem in problems), problems)

    def test_a_first_party_self_test_cannot_be_ci_only(self):
        # Protect the MSI-validator class of gap: a script's own tests must be a table step.
        without = tuple(step for step in gate.STEPS if step.id != "msi-validator-tests")
        entry = gate.CiOnly(
            "runtime-evidence", ".\\scripts\\validate-windows-msi_tests.ps1", ("windows-tests",), "fixture"
        )
        entries = gate.CI_ONLY + (entry,)
        # Parity alone would accept the entry, so only the self-test rule catches it.
        self.assertEqual(gate.ci_parity_problems(WORKFLOW, steps=without, entries=entries), [])
        problems = gate.ci_only_problems(ROOT, WORKFLOW, steps=without, entries=entries)
        self.assertTrue(any("first-party self-test" in problem for problem in problems), problems)

    def test_rerun_evidence_is_checked_against_the_test_source(self):
        # Protect reruns from hiding a target that the workspace pass skips as ignored.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            crate = root / "crates" / "fixture-crate"
            (crate / "tests").mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "fixture-crate"\n', encoding="utf-8")
            (root / "scripts").mkdir()
            (root / "scripts" / "check-workspace-crates.sh").write_text(
                "cargo test --workspace --lib --bins --tests --no-fail-fast\n", encoding="utf-8"
            )
            workflow = textwrap.dedent(
                """\
                name: Fixture
                jobs:
                  macos-core:
                    runs-on: macos-14
                    timeout-minutes: 5
                    steps:
                      - name: Run workspace unit and integration tests
                        timeout-minutes: 5
                        run: bash scripts/check-workspace-crates.sh

                      - name: Report probe
                        timeout-minutes: 5
                        run: cargo test -p fixture-crate --test probe -- --nocapture
                """
            )
            entry = gate.CiOnly(
                "evidence-rerun",
                "cargo test -p fixture-crate --test probe -- --nocapture",
                ("macos-core",),
                "fixture",
            )
            probe = crate / "tests" / "probe.rs"
            probe.write_text("#[test]\n#[ignore]\nfn probe() {}\n", encoding="utf-8")
            problems = gate.ci_only_problems(root, workflow, entries=(entry,))
            self.assertTrue(any("ignored tests" in problem for problem in problems), problems)

            probe.write_text("#[test]\nfn probe() {}\n", encoding="utf-8")
            self.assertEqual(gate.ci_only_problems(root, workflow, entries=(entry,)), [])

    def test_a_stale_ci_only_entry_is_reported(self):
        # Protect the allow-list from outliving the step it excuses.
        line = "run: cargo test -p sonicterm-gpu --test renderer_churn_baseline -- --nocapture"
        self.assertEqual(WORKFLOW.count(line), 2)
        problems = gate.ci_parity_problems(WORKFLOW.replace(line, "run: echo removed"))
        for job in ("macos-core", "windows-tests"):
            self.assertTrue(any(f"no longer runs in CI job {job}" in problem
                                for problem in problems), problems)

    def test_msi_validator_tests_join_the_windows_table(self):
        # Protect the Windows gate from omitting a test that CI runs.
        step = next(step for step in gate.STEPS if step.id == "msi-validator-tests")
        self.assertEqual(step.hosts, ("windows",))
        self.assertEqual(step.ci_jobs, ("windows-tests",))
        self.assertEqual(step.evidence, "local")
        self.assertNotIn(gate.command_text(step), [entry.command for entry in gate.CI_ONLY])

    def test_doctests_follow_each_shards_workspace_tests(self):
        # Protect the doctest step's reuse of the libraries the workspace step just built.
        jobs = gate.ci_job_commands(WORKFLOW)
        for job in ("macos-core", "windows-tests", "linux-core"):
            with self.subTest(job=job):
                commands = [command for _label, command in jobs[job]]
                index = commands.index("bash scripts/check-workspace-crates.sh")
                self.assertEqual(commands[index + 1], "cargo test --workspace --doc --no-fail-fast")


def _with_step(step: str) -> str:
    """Insert a step before macos-core's Clippy step in the complete workflow."""
    anchor = "      - name: Clippy\n"
    if WORKFLOW.count(anchor) != 3:
        raise AssertionError("the Clippy anchor moved; update the mutation tests")
    return WORKFLOW.replace(anchor, step + anchor, 1)


class CiParserTests(unittest.TestCase):
    """The ci.yml reader parses the forms it models and raises on every other form."""

    def test_a_block_header_comment_is_read_as_a_block(self):
        # Protect `run: | # comment`: the block is parsed, so a new gate in it is still checked.
        workflow = _with_step(
            "      - name: New gate\n"
            "        timeout-minutes: 2\n"
            "        run: | # explained here\n"
            "          bash scripts/check-something-new.sh\n\n"
        )
        commands = [command for _label, command in gate.ci_job_commands(workflow)["macos-core"]]
        self.assertIn("bash scripts/check-something-new.sh", commands)
        problems = gate.ci_parity_problems(workflow)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("neither a table step nor on the CI-only list", problems[0])

    def test_every_unmodeled_run_form_raises(self):
        # Protect parity from a new gate step that an unfamiliar spelling would hide: each of
        # these forms raises against the complete workflow instead of being skipped.
        new = "bash scripts/check-something-new.sh"
        forms = {
            "double-quoted key": f'        "run": {new}\n',
            "single-quoted key": f"        'run': {new}\n",
            "value on the next line": f"        run:\n          {new}\n",
            "continued plain value": f"        run: {new}\n          --extra\n",
            "folded block": f"        run: >\n          {new}\n",
            "stripped folded block": f"        run: >-\n          {new}\n",
            "indentation indicator": f"        run: |2\n          {new}\n",
            "unknown header text": f"        run: |x\n          {new}\n",
            "double-quoted value": f'        run: "{new}"\n',
            "single-quoted value": f"        run: '{new}'\n",
            "flow value": f"        run: [{new}]\n",
            "comment after a plain value": f"        run: {new} # note\n",
            "misaligned key": f"         run: {new}\n",
            "tab in indentation": f"        run: |\n        \t{new}\n",
            "block line left of its first line": f"        run: |\n            echo first\n          {new}\n",
            "empty block": "        run: |\n",
            "second run key": f"        run: echo first\n        run: {new}\n",
            "working directory": f"        working-directory: crates\n        run: {new}\n",
            "local action": "        uses: ./.github/actions/gate\n",
        }
        for form, body in forms.items():
            with self.subTest(form=form):
                workflow = _with_step("      - name: New gate\n        timeout-minutes: 2\n" + body + "\n")
                with self.assertRaises(ValueError):
                    gate.ci_job_commands(workflow)

    def test_every_unmodeled_job_form_raises(self):
        # Protect parity from jobs whose steps the reader cannot see: each form raises.
        anchor = "  macos-coverage:\n"
        self.assertEqual(WORKFLOW.count(anchor), 1)
        forms = {
            "reusable workflow job": "  reusable:\n    uses: ./.github/workflows/other.yml\n\n",
            "job without steps": "  empty:\n    runs-on: ubuntu-latest\n\n",
            "flow-style steps": "  flow:\n    runs-on: ubuntu-latest\n    steps: []\n\n",
            "quoted job id": '  "quoted":\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n\n',
            "step item without a key": (
                "  bare:\n    runs-on: ubuntu-latest\n    steps:\n      -\n        run: bash scripts/x.sh\n\n"
            ),
            "odd indentation": "  odd:\n    runs-on: ubuntu-latest\n    steps:\n     - run: bash scripts/x.sh\n\n",
        }
        for form, job in forms.items():
            with self.subTest(form=form):
                with self.assertRaises(ValueError):
                    gate.ci_job_commands(WORKFLOW.replace(anchor, job + anchor, 1))

    def test_inherited_run_defaults_raise_like_a_step_working_directory(self):
        # Protect parity from defaults that move or rewrap every run step: workflow and job
        # defaults, in block or flow form and before or after jobs, raise as a step's own
        # working-directory does.
        job = "  macos-core:\n    name: macOS core gates\n"
        self.assertEqual(WORKFLOW.count(job), 1)
        block = "defaults:\n  run:\n    working-directory: other-project\n"
        flow = "defaults: {run: {working-directory: other-project}}\n"
        forms = {
            "workflow block": WORKFLOW.replace("\njobs:\n", "\n" + block + "\njobs:\n", 1),
            "workflow flow": WORKFLOW.replace("\njobs:\n", "\n" + flow + "\njobs:\n", 1),
            "workflow block after jobs": WORKFLOW.rstrip("\n") + "\n\n" + block,
            "quoted workflow key": WORKFLOW.replace(
                "\njobs:\n", '\n"defaults": {run: {shell: bash}}\n\njobs:\n', 1
            ),
            "job block": WORKFLOW.replace(
                job, job + "    defaults:\n      run:\n        working-directory: other-project\n", 1
            ),
            "job flow": WORKFLOW.replace(
                job, job + "    defaults: {run: {working-directory: other-project}}\n", 1
            ),
        }
        for form, workflow in forms.items():
            with self.subTest(form=form):
                self.assertNotEqual(workflow, WORKFLOW)
                with self.assertRaisesRegex(ValueError, "defaults"):
                    gate.ci_job_commands(workflow)
                with self.assertRaisesRegex(ValueError, "defaults"):
                    gate.ci_parity_problems(workflow)

    def test_a_step_shell_other_than_bash_or_pwsh_raises(self):
        # Protect parity from a shell that moves or rewraps a step's commands: a step shell:
        # other than a plain bash or pwsh raises as a working-directory does, in any spelling,
        # while the plain names the shipped workflow uses are still read.
        step = (
            "      - name: Rewrapped gate\n"
            "        timeout-minutes: 2\n"
            "SHELL"
            "        run: cargo fmt --all --check\n\n"
        )
        forms = {
            "sh": "        shell: sh\n",
            "cmd": "        shell: cmd\n",
            "python": "        shell: python\n",
            "powershell": "        shell: powershell\n",
            "custom template": "        shell: env -C other-project bash -e {0}\n",
            "expression": "        shell: ${{ matrix.shell }}\n",
            "comment after the name": "        shell: bash # note\n",
            "single-quoted value": "        shell: 'bash'\n",
            "double-quoted value": '        shell: "pwsh"\n',
            "quoted key": '        "shell": sh\n',
            "flow sequence": "        shell: [bash]\n",
            "flow mapping": "        shell: {bash: -e}\n",
            "literal block": "        shell: |\n          bash\n",
            "folded block": "        shell: >-\n          env -C other-project bash -e {0}\n",
            "value on the next line": "        shell:\n          bash\n",
            "continued plain value": "        shell: bash\n          -e {0}\n",
            "continued template": "        shell: bash\n          -c 'cd other-project; bash {0}'\n",
        }
        for form, shell in forms.items():
            with self.subTest(form=form):
                workflow = _with_step(step.replace("SHELL", shell))
                with self.assertRaisesRegex(ValueError, "shell"):
                    gate.ci_job_commands(workflow)
                with self.assertRaisesRegex(ValueError, "shell"):
                    gate.ci_parity_problems(workflow)
        # Named literally, so widening the allowed set needs a test change too.
        for name in ("bash", "pwsh"):
            with self.subTest(shell=name):
                workflow = _with_step(step.replace("SHELL", f"        shell: {name}\n"))
                commands = [command for _label, command in gate.ci_job_commands(workflow)["macos-core"]]
                self.assertEqual(commands.count("cargo fmt --all --check"), 2)


class CommandClassificationTests(unittest.TestCase):
    """Gate commands are normalized, then every invocation in a line is classified and proved."""

    def test_spellings_normalize_to_one_invocation(self):
        # Protect classification from spelling: a toolchain override, quotes, and either path
        # separator name the same gate.
        plain, reasons = gate.classify_command("cargo test -p sonicterm-gpu --test probe -- --nocapture")
        self.assertEqual(reasons, [])
        pinned, reasons = gate.classify_command(
            "cargo +stable test -p sonicterm-gpu --test probe -- --nocapture"
        )
        self.assertEqual(reasons, [])
        self.assertEqual(
            [(invocation.kind, invocation.name, invocation.args) for invocation in pinned],
            [(invocation.kind, invocation.name, invocation.args) for invocation in plain],
        )
        self.assertEqual(pinned[0].toolchain, "+stable")
        for spelling in (
            "python3 scripts/check_tests.py",
            'python3 "scripts/check_tests.py"',
            "python3 'scripts/check_tests.py'",
            "python3 scripts\\check_tests.py",
            "python3 ./scripts/check_tests.py",
            '"$python_cmd" scripts/check_tests.py',
        ):
            with self.subTest(spelling=spelling):
                invocations, reasons = gate.classify_command(spelling)
                self.assertEqual(reasons, [])
                self.assertEqual(
                    [(invocation.kind, invocation.name) for invocation in invocations],
                    [("script", "check_tests.py")],
                )
                self.assertTrue(gate.is_self_test(invocations[0]))

    def test_a_toolchain_override_is_still_a_cargo_test(self):
        # Protect the missing-local-test rule from `cargo +toolchain test`, which the plain
        # spelling would not match.
        probe = "cargo +stable test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture"
        setup = gate.CiOnly("setup", probe, ("macos-core",), "fixture")
        problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(setup,))
        self.assertTrue(any("missing local test" in problem for problem in problems), problems)
        missing = gate.CiOnly(
            "evidence-rerun",
            "cargo +nightly test -p sonicterm-gpu --test no_such_target -- --nocapture",
            ("macos-core",),
            "fixture",
        )
        problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(missing,))
        self.assertTrue(any("no_such_target.rs does not exist" in problem for problem in problems), problems)
        # A rerun that widens the run, such as with --ignored, is not the workspace pass.
        widened = gate.CiOnly("evidence-rerun", probe + " --ignored", ("macos-core",), "fixture")
        problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(widened,))
        self.assertTrue(any("must be exactly" in problem for problem in problems), problems)
        mutated = WORKFLOW.replace("run: cargo fmt --all --check", "run: cargo +stable fmt --all --check", 1)
        problems = gate.ci_parity_problems(mutated)
        self.assertTrue(any("`cargo +stable fmt --all --check`, which is neither a table step"
                            in problem for problem in problems), problems)

    def test_quoted_or_backslash_self_test_paths_cannot_be_ci_only(self):
        # Protect the self-test rule from quoting and path separators.
        for command in (
            'python3 "scripts/check-something_tests.py"',
            "bash scripts\\test-soak-harness.sh",
            'bash "scripts/test-soak-harness.sh"',
            '".\\scripts\\validate-windows-msi_tests.ps1"',
        ):
            with self.subTest(command=command):
                entry = gate.CiOnly("runtime-evidence", command, ("windows-tests",), "fixture")
                problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(entry,))
                self.assertTrue(any("first-party self-test" in problem for problem in problems), problems)
        workflow = _with_step(
            "      - name: New self-test\n"
            "        timeout-minutes: 2\n"
            '        run: python3 "scripts/check-something_tests.py"\n\n'
        )
        problems = gate.ci_parity_problems(workflow)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("scripts/check-something_tests.py", problems[0])

    def test_every_command_of_a_compound_line_is_checked(self):
        # Protect proofs from covering only the first command: each command joined by `&&` or
        # `;` is classified and proved, and a compact compound still fails parity.
        probe = "cargo test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture"
        hidden = "cargo test -p sonicterm-gpu --test no_such_target -- --nocapture"
        for joiner in (" && ", "; ", ";"):
            with self.subTest(joiner=joiner):
                entry = gate.CiOnly("evidence-rerun", probe + joiner + hidden, ("macos-core",), "fixture")
                problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(entry,))
                self.assertTrue(any("no_such_target.rs does not exist" in problem
                                    for problem in problems), problems)
                entry = gate.CiOnly(
                    "runtime-evidence", probe + joiner + "bash scripts/test-soak-harness.sh",
                    ("macos-core",), "fixture",
                )
                problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(entry,))
                self.assertTrue(any("first-party self-test" in problem for problem in problems), problems)
        problems = gate.ci_parity_problems(_with_step(
            "      - name: Compact\n        timeout-minutes: 2\n        run: echo start;cargo test --workspace\n\n"
        ))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("neither a table step nor on the CI-only list", problems[0])

    def test_every_line_of_a_run_block_is_checked(self):
        # Protect multi-line blocks: a gate on a later line of a block that starts with a
        # table command is still checked.
        mutated = WORKFLOW.replace(
            "        run: cargo fmt --all --check\n",
            "        run: |\n          cargo fmt --all --check\n          python3 scripts/check-something_tests.py\n",
            1,
        )
        self.assertNotEqual(mutated, WORKFLOW)
        problems = gate.ci_parity_problems(mutated)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("`python3 scripts/check-something_tests.py`, which is neither", problems[0])

    def test_unmodeled_spellings_are_rejected(self):
        # Protect parity from gates the classifier cannot prove run with their exit status
        # seen: each spelling fails loudly instead of passing as a non-gate line.
        spellings = {
            "cargo test --workspace | tee test.log": "beside `|`",
            "cargo test --workspace || true": "beside `||`",
            "cargo test --workspace &": "beside `&`",
            "(cargo test --workspace)": "beside `(`",
            "cargo test --workspace > test.log": "a redirected gate",
            "env cargo test --workspace": "a gate after `env`",
            "time cargo test --workspace": "a gate after `time`",
            "cargo --locked test --workspace": "a cargo option before the gate subcommand",
            "bash -x scripts/check-something-new.sh": "a gate after `bash`",
            'echo "$(cargo test --workspace)"': "a gate inside a command substitution",
            "python3 -c \"import subprocess; subprocess.run(['cargo', 'test'])\"": "a gate after `python3`",
            "cargo test --workspace 'unterminated": "an unterminated single quote",
        }
        for spelling, reason in spellings.items():
            with self.subTest(spelling=spelling):
                _invocations, reasons = gate.classify_command(spelling)
                self.assertTrue(any(reason in text for text in reasons), reasons)
                workflow = _with_step(
                    f"      - name: Unmodeled\n        timeout-minutes: 2\n        run: |\n          {spelling}\n\n"
                )
                problems = gate.ci_parity_problems(workflow)
                self.assertTrue(any("a spelling the gate classifier does not model" in problem
                                    for problem in problems), problems)
        entry = gate.CiOnly(
            "runtime-evidence",
            "cargo test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture | tee probe.log",
            ("macos-core",),
            "fixture",
        )
        problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(entry,))
        self.assertTrue(any("beside `|`" in problem for problem in problems), problems)

    def test_an_expression_that_decides_what_runs_is_rejected(self):
        # Protect parity from a workflow expression that picks the gate: in the cargo
        # subcommand, a script argument, or the command after `--`, it is reported as
        # unmodeled, while an expression in an ordinary data argument stays supported.
        spellings = (
            "cargo ${{ 'test' }} --workspace",
            "cargo ${{ matrix.cmd }} --workspace",
            "cargo +stable ${{ env.CARGO_GATE }} --workspace",
            "cargo --locked ${{ matrix.cmd }} --workspace",
            "python3 ${{ 'scripts/new_tests.py' }}",
            "bash ${{ matrix.script }}",
            "python3 -u ${{ env.SCRIPT }}",
            '"$python_cmd" ${{ matrix.script }}',
            "python3 scripts/native-smoke-runner.py -- cargo ${{ 'test' }} --workspace",
            "python3 scripts/native-smoke-runner.py -- python3 ${{ matrix.script }}",
        )
        for spelling in spellings:
            with self.subTest(spelling=spelling):
                _invocations, reasons = gate.classify_command(spelling)
                self.assertTrue(any("workflow expression" in reason for reason in reasons), reasons)
                problems = gate.ci_parity_problems(_with_step(
                    f"      - name: Expression\n        timeout-minutes: 2\n        run: |\n          {spelling}\n\n"
                ))
                self.assertTrue(any("a spelling the gate classifier does not model" in problem
                                    for problem in problems), problems)
        # The shipped smoke and package lines put expressions only in data arguments.
        for entry in gate.CI_ONLY:
            if "${{" in entry.command:
                with self.subTest(entry=entry.command):
                    invocations, reasons = gate.classify_command(entry.command)
                    self.assertEqual(reasons, [])
                    self.assertEqual(invocations[0].kind, "script")


class DocParityTests(unittest.TestCase):
    """CLAUDE.md and both wiki files embed exactly the table's rendered form."""

    def test_each_documented_gate_equals_the_rendered_table(self):
        # Protect every documented copy of the gate from drifting from the runnable table.
        self.assertEqual(
            [name for name, _language in gate.DOCUMENTS],
            ["CLAUDE.md", "wiki/Development-and-Release.md", "wiki/Development-and-Release-zh-CN.md"],
        )
        for name, language in gate.DOCUMENTS:
            with self.subTest(document=name):
                text = (ROOT / name).read_text(encoding="utf-8")
                self.assertEqual(gate.doc_parity_problems(name, text, language), [])

    def test_editing_one_documented_gate_line_alone_fails_parity(self):
        # Protect each copy separately: an edit to one document's gate line is caught.
        old = "| `fmt` | `cargo fmt --all --check` |"
        for name, language in gate.DOCUMENTS:
            with self.subTest(document=name):
                text = (ROOT / name).read_text(encoding="utf-8")
                self.assertEqual(text.count(old), 1)
                mutated = text.replace(old, "| `fmt` | `cargo fmt --all` |")
                problems = gate.doc_parity_problems(name, mutated, language)
                self.assertTrue(any("gate block differs from the table" in problem
                                    for problem in problems), problems)

    def test_editing_the_table_alone_fails_every_document(self):
        # Protect the other direction: a table change must be pasted into every document.
        fmt = next(step for step in gate.STEPS if step.id == "fmt")
        steps = tuple(
            dataclasses.replace(fmt, hosts=("macos",), ci_jobs=("macos-core",)) if step.id == "fmt"
            else step
            for step in gate.STEPS
        )
        for name, language in gate.DOCUMENTS:
            with self.subTest(document=name):
                text = (ROOT / name).read_text(encoding="utf-8")
                self.assertNotEqual(gate.doc_parity_problems(name, text, language, steps), [])

    def test_the_block_markers_must_appear_exactly_once(self):
        # Protect extraction: a missing or duplicated block cannot pass as the gate.
        block = gate.render_gate_block("en")
        prose = f"{gate.INVOCATION}\n\n"
        self.assertEqual(gate.doc_parity_problems("doc", prose + block, "en"), [])
        self.assertNotEqual(gate.doc_parity_problems("doc", prose + block + "\n" + block, "en"), [])
        self.assertNotEqual(gate.doc_parity_problems("doc", prose, "en"), [])
        self.assertNotEqual(gate.doc_parity_problems("doc", block, "en"), [])

    def test_both_languages_render_the_same_rows(self):
        # Protect equivalent facts across the English and Chinese blocks.
        def rows(language):
            return [line for line in gate.render_gate_block(language).splitlines() if line.startswith("| `")]

        english = rows("en")
        self.assertEqual(len(english), len(gate.STEPS))
        self.assertEqual([row.replace(", ", "、") for row in english], rows("zh-CN"))


class CommandLineTests(unittest.TestCase):
    """The CLI renders, lists, and rejects invalid selections without running a step."""

    def cli(self, *arguments: str, cwd: Path | None = None) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "local-gate.py"), *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
            cwd=cwd,
        )

    def test_render_prints_the_documented_block(self):
        # Protect the paste source that the docs parity tests compare against.
        for language in ("en", "zh-CN"):
            with self.subTest(language=language):
                completed = self.cli("--render", language)
                self.assertEqual(completed.returncode, 0, completed.stderr)
                self.assertEqual(
                    completed.stdout.decode("utf-8").replace("\r\n", "\n"),
                    gate.render_gate_block(language) + "\n",
                )

    def test_list_shows_another_hosts_steps(self):
        # Protect review of a host's gate from any other host.
        completed = self.cli("--list", "--host", "windows")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        listing = completed.stdout.decode("utf-8")
        self.assertIn("msi-validator-tests [local]", listing)
        self.assertIn(".\\scripts\\validate-windows-msi_tests.ps1", listing)
        self.assertNotIn("logic-coverage", listing)

    def test_invalid_selections_exit_2_without_running_a_step(self):
        # Protect callers from a typo or a foreign host silently running a different gate.
        detected = gate.detect_host()
        foreign = next(host for host in gate.HOSTS if host != detected)
        for arguments in (("--host", foreign), ("--step", "no-such-step")):
            with self.subTest(arguments=arguments):
                completed = self.cli(*arguments)
                self.assertEqual(completed.returncode, 2, completed.stderr)

    def test_a_log_dir_at_or_above_the_root_is_refused(self):
        # Protect the snapshot: a log directory at or above the root would contain every path
        # it reads, so the runner exits 2 before it runs a step or writes a log.
        with tempfile.TemporaryDirectory() as directory:
            root = fixture_repository(Path(directory))
            for log_dir in (".", "..", str(root)):
                with self.subTest(log_dir=log_dir):
                    completed = self.cli(
                        "--root", str(root), "--log-dir", log_dir, "--step", "no-raw-exit", cwd=root
                    )
                    self.assertEqual(completed.returncode, 2, completed.stderr)
                    self.assertIn(b"is the repository root or an ancestor of it", completed.stderr)
                    for parent in (root, root.parent):
                        self.assertFalse((parent / "summary.txt").exists())
                        self.assertFalse((parent / "01-no-raw-exit.log").exists())
            with self.assertRaises(ValueError):
                gate.run_gate([python_step("passes", "pass")], root, root, "test", console=io.StringIO())

    @POSIX_ONLY
    def test_an_ignored_sigchld_is_refused_before_any_step(self):
        # Protect exit statuses: with SIGCHLD ignored the kernel can reap each step as it exits, so
        # a failing step could read as 0; the runner exits 2 before it runs a step or writes a log.
        with tempfile.TemporaryDirectory() as directory:
            root = fixture_repository(Path(directory))
            logs = Path(directory) / "logs"
            completed = subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "local-gate.py"), "--root", str(root),
                 "--log-dir", str(logs), "--step", "no-raw-exit"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60, check=False,
                preexec_fn=lambda: signal.signal(signal.SIGCHLD, signal.SIG_IGN),
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn(b"SIGCHLD is ignored", completed.stderr)
            self.assertFalse(logs.exists())


_FAKE_CARGO = """#!/bin/sh
# Record CARGO_TARGET_DIR and every argument, separated by the unit separator.
{
  printf '%s' "${CARGO_TARGET_DIR-<unset>}"
  for argument in "$@"; do printf '\\037%s' "$argument"; done
  printf '\\n'
} >> "$FAKE_LOG"
if [ "$1" = metadata ]; then
  cat "$FAKE_METADATA"
  exit 0
fi
if [ -n "$FAKE_FAIL" ]; then
  case " $* " in
    *"$FAKE_FAIL"*) exit 7 ;;
  esac
fi
exit 0
"""
_FAKE_SUCCESS = "#!/bin/sh\nexit 0\n"


def _write_tools(directory: Path, tools: dict[str, str]) -> Path:
    """Write executable fake tools into a directory that goes first on PATH."""
    bin_dir = directory / "bin"
    bin_dir.mkdir()
    for name, body in tools.items():
        path = bin_dir / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)
    return bin_dir


def _calls(log: Path) -> list[tuple[str, list[str]]]:
    """Read the fake cargo's (CARGO_TARGET_DIR, arguments) record, one entry per call."""
    if not log.exists():
        return []
    calls = []
    for line in log.read_text(encoding="utf-8").splitlines():
        target_dir, *arguments = line.split("\x1f")
        calls.append((target_dir, arguments))
    return calls


def _tool_environment(bin_dir: Path, log: Path, target_dir: str | None, **extra: str) -> dict[str, str]:
    """Return an environment that finds the fake tools first and records into log."""
    env = dict(os.environ)
    env.pop("CARGO_TARGET_DIR", None)
    if target_dir is not None:
        env["CARGO_TARGET_DIR"] = target_dir
    env["PATH"] = str(bin_dir) + os.pathsep + env.get("PATH", "")
    env["FAKE_LOG"] = str(log)
    env.update(extra)
    return env


@POSIX_ONLY
class WorkspaceGateScriptTests(unittest.TestCase):
    """check-workspace-crates.sh keeps a caller's target directory and runs every phase."""

    def run_script(self, directory: Path, target_dir: str | None = None, fail: str = ""):
        bin_dir = _write_tools(
            directory,
            {"cargo": _FAKE_CARGO, "rustfmt": _FAKE_SUCCESS, "python3": _FAKE_SUCCESS},
        )
        log = directory / "calls.log"
        completed = subprocess.run(
            ["bash", str(ROOT / "scripts" / "check-workspace-crates.sh")],
            env=_tool_environment(bin_dir, log, target_dir, FAKE_FAIL=fail),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
        return completed, _calls(log)

    def test_nested_winit_phases_honor_a_caller_target_dir(self):
        # Protect CARGO_TARGET_DIR isolation through the nested pinned-winit builds.
        with tempfile.TemporaryDirectory() as directory:
            isolated = str(Path(directory) / "isolated-target")
            completed, calls = self.run_script(Path(directory), target_dir=isolated)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(len(calls), 3, calls)
            for _target_dir, arguments in calls[:2]:
                self.assertIn("crates/sonicterm-winit/Cargo.toml", arguments)
                self.assertEqual(arguments[arguments.index("--target-dir") + 1], isolated)
            target_dir, arguments = calls[2]
            self.assertEqual(arguments, ["test", "--workspace", "--lib", "--bins", "--tests", "--no-fail-fast"])
            self.assertEqual(target_dir, isolated)

    def test_without_a_caller_target_dir_winit_shares_the_workspace_target(self):
        # Protect the default cache reuse between the workspace and the pinned winit.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_script(Path(directory))

            self.assertEqual(completed.returncode, 0, completed.stderr)
            expected = os.path.join(os.path.realpath(ROOT), "target")
            for _target_dir, arguments in calls[:2]:
                value = arguments[arguments.index("--target-dir") + 1]
                self.assertEqual(os.path.realpath(value), expected)

    def test_a_failing_phase_does_not_stop_later_phases(self):
        # Protect the script's fail-complete contract across its winit and workspace phases.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_script(Path(directory), fail="sonicterm-winit")

            self.assertNotEqual(completed.returncode, 0)
            self.assertEqual(len(calls), 3, calls)
            self.assertEqual(calls[2][1][:2], ["test", "--workspace"])


_FAKE_RUSTC = """#!/bin/sh
if [ "$1" = --print ] && [ "$2" = sysroot ]; then
  printf '%s\\n' "$FAKE_SYSROOT"
  exit 0
fi
exit 1
"""

VERIFIED = (
    "sonicterm-types", "sonicterm-grid", "sonicterm-vt", "sonicterm-cfg", "sonicterm-logging",
    "sonicterm-resource", "sonicterm-text", "sonicterm-ui", "sonicterm-app-core", "sonicterm-io",
    "sonicterm-render-model", "sonicterm-block-glyph", "sonicterm-font-config",
)
EXCLUDED = (
    "sonicterm-freetype", "sonicterm-harfbuzz", "sonicterm-fontconfig", "sonicterm-font",
    "sonicterm-engine", "sonicterm-gpu", "sonicterm-app", "sonicterm-mac", "sonicterm-windows",
    "sonicterm-linux",
)


def _workspace_package_names() -> list[str]:
    """Read the workspace members' package names from the manifests, without Cargo."""
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    members = re.search(r"(?m)^members\s*=\s*\[([^\]]*)\]", manifest).group(1)
    names = []
    for member in re.findall(r'"([^"]+)"', members):
        package = (ROOT / member / "Cargo.toml").read_text(encoding="utf-8")
        names.append(re.search(r'(?m)^name\s*=\s*"([^"]+)"', package).group(1))
    return names


def _fake_metadata(names) -> str:
    """Return the `cargo metadata --no-deps` fields the classification reads."""
    packages = [{"name": name, "id": f"path+file:///fixture/{name}#0.0.0"} for name in names]
    return json.dumps({"packages": packages, "workspace_members": [p["id"] for p in packages]})


def _script_list(name: str) -> list[str]:
    """Read one bash array's names from check-windows-target.sh without running the script."""
    source = (ROOT / "scripts" / "check-windows-target.sh").read_text(encoding="utf-8")
    body = re.search(rf"(?ms)^{name}=\(\n(.*?)^\)", source)
    if body is None:
        raise AssertionError(f"check-windows-target.sh has no {name}=( ... ) list")
    entries = []
    for line in body.group(1).splitlines():
        entry = line.strip().strip('"')
        if entry and not entry.startswith("#"):
            entries.append(entry.split("|", 1)[0])
    return entries


class WindowsTargetClassificationTests(unittest.TestCase):
    """The Windows-target lists classify every workspace member, checked statically on every host."""

    def test_the_script_lists_classify_every_workspace_member(self):
        # Protect classification completeness in CI: the script's own lists, read without
        # running it, match Cargo.toml's members, so a new crate must be classified even
        # though CI never runs the optional windows-target step.
        verified = _script_list("verified")
        excluded = _script_list("excluded")
        self.assertEqual(verified, list(VERIFIED))
        self.assertEqual(excluded, list(EXCLUDED))
        self.assertEqual(set(verified) & set(excluded), set())
        self.assertEqual(sorted(verified + excluded), sorted(_workspace_package_names()))
        self.assertEqual(len(verified), 13)


@POSIX_ONLY
class WindowsTargetCheckTests(unittest.TestCase):
    """check-windows-target.sh classifies every member and pins the checked scope."""

    def run_check(self, directory: Path, *, names=None, installed: bool = True,
                  target_dir: str | None = None, fail: str = ""):
        bin_dir = _write_tools(directory, {"cargo": _FAKE_CARGO, "rustc": _FAKE_RUSTC})
        sysroot = directory / "sysroot"
        if installed:
            (sysroot / "lib" / "rustlib" / "x86_64-pc-windows-msvc" / "lib").mkdir(parents=True)
        else:
            sysroot.mkdir()
        metadata = directory / "metadata.json"
        metadata.write_text(
            _fake_metadata(_workspace_package_names() if names is None else names), encoding="utf-8"
        )
        log = directory / "calls.log"
        env = _tool_environment(
            bin_dir, log, target_dir,
            FAKE_METADATA=str(metadata), FAKE_SYSROOT=str(sysroot), FAKE_FAIL=fail,
        )
        completed = subprocess.run(
            ["bash", str(ROOT / "scripts" / "check-windows-target.sh")],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=60,
            check=False,
        )
        return completed, _calls(log)

    def test_verified_members_get_the_pinned_clippy_and_winit_scope(self):
        # Protect the measured boundary: exactly the verified members are linted with all
        # targets, denied warnings, and --locked, and the pinned winit's Windows tests compile.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_check(Path(directory))

            self.assertEqual(completed.returncode, 0, completed.stderr)
            commands = [arguments for _target_dir, arguments in calls]
            self.assertEqual([arguments[0] for arguments in commands], ["metadata", "clippy", "check"])
            clippy = commands[1]
            self.assertEqual(clippy[:4], ["clippy", "--locked", "--target", "x86_64-pc-windows-msvc"])
            packages = [clippy[index + 1] for index, value in enumerate(clippy) if value == "-p"]
            self.assertEqual(packages, list(VERIFIED))
            self.assertEqual(clippy[-4:], ["--all-targets", "--", "-D", "warnings"])
            check = commands[2]
            self.assertEqual(
                check[:6],
                ["check", "--locked", "--manifest-path", "crates/sonicterm-winit/Cargo.toml",
                 "--target", "x86_64-pc-windows-msvc"],
            )
            self.assertEqual(check[-4:], ["--features", "serde", "--lib", "--tests"])
            expected = os.path.join(os.path.realpath(ROOT), "target", "check-windows-target")
            self.assertEqual(
                os.path.realpath(clippy[clippy.index("--target-dir") + 1]),
                os.path.join(expected, "workspace"),
            )
            self.assertEqual(
                os.path.realpath(check[check.index("--target-dir") + 1]),
                os.path.join(expected, "winit"),
            )
            for name in EXCLUDED:
                self.assertRegex(completed.stdout, rf"\[windows-target\]   {re.escape(name)}: \S")
            self.assertIn("nothing runs", completed.stdout)

    def test_check_directories_nest_under_a_caller_target_dir(self):
        # Protect CARGO_TARGET_DIR isolation for the Windows-target artifacts.
        with tempfile.TemporaryDirectory() as directory:
            isolated = str(Path(directory) / "isolated")
            completed, calls = self.run_check(Path(directory), target_dir=isolated)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            clippy = calls[1][1]
            self.assertEqual(
                clippy[clippy.index("--target-dir") + 1],
                os.path.join(isolated, "check-windows-target", "workspace"),
            )

    def test_a_missing_target_prints_the_rustup_hint_and_fails(self):
        # Protect a first run from failing without saying how to install the target.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_check(Path(directory), installed=False)

            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("rustup target add x86_64-pc-windows-msvc", completed.stderr)
            self.assertEqual([arguments[0] for _target_dir, arguments in calls], ["metadata"])

    def test_a_new_member_in_neither_list_fails_classification(self):
        # Protect the check from silently skipping a crate added to the workspace.
        with tempfile.TemporaryDirectory() as directory:
            names = _workspace_package_names() + ["sonicterm-newcomer"]
            completed, calls = self.run_check(Path(directory), names=names)

            self.assertNotEqual(completed.returncode, 0)
            self.assertIn(
                "workspace member sonicterm-newcomer is in neither the verified nor the excluded list",
                completed.stderr,
            )
            self.assertIn("clippy", [arguments[0] for _target_dir, arguments in calls])

    def test_a_listed_name_that_is_no_longer_a_member_fails_classification(self):
        # Protect the lists from keeping a removed or renamed crate.
        with tempfile.TemporaryDirectory() as directory:
            names = [name for name in _workspace_package_names() if name != "sonicterm-harfbuzz"]
            completed, _calls_made = self.run_check(Path(directory), names=names)

            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("sonicterm-harfbuzz is listed but is not a workspace member", completed.stderr)

    def test_a_clippy_failure_fails_the_check_and_still_checks_winit(self):
        # Protect the winit phase from being skipped after a lint failure.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_check(Path(directory), fail="clippy")

            self.assertNotEqual(completed.returncode, 0)
            self.assertEqual(
                [arguments[0] for _target_dir, arguments in calls], ["metadata", "clippy", "check"]
            )

    def test_a_winit_check_failure_fails_the_check(self):
        # Protect the winit phase's verdict: a Windows test that fails to compile fails the script.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_check(Path(directory), fail="sonicterm-winit")

            self.assertEqual(completed.returncode, 1, completed.stderr)
            self.assertEqual(
                [arguments[0] for _target_dir, arguments in calls], ["metadata", "clippy", "check"]
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
