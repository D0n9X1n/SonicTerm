#!/usr/bin/env python3
"""Tests for the frozen acceptance evaluator: run selection from metadata, artifact and run identity,
per-result validation, every row of the rule, and the whole-file freeze of the statistics it imports."""

from __future__ import annotations

import contextlib
import copy
import importlib.util
import io
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent

SPEC = importlib.util.spec_from_file_location("perf_1584_acceptance", SCRIPTS / "perf-1584-acceptance.py")
assert SPEC and SPEC.loader
evaluator = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = evaluator
SPEC.loader.exec_module(evaluator)

# The field table the fixtures build frame_counters from; read from the current perf-compare.
COMPARE_SPEC = importlib.util.spec_from_file_location("perf_1584_fixture_compare", SCRIPTS / "perf-compare.py")
assert COMPARE_SPEC and COMPARE_SPEC.loader
compare = importlib.util.module_from_spec(COMPARE_SPEC)
sys.modules[COMPARE_SPEC.name] = compare
COMPARE_SPEC.loader.exec_module(compare)

HEAD_SHA = "c" * 40
BASE_SHA = "b" * 40
RUN_ID = 4242
PULL_REQUEST = 9001
HARNESS = {"macOS": "aa" * 32, "Windows": "dd" * 32}
CREATED_AT = "2026-10-05T10:00:00Z"
PRESENTER = {"software_render_mode": "auto", "software_rendering": False, "software_render_degraded": False,
             "windows_gdi": False}
# Which scenario directories each comparison artifact holds, as section 7.2 lays them out.
LAYOUT = {
    "macOS": {"S1-S3-S6-S8-S12": ("S3-default", "S2-flood"), "S4-S5-S11": ("S4-default",)},
    "Windows": {"S1-S3-S6-S8-S12": ("S3-default",), "S9-S10": ("S2-flood",), "S4-S5-S11": ("S4-default",)},
}
PHASES = {"S3-default": ("startup", "flood"), "S2-flood": ("startup", "typing", "idle"),
          "S4-default": ("startup", "stream")}
RELEVANT = {"S3-default": "flood", "S2-flood": "typing", "S4-default": "stream"}
TIMED_RUNS = {"S3-default": 5, "S2-flood": 2, "S4-default": 5}
# Every relevant phase lasts 100 s, so presented frames are fps × 100.
PHASE_WALL_S = 100.0


def latency_run(values, unattributed=0):
    """One run's latency: attributed samples, then `unattributed` samples no frame was credited with."""
    return {"values": list(values), "unattributed": unattributed}


def default_figures():
    """A passing experiment: every row holds with margin on both platforms."""
    flat_latency = [latency_run([10.0 + step for step in range(100)]) for _ in range(2)]
    return {
        ("macOS", "S3-default", "fps"): {"base": [4.0, 4.1, 3.9, 4.2, 4.0], "head": [9.0, 8.5, 9.5, 8.8, 9.2]},
        ("macOS", "S3-default", "throughput"): {"base": [11.0, 11.1, 10.9, 11.2, 11.0],
                                                "head": [11.5, 11.4, 11.6, 11.3, 11.5]},
        ("Windows", "S3-default", "fps"): {"base": [20.0, 21.0, 19.0, 22.0, 20.0],
                                           "head": [21.0, 22.0, 20.0, 23.0, 21.0]},
        ("Windows", "S3-default", "throughput"): {"base": [9.0, 9.1, 8.9, 9.2, 9.0],
                                                  "head": [9.1, 9.2, 9.0, 9.3, 9.1]},
        ("macOS", "S2-flood", "latency"): {"base": flat_latency, "head": copy.deepcopy(flat_latency)},
        ("Windows", "S2-flood", "latency"): {"base": copy.deepcopy(flat_latency), "head": copy.deepcopy(flat_latency)},
        ("macOS", "S4-default", "fps"): {"base": [30.0] * 5, "head": [30.0] * 5},
        ("Windows", "S4-default", "fps"): {"base": [25.0] * 5, "head": [25.0] * 5},
        # Head counters runs' flood figures: wakes, frames, lost, tokens at start and end, sends, requests.
        ("macOS", "counters"): [dict(wakes=4, frames=4, lost=0, start=0, end=0, yields=4, requests=4)] * 2,
        ("Windows", "counters"): [dict(wakes=0, frames=0, lost=0, start=0, end=0, yields=0, requests=0)] * 2,
    }


