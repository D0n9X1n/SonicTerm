"""Perf outcomes tests."""

from __future__ import annotations
from pathlib import Path
import unittest

import perf_focus
import perf_memory_tests
import perf_outcomes
import perf_result_tests
import perf_scenarios
import perf_sessions
import perf_settings
import perf_watcher_tests
import perf_windows_tests


# The App's adapter line from a Windows run on the hosted runner's software adapter.
WINDOWS_ADAPTER_LINE = ("2026-10-02T11:24:16.123456Z  INFO sonicterm_gpu::recovery_context: wgpu adapter selected "
                        "backend=Dx12 name=Microsoft Basic Render Driver driver=10.0.26100.9278 device_type=Cpu "
                        "software_rendering=true device_memory_policy=MemoryUsage")


def make_outcome(**overrides):
    """A run outcome that classifies as valid; overrides replace single facts."""
    plan = overrides.pop("plan", perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "head", Path("/b"), perf_memory_tests.HARNESS_HASH))
    fields = dict(plan=plan, evidence=Path("/e"), status="PASS", exit_code=0, result=perf_result_tests.valid_result(),
                  schema_problems=[], not_exercised=False, focus=perf_focus.FocusVerdict(False, (), [], True),
                  watcher_problems=[], cleanup=perf_sessions.CleanupResult(), home=[],
                  deadline={"sent": False, "problem": None, "pid": None}, memory=[], laps=[], footprints={},
                  harness_pid=perf_watcher_tests.HARNESS_PID)
    fields.update(overrides)
    return perf_outcomes.RunOutcome(**fields)


