"""Perf host tests."""

from __future__ import annotations
from perf_test_fixture import perf
import contextlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import perf_arguments
import perf_execution
import perf_focus_tests
import perf_font_diagnostics
import perf_foreground_tests
import perf_host_info
import perf_main
import perf_memory_tests
import perf_report
import perf_settings
import perf_smoke
import perf_test_fixture
import perf_worktrees


class FakeGit:
    """Answers `git worktree` commands by creating and removing directories."""

    def __init__(self, fail_add=False):
        self.calls = []
        self.fail_add = fail_add

    def __call__(self, argv):
        self.calls.append(tuple(argv))
        if tuple(argv[1:3]) == ("worktree", "add"):
            if self.fail_add:
                return perf_focus_tests.command(argv, "", exit_code=128, stderr="fatal: invalid reference\n")
            Path(argv[-2]).mkdir(parents=True)
        elif tuple(argv[1:3]) == ("worktree", "remove"):
            perf.shutil.rmtree(argv[-1])
        return perf_focus_tests.command(argv, "")


class WorktreeTests(unittest.TestCase):
    SHA = "78766228f0a0e6dd12bb0c6c4a251ea0eef0f13b"

    def test_each_side_gets_its_own_detached_worktree_even_for_one_sha(self):
        # Two refs with one SHA still get two trees, so the overlay never targets its own source.
        with tempfile.TemporaryDirectory() as temporary:
            git = FakeGit()
            worktrees = perf_worktrees.Worktrees(git, Path(temporary) / "work")
            base, head = worktrees.create("base", self.SHA), worktrees.create("head", self.SHA)
            self.assertNotEqual(base, head)
            self.assertEqual(git.calls, [("git", "worktree", "add", "--detach", str(base), self.SHA),
                                         ("git", "worktree", "add", "--detach", str(head), self.SHA)])

    def test_existing_path_is_never_reused_and_only_created_trees_are_removed(self):
        # Removal touches only what this run created, with --force because the overlay changed it.
        with tempfile.TemporaryDirectory() as temporary:
            git = FakeGit()
            worktrees = perf_worktrees.Worktrees(git, Path(temporary) / "work")
            (Path(temporary) / "work" / "base").mkdir(parents=True)
            with self.assertRaises(ValueError):
                worktrees.create("base", self.SHA)
            self.assertEqual(git.calls, [])
            head = worktrees.create("head", self.SHA)
            self.assertEqual(worktrees.remove(), [])
            self.assertEqual(git.calls[-1], ("git", "worktree", "remove", "--force", str(head)))
            self.assertEqual(sum(call[2] == "remove" for call in git.calls), 1)
            self.assertTrue((Path(temporary) / "work" / "base").is_dir())

    def test_failed_add_is_not_recorded_for_removal(self):
        # A worktree git refused to create is never removed.
        with tempfile.TemporaryDirectory() as temporary:
            git = FakeGit(fail_add=True)
            worktrees = perf_worktrees.Worktrees(git, Path(temporary) / "work")
            with self.assertRaises(ValueError):
                worktrees.create("head", self.SHA)
            worktrees.remove()
            self.assertEqual([call[2] for call in git.calls], ["add"])

    def test_work_directory_is_under_the_ignored_target_tree(self):
        # Worktrees and target directories live under target/perf-compare/, which git ignores.
        self.assertEqual(perf_worktrees.work_directory("stamp").parent, perf_settings.ROOT / "target" / "perf-compare")


