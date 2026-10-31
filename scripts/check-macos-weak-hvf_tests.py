#!/usr/bin/env python3
"""Offline contracts for the packaged macOS executable's weak `libhvf` load check."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

_SPEC = importlib.util.spec_from_file_location("check_weak_hvf", Path(__file__).with_name("check-macos-weak-hvf.py"))
assert _SPEC is not None and _SPEC.loader is not None
CHECK = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(CHECK)


def load_command(index: int, command: str, name: str) -> str:
    """One `otool -l` dylib load command, laid out as otool prints it."""
    return (f"Load command {index}\n          cmd {command}\n      cmdsize 48\n"
            f"         name {name} (offset 24)\n   time stamp 2 Wed Dec 31 16:00:02 1969\n"
            "      current version 1.0.0\ncompatibility version 1.0.0\n")


SEGMENT = "Load command 0\n      cmd LC_SEGMENT_64\n  cmdsize 72\n  segname __PAGEZERO\n"
SYSTEM = load_command(1, "LC_LOAD_DYLIB", "/usr/lib/libSystem.B.dylib")


class WeakHvfLoadTests(unittest.TestCase):
    def test_a_weak_libhvf_load_passes(self):
        # The shipped binary must still launch below macOS 15.4, where libhvf has no renderer symbols.
        output = SEGMENT + SYSTEM + load_command(2, "LC_LOAD_WEAK_DYLIB", "/usr/lib/libhvf.dylib")
        self.assertEqual(CHECK.hvf_load_problems(output), [])

    def test_a_strong_libhvf_load_is_rejected(self):
        # A strong load means the availability check folded away; macOS 15.0 to 15.3 would refuse to launch.
        output = SEGMENT + SYSTEM + load_command(2, "LC_LOAD_DYLIB", "/usr/lib/libhvf.dylib")
        problems = CHECK.hvf_load_problems(output)
        self.assertEqual(len(problems), 1)
        self.assertIn("LC_LOAD_DYLIB", problems[0])

    def test_a_missing_libhvf_load_is_rejected(self):
        # No load at all means the release FreeType was built without HVF and cannot open PingFang.
        problems = CHECK.hvf_load_problems(SEGMENT + SYSTEM)
        self.assertEqual(len(problems), 1)
        self.assertIn("no load", problems[0])

    def test_only_the_exact_system_path_counts(self):
        # A same-named library elsewhere, or a second libhvf load, is not the system library's single weak load.
        lookalike = SEGMENT + load_command(1, "LC_LOAD_WEAK_DYLIB", "/opt/lib/libhvf.dylib")
        self.assertTrue(CHECK.hvf_load_problems(lookalike))
        twice = (SEGMENT + load_command(1, "LC_LOAD_WEAK_DYLIB", "/usr/lib/libhvf.dylib")
                 + load_command(2, "LC_LOAD_WEAK_DYLIB", "/usr/lib/libhvf.dylib"))
        self.assertTrue(CHECK.hvf_load_problems(twice))

    def test_main_reports_each_executable_and_fails_on_any_problem(self):
        # Every executable named is checked; one strong load fails the step and names the file.
        outputs = {
            "weak": SEGMENT + load_command(1, "LC_LOAD_WEAK_DYLIB", "/usr/lib/libhvf.dylib"),
            "strong": SEGMENT + load_command(1, "LC_LOAD_DYLIB", "/usr/lib/libhvf.dylib"),
        }
        with patch.object(CHECK, "otool_load_commands", side_effect=lambda path: outputs[Path(path).name]):
            self.assertEqual(CHECK.main(["weak"]), 0)
            with patch("sys.stderr") as stderr:
                self.assertEqual(CHECK.main(["weak", "strong"]), 1)
            written = "".join(call.args[0] for call in stderr.write.call_args_list)
            self.assertIn("strong", written)

    def test_an_otool_failure_fails_the_check(self):
        # An unreadable executable cannot prove a weak load, so it is an error, never a pass.
        failure = subprocess.CalledProcessError(1, ["otool", "-l", "missing"], stderr="no such file")
        with patch.object(CHECK, "otool_load_commands", side_effect=failure), patch("sys.stderr"):
            self.assertEqual(CHECK.main(["missing"]), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
