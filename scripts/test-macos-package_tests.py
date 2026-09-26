#!/usr/bin/env python3
"""Portable contracts for native package validation and controlled size evidence."""

import contextlib
import importlib.util
import io
import json
import shutil
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
            limit = deadline - tool.CLEANUP_RESERVE_SECONDS - tool.VALIDATION_SUPERVISION_SECONDS
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
            now = (deadline - tool.CLEANUP_RESERVE_SECONDS - tool.VALIDATION_SUPERVISION_SECONDS
                   - tool.BUSY_RETRY_WAIT_SECONDS - tool.BUSY_RETRY_MINIMUM_SECONDS + 1)
            with contextlib.redirect_stderr(io.StringIO()), patch.object(tool, "DEADLINE", deadline), \
                    patch.object(tool, "clock", return_value=now), \
                    patch.object(tool.RUNNER, "run_command", side_effect=scripted_hdiutil(created, outcomes)), \
                    patch.object(tool, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, "single-measurement exited 1: .*Resource busy"):
                    tool.create_measurement_image(state / "measured.app", state / "single-fonts.dmg", state,
                                                  "single-measurement")
            self.assertEqual(len(created), 1)
            self.assertEqual(created[0][1], min(120, int(deadline - tool.CLEANUP_RESERVE_SECONDS
                                                       - tool.VALIDATION_SUPERVISION_SECONDS - now)))
            sleep.assert_not_called()

    def test_main_starts_the_deadline_before_mount_and_still_unmounts(self):
        # Validation consumes the work budget, but owned cleanup retains its independent reserve.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            def validate(app, state, dmg, minimum):
                self.assertEqual(tool.DEADLINE, fake.started + tool.STEP_BUDGET_SECONDS)
                self.assertEqual((app / "Contents/fixture").read_bytes(), b"installed fixture")
                fake.now = tool.DEADLINE - tool.CLEANUP_RESERVE_SECONDS + 0.5
                tool.run(["never-executed"], state, "closure")
            argv = ["test-macos-package.py", "--dmg", str(fake.image), "--state-dir", str(fake.state)]
            with fake.active(), patch.object(sys, "argv", argv), \
                    patch.object(tool, "validate", side_effect=validate), \
                    patch.object(tool.RUNNER, "run_command") as native:
                with self.assertRaisesRegex(RuntimeError, "closure: validator time budget exhausted"):
                    tool.main()
                native.assert_not_called()
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach", "info"])
            self.assertEqual([step.timeout_s for step in fake.steps], [3, 60, 3, 3, 20, 3])
            self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")

    def test_timed_out_validation_preserves_the_cleanup_reserve(self):
        # The native runner's post-kill wait must not consume time reserved for owned attachment cleanup.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            timeouts = []
            remaining_after_timeout = []
            def execute(command, _cwd, timeout, _environment):
                timeouts.append(timeout)
                fake.now += timeout + 10
                remaining_after_timeout.append(tool.DEADLINE - fake.now)
                return subprocess.CompletedProcess(command, tool.RUNNER.TIMEOUT_EXIT_CODE,
                                                   b"", b"native smoke timed out\n")
            def validate(*_args):
                fake.now = tool.DEADLINE - tool.CLEANUP_RESERVE_SECONDS - 50
                tool.run(["never-executed"], fake.state, "closure")
            argv = ["test-macos-package.py", "--dmg", str(fake.image), "--state-dir", str(fake.state)]
            with fake.active(), patch.object(sys, "argv", argv), \
                    patch.object(tool, "validate", side_effect=validate), \
                    patch.object(tool.RUNNER, "run_command", side_effect=execute):
                with self.assertRaisesRegex(RuntimeError, "closure exited 124"):
                    tool.main()
            self.assertEqual(timeouts, [40])
            self.assertEqual(remaining_after_timeout, [tool.CLEANUP_RESERVE_SECONDS])
            self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")
            self.assertFalse(fake.mounted)

    def test_cleanup_failure_preserves_the_original_validation_error(self):
        # A failed detach is retained as secondary evidence, never substituted for validation's error.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.detach_result = ("FAIL", 1, 0)
            original = RuntimeError("original validation failure")
            argv = ["test-macos-package.py", "--dmg", str(fake.image), "--state-dir", str(fake.state)]
            with fake.active(), patch.object(sys, "argv", argv), \
                    patch.object(tool, "validate", side_effect=original):
                with self.assertRaisesRegex(RuntimeError, "original validation failure") as caught:
                    tool.main()
            self.assertIs(caught.exception, original)
            self.assertEqual(fake.report()["original_error"], str(original))
            self.assertTrue(fake.report()["cleanup_errors"])
            self.assertEqual(fake.report()["status"], "FAIL")

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

    def test_finish_lines_report_the_capped_timeout(self):
        # A completion record names the timeout the command actually ran with, as its start line does.
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            stderr = io.StringIO()

            def execute(command, _cwd, _timeout, _environment):
                return subprocess.CompletedProcess(command, 0, b"", b"")

            def launch(command, _cwd, _timeout, _environment):
                (state / "native-fonts-cairo.log").write_text("RESULT fonts=4/4 cairo=PASS verdict=PASS\n")
                return subprocess.CompletedProcess(command, 0, b"", b"")

            limit = 1000.0 - tool.CLEANUP_RESERVE_SECONDS - tool.VALIDATION_SUPERVISION_SECONDS
            with contextlib.redirect_stderr(stderr), patch.object(tool, "DEADLINE", 1000.0), \
                    patch.object(tool, "clock", return_value=limit - 10):
                with patch.object(tool.RUNNER, "run_command", side_effect=execute):
                    tool.run(["closure"], state, "closure")
                with patch.object(tool.RUNNER, "run_command", side_effect=launch):
                    tool.run_font_probe(state / "Probe.app", state, state / "libcairo.dylib")
            self.assertIn("[package-check] finish closure exit=0 timeout=10s", stderr.getvalue())
            self.assertIn("[package-check] finish probe-launch result=PASS timeout=10s", stderr.getvalue())



