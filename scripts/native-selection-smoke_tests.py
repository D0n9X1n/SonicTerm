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
from types import SimpleNamespace
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
            lines.append(f"PASS native surface retry child={child} topology={topology} baseline_frames=1 resumed_frames=2 recovery_events=0")
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

    def test_each_case_requires_one_surface_retry_recovery(self):
        # Selection alone cannot hide an absent or duplicated forced-retry recovery.
        output = valid_output()
        recovery = next(line for line in output.splitlines() if line.startswith("PASS native surface retry "))
        self.assertTrue(self.verdict(output.replace(recovery + "\n", "", 1)))
        self.assertTrue(self.verdict(output.replace(recovery + "\n", recovery + "\n" + recovery + "\n", 1)))
        self.assertTrue(self.verdict("\n".join(line for line in output.splitlines()
                                               if not line.startswith("PASS native surface retry "))))

    def test_surface_retry_requires_a_prior_frame_and_later_presentation(self):
        # A marker must prove progress beyond a nonzero same-window baseline.
        for old, new in (("baseline_frames=1", "baseline_frames=0"),
                         ("resumed_frames=2", "resumed_frames=1"),
                         ("resumed_frames=2", "resumed_frames=0"),
                         ("resumed_frames=2", "resumed_frames=unknown"),
                         ("recovery_events=0", "recovery_events=1"),
                         ("recovery_events=0", "recovery_events=unknown")):
            with self.subTest(new=new):
                self.assertTrue(self.verdict(valid_output().replace(old, new, 1)))

    def test_surface_retry_must_precede_its_own_case_pass(self):
        # Another case's recovery and a late marker cannot establish this case's readiness.
        lines = valid_output().splitlines()
        recovery = lines.pop(1)
        lines.insert(2, recovery)
        self.assertTrue(self.verdict("\n".join(lines)))
        self.assertTrue(self.verdict(valid_output().replace(
            "PASS native surface retry child=false topology=Horizontal",
            "PASS native surface retry child=true topology=Horizontal", 1)))

    def test_wrong_or_malformed_case_fails(self):
        for old, new in (("topology=Nested", "topology=Unknown"),
                         ("press_pane=1", "press_pane=garbled"),
                         ("foreign_pane=2", "foreign_pane=1")):
            with self.subTest(new=new):
                lines = valid_output().splitlines()
                index = next(index for index, line in enumerate(lines)
                             if line.startswith("PASS native selection ") and old in line)
                lines[index] = lines[index].replace(old, new, 1)
                self.assertTrue(self.verdict("\n".join(lines)))

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


