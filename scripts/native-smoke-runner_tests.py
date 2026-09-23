#!/usr/bin/env python3
"""Regression tests for the bounded native runtime-smoke wrapper."""

from __future__ import annotations

import ast
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "native_smoke_runner", _HERE / "native-smoke-runner.py"
)
assert _SPEC is not None and _SPEC.loader is not None
runner = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(runner)


class VerdictTests(unittest.TestCase):
    def test_required_capability_accepts_only_one_exercised_verdict(self):
        # Protect the required GDI gate from treating an informational host skip as success.
        exercised = b"capability=EXERCISED presenter=windows-software\n"
        self.assertTrue(runner.has_required_capability(exercised, "EXERCISED"))
        for output in (
            b"",
            b"capability=HOST_INCAPABLE reason=GetDC\n",
            b"capability=INCORRECT_OUTPUT reason=color\n",
            exercised + b"capability=HOST_INCAPABLE reason=later\n",
        ):
            self.assertFalse(runner.has_required_capability(output, "EXERCISED"))

    def test_wrapper_failure_codes_are_stable(self):
        # Protect CI diagnostics from changing timeout, verdict, and launch classifications.
        self.assertEqual(runner.TIMEOUT_EXIT_CODE, 124)
        self.assertEqual(runner.VERDICT_EXIT_CODE, 90)
        self.assertEqual(runner.LAUNCH_EXIT_CODE, 91)

    def test_missing_required_verdict_is_persisted_with_exit_90(self):
        # Preserve captured child output and the verdict diagnostic, without progress in the artifact.
        with tempfile.TemporaryDirectory() as directory:
            log_file = Path(directory) / "gdi.log"
            code = runner.main(
                [
                    "--timeout-seconds",
                    "10",
                    "--log-file",
                    str(log_file),
                    "--require-capability",
                    "EXERCISED",
                    "--",
                    sys.executable,
                    "-c",
                    "print('capability=HOST_INCAPABLE reason=fixture')",
                ]
            )
            self.assertEqual(code, runner.VERDICT_EXIT_CODE)
            child_output = b"capability=HOST_INCAPABLE reason=fixture" + os.linesep.encode()
            log = log_file.read_bytes()
            self.assertEqual(log, child_output + b"required capability verdict missing or not uniquely EXERCISED\n")
            self.assertNotIn(b"[native-smoke]", log)


class ExecutorTests(unittest.TestCase):
    def test_state_directory_does_not_replace_home(self):
        # Protect PTY shell behavior by keeping HOME/USERPROFILE while adding one smoke root.
        original = dict(os.environ)
        original["NO_COLOR"] = "1"
        with tempfile.TemporaryDirectory() as directory:
            environment = runner.smoke_environment(Path(directory), original)
        self.assertNotIn("NO_COLOR", environment)
        self.assertEqual(environment.get("HOME"), original.get("HOME"))
        self.assertEqual(environment.get("USERPROFILE"), original.get("USERPROFILE"))
        self.assertEqual(environment["SONICTERM_RUNTIME_SMOKE_DIR"], directory)

    def test_nonzero_exit_and_partial_output_are_preserved(self):
        # Protect stage-specific binary exit codes and diagnostics from wrapper translation.
        command = (
            sys.executable,
            "-c",
            "import sys; print('stdout-proof'); print('stderr-proof', file=sys.stderr); sys.exit(16)",
        )
        completed = runner.run_command(command, _HERE.parent, 10, dict(os.environ))
        self.assertEqual(completed.returncode, 16)
        self.assertIn(b"stdout-proof", completed.stdout)
        self.assertIn(b"stderr-proof", completed.stderr)

    def test_timeout_kills_descendants_and_preserves_partial_output(self):
        # Protect CI hosts from leaked PTY descendants when a native event loop wedges.
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "descendant-survived"
            child = (
                "import pathlib,time; time.sleep(3); "
                "pathlib.Path({!r}).write_text('alive')"
            ).format(str(marker))
            parent = (
                "import subprocess,sys,time; "
                "subprocess.Popen([sys.executable, '-c', {!r}]); "
                "print('partial', flush=True); time.sleep(60)"
            ).format(child)
            started = time.monotonic()
            completed = runner.run_command(
                (sys.executable, "-c", parent), _HERE.parent, 1, dict(os.environ)
            )
            elapsed = time.monotonic() - started
            time.sleep(3)

            self.assertLess(elapsed, 10)
            self.assertEqual(completed.returncode, runner.TIMEOUT_EXIT_CODE)
            self.assertIn(b"partial", completed.stdout)
            self.assertIn(b"timed out after 1 seconds", completed.stderr)
            self.assertFalse(marker.exists(), "timed-out smoke left its descendant running")


