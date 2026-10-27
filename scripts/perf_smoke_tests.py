"""Perf smoke tests."""

from __future__ import annotations
from perf_test_fixture import perf
import contextlib
import io
import json
import re
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import perf_build_policy
import perf_build_tests
import perf_focus
import perf_foreground_tests
import perf_memory_tests
import perf_outcomes
import perf_outcomes_tests
import perf_result_tests
import perf_scenarios
import perf_sessions
import perf_settings
import perf_smoke
import perf_watcher_tests
import perf_windows_tests


def outcome_of(kind):
    """Build a factory that answers a smoke plan with an outcome of the named kind."""
    def build(plan):
        killed = {"sent": True, "problem": None, "pid": perf_watcher_tests.HARNESS_PID}
        occlusion = perf_result_tests.valid_result(status="invalid", exit_code=3, notes=["native occlusion change"])
        if kind == "valid" and plan.kill_at_go:
            return perf_outcomes_tests.make_outcome(plan=plan, status="FAIL", exit_code=-9, result=None, deadline=killed)
        if kind == "valid" and plan.variant in ("gdi", "wgpu"):
            # A presenter variant is valid only with its presenter record and a logged adapter, as on Windows.
            presenter = dict(perf_foreground_tests.WGPU_PRESENTER, windows_gdi=plan.variant == "gdi")
            return perf_outcomes_tests.make_outcome(plan=plan, platform="win32", renderer=perf_foreground_tests.HARDWARE_RENDERER,
                                result=perf_result_tests.valid_result(presenter=presenter))
        if kind == "valid":
            return perf_outcomes_tests.make_outcome(plan=plan)
        if kind == "occluded":
            return perf_outcomes_tests.make_outcome(plan=plan, exit_code=3, result=occlusion)
        if kind == "startup":
            return perf_outcomes_tests.make_outcome(plan=plan, exit_code=3, result=perf_result_tests.valid_result(
                status="invalid", exit_code=3, invalid_reason=perf_watcher_tests.STARTUP_OCCLUSION_REASON))
        if kind == "blocked":
            return perf_outcomes_tests.make_outcome(plan=plan, exit_code=5, result=None)
        if kind == "schema":
            return perf_outcomes_tests.make_outcome(plan=plan, schema_problems=["managed is False, not true"])
        if kind == "theft":
            return perf_outcomes_tests.make_outcome(plan=plan, focus=perf_focus.FocusVerdict(True, (), ["focus theft"], True))
        if kind == "home":
            return perf_outcomes_tests.make_outcome(plan=plan, home=["config.toml: added"])
        if kind == "survivor":
            cleanup = perf_sessions.CleanupResult()
            cleanup.unresolved("members left", [perf_sessions.ProcessInfo(510, 500, 500, "11", 1001.0, "sleep")])
            return perf_outcomes_tests.make_outcome(plan=plan, cleanup=cleanup)
        if kind == "unmanaged theft":
            # An unmanaged exit-4 result that also reports focus theft, as the review reproduced it.
            unmanaged = perf_result_tests.valid_result(status="timeout", exit_code=4, managed=False)
            return perf_outcomes_tests.make_outcome(plan=plan, status="FAIL", exit_code=4, result=unmanaged,
                                schema_problems=["managed is False, not true"],
                                focus=perf_focus.FocusVerdict(True, (), ["focus theft"], True))
        if kind == "bound":
            return perf_outcomes_tests.make_outcome(plan=plan, status="TIMEOUT", exit_code=-9, result=None)
        if kind == "interrupted":
            # Ctrl-C in run_step's wait: it killed and reaped the harness without counting its group.
            return perf_outcomes_tests.make_outcome(plan=plan, status="INTERRUPTED", exit_code=-9, result=None,
                                step_detail="interrupted; process tree killed")
        if kind == "pane":
            return perf_outcomes_tests.make_outcome(plan=plan, exit_code=3, result=perf_result_tests.valid_result(
                status="invalid", exit_code=3, invalid_reason=perf_windows_tests.PANE_EXIT_REASON))
        if kind == "invalid":
            return perf_outcomes_tests.make_outcome(plan=plan, exit_code=3, result=perf_result_tests.valid_result(
                status="invalid", exit_code=3, invalid_reason="a phase ran past its bound"))
        if kind == "degraded":
            presenter = dict(perf_foreground_tests.WGPU_PRESENTER, software_rendering=True, software_render_degraded=True)
            return perf_outcomes_tests.make_outcome(plan=plan, platform="win32", renderer=perf_foreground_tests.HARDWARE_RENDERER,
                                result=perf_result_tests.valid_result(presenter=presenter))
        raise AssertionError(kind)
    return build


