#!/usr/bin/env python3
"""Regression tests for the bounded native runtime-smoke wrapper."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest

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
        # Protect failure artifacts from omitting the wrapper reason after captured child output.
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
            log = log_file.read_text(encoding="utf-8")
            self.assertIn("capability=HOST_INCAPABLE", log)
            self.assertIn("required capability verdict missing", log)


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
    unittest.main()