class ProgressTests(unittest.TestCase):
    def test_python_test_entry_points_default_to_verbose_unittest_reporting(self):
        # Keep the shared Python progress policy beside the smoke runner's redirected-stream contract.
        for path in sorted(_HERE.glob("*_tests.py")):
            with self.subTest(script=path.name):
                tree = ast.parse(path.read_text(encoding="utf-8"))
                mains = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
                         and isinstance(node.func, ast.Attribute) and node.func.attr == "main"
                         and isinstance(node.func.value, ast.Name)
                         and node.func.value.id == "unittest"]
                self.assertEqual(len(mains), 1)
                self.assertTrue(any(keyword.arg == "verbosity"
                                    and isinstance(keyword.value, ast.Constant)
                                    and keyword.value.value == 2 for keyword in mains[0].keywords))

    def test_start_is_explicitly_flushed_before_child_invocation(self):
        # TextIOWrapper supplies .buffer for child bytes; snapshots distinguish flush from line buffering.
        class FlushedStream(io.TextIOWrapper):
            flushed = b""

            def flush(self):
                super().flush()
                self.flushed = self.buffer.getvalue()

        with FlushedStream(io.BytesIO(), encoding="utf-8", newline="\n") as stderr, \
                io.TextIOWrapper(io.BytesIO(), encoding="utf-8") as stdout:
            def execute(command, cwd, timeout, environment):
                self.assertEqual(stderr.flushed, b"[native-smoke] start timeout=10s\n")
                return subprocess.CompletedProcess(command, 0, b"payload", b"diagnostic")

            with contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(stdout), \
                    patch.object(runner, "run_command", side_effect=execute):
                self.assertEqual(runner.main(["--timeout-seconds", "10", "--", "not-executed"]), 0)
            self.assertIn(b"[native-smoke] finish exit=0", stderr.flushed)

    def test_cli_merged_output_orders_start_child_and_finish(self):
        # A real merged pipe exposes buffered child stdout that otherwise appears after finish.
        result = subprocess.run(
            [sys.executable, str(_HERE / "native-smoke-runner.py"),
             "--timeout-seconds", "10", "--", sys.executable, "-c", "print('child-payload')"],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=30)
        self.assertEqual(result.returncode, 0)
        self.assertLess(result.stdout.index(b"[native-smoke] start"), result.stdout.index(b"child-payload"))
        self.assertLess(result.stdout.index(b"child-payload"), result.stdout.index(b"[native-smoke] finish exit=0"))

    def test_cli_reports_start_before_child_finishes_without_changing_stdout(self):
        # The child waits on stdin, so receiving progress before release proves pipe flushing.
        child = "import sys; sys.stdin.readline(); print('{\"ok\":true}')"
        command = [sys.executable, str(_HERE / "native-smoke-runner.py"),
                   "--timeout-seconds", "10", "--", sys.executable, "-c", child]
        with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE) as process:
            lines = queue.Queue()
            reader = threading.Thread(target=lambda: lines.put(process.stderr.readline()), daemon=True)
            reader.start()
            try:
                start = lines.get(timeout=5)
                self.assertEqual(start.rstrip(b"\r\n"), b"[native-smoke] start timeout=10s")
                self.assertIsNone(process.poll())
            finally:
                process.stdin.write(b"continue\n")
                process.stdin.flush()
                reader.join(timeout=15)
                stdout, stderr = process.communicate(timeout=15)
            self.assertEqual(process.returncode, 0)
            self.assertEqual(stdout, b'{"ok":true}\r\n' if os.name == "nt" else b'{"ok":true}\n')
            self.assertIn(b"[native-smoke] finish exit=0", stderr)
            self.assertNotIn(child.encode(), start + stderr)

    def test_unittest_identity_reaches_redirected_stderr_before_test_body(self):
        # Pause via CPython's private TestCase._callTestMethod to observe default reporting before the real test body.
        script = (
            "import runpy,sys,unittest; "
            "original=unittest.TestCase._callTestMethod; "
            "unittest.TestCase._callTestMethod=lambda self,method: "
            "(sys.stdin.readline(),original(self,method))[-1]; "
            f"sys.argv=[{str(_HERE / 'native-smoke-runner_tests.py')!r}, "
            "'VerdictTests.test_wrapper_failure_codes_are_stable']; "
            "runpy.run_path(sys.argv[0],run_name='__main__')"
        )
        with subprocess.Popen([sys.executable, "-c", script], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE) as process:
            prefix = b"test_wrapper_failure_codes_are_stable"
            chunks = queue.Queue()
            reader = threading.Thread(target=lambda: chunks.put(process.stderr.read(len(prefix))), daemon=True)
            reader.start()
            try:
                self.assertEqual(chunks.get(timeout=5), prefix)
                self.assertIsNone(process.poll())
            finally:
                process.stdin.write(b"continue\n")
                process.stdin.flush()
                reader.join(timeout=15)
                stdout, stderr = process.communicate(timeout=15)
            self.assertEqual(process.returncode, 0)
            self.assertEqual(stdout, b"")
            self.assertIn(b"ok", stderr)

    def test_cli_reports_final_failure_timeout_launch_and_verdict_codes(self):
        # Progress must reflect the wrapper's final status, not just a child's successful exit.
        cases = [
            ([sys.executable, "-c", "import sys; print('partial', flush=True); sys.exit(16)"], [], 16),
            ([sys.executable, "-c", "import time; print('partial', flush=True); time.sleep(60)"], [], 124),
            ([str(_HERE / "missing-progress-fixture")], [], 91),
            ([sys.executable, "-c", "print('partial')"], ["--require-capability", "EXERCISED"], 90),
        ]
        for child, options, code in cases:
            with self.subTest(code=code):
                timeout = 1 if code == 124 else 10
                result = subprocess.run(
                    [sys.executable, str(_HERE / "native-smoke-runner.py"),
                     "--timeout-seconds", str(timeout), *options, "--", *child],
                    capture_output=True, timeout=30)
                self.assertEqual(result.returncode, code)
                self.assertIn(f"[native-smoke] start timeout={timeout}s".encode(), result.stderr)
                self.assertIn(f"[native-smoke] finish exit={code}".encode(), result.stderr)
                self.assertNotIn(b"finish exit=0", result.stderr)
                if code != 91:
                    self.assertEqual(result.stdout.strip(), b"partial")