class ClassificationTests(unittest.TestCase):
    def kind(self, **overrides):
        return perf_outcomes.classify_outcome(make_outcome(**overrides))[0]

    def test_valid_run_needs_every_condition(self):
        # Exit 0, a valid result, focus judged and kept, sessions and cleanup settled.
        self.assertEqual(perf_outcomes.classify_outcome(make_outcome()), ("valid", []))
        self.assertEqual(self.kind(result=perf_result_tests.valid_result(finish_session_settled=False)), "cleanup")
        self.assertEqual(self.kind(focus=perf_focus.FocusVerdict(False, (), [], False)), "focus")

    def test_a_windows_run_must_report_its_adapter(self):
        # A Windows run proves its wgpu path only with the App's `wgpu adapter selected` or `reused` line, so a
        # run without it is not valid, even with a presenter; macOS logs no adapter and stays valid.
        # Resolve this runtime-only dependency after module initialization.
        import perf_foreground_tests
        wgpu = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "wgpu", "head", Path("/b"), perf_memory_tests.HARNESS_HASH)
        self.assertNotEqual(self.kind(plan=wgpu, platform="win32", renderer=None), "valid")
        kind, reasons = perf_outcomes.classify_outcome(make_outcome(
            platform="win32", renderer=None, result=perf_result_tests.valid_result(presenter=perf_foreground_tests.WGPU_PRESENTER)))
        self.assertEqual(kind, "adapter")
        self.assertTrue(any("wgpu adapter selected" in reason for reason in reasons), reasons)
        self.assertEqual(self.kind(platform="darwin", renderer=None), "valid")

    def test_schema_failures_stop_a_timed_out_or_blocked_run(self):
        # A harness timeout (exit 4) or block (exit 5) whose result is unmanaged, or another harness's, is a
        # schema failure, which stops a comparison; without one each keeps its kind. A run_step deadline is
        # unresolved cleanup either way, decided before the schema check, since run_step never counted the group.
        problems = ["managed is False, not true"]
        # Each case: how the run ended, the result's status, and the kind with and without the schema problem.
        cases = {
            "harness timeout": (dict(status="FAIL", exit_code=perf_settings.HARNESS_TIMEOUT), "timeout", "schema", "timeout"),
            "harness block": (dict(status="FAIL", exit_code=perf_settings.HARNESS_BLOCKED), "blocked", "schema", "blocked"),
            "run_step deadline": (dict(status="TIMEOUT", exit_code=-9), "timeout", "cleanup", "cleanup"),
        }
        for name, (launch, status, with_problem, without_problem) in cases.items():
            with self.subTest(name):
                result = perf_result_tests.valid_result(status=status, exit_code=launch["exit_code"], finish_session_settled=True)
                kind, reasons = perf_outcomes.classify_outcome(make_outcome(result=result, schema_problems=problems, **launch))
                self.assertEqual(kind, with_problem)
                if with_problem == "schema":
                    self.assertEqual(reasons, problems)
                self.assertEqual(perf_outcomes.compare_verdict(kind), "stop")
                self.assertEqual(perf_outcomes.classify_outcome(make_outcome(result=result, **launch))[0], without_problem)

    def test_a_launcher_failure_with_exit_0_is_never_valid(self):
        # run_step can report FAIL with exit 0 (a lost leader, a failed final log write), so only PASS can be valid.
        kind, reasons = perf_outcomes.classify_outcome(make_outcome(status="FAIL"))
        self.assertNotEqual(kind, "valid")
        self.assertTrue(reasons)
        self.assertEqual(perf_outcomes.smoke_verdict(kind), "fail")

    def test_process_group_survivors_fail_the_smoke_and_stop_a_comparison(self):
        # Members that outlived the harness, or could not be counted, are a safety failure like unresolved cleanup.
        for leftover in (2, None):
            with self.subTest(leftover=leftover):
                outcome = make_outcome(status="FAIL", leftover_processes=leftover,
                                       step_detail="2 leftover process(es) outlived the leader by 2s")
                kind, reasons = perf_outcomes.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any("process group" in reason for reason in reasons))
                self.assertEqual(perf_outcomes.smoke_verdict(kind), "fail")
                self.assertEqual(perf_outcomes.compare_verdict(kind), "stop")

    def test_an_unsettled_teardown_fails_before_any_occlusion_retry(self):
        # finish_session did not settle, so the run fails at once instead of being retried, in the smoke and a comparison.
        unsettled = perf_result_tests.valid_result(status="invalid", exit_code=3, finish_session_settled=False,
                                 invalid_reason="unrequested native occlusion change")
        deadline_plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("/b"), perf_memory_tests.HARNESS_HASH, kill_at_go=True)
        cases = {"occlusion": make_outcome(exit_code=3, result=unsettled),
                 "occlusion before GO": make_outcome(plan=deadline_plan, exit_code=3, result=unsettled),
                 "other invalidation": make_outcome(exit_code=3, result=dict(unsettled, invalid_reason="unexpected input")),
                 "harness timeout": make_outcome(exit_code=4, result=dict(unsettled, status="timeout", exit_code=4)),
                 "startup occlusion": make_outcome(exit_code=3, result=dict(
                     unsettled, invalid_reason=perf_watcher_tests.STARTUP_OCCLUSION_REASON))}
        for name, outcome in cases.items():
            with self.subTest(name):
                kind, reasons = perf_outcomes.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any("finish_session" in reason for reason in reasons))
                self.assertEqual((perf_outcomes.smoke_verdict(kind), perf_outcomes.compare_verdict(kind)), ("fail", "stop"))

    def test_startup_occlusion_is_an_occlusion_the_smoke_and_a_comparison_retry(self):
        # No frame within 10 s of the window opening ends Startup as invalid (exit 3) with a suspected occlusion,
        # the harness's own reason; a full-screen app hiding the window is environmental, so the run is retried.
        startup = perf_result_tests.valid_result(status="invalid", exit_code=3, finish_session_settled=True,
                               invalid_reason=perf_watcher_tests.STARTUP_OCCLUSION_REASON)
        kind, _reasons = perf_outcomes.classify_outcome(make_outcome(exit_code=3, result=startup))
        self.assertEqual(kind, "occluded")
        self.assertEqual((perf_outcomes.smoke_verdict(kind), perf_outcomes.compare_verdict(kind)), ("retry", "invalid"))

    def test_the_deadline_case_passes_only_on_its_own_collected_sigkill(self):
        # Signal acceptance proves no termination: only run_step's own reap of the harness, exit -SIGKILL, passes.
        plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("/b"), perf_memory_tests.HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": perf_watcher_tests.HARNESS_PID}
        self.assertEqual(self.kind(plan=plan, status="FAIL", exit_code=-9, result=None, deadline=killed), "valid")
        for status, exit_code in (("TIMEOUT", -9), ("TIMEOUT", None), ("FAIL", None), ("FAIL", 0), ("FAIL", -15)):
            with self.subTest(status=status, exit_code=exit_code):
                kind = self.kind(plan=plan, status=status, exit_code=exit_code, result=None, deadline=killed)
                self.assertNotEqual(kind, "valid")
                self.assertEqual(perf_outcomes.smoke_verdict(kind), "fail")

    def test_an_uncollected_harness_exit_is_unresolved_cleanup(self):
        # Without a collected exit run_step never counted the harness's group, so its leftover count is no measurement.
        deadline_plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("/b"), perf_memory_tests.HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": perf_watcher_tests.HARNESS_PID}
        cases = {"run_step deadline": make_outcome(status="TIMEOUT", exit_code=None, result=None),
                 "leader taken by another reaper": make_outcome(status="FAIL", exit_code=None, result=None),
                 "interrupted": make_outcome(status="INTERRUPTED", exit_code=None, result=None),
                 "deadline kill never reaped": make_outcome(plan=deadline_plan, status="TIMEOUT", exit_code=None,
                                                            result=None, deadline=killed)}
        for name, outcome in cases.items():
            with self.subTest(name):
                kind, reasons = perf_outcomes.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any("collected" in reason for reason in reasons))
                self.assertEqual((perf_outcomes.smoke_verdict(kind), perf_outcomes.compare_verdict(kind)), ("fail", "stop"))
        # A launch failure started no process, so there is nothing to collect.
        self.assertNotEqual(self.kind(status="LAUNCH", exit_code=None, result=None), "cleanup")

    def test_a_run_step_deadline_is_unresolved_cleanup_even_when_collected(self):
        # At the deadline either the harness hung, so its group was killed uncounted, or it exited with its group
        # counted empty while a process outside the group held the output open. Neither a collected exit nor settled
        # anchor cleanup measures what outlived the harness: the smoke fails, a comparison stops, and the reason
        # carries run_step's detail, which names a pipe holder.
        settled_anchors = perf_sessions.CleanupResult()
        self.assertTrue(settled_anchors.passed)
        deadline = f"deadline of {perf_scenarios.run_timeout_s(perf_watcher_tests.IDLE_SCENARIO, smoke=False)}s reached"
        hang = f"{deadline}; process tree killed"
        pipe = (f"{deadline}; the group kill was refused with EPERM, so only the leader was killed or reaped; "
                "a descendant outside the process group still holds the output pipe")
        cases = {"no result": (-9, None, hang),
                 "a settled result": (-9, perf_result_tests.valid_result(status="timeout", exit_code=4, finish_session_settled=True), hang),
                 "an unsettled result": (-9, perf_result_tests.valid_result(status="timeout", exit_code=4, finish_session_settled=False),
                                         hang),
                 "a pipe held outside the counted group": (0, None, pipe)}
        for name, (exit_code, result, detail) in cases.items():
            with self.subTest(name):
                outcome = make_outcome(status="TIMEOUT", exit_code=exit_code, result=result, cleanup=settled_anchors,
                                       step_detail=detail)
                kind, reasons = perf_outcomes.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any(detail in reason and perf_watcher_tests.UNCOUNTED_STOP in reason for reason in reasons), reasons)
                self.assertEqual((perf_outcomes.smoke_verdict(kind), perf_outcomes.compare_verdict(kind)), ("fail", "stop"))

    def test_an_interrupted_run_is_unresolved_cleanup_even_when_collected(self):
        # Ctrl-C in run_step's wait kills and reaps the harness without counting its group, as its deadline does.
        # Whatever exit it collected, the smoke fails and a comparison stops instead of starting its next attempt.
        detail = "interrupted; process tree killed"
        occluded = perf_result_tests.valid_result(status="invalid", exit_code=3, invalid_reason=perf_watcher_tests.STARTUP_OCCLUSION_REASON)
        cases = {"killed (-9)": (-9, None),
                 "already exited 0": (0, perf_result_tests.valid_result()),
                 "already exited with an occlusion (3)": (3, occluded),
                 "already exited blocked (5)": (5, None)}
        for name, (exit_code, result) in cases.items():
            with self.subTest(name):
                outcome = make_outcome(status="INTERRUPTED", exit_code=exit_code, result=result, step_detail=detail)
                kind, reasons = perf_outcomes.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any(detail in reason and perf_watcher_tests.UNCOUNTED_STOP in reason for reason in reasons), reasons)
                self.assertEqual((perf_outcomes.smoke_verdict(kind), perf_outcomes.compare_verdict(kind)), ("fail", "stop"))

    def test_the_harness_reason_leads_an_occlusion_or_invalidation(self):
        # The harness names the cause in invalid_reason; its notes are fixed text about the measurement, so the
        # reason comes first and the notes follow, before GO in the deadline case too.
        notes = ["A measurement ends when the dispatch returns, not at scanout."]
        checkpoint = ("checkpoint 0-end got no .done within 60 s, so its footprint is missing and the run stopped "
                      "before the next phase")
        deadline_plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("/b"), perf_memory_tests.HARNESS_HASH, kill_at_go=True)
        cases = {"startup occlusion": (None, perf_watcher_tests.STARTUP_OCCLUSION_REASON, "occluded"),
                 "occlusion before GO": (deadline_plan, perf_watcher_tests.STARTUP_OCCLUSION_REASON, "occluded"),
                 "checkpoint invalidation": (None, checkpoint, "invalid")}
        for name, (plan, reason, expected) in cases.items():
            with self.subTest(name):
                result = perf_result_tests.valid_result(status="invalid", exit_code=3, invalid_reason=reason, notes=notes)
                overrides = {"plan": plan} if plan else {}
                outcome = make_outcome(status="FAIL", exit_code=3, result=result, **overrides)
                self.assertEqual(perf_outcomes.classify_outcome(outcome), (expected, [reason] + notes))

    def test_the_harness_deadline_with_a_counted_group_stays_a_retryable_timeout(self):
        # Exit 4 is the harness's own deadline: run_step saw the harness exit and counted its group empty,
        # so a comparison retries the run; the smoke fails it, as it fails every timeout.
        result = perf_result_tests.valid_result(status="timeout", exit_code=perf_settings.HARNESS_TIMEOUT, finish_session_settled=True)
        outcome = make_outcome(status="FAIL", exit_code=perf_settings.HARNESS_TIMEOUT, result=result, leftover_processes=0)
        kind, reasons = perf_outcomes.classify_outcome(outcome)
        self.assertEqual(kind, "timeout")
        self.assertIn("the harness timed out (exit 4)", reasons)
        self.assertEqual((perf_outcomes.smoke_verdict(kind), perf_outcomes.compare_verdict(kind)), ("fail", "invalid"))

    def test_a_reported_teardown_failure_is_fatal_on_every_exit_path(self):
        # An unsettled finish_session may have left sessions behind, so no retryable reason or exit code hides it.
        # A run_step deadline is cleanup before any result is read, so its own test covers that path.
        unsettled = perf_result_tests.valid_result(status="invalid", exit_code=3, finish_session_settled=False)
        theft = perf_focus.FocusVerdict(True, (), ["focus theft"], True)
        cases = {
            "exit 3 with a session problem": make_outcome(exit_code=3, result=unsettled,
                                                          watcher_problems=["session 1 failed validation"]),
            "exit 3 with a home write": make_outcome(exit_code=3, result=unsettled, home=["config.toml: added"]),
            "exit 3 with focus theft": make_outcome(exit_code=3, result=unsettled, focus=theft),
            "exit 3 with a font error": make_outcome(exit_code=3, result=unsettled,
                                                     font_errors=["Unable to load the configured primary font"]),
            "exit 4": make_outcome(status="FAIL", exit_code=4, result=dict(unsettled, status="timeout", exit_code=4)),
            "exit 5": make_outcome(status="FAIL", exit_code=5, result=dict(unsettled, status="blocked", exit_code=5)),
        }
        for name, outcome in cases.items():
            with self.subTest(name):
                kind, reasons = perf_outcomes.classify_outcome(outcome)
                self.assertEqual(kind, "cleanup")
                self.assertTrue(any("finish_session" in reason for reason in reasons))
                self.assertEqual((perf_outcomes.smoke_verdict(kind), perf_outcomes.compare_verdict(kind)), ("fail", "stop"))

    def test_the_deliberate_kill_with_proven_cleanup_is_exempt(self):
        # The deadline case ends the harness before its teardown; run_step's reap and the anchors prove its cleanup.
        plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("/b"), perf_memory_tests.HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": perf_watcher_tests.HARNESS_PID}
        partial = perf_result_tests.valid_result(status="invalid", exit_code=3, finish_session_settled=False)
        self.assertEqual(self.kind(plan=plan, status="FAIL", exit_code=-9, result=partial, deadline=killed), "valid")

    def test_fatal_outcomes_are_decided_before_any_retryable_reason(self):
        # A retry would rerun on a host that may hold this run's processes, or trust an untrusted result.
        theft = perf_focus.FocusVerdict(True, (), ["focus theft"], True)
        unmanaged = perf_result_tests.valid_result(status="timeout", exit_code=4, managed=False)
        cases = {
            "unmanaged result and a home write": (
                "schema", make_outcome(schema_problems=["managed is False, not true"], home=["config.toml: added"])),
            "exit 0 without result.json and a home write": (
                "schema", make_outcome(result=None, home=["config.toml: added"])),
            "refusal and focus theft": ("refused", make_outcome(status="FAIL", exit_code=2, result=None, focus=theft)),
            "unmanaged exit-4 result and focus theft": (
                "schema", make_outcome(status="FAIL", exit_code=4, result=unmanaged,
                                       schema_problems=["managed is False, not true"], focus=theft)),
        }
        for name, (expected, outcome) in cases.items():
            with self.subTest(name):
                kind, _reasons = perf_outcomes.classify_outcome(outcome)
                self.assertEqual(kind, expected)
                self.assertEqual(perf_outcomes.compare_verdict(kind), "stop")

    def test_harness_exit_codes(self):
        # 2 refuses, 3 invalidates, 4 is the harness's timeout, 5 is blocked, others are unexpected; run_step's
        # own deadline is no harness exit code but unresolved cleanup.
        self.assertEqual(self.kind(exit_code=2, result=None), "refused")
        self.assertEqual(self.kind(exit_code=3, result=perf_result_tests.valid_result(status="invalid", exit_code=3,
                                                                    notes=["native occlusion change"])), "occluded")
        self.assertEqual(self.kind(exit_code=3, result=perf_result_tests.valid_result(status="invalid", exit_code=3,
                                                                    notes=["unexpected keyboard input"])), "invalid")
        self.assertEqual(self.kind(exit_code=4, result=None), "timeout")
        self.assertEqual(self.kind(status="TIMEOUT", exit_code=-9, result=None), "cleanup")
        self.assertEqual(self.kind(exit_code=5, result=None), "blocked")
        self.assertEqual(self.kind(exit_code=101, result=None), "unexpected")

    def test_not_exercised_without_a_result_is_blocked(self):
        # NOT_EXERCISED with exit 0 and no result.json means the host cannot exercise the scenario.
        self.assertEqual(self.kind(result=None, not_exercised=True), "blocked")
        self.assertEqual(self.kind(result=None), "schema")

    def test_schema_problems_are_never_retried_as_invalid(self):
        # An unmanaged result, another harness's hash, or a missing result is a schema failure.
        self.assertEqual(self.kind(schema_problems=["managed is False, not true"]), "schema")
        self.assertEqual(self.kind(exit_code=3, result=perf_result_tests.valid_result(status="invalid", exit_code=3),
                                   schema_problems=["harness_hash differs"]), "schema")
        self.assertEqual(self.kind(result=perf_result_tests.valid_result(status="invalid")), "schema")

    def test_safety_failures_outrank_the_exit_code(self):
        # Cleanup, session records, home writes and focus are judged even when the harness passed.
        failed = perf_sessions.CleanupResult()
        failed.unresolved("members left")
        self.assertEqual(self.kind(cleanup=failed), "cleanup")
        self.assertEqual(self.kind(watcher_problems=["session 0 failed validation"]), "session")
        self.assertEqual(self.kind(home=["config.toml: added"]), "home")
        self.assertEqual(self.kind(focus=perf_focus.FocusVerdict(True, (), ["focus theft"], True)), "focus")
        self.assertEqual(self.kind(exit_code=5, result=None, home=["config.toml: added"]), "home")

    def test_deadline_case_is_judged_by_cleanup_not_by_its_result(self):
        # A run ended at GO has no complete result; its kill, cleanup, focus and home writes decide.
        plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("/b"), perf_memory_tests.HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": perf_watcher_tests.HARNESS_PID}
        self.assertEqual(self.kind(plan=plan, status="FAIL", exit_code=-9, result=None, deadline=killed), "valid")
        self.assertEqual(self.kind(plan=plan, status="FAIL", exit_code=-9, result=None, deadline=killed,
                                   home=["config.toml: added"]), "home")
        refused = {"sent": False, "problem": "harness pid 900 runs 'zsh'", "pid": perf_watcher_tests.HARNESS_PID}
        self.assertEqual(self.kind(plan=plan, deadline=refused), "deadline")
        self.assertEqual(self.kind(plan=plan, exit_code=5, result=None), "blocked")
        self.assertEqual(self.kind(plan=plan), "unexpected")

    def test_verdicts_per_mode(self):
        # The smoke retries only an occlusion; a comparison retries any invalid run and stops on schema.
        self.assertEqual([perf_outcomes.smoke_verdict(kind) for kind in ("valid", "occluded", "blocked", "focus")],
                         ["pass", "retry", "blocked", "fail"])
        for kind in ("schema", "refused", "cleanup", "home", "session", "timeout", "unexpected", "invalid", "deadline"):
            with self.subTest(kind=kind):
                self.assertEqual(perf_outcomes.smoke_verdict(kind), "fail")
        self.assertEqual([perf_outcomes.compare_verdict(kind) for kind in ("valid", "focus", "occluded", "blocked",
                                                                  "schema", "refused")],
                         ["valid", "invalid", "invalid", "blocked", "stop", "stop"])

    def test_windows_deadline_case_is_valid_only_with_verified_custody(self):
        # TerminateProcess ends the harness with 124; the job's members alive at that moment are expected
        # only when the job's custody proves they were all ended.
        plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("/b"), perf_memory_tests.HARNESS_HASH, kill_at_go=True)
        killed = {"sent": True, "problem": None, "pid": perf_watcher_tests.HARNESS_PID}
        verified = perf_windows_tests.custody(active=3, cleanup="terminated")

        def windows(custody_record, exit_code=124):
            return make_outcome(
                plan=plan, status="FAIL", exit_code=exit_code, result=None, deadline=killed,
                cleanup=perf_outcomes.custody_cleanup(custody_record), custody=custody_record,
                deadline_exit_code=perf_outcomes.deadline_exit_code("win32"),
                leftover_processes=perf_outcomes.windows_leftover_processes(custody_record, deadline_case=True))
        self.assertEqual(perf_outcomes.classify_outcome(windows(verified)), ("valid", []))
        self.assertNotEqual(perf_outcomes.classify_outcome(windows(verified, exit_code=-9))[0], "valid")
        self.assertEqual(perf_outcomes.classify_outcome(windows(perf_windows_tests.custody(active=3, cleanup="terminated", empty=False)))[0],
                         "cleanup")
        self.assertEqual((perf_outcomes.deadline_exit_code("win32"), perf_outcomes.deadline_exit_code("darwin")), (124, -9))

    def test_windows_members_outliving_a_run_without_a_deadline_are_cleanup(self):
        # A job run_step had to end held members past the harness's exit, so the run is cleanup, never launcher.
        record = perf_windows_tests.custody(active=2, cleanup="terminated")
        outcome = make_outcome(status="FAIL", exit_code=0, cleanup=perf_outcomes.custody_cleanup(record), custody=record,
                               leftover_processes=perf_outcomes.windows_leftover_processes(record, deadline_case=False))
        kind, reasons = perf_outcomes.classify_outcome(outcome)
        self.assertEqual(kind, "cleanup")
        self.assertTrue(any("2 member(s)" in reason and "job" in reason for reason in reasons), reasons)
