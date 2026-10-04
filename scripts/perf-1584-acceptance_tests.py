#!/usr/bin/env python3
"""Tests for the frozen acceptance evaluator: run selection from metadata, artifact and run identity,
per-result validation, every row of the rule, and the whole-file freeze of the statistics it imports."""

from __future__ import annotations

import atexit
import contextlib
import copy
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
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

# Fixture commits use a fixed identity and keep their bytes, whatever the user's git configuration says.
GIT_ENV = dict(os.environ, GIT_AUTHOR_NAME="fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
               GIT_COMMITTER_NAME="fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid")


def git(directory, *arguments):
    """Run git in `directory` and return its output; any failure fails the test run."""
    return subprocess.run(["git", "-C", str(directory), "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false",
                           *arguments], env=GIT_ENV, capture_output=True, text=True, check=True, timeout=60).stdout


def fixture_repository():
    """A git repository whose one commit holds both frozen files byte for byte; returns (path, commit)."""
    repository = Path(tempfile.mkdtemp(prefix="perf-1584-fixture-repo-"))
    atexit.register(shutil.rmtree, repository, ignore_errors=True)
    git(repository, "init", "-q")
    for relative in evaluator.FROZEN_FILES:
        (repository / relative).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / relative, repository / relative)
    git(repository, "add", "-A")
    git(repository, "commit", "-q", "-m", "frozen files")
    return repository, git(repository, "rev-parse", "HEAD").strip()


# Every fixture's head is a real commit of this repository: the evaluator reads the frozen files from the
# head's commit, never from a working tree.
FIXTURE_TREE, HEAD_SHA = fixture_repository()
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


def build_artifacts(root, figures=None, edit=None, attempt=1, extra_lines=None, head=None):
    """Write every comparison artifact the rule reads; `edit(context, result, outcome)` may change a run
    before it is written. Returns {(platform, shard): artifact path}."""
    figures = figures or default_figures()
    head = head or HEAD_SHA
    artifacts = {}
    for platform, shards in LAYOUT.items():
        for shard, scenarios in shards.items():
            artifact = root / f"perf-comparison-{PULL_REQUEST}-{head}-{platform}-{shard}-{attempt}"
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
            document = "\n".join([f"- Base: `origin/main` = `{BASE_SHA}`", f"- Head: `HEAD` = `{head}`",
                                  f"- Harness hash (both trees): `{HARNESS[platform]}`", "", "Raw logs:", "",
                                  *lines, ""])
            (artifact / "comparison.md").write_text(document, encoding="utf-8")
    return artifacts


PRODUCER = evaluator.PRODUCER_JOB
WINDOWS_S7 = "Windows before/after comparison (S7)"
WINDOWS_S9 = "Windows before/after comparison (S9-S10)"
MACOS_COMPARISONS = [name for name in evaluator.REQUIRED_JOBS if name.startswith("macOS before/after")]
WINDOWS_COMPARISONS = [name for name in evaluator.REQUIRED_JOBS if name.startswith("Windows before/after")]
# Cause evidence bound to a failed job: a remote fetch that failed, an `actions/*` transfer's HTTP 5xx, and the
# runner annotation GitHub writes when it loses or cannot provision a runner.
FETCH_EVIDENCE = {"kind": "fetch", "url": "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe",
                  "error": "HTTP 503 Service Unavailable"}
TRANSFER_EVIDENCE = {"kind": "http", "status": 503, "source": "actions/cache/restore"}
RUNNER_EVIDENCE = {"kind": "runner", "annotation": "The hosted runner lost communication with the server."}
PULL_URL = f"https://github.com/{evaluator.DEFAULT_REPOSITORY}/pull/{PULL_REQUEST}"


def job_id(attempt, name):
    """The fixture's job ID for `name` in `attempt`; each attempt's jobs have their own IDs."""
    return attempt * 1000 + evaluator.REQUIRED_JOBS.index(name)


def jobs_record(conclusions=None, completed_at="2026-10-05T10:28:00Z", attempt=1, run_attempts=None,
                failed_steps=None, timing=None, started_at="2026-10-05T10:01:00Z", created_at="2026-10-05T10:00:05Z"):
    """One attempt's jobs API page: every required job completed, `success` unless overridden, with its ID,
    the attempt that ran it, its queue and run times, and its steps (`failed_steps` names a failed one)."""
    conclusions, run_attempts = conclusions or {}, run_attempts or {}
    failed_steps, timing = failed_steps or {}, timing or {}
    jobs = []
    for name in evaluator.REQUIRED_JOBS:
        steps = [{"name": "Set up job", "conclusion": "success"}]
        if name in failed_steps:
            steps.append({"name": failed_steps[name], "conclusion": "failure"})
        job = {"id": job_id(attempt, name), "run_attempt": run_attempts.get(name, attempt), "name": name,
               "status": "completed", "conclusion": conclusions.get(name, "success"),
               "created_at": created_at, "started_at": started_at, "completed_at": completed_at,
               "steps": steps}
        job.update(timing.get(name, {}))
        jobs.append(job)
    return jobs


def record_body(first=(RUN_ID, 1), exclusion=None, recorded_at="2026-10-05T10:30:00Z", head=None,
                published_at=None, published=True):
    """The operator's record: the first eligible run, the exclusion or null, and where and when the record was
    published (at `recorded_at` unless `published_at` says otherwise; none when `published` is false)."""
    publication = {"url": f"{PULL_URL}#issuecomment-1", "published_at": published_at or recorded_at}
    return {"schema_version": 1, "head_sha": head or HEAD_SHA, "merge_base": BASE_SHA,
            "first_eligible_run": {"run_id": first[0], "attempt": first[1]},
            "recorded_at": recorded_at, "publication": publication if published else None, "exclusion": exclusion}