class SmokeTests(unittest.TestCase):
    SCENARIOS = {"S1": perf_watcher_tests.IDLE_SCENARIO, "S3": perf_scenarios.Scenario("S3", ("default",), "Flood", 120, 30)}

    def run_cases(self, answers, host_platform="darwin", scenarios=None):
        """Drive the smoke's cases on `host_platform`; each case name answers from its queue of outcome kinds."""
        calls = []

        def run_case(plan, evidence):
            name = (plan.scenario.id + ("" if plan.variant == "default" else f"/{plan.variant}")
                    + ("-deadline" if plan.kill_at_go else ""))
            calls.append(name)
            self.assertTrue(plan.short and plan.smoke)
            queue = answers.get(name, ["valid"])
            return outcome_of(queue.pop(0) if len(queue) > 1 else queue[0])(plan)
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed), contextlib.redirect_stderr(io.StringIO()):
            code, reasons = perf_smoke.smoke_cases(scenarios or self.SCENARIOS, Path("/b"), perf_memory_tests.HARNESS_HASH, run_case,
                                             Path("/e"), host_platform=host_platform)
        # What the smoke printed, for tests that check the lines a CI log shows.
        self.printed = printed.getvalue()
        return code, reasons, calls

    def test_the_attempt_line_and_blocked_summary_name_a_suspected_occlusion(self):
        # The CI log shows these lines, so each names the harness's reason, not only its fixed notes.
        code, reasons, _calls = self.run_cases({"S1": ["startup"]})
        self.assertEqual(code, 3)
        self.assertIn(f"[perf-smoke] S1 attempt 1: occluded: {perf_watcher_tests.STARTUP_OCCLUSION_REASON}", self.printed)
        self.assertTrue(reasons[0].startswith(f"S1: no valid run after {perf_settings.RETRY_LIMIT} retries: "
                                              f"{perf_watcher_tests.STARTUP_OCCLUSION_REASON}"), reasons)

    def test_three_valid_cases_pass(self):
        # S1, S3 and the deadline case each need one valid run; no timing is asserted.
        code, reasons, calls = self.run_cases({})
        self.assertEqual((code, reasons), (0, []))
        self.assertEqual(calls, ["S1", "S3", "S1-deadline"])

    def test_an_occlusion_is_retried_within_the_bound(self):
        # An environmental invalidation retries; the case passes once a run is valid.
        code, _reasons, calls = self.run_cases({"S1": ["occluded", "occluded", "valid"]})
        self.assertEqual(code, 0)
        self.assertEqual(calls.count("S1"), 3)

    def test_no_valid_exercised_run_after_the_retries_is_blocked(self):
        # Four occluded runs, or a scenario the harness cannot run, exit 3 after every case ran.
        code, reasons, calls = self.run_cases({"S1": ["occluded"]})
        self.assertEqual(code, 3)
        self.assertEqual(calls.count("S1"), perf_settings.RETRY_LIMIT + 1)
        self.assertIn("S3", calls)
        self.assertTrue(reasons)
        self.assertEqual(self.run_cases({"S3": ["blocked"]})[0], 3)

    def test_safety_and_schema_failures_exit_1_at_once(self):
        # A schema, focus, cleanup or home failure, or a run that reached its bound, fails immediately.
        for kind in ("schema", "theft", "survivor", "home", "bound"):
            with self.subTest(kind=kind):
                code, reasons, calls = self.run_cases({"S1": [kind]})
                self.assertEqual(code, 1)
                self.assertEqual(calls, ["S1"])
                self.assertTrue(reasons)
        code, _reasons, calls = self.run_cases({"S1-deadline": ["survivor"]})
        self.assertEqual((code, calls), (1, ["S1", "S3", "S1-deadline"]))

    def test_startup_occlusion_after_the_retries_is_blocked(self):
        # A window that never reaches the screen is retried within the bound, then the smoke reports BLOCKED.
        code, _reasons, calls = self.run_cases({"S1": ["startup"]})
        self.assertEqual(code, 3)
        self.assertEqual(calls.count("S1"), perf_settings.RETRY_LIMIT + 1)

    def test_deadline_case_occlusion_is_retried(self):
        # The deadline case can be occluded before GO like any other run.
        code, _reasons, calls = self.run_cases({"S1-deadline": ["occluded", "valid"]})
        self.assertEqual(code, 0)
        self.assertEqual(calls.count("S1-deadline"), 2)

    def test_a_listless_scenario_fails(self):
        # The smoke cannot pass without the scenarios it is defined by.
        with contextlib.redirect_stdout(io.StringIO()):
            code, _reasons = perf_smoke.smoke_cases({"S1": perf_watcher_tests.IDLE_SCENARIO}, Path("/b"), perf_memory_tests.HARNESS_HASH, lambda plan, evidence: None,
                                              Path("/e"))
        self.assertEqual(code, 1)

    def test_evidence_directory_is_exported_kept_on_failure_and_removed_on_a_pass(self):
        # CI uploads the directory when the job fails, so a failure keeps it and prints its path.
        for code in (0, 1, 3):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as temporary:
                env_file = Path(temporary) / "github-env"
                seen = []

                def runner(evidence):
                    seen.append(evidence)
                    self.assertTrue(evidence.is_dir())
                    return code, ([] if code == 0 else ["reason"])
                output = io.StringIO()
                with mock.patch.object(perf.sys, "platform", "darwin"), contextlib.redirect_stdout(output), \
                        contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(perf_smoke.smoke_main({"GITHUB_ENV": str(env_file)}, runner), code)
                evidence = seen[0]
                self.assertTrue(evidence.name.startswith("sonicterm-perf-evidence-"))
                self.assertIn(f"SONICTERM_PERF_EVIDENCE_DIR={evidence}\n", env_file.read_text(encoding="utf-8"))
                self.assertIn(str(evidence), output.getvalue())
                self.assertEqual(evidence.exists(), code != 0)
                if evidence.exists():
                    perf.shutil.rmtree(evidence)

    def test_a_host_that_cannot_run_the_smoke_is_blocked(self):
        # The harness and lsappinfo exist only on macOS, so another host exits 3, never 0.
        with mock.patch.object(perf.sys, "platform", "linux"), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(perf_smoke.smoke_main({}, lambda evidence: self.fail("ran")), 3)

    def test_gate_problem_ignores_leader_watches_on_windows(self):
        # On Windows the gate's job owns every process, so no process-group id needs to stay reserved.
        gate = SimpleNamespace(sigchld_problem=lambda: None, leader_watches=lambda: ())
        self.assertIsNone(perf_build_policy.gate_problem(gate, os_name="nt"))
        self.assertIsNotNone(perf_build_policy.gate_problem(gate, os_name="posix"))

    def test_the_smoke_runs_on_windows(self):
        # Windows is a supported host, so the smoke runs there rather than reporting BLOCKED.
        with mock.patch.object(perf.sys, "platform", "win32"), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(perf_smoke.smoke_main({}, lambda evidence: (0, [])), 0)

    def test_passing_smoke_prints_members_before_removing_evidence(self):
        # A passing smoke deletes its evidence, so the gate log keeps each attempt's job members instead.
        many = [perf_windows_tests.member(1000 + index, f"worker{index}.exe") for index in range(20)]

        def run_case(plan, evidence):
            if plan.kill_at_go:
                record = perf_windows_tests.custody(active=20, cleanup="terminated", members=many)
                return perf_outcomes_tests.make_outcome(plan=plan, status="FAIL", exit_code=124, result=None,
                                    deadline={"sent": True, "problem": None, "pid": perf_watcher_tests.HARNESS_PID},
                                    custody=record, deadline_exit_code=124,
                                    cleanup=perf_outcomes.custody_cleanup(record),
                                    leftover_processes=perf_outcomes.windows_leftover_processes(record, deadline_case=True))
            record = perf_windows_tests.custody(active=0, members=[perf_windows_tests.member(perf_watcher_tests.HARNESS_PID, "perf_scenarios.exe"),
                                               perf_windows_tests.member(perf_windows_tests.PROGRAM_PID, "conhost.exe")])
            return perf_outcomes_tests.make_outcome(plan=plan, custody=record, cleanup=perf_outcomes.custody_cleanup(record))

        def runner(evidence):
            return perf_smoke.smoke_cases(self.SCENARIOS, Path("/b"), perf_memory_tests.HARNESS_HASH, run_case, evidence,
                                    cases=perf_smoke.SMOKE_CASES)
        printed = io.StringIO()
        with mock.patch.object(perf.sys, "platform", "win32"), contextlib.redirect_stdout(printed), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(perf_smoke.smoke_main({}, runner), 0)
        text = printed.getvalue()
        self.assertIn(f"[perf-smoke] S1 attempt 1 members: {perf_watcher_tests.HARNESS_PID} perf_scenarios.exe 130000000000; "
                      f"{perf_windows_tests.PROGRAM_PID} conhost.exe 130000000000", text)
        deadline_line = next(line for line in text.splitlines() if line.startswith("[perf-smoke] S1-deadline attempt 1 members:"))
        self.assertEqual(deadline_line.count(".exe"), 16)
        self.assertTrue(deadline_line.endswith("; and 4 more"), deadline_line)
        self.assertLess(text.index(" members: "), text.index("removed"))
        evidence = Path(re.search(r"evidence=(\S+)", text)[1])
        self.assertFalse(evidence.exists())

    def test_synthetic_steps_name_the_current_host(self):
        # local-gate selects and labels steps by host, so a Windows run's steps say windows.
        self.assertEqual(perf_build_policy.gate_hosts("win32"), ("windows",))
        self.assertEqual(perf_build_policy.gate_hosts("darwin"), ("macos",))
        with tempfile.TemporaryDirectory() as temporary:
            gate = perf_watcher_tests.FakeGate(lambda step: ("PASS", 0, json.dumps(perf_build_tests.LIST_JSON) + "\n"))
            with mock.patch.object(perf.sys, "platform", "win32"):
                perf_smoke.list_scenarios(gate, Path("C:/b/perf_scenarios.exe"), Path(temporary), 1)
        self.assertEqual(gate.steps[0].hosts, ("windows",))

    WINDOWS_SCENARIOS = {"S1": perf_scenarios.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 120, 30),
                         "S3": perf_scenarios.Scenario("S3", ("default",), "Flood", 120, 30)}

    def test_the_case_list_names_each_hosts_cases_and_expected_kinds(self):
        # Windows adds the wgpu presenter and a role program's exit; macOS keeps its three cases.
        def listed(platform_name):
            return [(case.name, case.expected) for case in perf_smoke.smoke_case_list(platform_name)]
        darwin = [("S1", "valid"), ("S3", "valid"), ("S1-deadline", "valid")]
        self.assertEqual(listed("darwin"), darwin)
        self.assertEqual(listed("win32"), darwin + [("S1/wgpu", "valid"), ("S1/role-exit", "invalid")])
        # Each case's variant must be one the harness lists, or the smoke fails before any run.
        code, reasons, calls = self.run_cases({}, host_platform="win32")
        self.assertEqual((code, calls), (1, []))
        self.assertTrue(any("S1/wgpu" in reason for reason in reasons), reasons)
        code, reasons, calls = self.run_cases({"S1/role-exit": ["pane"]}, "win32", self.WINDOWS_SCENARIOS)
        self.assertEqual((code, reasons), (0, []))
        self.assertEqual(calls, ["S1", "S3", "S1-deadline", "S1/wgpu", "S1/role-exit"])

    def test_a_blocked_wgpu_run_blocks_the_smoke(self):
        # A degraded wgpu presenter cannot measure the wgpu path, so the smoke exits 3, naming the case.
        code, reasons, _calls = self.run_cases({"S1/wgpu": ["degraded"], "S1/role-exit": ["pane"]}, "win32",
                                               self.WINDOWS_SCENARIOS)
        self.assertEqual(code, 3)
        self.assertTrue(any(reason.startswith("S1/wgpu") for reason in reasons), reasons)

    def test_role_exit_passes_only_as_invalid_naming_the_pane(self):
        # A role program that exits must end the run invalid with a reason that names its pane.
        for answer in ("valid", "invalid"):
            with self.subTest(answer=answer):
                code, reasons, _calls = self.run_cases({"S1/role-exit": [answer]}, "win32", self.WINDOWS_SCENARIOS)
                self.assertEqual(code, 1)
                self.assertTrue(any(reason.startswith("S1/role-exit") for reason in reasons), reasons)
