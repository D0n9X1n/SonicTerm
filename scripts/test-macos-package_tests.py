#!/usr/bin/env python3
"""Portable contracts for native package validation and controlled size evidence."""

import importlib.util
import os
from pathlib import Path
import plistlib
import tempfile
import sys
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("package_check", Path(__file__).with_name("test-macos-package.py"))
tool = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tool)


class PackageTests(unittest.TestCase):
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
            with patch.object(tool, "run", side_effect=fake_run):
                result = tool.measure_font_savings(app, state)
            self.assertEqual(result["font_dmg_saved_bytes"], 4)
            self.assertEqual(len(list(app.rglob("*.ttf"))), 4)
            self.assertFalse((state / "measurement").exists())
            images = [call for call in calls if call[0] == "/usr/bin/hdiutil"]
            self.assertEqual(images[0][images[0].index("-srcfolder") + 1], images[1][images[1].index("-srcfolder") + 1])


if __name__ == "__main__":
    unittest.main()
