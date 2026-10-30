"""Gate preparations tests."""

from __future__ import annotations
from gate_test_fixture import gate
import dataclasses
import io
from pathlib import Path
import tempfile
import time
import types
import unittest
from unittest import mock

import gate_catalog
import gate_execution
import gate_files
import gate_model
import gate_preparations
import gate_process
import gate_report
import gate_test_fixture


class WindowsPreparationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "scripts").mkdir()
        for name in ("check-workspace-crates.sh", "pty-backend-feasibility.sh"):
            (self.root / "scripts" / name).write_text((gate_test_fixture.ROOT / "scripts" / name).read_text(encoding="utf-8"), encoding="utf-8")
        self.calls = []

    @staticmethod
    def outcome(exit_code=0, *, cleaned=False, errors=(), interrupted=False, timed_out=False, launch_failed=False):
        custody = {"before_cleanup": {"active_processes": int(cleaned), "total_processes": 2},
                   "after_cleanup": {"active_processes": 0, "total_processes": 2},
                   "cleanup": "terminated" if cleaned else "none", "empty": True,
                   "bootstrap_reaped": True, "protocol_complete": True, "capture_complete": True,
                   "errors": list(errors)}
        return {"exit_code": exit_code, "interrupted": interrupted, "timed_out": timed_out,
                "launch_failed": launch_failed, "natural": not cleaned, "errors": list(errors), "custody": custody}

    def run_step(self, name, outcomes=None, *, mutate=None, limit=None):
        step = next(step for step in gate_catalog.STEPS if step.id == name)
        outcomes = iter(outcomes or [self.outcome()] * 8)
        def execute(command, **kwargs):
            self.calls.append((tuple(command), dict(kwargs)))
            if mutate:
                mutate(command, kwargs)
            return next(outcomes)
        with (mock.patch.object(gate_model, "WINDOWS_JOB", types.SimpleNamespace(run=execute)),
              mock.patch.object(gate_process, "resolve_program", side_effect=lambda program, root, env: program)):
            result = gate_execution.run_step(step, 1, self.root, self.root, {"PATH": "fixture"}, output_limit_bytes=limit)
        return result

    def test_preparation_cleanup_precedes_original_strict_execution(self):
        # Cleanup is allowed only for the compile phase; the original selector is still executed by Cargo once.
        step = next(step for step in gate_catalog.STEPS if step.id == "pty-close-baseline")
        result = self.run_step(step.id, [self.outcome(cleaned=True), self.outcome()])
        boundary = step.argv.index("--")
        expected = step.argv[:boundary] + ("--no-run",) + step.argv[boundary:]
        self.assertEqual([call[0] for call in self.calls], [expected, step.argv])
        self.assertEqual(result.status, gate_model.CLEANED_NOT_NATURAL)
        self.assertEqual(result.exit_code, 0)
        self.assertTrue(result.accepted)

    def test_scenario_variants_prepare_exact_selection_then_run_strictly(self):
        # Each plain harness variant prepares its exact feature set; execution keeps the original command and shared deadline.
        selections = {
            "perf-scenarios-tests": (),
            "perf-scenarios-counters-tests": ("--features", "perf-counters,perf-hook-checkpoint-memory,perf-hook-trim"),
            "perf-scenarios-frame-texture-tests": ("--features", "perf-frame-texture"),
            "perf-scenarios-echo-trace-tests": ("--features", "perf-echo-trace"),
        }
        for name, features in selections.items():
            with self.subTest(step=name):
                original = ("cargo", "test", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios", *features)
                step = next(item for item in gate_catalog.STEPS if item.id == name)
                self.assertEqual(step.argv, original)
                self.assertEqual(gate_preparations.windows_preparations(step, self.root, {}),
                                 (gate_model.Preparation((*original, "--no-run")),))
                self.calls.clear()
                result = self.run_step(name, [self.outcome(cleaned=True), self.outcome()])
                self.assertEqual([call[0] for call in self.calls], [(*original, "--no-run"), original])
                self.assertEqual(result.status, gate_model.CLEANED_NOT_NATURAL)
                self.assertTrue(result.accepted)
                self.assertEqual([phase.policy for phase in result.phases],
                                 [gate_model.WindowsPolicy.COMPILE_ONLY, gate_model.WindowsPolicy.STRICT])
                self.assertEqual(self.calls[0][1]["deadline"], self.calls[1][1]["deadline"])
                self.calls.clear()
                result = self.run_step(name, [self.outcome(), self.outcome(cleaned=True)])
                self.assertEqual(result.status, gate_model.FAIL)
                self.assertFalse(result.accepted)

    def test_scenario_preparation_rejects_copies_changed_records_and_unlisted_steps(self):
        # Authorization belongs to each canonical derived-test record, never a matching name or altered preparation.
        original = next(step for step in gate_catalog.STEPS if step.id == "perf-scenarios-tests")
        with self.assertRaisesRegex(ValueError, "synthetic"):
            gate_preparations.windows_preparations(dataclasses.replace(original), self.root, {})
        for changed in (dataclasses.replace(original, windows_preparations=()),
                        dataclasses.replace(original, windows_preparations=(gate_model.Preparation(("cargo", "check")),)),
                        dataclasses.replace(original, id="unreviewed-scenario", windows_preparations=(gate_model.Preparation(),))):
            with self.subTest(step=changed.id, preparations=changed.windows_preparations):
                with mock.patch.object(gate_catalog, "STEPS", (changed,)):
                    with self.assertRaisesRegex(ValueError, "changed|unreviewed"):
                        gate_preparations.windows_preparations(changed, self.root, {})
        for name in ("perf-scenarios-harness-api-tests", "perf-scenarios-harness-api-echo-trace-tests", "doctests"):
            step = next(item for item in gate_catalog.STEPS if item.id == name)
            self.assertEqual(gate_preparations.windows_preparations(step, self.root, {}), ())

    def test_script_feature_drift_refuses_all_launches(self):
        # A changed script selection must not execute stale preparation records or the original script.
        path = self.root / "scripts/check-workspace-crates.sh"
        path.write_text(path.read_text(encoding="utf-8").replace("--features serde", "--features different", 1), encoding="utf-8")
        result = self.run_step("workspace-crates")
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertEqual(self.calls, [])
        self.assertIn("parity", result.detail)


    def test_exact_preparation_selection_and_environment(self):
        # PTY/WARP derive their flags; script phases reproduce target-dir fallback and doc-only flags.
        for name in ("pty-close-baseline", "windows-warp-allocator"):
            step = next(step for step in gate_catalog.STEPS if step.id == name)
            actual = gate_preparations.windows_preparations(step, self.root, {})
            boundary = step.argv.index("--")
            self.assertEqual(actual[0].argv, step.argv[:boundary] + ("--no-run",) + step.argv[boundary:])
        workspace = next(step for step in gate_catalog.STEPS if step.id == "workspace-crates")
        for environment in ({}, {"CARGO_TARGET_DIR": ""}, {"CARGO_TARGET_DIR": "relative target", "RUSTDOCFLAGS": "inherited"}):
            actual = gate_preparations.windows_preparations(workspace, self.root, environment)
            target = environment.get("CARGO_TARGET_DIR") or str(self.root / "target")
            self.assertEqual(len(actual), 3)
            for phase in actual[:2]:
                self.assertEqual(phase.argv[phase.argv.index("--target-dir") + 1], target)
            self.assertEqual(actual[0].env, ())
            self.assertEqual(actual[1].env, (("RUSTDOCFLAGS", "-D warnings -A rustdoc::invalid_html_tags"),))
            self.assertEqual(actual[2].env, ())
            self.assertEqual(actual[0].argv[-1], "--no-run")
            self.assertEqual(actual[1].argv[1], "doc")
            self.assertEqual(actual[2].argv, ("cargo", "test", "--workspace", "--lib", "--bins", "--tests", "--no-fail-fast", "--no-run"))
        feasibility = next(step for step in gate_catalog.STEPS if step.id == "pty-feasibility")
        self.assertEqual(gate_preparations.windows_preparations(feasibility, self.root, {})[0].argv,
                         ("cargo", "build", "--quiet", "-p", "sonicterm-io", "--example", "pty_backend_feasibility_evidence"))
        self.assertEqual(next(step for step in gate_catalog.STEPS if step.id == "doctests").windows_preparations, ())

    def test_crlf_continuations_and_all_script_mutations(self):
        # Known CRLF/continuations are supported; each source/record drift must fail before any launch.
        workspace = self.root / "scripts/check-workspace-crates.sh"
        feasibility = self.root / "scripts/pty-backend-feasibility.sh"
        original = workspace.read_text(encoding="utf-8")
        doc_flags = 'RUSTDOCFLAGS="-D warnings -A rustdoc::invalid_html_tags" '
        workspace.write_bytes(original.replace("\n", "\r\n").encode())
        self.assertEqual(self.run_step("workspace-crates").status, gate_model.PASS)
        self.assertEqual(len(self.calls), 4)
        mutations = (
            original.replace("--features serde", "--features altered", 1),
            original.replace("status=0", "status=0\ncargo check"),
            original.replace("cargo test --workspace --lib --bins --tests --no-fail-fast\n", ""),
            original.replace('${CARGO_TARGET_DIR:-$repo_root/target}', '${CARGO_TARGET_DIR-$repo_root/target}'),
            original.replace('${CARGO_TARGET_DIR:-$repo_root/target}', '${CARGO_TARGET_DIR:-$repo_root/other}'),
            original.replace(doc_flags + 'cargo doc', 'RUSTDOCFLAGS="-A warnings" cargo doc'),
            # Dropping the pinned winit's one lint allowance without changing its record also fails closed.
            original.replace(doc_flags + 'cargo doc', 'RUSTDOCFLAGS="-D warnings" cargo doc'),
            original.replace('status=0', 'status=0\nexport RUSTDOCFLAGS="-D warnings"'),
            original.replace('cargo test --locked', '"cargo" test --locked'),
            original.replace('--target-dir "$winit_target_dir"', '--target-dir $winit_target_dir', 1),
            original.replace(chr(92) + "\n", chr(92) + "\n# interrupted continuation\n", 1),
            original.replace(chr(92) + "\n", chr(92) + "\n\n", 1),
            original.replace(chr(92) + "\n", chr(92) + " \n", 1),
            original.replace(doc_flags + 'cargo doc', 'cargo doc').replace(
                'cargo test --workspace', doc_flags + 'cargo test --workspace'),
            original.replace('cargo test --workspace --lib --bins --tests --no-fail-fast\n', '').replace(
                'status=0', 'status=0\ncargo test --workspace --lib --bins --tests --no-fail-fast'),
        )
        for text in mutations:
            with self.subTest(text=text):
                workspace.write_text(text, encoding="utf-8")
                self.calls.clear()
                result = self.run_step("workspace-crates")
                self.assertEqual(result.status, gate_model.FAIL)
                self.assertEqual(self.calls, [])
                self.assertEqual(result.phases[-1].status, gate_model.NOT_RUN)
        workspace.write_text(original, encoding="utf-8")
        text = feasibility.read_text(encoding="utf-8")
        for mutated in (text.replace("cargo run", "cargo test"), text + "\ncargo check\n",
                        text.replace("2>/dev/null", "2>&1")):
            with self.subTest(feasibility=mutated):
                feasibility.write_text(mutated, encoding="utf-8")
                self.calls.clear()
                self.assertEqual(self.run_step("pty-feasibility").status, gate_model.FAIL)
                self.assertEqual(self.calls, [])
        feasibility.write_text(text, encoding="utf-8")
        canonical = next(step for step in gate_catalog.STEPS if step.id == "workspace-crates")
        changed_record = dataclasses.replace(canonical.windows_preparations[0], argv=("cargo", "check"))
        altered = dataclasses.replace(canonical, windows_preparations=(changed_record, *canonical.windows_preparations[1:]))
        for changed in (altered, dataclasses.replace(canonical, windows_preparations=())):
            with mock.patch.object(gate_catalog, "STEPS", tuple(changed if step is canonical else step for step in gate_catalog.STEPS)):
                self.calls.clear()
                self.assertEqual(self.run_step("workspace-crates").status, gate_model.FAIL)
                self.assertEqual(self.calls, [])

    def test_ordinary_failures_continue_but_unsafe_phases_stop(self):
        # Ordinary compiler exit failure preserves the workspace script's fail-complete coverage.
        result = self.run_step("workspace-crates", [self.outcome(7), self.outcome(), self.outcome(), self.outcome()])
        self.assertEqual(len(self.calls), 4)
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertEqual(result.exit_code, 0)
        self.assertEqual([phase.status for phase in result.phases], [gate_model.FAIL, gate_model.PASS, gate_model.PASS, gate_model.PASS])
        for bad, expected in ((self.outcome(errors=("query failed",)), gate_model.FAIL),
                              (self.outcome(None, launch_failed=True), gate_model.LAUNCH),
                              (self.outcome(None, timed_out=True), gate_model.TIMEOUT),
                              (self.outcome(None, timed_out=True, interrupted=True), gate_model.INTERRUPTED)):
            self.calls.clear()
            result = self.run_step("workspace-crates", [bad])
            self.assertEqual(len(self.calls), 1)
            self.assertEqual(result.status, expected)
            self.assertIsNone(result.exit_code)
            self.assertTrue(all(phase.status == gate_model.NOT_RUN for phase in result.phases[1:]))
        self.calls.clear()
        result = self.run_step("pty-close-baseline", [self.outcome(), self.outcome(cleaned=True)])
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertEqual(result.phases[-1].policy, gate_model.WindowsPolicy.STRICT)

    def test_phase_deadline_output_budget_and_receipts_are_shared(self):
        # Child-output budget does not reset and preparation output goes to the log, not another phase's stdin.
        def output(command, kwargs):
            kwargs["sink"].write(b"abc")
        result = self.run_step("pty-close-baseline", mutate=output, limit=10)
        self.assertEqual([call[1]["output_limit_bytes"] for call in self.calls], [10, 7])
        self.assertEqual(self.calls[0][1]["deadline"], self.calls[1][1]["deadline"])
        self.assertTrue(all(call[1]["cwd"] == self.root for call in self.calls))
        self.assertEqual(self.calls[-1][1]["env"], {"PATH": "fixture"})
        text = result.log_path.read_text(encoding="utf-8")
        self.assertIn("phase=preparation-1", text)
        self.assertIn("phase=execution", text)
        report = gate_report.GateReport("windows", (result,), gate_files.GitSnapshot(False), gate_files.GitSnapshot(False), (), self.root)
        summary = gate_report.summary_json(report, [next(step for step in gate_catalog.STEPS if step.id == "pty-close-baseline")])
        self.assertEqual(len(summary["steps"][0]["phases"]), 2)
        self.assertEqual(summary["steps"][0]["phases"][1]["argv"], list(self.calls[-1][0]))
        self.assertIn("preparation-1", "\n".join(gate_report.summary_lines(report)))
        self.calls.clear()
        clock = [100.0]
        with mock.patch.object(gate.time, "monotonic", side_effect=lambda: clock[0]):
            result = self.run_step("pty-close-baseline", mutate=lambda command, kwargs: clock.__setitem__(0, kwargs["deadline"] + 1))
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(result.status, gate_model.TIMEOUT)
        self.assertEqual(result.phases[-1].status, gate_model.NOT_RUN)

    def test_completed_preparation_record_survives_result_log_failure(self):
        # Persist an observed preparation exit even if writing its log footer fails; never run execution afterward.
        class FailedFooter(io.BytesIO):
            def write(self, data):
                if b"phase=preparation-1 result=" in data:
                    raise OSError("phase footer refused")
                return super().write(data)
        step = next(step for step in gate_catalog.STEPS if step.id == "pty-close-baseline")
        execute = mock.Mock(return_value=self.outcome(7))
        with (mock.patch.object(gate_model, "WINDOWS_JOB", types.SimpleNamespace(run=execute)),
              mock.patch.object(gate_process, "resolve_program", return_value="cargo")):
            result = gate_execution._run_windows_step(step, self.root, {}, FailedFooter(), self.root / "log",
                                            time.monotonic(), None)
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertEqual(result.phases[0].exit_code, 7)
        self.assertIsNotNone(result.phases[0].custody)
        self.assertEqual(result.phases[-1].status, gate_model.NOT_RUN)
        self.assertEqual(execute.call_count, 1)

    def test_result_log_failure_preserves_timeout_precedence(self):
        # A reporting fault cannot replace an already observed deadline with a lower-priority generic failure.
        class FailedFooter(io.BytesIO):
            def write(self, data):
                if b"phase=preparation-1 result=" in data:
                    raise OSError("phase footer refused")
                return super().write(data)
        step = next(step for step in gate_catalog.STEPS if step.id == "pty-close-baseline")
        execute = mock.Mock(return_value=self.outcome(None, timed_out=True))
        with (mock.patch.object(gate_model, "WINDOWS_JOB", types.SimpleNamespace(run=execute)),
              mock.patch.object(gate_process, "resolve_program", return_value="cargo")):
            result = gate_execution._run_windows_step(step, self.root, {}, FailedFooter(), self.root / "log",
                                            time.monotonic(), None)
        self.assertEqual(result.status, gate_model.TIMEOUT)
        self.assertEqual(result.phases[0].status, gate_model.TIMEOUT)
        self.assertEqual(result.phases[-1].status, gate_model.NOT_RUN)

    def test_parity_failure_records_every_planned_phase_as_not_run(self):
        # A rejected preparation plan must be distinguishable from a plan that never existed.
        path = self.root / "scripts/check-workspace-crates.sh"
        path.write_text(path.read_text(encoding="utf-8").replace("--features serde", "--features changed", 1), encoding="utf-8")
        result = self.run_step("workspace-crates")
        self.assertEqual(self.calls, [])
        self.assertEqual([phase.name for phase in result.phases],
                         ["preparation-1", "preparation-2", "preparation-3", "execution"])
        self.assertTrue(all(phase.status == gate_model.NOT_RUN for phase in result.phases))
        self.assertTrue(all(phase.detail for phase in result.phases))

    def test_prepared_execution_cannot_inherit_compile_cleanup_policy(self):
        # Preparation authorization never grants forced-cleanup acceptance to the original mixed command.
        original = next(step for step in gate_catalog.STEPS if step.id == "pty-close-baseline")
        changed = dataclasses.replace(original, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY)
        with mock.patch.object(gate_catalog, "STEPS", tuple(changed if step is original else step for step in gate_catalog.STEPS)):
            result = self.run_step("pty-close-baseline", [self.outcome(), self.outcome(cleaned=True)])
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertEqual(result.phases[-1].policy, gate_model.WindowsPolicy.STRICT)

    def test_perf_builds_are_compile_only_cargo_builds_outside_the_table(self):
        # perf-compare.py's builds only compile, so each may accept forced cleanup, and none runs in the gate.
        self.assertEqual(set(gate_preparations.PERF_BUILDS), {"build-perf_scenarios", "build-head-perf_scenarios",
                                                 "build-base-perf_scenarios", "build-head-perf_scenarios_alloc",
                                                 "build-base-perf_scenarios_alloc"})
        for step_id, step in gate_preparations.PERF_BUILDS.items():
            self.assertEqual(step.id, step_id)
            self.assertEqual(step.argv[:3], ("cargo", "build", "--locked"))
            self.assertEqual(step.windows_policy, gate_model.WindowsPolicy.COMPILE_ONLY)
            self.assertNotIn(step, gate_catalog.STEPS)

    def test_a_perf_build_whose_compiler_helper_was_cleaned_is_accepted_only_as_the_gate_s_own_step(self):
        # MSVC's linker can leave vctip.exe alive after Cargo exits 0: the reviewed step is accepted as
        # CLEANED_NOT_NATURAL, and a copy of it fails without launching.
        step = gate_preparations.PERF_BUILDS["build-head-perf_scenarios"]
        execute = mock.Mock(return_value=self.outcome(cleaned=True))
        with (mock.patch.object(gate_model, "WINDOWS_JOB", types.SimpleNamespace(run=execute)),
              mock.patch.object(gate_process, "resolve_program", side_effect=lambda program, root, env: program)):
            result = gate_execution.run_step(step, 1, self.root, self.root, {})
            self.assertEqual(result.status, gate_model.CLEANED_NOT_NATURAL)
            self.assertEqual(result.exit_code, 0)
            self.assertTrue(result.accepted)
            execute.reset_mock()
            copied = gate_execution.run_step(dataclasses.replace(step), 2, self.root, self.root, {})
        self.assertEqual(copied.status, gate_model.FAIL)
        execute.assert_not_called()

    def test_synthetic_step_cannot_borrow_preparation_or_cleanup_authority(self):
        # Canonical identity, not just an ID copied onto another command, authorizes cleanup.
        real = next(step for step in gate_catalog.STEPS if step.id == "pty-close-baseline")
        synthetic = dataclasses.replace(real)
        execute = mock.Mock(return_value=self.outcome(cleaned=True))
        with (mock.patch.object(gate_model, "WINDOWS_JOB", types.SimpleNamespace(run=execute)),
              mock.patch.object(gate_process, "resolve_program", return_value="fake")):
            result = gate_execution.run_step(synthetic, 1, self.root, self.root, {})
        self.assertEqual(result.status, gate_model.FAIL)
        execute.assert_not_called()
        self.calls.clear()
        step = gate_test_fixture.python_step("clippy", "pass", windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY)
        with (mock.patch.object(gate_model, "WINDOWS_JOB", types.SimpleNamespace(run=execute)),
              mock.patch.object(gate_process, "resolve_program", return_value="fake")):
            result = gate_execution.run_step(step, 1, self.root, self.root, {})
        self.assertEqual(result.status, gate_model.FAIL)
        execute.assert_not_called()