class WorkflowShapeTests(unittest.TestCase):
    def test_ci_requires_native_binaries_and_exercised_gdi(self):
        # Protect reviewed heads from passing on unit-only or HOST_INCAPABLE evidence.
        workflow = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("  macos-smoke:\n", workflow)
        self.assertIn("  windows-smoke:\n", workflow)
        self.assertIn(
            "needs: [macos-core, macos-features, macos-coverage, macos-smoke]",
            workflow,
        )
        self.assertIn(
            "needs: [windows-native, windows-checks, windows-features, windows-tests, windows-smoke]",
            workflow,
        )
        self.assertIn("Require macOS native runtime smoke", workflow)
        self.assertIn("Require Windows native runtime smoke", workflow)
        self.assertEqual(workflow.count("Require Windows GDI capability=EXERCISED"), 1)
        self.assertEqual(workflow.count("windows_software_present_capability"), 1)
        self.assertIn("--require-capability EXERCISED", workflow)
        windows_tests = workflow.split("  windows-tests:\n", 1)[1].split("  windows-smoke:\n", 1)[0]
        windows_smoke = workflow.split("  windows-smoke:\n", 1)[1].split("  windows:\n", 1)[0]
        self.assertIn("Require Windows GDI capability=EXERCISED", windows_tests)
        self.assertNotIn("windows_software_present_capability", windows_smoke)
        self.assertIn("--timeout-seconds 45", workflow)
        self.assertIn("--state-dir", workflow)
        self.assertIn("--log-file", workflow)
        self.assertIn("target/release/sonicterm-mac", workflow)
        self.assertIn("target/release/sonicterm-windows.exe", workflow)
        self.assertGreaterEqual(workflow.count("save-if: false"), 4)
        self.assertIn("Upload macOS native smoke logs", workflow)
        self.assertIn("Upload Windows native smoke logs", workflow)

    def test_linux_package_smoke_uses_the_same_bounded_tree_reaper(self):
        # Protect packaged X11/Wayland runs from bypassing color cleanup and descendant teardown.
        script = (_HERE / "smoke-linux-packages.sh").read_text(encoding="utf-8")
        self.assertIn("command in dpkg dpkg-query python3", script)
        self.assertIn('ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"', script)
        self.assertIn('python3 "$ROOT/scripts/native-smoke-runner.py"', script)
        self.assertIn("--timeout-seconds 45", script)
        self.assertIn("--state-dir \"$state_dir\"", script)
        self.assertIn("--log-file \"$log\"", script)
        self.assertNotIn("timeout --signal=TERM", script)

    def test_release_runs_built_macos_and_windows_binaries(self):
        # Protect release packages from advancing without their native build runtime proof.
        workflow = (_HERE.parent / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        self.assertGreaterEqual(workflow.count("Require macOS native runtime smoke"), 2)
        self.assertIn("Require Windows native runtime smoke", workflow)
        self.assertNotIn("Require Windows GDI capability=EXERCISED", workflow)
        self.assertIn("Verify exact successful main CI", workflow)
        self.assertIn("target/x86_64-pc-windows-msvc/release/sonicterm-windows.exe", workflow)
        self.assertGreaterEqual(workflow.count("Upload macOS native smoke logs"), 2)
        self.assertIn("Upload Windows native smoke logs", workflow)


if __name__ == "__main__":
    unittest.main(verbosity=2)
