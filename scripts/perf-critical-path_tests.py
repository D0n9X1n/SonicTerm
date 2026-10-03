#!/usr/bin/env python3
"""Contracts of the performance comparison's critical-path accounting, driven by recorded and synthetic runs.

No test calls GitHub: the recorded run 37118041050 (two attempts, its Windows S9-S10 shard rerun) is a
fixture copied from `gh api`, with steps kept for the jobs its critical paths use, and every other run
is built here.
"""

from __future__ import annotations

import calendar
import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

SPEC = importlib.util.spec_from_file_location("perf_critical_path", Path(__file__).with_name("perf-critical-path.py"))
assert SPEC and SPEC.loader
accounting = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = accounting
SPEC.loader.exec_module(accounting)

FIXTURE = Path(__file__).with_name("perf-critical-path_fixture.json")
# Synthetic runs start here; every synthetic time below is seconds after it.
ORIGIN_S = calendar.timegm((2026, 10, 3, 12, 0, 0))
RUN_ID = 99
SHA = "c" * 40
PRODUCER = "macOS perf binaries (base and head)"
RESULT = "Performance comparison result"


def stamp(offset_s):
    """The UTC timestamp `offset_s` seconds after the synthetic origin, as GitHub writes it."""
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ORIGIN_S + offset_s))


def step(name, started, completed, number, conclusion="success"):
    """One job step between two synthetic offsets."""
    return {"name": name, "status": "completed", "conclusion": conclusion, "number": number,
            "started_at": stamp(started), "completed_at": stamp(completed)}


def quiet_steps(started, completed):
    """A job that only set up: its compare step skipped, so it has no evidence to bind."""
    return [step("Set up job", started, completed, 1), step("Compare the base and the head", completed, completed, 2,
                                                             "skipped")]


def job(name, created, started, completed, attempt=1, steps=None, conclusion="success", runner=None):
    """One job row of a synthetic run, with GitHub's fields that the accounting reads."""
    job.next_id = getattr(job, "next_id", 1000) + 1
    return {"id": job.next_id, "name": name, "run_attempt": attempt, "created_at": stamp(created),
            "started_at": stamp(started), "completed_at": stamp(completed), "conclusion": conclusion,
            "status": "completed", "runner_id": runner if runner is not None else job.next_id,
            "labels": ["macos-14"] if "macOS" in name else ["windows-latest"],
            "steps": quiet_steps(started, completed) if steps is None else steps}


def synthetic(rows, end, artifacts=(), timing=None):
    """A single-attempt run created at the origin whose attempt ends at `end`."""
    attempt = {"id": RUN_ID, "run_attempt": 1, "created_at": stamp(0), "run_started_at": stamp(0),
               "updated_at": stamp(end)}
    return {"run": dict(attempt), "attempts": {"1": attempt}, "jobs": {"1": list(rows)},
            "artifacts": list(artifacts), "timing": dict(timing or {})}


def artifact(name, created):
    """One artifact of a synthetic run."""
    return {"id": abs(hash(name)) % 10_000_000, "name": name, "created_at": stamp(created), "size_in_bytes": 1}


def recorded():
    """A fresh copy of the recorded two-attempt run."""
    return json.loads(FIXTURE.read_text(encoding="utf-8"))


def no_timing(name):
    """A historical run never reads timing.json; reaching this fails the test."""
    raise AssertionError(f"read timing.json of {name}")