def counters_object(figures=None):
    """A complete frame_counters object with every field zero, then the handshake figures given."""
    sections = {}
    for section, (counts, histograms, levels) in compare.FRAME_COUNTER_FIELDS.items():
        body = {name: 0 for name in counts + levels}
        for name in histograms:
            unit = name.rsplit("_", 1)[1]
            bounds = compare.HISTOGRAM_BOUNDS[unit]
            body[name] = {"unit": unit, "bounds": list(bounds), "counts": [0] * (len(bounds) + 1), "sum_us": 0}
        sections[section] = body
    if figures:
        window, vt_section = sections["window"], sections["vt"]
        window["parser_yield_wakes"] = figures["wakes"]
        window["parser_yield_frames"] = figures["frames"]
        window["parser_yield_lost"] = figures["lost"]
        window["parser_yield_tokens_start"] = figures["start"]
        window["parser_yield_tokens_end"] = figures["end"]
        window["parser_yield_requests"] = figures["requests"]
        vt_section["parser_yields"] = figures["yields"]
    return sections


def result_body(platform, scenario, set_name, side, index, figures):
    """One selected run's result.json, real-shaped for its scenario."""
    scenario_id, variant = scenario.split("-", 1)
    counting = set_name == "counters"
    phases, clock = [], 900.0
    for name in PHASES[scenario]:
        wall = PHASE_WALL_S if name == RELEVANT[scenario] else 10.0
        phase = {"name": name, "start_unix_s": clock, "end_unix_s": clock + wall, "cpu_user_s": 1.0,
                 "cpu_system_s": 0.5, "presented_frames": 60, "redraw_requested": 70, "dispatch_ms": [1.0],
                 "present_interval_ms": [16.7], "allocations_per_frame": None}
        clock += wall
        if counting:
            phase["frame_counters"] = counters_object()
        phases.append(phase)
    relevant = next(phase for phase in phases if phase["name"] == RELEVANT[scenario])
    fps_figures = figures.get((platform, scenario, "fps"))
    if fps_figures and not counting:
        relevant["presented_frames"] = round(fps_figures[side][index] * PHASE_WALL_S)
    if counting and side == "head":
        relevant["frame_counters"] = counters_object(figures[(platform, "counters")][index])
    throughput = None
    if scenario == "S3-default":
        megabytes_per_s = 10.0
        if not counting:
            megabytes_per_s = figures[(platform, scenario, "throughput")][side][index]
        throughput = {"bytes": round(megabytes_per_s * 10_000_000), "seconds": 10.0}
    latency = None
    if scenario == "S2-flood":
        run = figures[(platform, scenario, "latency")][side][index]
        samples = [{"latency_ms": value} for value in run["values"]] + [{"latency_ms": None}] * run["unattributed"]
        attributed = len(run["values"])
        total = attributed + run["unattributed"]
        latency = {"samples": samples, "attributed": attributed, "total": total, "coverage": attributed / total}
    return {"schema_version": 1, "managed": True, "harness_hash": HARNESS[platform], "status": "valid",
            "exit_code": 0, "scenario": scenario_id, "variant": variant, "short": True,
            "frame_counters": "on" if counting else "off", "grid": {"columns": 250, "rows": 70},
            "phases": phases, "latency": latency, "throughput": throughput, "uncover_ms": None,
            "scrollback_rows_retained": None,
            "checkpoints": [{"index": 0, "label": "end", "unix_s": clock, "footprint_file": None}],
            "finish_session_settled": True, "notes": [], "presenter": dict(PRESENTER)}


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def build_artifacts(root, figures=None, edit=None, attempt=1, extra_lines=None):
    """Write every comparison artifact the rule reads; `edit(context, result, outcome)` may change a run
    before it is written. Returns {(platform, shard): artifact path}."""
    figures = figures or default_figures()
    artifacts = {}
    for platform, shards in LAYOUT.items():
        for shard, scenarios in shards.items():
            artifact = root / f"perf-comparison-{PULL_REQUEST}-{HEAD_SHA}-{platform}-{shard}-{attempt}"
            artifacts[(platform, shard)] = artifact
            write_json(artifact / "timing.json", {
                "schema_version": 1, "run_id": str(RUN_ID), "run_attempt": str(attempt),
                "job": f"{platform} before/after comparison ({shard})", "shard": shard, "marks": {}})
            lines = []
            for scenario in scenarios:
                label = scenario.replace("-", "/", 1)
                sets = [("timed", TIMED_RUNS[scenario])]
                if scenario == "S3-default":
                    sets.append(("counters", 2))
                for set_name, run_count in sets:
                    attempt_number = 0
                    for index in range(run_count):
                        for side in ("base", "head"):
                            attempt_number += 1
                            folder = f"{attempt_number:02d}-{side}"
                            directory = artifact / "runs" / scenario / set_name / folder
                            result = result_body(platform, scenario, set_name, side, index, figures)
                            outcome = {"kind": "valid", "side": side, "scenario": scenario.split("-", 1)[0],
                                       "variant": scenario.split("-", 1)[1], "status": "PASS", "exit_code": 0}
                            context = dict(platform=platform, scenario=scenario, set_name=set_name, side=side,
                                           index=index, directory=directory)
                            if edit is not None:
                                edit(context, result, outcome)
                            write_json(directory / "outcome.json", outcome)
                            write_json(directory / "scratch" / "result.json", result)
                            if platform == "Windows":
                                evidence = f"D:\\a\\out\\runs\\{scenario}\\{set_name}\\{folder}"
                            else:
                                evidence = f"/Users/runner/out/runs/{scenario}/{set_name}/{folder}"
                            lines.append(f"- {label} {set_name} {side} {outcome['kind']}: `{evidence}/01-harness.log`")
            lines.extend((extra_lines or {}).get((platform, shard), []))
            document = "\n".join([f"- Base: `origin/main` = `{BASE_SHA}`", f"- Head: `HEAD` = `{HEAD_SHA}`",
                                  f"- Harness hash (both trees): `{HARNESS[platform]}`", "", "Raw logs:", "",
                                  *lines, ""])
            (artifact / "comparison.md").write_text(document, encoding="utf-8")
    return artifacts


