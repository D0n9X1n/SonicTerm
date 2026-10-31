"""Macos package validation tests."""

import contextlib
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

import macos_package_test_fixture


class PackageTests(unittest.TestCase):
    def test_named_progress_is_flushed_and_keeps_captured_bytes_and_exit_status(self):
        # Phase diagnostics cannot contaminate the package evidence or hide a child failure/timeout.
        for code in (0, 16, 124, 91):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                stderr, stdout = macos_package_test_fixture.FlushedStream(), io.StringIO()
                state = Path(directory)

                def execute(command, cwd, timeout, environment):
                    self.assertIn("[package-check] start closure timeout=60s", stderr.flushed)
                    self.assertEqual(timeout, 60)
                    return subprocess.CompletedProcess(command, code, b"payload", b"diagnostic")

                with contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(stdout), \
                        patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=execute):
                    if code:
                        with self.assertRaisesRegex(RuntimeError, f"closure exited {code}"):
                            macos_package_test_fixture.tool.run(["not-executed"], state, "closure")
                    else:
                        self.assertEqual(macos_package_test_fixture.tool.run(["not-executed"], state, "closure"), b"payloaddiagnostic")
                self.assertIn(f"[package-check] finish closure exit={code}", stderr.flushed)
                self.assertEqual(stdout.getvalue(), "")
                self.assertEqual((state / "closure.log").read_bytes(), b"payloaddiagnostic")

    def test_probe_progress_reports_semantic_failure_despite_successful_launch(self):
        # A zero launcher exit is not a passing font/Cairo report.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            stderr = macos_package_test_fixture.FlushedStream()

            def launch(command, *_args):
                self.assertIn("[package-check] start probe-launch timeout=25s", stderr.flushed)
                (state / "native-fonts-cairo.log").write_text("RESULT fonts=0/1 cairo=FAIL verdict=FAIL\n")
                return subprocess.CompletedProcess(command, 0, b"", b"")

            with contextlib.redirect_stderr(stderr), patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=launch):
                with self.assertRaisesRegex(RuntimeError, "verdict"):
                    macos_package_test_fixture.tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
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

    def test_packaging_declares_and_asserts_the_macos_15_floor(self):
        # The app's declared floor and the default ceiling both match the 15.0 deployment target.
        script = Path(__file__).with_name("make-macos-dmg.sh").read_text(encoding="utf-8")
        self.assertIn('MAX_MINIMUM="${SONICTERM_MAX_MACOS_MINIMUM:-15.0}"', script)
        self.assertIn("<key>LSMinimumSystemVersion</key>   <string>15.0</string>", script)
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(macos_package_test_fixture.tool, "validate") as validate, \
                patch.object(sys, "argv", ["test-macos-package.py", "--app", directory,
                                           "--state-dir", str(Path(directory) / "state")]):
            macos_package_test_fixture.tool.main()
        self.assertEqual(validate.call_args.args[3], "15.0")

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
                    report.write_text("START pid=123\nRESULT fonts=1/1 cairo=PASS verdict=PASS\n")
                    return subprocess.CompletedProcess(command, code, b"", error)
                progress = macos_package_test_fixture.FlushedStream()
                with contextlib.redirect_stderr(progress), patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=launch):
                    macos_package_test_fixture.tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
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
                    (state / "native-fonts-cairo.log").write_text("RESULT fonts=1/1 cairo=PASS verdict=PASS\n")
                    return subprocess.CompletedProcess(command, code, output, error)
                progress = macos_package_test_fixture.FlushedStream()
                with contextlib.redirect_stderr(progress), patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=launch):
                    with self.assertRaises(RuntimeError):
                        macos_package_test_fixture.tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
                self.assertIn("[package-check] finish probe-launch result=FAIL", progress.flushed)
                self.assertNotIn("result=PASS", progress.getvalue())

    def test_font_probe_requires_one_complete_fresh_passing_verdict(self):
        # Missing, partial, conflicting, and stale reports must never turn a launch into success.
        passed = "RESULT fonts=1/1 cairo=PASS verdict=PASS\n"
        diagnostic = b"Unable to block on applications (initial call to kevent() failed: No such process)\n"
        for text in [None, "START pid=123\n", "RESULT fonts=0/1 cairo=PASS verdict=FAIL\n",
                     passed + passed, passed + "RESULT fonts=0/1 cairo=FAIL verdict=FAIL\n",
                     passed + "unfinished\n"]:
            for code in [0, 1]:
                with self.subTest(text=text, code=code), tempfile.TemporaryDirectory() as directory:
                    state = Path(directory)
                    def launch(command, *_args):
                        if text is not None:
                            (state / "native-fonts-cairo.log").write_text(text)
                        return subprocess.CompletedProcess(command, code, b"", diagnostic if code else b"")
                    with patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=launch):
                        with self.assertRaisesRegex(RuntimeError, "report|verdict"):
                            macos_package_test_fixture.tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            (state / "native-fonts-cairo.log").write_text(passed)
            with patch.object(macos_package_test_fixture.tool.RUNNER, "run_command") as launch:
                with self.assertRaisesRegex(RuntimeError, "already exists"):
                    macos_package_test_fixture.tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
                launch.assert_not_called()

    def test_shared_runner_executes_and_preserves_isolated_environment(self):
        # Exercise the imported process runner rather than letting package mocks hide API drift.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            environment = macos_package_test_fixture.tool.RUNNER.smoke_environment(state, macos_package_test_fixture.tool.clean_environment())
            output = macos_package_test_fixture.tool.run([sys.executable, "-c", "print('package-runner-ok')"],
                              state, "runner", 10, environment)
            self.assertIn(b"package-runner-ok", output)
            self.assertEqual(environment["SONICTERM_RUNTIME_SMOKE_DIR"], str(state))

    def test_environment_preserves_home_but_removes_loader_and_color_overrides(self):
        # Isolation must not change the shell identity or inherit a way around library resolution.
        with patch.dict(os.environ, {"HOME": "/real/home", "NO_COLOR": "1", "DYLD_LIBRARY_PATH": "/host"}, clear=True):
            self.assertEqual(macos_package_test_fixture.tool.clean_environment(), {"HOME": "/real/home"})

    def test_font_measurement_changes_only_registration_and_duplicate_payload(self):
        # Compare the same executable, closure, and compression policy at the same staged path.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            app = state / "original.app"
            fonts = app / "Contents/Resources/assets/fonts"
            fonts.mkdir(parents=True)
            for name in macos_package_test_fixture.tool.FONTS:
                (fonts / name).write_bytes(name.encode())
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
            with patch.object(macos_package_test_fixture.tool, "run", side_effect=fake_run), \
                    patch.object(macos_package_test_fixture.tool, "run_capture", side_effect=fake_capture):
                result = macos_package_test_fixture.tool.measure_font_savings(app, state)
            self.assertEqual(result["font_dmg_saved_bytes"], 1)
            self.assertEqual(len(list(app.rglob("*.ttf"))), 1)
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
                    patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=macos_package_test_fixture.scripted_hdiutil(created, outcomes)), \
                    patch.object(macos_package_test_fixture.tool, "sleep") as sleep:
                result = macos_package_test_fixture.tool.measure_font_savings(macos_package_test_fixture.measurement_app(state), state)
            self.assertEqual(result["font_dmg_saved_bytes"], 1)
            self.assertEqual(len(created), 3)
            sleep.assert_called_once_with(macos_package_test_fixture.tool.BUSY_RETRY_WAIT_SECONDS)
            self.assertIn("[package-check] retry single-measurement attempt=2/3 after: "
                          "hdiutil: create failed - Resource busy", stderr.getvalue())
            self.assertTrue((state / "single-measurement.log").is_file())
            self.assertTrue((state / "single-measurement-attempt2.log").is_file())

    def test_measurement_fails_after_the_last_busy_attempt_with_its_error(self):
        # The retry is bounded: three busy refusals fail with the final attempt's error.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(1, f"hdiutil: create failed - Resource busy (attempt {attempt})\n".encode()) for attempt in (1, 2, 3)]
            with contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=macos_package_test_fixture.scripted_hdiutil(created, outcomes)), \
                    patch.object(macos_package_test_fixture.tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, r"single-measurement-attempt3 exited 1: .*\(attempt 3\)"):
                    macos_package_test_fixture.tool.measure_font_savings(macos_package_test_fixture.measurement_app(state), state)
            self.assertEqual(len(created), 3)
            self.assertEqual(sleep.call_count, 2)

    def test_measurement_does_not_retry_other_create_failures(self):
        # Only `Resource busy` is transient; any other create failure fails at once.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(1, b"hdiutil: create failed - No space left on device\n")]
            with contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=macos_package_test_fixture.scripted_hdiutil(created, outcomes)), \
                    patch.object(macos_package_test_fixture.tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, "single-measurement exited 1: .*No space left"):
                    macos_package_test_fixture.tool.measure_font_savings(macos_package_test_fixture.measurement_app(state), state)
            self.assertEqual(len(created), 1)
            sleep.assert_not_called()

    def test_measurement_does_not_retry_a_timed_out_create(self):
        # A create the runner timed out is not retried, even when its output mentions `Resource busy`.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(macos_package_test_fixture.tool.RUNNER.TIMEOUT_EXIT_CODE, b"Resource busy\nnative smoke timed out after 120 seconds\n")]
            with contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=macos_package_test_fixture.scripted_hdiutil(created, outcomes)), \
                    patch.object(macos_package_test_fixture.tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, f"single-measurement exited {macos_package_test_fixture.tool.RUNNER.TIMEOUT_EXIT_CODE}"):
                    macos_package_test_fixture.tool.measure_font_savings(macos_package_test_fixture.measurement_app(state), state)
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
            limit = deadline - macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS - macos_package_test_fixture.tool.VALIDATION_SUPERVISION_SECONDS
            with contextlib.redirect_stderr(io.StringIO()), patch.object(macos_package_test_fixture.tool, "DEADLINE", deadline), \
                    patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=execute):
                with patch.object(macos_package_test_fixture.tool, "clock", return_value=limit - 50):
                    macos_package_test_fixture.tool.run(["closure"], state, "closure")
                with patch.object(macos_package_test_fixture.tool, "clock", return_value=limit - 0.5):
                    with self.assertRaisesRegex(RuntimeError, "signature: validator time budget exhausted"):
                        macos_package_test_fixture.tool.run(["signature"], state, "signature")
                with patch.object(macos_package_test_fixture.tool, "clock", return_value=deadline - 10):
                    macos_package_test_fixture.tool.run(["unmount"], state, "unmount", cleanup=True)
                with patch.object(macos_package_test_fixture.tool, "clock", return_value=deadline + 5):
                    macos_package_test_fixture.tool.run(["unmount"], state, "unmount-late", cleanup=True)
            self.assertEqual(timeouts, [50, 10, 1])

    def test_a_busy_retry_is_skipped_when_too_little_budget_would_remain(self):
        # A retry that could not keep its minimum before the unmount reserve is not started; the busy error is raised.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            created = []
            outcomes = [(1, b"hdiutil: create failed - Resource busy\n")]
            deadline = 1000.0
            now = (deadline - macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS - macos_package_test_fixture.tool.VALIDATION_SUPERVISION_SECONDS
                   - macos_package_test_fixture.tool.BUSY_RETRY_WAIT_SECONDS - macos_package_test_fixture.tool.BUSY_RETRY_MINIMUM_SECONDS + 1)
            with contextlib.redirect_stderr(io.StringIO()), patch.object(macos_package_test_fixture.tool, "DEADLINE", deadline), \
                    patch.object(macos_package_test_fixture.tool, "clock", return_value=now), \
                    patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=macos_package_test_fixture.scripted_hdiutil(created, outcomes)), \
                    patch.object(macos_package_test_fixture.tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, "single-measurement exited 1: .*Resource busy"):
                    macos_package_test_fixture.tool.create_measurement_image(state / "measured.app", state / "single-fonts.dmg", state,
                                                  "single-measurement")
            self.assertEqual(len(created), 1)
            self.assertEqual(created[0][1], min(120, int(deadline - macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS
                                                       - macos_package_test_fixture.tool.VALIDATION_SUPERVISION_SECONDS - now)))
            sleep.assert_not_called()

    def test_main_starts_the_deadline_before_mount_and_still_unmounts(self):
        # Validation consumes the work budget, but owned cleanup retains its independent reserve.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            def validate(app, state, dmg, minimum):
                self.assertEqual(macos_package_test_fixture.tool.DEADLINE, fake.started + macos_package_test_fixture.tool.STEP_BUDGET_SECONDS)
                self.assertEqual((app / "Contents/fixture").read_bytes(), b"installed fixture")
                fake.now = macos_package_test_fixture.tool.DEADLINE - macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS + 0.5
                macos_package_test_fixture.tool.run(["never-executed"], state, "closure")
            argv = ["test-macos-package.py", "--dmg", str(fake.image), "--state-dir", str(fake.state)]
            with fake.active(), patch.object(sys, "argv", argv), \
                    patch.object(macos_package_test_fixture.tool, "validate", side_effect=validate), \
                    patch.object(macos_package_test_fixture.tool.RUNNER, "run_command") as native:
                with self.assertRaisesRegex(RuntimeError, "closure: validator time budget exhausted"):
                    macos_package_test_fixture.tool.main()
                native.assert_not_called()
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach", "info"])
            self.assertEqual([step.timeout_s for step in fake.steps], [3, 60, 3, 3, 20, 3])
            self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")

    def test_timed_out_validation_preserves_the_cleanup_reserve(self):
        # The native runner's post-kill wait must not consume time reserved for owned attachment cleanup.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            timeouts = []
            remaining_after_timeout = []
            def execute(command, _cwd, timeout, _environment):
                timeouts.append(timeout)
                fake.now += timeout + 10
                remaining_after_timeout.append(macos_package_test_fixture.tool.DEADLINE - fake.now)
                return subprocess.CompletedProcess(command, macos_package_test_fixture.tool.RUNNER.TIMEOUT_EXIT_CODE,
                                                   b"", b"native smoke timed out\n")
            def validate(*_args):
                fake.now = macos_package_test_fixture.tool.DEADLINE - macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS - 50
                macos_package_test_fixture.tool.run(["never-executed"], fake.state, "closure")
            argv = ["test-macos-package.py", "--dmg", str(fake.image), "--state-dir", str(fake.state)]
            with fake.active(), patch.object(sys, "argv", argv), \
                    patch.object(macos_package_test_fixture.tool, "validate", side_effect=validate), \
                    patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=execute):
                with self.assertRaisesRegex(RuntimeError, "closure exited 124"):
                    macos_package_test_fixture.tool.main()
            self.assertEqual(timeouts, [40])
            self.assertEqual(remaining_after_timeout, [macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS])
            self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")
            self.assertFalse(fake.mounted)

    def test_cleanup_failure_preserves_the_original_validation_error(self):
        # A failed detach is retained as secondary evidence, never substituted for validation's error.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.detach_result = ("FAIL", 1, 0)
            original = RuntimeError("original validation failure")
            argv = ["test-macos-package.py", "--dmg", str(fake.image), "--state-dir", str(fake.state)]
            with fake.active(), patch.object(sys, "argv", argv), \
                    patch.object(macos_package_test_fixture.tool, "validate", side_effect=original):
                with self.assertRaisesRegex(RuntimeError, "original validation failure") as caught:
                    macos_package_test_fixture.tool.main()
            self.assertIs(caught.exception, original)
            self.assertEqual(fake.report()["original_error"], str(original))
            self.assertTrue(fake.report()["cleanup_errors"])
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_validator_workflow_steps_have_no_timeout_override(self):
        # Native command budgets remain in the validator, not workflow job or step overrides.
        for workflow in ("ci.yml", "release.yml"):
            text = (macos_package_test_fixture.tool.ROOT / ".github/workflows" / workflow).read_text(encoding="utf-8")
            steps = [step for step in re.split(r"(?m)^      - ", text) if "scripts/test-macos-package.py" in step]
            self.assertTrue(steps, workflow)
            for step in steps:
                self.assertNotIn("timeout-minutes:", step)

    def test_finish_lines_report_the_capped_timeout(self):
        # A completion record names the timeout the command actually ran with, as its start line does.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            stderr = io.StringIO()

            def execute(command, _cwd, _timeout, _environment):
                return subprocess.CompletedProcess(command, 0, b"", b"")

            def launch(command, _cwd, _timeout, _environment):
                (state / "native-fonts-cairo.log").write_text("RESULT fonts=1/1 cairo=PASS verdict=PASS\n")
                return subprocess.CompletedProcess(command, 0, b"", b"")

            limit = 1000.0 - macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS - macos_package_test_fixture.tool.VALIDATION_SUPERVISION_SECONDS
            with contextlib.redirect_stderr(stderr), patch.object(macos_package_test_fixture.tool, "DEADLINE", 1000.0), \
                    patch.object(macos_package_test_fixture.tool, "clock", return_value=limit - 10):
                with patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=execute):
                    macos_package_test_fixture.tool.run(["closure"], state, "closure")
                with patch.object(macos_package_test_fixture.tool.RUNNER, "run_command", side_effect=launch):
                    macos_package_test_fixture.tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
            self.assertIn("[package-check] finish closure exit=0 timeout=10s", stderr.getvalue())
            self.assertIn("[package-check] finish probe-launch result=PASS timeout=10s", stderr.getvalue())
