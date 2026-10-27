"""Perf delivery tests."""

from __future__ import annotations
from perf_test_fixture import perf
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import perf_comparison_rows
import perf_delivery
import perf_memory_tests
import perf_metrics
import perf_scenarios
import perf_sets
import perf_settings
import perf_smoke
import perf_smoke_tests
import perf_watcher_tests
import perf_windows_tests


def delivery_record(**overrides):
    """A `delivery.json` record of one S10/sync replay whose every check passed."""
    record = {"schema_version": 2, "scenario": "S10", "variant": "sync", "bytes_kept": 4096,
              "checks": [{"name": "sync brackets", "ok": True, "detail": "enclosed 300, empty pair ahead 0, absent 0",
                          "unseen": 0, "brackets": 600, "unseen_markers": []}]}
    record.update(overrides)
    return record


def unseen_check(markers=("line 214 of 99999",), brackets=0, extra=0, **overrides):
    """A failed schema 2 `sync brackets` check whose frames `markers` (plus `extra` unlisted ones) never painted."""
    unseen = len(markers) + extra
    check = {"name": "sync brackets", "ok": False,
             "detail": f"enclosed 0, empty pair ahead 0, absent {300 - unseen}, never painted {unseen}",
             "unseen": unseen, "brackets": brackets, "unseen_markers": list(markers)}
    check.update(overrides)
    return check


def unseen_record(variant="default", **check_overrides):
    """A schema 2 S10 record whose only failed check is one retryable missing frame."""
    return delivery_record(variant=variant, checks=[unseen_check(**check_overrides)])


