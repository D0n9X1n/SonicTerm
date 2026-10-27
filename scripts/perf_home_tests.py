"""Perf home tests."""

from __future__ import annotations
import os
from pathlib import Path
import tempfile
import unittest

import perf_home
import perf_processes_tests


class HomeWriteTests(unittest.TestCase):
    SENTINEL_NS = 5_000_000_000
    HARNESS = 900

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.home = Path(self.temporary.name) / ".sonicterm"
        self.scratch = Path(self.temporary.name) / "scratch-run"

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, relative, data, mtime_ns=1_000_000_000):
        """Write a file under the temporary home with a chosen modification time."""
        target = self.home / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        os.utime(target, ns=(mtime_ns, mtime_ns))

    def violations(self, before, other_alive=False):
        after = perf_home.snapshot_home(self.home)
        return perf_home.home_violations(self.home, before, after, self.SENTINEL_NS, self.HARNESS,
                                    other_alive, [str(self.scratch)])

    def test_absent_home_is_recorded_and_never_created(self):
        # The check reads only; an absent directory stays absent and is no violation.
        self.assertIsNone(perf_home.snapshot_home(self.home))
        self.assertFalse(self.home.exists())
        self.assertEqual(self.violations(None), [])

    def test_created_home_or_changed_config_is_a_violation(self):
        # A write outside the attributable logs is a violation, including creating the directory.
        self.write("config.toml", b"x")
        self.assertTrue(self.violations(None))
        before = perf_home.snapshot_home(self.home)
        self.write("config.toml", b"changed", self.SENTINEL_NS + 1)
        self.assertEqual(len(self.violations(before)), 1)

    def test_ds_store_is_ignored(self):
        # Finder metadata is not a SonicTerm write.
        self.write("config.toml", b"x")
        before = perf_home.snapshot_home(self.home)
        self.write("logs/.DS_Store", b"finder", self.SENTINEL_NS + 1)
        self.assertEqual(self.violations(before), [])

    def test_breadcrumb_belongs_to_the_pid_in_its_name(self):
        # Another instance's breadcrumbs are its own; the harness starts no breadcrumb writer.
        self.write("config.toml", b"x")
        before = perf_home.snapshot_home(self.home)
        self.write("logs/breadcrumbs/breadcrumbs-20261002T112416-4242-0.log", b"crumb", self.SENTINEL_NS + 1)
        self.assertEqual(self.violations(before), [])
        self.write(f"logs/breadcrumbs/breadcrumbs-20261002T112416-{self.HARNESS}-1.log", b"crumb",
                   self.SENTINEL_NS + 1)
        self.assertEqual(len(self.violations(before)), 1)

    def test_log_growth_belongs_elsewhere_only_with_another_instance_and_without_this_run(self):
        # Both conditions must hold, and only the appended bytes are read.
        self.write("logs/sonicterm.log.2026-10-02", str(self.scratch).encode() + b"\n")
        before = perf_home.snapshot_home(self.home)
        self.write("logs/sonicterm.log.2026-10-02", str(self.scratch).encode() + b"\nother instance\n",
                   self.SENTINEL_NS + 1)
        self.assertEqual(self.violations(before, other_alive=True), [])
        self.assertTrue(self.violations(before, other_alive=False))
        self.write("logs/sonicterm.log.2026-10-02", str(self.scratch).encode() + b"\nopened "
                   + str(self.scratch).encode() + b"\n", self.SENTINEL_NS + 1)
        self.assertTrue(self.violations(before, other_alive=True))

    def test_a_shrunk_log_is_not_growth(self):
        # Rewriting a log is no append, so no other instance explains it.
        self.write("logs/sonicterm.log", b"long existing content\n")
        before = perf_home.snapshot_home(self.home)
        self.write("logs/sonicterm.log", b"short\n", self.SENTINEL_NS + 1)
        self.assertTrue(self.violations(before, other_alive=True))

    def test_removal_belongs_elsewhere_only_under_logs(self):
        # Another instance's retention may remove old logs; nothing else may disappear.
        self.write("logs/sonicterm.log.2026-09-01", b"old\n")
        self.write("themes/custom.toml", b"x")
        before = perf_home.snapshot_home(self.home)
        (self.home / "logs/sonicterm.log.2026-09-01").unlink()
        self.assertEqual(self.violations(before, other_alive=True), [])
        self.assertTrue(self.violations(before, other_alive=False))
        (self.home / "themes/custom.toml").unlink()
        self.assertEqual(len(self.violations(before, other_alive=True)), 1)

    def test_unchanged_file_newer_than_the_sentinel_is_a_candidate(self):
        # A write between the sentinel and the first snapshot still reads as newer than the sentinel.
        # 1 s later: a filesystem stores file times at its own resolution, 100 ns on NTFS and 1 s on HFS+.
        self.write("config.toml", b"x", self.SENTINEL_NS + 1_000_000_000)
        self.assertTrue(self.violations(perf_home.snapshot_home(self.home)))

    def test_another_instance_is_a_live_sonicterm_process_other_than_the_harness(self):
        # The harness itself, other programs and unreadable processes are not another instance.
        table = perf_processes_tests.FakeTable(perf_processes_tests.FakeProcess(self.HARNESS, 900, 900, command="sonicterm-mac"),
                          perf_processes_tests.FakeProcess(901, 901, 901, command="zsh"),
                          perf_processes_tests.FakeProcess(902, 902, 902, command="sonicterm-mac", unreadable=True))
        self.assertFalse(perf_home.other_instance_alive(table, self.HARNESS))
        table.processes[903] = perf_processes_tests.FakeProcess(903, 903, 903, command="sonicterm-linux")
        self.assertTrue(perf_home.other_instance_alive(table, self.HARNESS))
        table.enumeration_fails = True
        self.assertFalse(perf_home.other_instance_alive(table, self.HARNESS))

    def test_the_home_follows_the_app_home_then_userprofile(self):
        # The App reads HOME, then USERPROFILE (sonicterm-cfg dirs_home); Git Bash sets HOME on Windows.
        self.assertEqual(perf_home.sonicterm_home({"HOME": "/h", "USERPROFILE": "C:/u"}), Path("/h") / ".sonicterm")
        self.assertEqual(perf_home.sonicterm_home({"USERPROFILE": "C:/u"}), Path("C:/u") / ".sonicterm")
        self.assertEqual(perf_home.sonicterm_home({}), Path.home() / ".sonicterm")

    def test_the_windows_binary_counts_as_another_instance(self):
        # An installed SonicTerm on Windows runs as sonicterm-windows.exe and may write the shared home.
        table = perf_processes_tests.FakeTable(perf_processes_tests.FakeProcess(904, 0, 0, command="sonicterm-windows.exe"))
        self.assertTrue(perf_home.other_instance_alive(table, self.HARNESS))


SELECTION_TABLE = """[[example]]
name = "native_split_selection"
path = "examples/native_split_selection.rs"
harness = false
test = false
"""
HARNESS_TABLES = """[[example]]
name = "perf_scenarios"
path = "examples/perf_scenarios/main.rs"
test = true

[[example]]
name = "perf_scenarios_alloc"
path = "examples/perf_scenarios/alloc_main.rs"
test = false
"""
PACKAGE = '[package]\nname = "sonicterm-app"\n\n'
DEPENDENCIES = '\n[dev-dependencies]\ntempfile = "3"\n'
HEAD_MANIFEST = PACKAGE + SELECTION_TABLE + "\n" + HARNESS_TABLES + DEPENDENCIES
BASE_MANIFEST = PACKAGE + SELECTION_TABLE + DEPENDENCIES

try:
    import tomllib
except ImportError:  # Python before 3.11 has no TOML parser; the text checks still run.
    tomllib = None