def infra_entry(name, step, evidence=None):
    """One listed infrastructure failure of attempt 1: the job, the step that failed, and its cause evidence."""
    return {"name": name, "id": job_id(1, name), "failed_step": step, "evidence": evidence or FETCH_EVIDENCE}


def stamp(offset_s):
    """The fixture run's creation time plus `offset_s` seconds, as GitHub writes it."""
    created = datetime(2026, 10, 5, 10, 0, 0, tzinfo=timezone.utc)
    return (created + timedelta(seconds=offset_s)).strftime("%Y-%m-%dT%H:%M:%SZ")


def timeline(producer_queue_s=900, producer_run_s=600, mac_run_s=900, windows_end_s=1500, mac_created_s=None):
    """Every required job's times on a run created at CREATED_AT, and the run's B. The producer queues for
    `producer_queue_s`; the macOS comparisons, created when it finishes (or at `mac_created_s`), start 10 s
    later; the Windows comparisons run on their own until `windows_end_s`; the result job ends 10 s after the
    last of them."""
    producer_end_s = 5 + producer_queue_s + producer_run_s
    times = {PRODUCER: (5, 5 + producer_queue_s, producer_end_s)}
    for name in MACOS_COMPARISONS:
        created_s = producer_end_s if mac_created_s is None else mac_created_s
        times[name] = (created_s, producer_end_s + 10, producer_end_s + 10 + mac_run_s)
    for name in WINDOWS_COMPARISONS:
        times[name] = (5, 15, windows_end_s)
    ready_s = max(finished_s for _created_s, _started_s, finished_s in times.values())
    times[evaluator.RESULT_JOB] = (ready_s, ready_s + 5, ready_s + 10)
    timing = {name: {"created_at": stamp(created_s), "started_at": stamp(started_s), "completed_at": stamp(finished_s)}
              for name, (created_s, started_s, finished_s) in times.items()}
    return timing, ready_s + 10