def jobs_record(conclusions=None, completed_at="2026-10-05T10:28:00Z"):
    """The run's jobs API page: every required job completed, `success` unless overridden."""
    conclusions = conclusions or {}
    return [{"name": name, "status": "completed", "conclusion": conclusions.get(name, "success"),
             "started_at": "2026-10-05T10:01:00Z", "completed_at": completed_at}
            for name in evaluator.REQUIRED_JOBS]


class GhShim:
    """Answers only `gh run view` and the run's attempt jobs API; any other command fails the test."""

    def __init__(self, runs):
        # runs: {run_id: {"view": {...}, "jobs": {attempt: [...]}}}
        self.runs = runs
        self.calls = []

    def __call__(self, argv):
        argv = list(argv)
        self.calls.append(argv)
        if argv[:3] == ["gh", "run", "view"]:
            run = self.runs[int(argv[3])]
            assert argv[4:] == ["--repo", evaluator.DEFAULT_REPOSITORY, "--json", evaluator.VIEW_FIELDS], argv
            return json.dumps(run["view"]).encode("utf-8")
        if argv[:2] == ["gh", "api"] and len(argv) == 3 and "/attempts/" in argv[2] and "/jobs?" in argv[2]:
            path = argv[2]
            run_id = int(path.split("/actions/runs/")[1].split("/")[0])
            attempt = int(path.split("/attempts/")[1].split("/")[0])
            jobs = self.runs[run_id]["jobs"][attempt]
            return json.dumps({"total_count": len(jobs), "jobs": jobs}).encode("utf-8")
        raise AssertionError(f"the evaluator ran a command it may not: {argv}")


def run_view(run_id=RUN_ID, attempt=1, created_at=CREATED_AT, head_sha=HEAD_SHA, conclusions=None):
    """`gh run view --json` of one run attempt."""
    conclusions = conclusions or {}
    return {"databaseId": run_id, "attempt": attempt, "workflowName": "Performance comparison",
            "event": "pull_request", "headSha": head_sha, "createdAt": created_at,
            "updatedAt": "2026-10-05T10:28:30Z",
            "jobs": [{"name": name, "status": "completed", "conclusion": conclusions.get(name, "success")}
                     for name in evaluator.REQUIRED_JOBS]}


def call_main(argv, runner=None):
    """Run the evaluator's main; return its exit code and what it printed."""
    output, errors = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
        code = evaluator.main(argv, runner=runner)
    return code, output.getvalue() + errors.getvalue()


class AcceptanceFixture(unittest.TestCase):
    """A temp directory with a selection.json from `select` and a passing artifact set."""

    def setUp(self):
        self.temp = Path(tempfile.mkdtemp(prefix="perf-1584-tests-"))
        self.addCleanup(shutil.rmtree, self.temp, ignore_errors=True)
        self.artifacts = self.temp / "artifacts"
        self.artifacts.mkdir()

    def select(self, runs=None, extra=(), run_id=RUN_ID):
        """Write selection.json through `select` and the shim; return (exit code, output, shim)."""
        runs = runs or {RUN_ID: {"view": run_view(), "jobs": {1: jobs_record()}}}
        shim = GhShim(runs)
        code, output = call_main(["select", "--run", str(run_id), "--head", HEAD_SHA, "--merge-base", BASE_SHA,
                                  "--output", str(self.temp / "selection.json"), "--tree", str(ROOT), *extra],
                                 runner=shim)
        return code, output, shim

    def evaluate(self, tree=ROOT):
        """Evaluate the fixture's selection and artifacts; return (exit code, output)."""
        return call_main(["evaluate", "--selection", str(self.temp / "selection.json"),
                          "--artifacts", str(self.artifacts), "--tree", str(tree)])

    def passing(self, **build):
        """A selection and an artifact set built with `build`."""
        code, output, _shim = self.select()
        self.assertEqual(code, 0, output)
        build_artifacts(self.artifacts, **build)

    def row(self, output, name):
        """The printed verdict of row `name`."""
        for line in output.splitlines():
            if line.startswith(f"{name}: "):
                return line.split(": ", 2)[1]
        self.fail(f"row {name} not printed:\n{output}")


