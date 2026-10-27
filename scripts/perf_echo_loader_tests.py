"""Perf echo loader tests."""

from __future__ import annotations
import contextlib
import io
import json
import os
import shutil
from pathlib import Path
import tempfile
import unittest

import perf_arguments
import perf_attribution_schema
import perf_comparison_rows
import perf_comparison_tests
import perf_counter_schema_tests
import perf_echo_analysis
import perf_echo_loader
import perf_echo_tests
import perf_execution
import perf_inventory
import perf_kind_tests
import perf_latency_tests
import perf_main
import perf_memory_tests
import perf_metrics
import perf_result_schema
import perf_result_tests
import perf_settings
import perf_timeline_tests


def write_echo_artifact(root, runs, *, run_id="700", head=perf_echo_tests.ECHO_HEAD_SHA, kinds=None, statuses=None,
                        capabilities=None, results=None, classifications=None, run_attempt=1, guard_api=None):
    """One workflow execution's downloaded artifact under `root`, as perf-compare writes it: run-identity.json with
    the S2/flood counters set's final inventory, and each attempt's outcome, classification and result. `runs` maps
    an attempt name such as `01-base` to its timeline entries (or, under a harness without the timeline schema, its
    sample count). `kinds` overrides an attempt's final kind; `statuses` a side's final status, which accepts
    nothing; `results` replaces result keys per attempt; `classifications` replaces a classification per attempt;
    `run_attempt` and `guard_api` are the identity's own."""
    artifact = root / "perf-comparison-macOS"
    capabilities = dict(perf_echo_tests.ECHO_CAPABILITIES if capabilities is None else capabilities)
    statuses = statuses or {}
    entry = {"label": "S2/flood", "dataset": "counters", "base": {"status": statuses.get("base", ""), "accepted": []},
             "head": {"status": statuses.get("head", ""), "accepted": []}}
    for name, entries in runs.items():
        side = name.split("-")[1]
        kind = (kinds or {}).get(name, "valid")
        attempt = artifact / "runs" / "S2-flood" / "counters" / name
        write_json(attempt / "outcome.json", {"kind": kind, "side": side, "scenario": "S2", "variant": "flood",
                                              "exit_code": 0})
        classification = {"side": side, "kind": kind, "reasons": []}
        write_json(attempt / "classification.json", (classifications or {}).get(name, classification))
        if capabilities["echo_timeline_schema"] is None:
            latency = perf_latency_tests.split_latency([perf_latency_tests.split_sample() for _ in range(entries)])
        else:
            latency = perf_timeline_tests.timeline_latency([perf_echo_tests.echo_sample(item) for item in entries])
        result = perf_counter_schema_tests.counters_result(latency=latency, scenario="S2", variant="flood", short=True)
        result.update((results or {}).get(name, {}))
        write_json(attempt / "scratch" / "result.json", result)
        if kind == "valid" and not entry[side]["status"]:
            entry[side]["accepted"].append(name)
    write_json(artifact / "run-identity.json", {
        "schema_version": perf_inventory.RUN_IDENTITY_SCHEMA, "run_id": run_id, "run_attempt": run_attempt, "platform": "macos",
        "base_sha": perf_echo_tests.ECHO_BASE_SHA, "head_sha": head, "harness_hash": perf_memory_tests.HARNESS_HASH, "settings": perf_echo_tests.ECHO_SETTINGS,
        "flag_metrics_version": perf_comparison_rows.FLAG_METRICS_VERSION, "sets": [entry], "capabilities": capabilities,
        "guard_api": guard_api})
    return root


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


ECHO_ATTEMPTS = {"01-base": perf_echo_tests.ECHO_BASE, "02-head": perf_echo_tests.ECHO_RENDER, "03-head": perf_echo_tests.ECHO_RENDER, "04-base": perf_echo_tests.ECHO_BASE}