def three_attempts():
    """The recorded run plus a synthetic attempt 3 that reruns macOS S7 alone.

    Attempt 3 starts at 12:00:00 and ends at 12:20:00; S7 runs 12:00:05-12:18:40 on a new runner. The
    other nine rows are copied with new ids, attempt 3 and creation 12:00:02, keeping their executed
    origin's times, runner and steps, as GitHub lists them.
    """
    record = recorded()
    attempt = dict(record["attempts"]["2"], run_attempt=3, created_at="2026-10-03T12:00:01Z",
                   run_started_at="2026-10-03T12:00:00Z", updated_at="2026-10-03T12:20:00Z")
    record["attempts"]["3"] = attempt
    record["run"].update(run_attempt=3, run_started_at=attempt["run_started_at"], updated_at=attempt["updated_at"])
    rows = []
    for row in record["jobs"]["2"]:
        copied = dict(copy.deepcopy(row), id=row["id"] + 1_000_000, run_attempt=3, created_at="2026-10-03T12:00:02Z")
        if row["name"] == "macOS before/after comparison (S7)":
            copied.update(started_at="2026-10-03T12:00:05Z", completed_at="2026-10-03T12:18:40Z", runner_id=1000099,
                          steps=[{"name": "Set up job", "status": "completed", "conclusion": "success", "number": 1,
                                  "started_at": "2026-10-03T12:00:05Z", "completed_at": "2026-10-03T12:18:40Z"}])
        rows.append(copied)
    record["jobs"]["3"] = rows
    record["artifacts"].append({"id": 1, "name": "perf-comparison-1569-0e83fc0993bfcb2c4add474c5850484d08d1a8ce-macOS-S7",
                                "created_at": "2026-10-03T12:18:30Z", "size_in_bytes": 1})
    return record


def segment(path, name):
    """The segment of a path that a job contributes."""
    return next(part for part in path.segments if part.name == name)