class DeliveryResultTests(unittest.TestCase):
    def write(self, directory, record):
        """Write `record` as the replay's `delivery.json` and return the scratch path."""
        path = Path(directory) / "delivery.json"
        path.write_text(record if isinstance(record, str) else json.dumps(record), encoding="utf-8")
        return Path(directory)

    def test_a_passed_replay_becomes_one_row_per_check(self):
        # Each check of the replay is a table row; both sides share one replay, so both cells hold its detail.
        with tempfile.TemporaryDirectory() as directory:
            record, problem = perf_delivery.read_delivery(self.write(directory, delivery_record()), "S10", "sync")
        self.assertIsNone(problem)
        rows = perf_delivery.delivery_rows("S10/sync", record, problem)
        self.assertEqual(rows, [["S10/sync", "delivery: sync brackets", "enclosed 300, empty pair ahead 0, absent 0",
                                 "enclosed 300, empty pair ahead 0, absent 0", perf_metrics.DELIVERY_NOTE]])

    def test_a_failed_check_blocks_the_scenario_and_names_it(self):
        # A failed check is the scenario's blocked reason, naming the check and its detail.
        failed = delivery_record(checks=[
            delivery_record()["checks"][0],
            {"name": "delivered lines", "ok": False, "detail": "240960 delivered, 240961 planned"}])
        with tempfile.TemporaryDirectory() as directory:
            record, problem = perf_delivery.read_delivery(self.write(directory, failed), "S10", "sync")
        self.assertEqual(problem, "delivery check failed: delivered lines: 240960 delivered, 240961 planned")
        rows = perf_delivery.delivery_rows("S10/sync", record, problem)
        self.assertEqual(rows[1][4], "blocked: " + problem)

    def test_a_missing_or_unreadable_record_blocks_the_scenario(self):
        # No file, broken JSON, another schema, another scenario and no checks each block with a reason.
        cases = {
            "missing": None,
            "broken": "{not json",
            "schema": delivery_record(schema_version=3),
            "scenario": delivery_record(scenario="S9"),
            "variant": delivery_record(variant="default"),
            "no checks": delivery_record(checks=[]),
            "check shape": delivery_record(checks=[{"name": "sync brackets", "ok": "yes", "detail": ""}]),
        }
        for name, content in cases.items():
            with self.subTest(case=name), tempfile.TemporaryDirectory() as directory:
                scratch = Path(directory) if content is None else self.write(directory, content)
                record, problem = perf_delivery.read_delivery(scratch, "S10", "sync")
                self.assertIsNone(record)
                self.assertTrue(problem and problem.startswith("delivery.json"), problem)
                self.assertEqual(perf_delivery.delivery_rows("S10/sync", record, problem),
                                 [["S10/sync", "delivery", "blocked", "blocked", "blocked: " + problem]])

    def test_a_malformed_schema_2_record_is_never_trusted(self):
        # A schema 2 record is admitted or retried only when every structured field is typed and in range and
        # the detail is the exact classification whose numbers agree with them; anything else blocks.
        broken_fields = delivery_record(checks=[dict(delivery_record()["checks"][0], unseen="0")])
        no_brackets = dict(delivery_record()["checks"][0])
        del no_brackets["brackets"]
        cases = {
            "a non-classification detail with a valid suffix": unseen_record(
                detail="not a frame classification, never painted 1"),
            "a passing record whose unseen count is a string": broken_fields,
            "a passing record with no bracket count": delivery_record(checks=[no_brackets]),
            "a passing record with a negative bracket count": delivery_record(checks=[
                dict(delivery_record()["checks"][0], brackets=-1)]),
            "a passing record whose markers are not a list": delivery_record(checks=[
                dict(delivery_record()["checks"][0], unseen_markers="none")]),
            "a passing verdict the counts contradict": unseen_record(ok=True),
            "a failing verdict the counts contradict": delivery_record(checks=[
                dict(delivery_record()["checks"][0], ok=False)]),
            "enclosed frames with no bracket": unseen_record(
                detail="enclosed 2, empty pair ahead 0, absent 297, never painted 1"),
            "no sync brackets check for S10": delivery_record(checks=[
                {"name": "completion", "ok": True, "detail": "arrived"}]),
            "schema 2.0": delivery_record(schema_version=2.0),
            "schema true": delivery_record(schema_version=True),
            "a bytes_kept that is not a count": delivery_record(bytes_kept="4096"),
        }
        for name, content in cases.items():
            with self.subTest(case=name), tempfile.TemporaryDirectory() as directory:
                variant = content["variant"]
                record, problem = perf_delivery.read_delivery(self.write(directory, content), "S10", variant)
                self.assertIsNone(record)
                self.assertTrue(problem and problem.startswith("delivery.json"), problem)

    def test_a_version_1_record_is_still_read(self):
        # A harness from before the structured frame fields writes schema 1; its checks still fill the table.
        legacy = delivery_record(schema_version=1, checks=[
            {"name": "sync brackets", "ok": True, "detail": "enclosed 300, empty pair ahead 0, absent 0"}])
        with tempfile.TemporaryDirectory() as directory:
            record, problem = perf_delivery.read_delivery(self.write(directory, legacy), "S10", "sync")
        self.assertIsNone(problem)
        self.assertEqual(record["schema_version"], 1)

    def test_the_replay_command_line_names_capture_delivery(self):
        # The replay is the harness's `--capture-delivery` mode, with the scenario, variant and run length.
        argv = perf_delivery.capture_delivery_argv(Path("C:/build/perf_scenarios.exe"), "S10", "sync",
                                          Path("C:/tmp/replay"), short=True)
        self.assertEqual(argv, (str(Path("C:/build/perf_scenarios.exe")), "--run", "S10", "--variant", "sync",
                                "--short", "--capture-delivery", str(Path("C:/tmp/replay"))))
        self.assertNotIn("--short", perf_delivery.capture_delivery_argv(Path("h"), "S3", "default", Path("s")))

    def test_only_windows_replays_the_delivered_scenarios(self):
        # S3, S9, S10 and S11 get a replay on Windows; no scenario does on macOS.
        for scenario_id in ("S3", "S9", "S10", "S11"):
            self.assertTrue(perf_delivery.delivery_replayed(scenario_id, "win32"))
            self.assertFalse(perf_delivery.delivery_replayed(scenario_id, "darwin"))
        self.assertFalse(perf_delivery.delivery_replayed("S1", "win32"))


VERIFIED = object()


class DeliveryReplayTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.temp_root, self.evidence = root / "temp", root / "evidence"
        self.temp_root.mkdir()
        self.evidence.mkdir()

    def tearDown(self):
        self.temporary.cleanup()

    def replay_attempts(self, attempts, variant="sync", platform_name="win32", evidence=None):
        """Run one S10 replay whose harness answers its Nth attempt from `attempts[N - 1]`.

        Each attempt is a dict: `record` (None writes nothing), `status`, `exit_code`, `custody` (verified unless
        given; None records none), `leftover`, and `text`, the delivered text the harness keeps beside its record.
        A replay that asks for more attempts than given fails the test.
        """
        pending = list(attempts)
        gate = perf_watcher_tests.FakeGate(None)

        def answer(step):
            if not pending:
                raise AssertionError(f"attempt {len(gate.steps)} was not expected")
            attempt = pending.pop(0)
            scratch = Path(step.argv[-1])
            scratch.mkdir(parents=True, exist_ok=True)
            if attempt.get("text") is not None:
                # Bytes, not write_text: Windows would turn each LF into CR LF, and the record's bytes_kept
                # counts the delivered bytes exactly.
                (scratch / "delivery.txt").write_bytes(attempt["text"].encode("utf-8"))
            if attempt.get("record") is not None:
                (scratch / "delivery.json").write_text(json.dumps(attempt["record"]), encoding="utf-8")
            # FakeGate reads custody and leftover after the handler answers, so each attempt sets its own.
            custody_record = attempt.get("custody", VERIFIED)
            gate.custody = perf_windows_tests.custody() if custody_record is VERIFIED else custody_record
            gate.leftover = attempt.get("leftover", 0)
            return attempt.get("status", "PASS"), attempt.get("exit_code", 0), f"replay log {len(gate.steps)}\n"
        gate.handler = answer
        result = perf_delivery.run_delivery_replay(gate, Path("h.exe"), "S10", variant, evidence or self.evidence, 3,
                                          short=True, timeout_s=60, temp_root=self.temp_root,
                                          environ={"NO_COLOR": "1"}, platform_name=platform_name)
        return gate, result

    def replay(self, record, status="PASS", exit_code=0, custody_record=VERIFIED, platform_name="win32",
               leftover=0):
        """Run one S10/sync replay whose single attempt writes `record` (None writes nothing) and ends as given."""
        attempt = {"record": record, "status": status, "exit_code": exit_code, "custody": custody_record,
                   "leftover": leftover}
        return self.replay_attempts([attempt], platform_name=platform_name)

    @staticmethod
    def retryable(variant="default", **check_overrides):
        """An attempt that missed frame markers: exit 5, a schema 2 record and the delivered text it states."""
        text = "partial frames\n"
        record = unseen_record(variant, **check_overrides)
        record["bytes_kept"] = len(text.encode("utf-8"))
        return {"record": record, "status": "FAIL", "exit_code": 5, "text": text}

    @staticmethod
    def passed(variant="default"):
        """An attempt whose every check passed, with kept delivered text."""
        brackets = 600 if variant == "sync" else 0
        check = {"name": "sync brackets", "ok": True, "detail": "enclosed 0, empty pair ahead 0, absent 300",
                 "unseen": 0, "brackets": brackets, "unseen_markers": []}
        return {"record": delivery_record(variant=variant, checks=[check]), "status": "PASS", "exit_code": 0,
                "text": "every frame\n"}

    def test_a_replay_whose_job_custody_is_unverified_stops_the_comparison(self):
        # Processes the replay may have left would disturb every later run, as a measured run's would,
        # so an unverified or missing custody stops the lifecycle instead of blocking one scenario.
        for custody_record in (perf_windows_tests.custody(empty=False, errors=["3 members alive"]), None):
            with self.subTest(custody=custody_record),                     self.assertRaisesRegex(perf_sets.StopComparison, "delivery replay S10/sync: unresolved cleanup"):
                self.replay(delivery_record(), custody_record=custody_record)
        self.assertFalse(any(self.temp_root.iterdir()))

    def test_a_timed_out_replay_with_verified_custody_only_blocks_its_scenario(self):
        # A deadline whose job was emptied is the scenario's problem, not the comparison's.
        _gate, (record, problem, _note) = self.replay(None, "TIMEOUT", 124)
        self.assertIsNone(record)
        self.assertIn("TIMEOUT, exit 124", problem)

    def test_a_posix_replay_that_left_processes_stops_the_comparison(self):
        # Without a job, the gate's leftover count proves teardown; an unknown count proves nothing.
        for leftover in (2, None):
            with self.subTest(leftover=leftover),                     self.assertRaisesRegex(perf_sets.StopComparison, "delivery replay S10/sync"):
                self.replay(delivery_record(), platform_name="darwin", leftover=leftover)
        _gate, (_record, problem, _note) = self.replay(delivery_record(), platform_name="darwin")
        self.assertIsNone(problem)

    def test_the_smoke_fails_at_once_when_its_replay_stops(self):
        # A smoke replay with unproven teardown fails the smoke before any case runs.
        scenarios = dict(perf_smoke_tests.SmokeTests.SCENARIOS, S1=perf_scenarios.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 300, 80),
                         S10=perf_scenarios.Scenario("S10", ("default", "sync"), "Redraw", 300, 240))
        cases = []

        def replay(scenario, variant, evidence):
            raise perf_sets.StopComparison("delivery replay S10/sync: unresolved cleanup: job not empty")
        with contextlib.redirect_stdout(io.StringIO()):
            code, reasons = perf_smoke.smoke_cases(scenarios, Path("/b"), perf_memory_tests.HARNESS_HASH,
                                             lambda plan, evidence: cases.append(plan), self.evidence,
                                             host_platform="win32", replay=replay)
        self.assertEqual(code, perf_settings.EXIT_FAIL)
        self.assertEqual(reasons, ["delivery replay S10/sync: unresolved cleanup: job not empty"])
        self.assertEqual(cases, [])

    def test_a_passed_replay_keeps_its_record_as_evidence_and_removes_its_scratch(self):
        # The replay runs `--capture-delivery` in a fresh scratch under the temp root, without NO_COLOR;
        # its record is copied into the evidence and the scratch is removed.
        gate, (record, problem, note) = self.replay(delivery_record())
        self.assertIsNone(problem)
        self.assertEqual(record["scenario"], "S10")
        # Even a first-attempt pass states how many attempts it took.
        self.assertEqual(note, perf_metrics.DELIVERY_NOTE + "; passed on attempt 1 of 3")
        argv = gate.steps[0].argv
        self.assertEqual(argv[:-1], ("h.exe", "--run", "S10", "--variant", "sync", "--short", "--capture-delivery"))
        self.assertEqual(Path(argv[-1]).parent, self.temp_root)
        self.assertFalse(Path(argv[-1]).exists())
        self.assertEqual(gate.steps[0].timeout_s, 60)
        self.assertNotIn("NO_COLOR", gate.environs[0])
        self.assertTrue((self.evidence / "delivery-S10-sync-attempt1.json").exists())
        self.assertEqual(len(gate.steps), 1)

    def test_a_replay_with_no_record_names_its_step_status(self):
        # A harness that wrote nothing blocks the scenario with the step's status and exit code.
        _gate, (record, problem, _note) = self.replay(None, "FAIL", 1)
        self.assertIsNone(record)
        self.assertIn("FAIL", problem)
        self.assertIn("exit 1", problem)

    def test_a_failed_check_keeps_the_record_and_its_reason(self):
        # A failed check exits as blocked (5); the record stays so the table shows every check.
        failed = delivery_record(schema_version=1,
                                 checks=[{"name": "sync brackets", "ok": False, "detail": "absent 300"}])
        _gate, (record, problem, _note) = self.replay(failed, "FAIL", 5)
        self.assertIsNotNone(record)
        self.assertEqual(problem, "delivery check failed: sync brackets: absent 300")

    def test_a_record_that_disagrees_with_the_exit_blocks(self):
        # A passing record from a harness that did not exit 0 is not trusted.
        _gate, (_record, problem, _note) = self.replay(delivery_record(), "FAIL", 5)
        self.assertIn("passed every check, but the replay ended FAIL, exit 5", problem)

    def test_blocked_sets_block_both_sides_of_every_set(self):
        # A scenario whose delivery is blocked runs no set; each set reports both sides blocked.
        results = perf_comparison_rows.blocked_set_results("S10/sync", "delivery check failed: x", ("timed", "laps"))
        self.assertEqual([result.set_name for result in results], ["timed", "laps"])
        for result in results:
            self.assertEqual((result.base.blocked, result.head.blocked),
                             ("delivery check failed: x", "delivery check failed: x"))
        self.assertEqual(perf_sets.comparison_exit(results), perf_settings.EXIT_BLOCKED)

    def test_the_windows_smoke_replays_s10_sync_and_a_failed_replay_blocks_it(self):
        # On Windows the smoke replays S10/sync before its runs; a blocked replay makes it exit 3, and
        # macOS never replays.
        scenarios = dict(perf_smoke_tests.SmokeTests.SCENARIOS, S1=perf_scenarios.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 300, 80),
                         S10=perf_scenarios.Scenario("S10", ("default", "sync"), "Redraw", 300, 240))
        replays = []

        def replay(scenario, variant, evidence):
            replays.append((scenario.id, variant))
            return perf_delivery.DeliveryOutcome(None, "delivery.json is missing", None)

        def run_case(plan, evidence):
            kind = "invalid" if plan.variant == "role-exit" else "valid"
            return perf_smoke_tests.outcome_of(kind)(plan)
        for platform_name, expected_replays, expected_code in (("win32", [("S10", "sync")], perf_settings.EXIT_BLOCKED),
                                                               ("darwin", [], perf_settings.EXIT_PASS)):
            replays.clear()
            with self.subTest(platform=platform_name), contextlib.redirect_stdout(io.StringIO()), \
                    mock.patch.object(perf_smoke, "case_verdict", lambda case, kind, reasons: (
                        "pass" if kind == "valid" or case.expected == kind else "fail", list(reasons))):
                code, reasons = perf_smoke.smoke_cases(scenarios, Path("/b"), perf_memory_tests.HARNESS_HASH, run_case, self.evidence,
                                                 host_platform=platform_name, replay=replay)
            self.assertEqual(replays, expected_replays)
            self.assertEqual(code, expected_code)
            if expected_code == perf_settings.EXIT_BLOCKED:
                self.assertIn("S10/sync delivery: not exercised: delivery.json is missing", reasons)


    def test_one_missing_frame_is_retried_and_a_passing_attempt_admits_the_scenario(self):
        # A replay that never found one frame marker (exit 5, schema 2, no bracket in `default`) is retried in a
        # fresh scratch; the passing second attempt admits S10, and the note discloses attempt 1 and its marker.
        gate, outcome = self.replay_attempts([self.retryable(), self.passed()], variant="default")
        self.assertIsNone(outcome.problem)
        self.assertTrue(outcome.record["checks"][0]["ok"])
        self.assertEqual(outcome.note, "untimed ConPTY replay by the head build, shared by both sides; passed on "
                                       "attempt 2 of 3; attempt 1: enclosed 0, empty pair ahead 0, absent 299, "
                                       "never painted 1 (marker `line 214 of 99999`)")
        self.assertEqual(perf_delivery.delivery_rows("S10/default", *outcome)[0][4], outcome.note)
        # Each attempt has its own scratch, removed afterwards, its own step and the same outer deadline.
        scratches = [Path(step.argv[-1]) for step in gate.steps]
        self.assertEqual(len(set(scratches)), 2)
        self.assertFalse(any(path.exists() for path in scratches))
        self.assertEqual([step.id for step in gate.steps],
                         ["delivery-S10-default-attempt1", "delivery-S10-default-attempt2"])
        self.assertEqual([step.timeout_s for step in gate.steps], [60, 60])
        # Both attempts keep their record, delivered text and log.
        self.assertEqual((self.evidence / "delivery-S10-default-attempt1.txt").read_text(encoding="utf-8"),
                         "partial frames\n")
        first = json.loads((self.evidence / "delivery-S10-default-attempt1.json").read_text(encoding="utf-8"))
        self.assertEqual(first["checks"][0]["unseen_markers"], ["line 214 of 99999"])
        self.assertTrue((self.evidence / "delivery-S10-default-attempt2.json").is_file())
        self.assertTrue((self.evidence / "delivery-S10-default-attempt2.txt").is_file())
        self.assertEqual(sorted(path.name for path in self.evidence.glob("*.log")),
                         ["03-delivery-S10-default-attempt1.log", "03-delivery-S10-default-attempt2.log"])

    def test_three_retryable_attempts_block_and_disclose_every_detail(self):
        # The retry is bounded: a third missing-frame attempt blocks S10, and the note gives all three details.
        markers = ("line 214 of 99999", "Uptime frame 160", "line 3 of 99999")
        gate, outcome = self.replay_attempts([self.retryable(markers=(marker,)) for marker in markers],
                                             variant="default")
        self.assertEqual(len(gate.steps), 3)
        self.assertEqual(outcome.problem, "delivery check failed: sync brackets: enclosed 0, empty pair ahead 0, "
                                          "absent 299, never painted 1")
        self.assertIn("; blocked on attempt 3 of 3; no attempts left", outcome.note)
        for attempt, marker in enumerate(markers, start=1):
            self.assertIn(f"attempt {attempt}: enclosed 0, empty pair ahead 0, absent 299, never painted 1 "
                          f"(marker `{marker}`)", outcome.note)
        self.assertEqual(perf_delivery.delivery_rows("S10/default", *outcome)[0][4],
                         f"blocked: {outcome.problem}; {outcome.note}")

    def test_sync_retries_with_brackets_and_lists_at_most_eight_markers(self):
        # `sync` accepts any bracket placement, so a missing frame with brackets is retried there; a long list
        # of missing markers is named up to the harness's eight, then counted.
        markers = tuple(f"line {number} of 99999" for number in range(1, 9))
        _gate, outcome = self.replay_attempts([self.retryable("sync", markers=markers, brackets=37, extra=3),
                                               self.passed("sync")])
        self.assertIsNone(outcome.problem)
        self.assertIn("never painted 11 (markers `line 1 of 99999`, `line 2 of 99999`, `line 3 of 99999`, "
                      "`line 4 of 99999`, `line 5 of 99999`, `line 6 of 99999`, `line 7 of 99999`, "
                      "`line 8 of 99999`, and 3 more)", outcome.note)

    def test_every_other_failure_blocks_on_its_first_attempt(self):
        # Only one missing-frame shape is retried; everything else blocks where it happens, as before.
        sentinel = {"name": "completion", "ok": False,
                    "detail": "the sentinel did not arrive before the scenario's timeout"}
        no_brackets = unseen_check()
        del no_brackets["brackets"]
        legacy = {"name": "sync brackets", "ok": False,
                  "detail": "enclosed 0, empty pair ahead 0, absent 299, never painted 1"}
        cases = {
            "default with an unseen frame and brackets": self.retryable(brackets=4),
            "default with brackets only": self.retryable(markers=(), brackets=4,
                                                         detail="enclosed 0, empty pair ahead 0, absent 300"),
            "another failing check": {"record": delivery_record(variant="default", checks=[sentinel]),
                                      "status": "FAIL", "exit_code": 5},
            "two failing checks": {"record": delivery_record(variant="default", checks=[unseen_check(), sentinel]),
                                   "status": "FAIL", "exit_code": 5},
            "a detail that disagrees": self.retryable(detail="enclosed 0, empty pair ahead 0, absent 298, "
                                                             "never painted 2"),
            "a detail with no count": self.retryable(detail="absent 300"),
            "an unseen count that is not a number": self.retryable(unseen="1"),
            "a zero unseen count": self.retryable(markers=(), detail="enclosed 0, empty pair ahead 0, absent 300"),
            "markers that disagree with the count": self.retryable(unseen_markers=[]),
            "markers that are not strings": self.retryable(unseen_markers=[214]),
            "no bracket count": {"record": delivery_record(variant="default", checks=[no_brackets]),
                                 "status": "FAIL", "exit_code": 5},
            "a version 1 record": {"record": delivery_record(schema_version=1, variant="default", checks=[legacy]),
                                   "status": "FAIL", "exit_code": 5},
            "exit 1": dict(self.retryable(), exit_code=1),
            "a timeout": dict(self.retryable(), status="TIMEOUT", exit_code=124),
            "a crash after the record": dict(self.retryable(), exit_code=3221225477),
            "a launch failure": dict(self.retryable(), status="LAUNCH", exit_code=None),
            "a missing record": {"record": None, "status": "FAIL", "exit_code": 5},
            "a passing step with a failed record": dict(self.retryable(), status="PASS", exit_code=0),
            "a non-classification detail with a valid suffix": self.retryable(
                detail="not a frame classification, never painted 1"),
            "schema 2.0": dict(self.retryable(), record=dict(self.retryable()["record"], schema_version=2.0)),
            "schema true": dict(self.retryable(), record=dict(self.retryable()["record"], schema_version=True)),
            "no delivered text": dict(self.retryable(), text=None),
            "delivered text shorter than the record states": dict(self.retryable(), text="partial"),
        }
        for name, attempt in cases.items():
            with self.subTest(case=name):
                for stale_file in self.evidence.iterdir():
                    stale_file.unlink()
                gate, outcome = self.replay_attempts([attempt], variant="default")
                self.assertEqual(len(gate.steps), 1)
                self.assertIsNotNone(outcome.problem)
                self.assertEqual(outcome.note, perf_metrics.DELIVERY_NOTE + "; blocked on attempt 1 of 3; not retryable")

    def test_a_failed_record_whose_replay_did_not_end_blocked_names_how_it_ended(self):
        # A later attempt that writes the same failed record but crashes, times out or exits other than 5 is
        # not retried, and its blocked reason names the step's real end, not only the record's failed check.
        for status, exit_code in (("FAIL", 3221225477), ("TIMEOUT", 124), ("FAIL", 1)):
            with self.subTest(status=status, exit_code=exit_code):
                for stale_file in self.evidence.iterdir():
                    stale_file.unlink()
                gate, outcome = self.replay_attempts(
                    [self.retryable(), dict(self.retryable(), status=status, exit_code=exit_code)], variant="default")
                self.assertEqual(len(gate.steps), 2)
                self.assertIn("never painted 1", outcome.problem)
                self.assertIn(f"the replay ended {status}, exit {exit_code}", outcome.problem)
                self.assertTrue(outcome.note.startswith(
                    perf_metrics.DELIVERY_NOTE + "; blocked on attempt 2 of 3; not retryable; attempt 1: "))

    def test_a_retryable_attempt_then_another_failure_blocks_with_the_second_reason(self):
        # A non-retryable failure after a retryable one blocks at once with its own reason, disclosing attempt 1.
        gate, outcome = self.replay_attempts([self.retryable(), {"record": None, "status": "FAIL", "exit_code": 1}],
                                             variant="default")
        self.assertEqual(len(gate.steps), 2)
        self.assertIsNone(outcome.record)
        self.assertIn("delivery.json is missing", outcome.problem)
        self.assertIn("the replay ended FAIL, exit 1", outcome.problem)
        self.assertEqual(outcome.note, perf_metrics.DELIVERY_NOTE + "; blocked on attempt 2 of 3; not retryable; attempt 1: "
                                       "enclosed 0, empty pair ahead 0, absent 299, never painted 1 "
                                       "(marker `line 214 of 99999`)")
        self.assertEqual(perf_delivery.delivery_rows("S10/default", *outcome),
                         [["S10/default", "delivery", "blocked", "blocked",
                           f"blocked: {outcome.problem}; {outcome.note}"]])

    def test_unproven_teardown_on_any_attempt_stops_the_comparison(self):
        # A retry never hides unproven teardown: whichever attempt leaves it raises StopComparison.
        unverified = perf_windows_tests.custody(empty=False, errors=["2 members alive"])
        for failing_attempt in (1, 2):
            attempts = [self.retryable(), self.retryable()]
            attempts[failing_attempt - 1] = dict(attempts[failing_attempt - 1], custody=unverified)
            with self.subTest(attempt=failing_attempt), \
                    self.assertRaisesRegex(perf_sets.StopComparison, "delivery replay S10/default: unresolved cleanup"):
                self.replay_attempts(attempts, variant="default")

    def test_the_smoke_applies_the_same_retry_rule(self):
        # The Windows smoke reaches the replay through run_delivery_replay too, so one missing frame is retried
        # there, a pass on attempt 2 lets the smoke pass, and three missing frames block it with every detail.
        scenarios = dict(perf_smoke_tests.SmokeTests.SCENARIOS, S1=perf_scenarios.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 300, 80),
                         S10=perf_scenarios.Scenario("S10", ("default", "sync"), "Redraw", 300, 240))

        def run_case(plan, evidence):
            kind = "invalid" if plan.variant == "role-exit" else "valid"
            return perf_smoke_tests.outcome_of(kind)(plan)
        sequences = {
            "retried": ([self.retryable("sync"), self.passed("sync")], perf_settings.EXIT_PASS),
            "exhausted": ([self.retryable("sync")] * 3, perf_settings.EXIT_BLOCKED),
        }
        for name, (attempts, expected_code) in sequences.items():
            for stale_file in self.evidence.iterdir():
                stale_file.unlink()
            output = io.StringIO()

            def replay(scenario, variant, evidence, attempts=attempts):
                return self.replay_attempts(attempts, variant=variant)[1]
            with self.subTest(sequence=name), contextlib.redirect_stdout(output), \
                    mock.patch.object(perf_smoke, "case_verdict", lambda case, kind, reasons: (
                        "pass" if kind == "valid" or case.expected == kind else "fail", list(reasons))):
                code, reasons = perf_smoke.smoke_cases(scenarios, Path("/b"), perf_memory_tests.HARNESS_HASH, run_case, self.evidence,
                                                 host_platform="win32", replay=replay)
            self.assertEqual(code, expected_code, reasons)
            if expected_code == perf_settings.EXIT_PASS:
                self.assertIn("passed on attempt 2 of 3", output.getvalue())
            else:
                self.assertTrue(any("blocked on attempt 3 of 3" in reason and "attempt 3: " in reason
                                    for reason in reasons), reasons)


    def test_a_passing_record_with_malformed_fields_is_not_admitted(self):
        # Validation runs before admission too: a passing step whose record has broken fields blocks S10.
        broken = self.passed()
        broken["record"]["checks"][0]["unseen"] = None
        gate, outcome = self.replay_attempts([broken], variant="default")
        self.assertEqual(len(gate.steps), 1)
        self.assertIsNone(outcome.record)
        self.assertIn("malformed", outcome.problem)

    def test_a_retry_needs_its_delivered_text_within_the_cap(self):
        # A retried attempt's classified bytes are its only evidence, so a missing text file blocks instead of
        # retrying, and so does text past the harness's kept-output cap.
        gate, outcome = self.replay_attempts([dict(self.retryable(), text=None)], variant="default")
        self.assertEqual(len(gate.steps), 1)
        self.assertIn("delivery-S10-default-attempt1.txt is missing", outcome.problem)
        self.assertEqual(outcome.note, perf_metrics.DELIVERY_NOTE + "; blocked on attempt 1 of 3; not retryable")
        for stale_file in self.evidence.iterdir():
            stale_file.unlink()
        with mock.patch.object(perf_metrics, "DELIVERY_TEXT_LIMIT_BYTES", 4):
            gate, outcome = self.replay_attempts([self.retryable()], variant="default")
        self.assertEqual(len(gate.steps), 1)
        self.assertIn("passes the 4-byte cap", outcome.problem)

    def test_a_retried_smoke_pass_keeps_and_flags_its_evidence(self):
        # smoke_main deletes a passing smoke's evidence, but not when a replay was retried: then every attempt's
        # record, text and log stay, and the job is told to upload them. An unretried pass still removes them.
        scenarios = dict(perf_smoke_tests.SmokeTests.SCENARIOS, S1=perf_scenarios.Scenario("S1", ("default", "wgpu", "role-exit"), "Idle", 300, 80),
                         S10=perf_scenarios.Scenario("S10", ("default", "sync"), "Redraw", 300, 240))

        def run_case(plan, evidence):
            kind = "invalid" if plan.variant == "role-exit" else "valid"
            return perf_smoke_tests.outcome_of(kind)(plan)
        for name, attempts in (("retried", [self.retryable("sync"), self.passed("sync")]),
                               ("first try", [self.passed("sync")])):
            kept = []

            def runner(evidence, attempts=attempts):
                kept.append(evidence)

                def replay(scenario, variant, replay_evidence):
                    return self.replay_attempts(attempts, variant=variant, evidence=replay_evidence)[1]
                return perf_smoke.smoke_cases(scenarios, Path("/b"), perf_memory_tests.HARNESS_HASH, run_case, evidence,
                                        host_platform="win32", replay=replay)
            env_file = self.evidence / f"github-env-{name.replace(' ', '-')}"
            output = io.StringIO()
            with self.subTest(sequence=name), mock.patch.object(perf.sys, "platform", "win32"), \
                    contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()), \
                    mock.patch.object(perf_smoke, "case_verdict", lambda case, kind, reasons: (
                        "pass" if kind == "valid" or case.expected == kind else "fail", list(reasons))):
                self.assertEqual(perf_smoke.smoke_main({"GITHUB_ENV": str(env_file)}, runner), perf_settings.EXIT_PASS)
                exported = env_file.read_text(encoding="utf-8")
                if name == "retried":
                    self.assertTrue(kept[0].is_dir())
                    self.assertEqual(sorted(path.name for path in kept[0].iterdir()),
                                     ["03-delivery-S10-sync-attempt1.log", "03-delivery-S10-sync-attempt2.log",
                                      "delivery-S10-sync-attempt1.json", "delivery-S10-sync-attempt1.txt",
                                      "delivery-S10-sync-attempt2.json", "delivery-S10-sync-attempt2.txt"])
                    self.assertIn(f"{perf_delivery.REPLAY_RETRIED_ENV}=1\n", exported)
                    self.assertIn("a delivery replay was retried", output.getvalue())
                    perf.shutil.rmtree(kept[0])
                else:
                    self.assertFalse(kept[0].exists())
                    self.assertNotIn(perf_delivery.REPLAY_RETRIED_ENV, exported)

    def test_ci_uploads_the_windows_smoke_evidence_on_a_retried_pass(self):
        # A retried pass is green, so the Windows upload step must also run when smoke_main flags a retry.
        workflow = (Path(__file__).resolve().parent.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8")
        step = workflow[workflow.index("      - name: Upload Windows perf scenario smoke evidence\n"):]
        condition = step.splitlines()[1]
        self.assertEqual(condition, "        if: ${{ (failure() || env.%s == '1') && "
                                    "env.SONICTERM_PERF_EVIDENCE_DIR != '' }}" % perf_delivery.REPLAY_RETRIED_ENV)