class RuleTests(AcceptanceFixture):
    def test_a_passing_set_is_accepted_with_every_row_printed(self):
        # Every row holds on real-shaped results, S2/flood and S4/default with no throughput, so the rule
        # accepts; each row prints its verdict with its operands.
        self.passing()
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)
        for name in ("A1", "A2", "G1 macOS", "G1 Windows", "G2", "G3 macOS", "G3 Windows", "G4 macOS",
                     "G4 Windows", "G5", "B"):
            self.assertEqual(self.row(output, name), "PASS", name)
        self.assertIn("worker_hold", output)

    def assert_only_row_fails(self, name, figures=None, edit=None):
        """Build with `figures` or `edit`, then expect a rejection in which only `name` fails."""
        self.passing(figures=figures, edit=edit)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_REJECT, output)
        failing = [line.split(":", 1)[0] for line in output.splitlines() if ": FAIL: " in line]
        self.assertEqual(failing, [name], output)

    def test_a1_fails_on_its_median_clause(self):
        # The head median must reach twice the base median.
        figures = default_figures()
        figures[("macOS", "S3-default", "fps")]["head"] = [7.9, 8.0, 7.8, 8.1, 7.9]
        self.assert_only_row_fails("A1", figures)

    def test_a1_fails_on_its_minimum_clause(self):
        # The head's worst run must also beat the base's typical run.
        figures = default_figures()
        figures[("macOS", "S3-default", "fps")]["head"] = [9.0, 8.5, 9.5, 8.8, 4.0]
        self.assert_only_row_fails("A1", figures)

    def test_a2_fails_below_the_frame_ratio(self):
        # Tokens resolved inside the phase must be at least 80% frames.
        figures = default_figures()
        figures[("macOS", "counters")] = [dict(wakes=5, frames=3, lost=2, start=0, end=0, yields=5, requests=5)] * 2
        self.assert_only_row_fails("A2", figures)

    def test_a2_fails_without_a_frame_in_a_run(self):
        # Every selected head counters run needs at least one wake and one frame.
        figures = default_figures()
        figures[("macOS", "counters")] = [dict(wakes=4, frames=4, lost=0, start=0, end=0, yields=4, requests=4),
                                          dict(wakes=0, frames=0, lost=0, start=0, end=0, yields=0, requests=0)]
        self.assert_only_row_fails("A2", figures)

    def test_a2_counts_a_token_crossing_the_phase_boundary_once(self):
        # A token open at the phase start and resolved inside it, and one open at its end, keep the invariant;
        # the ratio counts what resolved inside the phase and not the open one.
        figures = default_figures()
        figures[("macOS", "counters")] = [dict(wakes=3, frames=4, lost=0, start=1, end=0, yields=3, requests=3),
                                          dict(wakes=4, frames=3, lost=0, start=0, end=1, yields=4, requests=4)]
        self.passing(figures=figures)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)
        self.assertIn("ratio 7/7", output)

    def test_g1_fails_on_each_platform(self):
        # Throughput must hold 90% of the base median and the base minimum, per platform.
        figures = default_figures()
        figures[("macOS", "S3-default", "throughput")]["head"] = [9.8, 9.9, 9.7, 10.0, 9.8]
        self.assert_only_row_fails("G1 macOS", figures)

    def test_g1_fails_on_its_median_clause_alone(self):
        # A head median at or above the base minimum still fails below 90% of the base median.
        figures = default_figures()
        figures[("macOS", "S3-default", "throughput")] = {"base": [11.0, 11.0, 11.0, 11.0, 8.0],
                                                          "head": [9.5, 9.5, 9.5, 9.5, 9.5]}
        self.assert_only_row_fails("G1 macOS", figures)

    def test_g1_windows_fails_below_the_base_minimum(self):
        # 90% of the base median alone is not enough; the head median must also reach the base minimum.
        figures = default_figures()
        figures[("Windows", "S3-default", "throughput")]["head"] = [8.85, 8.85, 8.8, 8.9, 8.85]
        self.assert_only_row_fails("G1 Windows", figures)

    def test_g2_fails_below_the_windows_base_minimum(self):
        figures = default_figures()
        figures[("Windows", "S3-default", "fps")]["head"] = [18.0, 18.5, 18.0, 18.2, 18.1]
        self.assert_only_row_fails("G2", figures)

    def test_g3_fails_at_79_percent_coverage(self):
        # perf-compare's latency_acceptance needs 80% attribution on each side.
        figures = default_figures()
        figures[("macOS", "S2-flood", "latency")]["head"] = [latency_run([10.0] * 79, 21)] * 2
        self.assert_only_row_fails("G3 macOS", figures)

    def test_g3_fails_on_an_eleven_point_coverage_gap(self):
        # Both sides above 80% but 11 points apart cannot be compared.
        figures = default_figures()
        figures[("Windows", "S2-flood", "latency")]["head"] = [latency_run([10.0] * 89, 11)] * 2
        self.assert_only_row_fails("G3 Windows", figures)

    def test_g3_fails_on_its_p95(self):
        # The head pooled p95 may be at most 110% of the base's.
        figures = default_figures()
        slower = [latency_run([(10.0 + step) * 1.2 for step in range(100)]) for _ in range(2)]
        figures[("macOS", "S2-flood", "latency")]["head"] = slower
        self.assert_only_row_fails("G3 macOS", figures)

    def test_g4_fails_on_each_platform(self):
        figures = default_figures()
        figures[("Windows", "S4-default", "fps")]["head"] = [20.0] * 5
        self.assert_only_row_fails("G4 Windows", figures)

    def test_g5_fails_when_windows_yields(self):
        # Windows flood runs on the software path, where no request may be published or sent.
        figures = default_figures()
        figures[("Windows", "counters")] = [dict(wakes=0, frames=0, lost=0, start=0, end=0, yields=0, requests=1)] * 2
        self.assert_only_row_fails("G5", figures)

    def test_b_fails_over_budget(self):
        # The decisive run's creation-to-last-required-job span must be at most 30 minutes.
        runs = {RUN_ID: {"view": run_view(), "jobs": {1: jobs_record(completed_at="2026-10-05T10:31:00Z")}}}
        code, output, _shim = self.select(runs)
        self.assertEqual(code, 0, output)
        build_artifacts(self.artifacts)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_REJECT, output)
        self.assertEqual(self.row(output, "B"), "FAIL")

    def test_the_rejected_runs_real_numbers_fail_a1(self):
        # The rejected 4 KiB-section run: base 4.03 (1.86–13.71) fps and head 5.00 (3.65–7.48), throughput
        # 11.05 and 13.14 MB/s. A1 fails both clauses; throughput passes G1.
        figures = default_figures()
        figures[("macOS", "S3-default", "fps")] = {"base": [1.86, 3.2, 4.03, 6.0, 13.71],
                                                   "head": [3.65, 4.5, 5.0, 6.1, 7.48]}
        figures[("macOS", "S3-default", "throughput")] = {"base": [8.95, 10.5, 11.05, 11.2, 11.47],
                                                          "head": [10.39, 12.0, 13.14, 13.5, 14.12]}
        self.passing(figures=figures)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_REJECT, output)
        self.assertEqual(self.row(output, "A1"), "FAIL")
        self.assertEqual(self.row(output, "G1 macOS"), "PASS")