class PathAccountingTests(unittest.TestCase):
    """How one attempt's time splits along each chain of needs into waits, queue and runtime."""

    def two_needs(self, result_created=0, result_started=110):
        """Two Windows shards finishing at 50 and 100, and perf-result needing both."""
        rows = [job("Windows before/after comparison (S1)", 0, 0, 50),
                job("Windows before/after comparison (S2)", 0, 0, 100),
                job(RESULT, result_created, result_started, 120)]
        return accounting.account(synthetic(rows, 121), no_timing).attempts[0]

    def test_precreated_job_keeps_full_dependency_wait(self):
        # Created at 0, the job waits for its slower need (100) before it can queue: 50 + 0 + 10 from its path's 50.
        attempt = self.two_needs()
        path = next(path for path in attempt.paths if path.names[0] == "Windows before/after comparison (S1)")
        result = segment(path, RESULT)
        self.assertEqual((result.sibling_wait_s, result.creation_wait_s, result.runner_queue_s), (50, 0, 10))
        self.assertEqual(result.sibling_wait_s + result.creation_wait_s + result.runner_queue_s, 110 - 50)

    def test_unequal_dependency_finishes_put_slack_on_the_earlier_path(self):
        # The path through the shard that finished at 50 waits 50 s on its sibling; the other path has none.
        attempt = self.two_needs()
        slack = {path.names[0]: path.slack_s for path in attempt.paths}
        self.assertEqual(slack, {"Windows before/after comparison (S1)": 50,
                                 "Windows before/after comparison (S2)": 0})
        self.assertEqual(attempt.critical.names, ["Windows before/after comparison (S2)", RESULT])

    def test_created_after_ready_counts_creation_wait(self):
        # Created at 105, five seconds after its needs finished: 5 s waiting to exist, then 5 s in the queue.
        result = segment(self.two_needs(result_created=105).critical, RESULT)
        self.assertEqual((result.creation_wait_s, result.runner_queue_s), (5, 5))

    def test_negative_runner_queue_fails(self):
        # A job that started before its needs finished means the needs or rows are wrong, so nothing is reported.
        with self.assertRaisesRegex(accounting.AccountingError, "runner_queue"):
            self.two_needs(result_started=90)

    def test_delayed_producer_is_producer_queue_not_consumer_queue(self):
        # A producer that waited 300 s for a runner shows that wait; the shard queued only after it finished.
        rows = [job(PRODUCER, 0, 300, 600), job("macOS before/after comparison (S7)", 0, 610, 1000),
                job(RESULT, 0, 1005, 1010)]
        path = accounting.account(synthetic(rows, 1011), no_timing).attempts[0].critical
        self.assertEqual(path.names, [PRODUCER, "macOS before/after comparison (S7)", RESULT])
        self.assertEqual(segment(path, PRODUCER).runner_queue_s, 300)
        shard = segment(path, "macOS before/after comparison (S7)")
        self.assertEqual((shard.sibling_wait_s, shard.creation_wait_s, shard.runner_queue_s), (0, 0, 10))

    def evidence_run(self, marks=None, timing_overrides=None, read=None):
        """A Windows shard whose compare step runs 10-500 and whose evidence upload takes 300 s."""
        name = "Windows before/after comparison (S7)"
        steps = [step("Set up job", 0, 10, 1), step("Compare the base and the head", 10, 500, 2),
                 step("Upload the comparison evidence", 500, 800, 3), step("Complete job", 800, 805, 4)]
        evidence = f"perf-comparison-1-{SHA}-Windows-S7-1"
        timing = {"schema_version": 1, "run_id": str(RUN_ID), "run_attempt": "1", "job": name, "shard": "S7",
                  "marks": {mark: ORIGIN_S + offset for mark, offset in (marks or {
                      "start": 12, "listed": 20, "measure_start": 22, "measure_end": 499,
                      "report_written": 500}).items()}}
        timing.update(timing_overrides or {})
        record = synthetic([job(name, 0, 0, 805, steps=steps)], 806, [artifact(evidence, 801)], {evidence: timing})
        return accounting.account(record, read or (lambda artifact_name: record["timing"].get(artifact_name)))

    def test_long_evidence_upload_is_evidence_not_scenarios(self):
        # A slow upload after the comparison is evidence time; the scenarios are only the measured window.
        classes = self.evidence_run().attempts[0].critical.segments[0].classes
        self.assertEqual((classes["evidence"], classes["compare"], classes["setup"], classes["teardown"]),
                         (300, 490, 10, 5))
        self.assertEqual(classes["gap"], 0)

    def test_timing_json_splits_prepare_and_scenarios(self):
        # start→listed is prepare, measure_start→measure_end the scenarios, measure_end→report_written the report.
        compare = self.evidence_run().attempts[0].critical.segments[0].compare
        self.assertEqual(compare, {"prepare": 8, "scenarios": 477, "report": 1, "other": 4})

    def test_every_path_reconciles_and_zero_slack_path_is_critical(self):
        # Every chain sums to the terminal's finish less A_k, and the walk back through the latest need has no slack.
        rows = [job(PRODUCER, 0, 5, 300), job("macOS before/after comparison (S7)", 0, 320, 1100),
                job("macOS before/after comparison (S9-S10)", 0, 310, 900),
                job("Windows before/after comparison (S7)", 0, 3, 1000), job(RESULT, 0, 1110, 1115)]
        attempt = accounting.account(synthetic(rows, 1120), no_timing).attempts[0]
        # perf-result needs the producer directly too, so producer→result is a fourth chain.
        self.assertEqual(len(attempt.paths), 4)
        for path in attempt.paths:
            self.assertEqual(path.total_s, 1115)
            self.assertEqual(path.total_s + attempt.tail_s, 1120)
        self.assertEqual(attempt.critical.slack_s, 0)
        self.assertEqual(attempt.critical.names, [PRODUCER, "macOS before/after comparison (S7)", RESULT])
        # Slack is each chain's waits on later siblings: Windows S7 100, macOS S9-S10 200, producer→result 800.
        self.assertEqual(sorted(path.slack_s for path in attempt.paths), [0, 100, 200, 800])


    def test_every_executed_job_prints_its_breakdown_once(self):
        # Producer, two macOS shards, a Windows shard and the result: each job's ready time, queue, runtime and
        # classes print once, including the Windows shard that is off the critical path.
        rows = [job(PRODUCER, 0, 5, 300, steps=[step("Set up job", 5, 100, 1), step("Build both refs once", 100, 300, 2)]),
                job("macOS before/after comparison (S7)", 0, 320, 1100, steps=[
                    step("Set up job", 320, 330, 1), step("Download the binaries", 330, 340, 2),
                    step("Unpack the binaries", 340, 345, 3),
                    step("Compare the base and the head", 1100, 1100, 4, "skipped")]),
                job("macOS before/after comparison (S9-S10)", 0, 310, 900),
                job("Windows before/after comparison (S7)", 0, 3, 1000), job(RESULT, 0, 1110, 1115)]
        report = accounting.account(synthetic(rows, 1120), no_timing)
        self.assertEqual(sorted(report.attempts[0].jobs), sorted(row["name"] for row in rows))
        text = accounting.render(report)
        for row in rows:
            self.assertEqual(text.count(f"{row['name']}: ready +"), 1, row["name"])
        self.assertIn("Windows before/after comparison (S7): ready +0 s, creation_wait 0 s, runner_queue 3 s, "
                      "runtime 997 s", text)
        self.assertIn("build 200 s", text)
        self.assertIn("download+extract 15 s", text)
        self.assertIn("macOS before/after comparison (S9-S10): ready +300 s, creation_wait 0 s, runner_queue 10 s", text)


