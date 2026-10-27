"""Perf execution tests."""

from __future__ import annotations
from perf_test_fixture import perf
import contextlib
import dataclasses
import inspect
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import perf_attribution_report
import perf_counter_schema_tests
import perf_driver
import perf_execution
import perf_focus
import perf_focus_tests
import perf_foreground_tests
import perf_guard_schema
import perf_guard_tests
import perf_home
import perf_inventory
import perf_memory_tests
import perf_outcomes
import perf_outcomes_tests
import perf_processes_tests
import perf_result_schema
import perf_result_tests
import perf_scenarios
import perf_smoke
import perf_timeline_tests
import perf_watcher_tests
import perf_windows_tests


def wait_for(condition, timeout_s=10.0):
    """Poll a condition the watcher thread satisfies; fail the test if it never holds."""
    deadline = perf.time.monotonic() + timeout_s
    while not condition():
        if perf.time.monotonic() > deadline:
            raise AssertionError("condition never held")
        perf.time.sleep(0.01)


class ExecuteRunTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.temp_root = root / "tmp"
        self.temp_root.mkdir()
        self.home = root / "home" / ".sonicterm"
        self.table = perf_processes_tests.FakeTable(perf_processes_tests.leader(), perf_processes_tests.anchor(), perf_watcher_tests.harness_process(), perf_processes_tests.FakeProcess(510, 510, 500, start="11"))
        self.home_write = False
        self.skip_ack = False
        self.font_error = False
        self.progress = False
        self.write_result = True
        # The result.json the harness writes when it passes; None writes valid_result().
        self.result_body = None
        self.deadline_answer = ("FAIL", -9, "killed\n")
        # When set, the harness never exits by itself, and run_step's deadline reports this.
        self.hang_answer = None
        self.front_pids = []
        self.source_root = Path(self.temporary.name) / "tree"
        self.source_root.mkdir()

    def tearDown(self):
        self.temporary.cleanup()

    def test_a_smoke_carries_the_guard_capability_and_its_cfg_decision(self):
        # Finding 1 (round 2): a smoke built from a listing that declares guard_correlation_schema plans each case
        # with it and with the smoke build's cfg decision, so a realistic finalized result of a cfg-off build,
        # every phase `unavailable`, passes the schema and reads n/a; the same output under a cfg-on decision is
        # an impossible availability.
        scenario = dataclasses.replace(perf_watcher_tests.IDLE_SCENARIO, guard_correlation_schema=1)
        body = perf_result_tests.valid_result()
        body["phases"] = [dict(body["phases"][0], guard_correlation={
            "status": "unavailable", "file": None, "take_seq": None, "bytes": None, "sha256": None,
            "window": {"label": perf_guard_schema.GUARD_WINDOW_LABEL, "start_ns": None, "end_ns": None}},
            guard_correlation_transport_ns=7)]
        self.result_body = body
        plans = []

        def run_case(plan, case_evidence):
            plans.append(plan)
            # Each run gets a fresh process table and host, as each smoke attempt meets a fresh harness process.
            self.table = perf_processes_tests.FakeTable(perf_processes_tests.leader(), perf_processes_tests.anchor(), perf_watcher_tests.harness_process(), perf_processes_tests.FakeProcess(510, 510, 500, start="11"))
            host = perf_outcomes.Host(perf_watcher_tests.FakeGate(lambda step: self.fake_harness(step, False)), self.table, self.front_run,
                             self.home, self.temp_root, set(), {"HOME": "/h"}, clock=lambda: perf_processes_tests.LAUNCH_UNIX_S)
            return perf_execution.execute_run(plan, host, case_evidence)
        for cfgs, expected in (((), "n/a"), ((perf_attribution_report.GUARD_SPANS_CFG,), "incomplete")):
            evidence = Path(self.temporary.name) / "smoke" / (cfgs[0] if cfgs else "off")
            with contextlib.redirect_stdout(io.StringIO()):
                perf_smoke.smoke_cases({"S1": scenario}, Path("/b/perf_scenarios"), perf_memory_tests.HARNESS_HASH, run_case, evidence,
                                 cases=[perf_smoke.SmokeCase("S1")], harness_cfgs=cfgs)
            plan = plans[-1]
            self.assertEqual((plan.guard_correlation_schema, plan.guard_api), (1, bool(cfgs)))
            joined = json.loads((evidence / "S1-1" / "guard-correlation-join.json").read_text(encoding="utf-8"))
            self.assertEqual([verdict["verdict"] for verdict in joined], [expected], cfgs)
        self.assertEqual(perf_result_schema.validate_result(body, perf_memory_tests.HARNESS_HASH, 0, guard_correlation_schema=plans[0]
                                              .guard_correlation_schema), [])

    def test_the_run_identity_records_the_guard_cfg_decision(self):
        # Finding 2 (round 2): run-identity.json records guard_api, the effective cfg decision perf-flags binds
        # each phase's availability to; null when nothing decided it.
        identity = perf_inventory.run_identity_document({"base": "1" * 40, "head": "2" * 40}, "ab" * 32,
                                              {"base": [], "head": []}, True, {}, False, {}, guard_api=True)
        self.assertIs(identity["guard_api"], True)
        self.assertIsNone(perf_inventory.run_identity_document({"base": "1" * 40, "head": "2" * 40}, "ab" * 32,
                                                     {"base": [], "head": []}, True, {}, False, {})["guard_api"])
        self.assertIn("GUARD_SPANS_CFG in harness_cfgs))", inspect.getsource(perf_driver._compare))

    def test_a_malformed_middle_take_is_incomplete_through_execute_run(self):
        # Finding 3 through the production path: execute_run joins a run whose middle take is malformed into
        # named, incomplete verdicts kept as evidence, without raising.
        staging = Path(self.temporary.name) / "staging"
        sidecars = perf_guard_tests.guard_run(3)
        sidecars[1]["panes"][0].pop("pending")
        phases = perf_guard_tests.guard_phases(staging, sidecars)
        self.scratch_files = {phase["guard_correlation"]["file"]: (staging / phase["guard_correlation"]["file"]).read_bytes()
                              for phase in phases}
        body = perf_result_tests.valid_result()
        template = body["phases"][0]
        body["phases"] = [dict(template, name=phase["name"], guard_correlation=phase["guard_correlation"],
                               guard_correlation_transport_ns=10) for phase in phases]
        self.result_body = body
        gate = perf_watcher_tests.FakeGate(lambda step: self.fake_harness(step, False))
        host = perf_outcomes.Host(gate, self.table, self.front_run, self.home, self.temp_root, set(), {"HOME": "/h"},
                         clock=lambda: perf_processes_tests.LAUNCH_UNIX_S)
        plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("/b/perf_scenarios"), perf_timeline_tests.GUARD_HASH, short=True,
                            smoke=True, source_root=self.source_root, guard_correlation_schema=1, guard_api=True)
        evidence = Path(self.temporary.name) / "evidence" / "guard"
        with contextlib.redirect_stdout(io.StringIO()) as printed:
            try:
                perf_execution.execute_run(plan, host, evidence)
            except Exception as error:  # noqa: BLE001 - any raise is the defect under test
                self.fail(f"execute_run raised on a malformed take: {type(error).__name__}: {error}")
        verdicts = json.loads((evidence / "guard-correlation-join.json").read_text(encoding="utf-8"))
        self.assertEqual([verdict["verdict"] for verdict in verdicts], ["incomplete"] * 3)
        self.assertTrue(any("lacks ['pending']" in reason for reason in verdicts[1]["reasons"]), verdicts[1])
        self.assertIn("guard correlation flood: incomplete", printed.getvalue())

    def front_run(self, argv, timeout_s):
        """Answer lsappinfo: no front application, or each sample's front pid in turn; None fails the lookup."""
        if not self.front_pids:
            self.assertEqual(tuple(argv), perf_focus.FRONT_ARGV)
            return perf_focus_tests.command(argv, "[ NULL ]\n")
        if tuple(argv) == perf_focus.FRONT_ARGV:
            return perf_focus_tests.command(argv, "ASN:0x0-0x1:\n")
        pid = self.front_pids.pop(0) if len(self.front_pids) > 1 else self.front_pids[0]
        if pid is None:
            return perf_focus_tests.command(argv, "", exit_code=1, stderr="lookup failed\n")
        return perf_focus_tests.command(argv, f'"pid"={pid}\n')

    def fake_harness(self, step, deadline):
        """Act as the harness: register a session, wait for its acknowledgement, then pass or be killed."""
        scratch = Path(step.argv[-1])
        self.assertFalse(scratch.exists())
        (scratch / "logs").mkdir(parents=True)
        log = perf_memory_tests.memory_line() + "\n"
        if getattr(self, "windows_run", False):
            # A Windows run's App logs the adapter it selected, which the comparison requires there.
            log += perf_outcomes_tests.WINDOWS_ADAPTER_LINE + "\n"
        (scratch / "logs" / "sonicterm.log.2026-10-02").write_text(log, encoding="utf-8")
        (scratch / "harness.pid").write_text(str(perf_watcher_tests.HARNESS_PID), encoding="utf-8")
        (scratch / "sessions").mkdir()
        (scratch / "sessions" / "0.json").write_text(getattr(self, "session_text", perf_processes_tests.RECORD_TEXT), encoding="utf-8")
        if not self.skip_ack:
            wait_for(lambda: (scratch / "acks" / "0").exists())
        if self.home_write:
            self.home.mkdir(parents=True)
            (self.home / "config.toml").write_text("x", encoding="utf-8")
        (scratch / "go").mkdir()
        (scratch / "go" / "0").write_bytes(b"")
        if self.progress:
            # Shaped like a valid result, so a test can show it is never read as one.
            (scratch / "progress.json").write_text(json.dumps(perf_result_tests.valid_result()), encoding="utf-8")
        if self.hang_answer is not None:
            return self.hang_answer
        if deadline:
            wait_for(lambda: self.table.group_kills)
            return self.deadline_answer
        if self.write_result:
            # A Windows run also records how it presented, which a valid Windows result must carry.
            default = perf_result_tests.valid_result(presenter=perf_foreground_tests.WGPU_PRESENTER) if getattr(self, "windows_run", False) else perf_result_tests.valid_result()
            body = self.result_body if self.result_body is not None else default
            (scratch / "result.json").write_text(json.dumps(body), encoding="utf-8")
            # Extra scratch files a test names, such as guard-correlation sidecars, written beside the result.
            for relative, raw in getattr(self, "scratch_files", {}).items():
                (scratch / relative).parent.mkdir(parents=True, exist_ok=True)
                (scratch / relative).write_bytes(raw)
        output = "harness finished\n"
        if self.font_error:
            output = ('E config: Unable to load the configured primary font "Menlo" (weight=Regular, '
                      'stretch=Normal, style=Normal). Using "Symbols Nerd Font Mono" instead\n') + output
        return "PASS", 0, output

    def run_plan(self, deadline=False, smoke=True, environ=None, name=None, short=None, counters=False, side="head"):
        gate = perf_watcher_tests.FakeGate(lambda step: self.fake_harness(step, deadline))
        host = perf_outcomes.Host(gate, self.table, self.front_run, self.home, self.temp_root, set(),
                         environ or {"HOME": "/h"}, clock=lambda: perf_processes_tests.LAUNCH_UNIX_S)
        # Only a counters run names the field, so every other plan is built exactly as before.
        plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke" if smoke else side, Path("/b/perf_scenarios"),
                            perf_memory_tests.HARNESS_HASH, short=smoke if short is None else short, smoke=smoke,
                            kill_at_go=deadline, source_root=self.source_root,
                            **({"counters": True} if counters else {}))
        evidence = Path(self.temporary.name) / "evidence" / (name or ("deadline" if deadline else "run"))
        with contextlib.redirect_stdout(io.StringIO()):
            outcome = perf_execution.execute_run(plan, host, evidence)
        return outcome, evidence, gate

    def test_a_counters_run_passes_counters_and_reads_a_result_with_the_gate_on(self):
        # The harness gets --counters, and a result that reports the gate forced on passes the schema.
        self.result_body = perf_counter_schema_tests.counters_result()
        outcome, _evidence, gate = self.run_plan(smoke=False, counters=True)
        self.assertIn("--counters", gate.steps[0].argv)
        self.assertEqual(outcome.schema_problems, [])

    def test_a_base_counters_run_may_lack_a_field_its_tree_does_not_report(self):
        # The base's App is older, so a contract field it never had is not a schema failure on that side.
        result = perf_counter_schema_tests.counters_result()
        del result["phases"][0]["frame_counters"]["app"]["native_request_redraw_unregistered"]
        self.result_body = result
        outcome, _evidence, _gate = self.run_plan(smoke=False, counters=True, side="base")
        self.assertEqual(outcome.schema_problems, [])

    def test_a_head_counters_run_must_report_every_field(self):
        # The head's counters are the contract under test, so a field it leaves out is a schema failure.
        result = perf_counter_schema_tests.counters_result()
        del result["phases"][0]["frame_counters"]["app"]["native_request_redraw_unregistered"]
        self.result_body = result
        outcome, _evidence, _gate = self.run_plan(smoke=False, counters=True, side="head")
        self.assertTrue(any("native_request_redraw_unregistered" in problem for problem in outcome.schema_problems))

    def test_a_counters_run_whose_harness_ignored_the_flag_is_a_schema_failure(self):
        # A harness that ignored --counters measured nothing, so the run stops the comparison instead of a row of 0.
        outcome, _evidence, _gate = self.run_plan(smoke=False, counters=True)
        self.assertEqual(perf_outcomes.classify_outcome(outcome)[0], "schema")
        self.assertTrue(any("--counters" in problem for problem in outcome.schema_problems), outcome.schema_problems)

    def test_a_short_comparison_run_passes_short_to_the_harness_and_its_bound(self):
        # A comparison run planned short gets `--short` and the short deadline, not the full scenario's.
        _outcome, _evidence, gate = self.run_plan(smoke=False, short=True)
        self.assertIn("--short", gate.steps[0].argv)
        self.assertEqual(gate.steps[0].timeout_s, perf_scenarios.run_timeout_s(perf_watcher_tests.IDLE_SCENARIO, smoke=False, short=True))

    def test_managed_run_is_acknowledged_cleaned_and_recorded(self):
        # The whole run: acknowledgement before GO, cleanup after, and the evidence the brief lists.
        outcome, evidence, gate = self.run_plan()
        self.assertEqual(perf_outcomes.classify_outcome(outcome), ("valid", []))
        self.assertEqual(gate.steps[0].timeout_s, perf_scenarios.run_timeout_s(perf_watcher_tests.IDLE_SCENARIO, smoke=True))
        self.assertIn("--managed", gate.steps[0].argv)
        self.assertEqual(sorted(self.table.kills), [500, 501, 510])
        self.assertEqual(self.table.kills[-1], 501)
        self.assertEqual(len(outcome.memory), 1)
        for name in ("scratch/result.json", "scratch/logs/sonicterm.log.2026-10-02", "scratch/sessions/0.json",
                     "front-samples.log", "cleanup.json", "home-check.json", "outcome.json"):
            with self.subTest(name=name):
                self.assertTrue((evidence / name).is_file())
        self.assertEqual(list(self.temp_root.iterdir()), [])
        self.assertTrue(json.loads((evidence / "cleanup.json").read_text(encoding="utf-8"))["settled"])

    def test_deadline_case_kills_the_harness_group_and_cleanup_ends_the_sessions(self):
        # Killed at GO like a step deadline: no result, every PTY session still ended by cleanup.
        outcome, _evidence, _gate = self.run_plan(deadline=True)
        self.assertEqual(self.table.group_kills, [perf_watcher_tests.HARNESS_PID])
        self.assertEqual(sorted(self.table.kills), [500, 501, 510])
        self.assertIsNone(outcome.result)
        self.assertEqual(perf_outcomes.classify_outcome(outcome)[0], "valid")

    def test_a_session_without_a_valid_anchor_leaves_the_run_unresolved(self):
        # The run's cleanup lists the anchorless session's members in its evidence and signals none of them.
        self.table = perf_processes_tests.FakeTable(perf_watcher_tests.harness_process(), perf_processes_tests.FakeProcess(510, 500, 500, start="11"))
        self.skip_ack = True
        outcome, evidence, _gate = self.run_plan()
        self.assertEqual(perf_outcomes.classify_outcome(outcome)[0], "cleanup")
        self.assertEqual(self.table.kills, [])
        survivors = json.loads((evidence / "cleanup.json").read_text(encoding="utf-8"))["survivors"]
        self.assertEqual([member["pid"] for member in survivors], [510])

    def test_the_harness_runs_in_its_source_tree_and_logs_into_the_evidence(self):
        # asset_dir() finds development assets from the working directory, so a run's cwd is the tree that built it.
        _outcome, evidence, gate = self.run_plan()
        self.assertEqual(gate.roots, [self.source_root])
        self.assertTrue((evidence / "01-harness.log").is_file())

    def test_a_primary_font_that_failed_to_load_invalidates_the_run(self):
        # The App fell back to another font, so the run measured another renderer; the smoke fails at once.
        self.font_error = True
        outcome, _evidence, _gate = self.run_plan()
        kind, reasons = perf_outcomes.classify_outcome(outcome)
        self.assertEqual(kind, "font")
        self.assertIn("Menlo", reasons[0])
        self.assertEqual(perf_outcomes.smoke_verdict(kind), "fail")

    def test_an_unresolved_home_check_invalidates_the_run(self):
        # A home check that could not finish cannot clear the run of a write, so the run is invalid.
        unresolved = perf_home.HomeSnapshotUnresolved("more than 200000 entries under the home")
        with mock.patch.object(perf_home, "snapshot_home", side_effect=unresolved):
            outcome, evidence, _gate = self.run_plan()
        kind, reasons = perf_outcomes.classify_outcome(outcome)
        self.assertEqual(kind, "home")
        self.assertTrue(any("unresolved" in reason for reason in reasons))
        self.assertTrue(json.loads((evidence / "home-check.json").read_text(encoding="utf-8"))["unresolved"])

    def test_progress_from_a_killed_run_is_kept_as_evidence(self):
        # A run ended at its deadline keeps its earlier phases as evidence; they are never its result.
        self.progress = True
        outcome, evidence, _gate = self.run_plan(deadline=True)
        self.assertTrue((evidence / "scratch" / "progress.json").is_file())
        self.assertIsNone(outcome.result)
        self.assertEqual(perf_outcomes.classify_outcome(outcome)[0], "valid")

    def test_progress_never_stands_in_for_a_missing_result(self):
        # Without result.json a run classifies as before, even when progress.json looks like a valid result.
        self.progress, self.write_result = True, False
        outcome, evidence, _gate = self.run_plan()
        self.assertTrue((evidence / "scratch" / "progress.json").is_file())
        self.assertIsNone(outcome.result)
        self.assertEqual(perf_outcomes.classify_outcome(outcome), ("schema", ["exit 0 without result.json"]))

    def test_a_deadline_kill_run_step_never_collected_fails(self):
        # kill_group was accepted, but run_step's bounded reap never collected the harness: cleanup is unresolved.
        self.deadline_answer = ("TIMEOUT", None, "deadline reached; the leader was never reaped\n")
        outcome, _evidence, _gate = self.run_plan(deadline=True)
        self.assertTrue(outcome.deadline["sent"])
        kind, _reasons = perf_outcomes.classify_outcome(outcome)
        self.assertEqual(kind, "cleanup")
        self.assertEqual(perf_outcomes.smoke_verdict(kind), "fail")

    def test_a_run_step_deadline_after_settled_anchor_cleanup_stops_a_comparison(self):
        # The harness hangs until run_step's deadline kills and collects it. The anchor cleanup settles, but the
        # group was never counted, so the run is unresolved cleanup that stops a comparison, not a retried timeout.
        self.hang_answer = ("TIMEOUT", -9, "")
        outcome, evidence, _gate = self.run_plan(smoke=False)
        self.assertTrue(outcome.cleanup.passed)
        self.assertTrue(json.loads((evidence / "cleanup.json").read_text(encoding="utf-8"))["settled"])
        kind, reasons = perf_outcomes.classify_outcome(outcome)
        self.assertEqual(kind, "cleanup")
        self.assertTrue(any(perf_watcher_tests.UNCOUNTED_STOP in reason for reason in reasons), reasons)
        self.assertEqual(perf_outcomes.compare_verdict(kind), "stop")

    def test_focus_exception_covers_only_a_github_hosted_runner(self):
        # The harness becoming front after another app is theft, except on a GitHub-hosted runner, in the smoke and
        # in a comparison, where the evidence records the activation; a failed sample fails even there.
        hosted = {"HOME": "/h", "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
        self_hosted = dict(hosted, RUNNER_ENVIRONMENT="self-hosted")
        cases = {"github-hosted smoke": (hosted, True, [10, perf_watcher_tests.HARNESS_PID], "valid"),
                 "github-hosted comparison": (hosted, False, [10, perf_watcher_tests.HARNESS_PID], "valid"),
                 "self-hosted smoke": (self_hosted, True, [10, perf_watcher_tests.HARNESS_PID], "focus"),
                 "self-hosted comparison": (self_hosted, False, [10, perf_watcher_tests.HARNESS_PID], "focus"),
                 "github-hosted smoke with a failed sample": (hosted, True, [None], "focus"),
                 "github-hosted comparison with a failed sample": (hosted, False, [None], "focus")}
        for name, (environ, smoke, front_pids, expected) in cases.items():
            with self.subTest(name):
                self.table = perf_processes_tests.FakeTable(perf_processes_tests.leader(), perf_processes_tests.anchor(), perf_watcher_tests.harness_process(), perf_processes_tests.FakeProcess(510, 510, 500, start="11"))
                self.front_pids = list(front_pids)
                outcome, evidence, _gate = self.run_plan(smoke=smoke, environ=environ, name=name.replace(" ", "-"))
                self.assertEqual(perf_outcomes.classify_outcome(outcome)[0], expected)
                notes = json.loads((evidence / "outcome.json").read_text(encoding="utf-8"))["focus_notes"]
                self.assertEqual(bool(notes), name in ("github-hosted smoke", "github-hosted comparison"))

    def test_home_write_during_a_run_is_reported(self):
        # A write under the SonicTerm home invalidates the run and names the path.
        self.home_write = True
        outcome, evidence, _gate = self.run_plan()
        kind, reasons = perf_outcomes.classify_outcome(outcome)
        self.assertEqual(kind, "home")
        self.assertTrue(any("config.toml" in reason or "created" in reason for reason in reasons))
        self.assertTrue(json.loads((evidence / "home-check.json").read_text(encoding="utf-8"))["violations"])

    def test_windows_run_is_settled_by_job_custody(self):
        # On Windows the gate's job, not anchor cleanup, proves teardown; cleanup.json keeps its counts and members.
        self.windows_run = True
        self.table = perf_windows_tests.program_table()
        self.session_text = perf_windows_tests.PROGRAM_TEXT
        record = perf_windows_tests.custody(active=0, members=[perf_windows_tests.member(perf_watcher_tests.HARNESS_PID, "perf_scenarios.exe")])

        def run(name, custody_record):
            gate = perf_watcher_tests.FakeGate(lambda step: self.fake_harness(step, False))
            gate.custody = custody_record
            host = perf_outcomes.Host(gate, self.table, self.front_run, self.home, self.temp_root, set(), {"HOME": "/h"},
                             clock=lambda: perf_processes_tests.LAUNCH_UNIX_S, platform="win32")
            plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("C:/b/perf_scenarios.exe"), perf_memory_tests.HARNESS_HASH,
                                short=True, smoke=True, source_root=self.source_root)
            evidence = Path(self.temporary.name) / "evidence" / name
            with contextlib.redirect_stdout(io.StringIO()):
                return perf_execution.execute_run(plan, host, evidence), evidence, gate
        outcome, evidence, gate = run("windows", record)
        self.assertEqual(perf_outcomes.classify_outcome(outcome), ("valid", []))
        self.assertEqual((self.table.kills, self.table.group_kills), ([], []))
        self.assertEqual(gate.steps[0].hosts, ("windows",))
        recorded = json.loads((evidence / "cleanup.json").read_text(encoding="utf-8"))
        self.assertEqual(recorded["cleanup"], "none")
        self.assertEqual(recorded["before_cleanup"]["active_processes"], 0)
        self.assertEqual(recorded["before_cleanup"]["members"]["processes"][0]["pid"], perf_watcher_tests.HARNESS_PID)
        self.assertEqual([program["role"] for program in recorded["programs"]], ["0"])
        # Without a custody record the teardown is unproven, so the run fails as cleanup.
        outcome, _evidence, _gate = run("windows-no-custody", None)
        self.assertEqual(perf_outcomes.classify_outcome(outcome)[0], "cleanup")

    def test_windows_foreground_changes_are_listed_and_invalidate_a_desk_run(self):
        # On Windows the foreground window replaces lsappinfo; a change at a desk invalidates the run and is listed.
        self.windows_run = True
        self.table = perf_windows_tests.program_table()
        self.session_text = perf_windows_tests.PROGRAM_TEXT
        readings = []

        def front_sample():
            reading = perf_foreground_tests.foreground(1, 100 if not readings else 200)
            readings.append(reading)
            return reading

        def handler(step):
            # The run lasts until the sampler has seen the change, so the test does not depend on its timing.
            wait_for(lambda: len(readings) >= 2)
            return self.fake_harness(step, False)
        gate = perf_watcher_tests.FakeGate(handler)
        gate.custody = perf_windows_tests.custody(active=0)
        host = perf_outcomes.Host(gate, self.table, self.front_run, self.home, self.temp_root, set(), {"HOME": "/h"},
                         clock=lambda: perf_processes_tests.LAUNCH_UNIX_S, platform="win32", front_sample=front_sample)
        plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", "smoke", Path("C:/b/perf_scenarios.exe"), perf_memory_tests.HARNESS_HASH,
                            short=True, smoke=True, source_root=self.source_root)
        evidence = Path(self.temporary.name) / "evidence" / "foreground"
        with contextlib.redirect_stdout(io.StringIO()):
            outcome = perf_execution.execute_run(plan, host, evidence)
        self.assertEqual(perf_outcomes.classify_outcome(outcome)[0], "focus")
        recorded = json.loads((evidence / "outcome.json").read_text(encoding="utf-8"))
        self.assertEqual([(change["from_pid"], change["to_pid"]) for change in recorded["foreground_changes"]],
                         [(100, 200)])
        samples = (evidence / "front-samples.log").read_text(encoding="utf-8").splitlines()
        self.assertTrue(samples)
        self.assertTrue(all(json.loads(line)["argv"] == ["GetForegroundWindow"] for line in samples))