class RoutingTests(unittest.TestCase):
    def test_macos_fixture_forwards_app_deadlines_and_does_not_spin_redraw(self):
        # Backend-only surface recovery belongs to App's callbacks, not blind fixture redraw requests.
        source = (smoke.ROOT / "crates/sonicterm-app/tests/native_split_selection/mod.rs").read_text(encoding="utf-8")
        handler = source.split("impl ApplicationHandler for Probe {", 1)[1]
        self.assertIn("ApplicationHandler::new_events(&mut case.app, event_loop, cause);", handler)
        self.assertIn("ApplicationHandler::about_to_wait(&mut case.app, event_loop);", handler)
        self.assertIn("at.min(case.deadline)", handler)
        self.assertLess(handler.index("ApplicationHandler::about_to_wait(&mut case.app, event_loop);"),
                        handler.index("at.min(case.deadline)"))
        retry = source.split("if self.frames() == before {", 1)[1].split("return Ok(false);", 1)[0]
        self.assertIn('#[cfg(not(target_os = "macos"))]', retry)
        self.assertEqual(retry.count("self.request_frame();"), 1)
        self.assertIn("config.window.warm_window_pool = 0;", source)

    def assert_recovery_event_guard(self, source):
        handler = source.split("    fn window_event(", 1)[1].split("    fn about_to_wait(", 1)[0]
        dispatch = handler.index("        match event {")
        guarded = handler[:dispatch]
        start = guarded.rfind('        #[cfg(target_os = "macos")]')
        self.assertGreaterEqual(start, 0)
        guard = guarded[start:]
        events = " ".join(guard.split())
        self.assertIn(
            "if matches!( event, WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Occluded(_) )",
            events)
        self.assertIn("if let Some(proof) = case.surface_retry.as_mut() {", guard)
        increment = guard.index("proof.recovery_events += 1;")
        failure = guard.index("self.finish(", increment)
        error = guard.index("Err(anyhow::anyhow!(", failure)
        returned = guard.index("return;", error)
        self.assertLess(increment, failure)
        self.assertLess(failure, error)
        self.assertLess(error, returned)
        self.assertIn("native event confounded surface recovery", guard)

    def test_native_events_cannot_rescue_forced_surface_recovery(self):
        # Every confounding event must fail before App can change the recovery state.
        source = (smoke.ROOT / "crates/sonicterm-app/tests/native_split_selection/mod.rs").read_text(encoding="utf-8")
        self.assert_recovery_event_guard(source)

    def test_recovery_guard_contract_rejects_removed_or_nonterminal_guard(self):
        # Mutations prove the source contract depends on all events, failure, and pre-dispatch placement.
        source = (smoke.ROOT / "crates/sonicterm-app/tests/native_split_selection/mod.rs").read_text(encoding="utf-8")
        start = source.index('        #[cfg(target_os = "macos")]\n        if matches!(', source.index("    fn window_event("))
        end = source.index("        match event {", start)
        guard = source[start:end]
        mutations = [
            "",
            guard.replace("WindowEvent::Resized(_)", "WindowEvent::Focused(_)", 1),
            guard.replace("WindowEvent::ScaleFactorChanged { .. }", "WindowEvent::Focused(_)", 1),
            guard.replace("WindowEvent::Occluded(_)", "WindowEvent::Focused(_)", 1),
            guard.replace("proof.recovery_events += 1;", "", 1),
            guard.replace("self.finish(", "self.ignore(", 1),
            guard.replace("Err(anyhow::anyhow!(", "Ok(anyhow::anyhow!(", 1),
            guard.replace("return;", "", 1),
        ]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                with self.assertRaises((AssertionError, ValueError, IndexError)):
                    self.assert_recovery_event_guard(source[:start] + mutation + source[end:])
        moved = source[:start] + source[end:].replace("        match event {", "        match event {\n" + guard, 1)
        with self.assertRaises((AssertionError, ValueError, IndexError)):
            self.assert_recovery_event_guard(moved)

    def test_native_visibility_reaches_the_mapped_app_owner(self):
        # Native visibility supersedes retained backend state without being required for timed recovery.
        fixture = smoke.ROOT / "crates/sonicterm-app/tests/native_split_selection/mod.rs"
        source = fixture.read_text(encoding="utf-8")
        arm = source.split("WindowEvent::Occluded(occluded) => {", 1)[1].split("\n            }", 1)[0]
        self.assertIn("case.last_occluded = Some(occluded);", arm)
        self.assertEqual(arm.count("dispatch(&mut case.app, event_loop, case.id, WindowEvent::Occluded(occluded));"), 1)


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

    def test_unsupported_host_fails_instead_of_skipping(self):
        with mock.patch.object(smoke.sys, "platform", "win32"):
            self.assertEqual(smoke.main([]), 2)


class CargoLaunchTests(unittest.TestCase):
    def test_cargo_selects_the_current_example_instead_of_a_stale_default(self):
        # Cargo owns environment/config/target layout; a default-path file must never select the executable.
        for overrides in ({}, {"CARGO_TARGET_DIR": "explicit"},
                          {"CARGO_BUILD_TARGET_DIR": "configured"},
                          {"CARGO_BUILD_TARGET": "aarch64-apple-darwin"}):
            with self.subTest(overrides=overrides), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                stale = root / "target/debug/examples/native_split_selection"
                stale.parent.mkdir(parents=True)
                stale.write_text("stale artifact must not be launched", encoding="utf-8")
                config = root / ".cargo/config.toml"
                config.parent.mkdir()
                config.write_text('[build]\ntarget-dir = "from-config"\n', encoding="utf-8")
                log_dir = root / "evidence"
                log_dir.mkdir()
                log_path = log_dir / "native.log"
                log_path.write_text(valid_output(), encoding="utf-8")
                real_mkdtemp = tempfile.mkdtemp

                def make_temp(*args, **kwargs):
                    if kwargs.get("prefix") == "sonicterm-selection-evidence-":
                        return str(log_dir)
                    return real_mkdtemp(*args, **kwargs)

                gate = mock.Mock()
                gate.Step.side_effect = lambda *args: SimpleNamespace(command=args[1])
                gate.run_step.return_value = SimpleNamespace(
                    log_path=log_path, exit_code=0, status="PASS", leftover_processes=0, detail="")
                env = {"HOME": "real-home", **overrides}
                with contextlib.ExitStack() as stack:
                    stack.enter_context(mock.patch.object(smoke, "ROOT", root))
                    stack.enter_context(mock.patch.object(smoke.sys, "platform", "darwin"))
                    stack.enter_context(mock.patch.object(smoke.signal, "SIGCHLD", 17, create=True))
                    stack.enter_context(mock.patch.object(smoke.signal, "getsignal", return_value=0))
                    stack.enter_context(mock.patch.object(smoke, "load_gate", return_value=gate))
                    stack.enter_context(mock.patch.object(smoke.tempfile, "mkdtemp", side_effect=make_temp))
                    stack.enter_context(mock.patch.dict(os.environ, env, clear=True))
                    stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                    self.assertEqual(smoke.main([]), 0)
                args = gate.run_step.call_args.args
                self.assertEqual(args[0].command[:8], (
                    "cargo", "run", "--locked", "-p", "sonicterm-app", "--example",
                    "native_split_selection", "--"))
                self.assertEqual(args[0].command[8], "--run")
                self.assertEqual(args[2], root)
                for key, value in overrides.items():
                    self.assertEqual(args[4][key], value)