class AttemptResolutionTests(unittest.TestCase):
    """Rows of a rerun: each is skipped, executed in its attempt, or inherited from one executed origin."""

    def test_inherited_rows_map_to_their_attempt_1_origin(self):
        # Attempt 2 reran Windows S9-S10 only; its nine other rows carry attempt 1's times, runner and steps.
        rows = accounting.resolve_rows(recorded())[2]
        executed = [row.name for row in rows if row.kind == "executed"]
        self.assertEqual(executed, ["Windows before/after comparison (S9-S10)"])
        inherited = [row for row in rows if row.kind == "inherited"]
        self.assertEqual(len(inherited), 9)
        attempt_1_ids = {row["id"] for row in recorded()["jobs"]["1"]}
        for row in inherited:
            self.assertEqual(row.origin.attempt, 1)
            self.assertIn(row.origin.data["id"], attempt_1_ids)
            self.assertEqual(row.origin.name, row.name)

    def test_three_attempts_resolve_each_row_to_one_executed_origin(self):
        # Eight rows go back to attempt 1, Windows S9-S10 to attempt 2, and macOS S7 executed in attempt 3.
        rows = accounting.resolve_rows(three_attempts())[3]
        origins = {row.name: (row.kind, row.origin.attempt) for row in rows}
        self.assertEqual(origins.pop("Windows before/after comparison (S9-S10)"), ("inherited", 2))
        self.assertEqual(origins.pop("macOS before/after comparison (S7)"), ("executed", 3))
        self.assertEqual(set(origins.values()), {("inherited", 1)})
        self.assertEqual(len(origins), 8)

    def test_inherited_copies_are_never_candidates(self):
        # Attempt 2's copy of Windows S7 has the same fingerprint as attempt 1's row; only the executed one matches.
        record = three_attempts()
        copies = [row for attempt in ("1", "2") for row in record["jobs"][attempt]
                  if row["name"] == "Windows before/after comparison (S7)"]
        self.assertEqual(len({accounting.fingerprint(row) for row in copies}), 1)
        row = next(row for row in accounting.resolve_rows(record)[3]
                   if row.name == "Windows before/after comparison (S7)")
        self.assertEqual((row.kind, row.origin.attempt), ("inherited", 1))

    def test_missing_or_duplicate_executed_origin_fails(self):
        # No executed match, or two, is an error; the report never falls back to a guess.
        record = recorded()
        moved = next(row for row in record["jobs"]["2"] if row["name"] == "Windows before/after comparison (S7)")
        moved["started_at"] = "2026-10-03T10:56:40Z"
        with self.assertRaisesRegex(accounting.AccountingError, "no executed origin"):
            accounting.resolve_rows(record)
        record = recorded()
        original = next(row for row in record["jobs"]["1"] if row["name"] == "Windows before/after comparison (S7)")
        record["jobs"]["1"].append(dict(original, id=original["id"] + 1))
        with self.assertRaisesRegex(accounting.AccountingError, "2 executed origins"):
            accounting.resolve_rows(record)

    def test_skipped_row_is_outside_paths_and_ordering(self):
        # A skipped row lists a start one second after its completion; it is neither checked nor on any path.
        record = recorded()
        record["jobs"]["1"].append({"id": 1, "name": PRODUCER, "run_attempt": 1, "conclusion": "skipped",
                                    "status": "completed", "created_at": "2026-10-03T10:56:39Z",
                                    "started_at": "2026-10-03T10:56:41Z", "completed_at": "2026-10-03T10:56:40Z",
                                    "runner_id": 0, "steps": [], "labels": []})
        report = accounting.account(record, no_timing)
        self.assertIn(("skipped", PRODUCER), [(row.kind, row.name) for row in accounting.resolve_rows(record)[1]])
        for attempt in report.attempts:
            for path in attempt.paths:
                self.assertNotIn(PRODUCER, path.names)

    def test_inherited_created_after_completed_is_accepted(self):
        # An inherited row is created by its rerun but keeps its origin's times, so its creation is not checked.
        row = next(row for row in accounting.resolve_rows(recorded())[2]
                   if row.name == "macOS before/after comparison (S2-S10sync)")
        self.assertEqual(row.kind, "inherited")
        self.assertGreater(accounting.parse_time(row.data["created_at"]), accounting.parse_time(row.data["completed_at"]))