class FakeAttachmentGate:
    def __init__(self, root):
        self.root = root.resolve()
        self.state = self.root / "state"
        self.mount = self.state / "mounted"
        self.image = self.root / "fixture image.dmg"
        self.image.write_bytes(b"image fixture")
        self.started = self.now = 1000.0
        self.deadline = self.now + 420
        self.mounted = False
        self.inventory = []
        self.steps = []
        self.queries = 0
        self.late_attach_query = None
        self.attach_result = ("PASS", 0, 0)
        self.detach_result = ("PASS", 0, 0)
        self.after_attach = None
        self.census_payload = None
        self.oversleep = 0
        self.waits = []

    def owned(self, image=None, mount=None):
        return {"image-path": str(image or self.image), "system-entities": [
            {"dev-entry": "/dev/disk2"},
            {"dev-entry": "/dev/disk2s1", "mount-point": str(mount or self.mount)}]}

    def install(self):
        self.mounted = True
        self.inventory = [self.owned()]
        app = self.mount / "SonicTerm.app/Contents"
        app.mkdir(parents=True, exist_ok=True)
        (app / "fixture").write_bytes(b"installed fixture")

    def clear(self):
        self.mounted = False
        self.inventory = []
        app = self.mount / "SonicTerm.app"
        if app.exists():
            shutil.rmtree(app)

    def execute(self, step, index, root, log_dir, environment, *, output_limit_bytes):
        assert output_limit_bytes == 1 << 20
        assert environment.get("HOME") == os.environ.get("HOME")
        assert "NO_COLOR" not in environment
        assert tuple(step.argv[:1]) == ("/usr/bin/hdiutil",)
        self.steps.append(step)
        status, code, leftovers = "PASS", 0, 0
        payload = b""
        operation = step.argv[1]
        if operation == "info":
            assert step.argv[2:] == ("-plist",)
            self.queries += 1
            if self.queries == self.late_attach_query:
                self.install()
            payload = self.census_payload if self.census_payload is not None else plistlib.dumps({"images": self.inventory})
        elif operation == "attach":
            status, code, leftovers = self.attach_result
            if status == "PASS" and code == 0 and leftovers == 0:
                self.install()
            else:
                payload = b"hdiutil: attach failed - Resource temporarily unavailable\n"
            if self.after_attach:
                self.after_attach(self)
        elif operation == "detach":
            assert step.argv == ("/usr/bin/hdiutil", "detach", "/dev/disk2s1")
            status, code, leftovers = self.detach_result
            if status == "PASS" and code == 0:
                self.clear()
            else:
                payload = b"secondary detach failure\n"
        else:
            raise AssertionError(step.argv)
        path = log_dir / f"{index:02d}-{step.id}.log"
        with path.open("xb") as log:
            tool.GATE._write_header(log, step, tool.GATE.launch_argv(step), root)
            log.write(payload)
            return tool.GATE._finish(log, step, path, tool.GATE.time.monotonic(),
                                     status, code, "", leftovers)

    @contextlib.contextmanager
    def active(self):
        original_is_mount = type(self.mount).is_mount
        def is_mount(path):
            return self.mounted if path == self.mount else original_is_mount(path)
        def sleep(seconds):
            self.waits.append(seconds)
            self.now += seconds + self.oversleep
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(tool.GATE, "run_step", side_effect=self.execute))
            stack.enter_context(patch.object(type(self.mount), "is_mount", is_mount))
            stack.enter_context(patch.object(tool, "DEADLINE", self.deadline))
            stack.enter_context(patch.object(tool, "clock", side_effect=lambda: self.now))
            stack.enter_context(patch.object(tool, "sleep", side_effect=sleep))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            yield self

    def attachment(self):
        self.state.mkdir()
        return tool.DmgAttachment(self.image, self.state)

    def operations(self):
        return [step.argv[1] for step in self.steps]

    def report(self):
        return json.loads((self.state / "attachment-result.json").read_text())