def ineligible_jobs():
    """An ineligible run's jobs: every one skipped, the result job under its unevaluated name expression."""
    jobs = jobs_record({name: "skipped" for name in evaluator.REQUIRED_JOBS})
    jobs[-1]["name"] = ("${{ (github.event_name == 'push' || ...) && 'Performance comparison result' || "
                        "'Performance comparison result (not run)' }}")
    return jobs


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
        if argv[:2] == ["gh", "api"] and len(argv) == 3 and "/actions/runs?head_sha=" in argv[2]:
            listed = self.runs.get("head_runs", [])
            return json.dumps({"total_count": len(listed), "workflow_runs": listed}).encode("utf-8")
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

    def select(self, runs=None, extra=(), run_id=RUN_ID, record=None, head=None, tree=None):
        """Write the operator's record and selection.json through `select` and the shim; return (exit code,
        output, shim)."""
        runs = runs or {RUN_ID: {"view": run_view(), "jobs": {1: jobs_record()}}}
        record_path = self.temp / "record.json"
        write_json(record_path, record or record_body())
        shim = GhShim(runs)
        code, output = call_main(["select", "--run", str(run_id), "--head", head or HEAD_SHA, "--merge-base",
                                  BASE_SHA, "--output", str(self.temp / "selection.json"), "--record",
                                  str(record_path), "--tree", str(tree or FIXTURE_TREE), *extra], runner=shim)
        return code, output, shim

    def evaluate(self, tree=None):
        """Evaluate the fixture's selection and artifacts; return (exit code, output)."""
        return call_main(["evaluate", "--selection", str(self.temp / "selection.json"),
                          "--artifacts", str(self.artifacts), "--tree", str(tree or FIXTURE_TREE)])

    def row_verdict(self, name, figures=None, edit=None, completed_at=None):
        """A fresh selection and artifact set, evaluated: (exit code, row `name`'s verdict, output)."""
        self.artifacts = Path(tempfile.mkdtemp(prefix="artifacts-", dir=self.temp))
        runs = None
        if completed_at is not None:
            runs = {RUN_ID: {"view": run_view(), "jobs": {1: jobs_record(completed_at=completed_at)}}}
        code, output, _shim = self.select(runs)
        self.assertEqual(code, 0, output)
        build_artifacts(self.artifacts, figures=figures, edit=edit)
        code, output = self.evaluate()
        return code, self.row(output, name), output

    def assert_inclusive(self, name, at, outside):
        """`at` sits exactly on row `name`'s threshold and passes; `outside` is just past it and fails."""
        for label, build, expected in (("at", at, "PASS"), ("outside", outside, "FAIL")):
            with self.subTest(boundary=label):
                _code, verdict, output = self.row_verdict(name, **build)
                self.assertEqual(verdict, expected, output)

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


    def test_every_inclusive_threshold_passes_at_equality_and_fails_just_outside(self):
        # Each inclusive bound compares exact rationals built from the results' integers and decimals, so a
        # figure exactly on the bound passes and one just past it fails; no float rounding decides a row.
        def figures(changes):
            changed = default_figures()
            changed.update(changes)
            return {"figures": changed}

        def counters(*runs):
            return figures({("macOS", "counters"): [dict(wakes=wakes, frames=frames, lost=lost, start=0, end=0,
                                                         yields=wakes, requests=wakes)
                                                    for wakes, frames, lost in runs]})

        def flat(values, unattributed=0):
            return [latency_run(values, unattributed) for _ in range(2)]

        g1_base = {"base": [9.0, 9.0, 9.0, 8.0, 9.5]}
        cases = {
            "A1": (figures({("macOS", "S3-default", "fps"): {"base": [4.0, 4.1, 3.9, 4.2, 4.0], "head": [8.0] * 5}}),
                   figures({("macOS", "S3-default", "fps"): {"base": [4.0, 4.1, 3.9, 4.2, 4.0], "head": [7.99] * 5}})),
            "A2": (counters((100, 80, 20), (100, 80, 20)), counters((100, 80, 20), (100, 79, 21))),
            "G1 macOS": (figures({("macOS", "S3-default", "throughput"): {**g1_base, "head": [8.1] * 5}}),
                         figures({("macOS", "S3-default", "throughput"): {**g1_base, "head": [8.0999999] * 5}})),
            "G1 Windows": (figures({("Windows", "S3-default", "throughput"): {"base": [9.0, 9.1, 8.9, 9.2, 9.0],
                                                                               "head": [8.9] * 5}}),
                           figures({("Windows", "S3-default", "throughput"): {"base": [9.0, 9.1, 8.9, 9.2, 9.0],
                                                                               "head": [8.8999999] * 5}})),
            "G2": (figures({("Windows", "S3-default", "fps"): {"base": [20.0, 21.0, 19.0, 22.0, 20.0],
                                                               "head": [19.0] * 5}}),
                   figures({("Windows", "S3-default", "fps"): {"base": [20.0, 21.0, 19.0, 22.0, 20.0],
                                                               "head": [18.99] * 5}})),
            "G3 macOS": (figures({("macOS", "S2-flood", "latency"): {"base": flat([1.0] * 100),
                                                                    "head": flat([1.1] * 100)}}),
                         figures({("macOS", "S2-flood", "latency"): {"base": flat([1.0] * 100),
                                                                    "head": flat([1.1000001] * 100)}})),
            "G3 Windows": (figures({("Windows", "S2-flood", "latency"): {
                               "base": flat([10.0 + step for step in range(80)], 20),
                               "head": flat([10.0 + step for step in range(80)], 20)}}),
                           figures({("Windows", "S2-flood", "latency"): {
                               "base": flat([10.0 + step for step in range(80)], 21),
                               "head": flat([10.0 + step for step in range(80)], 20)}})),
            "G4 macOS": (figures({}), figures({("macOS", "S4-default", "fps"): {"base": [30.0] * 5,
                                                                              "head": [29.99] * 5}})),
            "B": ({"completed_at": "2026-10-05T10:30:00Z"}, {"completed_at": "2026-10-05T10:30:01Z"}),
        }
        for name, (at, outside) in cases.items():
            with self.subTest(row=name):
                self.assert_inclusive(name, at, outside)

    def test_a1_minimum_clause_is_strict(self):
        # A1's minimum clause is the one strict bound: a head minimum equal to the base median fails.
        changed = default_figures()
        changed[("macOS", "S3-default", "fps")] = {"base": [4.0, 4.1, 3.9, 4.2, 4.0], "head": [9.0] * 4 + [4.0]}
        _code, verdict, output = self.row_verdict("A1", figures=changed)
        self.assertEqual(verdict, "FAIL", output)

    def test_g3_fails_when_a_side_counts_no_samples(self):
        # A zero total on either side, on either platform, has no coverage, so G3 fails there: a zero
        # denominator never satisfies the 80% and 10-point bounds.
        for platform in ("macOS", "Windows"):
            for side in ("base", "head"):
                def edit(context, result, _outcome, platform=platform, side=side):
                    if (context["platform"], context["scenario"], context["side"]) == (platform, "S2-flood", side):
                        result["latency"]["attributed"] = 0
                        result["latency"]["total"] = 0
                with self.subTest(platform=platform, side=side):
                    code, verdict, output = self.row_verdict(f"G3 {platform}", edit=edit)
                    self.assertEqual((code, verdict), (evaluator.EXIT_REJECT, "FAIL"), output)

    def test_a2_an_open_token_is_not_a_frame(self):
        # Runs (W2 F1 L0, start 0 end 1) and (W3 F2 L1, start 0 end 0) resolve 3 frames of 4, below 4/5; a
        # token still open at the phase's end is never credited as a frame, which would read 4 of 5.
        changed = default_figures()
        changed[("macOS", "counters")] = [dict(wakes=2, frames=1, lost=0, start=0, end=1, yields=2, requests=2),
                                         dict(wakes=3, frames=2, lost=1, start=0, end=0, yields=3, requests=3)]
        code, verdict, output = self.row_verdict("A2", figures=changed)
        self.assertEqual((code, verdict), (evaluator.EXIT_REJECT, "FAIL"), output)


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


    def test_a_non_finite_latency_sample_is_invalid(self):
        # Every attributed sample must be finite, whether the bad one would be the selected p95 or not.
        def away(context, result, _outcome):
            if (context["platform"], context["scenario"], context["side"], context["index"]) \
                    == ("macOS", "S2-flood", "head", 0):
                result["latency"]["samples"][0]["latency_ms"] = float("nan")

        def at_p95(context, result, _outcome):
            if (context["platform"], context["scenario"], context["side"]) == ("macOS", "S2-flood", "head"):
                for sample in result["latency"]["samples"]:
                    if sample["latency_ms"] is not None and sample["latency_ms"] >= 100.0:
                        sample["latency_ms"] = float("inf")
        for label, edit in (("away from the p95", away), ("at the p95", at_p95)):
            with self.subTest(sample=label):
                code, output = self.invalid_with(edit)
                self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def invalid_with(self, edit=None, extra_lines=None):
        """A fresh selection and artifact set built with `edit` or `extra_lines`, evaluated."""
        self.artifacts = Path(tempfile.mkdtemp(prefix="artifacts-", dir=self.temp))
        code, output, _shim = self.select()
        self.assertEqual(code, 0, output)
        build_artifacts(self.artifacts, edit=edit, extra_lines=extra_lines)
        return self.evaluate()

    def counters_result(self, folder):
        """The macOS S3/default counters run `folder`'s result.json."""
        artifact = self.artifacts / f"perf-comparison-{PULL_REQUEST}-{HEAD_SHA}-macOS-S1-S3-S6-S8-S12-1"
        return artifact / "runs" / "S3-default" / "counters" / folder / "scratch" / "result.json"

    def test_a_non_object_base_counters_result_is_only_a_note(self):
        # A base counters run feeds only the report, so a result.json that is not an object is a note and
        # every row is unchanged.
        self.passing()
        self.counters_result("01-base").write_text("[]", encoding="utf-8")
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)
        self.assertIn("base counters run excluded", output)

    def test_a_non_object_head_counters_result_is_invalid(self):
        # The same shape in a head counters run is evidence the rule reads, so it is invalid.
        self.passing()
        self.counters_result("02-head").write_text("[]", encoding="utf-8")
        code, output = self.evaluate()
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_duplicated_path_on_an_unselected_line_is_invalid(self):
        # A raw-log path listed twice is invalid even when the second line is a kind the rule never selects.
        line = "- S3/default timed head display: `/Users/runner/out/runs/S3-default/timed/02-head/01-harness.log`"
        code, output = self.invalid_with(extra_lines={("macOS", "S1-S3-S6-S8-S12"): [line]})
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_missing_directory_on_an_unselected_line_is_invalid(self):
        # Every raw-log line needs its attempt directory, even one of a kind the rule never selects.
        line = "- S3/default timed head display: `/Users/runner/out/runs/S3-default/timed/99-head/01-harness.log`"
        code, output = self.invalid_with(extra_lines={("macOS", "S1-S3-S6-S8-S12"): [line]})
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_negative_duration_phase_is_invalid(self):
        # A phase that ends before it starts is invalid evidence; read as a figure it would leave the
        # median untouched and the rule would accept.
        def edit(context, result, _outcome):
            if (context["platform"], context["scenario"], context["set_name"], context["side"], context["index"]) \
                    == ("macOS", "S3-default", "timed", "base", 0):
                phase = next(phase for phase in result["phases"] if phase["name"] == "flood")
                phase["end_unix_s"] = phase["start_unix_s"] - PHASE_WALL_S
        code, output = self.invalid_with(edit)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_the_frozen_and_evaluator_files_check_out_with_lf(self):
        # A CRLF checkout would change the frozen bytes, so these files always check out with LF.
        names = ["scripts/perf-compare.py", "scripts/perf-critical-path.py", "scripts/perf-1584-acceptance.py",
                 "scripts/perf-1584-acceptance_tests.py"]
        output = git(ROOT, "check-attr", "eol", "--", *names)
        for name in names:
            self.assertIn(f"{name}: eol: lf", output)


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
        # A second attempt is the replacement execution; with no recorded exclusion it cannot be decisive.
        runs = {RUN_ID: {"view": run_view(attempt=2), "jobs": {2: jobs_record(attempt=2)}}}
        code, output, _shim = self.select(runs)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_selection_is_bound_to_the_recorded_first_eligible_run(self):
        # Without an exclusion the decisive run is the operator's recorded first eligible run, and the record
        # must name this head and merge base.
        code, output, _shim = self.select(record=record_body(first=(RUN_ID - 1, 1)))
        self.assertEqual(code, evaluator.EXIT_INVALID, output)
        code, output, _shim = self.select(record=record_body(head="e" * 40))
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def infra_runs(self, conclusions=None, failed_steps=None, run_attempts=None,
                   rerun_created="2026-10-05T10:34:00Z"):
        """The first eligible run: attempt 1 failed S7 at `Install Rust`, and attempt 2, its jobs created at
        `rerun_created`, re-ran all jobs."""
        conclusions = {WINDOWS_S7: "failure"} if conclusions is None else conclusions
        failed_steps = {WINDOWS_S7: "Install Rust"} if failed_steps is None else failed_steps
        return {RUN_ID: {"view": run_view(attempt=2), "jobs": {
            1: jobs_record(conclusions, failed_steps=failed_steps),
            2: jobs_record(attempt=2, run_attempts=run_attempts, created_at=rerun_created,
                           started_at="2026-10-05T10:35:00Z", completed_at="2026-10-05T10:58:00Z")}}}

    def infra_record(self, category="toolchain-fetch", jobs=None, recorded_at="2026-10-05T10:30:00Z", **publication):
        """The record of an infrastructure exclusion of the first execution."""
        jobs = jobs if jobs is not None else [infra_entry(WINDOWS_S7, "Install Rust")]
        return record_body(recorded_at=recorded_at, exclusion={"run_id": RUN_ID, "attempt": 1,
                                                               "category": category, "jobs": jobs}, **publication)

    def test_a_listed_infrastructure_failure_allows_one_rerun_of_all_jobs(self):
        # A toolchain fetch that failed with evidence, recorded and published before the rerun's jobs exist,
        # admits attempt 2 of the same run, which re-ran every required job.
        code, output, _shim = self.select(self.infra_runs(), record=self.infra_record())
        self.assertEqual(code, 0, output)
        selection = json.loads((self.temp / "selection.json").read_text(encoding="utf-8"))
        self.assertTrue(selection["replacement"]["used"])
        self.assertEqual(selection["replacement"]["category"], "toolchain-fetch")

    def test_a_lost_runner_may_fail_in_or_outside_a_step(self):
        # A runner that lost communication fails its job between steps or in the middle of one, the comparison
        # step included; the annotation, not the step, is the cause evidence.
        for label, step in (("between steps", None), ("mid-step", "Compare the base and the head")):
            with self.subTest(failed=label):
                runs = self.infra_runs(failed_steps={} if step is None else {WINDOWS_S7: step})
                record = self.infra_record("runner-lost", [infra_entry(WINDOWS_S7, step, RUNNER_EVIDENCE)])
                code, output, _shim = self.select(runs, record=record)
                self.assertEqual(code, 0, output)

    def test_a_failed_producer_explains_its_skipped_comparisons(self):
        # When the producer fails on a listed cause, the macOS comparisons it feeds are skipped and the result
        # job fails at its own check; both follow from the listed failure.
        conclusions = {PRODUCER: "failure", evaluator.RESULT_JOB: "failure",
                       **{name: "skipped" for name in MACOS_COMPARISONS}}
        failed_steps = {PRODUCER: "Install Rust", evaluator.RESULT_JOB: "Require every comparison job to succeed"}
        record = self.infra_record(jobs=[infra_entry(PRODUCER, "Install Rust")])
        code, output, _shim = self.select(self.infra_runs(conclusions, failed_steps), record=record)
        self.assertEqual(code, 0, output)

    def test_unlisted_causes_never_qualify_as_infrastructure(self):
        # A build, test or comparison failure, a cancellation, a skip, an unknown category, an unlisted failed
        # job, a misreported step or another job's ID never admits a replacement.
        skipped = [infra_entry(WINDOWS_S7, None, RUNNER_EVIDENCE)]
        cases = {
            "comparison step": (self.infra_runs(failed_steps={WINDOWS_S7: "Compare the base and the head"}),
                                self.infra_record(jobs=[infra_entry(WINDOWS_S7, "Compare the base and the head")])),
            "cancelled": (self.infra_runs(conclusions={WINDOWS_S7: "cancelled"}, failed_steps={}),
                          self.infra_record("runner-lost", skipped)),
            "skipped": (self.infra_runs(conclusions={WINDOWS_S7: "skipped"}, failed_steps={}),
                        self.infra_record("runner-lost", skipped)),
            "unknown category": (self.infra_runs(), self.infra_record("flaky")),
            "unlisted failure": (self.infra_runs(
                conclusions={WINDOWS_S7: "failure", WINDOWS_S9: "failure"},
                failed_steps={WINDOWS_S7: "Install Rust", WINDOWS_S9: "Compare the base and the head"}),
                self.infra_record()),
            "misreported step": (self.infra_runs(failed_steps={WINDOWS_S7: "Compare the base and the head"}),
                                 self.infra_record()),
            "wrong job ID": (self.infra_runs(), self.infra_record(jobs=[
                {**infra_entry(WINDOWS_S7, "Install Rust"), "id": job_id(1, WINDOWS_S9)}])),
        }
        for label, (runs, record) in cases.items():
            with self.subTest(case=label):
                code, output, _shim = self.select(runs, record=record)
                self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_cause_evidence_must_prove_the_listed_category(self):
        # The step map is only a consistency check: each listed job carries evidence of its category's cause. A
        # local setup error, a non-5xx or non-`actions/*` transfer, a fetch with no URL, an unannotated runner
        # loss and the local `git rev-parse` of the vcpkg commit never pass as infrastructure.
        def case(category, step, evidence):
            return (self.infra_runs(failed_steps={WINDOWS_S7: step}),
                    self.infra_record(category, [infra_entry(WINDOWS_S7, step, evidence)]))
        cases = {
            "local setup error": case("toolchain-fetch", "Install Cairo for Windows", {
                "kind": "local", "url": "https://github.com/microsoft/vcpkg", "error": "cairo-2.dll was not found"}),
            "non-5xx transfer": case("actions-transfer", "Restore vcpkg binaries (Cairo)",
                                     {**TRANSFER_EVIDENCE, "status": 404}),
            "not an actions transfer": case("actions-transfer", "Upload the comparison evidence",
                                            {**TRANSFER_EVIDENCE, "source": "curl"}),
            "fetch with no URL": case("toolchain-fetch", "Install Rust", {"kind": "fetch", "error": "timed out"}),
            "unannotated runner loss": case("runner-lost", "Install Rust", {"kind": "runner", "annotation": ""}),
            "local vcpkg commit resolution": case("toolchain-fetch", "Resolve vcpkg commit", FETCH_EVIDENCE),
        }
        for label, (runs, record) in cases.items():
            with self.subTest(case=label):
                code, output, _shim = self.select(runs, record=record)
                self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_cause_text_must_be_a_non_blank_string(self):
        # A runner-loss annotation and a fetch error are each a string that is non-blank once stripped: null, true,
        # false, an object, a list, an empty or a blank string never pass as one, and real text does.
        not_text = (("null", None), ("true", True), ("false", False), ("object", {}), ("list", []), ("empty", ""),
                    ("blank", "   "))
        fields = {
            "annotation": ("runner-lost", None, lambda value: {"kind": "runner", "annotation": value}, RUNNER_EVIDENCE),
            "error": ("toolchain-fetch", "Install Rust", lambda value: {**FETCH_EVIDENCE, "error": value},
                      FETCH_EVIDENCE),
        }
        for field, (category, step, evidence, real) in fields.items():
            runs = self.infra_runs(failed_steps={} if step is None else {WINDOWS_S7: step})
            for label, value in not_text:
                with self.subTest(field=field, value=label):
                    record = self.infra_record(category, [infra_entry(WINDOWS_S7, step, evidence(value))])
                    code, output, _shim = self.select(runs, record=record)
                    self.assertEqual(code, evaluator.EXIT_INVALID, output)
            with self.subTest(field=field, value="real text"):
                code, output, _shim = self.select(runs, record=self.infra_record(
                    category, [infra_entry(WINDOWS_S7, step, real)]))
                self.assertEqual(code, 0, output)

    def test_select_marks_operator_attested_evidence(self):
        # Cause evidence, the label trigger and the publication are the operator's word: select checks only their
        # shape, so selection.json and its printed summary label them operator-attested. The operator verifies the
        # linked records before triggering the replacement and before reading measurements; a successful select is
        # not authenticated proof.
        for label, runs, record, fields in (
                ("infrastructure", self.infra_runs(), self.infra_record(), ["exclusion.jobs[].evidence", "publication"]),
                ("queue", self.queue_runs(), self.queue_record(), ["exclusion.replacement_trigger", "publication"])):
            with self.subTest(exclusion=label):
                code, output, _shim = self.select(runs, record=record)
                self.assertEqual(code, 0, output)
                selection = json.loads((self.temp / "selection.json").read_text(encoding="utf-8"))
                attested = selection["replacement"].get("operator_attested") or {}
                self.assertEqual(attested.get("fields"), fields)
                status = str(attested.get("status"))
                for phrase in ("before triggering the replacement", "before reading measurements",
                               "not authenticated proof"):
                    self.assertIn(phrase, status)
                # The summary gives the label its own line, naming the fields, not only inside the replacement dict.
                self.assertIn(f"operator-attested {fields}: ", output)

    def test_an_infrastructure_replacement_is_recorded_before_a_full_rerun(self):
        # The exclusion is recorded after the failed attempt and before the rerun's first job is created, and
        # Re-run all jobs leaves no required job inherited from attempt 1.
        cases = {
            "inherited job": (self.infra_runs(run_attempts={WINDOWS_S9: 1}), self.infra_record()),
            "jobs created before the record": (self.infra_runs(rerun_created="2026-10-05T10:29:00Z"),
                                               self.infra_record()),
            "recorded after the rerun began": (self.infra_runs(), self.infra_record(recorded_at="2026-10-05T10:40:00Z")),
            "recorded before the failure finished": (self.infra_runs(),
                                                     self.infra_record(recorded_at="2026-10-05T10:20:00Z")),
            "another run excluded": (self.infra_runs(), record_body(exclusion={
                "run_id": RUN_ID - 1, "attempt": 1, "category": "toolchain-fetch", "jobs": []})),
        }
        for label, (runs, record) in cases.items():
            with self.subTest(case=label):
                code, output, _shim = self.select(runs, record=record)
                self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_the_exclusion_is_published_before_the_trigger(self):
        # The operator's record carries where it was published and when: no earlier than it was written and
        # before the replacement was triggered, for either kind of exclusion.
        cases = {
            "no publication": (self.infra_runs(), self.infra_record(published=False)),
            "published after the rerun's jobs were created": (
                self.infra_runs(), self.infra_record(published_at="2026-10-05T10:34:30Z")),
            "published before it was recorded": (self.infra_runs(),
                                                 self.infra_record(published_at="2026-10-05T10:29:00Z")),
            "queue record published at the replacement": (
                self.queue_runs(), self.queue_record(published_at="2026-10-05T11:00:00Z")),
        }
        for label, (runs, record) in cases.items():
            with self.subTest(case=label):
                code, output, _shim = self.select(runs, record=record)
                self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def queue_runs(self, predecessor_attempt=1, replacement_created="2026-10-05T11:00:00Z", timing=None,
                   extra_runs=(), conclusions=None):
        """The first eligible run over budget (the producer queued 900 s, B 2425 s), then the replacement; each
        `extra_runs` entry is a (listed run, its jobs) created between them."""
        excluded = RUN_ID - 1
        listed = [{"id": excluded, "name": evaluator.WORKFLOW_NAME, "event": "pull_request",
                   "head_sha": HEAD_SHA, "created_at": CREATED_AT},
                  *(listed_run for listed_run, _jobs in extra_runs),
                  {"id": RUN_ID, "name": evaluator.WORKFLOW_NAME, "event": "pull_request", "head_sha": HEAD_SHA,
                   "created_at": replacement_created}]
        runs = {RUN_ID: {"view": run_view(created_at=replacement_created),
                         "jobs": {1: jobs_record(completed_at="2026-10-05T11:28:00Z")}},
                excluded: {"view": run_view(run_id=excluded, attempt=predecessor_attempt),
                           "jobs": {1: jobs_record(conclusions, timing=timing or timeline()[0])}},
                "head_runs": listed}
        for listed_run, jobs in extra_runs:
            runs[listed_run["id"]] = {"jobs": {1: jobs}}
        return runs

    def queue_record(self, intervals=None, counterfactual_s=1525, elapsed_s=2425,
                     recorded_at="2026-10-05T10:45:00Z", trigger=None, **publication):
        """The record of a queue exclusion: the producer's 900 s runner queue, which B through the dependency graph
        loses (2425 s becomes 1525 s), and the label event that triggers the replacement."""
        intervals = intervals if intervals is not None else [self.interval(PRODUCER, 5, 905)]
        trigger = trigger if trigger is not None else {"action": "labeled", "label": "perf", "url": PULL_URL}
        return record_body(first=(RUN_ID - 1, 1), recorded_at=recorded_at, exclusion={
            "run_id": RUN_ID - 1, "attempt": 1, "category": "queue", "elapsed_s": elapsed_s,
            "counterfactual_s": counterfactual_s, "intervals": intervals, "replacement_trigger": trigger},
            **publication)

    @staticmethod
    def interval(name, start_s, end_s):
        """A claimed runner-queue interval of `name` in the excluded run, in seconds after its creation."""
        return {"name": name, "id": job_id(1, name), "start": stamp(start_s), "end": stamp(end_s)}

    def test_a_queue_budget_replacement_is_the_first_new_run_on_the_same_head(self):
        # Every excluded job succeeded but B failed by 625 s; without the producer's 900 s runner queue the
        # dependency graph finishes at 1525 s <= 1800 s, so the first later eligible run on the head replaces it.
        code, output, _shim = self.select(self.queue_runs(), record=self.queue_record())
        self.assertEqual(code, 0, output)
        selection = json.loads((self.temp / "selection.json").read_text(encoding="utf-8"))
        self.assertEqual(selection["replacement"]["counterfactual_s"], 1525)

    def test_a_queue_exclusion_must_reconcile_with_the_job_record(self):
        # The intervals are macOS required jobs' own runner-queue windows, disjoint within a job, and the recorded
        # B and counterfactual equal the recomputed ones.
        windows_timing = {**timeline()[0], WINDOWS_S7: {"created_at": stamp(5), "started_at": stamp(905),
                                                         "completed_at": stamp(1500)}}
        cases = {
            "too short to recover B": (self.queue_runs(), self.queue_record([self.interval(PRODUCER, 5, 305)], 2125)),
            "wrong counterfactual": (self.queue_runs(), self.queue_record(counterfactual_s=1500)),
            "wrong elapsed": (self.queue_runs(), self.queue_record(elapsed_s=2400)),
            "outside the queue window": (self.queue_runs(),
                                         self.queue_record([self.interval(PRODUCER, 5, 1205)], 1225)),
            "a Windows job": (self.queue_runs(timing=windows_timing),
                              self.queue_record([self.interval(WINDOWS_S7, 5, 905)])),
            "overlapping": (self.queue_runs(), self.queue_record([self.interval(PRODUCER, 5, 905)] * 2, 625)),
            "partially overlapping": (self.queue_runs(), self.queue_record(
                [self.interval(PRODUCER, 5, 405), self.interval(PRODUCER, 305, 630)], 1700)),
        }
        for label, (runs, record) in cases.items():
            with self.subTest(case=label):
                code, output, _shim = self.select(runs, record=record)
                self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_the_counterfactual_follows_the_dependency_graph(self):
        # Removing a runner queue moves only its job and what depends on it: a queue off the critical path saves
        # nothing, a Windows branch can still miss B, a shorter branch can become critical, and time spent waiting
        # for the producer is dependency wait, never runner queue.
        cases = {
            "a queue off the critical path": (
                timeline(producer_run_s=100, mac_run_s=85, windows_end_s=2390), 2400, 1500, None, 2),
            "a Windows branch still over budget": (timeline(windows_end_s=2000), 2425, 1525, None, 2),
            "a shorter branch becomes critical": (timeline(windows_end_s=1700), 2425, 1710, None, 0),
            "dependency wait submitted as queue": (
                timeline(mac_created_s=5), 2425, 925, [self.interval(MACOS_COMPARISONS[0], 5, 1505)], 2),
            # Read as queue, every comparison's wait for the producer would finish the run at 1515 s.
            "dependency wait on every comparison": (
                timeline(mac_created_s=5), 2425, 1515, [self.interval(name, 5, 1505) for name in MACOS_COMPARISONS], 2),
        }
        for label, ((timing, elapsed_s), recorded_elapsed_s, counterfactual_s, intervals, expected) in cases.items():
            with self.subTest(case=label):
                self.assertEqual(elapsed_s, recorded_elapsed_s)
                code, output, _shim = self.select(self.queue_runs(timing=timing), record=self.queue_record(
                    intervals, counterfactual_s, elapsed_s))
                self.assertEqual(code, expected, output)

    def test_a_queue_replacement_follows_the_record_and_takes_the_one_allowance(self):
        # The excluded run is the recorded first eligible run, still on attempt 1; the replacement was created
        # after the record, is the first later eligible run on the head, and a perf label triggered it.
        not_first = self.queue_record()
        not_first["first_eligible_run"]["run_id"] = RUN_ID - 5
        between = ({"id": RUN_ID + 100, "name": evaluator.WORKFLOW_NAME, "event": "pull_request",
                    "head_sha": HEAD_SHA, "created_at": "2026-10-05T10:50:00Z"}, jobs_record())
        cases = {
            "predecessor re-run": (self.queue_runs(predecessor_attempt=2), self.queue_record()),
            "excluded run is not the first eligible": (self.queue_runs(), not_first),
            "not the first new eligible run": (self.queue_runs(extra_runs=(between,)), self.queue_record()),
            "recorded after the replacement": (self.queue_runs(),
                                               self.queue_record(recorded_at="2026-10-05T11:05:00Z")),
            "recorded before the run finished": (self.queue_runs(),
                                                 self.queue_record(recorded_at="2026-10-05T10:40:00Z")),
            "not triggered by a label": (self.queue_runs(), self.queue_record(
                trigger={"action": "reopened", "url": PULL_URL})),
            "reopened with the label": (self.queue_runs(), self.queue_record(
                trigger={"action": "reopened", "label": "perf", "url": PULL_URL})),
            "another label": (self.queue_runs(), self.queue_record(
                trigger={"action": "labeled", "label": "bug", "url": PULL_URL})),
        }
        for label, (runs, record) in cases.items():
            with self.subTest(case=label):
                code, output, _shim = self.select(runs, record=record)
                self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_an_ineligible_run_in_between_is_not_the_replacement(self):
        # A run another label started on the head skips every job and its result job keeps the unevaluated name
        # expression, so it is not eligible and the later eligible run is the replacement.
        ineligible = ({"id": RUN_ID + 100, "name": evaluator.WORKFLOW_NAME, "event": "pull_request",
                       "head_sha": HEAD_SHA, "created_at": "2026-10-05T10:50:00Z"}, ineligible_jobs())
        code, output, _shim = self.select(self.queue_runs(extra_runs=(ineligible,)), record=self.queue_record())
        self.assertEqual(code, 0, output)

    def test_a_queue_budget_replacement_of_a_run_within_budget_is_refused(self):
        timing, elapsed_s = timeline(producer_queue_s=0)
        code, output, _shim = self.select(self.queue_runs(timing=timing),
                                          record=self.queue_record(elapsed_s=elapsed_s, counterfactual_s=elapsed_s))
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def test_a_queue_budget_replacement_needs_every_excluded_job_to_succeed(self):
        for conclusion in ("failure", "skipped", "cancelled"):
            with self.subTest(conclusion=conclusion):
                code, output, _shim = self.select(self.queue_runs(conclusions={evaluator.RESULT_JOB: conclusion}),
                                                  record=self.queue_record())
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
    def committed_change(self, change=None, remove=None):
        """Clone the fixture repository, change perf-compare with `change(text) -> text` or delete `remove`,
        commit, and return (clone, the new commit)."""
        tree = self.temp / "tree"
        git(self.temp, "clone", "-q", str(FIXTURE_TREE), str(tree))
        if change is not None:
            path = tree / "scripts" / "perf-compare.py"
            original = path.read_text(encoding="utf-8")
            changed = change(original)
            self.assertNotEqual(changed, original, "the mutation changed nothing")
            path.write_bytes(changed.encode("utf-8"))
        if remove is not None:
            (tree / remove).unlink()
        git(tree, "commit", "-q", "-a", "-m", "change")
        return tree, git(tree, "rev-parse", "HEAD").strip()

    def evaluate_commit(self, tree, commit):
        """Select as usual, then evaluate artifacts and a selection that name `commit` as the head."""
        code, output, _shim = self.select()
        self.assertEqual(code, 0, output)
        path = self.temp / "selection.json"
        selection = json.loads(path.read_text(encoding="utf-8"))
        selection["head_sha"] = commit
        path.write_text(json.dumps(selection), encoding="utf-8")
        build_artifacts(self.artifacts, head=commit)
        return self.evaluate(tree)

    def test_the_frozen_hashes_match_the_current_tree(self):
        # The embedded hashes name these exact bytes; a later change to a frozen file fails here first.
        for relative, digest in evaluator.FROZEN_FILES.items():
            self.assertEqual(evaluator.hashlib.sha256((ROOT / relative).read_bytes()).hexdigest(), digest, relative)

    def test_an_unchanged_commit_is_accepted(self):
        # The control: the fixture commit holds the frozen bytes, so the rule evaluates and accepts.
        tree = self.temp / "tree"
        git(self.temp, "clone", "-q", str(FIXTURE_TREE), str(tree))
        self.assertEqual(self.evaluate_commit(tree, HEAD_SHA)[0], evaluator.EXIT_ACCEPT)

    def test_a_working_tree_change_is_never_read(self):
        # The frozen files come from the head's commit, so an uncommitted edit in the checkout changes nothing.
        tree = self.temp / "tree"
        git(self.temp, "clone", "-q", str(FIXTURE_TREE), str(tree))
        with (tree / "scripts" / "perf-compare.py").open("a", encoding="utf-8") as handle:
            handle.write("\nrun_summary = None\n")
        code, output = self.evaluate_commit(tree, HEAD_SHA)
        self.assertEqual(code, evaluator.EXIT_ACCEPT, output)

    def test_a_pristine_checkout_cannot_hide_a_changed_commit(self):
        # The head's commit changed a frozen file; restoring the old bytes in the checkout does not hide it.
        tree, commit = self.committed_change(lambda text: text + "\nrun_summary = None\n")
        git(tree, "checkout", "-q", "HEAD~1", "--", "scripts/perf-compare.py")
        code, output = self.evaluate_commit(tree, commit)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)
        self.assertIn("scripts/perf-compare.py", output)

    def test_select_reads_the_frozen_files_at_its_head(self):
        # select reads the frozen files from --head's commit, so a head that changed one is refused.
        tree, commit = self.committed_change(lambda text: text + "\nrun_summary = None\n")
        runs = {RUN_ID: {"view": run_view(head_sha=commit), "jobs": {1: jobs_record()}}}
        code, output, _shim = self.select(runs, record=record_body(head=commit), head=commit, tree=tree)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)
        self.assertIn("scripts/perf-compare.py", output)

    def test_an_unknown_commit_is_refused(self):
        code, output = self.evaluate_commit(FIXTURE_TREE, "e" * 40)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)

    def assert_mutation_refused(self, change):
        tree, commit = self.committed_change(change)
        code, output = self.evaluate_commit(tree, commit)
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
        tree, commit = self.committed_change(remove="scripts/perf-critical-path.py")
        code, output = self.evaluate_commit(tree, commit)
        self.assertEqual(code, evaluator.EXIT_INVALID, output)
        self.assertIn("scripts/perf-critical-path.py", output)

    def test_only_the_verified_copy_is_imported(self):
        # The module the evaluator uses is the private copy of the commit's bytes, never a checkout file.
        workdir = self.temp / "work"
        workdir.mkdir()
        modules = evaluator.load_frozen(FIXTURE_TREE, HEAD_SHA, workdir)
        for module in modules.values():
            self.assertEqual(Path(module.__file__).parent, workdir)

    def test_the_evaluator_never_calls_load_gate(self):
        # perf-compare's load_gate would import local-gate.py, which is not frozen; it is patched to raise and
        # the evaluation still completes.
        original = evaluator.load_frozen

        def load_without_gate(tree, commit, workdir):
            modules = original(tree, commit, workdir)

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