class EvidenceTests(AcceptanceFixture):
    def assert_invalid(self, expected_text, **build):
        """Build with `build` and expect exit 2 naming `expected_text`."""
        self.passing(**build)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_INVALID, output)
        self.assertIn(expected_text, output)

    def test_a_display_classified_run_is_excluded_even_if_its_outcome_says_valid(self):
        # The raw log's kind is perf-compare's final classification; a run it called `display` is not selected
        # however good its numbers or its outcome.json.
        extra = {("macOS", "S1-S3-S6-S8-S12"): [
            "- S3/default timed head display: `/Users/runner/out/runs/S3-default/timed/11-head/01-harness.log`"]}

        def add_display_run(context, result, outcome):
            if context["directory"].name == "10-head" and context["scenario"] == "S3-default" \
                    and context["set_name"] == "timed" and context["platform"] == "macOS":
                spare = context["directory"].with_name("11-head")
                write_json(spare / "outcome.json", dict(outcome))
                write_json(spare / "scratch" / "result.json", copy.deepcopy(result))
        self.passing(edit=add_display_run, extra_lines=extra)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)

    def test_a_duplicated_raw_log_path_is_invalid(self):
        extra = {("macOS", "S4-S5-S11"): [
            "- S4/default timed head valid: `/Users/runner/out/runs/S4-default/timed/02-head/01-harness.log`"]}
        self.assert_invalid("more than once", extra_lines=extra)

    def test_a_raw_log_line_with_no_attempt_directory_is_invalid(self):
        extra = {("Windows", "S4-S5-S11"): [
            "- S4/default timed head valid: `D:\\a\\out\\runs\\S4-default\\timed\\99-head/01-harness.log`"]}
        self.assert_invalid("no attempt directory", extra_lines=extra)

    def test_windows_separators_map_to_the_attempt_directory(self):
        # Every Windows fixture line uses backslashes, so the passing set already proves the mapping; the
        # selected Windows runs are counted.
        self.passing()
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)
        self.assertIn("Windows S3-default timed head: 5 selected", output)

    def edit_one(self, change, scenario="S3-default", set_name="timed", side="head", platform="macOS"):
        """An edit hook that applies `change(result, outcome)` to the first matching run."""
        def edit(context, result, outcome):
            if (context["platform"], context["scenario"], context["set_name"], context["side"], context["index"]) \
                    == (platform, scenario, set_name, side, 0):
                change(result, outcome)
        return edit

    def test_a_wrong_harness_hash_is_invalid(self):
        self.assert_invalid("harness_hash", edit=self.edit_one(lambda result, _: result.update(harness_hash="ee" * 32)))

    def test_a_result_not_in_short_mode_is_invalid(self):
        self.assert_invalid("short", edit=self.edit_one(lambda result, _: result.update(short=False)))

    def test_two_relevant_phases_are_invalid(self):
        def duplicate(result, _outcome):
            result["phases"].append(copy.deepcopy(result["phases"][1]))
        self.assert_invalid("exactly one flood phase", edit=self.edit_one(duplicate))

    def test_a_zero_duration_phase_is_invalid(self):
        def collapse(result, _outcome):
            result["phases"][1]["end_unix_s"] = result["phases"][1]["start_unix_s"]
        self.assert_invalid("duration", edit=self.edit_one(collapse))

    def test_an_outcome_status_other_than_pass_is_invalid(self):
        self.assert_invalid("status", edit=self.edit_one(lambda _, outcome: outcome.update(status="FAIL")))

    def test_a_nonzero_exit_code_is_invalid(self):
        def nonzero(result, outcome):
            result["exit_code"] = 3
            outcome["exit_code"] = 3
        self.assert_invalid("exit code", edit=self.edit_one(nonzero))

    def test_differing_exit_codes_are_invalid(self):
        self.assert_invalid("exit_code", edit=self.edit_one(lambda _, outcome: outcome.update(exit_code=4)))

    def test_negative_presented_frames_are_invalid(self):
        def negative(result, _outcome):
            result["phases"][1]["presented_frames"] = -5
        self.assert_invalid("presented_frames", edit=self.edit_one(negative))

    def test_an_invariant_violation_is_invalid(self):
        # W - F - L must equal end - start exactly, or the evidence is invalid, not a rejection.
        figures = default_figures()
        figures[("macOS", "counters")] = [dict(wakes=4, frames=3, lost=0, start=0, end=0, yields=4, requests=4)] * 2
        self.assert_invalid("invariant", figures=figures)

    def test_a_missing_handshake_field_in_a_head_counters_run_is_invalid(self):
        def strip(result, _outcome):
            del result["phases"][1]["frame_counters"]["window"]["parser_yield_tokens_end"]
        self.assert_invalid("parser_yield_tokens_end", edit=self.edit_one(strip, set_name="counters"))

    def test_a_base_without_the_new_fields_affects_only_the_report(self):
        # An older base's counters lack every handshake field; that is never a row failure, since A2 and G5
        # read the head only.
        def strip(result, _outcome):
            for phase in result["phases"]:
                for section, name in (("window", "parser_yield_wakes"), ("window", "parser_yield_tokens_start"),
                                      ("window", "parser_yield_tokens_end"), ("vt", "parser_yields")):
                    del phase["frame_counters"][section][name]
        self.passing(edit=lambda context, result, outcome: strip(result, outcome)
                     if context["set_name"] == "counters" and context["side"] == "base" else None)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)

    def test_a_base_counters_run_that_fails_validation_is_only_a_report_note(self):
        # A base counters run feeds no row, so one that fails section 7.3's checks is named in the report
        # and the evaluation proceeds; the same failure in a head counters run would be invalid evidence.
        self.passing(edit=self.edit_one(lambda result, _: result.update(short=False), set_name="counters",
                                        side="base"))
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)
        self.assertIn("base counters run excluded", output)

    def test_a_wrong_head_in_an_artifact_name_is_invalid(self):
        self.passing()
        artifact = next(self.artifacts.iterdir())
        artifact.rename(artifact.with_name(artifact.name.replace(HEAD_SHA, "d" * 40)))
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def rewrite(self, platform, shard, filename, old, new):
        """Replace `old` with `new` in one artifact file of the passing set."""
        self.passing()
        path = self.artifacts / f"perf-comparison-{PULL_REQUEST}-{HEAD_SHA}-{platform}-{shard}-1" / filename
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new), encoding="utf-8")
        return self.evaluate()

    def test_a_wrong_base_is_invalid(self):
        code, output = self.rewrite("macOS", "S4-S5-S11", "comparison.md", BASE_SHA, "e" * 40)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)
        self.assertIn("Base", output)

    def test_a_harness_hash_differing_within_a_platform_is_invalid(self):
        code, output = self.rewrite("Windows", "S9-S10", "comparison.md", HARNESS["Windows"], "ff" * 32)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_wrong_attempt_in_timing_is_invalid(self):
        code, output = self.rewrite("macOS", "S4-S5-S11", "timing.json", '"run_attempt": "1"', '"run_attempt": "2"')
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_wrong_shard_in_timing_is_invalid(self):
        code, output = self.rewrite("Windows", "S4-S5-S11", "timing.json", '"shard": "S4-S5-S11"', '"shard": "S7"')
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_wrong_job_name_in_timing_is_invalid(self):
        code, output = self.rewrite("macOS", "S1-S3-S6-S8-S12", "timing.json", "macOS before/after",
                                    "Windows before/after")
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_missing_selected_run_count_is_invalid(self):
        # Five timed runs per side are required; one fewer selected run is invalid evidence.
        def drop(context, _result, outcome):
            if (context["platform"], context["scenario"], context["set_name"], context["side"], context["index"]) \
                    == ("Windows", "S4-default", "timed", "base", 4):
                outcome["kind"] = "grid"
        self.passing(edit=drop)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_INVALID, output)


