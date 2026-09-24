#!/usr/bin/env python3
"""Portable contracts for native package validation and controlled size evidence."""

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile
import sys
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("package_check", Path(__file__).with_name("test-macos-package.py"))
tool = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tool)


class FlushedStream(io.StringIO):
    flushed = ""

    def flush(self):
        self.flushed = self.getvalue()


class PackageTests(unittest.TestCase):
    def test_named_progress_is_flushed_and_keeps_captured_bytes_and_exit_status(self):
        # Phase diagnostics cannot contaminate the package evidence or hide a child failure/timeout.
        for code in (0, 16, 124, 91):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                stderr, stdout = FlushedStream(), io.StringIO()
                state = Path(directory)

                def execute(command, cwd, timeout, environment):
                    self.assertIn("[package-check] start closure timeout=60s", stderr.flushed)
                    self.assertEqual(timeout, 60)
                    return subprocess.CompletedProcess(command, code, b"payload", b"diagnostic")

                with contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(stdout), \
                        patch.object(tool.RUNNER, "run_command", side_effect=execute):
                    if code:
                        with self.assertRaisesRegex(RuntimeError, f"closure exited {code}"):
                            tool.run(["not-executed"], state, "closure")
                    else:
                        self.assertEqual(tool.run(["not-executed"], state, "closure"), b"payloaddiagnostic")
                self.assertIn(f"[package-check] finish closure exit={code}", stderr.flushed)
                self.assertEqual(stdout.getvalue(), "")
                self.assertEqual((state / "closure.log").read_bytes(), b"payloaddiagnostic")

    def test_probe_progress_reports_semantic_failure_despite_successful_launch(self):
        # A zero launcher exit is not a passing font/Cairo report.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            stderr = FlushedStream()

            def launch(command, *_args):
                self.assertIn("[package-check] start probe-launch timeout=25s", stderr.flushed)
                (state / "native-fonts-cairo.log").write_text("RESULT fonts=0/4 cairo=FAIL verdict=FAIL\n")
                return subprocess.CompletedProcess(command, 0, b"", b"")

            with contextlib.redirect_stderr(stderr), patch.object(tool.RUNNER, "run_command", side_effect=launch):
                with self.assertRaisesRegex(RuntimeError, "verdict"):
                    tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
            self.assertIn("[package-check] finish probe-launch result=FAIL", stderr.flushed)
            self.assertNotIn("result=PASS", stderr.getvalue())

    def test_winit_license_is_copied_after_native_bundle_and_before_app_seal(self):
        # Native closure assembly requires a fresh licenses directory; the app seal covers the static license too.
        script = Path(__file__).with_name("make-macos-dmg.sh").read_text(encoding="utf-8")
        commands = [line.strip() for line in script.splitlines() if not line.lstrip().startswith("#")]
        bundle_index = commands.index('python3 "$ROOT/scripts/macos-bundle.py" bundle "$APP" --max-minimum-macos "$MAX_MINIMUM"')
        directory_index = commands.index('mkdir -p "$APP/Contents/Resources/licenses"')
        copy_index = commands.index('cp "$ROOT/crates/sonicterm-winit/LICENSE" "$APP/Contents/Resources/licenses/LICENSE-winit-Apache-2.0"')
        self.assertLess(bundle_index, directory_index)
        self.assertLess(directory_index, copy_index)
        seal_index = commands.index('codesign --force --sign - "$APP"')
        self.assertLess(copy_index, seal_index)

    def test_font_probe_accepts_completed_report_when_open_wait_loses_process(self):
        # A fast successful app can exit before open registers its kevent process wait.
        diagnostic = b"Unable to block on applications (initial call to kevent() failed: No such process)\n"
        for code, error in [(0, b""), (1, diagnostic)]:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                state = Path(directory)
                report = state / "native-fonts-cairo.log"
                def launch(command, cwd, timeout, environment):
                    self.assertEqual(command[:5], ["/usr/bin/open", "-n", "-g", "-W", str(state / "Probe.app")])
                    self.assertEqual(timeout, 25)
                    self.assertNotIn("NO_COLOR", environment)
                    report.write_text("START pid=123\nRESULT fonts=4/4 cairo=PASS verdict=PASS\n")
                    return subprocess.CompletedProcess(command, code, b"", error)
                progress = FlushedStream()
                with contextlib.redirect_stderr(progress), patch.object(tool.RUNNER, "run_command", side_effect=launch):
                    tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
                self.assertIn("[package-check] finish probe-launch result=PASS", progress.flushed)
                self.assertNotIn("result=FAIL", progress.getvalue())
                self.assertEqual((state / "probe-launch.log").read_bytes(), error)

    def test_font_probe_rejects_other_launch_errors_even_with_pass_report(self):
        # Only the exact exit-before-wait diagnostic can be accepted; other launch errors report FAIL and still raise.
        diagnostic = b"Unable to block on applications (initial call to kevent() failed: No such process)\n"
        for code, output, error in [(1, b"", b"launch failed"), (124, b"", diagnostic),
                                    (91, b"", diagnostic), (1, b"unexpected", diagnostic),
                                    (1, b"", diagnostic + b"another error\n")]:
            with self.subTest(code=code, output=output, error=error), tempfile.TemporaryDirectory() as directory:
                state = Path(directory)
                def launch(command, *_args):
                    (state / "native-fonts-cairo.log").write_text("RESULT fonts=4/4 cairo=PASS verdict=PASS\n")
                    return subprocess.CompletedProcess(command, code, output, error)
                progress = FlushedStream()
                with contextlib.redirect_stderr(progress), patch.object(tool.RUNNER, "run_command", side_effect=launch):
                    with self.assertRaises(RuntimeError):
                        tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
                self.assertIn("[package-check] finish probe-launch result=FAIL", progress.flushed)
                self.assertNotIn("result=PASS", progress.getvalue())

    def test_font_probe_requires_one_complete_fresh_passing_verdict(self):
        # Missing, partial, conflicting, and stale reports must never turn a launch into success.
        passed = "RESULT fonts=4/4 cairo=PASS verdict=PASS\n"
        diagnostic = b"Unable to block on applications (initial call to kevent() failed: No such process)\n"
        for text in [None, "START pid=123\n", "RESULT fonts=3/4 cairo=PASS verdict=FAIL\n",
                     passed + passed, passed + "RESULT fonts=0/4 cairo=FAIL verdict=FAIL\n",
                     passed + "unfinished\n"]:
            for code in [0, 1]:
                with self.subTest(text=text, code=code), tempfile.TemporaryDirectory() as directory:
                    state = Path(directory)
                    def launch(command, *_args):
                        if text is not None:
                            (state / "native-fonts-cairo.log").write_text(text)
                        return subprocess.CompletedProcess(command, code, b"", diagnostic if code else b"")
                    with patch.object(tool.RUNNER, "run_command", side_effect=launch):
                        with self.assertRaisesRegex(RuntimeError, "report|verdict"):
                            tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            (state / "native-fonts-cairo.log").write_text(passed)
            with patch.object(tool.RUNNER, "run_command") as launch:
                with self.assertRaisesRegex(RuntimeError, "already exists"):
                    tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
                launch.assert_not_called()

    def test_shared_runner_executes_and_preserves_isolated_environment(self):
        # Exercise the imported process runner rather than letting package mocks hide API drift.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            environment = tool.RUNNER.smoke_environment(state, tool.clean_environment())
            output = tool.run([sys.executable, "-c", "print('package-runner-ok')"],
                              state, "runner", 10, environment)
            self.assertIn(b"package-runner-ok", output)
            self.assertEqual(environment["SONICTERM_RUNTIME_SMOKE_DIR"], str(state))

    def test_environment_preserves_home_but_removes_loader_and_color_overrides(self):
        # Isolation must not change the shell identity or inherit a way around library resolution.
        with patch.dict(os.environ, {"HOME": "/real/home", "NO_COLOR": "1", "DYLD_LIBRARY_PATH": "/host"}, clear=True):
            self.assertEqual(tool.clean_environment(), {"HOME": "/real/home"})

    def test_font_measurement_changes_only_registration_and_duplicate_payload(self):
        # Compare the same executable, closure, and compression policy at the same staged path.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            app = state / "original.app"
            fonts = app / "Contents/Resources/assets/fonts"
            fonts.mkdir(parents=True)
            for face in tool.FACES:
                (fonts / f"RecMonoSt.Helens-{face}.ttf").write_bytes(face.encode())
            executable = app / "Contents/MacOS/sonicterm-mac"
            executable.parent.mkdir()
            executable.write_bytes(b"same executable")
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"ATSApplicationFontsPath": "assets/fonts"}))
            calls = []
            def fake_run(command, _state, label, timeout=60, env=None):
                calls.append(command)
                if command[0] == "/usr/bin/hdiutil":
                    subject = Path(command[command.index("-srcfolder") + 1])
                    self.assertEqual((subject / "Contents/MacOS/sonicterm-mac").read_bytes(), b"same executable")
                    self.assertEqual(command[command.index("-format") + 1], "UDZO")
                    Path(command[-1]).write_bytes(b"x" * len(list(subject.rglob("*.ttf"))))
                return b""
            def fake_capture(command, state_dir, label, timeout=60, env=None, cleanup=False):
                # Image creation receives the result instead of an exception, so a busy refusal can be retried.
                return subprocess.CompletedProcess(command, 0, fake_run(command, state_dir, label, timeout, env), b"")
            with patch.object(tool, "run", side_effect=fake_run), \
                    patch.object(tool, "run_capture", side_effect=fake_capture):
                result = tool.measure_font_savings(app, state)
            self.assertEqual(result["font_dmg_saved_bytes"], 4)
            self.assertEqual(len(list(app.rglob("*.ttf"))), 4)
            self.assertFalse((state / "measurement").exists())
            images = [call for call in calls if call[0] == "/usr/bin/hdiutil"]
            self.assertEqual(images[0][images[0].index("-srcfolder") + 1], images[1][images[1].index("-srcfolder") + 1])

    def test_measurement_retries_a_busy_image_create_and_completes(self):
        # A transient `Resource busy` refusal is retried and logged, and the measurement still completes.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(1, b"hdiutil: create failed - Resource busy\n"), (0, b""), (0, b"")]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), \
                    patch.object(tool.RUNNER, "run_command", side_effect=scripted_hdiutil(created, outcomes)), \
                    patch.object(tool, "sleep") as sleep:
                result = tool.measure_font_savings(measurement_app(state), state)
            self.assertEqual(result["font_dmg_saved_bytes"], 4)
            self.assertEqual(len(created), 3)
            sleep.assert_called_once_with(tool.BUSY_RETRY_WAIT_SECONDS)
            self.assertIn("[package-check] retry single-measurement attempt=2/3 after: "
                          "hdiutil: create failed - Resource busy", stderr.getvalue())
            self.assertTrue((state / "single-measurement.log").is_file())
            self.assertTrue((state / "single-measurement-attempt2.log").is_file())

    def test_measurement_fails_after_the_last_busy_attempt_with_its_error(self):
        # The retry is bounded: three busy refusals fail with the final attempt's error.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(1, f"hdiutil: create failed - Resource busy (attempt {n})\n".encode()) for n in (1, 2, 3)]
            with contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(tool.RUNNER, "run_command", side_effect=scripted_hdiutil(created, outcomes)), \
                    patch.object(tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, r"single-measurement-attempt3 exited 1: .*\(attempt 3\)"):
                    tool.measure_font_savings(measurement_app(state), state)
            self.assertEqual(len(created), 3)
            self.assertEqual(sleep.call_count, 2)

    def test_measurement_does_not_retry_other_create_failures(self):
        # Only `Resource busy` is transient; any other create failure fails at once.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(1, b"hdiutil: create failed - No space left on device\n")]
            with contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(tool.RUNNER, "run_command", side_effect=scripted_hdiutil(created, outcomes)), \
                    patch.object(tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, "single-measurement exited 1: .*No space left"):
                    tool.measure_font_savings(measurement_app(state), state)
            self.assertEqual(len(created), 1)
            sleep.assert_not_called()

    def test_measurement_does_not_retry_a_timed_out_create(self):
        # A create the runner timed out is not retried, even when its output mentions `Resource busy`.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(tool.RUNNER.TIMEOUT_EXIT_CODE, b"Resource busy\nnative smoke timed out after 120 seconds\n")]
            with contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(tool.RUNNER, "run_command", side_effect=scripted_hdiutil(created, outcomes)), \
                    patch.object(tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, f"single-measurement exited {tool.RUNNER.TIMEOUT_EXIT_CODE}"):
                    tool.measure_font_savings(measurement_app(state), state)
            self.assertEqual(len(created), 1)
            sleep.assert_not_called()

    def test_commands_are_capped_by_the_validator_deadline(self):
        # A command gets at most the time left before the unmount reserve and does not start with under 1 s;
        # the unmount itself may use the reserve and always gets at least 1 s.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            timeouts = []

            def execute(command, _cwd, timeout, _environment):
                timeouts.append(timeout)
                return subprocess.CompletedProcess(command, 0, b"", b"")

            deadline = 1000.0
            limit = deadline - tool.CLEANUP_RESERVE_SECONDS
            with contextlib.redirect_stderr(io.StringIO()), patch.object(tool, "DEADLINE", deadline), \
                    patch.object(tool.RUNNER, "run_command", side_effect=execute):
                with patch.object(tool, "clock", return_value=limit - 50):
                    tool.run(["closure"], state, "closure")
                with patch.object(tool, "clock", return_value=limit - 0.5):
                    with self.assertRaisesRegex(RuntimeError, "signature: validator time budget exhausted"):
                        tool.run(["signature"], state, "signature")
                with patch.object(tool, "clock", return_value=deadline - 10):
                    tool.run(["unmount"], state, "unmount", cleanup=True)
                with patch.object(tool, "clock", return_value=deadline + 5):
                    tool.run(["unmount"], state, "unmount-late", cleanup=True)
            self.assertEqual(timeouts, [50, 10, 1])

    def test_a_busy_retry_is_skipped_when_too_little_budget_would_remain(self):
        # A retry that could not keep its minimum before the unmount reserve is not started; the busy error is raised.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(1, b"hdiutil: create failed - Resource busy\n")]
            deadline = 1000.0
            now = (deadline - tool.CLEANUP_RESERVE_SECONDS - tool.BUSY_RETRY_WAIT_SECONDS
                   - tool.BUSY_RETRY_MINIMUM_SECONDS + 1)
            with contextlib.redirect_stderr(io.StringIO()), patch.object(tool, "DEADLINE", deadline), \
                    patch.object(tool, "clock", return_value=now), \
                    patch.object(tool.RUNNER, "run_command", side_effect=scripted_hdiutil(created, outcomes)), \
                    patch.object(tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, "single-measurement exited 1: .*Resource busy"):
                    tool.create_measurement_image(state / "measured.app", state / "single-fonts.dmg", state,
                                                  "single-measurement")
            self.assertEqual(len(created), 1)
            self.assertEqual(created[0][1], min(120, int(deadline - tool.CLEANUP_RESERVE_SECONDS - now)))
            sleep.assert_not_called()

    def test_main_starts_the_deadline_before_mount_and_still_unmounts(self):
        # The budget counts from entry: a slow mount leaves later commands refused, but the unmount still runs.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / "state"
            now = [1000.0]
            calls = []

            def execute(command, _cwd, timeout, _environment):
                calls.append((command[:2], timeout))
                if command[:2] == ["/usr/bin/hdiutil", "attach"]:
                    self.assertEqual(tool.DEADLINE, 1000.0 + tool.STEP_BUDGET_SECONDS)
                    app = Path(command[command.index("-mountpoint") + 1]) / "SonicTerm.app"
                    (app / "Contents").mkdir(parents=True)
                    now[0] += 400
                return subprocess.CompletedProcess(command, 0, b"", b"")

            argv = ["test-macos-package.py", "--dmg", str(root / "image.dmg"), "--state-dir", str(state)]
            with contextlib.redirect_stderr(io.StringIO()), patch.object(sys, "argv", argv), \
                    patch.object(tool, "DEADLINE", None), patch.object(tool, "clock", side_effect=lambda: now[0]), \
                    patch.object(tool.RUNNER, "run_command", side_effect=execute), \
                    patch.object(type(state), "is_mount", lambda path: path.name == "mounted"):
                with self.assertRaisesRegex(RuntimeError, "closure: validator time budget exhausted"):
                    tool.main()
            self.assertEqual(calls, [(["/usr/bin/hdiutil", "attach"], 60),
                                     (["/usr/bin/hdiutil", "detach"],
                                      int(1000.0 + tool.STEP_BUDGET_SECONDS - now[0]))])

    def test_every_validator_step_timeout_covers_the_validator_budget(self):
        # A workflow step shorter than the validator's budget would kill it before its own deadline reports.
        margin = 60  # Interpreter start, the runner's post-kill waits, and file work outside commands.
        for workflow in ("ci.yml", "release.yml"):
            text = (tool.ROOT / ".github/workflows" / workflow).read_text(encoding="utf-8")
            steps = [step for step in re.split(r"(?m)^      - ", text) if "scripts/test-macos-package.py" in step]
            self.assertTrue(steps, workflow)
            for step in steps:
                name = step.splitlines()[0]
                minutes = re.search(r"(?m)^        timeout-minutes: (\d+)$", step)
                self.assertIsNotNone(minutes, name)
                self.assertGreaterEqual(int(minutes.group(1)) * 60, tool.STEP_BUDGET_SECONDS + margin, name)