class BudgetTests(unittest.TestCase):
    """The 30-minute budget runs from the run's creation to its last attempt's finish, reruns included."""

    def test_budget_from_original_creation_to_final_finish(self):
        # Two attempts: 2524 s = 1413 + 48 + 1063; three: 5001 s = 1413 + 48 + 1063 + 1277 + 1200.
        two = accounting.budget(recorded())
        self.assertEqual((two.elapsed_s, two.spans_s, two.rerun_waits_s, two.first_wait_s), (2524, [1413, 1063], [48], 0))
        three = accounting.budget(three_attempts())
        self.assertEqual((three.elapsed_s, three.spans_s, three.rerun_waits_s), (5001, [1413, 1063, 1200], [48, 1277]))
        self.assertGreater(two.elapsed_s, accounting.BUDGET_S)


class EvidenceTests(unittest.TestCase):
    """Which artifact is a job's evidence, and when its timing.json is required."""

    def test_historical_run_reconciles_without_timing_json(self):
        # The recorded run predates timing.json: it reconciles from the rows alone and says the split is unavailable.
        report = accounting.account(recorded(), no_timing)
        self.assertEqual(report.mode, "historical")
        first, second = report.attempts
        self.assertEqual(first.critical.names, ["Windows before/after comparison (S7)"])
        windows_s7 = first.critical.segments[0]
        self.assertEqual((windows_s7.creation_wait_s, windows_s7.runner_queue_s, windows_s7.runtime_s, first.tail_s),
                         (0, 13, 1399, 1))
        self.assertEqual(second.critical.names, ["Windows before/after comparison (S9-S10)"])
        rerun = second.critical.segments[0]
        self.assertEqual((rerun.creation_wait_s, rerun.runner_queue_s, rerun.runtime_s, second.tail_s), (2, 4, 1056, 1))
        text = accounting.render(report)
        for expected in ("2524 s", "rerun_wait 1→2: 48 s", "attempt_tail: 1 s",
                         "unavailable (historical run: no timing.json)"):
            self.assertIn(expected, text)

    def test_historical_evidence_binds_by_window(self):
        # Both attempts' Windows S9-S10 artifacts share a name; each binds to the one created inside its row.
        report = accounting.account(recorded(), no_timing)
        names = {(attempt, name): evidence["created_at"] for (attempt, name), evidence in report.evidence.items()}
        self.assertEqual(names[(1, "Windows before/after comparison (S9-S10)")], "2026-10-03T11:11:56Z")
        self.assertEqual(names[(2, "Windows before/after comparison (S9-S10)")], "2026-10-03T11:38:37Z")

    def test_mode_is_new_design_when_result_row_listed(self):
        # The result job's row, even skipped, marks the new layout.
        record = recorded()
        record["jobs"]["1"].append({"id": 2, "name": RESULT, "run_attempt": 1, "conclusion": "skipped",
                                    "status": "completed", "steps": []})
        self.assertEqual(accounting.evidence_mode(record), "new-design")

    def test_mode_is_new_design_when_the_not_run_result_row_is_listed(self):
        # An ineligible run names its skipped result job "(not run)"; that row marks the new layout too.
        record = recorded()
        record["jobs"]["1"].append({"id": 4, "name": f"{RESULT} (not run)", "run_attempt": 1,
                                    "conclusion": "skipped", "status": "completed", "steps": []})
        self.assertEqual(accounting.evidence_mode(record), "new-design")

    def test_mode_is_new_design_when_attempt_suffixed_artifact_exists(self):
        # An evidence name ending in its attempt marks the new layout too.
        record = recorded()
        self.assertEqual(accounting.evidence_mode(record), "historical")
        record["artifacts"].append({"id": 3, "name": f"perf-comparison-1-{SHA}-macOS-S7-1",
                                    "created_at": "2026-10-03T11:00:00Z"})
        self.assertEqual(accounting.evidence_mode(record), "new-design")

    def test_new_design_missing_timing_json_fails(self):
        # A new-design comparison without timing.json cannot split its compare step, so the report stops.
        with self.assertRaisesRegex(accounting.AccountingError, "timing.json"):
            PathAccountingTests.evidence_run(PathAccountingTests(), read=lambda name: None)

    def test_new_design_timing_json_wrong_run_attempt_or_shard_fails(self):
        # timing.json must name this run, this attempt and this shard (and its job).
        for override in ({"run_id": "98"}, {"run_attempt": "2"}, {"shard": "S9-S10"},
                         {"job": "macOS before/after comparison (S7)"}):
            with self.subTest(override=override), self.assertRaisesRegex(accounting.AccountingError, "timing.json"):
                PathAccountingTests.evidence_run(PathAccountingTests(), timing_overrides=override)

    def test_new_design_timing_marks_outside_compare_step_fail(self):
        # Marks outside the compare step, or out of order, describe another step's time.
        for marks in ({"start": 5, "listed": 20, "measure_start": 22, "measure_end": 499, "report_written": 500},
                      {"start": 12, "listed": 20, "measure_start": 22, "measure_end": 499, "report_written": 520},
                      {"start": 12, "listed": 30, "measure_start": 22, "measure_end": 499, "report_written": 500}):
            with self.subTest(marks=marks), self.assertRaisesRegex(accounting.AccountingError, "marks"):
                PathAccountingTests.evidence_run(PathAccountingTests(), marks=marks)


    def test_non_finite_boolean_or_text_marks_fail(self):
        # NaN slips through sorting, so a NaN mark followed by one 100 s before the step must still be refused;
        # so must infinity, a boolean and a string.
        valid = {"start": ORIGIN_S + 12, "listed": ORIGIN_S + 20, "measure_start": ORIGIN_S + 22,
                 "measure_end": ORIGIN_S + 499, "report_written": ORIGIN_S + 500}
        cases = (dict(valid, listed=float("nan"), measure_start=ORIGIN_S - 90),
                 dict(valid, report_written=float("inf")), dict(valid, listed=True), dict(valid, measure_end="499"))
        for marks in cases:
            with self.subTest(marks=marks), self.assertRaisesRegex(accounting.AccountingError, "marks"):
                PathAccountingTests.evidence_run(PathAccountingTests(), timing_overrides={"marks": marks})

    def test_each_mark_is_checked_inside_the_step(self):
        # Every mark, not only the first and last, must lie inside the compare step and after the one before it.
        valid = {"start": ORIGIN_S + 12, "listed": ORIGIN_S + 20, "measure_start": ORIGIN_S + 22,
                 "measure_end": ORIGIN_S + 499, "report_written": ORIGIN_S + 500}
        for marks in (dict(valid, measure_start=ORIGIN_S + 600), dict(valid, listed=ORIGIN_S + 5)):
            with self.subTest(marks=marks), self.assertRaisesRegex(accounting.AccountingError, "marks"):
                PathAccountingTests.evidence_run(PathAccountingTests(), timing_overrides={"marks": marks})