class HostBlockTests(unittest.TestCase):
    OUTPUTS = {
        "model": "Mac14,9\n", "cpu": "Apple M2 Pro\n", "memory": "34359738368\n",
        "os": "ProductName:\t\tmacOS\nProductVersion:\t\t26.6.2\nBuildVersion:\t\t25G123\n",
        "displays": ("Graphics/Displays:\n\n    Apple M2 Pro:\n\n      Chipset Model: Apple M2 Pro\n"
                     "      Displays:\n        Color LCD:\n          Resolution: 3024 x 1964 Retina\n"
                     "          UI Looks like: 1512 x 982 @ 120.00Hz\n          Main Display: Yes\n"),
        "power": "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; present: true\n",
        "power_settings": "System-wide power settings:\nCurrently in use:\n standby              1\n lowpowermode         0\n",
    }

    def test_host_block_names_machine_os_gpu_display_and_power(self):
        # The PR's host block names what the brief lists, from the raw command outputs.
        text = "\n".join(perf_host_info.host_block(self.OUTPUTS))
        for expected in ("Mac14,9", "Apple M2 Pro", "32 GiB", "macOS 26.6.2 (25G123)", "3024 x 1964 Retina",
                         "120.00Hz", "scale 2.00", "AC Power", "Low Power Mode off"):
            with self.subTest(expected=expected):
                self.assertIn(expected, text)

    def test_missing_outputs_read_unavailable(self):
        # A command that failed leaves its figure unavailable rather than guessed.
        text = "\n".join(perf_host_info.host_block({}))
        self.assertIn("unavailable", text)
        self.assertNotIn("None", text)

    def test_document_holds_the_tables_host_and_collapsed_details(self):
        # Laps and allocation tables appear only when those run sets ran.
        rows = [["S1/default", "status", "5 valid runs", "5 valid runs", ""]]
        document = perf_report.comparison_document(rows, [], [], ["- OS: macOS"], ["- Base: main 7876"])
        self.assertIn(perf_execution.TABLE_HEADER, document)
        self.assertIn("### Host\n\n- OS: macOS", document)
        self.assertIn("<details><summary>", document)
        self.assertNotIn("Laps", document)
        self.assertIn("Laps", perf_report.comparison_document(rows, rows, [], [], []))

    WINDOWS_VALUES = {"cpu": "Intel(R) Core(TM) i7-1185G7 @ 3.00GHz", "manufacturer": "LENOVO",
                      "product": "20XW0026US", "os_name": "Windows 10 Enterprise", "os_version": "24H2",
                      "os_build": "26100", "os_ubr": 4061}
    POWERCFG = "Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)\n"

    def windows_outputs(self, values, powercfg_exit=0, memory=34359738368, conhost="10.0.26100.4061"):
        """Read the Windows host through a fake registry, memory status, powercfg and file version."""
        registry = {perf_host_info.WINDOWS_REGISTRY_VALUES[name]: value for name, value in values.items()}
        argvs = []

        def host_run(argv, timeout_s):
            argvs.append(tuple(argv))
            return perf_focus_tests.command(argv, self.POWERCFG if powercfg_exit == 0 else "", exit_code=powercfg_exit)
        outputs = perf_host_info.windows_host_outputs(host_run, registry=lambda key, name: registry.get((key, name)),
                                            memory=lambda: memory, file_version=lambda path: conhost)
        return outputs, argvs

    def test_the_windows_host_block_reads_registry_memory_power_and_conhost(self):
        # The Windows block names machine, OS build, adapter, display, power plan and console host version.
        outputs, argvs = self.windows_outputs(self.WINDOWS_VALUES)
        self.assertEqual(argvs, [perf_host_info.POWERCFG_ARGV])
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 60000, "scale_factor": 1.5}
        text = "\n".join(perf_host_info.host_block_windows(outputs, monitor, perf_foreground_tests.HARDWARE_RENDERER))
        for expected in ("LENOVO 20XW0026US", "Intel(R) Core(TM) i7-1185G7 @ 3.00GHz", "32 GiB",
                         "Windows 10 Enterprise 24H2 (build 26100.4061)", "NVIDIA GeForce RTX 4070",
                         "Built-in Display, 60 Hz, scale 1.5", "Balanced", "conhost.exe 10.0.26100.4061"):
            with self.subTest(expected=expected):
                self.assertIn(expected, text)

    def test_missing_windows_values_read_unavailable(self):
        # A registry value, memory status, power plan or version that could not be read is unavailable, never guessed.
        outputs, _argvs = self.windows_outputs({}, powercfg_exit=1, memory=None, conhost=None)
        text = "\n".join(perf_host_info.host_block_windows(outputs, None, None))
        self.assertIn("unavailable", text)
        self.assertNotIn("None", text)

    def test_adapter_lines_split_at_the_known_keys(self):
        # Adapter names and drivers hold spaces, so a line is split at the App's field names, not at spaces.
        measured = ("2026-10-02T11:24:16.123456Z  INFO sonicterm_gpu::recovery_context: wgpu adapter selected "
                    "backend=Dx12 name=Microsoft Basic Render Driver driver=10.0.26100.9278 device_type=Cpu "
                    "software_rendering=true device_memory_policy=MemoryUsage")
        self.assertEqual(perf_font_diagnostics.parse_adapter_line(measured), {
            "event": "selected", "backend": "Dx12", "name": "Microsoft Basic Render Driver",
            "driver": "10.0.26100.9278", "device_type": "Cpu", "software_rendering": True})
        hardware = ("2026-10-02T11:24:17Z  INFO sonicterm_gpu::core: wgpu adapter reused backend=Dx12 "
                    "name=NVIDIA GeForce RTX 4070 driver=NVIDIA 560.94 device_type=DiscreteGpu "
                    "software_rendering=false")
        self.assertEqual(perf_font_diagnostics.parse_adapter_line(hardware), {
            "event": "reused", "backend": "Dx12", "name": "NVIDIA GeForce RTX 4070", "driver": "NVIDIA 560.94",
            "device_type": "DiscreteGpu", "software_rendering": False})
        self.assertIsNone(perf_font_diagnostics.parse_adapter_line(perf_memory_tests.memory_line()))
        self.assertIsNone(perf_font_diagnostics.parse_adapter_line("wgpu adapter selected backend=Dx12 name=cut off"))