class SelectTests(AcceptanceFixture):
    def test_select_reads_only_run_metadata_and_records_the_decision(self):
        # select runs `gh run view` and the run's jobs API, nothing else: it never downloads an artifact, so no
        # measurement can influence selection. It records the run, head, merge base, every required job and B.
        code, output, shim = self.select()
        self.assertEqual(code, 0, output)
        self.assertTrue(all(call[:3] == ["gh", "run", "view"] or call[:2] == ["gh", "api"] for call in shim.calls))
        self.assertFalse(any("download" in part or "artifacts" in part for call in shim.calls for part in call))
        selection = json.loads((self.temp / "selection.json").read_text(encoding="utf-8"))
        self.assertEqual((selection["run_id"], selection["attempt"], selection["head_sha"], selection["merge_base"]),
                         (RUN_ID, 1, HEAD_SHA, BASE_SHA))
        self.assertEqual([job["name"] for job in selection["jobs"]], list(evaluator.REQUIRED_JOBS))
        self.assertEqual(len(selection["jobs"]), 12)
        self.assertEqual(selection["budget"]["elapsed_s"], 28 * 60)
        self.assertFalse(selection["replacement"]["used"])

    def test_an_undeclared_rerun_is_refused(self):
        # A second attempt is the replacement execution; without --replaces it cannot be decisive.
        runs = {RUN_ID: {"view": run_view(attempt=2), "jobs": {2: jobs_record()}}}
        code, output, _shim = self.select(runs)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_listed_infrastructure_failure_allows_one_rerun_of_all_jobs(self):
        failed = {"Windows before/after comparison (S7)": "failure"}
        runs = {RUN_ID: {"view": run_view(attempt=2), "jobs": {1: jobs_record(failed), 2: jobs_record()}}}
        code, output, _shim = self.select(runs, extra=(
            "--replaces", f"{RUN_ID}:1", "--replacement-kind", "infrastructure",
            "--replacement-evidence", "runner lost communication in Windows before/after comparison (S7)"))
        self.assertEqual(code, 0, output)
        selection = json.loads((self.temp / "selection.json").read_text(encoding="utf-8"))
        self.assertTrue(selection["replacement"]["used"])

    def test_an_infrastructure_replacement_needs_a_failed_excluded_attempt(self):
        runs = {RUN_ID: {"view": run_view(attempt=2), "jobs": {1: jobs_record(), 2: jobs_record()}}}
        code, output, _shim = self.select(runs, extra=(
            "--replaces", f"{RUN_ID}:1", "--replacement-kind", "infrastructure", "--replacement-evidence", "none"))
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_queue_budget_replacement_is_a_new_run_on_the_same_head(self):
        # The excluded run succeeded in every job but over budget; the replacement is a later run ID.
        excluded = RUN_ID - 1
        runs = {RUN_ID: {"view": run_view(created_at="2026-10-05T11:00:00Z"),
                         "jobs": {1: jobs_record(completed_at="2026-10-05T11:28:00Z")}},
                excluded: {"view": run_view(run_id=excluded),
                           "jobs": {1: jobs_record(completed_at="2026-10-05T10:41:00Z")}}}
        code, output, _shim = self.select(runs, extra=(
            "--replaces", f"{excluded}:1", "--replacement-kind", "queue-budget",
            "--replacement-evidence", "macOS provisioning queued 14 min on the critical path"))
        self.assertEqual(code, 0, output)

    def test_a_queue_budget_replacement_of_a_run_within_budget_is_refused(self):
        excluded = RUN_ID - 1
        runs = {RUN_ID: {"view": run_view(created_at="2026-10-05T11:00:00Z"),
                         "jobs": {1: jobs_record(completed_at="2026-10-05T11:28:00Z")}},
                excluded: {"view": run_view(run_id=excluded), "jobs": {1: jobs_record()}}}
        code, output, _shim = self.select(runs, extra=(
            "--replaces", f"{excluded}:1", "--replacement-kind", "queue-budget", "--replacement-evidence", "queue"))
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_queue_budget_replacement_needs_every_excluded_job_to_succeed(self):
        excluded = RUN_ID - 1
        runs = {RUN_ID: {"view": run_view(created_at="2026-10-05T11:00:00Z"),
                         "jobs": {1: jobs_record(completed_at="2026-10-05T11:28:00Z")}},
                excluded: {"view": run_view(run_id=excluded),
                           "jobs": {1: jobs_record({evaluator.RESULT_JOB: "failure"},
                                                   completed_at="2026-10-05T10:41:00Z")}}}
        code, output, _shim = self.select(runs, extra=(
            "--replaces", f"{excluded}:1", "--replacement-kind", "queue-budget", "--replacement-evidence", "queue"))
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_run_of_another_head_is_refused(self):
        runs = {RUN_ID: {"view": run_view(head_sha="d" * 40), "jobs": {1: jobs_record()}}}
        code, output, _shim = self.select(runs)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_failed_required_job_is_invalid_at_evaluation(self):
        runs = {RUN_ID: {"view": run_view(conclusions={evaluator.PRODUCER_JOB: "failure"}),
                         "jobs": {1: jobs_record({evaluator.PRODUCER_JOB: "failure"})}}}
        code, output, _shim = self.select(runs)
        self.assertEqual(code, 0, output)
        build_artifacts(self.artifacts)
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_INVALID, output)


