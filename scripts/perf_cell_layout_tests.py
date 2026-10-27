"""Perf cell layout tests."""

from __future__ import annotations
from perf_test_fixture import perf
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import perf_cell_layout
import perf_hook_tests
import perf_memory
import perf_memory_tests
import perf_processes_tests


def cell_layout_run(name, grid_bytes, resident_bytes, workflow_run="1", panes=(3, 3, 0), supported=True):
    """A head S12 run whose `end` checkpoint read `grid_bytes` of grid and `resident_bytes` resident."""
    total, sampled, contended = panes
    sample = perf_memory.MemorySample(60.0, resident_bytes, 16 * perf_hook_tests.CELL_MIB, 0, checkpoint_index=2, checkpoint_label="end",
                               checkpoint_attempt=1, checkpoint_complete=True, panes_total=total,
                               panes_sampled=sampled, panes_contended=contended, grid_visible_bytes=grid_bytes,
                               grid_history_bytes=0, grid_alternate_bytes=0)
    result = {"checkpoint_memory": "supported" if supported else "unsupported",
              "checkpoints": [{"index": 2, "label": "end", "sampling": "complete", "attempts": 1,
                               "last_attempt_complete": True}]}
    return perf_cell_layout.CellLayoutRun(name, workflow_run, result, [sample] if supported else [])


def cell_layout_runs(pairs, prefix="run"):
    """Five (or however many) runs from (grid MiB, resident MiB) pairs."""
    return [cell_layout_run(f"{prefix}{index}", int(grid * perf_hook_tests.CELL_MIB), int(resident * perf_hook_tests.CELL_MIB))
            for index, (grid, resident) in enumerate(pairs)]


PASSING = [(30, 100)] * 5
FAILING = [(6, 100)] * 5