class EchoAttributionLoaderTests(unittest.TestCase):
    def load(self, **artifact):
        """Load one written artifact; a crash fails the test as an assertion, since malformed evidence must be
        refused with a reason, never raise."""
        with tempfile.TemporaryDirectory() as temporary:
            root = write_echo_artifact(Path(temporary), artifact.pop("runs", ECHO_ATTEMPTS), **artifact)
            try:
                return perf_echo_loader.load_echo_execution(root, "replay 1")
            except Exception as error:  # noqa: BLE001 - any exception is the defect under test
                self.fail(f"load_echo_execution raised {error!r} instead of refusing")

    def refused(self, expected, **artifact):
        execution, problems = self.load(**artifact)
        self.assertIsNone(execution)
        self.assertTrue(any(expected in problem for problem in problems), problems)

    def test_accepted_attempts_load_with_their_identity_and_rejected_ones_are_named(self):
        # Only the final inventory's accepted attempts are evidence; an invalid attempt is skipped and named.
        execution, problems = self.load(runs={**ECHO_ATTEMPTS, "05-base": perf_echo_tests.ECHO_BASE}, kinds={"05-base": "invalid"})
        self.assertEqual(problems, [])
        self.assertEqual((len(execution.base), len(execution.head)), (2, 2))
        self.assertEqual(execution.skipped, ["05-base (invalid)"])
        self.assertEqual((execution.identity["run_id"], execution.identity["head_sha"]), ("700", perf_echo_tests.ECHO_HEAD_SHA))

    def test_a_blocked_side_accepts_nothing_even_of_valid_attempts(self):
        # The inventory of a side that ended blocked accepts no attempt, valid ones included; they are named.
        execution, problems = self.load(statuses={"head": "blocked"})
        self.assertEqual(problems, [])
        self.assertEqual((len(execution.base), execution.head, execution.statuses["head"]), (2, [], "blocked"))
        self.assertIn("02-head (valid, discarded by a 'blocked' side)", execution.skipped)

    def test_an_accepted_result_of_another_scenario_or_dataset_is_refused(self):
        # The outcome and result must be a valid, exit-0 run of S2/flood counters on its side.
        self.refused("result scenario 'S3' is not 'S2'", results={"02-head": {"scenario": "S3"}})
        self.refused("result status 'invalid' is not 'valid'", results={"02-head": {"status": "invalid"}})
        self.refused("frame_counters", results={"02-head": {"frame_counters": "off"}})

    def test_latency_counts_are_recomputed_from_the_samples(self):
        # attributed and total must be the samples' own counts: 100 of 1 with five samples is refused.
        latency = perf_timeline_tests.timeline_latency([perf_echo_tests.echo_sample(item) for item in perf_echo_tests.ECHO_RENDER])
        self.refused("total 1 are not the samples' 5 and 5", results={"02-head": {"latency": dict(latency, total=1)}})
        self.refused("attributed", results={"02-head": {"latency": dict(latency, attributed=100, total=1)}})

    def test_a_classification_without_a_known_kind_is_refused(self):
        # A missing kind or one perf-compare never writes is malformed, never a skipped "None" attempt.
        self.refused("final classification", classifications={"04-base": {"side": "base", "reasons": []}},
                     kinds={"04-base": "invalid"})
        self.refused("classification kind 'mystery' is not one perf-compare writes",
                     classifications={"04-base": {"side": "base", "kind": "mystery", "reasons": []}},
                     kinds={"04-base": "mystery"})

    def test_an_unexpected_attempt_directory_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = write_echo_artifact(Path(temporary), ECHO_ATTEMPTS)
            (root / "perf-comparison-macOS" / "runs" / "S2-flood" / "counters" / "notes").mkdir()
            execution, problems = perf_echo_loader.load_echo_execution(root, "replay 1")
        self.assertIsNone(execution)
        self.assertTrue(any("not an attempt directory" in problem for problem in problems), problems)

    def test_an_old_harness_is_read_as_unavailable_timelines(self):
        # A harness that never declared the timeline schema wrote none: the run loads, and the analysis names the
        # undeclared schema instead of inferring the capability from absent fields.
        capabilities = dict(perf_echo_tests.ECHO_CAPABILITIES, echo_timeline_schema=None)
        executions = []
        with tempfile.TemporaryDirectory() as temporary:
            for run_id in ("700", "701"):
                root = write_echo_artifact(Path(temporary) / run_id, {"01-base": 5, "02-head": 5},
                                           run_id=run_id, capabilities=capabilities)
                execution, problems = perf_echo_loader.load_echo_execution(root, f"replay {run_id}")
                self.assertEqual(problems, [])
                executions.append(execution)
        for execution in executions:
            execution.head = [perf_echo_analysis.EchoRun([perf_latency_tests.split_sample((2.0, 3.0, 9.0)) for _ in range(5)], 5, 5)]
        result = perf_echo_analysis.echo_attribution(executions)
        self.assertEqual(result.outcome, "inconclusive (evidence unavailable)")
        self.assertTrue(any("declares no echo_timeline_schema" in line for line in result.lines), result.lines)

    def test_a_declared_timeline_with_deleted_fields_is_refused(self):
        # Under a declared schema a sample whose timeline was deleted is malformed evidence, not an old harness.
        latency = perf_timeline_tests.timeline_latency([perf_echo_tests.echo_sample(item) for item in perf_echo_tests.ECHO_RENDER])
        del latency["samples"][0]["echo_timeline"]
        self.refused("missing echo_timeline", results={"02-head": {"latency": latency}})

    def test_the_same_run_under_two_paths_is_refused(self):
        # Two directories holding one workflow run are one execution, detected by run id, not by path spelling.
        with tempfile.TemporaryDirectory() as temporary:
            first = write_echo_artifact(Path(temporary) / "one", ECHO_ATTEMPTS)
            second = Path(temporary) / "copy"
            shutil.copytree(first, second)
            errors = io.StringIO()
            with contextlib.redirect_stderr(errors):
                code = perf_main.main(["--echo-attribution", str(first), str(second)])
        self.assertEqual(code, perf_settings.EXIT_FAIL)
        self.assertIn("replay 2: workflow run 700 is already read as replay 1", errors.getvalue())

    def test_the_command_prints_the_verdict_and_exits_zero(self):
        # Two replay executions and one paired control: exit 0 with the outcome, the unverified attribution and
        # the medians in milliseconds.
        control = [perf_echo_tests.echo_entry(render=4.0)] * 5
        with tempfile.TemporaryDirectory() as temporary:
            paths = [write_echo_artifact(Path(temporary) / run_id, attempts, run_id=run_id, head=head)
                     for run_id, attempts, head in (
                         ("700", ECHO_ATTEMPTS, perf_echo_tests.ECHO_HEAD_SHA), ("701", ECHO_ATTEMPTS, perf_echo_tests.ECHO_HEAD_SHA),
                         ("800", {**ECHO_ATTEMPTS, "02-head": control, "03-head": control}, perf_echo_tests.control_head("C2")),
                         ("801", {**ECHO_ATTEMPTS, "02-head": control, "03-head": control}, perf_echo_tests.control_head("C2")))]
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = perf_main.main(["--echo-attribution", str(paths[0]), str(paths[1]),
                                  "--echo-control", "C2", str(paths[2]), "--echo-control", "C2", str(paths[3])])
        self.assertEqual(code, perf_settings.EXIT_PASS, output.getvalue())
        self.assertIn("- outcome: M1 renderer work (step 4)", output.getvalue())
        self.assertIn("- M1 renderer work: attributed (C2, control identity unverified)", output.getvalue())
        self.assertIn("head run medians 14.000 ms, 14.000 ms; base 10.000 ms, 10.000 ms", output.getvalue())

    def test_the_command_refuses_malformed_evidence_with_exit_one(self):
        with tempfile.TemporaryDirectory() as temporary:
            errors = io.StringIO()
            with contextlib.redirect_stderr(errors):
                code = perf_main.main(["--echo-attribution", temporary, temporary])
        self.assertEqual(code, perf_settings.EXIT_FAIL)
        self.assertIn("echo attribution refused", errors.getvalue())

    def run_command(self, replay, controls=()):
        """Run --echo-attribution on artifacts written from (run id, keyword arguments) pairs, the controls named
        C2; returns the exit code and stdout plus stderr. A crash fails the test as an assertion: the command must
        refuse malformed evidence through exit 1."""
        with tempfile.TemporaryDirectory() as temporary:
            # Each artifact gets its own numbered directory, so a blank run id cannot nest one inside another.
            def written(index, run_id, artifact):
                artifact = dict(artifact)
                return str(write_echo_artifact(Path(temporary) / f"execution-{index}",
                                               artifact.pop("runs", ECHO_ATTEMPTS), run_id=run_id, **artifact))
            executions = [*replay, *controls]
            paths = [written(index, run_id, artifact) for index, (run_id, artifact) in enumerate(executions)]
            argv = ["--echo-attribution", *paths[:len(replay)]]
            for path in paths[len(replay):]:
                argv += ["--echo-control", "C2", path]
            output = io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
                try:
                    code = perf_main.main(argv)
                except Exception as error:  # noqa: BLE001 - any exception is the defect under test
                    self.fail(f"--echo-attribution raised {error!r} instead of exiting 1")
        return code, output.getvalue()

    def test_the_command_refuses_executions_that_differ_in_guard_api(self):
        # Real artifacts whose guard_api differs between the replay executions, or between a control and the
        # replay, are refused with exit 1; a shared guard_api reads normally.
        control = [perf_echo_tests.echo_entry(render=4.0)] * 5
        control_runs = {**ECHO_ATTEMPTS, "02-head": control, "03-head": control}
        code, output = self.run_command([("700", {"guard_api": True}), ("701", {"guard_api": False})])
        self.assertEqual(code, perf_settings.EXIT_FAIL, output)
        self.assertIn("replay 2: guard_api differs from replay 1's", output)
        replay = [("700", {"guard_api": False}), ("701", {"guard_api": False})]
        controls = [("800", {"runs": control_runs, "head": perf_echo_tests.control_head("C2"), "guard_api": False}),
                    ("801", {"runs": control_runs, "head": perf_echo_tests.control_head("C2"), "guard_api": None})]
        code, output = self.run_command(replay, controls)
        self.assertEqual(code, perf_settings.EXIT_FAIL, output)
        self.assertIn("C2 2: guard_api differs from replay 1's", output)
        controls[1][1]["guard_api"] = False
        code, output = self.run_command(replay, controls)
        self.assertEqual(code, perf_settings.EXIT_PASS, output)

    def test_the_command_refuses_an_identity_that_names_no_workflow_execution(self):
        # A blank or malformed run id, or attempt 0, paired with a normal execution is refused with exit 1;
        # perf-flags.py still loads such a local artifact, so the check is the attribution's own.
        for run_id, attempt, expected in (("", 1, "run_id '' is not a workflow run id"),
                                          ("run-7", 1, "run_id 'run-7' is not a workflow run id"),
                                          ("702", 0, "run_attempt 0 is not a workflow run attempt")):
            with self.subTest(run_id=run_id, attempt=attempt):
                code, output = self.run_command([("700", {}), (run_id, {"run_attempt": attempt})])
                self.assertEqual(code, perf_settings.EXIT_FAIL, output)
                self.assertIn(f"replay 2: {expected}", output)
        code, output = self.run_command([("700", {}), ("701", {"run_attempt": 2})])
        self.assertEqual(code, perf_settings.EXIT_PASS, output)

    def test_a_legacy_harness_sample_the_attribution_cannot_read_is_refused(self):
        # A harness declaring neither the split nor the timeline schema gets no sample checks from the shared
        # validator, so NaN, infinity, a negative latency or a bare number must be refused here with exit 1,
        # never a crash in the exact arithmetic.
        legacy = dict(perf_echo_tests.ECHO_CAPABILITIES, latency_split_schema=None, echo_timeline_schema=None)
        good = [{"latency_ms": 10.0}] * 5
        for bad, expected in ((float("nan"), "latency_ms nan is neither null"),
                              (float("inf"), "latency_ms inf is neither null"),
                              (-1.0, "latency_ms -1.0 is neither null")):
            with self.subTest(bad=bad):
                samples = [{"latency_ms": bad}] + good[1:]
                results = {"01-base": {"latency": {"samples": good, "attributed": 5, "total": 5, "coverage": 1.0}},
                           "02-head": {"latency": {"samples": samples, "attributed": 5, "total": 5, "coverage": 1.0}}}
                artifact = {"runs": {"01-base": 5, "02-head": 5}, "capabilities": legacy, "results": results}
                code, output = self.run_command([("700", artifact), ("701", artifact)])
                self.assertEqual(code, perf_settings.EXIT_FAIL, output)
                self.assertIn(f"replay 1 02-head sample 1: {expected}", output)
        for sample, expected in ((10.0, "float is not a sample object"), ({"attributed": True}, "has no latency_ms")):
            with self.subTest(sample=sample):
                head_latency = {"samples": [sample] + good[1:], "attributed": 5, "total": 5, "coverage": 1.0}
                results = {"01-base": {"latency": {"samples": good, "attributed": 5, "total": 5, "coverage": 1.0}},
                           "02-head": {"latency": head_latency}}
                execution, problems = self.load(runs={"01-base": 5, "02-head": 5}, capabilities=legacy,
                                                results=results)
                self.assertIsNone(execution)
                self.assertIn(f"replay 1 02-head sample 1: {expected}", problems)

    def legacy_artifact(self, head_sample):
        """A legacy-harness artifact (no split or timeline schema) whose first head sample is `head_sample`."""
        legacy = dict(perf_echo_tests.ECHO_CAPABILITIES, latency_split_schema=None, echo_timeline_schema=None)
        good = [{"latency_ms": 10.0}] * 5
        results = {"01-base": {"latency": {"samples": good, "attributed": 5, "total": 5, "coverage": 1.0}},
                   "02-head": {"latency": {"samples": [head_sample] + good[1:], "attributed": 5, "total": 5,
                                           "coverage": 1.0}}}
        return {"runs": {"01-base": 5, "02-head": 5}, "capabilities": legacy, "results": results}

    def test_an_oversized_integer_latency_is_refused_and_ordinary_numbers_read(self):
        # An integer latency above U64_MAX overflows a float conversion, so it is refused by bound with a named
        # sample error and exit 1, never an OverflowError; U64_MAX itself, an ordinary int and float still read.
        artifact = self.legacy_artifact({"latency_ms": 10 ** 400})
        code, output = self.run_command([("700", artifact), ("701", artifact)])
        self.assertEqual(code, perf_settings.EXIT_FAIL, output)
        self.assertIn("replay 1 02-head sample 1: latency_ms is an integer above 2**64 - 1", output)
        execution, problems = self.load(**self.legacy_artifact({"latency_ms": perf_attribution_schema.U64_MAX + 1}))
        self.assertIsNone(execution)
        self.assertIn("replay 1 02-head sample 1: latency_ms is an integer above 2**64 - 1", problems)
        execution, problems = self.load(**self.legacy_artifact({"latency_ms": perf_attribution_schema.U64_MAX}))
        self.assertEqual(problems, [])
        for ordinary in (12, 12.5):
            with self.subTest(latency_ms=ordinary):
                artifact = self.legacy_artifact({"latency_ms": ordinary})
                code, output = self.run_command([("700", artifact), ("701", artifact)])
                self.assertEqual(code, perf_settings.EXIT_PASS, output)
                self.assertIn("- outcome: inconclusive", output)

    def test_another_platform_has_no_set_to_read(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = write_echo_artifact(Path(temporary), ECHO_ATTEMPTS)
            execution, problems = perf_echo_loader.load_echo_execution(root, "replay 1", "windows")
        self.assertIsNone(execution)
        self.assertIn("has no windows S2/flood counters set in its inventory", problems[0])

    def test_the_mode_takes_no_comparison_option_and_names_its_controls(self):
        for argv in (["--echo-attribution", "a", "--base", "main"], ["--echo-attribution", "a", "--smoke"],
                     ["--echo-attribution", "a", "--check-previous-release"], ["--echo-attribution", "a", "--out", "o"],
                     ["--echo-control", "C1", "a"], ["--echo-platform", "macos"],
                     ["--echo-attribution", "a", "--echo-control", "C6", "b"]):
            with self.subTest(argv=argv), self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                perf_arguments.parse_args(argv)
        args = perf_arguments.parse_args(["--echo-attribution", "a", "b", "--echo-control", "C5", "c", "--echo-platform",
                                "windows"])
        self.assertEqual((len(args.echo_attribution), args.echo_control, args.echo_platform),
                         (2, [["C5", "c"]], "windows"))



@unittest.skipUnless(hasattr(os, "mkfifo"), "named pipes are POSIX only")
class ScratchKeepingTests(unittest.TestCase):
    def test_a_go_fifo_is_recorded_by_name_and_not_copied(self) -> None:
        # The harness releases a POSIX role through a FIFO at go/<role>. Keeping the scratch must not read
        # or copy it (copytree fails on a named pipe); it is listed by name and the rest is still kept.
        with tempfile.TemporaryDirectory() as temp:
            scratch, kept = Path(temp) / "scratch", Path(temp) / "kept"
            for name in ("result.json", "logs/sonicterm.log.2026-10-26", "go/1.port"):
                (scratch / name).parent.mkdir(parents=True, exist_ok=True)
                (scratch / name).write_text("x", encoding="utf-8")
            os.mkfifo(scratch / "go" / "0")
            perf_execution._keep_scratch(scratch, kept)
            for name in ("result.json", "logs/sonicterm.log.2026-10-26", "go/1.port"):
                self.assertTrue((kept / name).is_file(), name)
            self.assertFalse(os.path.lexists(kept / "go" / "0"), "the FIFO itself is never copied")
            record = json.loads((kept / perf_execution.SPECIAL_FILES_RECORD).read_text(encoding="utf-8"))
            self.assertEqual(record, [{"path": "go/0", "kind": "fifo"}])



class OutputDiagnosticTests(unittest.TestCase):
    def problems(self, **fields):
        """What validate_result reports for one transition phase carrying `fields`, under phase kinds."""
        result = perf_result_tests.valid_result(phases=[perf_kind_tests.transition_phase(40.0, **fields)])
        return perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0, phase_kinds=1)

    def test_output_diagnostics_are_optional_and_checked_only_when_present(self):
        # A print phase's GO-to-first-output and first-output-to-present diagnostics may be absent on either ref,
        # which is never a schema problem; when present they must be finite and non-negative.
        self.assertEqual(self.problems(), [])
        self.assertEqual(self.problems(go_to_first_output_ms=8.5, first_output_to_present_ms=4.0), [])
        self.assertEqual(self.problems(go_to_first_output_ms=8.5), [])
        self.assertIn("phase 'image' field go_to_first_output_ms has the wrong type",
                      self.problems(go_to_first_output_ms=-1.0))
        self.assertIn("phase 'image' field first_output_to_present_ms has the wrong type",
                      self.problems(first_output_to_present_ms=float("nan")))

    def test_output_diagnostics_are_rows_with_their_run_count_and_never_metrics(self):
        # The comparison shows each diagnostic with how many accepted runs recorded it; a run without it is
        # counted, never failed, and no diagnostic becomes a run metric, so no flag or regression list reads it.
        base = perf_kind_tests.side_with(perf_kind_tests.kind_outcome(perf_kind_tests.transition_phase(40.0, go_to_first_output_ms=8.0, first_output_to_present_ms=5.0),
                                      evidence="/e/01-base"),
                         perf_kind_tests.kind_outcome(perf_kind_tests.transition_phase(42.0), evidence="/e/04-base"))
        head = perf_kind_tests.side_with(perf_kind_tests.kind_outcome(perf_kind_tests.transition_phase(30.0, go_to_first_output_ms=6.0), evidence="/e/02-head"),
                         perf_kind_tests.kind_outcome(perf_kind_tests.transition_phase(31.0, go_to_first_output_ms=7.0), evidence="/e/03-head"))
        rows = perf_comparison_rows.comparison_rows("S8/default", base, head)
        self.assertEqual(perf_comparison_tests.row_for(rows, "image GO to first output (ms)")[2:4],
                         ["8.00 (8.00–8.00), recorded in 1 of 2 runs", "6.50 (6.00–7.00), recorded in 2 of 2 runs"])
        self.assertEqual(perf_comparison_tests.row_for(rows, "image first output to present (ms)")[2:4],
                         ["5.00 (5.00–5.00), recorded in 1 of 2 runs", "n/a, recorded in 0 of 2 runs"])
        for outcome in base.outcomes + head.outcomes:
            metric_names = [name for name, _unit, _kind in perf_metrics.run_metrics(outcome)]
            self.assertFalse(any("first output" in name for name in metric_names), metric_names)
        plain = perf_kind_tests.side_with(perf_kind_tests.kind_outcome(perf_kind_tests.transition_phase(40.0), evidence="/e/01-base"))
        names = [row[1] for row in perf_comparison_rows.comparison_rows("S8/default", plain, plain)]
        self.assertFalse(any("first output" in name for name in names), "no run recorded one, so no row")