class FreezeTests(AcceptanceFixture):
    def frozen_tree(self, change=None):
        """A copy of the two frozen files, optionally edited by `change(text) -> text` on perf-compare."""
        tree = self.temp / "tree"
        (tree / "scripts").mkdir(parents=True)
        for relative in evaluator.FROZEN_FILES:
            shutil.copyfile(ROOT / relative, tree / relative)
        if change is not None:
            path = tree / "scripts" / "perf-compare.py"
            original = path.read_text(encoding="utf-8")
            changed = change(original)
            self.assertNotEqual(changed, original, "the mutation changed nothing")
            path.write_text(changed, encoding="utf-8")
        return tree

    def test_the_frozen_hashes_match_the_current_tree(self):
        # The embedded hashes name these exact bytes; a later change to a frozen file fails here first.
        for relative, digest in evaluator.FROZEN_FILES.items():
            self.assertEqual(evaluator.hashlib.sha256((ROOT / relative).read_bytes()).hexdigest(), digest, relative)

    def test_an_unchanged_copy_is_accepted(self):
        # The control: a byte-identical tree evaluates exactly as the repository does.
        self.passing()
        code, output = self.evaluate(self.frozen_tree())
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)

    def assert_mutation_refused(self, change):
        self.passing()
        code, output = self.evaluate(self.frozen_tree(change))
        self.assertEqual(code, evaluator.EXIT_INVALID, output)
        self.assertIn("scripts/perf-compare.py", output)

    def test_a_changed_constant_is_refused(self):
        self.assert_mutation_refused(lambda text: text.replace("LATENCY_MIN_PERCENT = 80", "LATENCY_MIN_PERCENT = 79"))

    def test_a_changed_helper_is_refused(self):
        self.assert_mutation_refused(lambda text: text.replace(
            "    return nearest_rank(values, 95)", "    return nearest_rank(values, 94)"))

    def test_a_changed_class_declaration_is_refused(self):
        self.assert_mutation_refused(lambda text: text.replace(
            '    """The median and min-max of one value per valid run."""\n\n    median: float\n',
            '    """The median and min-max of one value per valid run."""\n\n    median: float\n'
            '    trimmed: float = 0.0\n'))

    def test_a_rebound_alias_is_refused(self):
        self.assert_mutation_refused(lambda text: text + "\nrun_summary = frame_summary\n")

    def test_a_missing_frozen_file_is_refused(self):
        self.passing()
        tree = self.frozen_tree()
        (tree / "scripts" / "perf-critical-path.py").unlink()
        code, output = self.evaluate(tree)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)
        self.assertIn("scripts/perf-critical-path.py", output)

    def test_only_the_verified_copy_is_imported(self):
        # The module the evaluator uses is the private copy, never the file it read from the tree.
        workdir = self.temp / "work"
        workdir.mkdir()
        modules = evaluator.load_frozen(ROOT, workdir)
        for module in modules.values():
            self.assertEqual(Path(module.__file__).parent, workdir)

    def test_the_evaluator_never_calls_load_gate(self):
        # perf-compare's load_gate would import local-gate.py, which is not frozen; it is patched to raise and
        # the evaluation still completes.
        original = evaluator.load_frozen

        def load_without_gate(tree, workdir):
            modules = original(tree, workdir)

            def refuse():
                raise AssertionError("load_gate was called")
            modules["scripts/perf-compare.py"].load_gate = refuse
            return modules
        self.passing()
        evaluator.load_frozen = load_without_gate
        try:
            code, output = self.evaluate()
        finally:
            evaluator.load_frozen = original
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)

    def test_the_tests_run_in_the_workflow_supply_chain_step(self):
        # The evaluator's tests run wherever the supply-chain step runs, so the local-gate table is unchanged.
        script = (SCRIPTS / "check-workflow-supply-chain.sh").read_text(encoding="utf-8")
        self.assertIn('"$PY" perf-1584-acceptance_tests.py', script)
        self.assertIn("perf-1584-acceptance.py", script)


if __name__ == "__main__":
    unittest.main(verbosity=2)