@unittest.skipIf(os.name == "nt", "process-group lifetime requires POSIX")
class LifetimeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.gate = smoke.load_gate()

    def test_wrapper_runs_the_example_and_preserves_evidence(self):
        # A Cargo shim proves argument routing and failures without compiling or accepting a stale default artifact.
        for leave_fixture, exit_code, cargo_failure in (
                (False, 0, False), (True, 0, False), (False, 1, False), (False, 0, True)):
            with self.subTest(leave_fixture=leave_fixture, exit_code=exit_code, cargo_failure=cargo_failure), tempfile.TemporaryDirectory() as temporary:
                # Native getcwd may resolve a temporary-directory alias to a different spelling.
                actual = Path(temporary) / "actual"
                actual.mkdir()
                root = Path(temporary) / "alias"
                root.symlink_to(actual, target_is_directory=True)
                binary = root / "redirected/example"
                binary.parent.mkdir()
                stale = root / "target/debug/examples/native_split_selection"
                stale.parent.mkdir(parents=True)
                stale.write_text(f"#!{sys.executable}\nraise RuntimeError('stale artifact ran')\n", encoding="utf-8")
                stale.chmod(0o700)
                cargo = root / "bin/cargo"
                cargo.parent.mkdir()
                cargo.write_text(
                    f"#!{sys.executable}\nimport os,sys\n"
                    "assert sys.argv[1:8] == ['run', '--locked', '-p', 'sonicterm-app', '--example', 'native_split_selection', '--']\n"
                    f"assert os.path.samefile(os.getcwd(), {str(root)!r})\n"
                    "assert os.environ['CARGO_BUILD_TARGET_DIR'] == 'redirected'\n"
                    "print('    Finished dev profile; running current example', file=sys.stderr)\n"
                    + ("sys.exit(101)\n" if cargo_failure else
                       f"os.execv(sys.executable, [sys.executable, {str(binary)!r}, *sys.argv[8:]])\n"),
                    encoding="utf-8")
                cargo.chmod(0o700)
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
                    stack.enter_context(mock.patch.object(smoke, "ROOT", root))
                    stack.enter_context(mock.patch.object(smoke.tempfile, "mkdtemp", side_effect=make_temp))
                    stack.enter_context(mock.patch.dict(os.environ, {
                        "HOME": "/fixture-real-home", "NO_COLOR": "1", "GITHUB_ENV": str(env_file),
                        "PATH": str(cargo.parent) + os.pathsep + os.environ.get("PATH", ""),
                        "CARGO_BUILD_TARGET_DIR": "redirected"}))
                    stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                    stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                    result = smoke.main([])
                self.assertEqual(result, 0 if not leave_fixture and exit_code == 0 and not cargo_failure else 1)
                report = json.loads((evidence / "result.json").read_text())
                self.assertEqual(report["status"], "PASS" if result == 0 else "FAIL")
                self.assertTrue(Path(report["log"]).is_file())
                self.assertEqual(report["command"][0:2], ["cargo", "run"])
                self.assertEqual(report["exit_code"], 101 if cargo_failure else exit_code)
                self.assertNotIn("stale artifact ran", Path(report["log"]).read_text())
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