class CliTests(unittest.TestCase):
    def parse(self, *argv):
        with contextlib.redirect_stderr(io.StringIO()):
            return perf_arguments.parse_args(list(argv))

    def test_scenario_values_accumulate_in_order(self):
        # `--scenario all S10/sync` and repeated flags both reach the selection in the order given.
        args = self.parse("--base", "main", "--head", "HEAD", "--scenario", "S10/sync", "all", "--scenario", "S1")
        self.assertEqual(args.scenario, ["S10/sync", "all", "S1"])
        self.assertIsNone(self.parse("--base", "main", "--head", "HEAD").scenario)

    def test_invalid_combinations_are_refused(self):
        # The smoke takes no comparison option, a comparison needs both refs, and runs are positive.
        for argv in (("--smoke", "--base", "main"), ("--smoke", "--runs", "5"), ("--base", "main"), (),
                     ("--base", "main", "--head", "HEAD", "--runs", "0")):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                self.parse(*argv)
        self.assertTrue(self.parse("--smoke").smoke)

    def test_short_is_a_comparison_option_the_smoke_refuses(self):
        # `--short` shortens a comparison's runs; the smoke is always short and takes no comparison option.
        self.assertTrue(self.parse("--base", "main", "--head", "HEAD", "--short").short)
        self.assertFalse(self.parse("--base", "main", "--head", "HEAD").short)
        with self.assertRaises(SystemExit):
            self.parse("--smoke", "--short")

    def test_help_exits_zero(self):
        # `--help` is the one invocation that may run anywhere without building anything.
        output = io.StringIO()
        with contextlib.redirect_stdout(output), self.assertRaises(SystemExit) as raised:
            perf_arguments.parse_args(["--help"])
        self.assertEqual(raised.exception.code, 0)
        self.assertIn("--smoke", output.getvalue())

    def test_main_routes_the_smoke_and_blocks_comparisons_off_macos(self):
        # The smoke goes to its driver; a comparison on another host is BLOCKED, never a pass.
        with mock.patch.object(perf_smoke, "smoke_main", return_value=7) as smoke:
            self.assertEqual(perf_main.main(["--smoke"]), 7)
        smoke.assert_called_once()
        with mock.patch.object(perf.sys, "platform", "linux"), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(perf_main.main(["--base", "main", "--head", "HEAD"]), 3)

    def test_a_legacy_console_encoding_still_prints_the_counters_table(self):
        # Windows runners give Python a cp1252 console, which cannot encode the `≤` of a histogram bucket bound.
        # After use_utf8_output, printing the table succeeds and the bytes are UTF-8.
        script = ("import importlib.util, sys\n"
                  f"sys.path.insert(0, {str(Path(perf_test_fixture.SPEC.origin).parent)!r})\n"
                  f"spec = importlib.util.spec_from_file_location('perf_compare', {str(Path(perf_test_fixture.SPEC.origin).with_name('perf_compare.py'))!r})\n"
                  "module = importlib.util.module_from_spec(spec)\n"
                  "sys.modules[spec.name] = module\n"
                  "spec.loader.exec_module(module)\n"
                  "module.use_utf8_output()\n"
                  "print('p95 ≤5000 us', flush=True)\n")
        environment = {**os.environ, "PYTHONIOENCODING": "cp1252", "PYTHONUTF8": "0"}
        completed = subprocess.run([sys.executable, "-c", script], capture_output=True, env=environment, check=False)
        self.assertEqual(completed.returncode, 0, completed.stderr.decode("utf-8", "replace"))
        self.assertIn("p95 ≤5000 us", completed.stdout.decode("utf-8"))

    def test_main_switches_the_console_to_utf8_before_anything_prints(self):
        # Every path through main prints; the switch must come first, so no message can hit the legacy encoding.
        calls = []
        with mock.patch.object(perf_main, "use_utf8_output", side_effect=lambda: calls.append("utf8")), \
                mock.patch.object(perf_smoke, "smoke_main", side_effect=lambda _environ: calls.append("smoke") or 0):
            self.assertEqual(perf_main.main(["--smoke"]), 0)
        self.assertEqual(calls, ["utf8", "smoke"])

    def test_comparisons_are_not_blocked_on_windows(self):
        # On Windows a comparison reaches the gate checks instead of reporting BLOCKED.
        gate = SimpleNamespace(sigchld_problem=lambda: "stop here", leader_watches=lambda: ())
        with mock.patch.object(perf.sys, "platform", "win32"), mock.patch.object(perf_settings, "load_gate", return_value=gate), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(perf_main.main(["--base", "main", "--head", "HEAD"]), perf_settings.EXIT_FAIL)


RUSTC_VV = ("rustc 1.99.0 (abcdef 2026-09-01)\nbinary: rustc\ncommit-hash: abcdef\n"
            "host: aarch64-apple-darwin\nrelease: 1.99.0\nLLVM version: 21.1.0\n")