def measurement_app(state: Path) -> Path:
    """Build the smallest app bundle the font measurement can stage, sign, and image."""
    app = state / "original.app"
    fonts = app / "Contents/Resources/assets/fonts"
    fonts.mkdir(parents=True)
    for face in tool.FACES:
        (fonts / f"RecMonoSt.Helens-{face}.ttf").write_bytes(face.encode())
    executable = app / "Contents/MacOS/sonicterm-mac"
    executable.parent.mkdir()
    executable.write_bytes(b"same executable")
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"ATSApplicationFontsPath": "assets/fonts"}))
    return app


def scripted_hdiutil(created: list, outcomes: list):
    """Return a fake runner that answers each `hdiutil create` with the next scripted outcome."""
    def execute(command, _cwd, timeout, _environment):
        if command[:2] != ["/usr/bin/hdiutil", "create"]:
            return subprocess.CompletedProcess(command, 0, b"", b"")
        created.append((command, timeout))
        code, stderr = outcomes.pop(0)
        if code == 0:
            # A successful create writes an image whose size reflects the staged fonts.
            subject = Path(command[command.index("-srcfolder") + 1])
            Path(command[-1]).write_bytes(b"x" * len(list(subject.rglob("*.ttf"))))
        return subprocess.CompletedProcess(command, code, b"", stderr)
    return execute


if __name__ == "__main__":
    unittest.main(verbosity=2)