class FakePipe:
    """A child's output pipe that records whether it was closed."""

    def __init__(self):
        self.closed = False

    def close(self):
        self.closed = True


class FakeChild:
    """A child process whose first `timeouts` communicate calls time out; it records every call."""

    def __init__(self, timeouts=1):
        self.pid, self.returncode, self.remaining = 4242, None, timeouts
        self.stdout, self.stderr, self.calls = FakePipe(), FakePipe(), []

    def communicate(self, timeout=None):
        self.calls.append(("communicate", timeout))
        if self.remaining:
            self.remaining -= 1
            raise subprocess.TimeoutExpired("gh", timeout)
        self.returncode = -9
        return b"", b""

    def kill(self):
        self.calls.append(("kill",))

    def wait(self, timeout=None):
        self.calls.append(("wait", timeout))
        self.returncode = -9
        return -9

    def poll(self):
        return self.returncode


class BoundedCleanupTests(unittest.TestCase):
    """run_bounded always kills and reaps the command it started, whatever its tree-kill command does."""

    def run_child(self, child, windows, run=None, killpg=None):
        """Run a fake `gh` through run_bounded; return the error and every popen option it used."""
        options = {}

        def popen(argv, **given):
            options.update(given)
            return child
        with self.assertRaises(accounting.AccountingError) as raised:
            accounting.run_bounded(["gh", "api", "x"], timeout_s=2, windows=windows, popen=popen,
                                   run=run or (lambda *args, **kwargs: None), killpg=killpg or (lambda *args: None))
        return str(raised.exception), options

    def test_the_posix_kill_signals_the_group_then_kills_and_reaps_the_child(self):
        # The deadline kills the whole session with SIGKILL, kills the child itself and waits for it.
        child, signalled = FakeChild(), []
        message, options = self.run_child(child, windows=False, killpg=lambda pid, number: signalled.append((pid, number)))
        self.assertIn("timed out", message)
        self.assertTrue(options["start_new_session"])
        self.assertEqual(signalled, [(4242, accounting.KILL_SIGNAL)])
        self.assertIn(("kill",), child.calls)
        self.assertEqual(child.calls[-1][0], "communicate")
        self.assertEqual(child.poll(), -9)

    def test_a_group_that_already_exited_still_has_its_child_reaped(self):
        # killpg finding no group is not a reason to skip killing and reaping the child.
        def gone(*_args):
            raise ProcessLookupError()
        child = FakeChild()
        self.run_child(child, windows=False, killpg=gone)
        self.assertIn(("kill",), child.calls)
        self.assertEqual(child.poll(), -9)

    def test_a_windows_taskkill_that_times_out_or_cannot_start_still_kills_and_reaps(self):
        # taskkill's failure is reported, and the child is still killed and reaped.
        for failure in (subprocess.TimeoutExpired("taskkill", 10), FileNotFoundError("taskkill")):
            with self.subTest(failure=type(failure).__name__):
                def run(*_args, **_kwargs):
                    raise failure
                child = FakeChild()
                message, options = self.run_child(child, windows=True, run=run)
                self.assertIn("taskkill", message)
                self.assertIn("creationflags", options)
                self.assertIn(("kill",), child.calls)
                self.assertEqual(child.poll(), -9)

    def test_a_child_whose_pipes_stay_open_is_waited_for_after_closing_them(self):
        # A grandchild that kept the pipes open makes the second communicate time out: close them and wait.
        child = FakeChild(timeouts=2)
        self.run_child(child, windows=True)
        self.assertTrue(child.stdout.closed and child.stderr.closed)
        self.assertEqual(child.calls[-1][0], "wait")
        self.assertEqual(child.poll(), -9)

    def test_a_command_that_cannot_start_is_an_accounting_error(self):
        # A missing `gh` stops the report with the reason rather than a traceback.
        def popen(*_args, **_kwargs):
            raise FileNotFoundError("gh")
        with self.assertRaisesRegex(accounting.AccountingError, "could not start"):
            accounting.run_bounded(["gh"], timeout_s=1, popen=popen)