class AttachmentTests(unittest.TestCase):
    def test_census_accepts_empty_and_complete_multidevice_inventory(self):
        # Whole disks and slices remain distinct; normalized aliases preserve actual path identity.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            folder = fake.root / "alias"
            folder.mkdir()
            image = fake.owned(image=folder / ".." / fake.image.name)
            other = {"image-path": str(fake.root / "other.dmg"), "system-entities": [
                {"dev-entry": "/dev/disk3"}, {"dev-entry": "/dev/disk3s1"},
                {"dev-entry": "/dev/disk3s1s1", "mount-point": str(fake.root / "other mount")}]}
            self.assertEqual(tool.attachment_census(plistlib.dumps({"images": []})), [])
            parsed = tool.attachment_census(plistlib.dumps({"images": [image, other]}))
            self.assertEqual(parsed[0]["image"], str(fake.image))
            self.assertEqual(parsed[0]["entities"], [{"device": "/dev/disk2", "mount": None},
                {"device": "/dev/disk2s1", "mount": str(fake.mount)}])
            self.assertEqual(len(parsed[1]["entities"]), 3)

    def test_census_rejects_incomplete_ambiguous_and_unbounded_payloads(self):
        # Invalid census data cannot serve as absence or ownership evidence.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            invalid = [{}, {"images": {}}, {"images": [1]},
                {"images": [{"image-path": "relative.dmg", "system-entities": []}]},
                {"images": [{"image-path": str(fake.image)}]},
                {"images": [{"image-path": str(fake.image), "system-entities": {}}]},
                {"images": [{"image-path": str(fake.image), "system-entities": [1]}]},
                {"images": [{"image-path": str(fake.image), "system-entities": [{"dev-entry": "/dev/other"}]}]},
                {"images": [{"image-path": str(fake.image), "system-entities": [{"dev-entry": "/dev/disk2", "mount-point": "relative"}]}]},
                {"images": [fake.owned(), fake.owned()]}]
            payloads = [plistlib.dumps(value) for value in invalid]
            valid = plistlib.dumps({"images": []})
            payloads += [b"not plist", valid[:-12], valid + b"unparsed trailing bytes"]
            for payload in payloads:
                with self.subTest(payload=payload[:90]), self.assertRaises(RuntimeError):
                    tool.attachment_census(payload)

    def test_census_rejects_oversized_otherwise_valid_plist(self):
        # Valid inventory syntax must not make the independent byte limit optional.
        payload = plistlib.dumps({"images": [], "padding": "x" * tool.ATTACH_OUTPUT_LIMIT})
        self.assertGreater(len(payload), tool.ATTACH_OUTPUT_LIMIT)
        with self.assertRaisesRegex(RuntimeError, "census exceeds its output bound"):
            tool.attachment_census(payload)
        with patch.object(tool, "ATTACH_OUTPUT_LIMIT", len(payload)):
            self.assertEqual(tool.attachment_census(payload), [])

    def test_attach_success_owns_one_new_device_and_proves_detach(self):
        # Successful attachment is accepted only with matching private ownership and confirmed cleanup.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            with fake.active():
                attachment = fake.attachment()
                self.assertEqual(attachment.attach(), fake.mount)
                attachment.finish(None)
            attach = next(step for step in fake.steps if step.argv[1] == "attach")
            self.assertEqual(attach.argv, ("/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse",
                "-mountpoint", str(fake.mount), str(fake.image)))
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach", "info"])
            report = fake.report()
            self.assertEqual((report["status"], report["attach_attempts"]), ("PASS", 1))
            self.assertEqual(report["owned_device"], "/dev/disk2s1")
            self.assertEqual(report["detached_device"], "/dev/disk2s1")
            self.assertEqual(report["censuses"][-1]["images"], [])
            self.assertTrue(all((fake.state / command["log"]).is_file() for command in report["commands"]))

    def test_refusal_is_never_retried_and_retains_three_late_censuses(self):
        # EAGAIN is one failed attempt, followed by evidence collection rather than another attach.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaisesRegex(RuntimeError, "Resource temporarily unavailable") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "info"])
            self.assertEqual(fake.waits, [2, 3])
            self.assertEqual(fake.report()["attach_attempts"], 1)
            self.assertEqual(fake.report()["status"], "FAIL")
            self.assertEqual([row["label"] for row in fake.report()["censuses"]],
                ["mount-before", "mount-cleanup-state-0", "mount-cleanup-state-1", "mount-cleanup-state-2"])
            self.assertTrue(all((fake.state / row["log"]).exists() for row in fake.report()["commands"]))

    def test_late_owned_mount_is_detached_without_erasing_attach_failure(self):
        # A later census can establish custody, but cannot retroactively make attach successful.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            fake.late_attach_query = 3
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations().count("attach"), 1)
            self.assertEqual(fake.operations().count("detach"), 1)
            self.assertEqual(fake.report()["status"], "FAIL")
            self.assertEqual(fake.report()["original_error"], str(caught.exception))
            self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")

    def test_partial_conflicting_preexisting_and_raced_mounts_are_not_detached(self):
        # Device names and mount paths alone never authorize teardown of an ambiguous attachment.
        for case in ("partial", "elsewhere", "conflict", "preexisting-device", "image-race", "directory-race"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                fake = FakeAttachmentGate(Path(directory))
                if case == "preexisting-device":
                    fake.inventory = [{"image-path": str(fake.root / "other.dmg"),
                        "system-entities": [{"dev-entry": "/dev/disk2"}]}]
                def after(fake):
                    if case == "partial":
                        fake.clear()
                        fake.inventory = [{"image-path": str(fake.image), "system-entities": [{"dev-entry": "/dev/disk2"}]}]
                    elif case == "elsewhere":
                        fake.inventory = [fake.owned(mount=fake.root / "shared mount")]
                    elif case == "conflict":
                        fake.inventory = [fake.owned(image=fake.root / "other.dmg")]
                    elif case == "image-race":
                        fake.image.rename(fake.root / "original.dmg")
                        fake.image.write_bytes(b"replacement")
                    elif case == "directory-race":
                        fake.clear()
                        fake.mount.rename(fake.state / "original-mount")
                        fake.mount.mkdir()
                fake.after_attach = after
                with fake.active():
                    attachment = fake.attachment()
                    with self.assertRaises(RuntimeError) as caught:
                        attachment.attach()
                    attachment.finish(caught.exception)
                self.assertNotIn("detach", fake.operations())
                self.assertEqual(fake.report()["status"], "FAIL")

    def test_owned_census_requires_an_actual_mount_and_unique_mountpoint(self):
        # Census device membership alone cannot prove a live mount or choose between two mountpoint claimants.
        for case, reason in (("unmounted", "census mount is not present"),
                             ("duplicate-mount", "ownership is partial or ambiguous")):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                fake = FakeAttachmentGate(Path(directory))
                def after(state):
                    if case == "unmounted":
                        state.mounted = False
                    else:
                        state.inventory[0]["system-entities"].append(
                            {"dev-entry": "/dev/disk3s1", "mount-point": str(state.mount)})
                fake.after_attach = after
                with fake.active():
                    attachment = fake.attachment()
                    with self.assertRaisesRegex(RuntimeError, reason) as caught:
                        attachment.attach()
                    attachment.finish(caught.exception)
                self.assertNotIn("detach", fake.operations())
                self.assertTrue(fake.report()["cleanup_errors"])

    def test_owned_image_accepts_physical_and_synthesized_devices(self):
        # APFS can expose physical and synthesized disk identities within one uniquely owned image.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            def after(state):
                state.inventory[0]["system-entities"].insert(0, {"dev-entry": "/dev/disk7"})
                state.inventory[0]["system-entities"].insert(1, {"dev-entry": "/dev/disk7s1"})
            fake.after_attach = after
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                attachment.finish(None)
            self.assertEqual(fake.report()["status"], "PASS")
            self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")
            self.assertEqual(len(fake.report()["censuses"][1]["images"][0]["entities"]), 4)

    def test_zero_exit_detach_requires_complete_absence_evidence(self):
        # A zero detach exit cannot replace the independent mount, device and complete-plist checks.
        cases = {"retained-mount": "owned attachment remains after detach",
                 "retained-device": "ownership is partial or ambiguous",
                 "invalid-plist": "census is not a complete plist"}
        for case, reason in cases.items():
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                fake = FakeAttachmentGate(Path(directory))
                execute = fake.execute
                def contradict(step, *args, **kwargs):
                    result = execute(step, *args, **kwargs)
                    if step.id == "unmount":
                        if case == "retained-mount":
                            fake.install()
                        elif case == "retained-device":
                            fake.inventory = [{"image-path": str(fake.image),
                                               "system-entities": [{"dev-entry": "/dev/disk2"}]}]
                        else:
                            fake.census_payload = b"<plist><dict>"
                    return result
                with fake.active(), patch.object(tool.GATE, "run_step", side_effect=contradict):
                    attachment = fake.attachment()
                    attachment.attach()
                    with self.assertRaisesRegex(RuntimeError, reason):
                        attachment.finish(None)
                self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach", "info"])
                self.assertNotIn("detached_device", fake.report())
                self.assertEqual(fake.report()["status"], "FAIL")
                self.assertIsNone(fake.report()["original_error"])

    def test_preexisting_image_alias_is_refused_before_attach(self):
        # Filesystem aliases to the same image cannot evade the pre-existing attachment census.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            alias = fake.root / "hardlink.dmg"
            os.link(fake.image, alias)
            fake.inventory = [fake.owned(image=alias, mount=fake.root / "shared")]
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaisesRegex(RuntimeError, "already attached") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info"])
            self.assertEqual(fake.report()["attach_attempts"], 0)

    def test_zero_exit_without_owned_mount_is_not_success(self):
        # A successful process exit without a census-backed mount cannot reach validation.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.after_attach = lambda fake: fake.clear()
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaisesRegex(RuntimeError, "no owned mount") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertNotIn("detach", fake.operations())
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_unsettled_or_fatal_supervisor_results_never_accept_attach(self):
        # Even owned-looking native side effects cannot turn an unsettled command into accepted attachment.
        outcomes = [("TIMEOUT", -9, 0), ("LAUNCH", None, 0), ("FAIL", None, 0),
            ("FAIL", -15, 0), ("FAIL", 0, 0), ("FAIL", 1, None),
            ("PASS", None, 0), ("PASS", -15, 0), ("PASS", 0, None), ("PASS", 0, 1)]
        for outcome in outcomes:
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as directory:
                fake = FakeAttachmentGate(Path(directory))
                fake.attach_result = outcome
                fake.after_attach = lambda state: state.install()
                with fake.active():
                    attachment = fake.attachment()
                    prefix = re.escape(f"mount: {outcome[0]} exit={outcome[1]}:")
                    with self.assertRaisesRegex(RuntimeError, "^" + prefix) as caught:
                        attachment.attach()
                    self.assertTrue(fake.mounted)
                    self.assertEqual(fake.operations(), ["info", "attach"])
                    self.assertNotIn("owned_device", attachment.report)
                    attachment.finish(caught.exception)
                self.assertEqual(fake.operations(), ["info", "attach", "info", "detach", "info"])
                self.assertFalse(fake.mounted)
                self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")
                self.assertEqual(fake.report()["original_error"], str(caught.exception))
                self.assertEqual(fake.report()["status"], "FAIL")

    def test_invalid_census_blocks_new_attachment(self):
        # Invalid successful query output is not treated as an empty inventory.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.census_payload = b"<plist><dict>"
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info"])
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_admission_and_expired_deadline_never_launch_operations(self):
        # Admission includes command and supervision allowances, and expired cleanup cannot launch.
        for remaining in (17, -1):
            with self.subTest(remaining=remaining), tempfile.TemporaryDirectory() as directory:
                fake = FakeAttachmentGate(Path(directory))
                fake.now = fake.deadline - tool.CLEANUP_RESERVE_SECONDS - remaining
                with fake.active():
                    attachment = fake.attachment()
                    with self.assertRaisesRegex(RuntimeError, "budget") as caught:
                        attachment.attach()
                    fake.now = fake.deadline + 1
                    attachment.attempted = True
                    attachment.finish(caught.exception)
                self.assertEqual(fake.steps, [])
                self.assertTrue(fake.report()["cleanup_errors"])

    def test_insufficient_attach_proof_budget_stops_after_initial_census(self):
        # The attach command must fit together with the required post-attach ownership census.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.now = fake.deadline - tool.CLEANUP_RESERVE_SECONDS - 92
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaisesRegex(RuntimeError, "budget") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info"])

    def test_oversleep_cannot_spend_protected_settlement_budget(self):
        # A late scheduler wake is rechecked before launching any later query or detach.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            fake.oversleep = 500
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info", "attach", "info"])
            self.assertEqual(fake.waits, [2])
            self.assertTrue(fake.report()["cleanup_errors"])

    def test_initial_cleanup_census_can_use_confirmation_slack(self):
        # Current ownership and detach take priority when the full confirmation reserve no longer fits.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                fake.now = fake.deadline - 72
                attachment.finish(None)
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach", "info"])
            self.assertEqual(fake.report()["status"], "PASS")
            self.assertFalse(fake.mounted)

    def test_optional_late_census_preserves_detach_and_confirmation_time(self):
        # A failed attach may have escaped helpers; later observations cannot spend the settlement reserve.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                fake.now = fake.deadline - 72
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info", "attach", "info"])
            self.assertEqual([row["label"] for row in fake.report()["commands"]
                              if row["status"] == "SKIPPED_BUDGET"],
                             ["mount-cleanup-state-1", "mount-cleanup-state-2"])
            self.assertTrue(fake.report()["cleanup_errors"])

    def test_slow_census_does_not_reserve_confirmation_instead_of_detaching(self):
        # Supervisor overrun may leave only enough time to detach; missing confirmation must still fail.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            execute = fake.execute
            def slow(step, *args, **kwargs):
                result = execute(step, *args, **kwargs)
                if step.id == "mount-cleanup-state-0":
                    fake.now = fake.deadline - 40
                elif step.id == "unmount":
                    fake.now = fake.deadline - 5
                return result
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                fake.now = fake.deadline - 75
                with patch.object(tool.GATE, "run_step", side_effect=slow), \
                        self.assertRaisesRegex(RuntimeError, "cleanup unconfirmed"):
                    attachment.finish(None)
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach"])
            self.assertFalse(fake.mounted)
            self.assertNotIn("detached_device", fake.report())
            self.assertEqual(fake.report()["commands"][-1],
                             {"label": "unmount-after", "status": "SKIPPED_BUDGET"})

    def test_later_owned_detach_resolves_earlier_observation_errors(self):
        # Retain a malformed early observation without misreporting a later confirmed owned cleanup.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            execute = fake.execute
            def observe(step, *args, **kwargs):
                fake.census_payload = b"invalid plist" if step.id == "mount-cleanup-state-0" else None
                if step.id == "mount-cleanup-state-1":
                    fake.install()
                return execute(step, *args, **kwargs)
            with fake.active(), patch.object(tool.GATE, "run_step", side_effect=observe):
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            report = fake.report()
            self.assertFalse(fake.mounted)
            self.assertEqual(report["detached_device"], "/dev/disk2s1")
            self.assertEqual(report["cleanup_errors"], [])
            self.assertTrue(report["observation_errors"])
            self.assertEqual(report["original_error"], str(caught.exception))
            self.assertEqual(report["status"], "FAIL")

    def test_unreadable_image_alias_remains_unknown(self):
        # A different unreadable pathname could be a hard link to the target, so it cannot prove absence.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            candidate = fake.root / "unreadable.dmg"
            fake.inventory = [fake.owned(image=candidate, mount=fake.root / "shared")]
            original_stat = type(candidate).stat
            def stat(path, *args, **kwargs):
                if path == candidate:
                    raise PermissionError("unreadable image identity")
                return original_stat(path, *args, **kwargs)
            with fake.active(), patch.object(type(candidate), "stat", stat):
                attachment = fake.attachment()
                with self.assertRaisesRegex(PermissionError, "unreadable image identity") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info"])
            self.assertEqual(fake.report()["attach_attempts"], 0)

    def test_cleanup_failure_alone_fails_the_result(self):
        # A valid mount and validation cannot make an unconfirmed detach a passing result.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            fake.detach_result = ("FAIL", 1, 0)
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                with self.assertRaisesRegex(RuntimeError, "cleanup"):
                    attachment.finish(None)
            self.assertIsNone(fake.report()["original_error"])
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_evidence_write_failure_preserves_original_and_existing_bytes(self):
        # Exclusive evidence creation must preserve both the initial error and an existing receipt.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            original = RuntimeError("original validation failure")
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                receipt = fake.state / "attachment-result.json"
                receipt.write_bytes(b"earlier evidence")
                attachment.finish(original)
            self.assertEqual(receipt.read_bytes(), b"earlier evidence")
            self.assertEqual(attachment.report["original_error"], str(original))
            self.assertIn("detach", fake.operations())

    def test_cleanup_interrupt_does_not_replace_original(self):
        # A cleanup interrupt is secondary when an earlier validation error already owns the result.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            original = RuntimeError("original validation failure")
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                with patch.object(tool.GATE, "run_step", side_effect=KeyboardInterrupt):
                    attachment.finish(original)
            self.assertEqual(fake.report()["original_error"], str(original))
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_replaced_evidence_directory_prevents_commands_and_receipt_writes(self):
        # A renamed private directory cannot authorize commands or evidence writes into its replacement.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            with fake.active():
                attachment = fake.attachment()
                fake.state.rename(fake.root / "original-state")
                fake.state.mkdir()
                with self.assertRaisesRegex(RuntimeError, "directory changed"):
                    attachment.command("must-not-launch", ["/usr/bin/hdiutil", "info", "-plist"], 3)
                original = RuntimeError("original validation failure")
                attachment.finish(original)
            self.assertEqual(fake.steps, [])
            self.assertFalse((fake.state / "attachment-result.json").exists())
            self.assertEqual(list(fake.state.iterdir()), [])

    def test_symlinked_mountpoint_is_rejected_without_native_work(self):
        # Simulate the filesystem symlink boundary without requiring Windows symlink privileges.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            with fake.active():
                attachment = fake.attachment()
                original_is_symlink = type(fake.mount).is_symlink
                with patch.object(type(fake.mount), "is_symlink",
                                  lambda path: True if path == fake.mount else original_is_symlink(path)):
                    with self.assertRaisesRegex(RuntimeError, "symlink") as caught:
                        attachment.attach()
                    attachment.finish(caught.exception)
            self.assertEqual(fake.steps, [])
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_real_supervisor_overflow_is_not_accepted_as_successful_payload(self):
        # Truncated child output stays a custody failure even when the child itself exits zero.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            attachment = fake.attachment()
            code = f"import sys; sys.stdout.buffer.write(b'x' * {tool.ATTACH_OUTPUT_LIMIT + 1})"
            with patch.object(tool, "DEADLINE", None), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, r"^python-overflow: FAIL exit=0:") as caught:
                    attachment.command("python-overflow", [sys.executable, "-c", code], 5)
                attachment.finish(caught.exception)
            command = fake.report()["commands"][0]
            self.assertEqual((command["status"], command["exit_code"], command["leftover_processes"]), ("FAIL", 0, 0))
            self.assertIn(f"output limit of {tool.ATTACH_OUTPUT_LIMIT} bytes exceeded", command["detail"])
            self.assertEqual(fake.report()["status"], "FAIL")
            self.assertEqual(fake.report()["attach_attempts"], 0)

    def test_real_supervisor_framing_preserves_home_and_removes_color(self):
        # A harmless real Python child pins the actual shared-runner framing and environment contract.
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeAttachmentGate(Path(directory))
            attachment = fake.attachment()
            home = str(fake.root / "real home")
            code = "import json,os; print(json.dumps([os.environ.get('HOME'),os.environ.get('NO_COLOR')]))"
            with patch.object(tool, "DEADLINE", None), patch.dict(os.environ, {"HOME": home, "NO_COLOR": "1"}), \
                    contextlib.redirect_stderr(io.StringIO()):
                payload = attachment.command("python-framing", [sys.executable, "-c", code], 5)
                attachment.finish(None)
            self.assertEqual(json.loads(payload), [home, None])
            command = fake.report()["commands"][0]
            self.assertEqual((command["status"], command["exit_code"], command["leftover_processes"]), ("PASS", 0, 0))
            self.assertIn(b"[local-gate] result=PASS", (fake.state / command["log"]).read_bytes())

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
