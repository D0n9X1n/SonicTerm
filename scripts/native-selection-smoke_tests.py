#!/usr/bin/env python3
"""Native selection verdict, environment and process-lifetime contracts."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "native_selection_smoke", Path(__file__).with_name("native-selection-smoke.py")
)
assert SPEC and SPEC.loader
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


def valid_output() -> str:
    lines = []
    for child in ("false", "true"):
        for topology in ("Horizontal", "Vertical", "Nested"):
            lines.append('INFO sonicterm_gpu::core: wgpu adapter selected backend=Metal name=Paravirtual device_type=Other software_rendering=false')
            lines.append(f"PASS native selection child={child} topology={topology} press_pane=1 foreign_pane=2")
    lines.append(smoke.FINAL_PASS)
    return "\n".join(lines) + "\n"


class VerdictTests(unittest.TestCase):
    def verdict(self, text=None, **kwargs):
        return smoke.verdict_problems(valid_output() if text is None else text, **kwargs)

    def test_exact_matrix_and_hardware_metal_pass(self):
        self.assertEqual(self.verdict(), [])
        self.assertEqual(self.verdict(valid_output().replace("INFO", "\x1b[32mINFO\x1b[0m")), [])

    def test_empty_missing_and_duplicate_cases_fail(self):
        self.assertTrue(self.verdict(""))
        lines = valid_output().splitlines()
        missing = "\n".join(line for line in lines if "child=true topology=Nested" not in line)
        duplicate = valid_output() + next(line for line in lines if line.startswith("PASS native selection")) + "\n"
        self.assertTrue(self.verdict(missing))
        self.assertTrue(self.verdict(duplicate))

    def test_wrong_or_malformed_case_fails(self):
        for old, new in (("topology=Nested", "topology=Unknown"),
                         ("press_pane=1", "press_pane=garbled"),
                         ("foreign_pane=2", "foreign_pane=1")):
            with self.subTest(new=new):
                self.assertTrue(self.verdict(valid_output().replace(old, new, 1)))

    def test_final_pass_must_be_unique_and_last_case_must_precede_it(self):
        self.assertTrue(self.verdict(valid_output().replace(smoke.FINAL_PASS, "")))
        self.assertTrue(self.verdict(valid_output() + smoke.FINAL_PASS + "\n"))
        self.assertTrue(self.verdict(smoke.FINAL_PASS + "\n" + valid_output().replace(smoke.FINAL_PASS, "")))

    def test_nonzero_exit_and_launcher_failure_cannot_pass(self):
        for code in (1, 124, -6):
            self.assertTrue(self.verdict(exit_code=code))
        self.assertTrue(self.verdict(step_status="FAIL"))
        self.assertTrue(self.verdict(step_status="TIMEOUT"))
        self.assertTrue(self.verdict(leftover_processes=1))
        self.assertTrue(self.verdict(leftover_processes=None))

    def test_skips_blockers_panics_and_cleanup_warnings_fail(self):
        for line in ("NOT_EXERCISED", "HOST_INCAPABLE", "BLOCKED: no frame", "FAIL: assertion",
                     "thread main panicked at assertion", "native selection scratch cleanup failed: busy"):
            with self.subTest(line=line):
                self.assertTrue(self.verdict(valid_output() + line + "\n"))
        self.assertTrue(self.verdict(fixture_exists=True))

    def test_missing_or_nonmetal_or_software_adapter_fails(self):
        for old, new in (("backend=Metal", "backend=Vulkan"),
                         ("backend=Metal", ""),
                         ("device_type=Other", "device_type=Cpu"),
                         ("device_type=Other", ""),
                         ("software_rendering=false", "software_rendering=true")):
            with self.subTest(new=new):
                self.assertTrue(self.verdict(valid_output().replace(old, new)))
        lines = valid_output().splitlines()
        self.assertTrue(self.verdict("\n".join(lines[1:])))
        self.assertTrue(self.verdict(valid_output() + lines[0] + "\n"))

    def test_log_limit_is_fail_closed(self):
        self.assertTrue(self.verdict(overflow=True))

    def test_real_windows_adapter_record_is_not_metal_acceptance(self):
        # A completed Windows matrix must not satisfy the separate Metal execution gate.
        output = valid_output().replace("backend=Metal", "backend=Dx12").replace(
            "device_type=Other", "device_type=Cpu")
        self.assertTrue(self.verdict(output))


class EnvironmentTests(unittest.TestCase):
    def test_real_home_and_temp_are_preserved_but_no_color_is_removed(self):
        source = {"HOME": "/real/home", "TMPDIR": "/actual/tmp", "NO_COLOR": "1",
                  "WGPU_BACKEND": "vulkan", "RUST_LOG": "off"}
        with mock.patch.object(smoke.tempfile, "gettempdir", return_value="/actual/tmp"):
            actual = smoke.probe_environment(source)
        self.assertEqual(actual["HOME"], source["HOME"])
        self.assertEqual(actual["TMPDIR"], source["TMPDIR"])
        self.assertNotIn("NO_COLOR", actual)
        self.assertEqual(actual["WGPU_BACKEND"], "metal")
        self.assertEqual(actual["RUST_LOG"], "warn,sonicterm_gpu::core=info")
        self.assertIn("NO_COLOR", source)

    def test_rust_receives_the_same_temp_root_as_python(self):
        # Rust and Python must not choose different platform fallbacks when TMPDIR is unset.
        with mock.patch.object(smoke.tempfile, "gettempdir", return_value="/selected/tmp"):
            for source in ({"HOME": "/real/home"}, {"TMPDIR": "/unusable/path"}):
                self.assertEqual(smoke.probe_environment(source)["TMPDIR"], "/selected/tmp")

    def test_binary_uses_explicit_cargo_target_directory(self):
        root = Path("/repo")
        self.assertEqual(smoke.example_path(root, {}), root / "target/debug/examples/native_split_selection")
        self.assertEqual(smoke.example_path(root, {"CARGO_TARGET_DIR": "custom"}), root / "custom/debug/examples/native_split_selection")
        self.assertEqual(smoke.example_path(root, {"CARGO_TARGET_DIR": "/external"}), Path("/external/debug/examples/native_split_selection"))

    def test_unsupported_host_fails_instead_of_skipping(self):
        with mock.patch.object(smoke.sys, "platform", "win32"):
            self.assertEqual(smoke.main([]), 2)


@unittest.skipIf(os.name == "nt", "process-group lifetime requires POSIX")
class LifetimeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.gate = smoke.load_gate()

    def test_wrapper_runs_the_example_and_preserves_evidence(self):
        # Real subprocesses prove main passes --run, enforces exit/cleanup, and keeps evidence outside the fixture.
        for leave_fixture, exit_code in ((False, 0), (True, 0), (False, 1)):
            with self.subTest(leave_fixture=leave_fixture, exit_code=exit_code), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                binary = root / "probe"
                evidence = root / "evidence"
                evidence.mkdir()
                env_file = root / "github-env"
                code = (
                    f"#!{sys.executable}\n"
                    "import os, pathlib, shutil, sys\n"
                    "assert sys.argv[1] == '--run'\n"
                    "fixture = pathlib.Path(sys.argv[2])\n"
                    "assert fixture.is_absolute() and not fixture.exists()\n"
                    "assert 'NO_COLOR' not in os.environ\n"
                    "assert os.environ['HOME'] == '/fixture-real-home'\n"
                    "assert os.environ['WGPU_BACKEND'] == 'metal'\n"
                    "fixture.mkdir()\n"
                    + ("shutil.rmtree(fixture)\n" if not leave_fixture else "")
                    + f"print({valid_output()!r}, end='')\n"
                    + f"sys.exit({exit_code})\n"
                )
                binary.write_text(code, encoding="utf-8")
                binary.chmod(0o700)
                real_mkdtemp = tempfile.mkdtemp

                def make_temp(*args, **kwargs):
                    if kwargs.get("prefix") == "sonicterm-selection-evidence-":
                        return str(evidence)
                    return real_mkdtemp(*args, **kwargs)

                with contextlib.ExitStack() as stack:
                    stack.enter_context(mock.patch.object(smoke.sys, "platform", "darwin"))
                    stack.enter_context(mock.patch.object(smoke, "load_gate", return_value=self.gate))
                    stack.enter_context(mock.patch.object(smoke, "example_path", return_value=binary))
                    stack.enter_context(mock.patch.object(smoke.tempfile, "mkdtemp", side_effect=make_temp))
                    stack.enter_context(mock.patch.dict(os.environ, {
                        "HOME": "/fixture-real-home", "NO_COLOR": "1", "GITHUB_ENV": str(env_file)}))
                    stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                    stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                    result = smoke.main([])
                self.assertEqual(result, 0 if not leave_fixture and exit_code == 0 else 1)
                report = json.loads((evidence / "result.json").read_text())
                self.assertEqual(report["status"], "PASS" if result == 0 else "FAIL")
                self.assertTrue(Path(report["log"]).is_file())
                self.assertIn(f"SONICTERM_SELECTION_LOG_DIR={evidence}", env_file.read_text())

    def test_survivor_with_or_without_output_pipe_fails(self):
        # A normally exited leader cannot hide a same-group child by closing its output.
        for redirected in (False, True):
            with self.subTest(redirected=redirected), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                pidfile = root / "child.pid"
                code = (
                    "import pathlib, subprocess, sys; "
                    "p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']"
                    + (",stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL" if redirected else "")
                    + "); import os; pathlib.Path(sys.argv[1]).write_text(str(os.getpgrp()))"
                )
                step = self.gate.Step("fixture", (sys.executable, "-c", code, str(pidfile)),
                                      ("macos", "linux"), 10, "local", (), ())
                result = self.gate.run_step(step, 1, root, root, os.environ)
                self.assertEqual(result.status, "FAIL")
                self.assertEqual(result.leftover_processes, 1)
                pid = int(pidfile.read_text())
                members = self.gate._group_members(pid)
                self.assertEqual(members, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