class CommandLineTests(unittest.TestCase):
    """The command's exit codes, its GitHub reads and its bounded child process."""

    def test_the_recorded_run_is_over_budget(self):
        # Exit 1 when the run took longer than 30 minutes; the report still prints.
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            self.assertEqual(accounting.main(["--fixture", str(FIXTURE)]), 1)
        self.assertIn("OVER", printed.getvalue())

    def test_an_unreconciled_run_exits_2(self):
        # An accounting error prints the reason and exits 2, never a partial report.
        record = recorded()
        record["jobs"]["2"][0]["started_at"] = "2026-10-03T10:56:40Z"
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "run.json"
            path.write_text(json.dumps(record), encoding="utf-8")
            with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(accounting.main(["--fixture", str(path)]), 2)

    def test_fetch_reads_every_attempt_and_page(self):
        # Each attempt's jobs and the artifacts are read page by page until a short page.
        calls = []
        pages = {"jobs": [[{"id": number} for number in range(100)], [{"id": 100}]]}

        def runner(argv):
            calls.append(argv[-1])
            path = argv[-1]
            if path.endswith("runs/5"):
                return json.dumps({"id": 5, "run_attempt": 2}).encode()
            if "/attempts/" in path and "/jobs" not in path:
                return json.dumps({"run_attempt": int(path.rsplit("/", 1)[1])}).encode()
            if "/jobs" in path:
                page = int(path.rsplit("page=", 1)[1])
                return json.dumps({"jobs": pages["jobs"][page - 1]}).encode()
            return json.dumps({"artifacts": []}).encode()
        record = accounting.fetch_record("owner/repo", "5", runner)
        self.assertEqual(sorted(record["attempts"]), ["1", "2"])
        self.assertEqual(len(record["jobs"]["1"]), 101)
        self.assertIn("repos/owner/repo/actions/runs/5/attempts/2/jobs?per_page=100&page=2", calls)

    @unittest.skipIf(os.name == "nt", "checks POSIX process-group reaping")
    def test_a_hung_command_is_killed_with_its_children(self):
        # A `gh` that hangs is stopped at its bound with every process it started, and the error says so.
        with tempfile.TemporaryDirectory() as temp:
            pid_file = Path(temp) / "child.pid"
            script = ("import subprocess, sys, time\n"
                      "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
                      f"open({str(pid_file)!r}, 'w').write(str(child.pid))\n"
                      "time.sleep(60)\n")
            started = time.monotonic()
            with self.assertRaisesRegex(accounting.AccountingError, "timed out"):
                accounting.run_bounded([sys.executable, "-c", script], timeout_s=2)
            self.assertLess(time.monotonic() - started, 20)
            child_pid = int(pid_file.read_text(encoding="utf-8"))
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    os.kill(child_pid, 0)
                except ProcessLookupError:
                    break
                # When: the killed child has not been reaped by init yet, it can still be signalled briefly.
                if subprocess.run(["ps", "-o", "stat=", "-p", str(child_pid)], capture_output=True,
                                  text=True, check=False).stdout.strip().startswith("Z"):
                    break
                time.sleep(0.1)
            else:
                self.fail(f"child {child_pid} survived the bound")

    def test_the_macos_timeline_counts_concurrent_jobs(self):
        # Across the perf and CI runs, the timeline shows how many macOS jobs ran at once.
        rows = [job("macOS before/after comparison (S7)", 0, 0, 100), job("macos-core", 0, 50, 150),
                job("Windows before/after comparison (S7)", 0, 0, 200)]
        rows[1]["labels"] = ["macos-14"]
        record = synthetic(rows, 201)
        timeline = accounting.macos_timeline([record])
        self.assertEqual(max(count for _time, count, _event in timeline), 2)
        self.assertEqual(len(timeline), 4)


if __name__ == "__main__":
    unittest.main(verbosity=2)