class CellLayoutBoundaryTests(unittest.TestCase):
    """Both clauses are inclusive and exact: 10G = 3R passes the share clause, one byte less fails; G = 24 MiB
    passes the size clause (8 MiB saved), one byte less fails."""

    def platform(self, grid_bytes, resident_bytes):
        runs = [cell_layout_run(f"run{index}", grid_bytes, resident_bytes) for index in range(5)]
        return perf_cell_layout.cell_layout_platform("macOS", runs)

    def test_the_share_clause_is_inclusive_and_exact(self):
        # 10G = 3R exactly passes; with 3R % 10 == 1, G = (3R - 1) / 10 is the nearest integer pair that fails.
        resident = 100 * perf_hook_tests.CELL_MIB
        self.assertNotIn("share", self.platform(3 * resident // 10, resident).failed_clauses)
        odd_resident = 100 * perf_hook_tests.CELL_MIB + 7
        self.assertEqual(3 * odd_resident % 10, 1)
        self.assertIn("share", self.platform((3 * odd_resident - 1) // 10, odd_resident).failed_clauses)

    def test_the_size_clause_is_inclusive_and_exact(self):
        self.assertNotIn("size", self.platform(24 * perf_hook_tests.CELL_MIB, 50 * perf_hook_tests.CELL_MIB).failed_clauses)
        self.assertIn("size", self.platform(24 * perf_hook_tests.CELL_MIB - 1, 50 * perf_hook_tests.CELL_MIB).failed_clauses)


class CellLayoutAggregationTests(unittest.TestCase):
    """The platform statistic is the median of per-run ratios, never a ratio of medians, and one outlier does
    not move it."""

    def test_the_median_of_ratios_not_the_ratio_of_medians(self):
        # Per-run shares 30%, 15%, 15%, 1%, 1%: median 15% fails, although median G / median R = 30 / 100 passes.
        judged = perf_cell_layout.cell_layout_platform("macOS", cell_layout_runs([(30, 100), (30, 200), (30, 200),
                                                                      (1, 100), (1, 100)]))
        self.assertIn("share", judged.failed_clauses)
        self.assertFalse(judged.passes)

    def test_an_outlier_does_not_change_the_result(self):
        steady = perf_cell_layout.cell_layout_platform("macOS", cell_layout_runs(FAILING))
        outlier = perf_cell_layout.cell_layout_platform("macOS", cell_layout_runs([(60, 100)] + FAILING[1:]))
        self.assertEqual((steady.passes, steady.share), (outlier.passes, outlier.share))


class CellLayoutOutcomeTests(unittest.TestCase):
    """Go needs one platform passing both clauses; NoGo needs both platforms decidable and failing; anything
    missing is Inconclusive and names the gap, never a NoGo."""

    def test_both_clauses_must_hold_on_the_same_platform(self):
        share_only = cell_layout_runs([(6, 10)] * 5)       # 60% share, 2 MiB saving
        size_only = cell_layout_runs([(30, 1000)] * 5)     # 3% share, 10 MiB saving
        self.assertEqual(perf_cell_layout.cell_layout_decision({"macOS": share_only, "Windows": size_only}).outcome, "NoGo")
        both = cell_layout_runs(PASSING)
        self.assertEqual(perf_cell_layout.cell_layout_decision({"macOS": both, "Windows": size_only}).outcome, "Go")

    def test_missing_runs_are_inconclusive_unless_the_other_platform_passes(self):
        decision = perf_cell_layout.cell_layout_decision({"macOS": cell_layout_runs(FAILING[:4]),
                                              "Windows": cell_layout_runs(FAILING)})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertIn("macOS: 4 valid runs, need exactly 5", decision.reasons)
        other_passes = perf_cell_layout.cell_layout_decision({"macOS": cell_layout_runs(FAILING[:4]),
                                                  "Windows": cell_layout_runs(PASSING)})
        self.assertEqual(other_passes.outcome, "Go")

    def test_runs_from_two_workflow_runs_are_never_pooled(self):
        mixed = cell_layout_runs(FAILING[:3]) + [cell_layout_run(f"late{index}", 6 * perf_hook_tests.CELL_MIB, 100 * perf_hook_tests.CELL_MIB,
                                                                 workflow_run="2") for index in range(3)]
        decision = perf_cell_layout.cell_layout_decision({"macOS": mixed, "Windows": cell_layout_runs(FAILING)})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertTrue(any("2 workflow runs" in reason for reason in decision.reasons))

    def test_invalid_inputs_name_their_reason(self):
        cases = {"panes_contended=1": dict(panes=(3, 3, 1)), "panes_sampled=2": dict(panes=(3, 2, 0)),
                 "panes_total=4": dict(panes=(4, 4, 0))}
        for reason, overrides in cases.items():
            run = cell_layout_run("bad", 6 * perf_hook_tests.CELL_MIB, 100 * perf_hook_tests.CELL_MIB, **overrides)
            self.assertEqual(perf_cell_layout.cell_layout_reading(run).problem, reason)
        self.assertEqual(perf_cell_layout.cell_layout_reading(cell_layout_run("zero", 6 * perf_hook_tests.CELL_MIB, 0)).problem,
                         "process_resident_bytes=0")

    def test_unsupported_resident_and_a_missing_grid_field_are_inconclusive_not_nogo(self):
        unsupported = cell_layout_run("unsupported", 6 * perf_hook_tests.CELL_MIB, None)
        self.assertEqual(perf_cell_layout.cell_layout_reading(unsupported).problem, "process_resident_bytes unsupported")
        base = cell_layout_run("no-history", 6 * perf_hook_tests.CELL_MIB, 100 * perf_hook_tests.CELL_MIB)
        sample = base.memory[0]
        no_history = perf_cell_layout.CellLayoutRun("no-history", "1", base.result, [perf_memory.MemorySample(
            sample.unix_s, sample.process_resident_bytes, sample.renderer_total_bytes, 0, checkpoint_index=2,
            checkpoint_label="end", checkpoint_attempt=1, checkpoint_complete=True, panes_total=3, panes_sampled=3,
            panes_contended=0, grid_visible_bytes=6 * perf_hook_tests.CELL_MIB, grid_history_bytes=None, grid_alternate_bytes=0)])
        self.assertEqual(perf_cell_layout.cell_layout_reading(no_history).problem, "missing grid_history_bytes")
        for bad in (unsupported, no_history):
            decision = perf_cell_layout.cell_layout_decision({"macOS": cell_layout_runs(FAILING[:4]) + [bad],
                                                  "Windows": cell_layout_runs(FAILING)})
            self.assertEqual(decision.outcome, "Inconclusive")
            self.assertTrue(any(bad.name in reason for reason in decision.reasons))

    def test_a_rejected_replacement_is_named_and_not_counted(self):
        focus = perf_cell_layout.CellLayoutRun("focus", "1", cell_layout_run("focus", 6 * perf_hook_tests.CELL_MIB, 100 * perf_hook_tests.CELL_MIB).result, [],
                                   classification="focus")
        judged = perf_cell_layout.cell_layout_platform("macOS", cell_layout_runs(FAILING) + [focus])
        self.assertFalse(judged.passes)
        self.assertEqual(sum(reading.problem is None for reading in judged.readings), 5)
        self.assertIn("macOS focus: run classified focus", judged.reasons)

    def test_the_same_run_counted_five_times_is_not_five_runs(self):
        same = cell_layout_run("only", 30 * perf_hook_tests.CELL_MIB, 100 * perf_hook_tests.CELL_MIB)
        decision = perf_cell_layout.cell_layout_decision({"macOS": [same] * 5, "Windows": cell_layout_runs(FAILING)})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertTrue(any("runs repeated" in reason for reason in decision.reasons))

    def test_platforms_from_two_workflow_runs_never_make_a_nogo(self):
        windows = [cell_layout_run(f"win{index}", 6 * perf_hook_tests.CELL_MIB, 100 * perf_hook_tests.CELL_MIB, workflow_run="2")
                   for index in range(5)]
        decision = perf_cell_layout.cell_layout_decision({"macOS": cell_layout_runs(FAILING), "Windows": windows})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertTrue(any("different workflow runs" in reason for reason in decision.reasons))
        # A platform whose own five runs pass still decides a go.
        passing = perf_cell_layout.cell_layout_decision({"macOS": cell_layout_runs(PASSING), "Windows": windows})
        self.assertEqual(passing.outcome, "Go")


class CellLayoutHeadOnlyTests(unittest.TestCase):
    """The decision reads the head alone: a head without checkpoint sampling is Inconclusive, never NoGo."""

    def test_unsupported_head_sampling_is_inconclusive(self):
        unsupported = [cell_layout_run(f"run{index}", 0, 0, supported=False) for index in range(5)]
        decision = perf_cell_layout.cell_layout_decision({"macOS": unsupported, "Windows": unsupported})
        self.assertEqual(decision.outcome, "Inconclusive")
        self.assertIn("checkpoint sampling unsupported", decision.reasons[0])


def cell_line(index, label, attempt, complete, grid_history_bytes, resident_bytes=100 * perf_hook_tests.CELL_MIB):
    """A tagged memory line as the App writes it, for checkpoint `index`."""
    return perf_memory_tests.memory_line(resident=str(resident_bytes), grid=(
        f"grid_visible_bytes=0 grid_history_bytes={grid_history_bytes} grid_alternate_bytes=0 panes_total=3 "
        f"panes_sampled=3 panes_contended=0 checkpoint_index={index} checkpoint_label=\"{label}\" "
        f"checkpoint_attempt={attempt} checkpoint_complete={'true' if complete else 'false'} "))


class CellLayoutParsedLineTests(unittest.TestCase):
    """Retried, exhausted, repeated, conflicting and earlier-checkpoint samples reach the decision correctly, from
    parsed log lines, each beside four other valid runs."""

    def run_from(self, name, lines, sampling="complete"):
        memory = [sample for sample in map(perf_memory.parse_memory_line, lines) if sample]
        result = {"checkpoint_memory": "supported",
                  "checkpoints": [{"index": 0, "label": "settled", "sampling": "complete"},
                                  {"index": 1, "label": "covered", "sampling": "complete"},
                                  {"index": 2, "label": "end", "sampling": sampling}]}
        return perf_cell_layout.CellLayoutRun(name, "1", result, memory)

    def decide(self, candidate):
        """The decision with `candidate` as macOS's fifth run and five failing Windows runs."""
        others = [self.run_from(f"ok{index}", [cell_line(2, "end", 1, True, 7)]) for index in range(4)]
        decision = perf_cell_layout.cell_layout_decision({"macOS": others + [candidate], "Windows": cell_layout_runs(FAILING)})
        valid = sum(reading.problem is None for reading in decision.platforms["macOS"].readings)
        return decision, valid

    def test_a_retried_end_uses_its_complete_attempt(self):
        decision, valid = self.decide(self.run_from(
            "retried", [cell_line(2, "end", 1, False, 1), cell_line(2, "end", 2, True, 7)]))
        self.assertEqual((decision.outcome, valid), ("NoGo", 5))
        # The candidate contributes attempt 2's grid bytes, not attempt 1's.
        candidate = decision.platforms["macOS"].readings[-1]
        self.assertEqual((candidate.name, candidate.grid_bytes, candidate.resident_bytes),
                         ("retried", 7, 100 * perf_hook_tests.CELL_MIB))

    def test_exhausted_sampling_is_inconclusive_not_nogo(self):
        decision, valid = self.decide(self.run_from(
            "exhausted", [cell_line(2, "end", attempt, False, 7) for attempt in range(1, 9)], sampling="exhausted"))
        self.assertEqual((decision.outcome, valid), ("Inconclusive", 4))
        self.assertTrue(any("exhausted: partial" in reason for reason in decision.reasons))

    def test_a_repeated_end_line_counts_once(self):
        decision, valid = self.decide(self.run_from("repeated", [cell_line(2, "end", 1, True, 7)] * 2))
        self.assertEqual((decision.outcome, valid), ("NoGo", 5))

    def test_conflicting_end_samples_make_the_run_invalid(self):
        decision, valid = self.decide(self.run_from(
            "conflict", [cell_line(2, "end", 1, True, 7), cell_line(2, "end", 1, True, 8)]))
        self.assertEqual((decision.outcome, valid), ("Inconclusive", 4))
        self.assertTrue(any("conflict: conflicting samples" in reason for reason in decision.reasons))

    def test_only_the_end_checkpoint_is_read(self):
        # Huge settled and covered samples would pass both clauses; only end's small one may count.
        lines = [cell_line(0, "settled", 1, True, 90 * perf_hook_tests.CELL_MIB), cell_line(1, "covered", 1, True, 90 * perf_hook_tests.CELL_MIB),
                 cell_line(2, "end", 1, True, 7)]
        decision, valid = self.decide(self.run_from("earlier", lines))
        self.assertEqual((decision.outcome, valid), ("NoGo", 5))
        self.assertEqual(decision.platforms["macOS"].readings[-1].grid_bytes, 7)


CELL_HEAD = "a" * 40


def write_cell_artifact(root, platform, runs, shard="S1-S3-S6-S8-S12", head=CELL_HEAD, run_id="1"):
    """A `perf-comparison-*` artifact holding head S12 runs; each run is (name, outcome dict or raw text)."""
    artifact = root / f"perf-comparison-1586-{head}-{platform}-{shard}-1"
    (artifact / "runs" / "S12-default" / "timed").mkdir(parents=True)
    (artifact / "timing.json").write_text(json.dumps({"run_id": run_id, "shard": shard}), encoding="utf-8")
    for name, outcome in runs:
        run_dir = artifact / "runs" / "S12-default" / "timed" / name
        (run_dir / "scratch").mkdir(parents=True)
        text = outcome if isinstance(outcome, str) else json.dumps(outcome)
        (run_dir / "outcome.json").write_text(text, encoding="utf-8")
        (run_dir / "scratch" / "result.json").write_text(json.dumps(
            {"checkpoint_memory": "supported", "checkpoints": [{"index": 2, "label": "end", "sampling": "complete"}]}),
            encoding="utf-8")
        (run_dir / "01-harness.log").write_text(
            "\x1b[2m" + cell_line(2, "end", 1, True, 7) + "\x1b[0m\n", encoding="utf-8")
    return artifact


HEAD_OUTCOME = {"kind": "valid", "side": "head", "scenario": "S12", "variant": "default"}


class CellLayoutArtifactTests(unittest.TestCase):
    """The reader takes only the requested workflow run's S12-shard head runs, names evidence from anything else, and
    turns unreadable files into named invalid runs instead of failing."""

    def test_each_platform_reads_only_its_own_s12_shard(self):
        # macOS reads S2-S3-S8-S12 among its S12 shards and its earlier S1-S3-S6-S8-S12; Windows reads only its own
        # S1-S3-S6-S8-S12, never the macOS name; a timing.json naming another shard is a named problem.
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            write_cell_artifact(root, "macOS", [("01-head", HEAD_OUTCOME)], shard="S2-S3-S8-S12")
            write_cell_artifact(root, "Windows", [("01-head", HEAD_OUTCOME)], shard="S2-S3-S8-S12")
            runs = perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)
            self.assertEqual([run.problem for run in runs["macOS"]], [None])
            self.assertEqual(runs["Windows"], [])
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            artifact = write_cell_artifact(root, "macOS", [("01-head", HEAD_OUTCOME)], shard="S2-S3-S8-S12")
            (artifact / "timing.json").write_text(json.dumps({"run_id": "1", "shard": "S7-S4"}), encoding="utf-8")
            runs = perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)
            # A run read without its shard check has no problem; that must fail as an assertion, not a TypeError.
            self.assertIn("artifact shard S7-S4, expected S2-S3-S8-S12", runs["macOS"][0].problem or "")

    def test_the_reader_keeps_provenance_and_names_foreign_evidence(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            write_cell_artifact(root, "macOS", [(f"0{index}-head", HEAD_OUTCOME) for index in range(1, 6)])
            write_cell_artifact(root, "Windows", [("01-head", HEAD_OUTCOME)], shard="S7")
            write_cell_artifact(root, "Windows", [("01-head", HEAD_OUTCOME), ("02-head", HEAD_OUTCOME)], run_id="2")
            runs = perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)
            self.assertEqual(len(runs["macOS"]), 5)
            self.assertTrue(all(run.problem is None for run in runs["macOS"]))
            self.assertEqual(perf_cell_layout.cell_layout_reading(runs["macOS"][0]).grid_bytes, 7)
            # The S7 artifact is not read; the other workflow run's is read but named.
            self.assertEqual(len(runs["Windows"]), 2)
            self.assertTrue(all("workflow run 2" in run.problem for run in runs["Windows"]))

    def test_another_head_side_scenario_or_unreadable_file_is_a_named_invalid_run(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            write_cell_artifact(root, "macOS", [("01-head", dict(HEAD_OUTCOME, side="base")),
                                                ("02-head", dict(HEAD_OUTCOME, scenario="S7")),
                                                ("03-head", "{not json"),
                                                ("04-head", "[1, 2]")])
            write_cell_artifact(root, "Windows", [("01-head", HEAD_OUTCOME)], head="b" * 40)
            runs = perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)
            problems = [run.problem for run in runs["macOS"]]
            self.assertIn("side=base", problems[0])
            self.assertIn("scenario=S7", problems[1])
            self.assertIn("outcome.json unreadable", problems[2])
            self.assertEqual(problems[3], "outcome.json is not an object")
            self.assertIn("measured head " + "b" * 40, runs["Windows"][0].problem)
            decision = perf_cell_layout.cell_layout_decision(runs)
            self.assertEqual(decision.outcome, "Inconclusive")
            # Every rejected run is named: the four macOS runs and the Windows run of the other head.
            for run in runs["macOS"] + runs["Windows"]:
                self.assertTrue(any(run.name in reason for reason in decision.reasons), run.name)


class CellLayoutDamagedEvidenceTests(unittest.TestCase):
    """Damaged result, timing and log evidence becomes a named invalid run; collection and the other platform's
    decision continue."""

    def test_damaged_files_are_named_and_a_passing_platform_still_decides(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            macos = write_cell_artifact(root, "macOS", [(f"0{index}-head", HEAD_OUTCOME) for index in range(1, 6)])
            timed = macos / "runs" / "S12-default" / "timed"
            (timed / "01-head" / "scratch" / "result.json").write_text(
                json.dumps({"checkpoint_memory": "supported", "checkpoints": 1}), encoding="utf-8")
            (timed / "02-head" / "scratch" / "result.json").write_text("[1]", encoding="utf-8")
            (timed / "03-head" / "scratch" / "result.json").write_text("{broken", encoding="utf-8")
            (timed / "04-head" / "01-harness.log").write_text(
                cell_line(2, "end", 1, True, 7).replace(perf_processes_tests.STAMP[:10], "2026-99-03"), encoding="utf-8")
            unreadable = timed / "05-head" / "01-harness.log"
            original_read_text = Path.read_text

            def refuse_one_log(path, *args, **kwargs):
                # A portable unreadable file: chmod cannot make one on Windows, so this one read is refused.
                if path == unreadable:
                    raise PermissionError(13, "Permission denied", str(path))
                return original_read_text(path, *args, **kwargs)

            with mock.patch.object(Path, "read_text", refuse_one_log):
                runs = perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)
            problems = [perf_cell_layout.cell_layout_reading(run).problem for run in runs["macOS"]]
            self.assertEqual(problems[0], "result.json checkpoints is not a list")
            self.assertEqual(problems[1], "result.json is not an object")
            self.assertIn("result.json unreadable", problems[2])
            self.assertIn("memory log unreadable", problems[3])
            self.assertIn("memory log unreadable", problems[4])
            # Windows' own five passing runs still decide a go.
            windows = [cell_layout_run(f"win{index}", 30 * perf_hook_tests.CELL_MIB, 100 * perf_hook_tests.CELL_MIB, workflow_run="1")
                       for index in range(5)]
            windows = [perf_cell_layout.CellLayoutRun(run.name, run.workflow_run, run.result, run.memory, head_sha=CELL_HEAD)
                       for run in windows]
            self.assertEqual(perf_cell_layout.cell_layout_decision({"macOS": runs["macOS"], "Windows": windows}).outcome, "Go")

    def test_a_damaged_timing_file_names_every_run_of_its_artifact(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            artifact = write_cell_artifact(root, "macOS", [("01-head", HEAD_OUTCOME)])
            (artifact / "timing.json").write_text("{broken", encoding="utf-8")
            broken = perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)["macOS"][0]
            self.assertIn("timing.json unreadable", broken.problem)
            (artifact / "timing.json").write_text("[1]", encoding="utf-8")
            self.assertEqual(perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)["macOS"][0].problem,
                             "timing.json is not an object")


class CellLayoutUndecodableEvidenceTests(unittest.TestCase):
    """JSON that fails to decode for any reason, and an unavailable download, leave named or missing evidence instead
    of an exception."""

    def test_an_over_long_integer_and_deep_nesting_are_named(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            artifact = write_cell_artifact(root, "macOS", [("01-head", HEAD_OUTCOME), ("02-head", HEAD_OUTCOME)])
            timed = artifact / "runs" / "S12-default" / "timed"
            # Python refuses to convert an integer string past its default 4,300-digit limit (ValueError).
            (timed / "01-head" / "scratch" / "result.json").write_text('{"n": ' + "9" * 5000 + "}", encoding="utf-8")
            first = perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)["macOS"][0]
            self.assertIn("result.json unreadable", first.problem)
            # Whether deep nesting overflows depends on the interpreter, so the RecursionError is injected.
            with mock.patch.object(perf.json, "loads", side_effect=RecursionError("maximum recursion depth")):
                nested = perf_cell_layout.read_cell_layout_runs(root, "1", CELL_HEAD)["macOS"]
            self.assertTrue(all("unreadable: maximum recursion depth" in run.problem for run in nested))

    def test_an_unavailable_download_is_missing_evidence(self):
        runs = perf_cell_layout.read_cell_layout_runs(Path(tempfile.gettempdir()) / "no-such-download-1586", "1", CELL_HEAD)
        self.assertEqual(runs, {"macOS": [], "Windows": []})
        self.assertEqual(perf_cell_layout.cell_layout_decision(runs).outcome, "Inconclusive")
