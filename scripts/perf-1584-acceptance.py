#!/usr/bin/env python3
"""The pre-registered acceptance rule for the renderer-waiting handshake, as a frozen evaluator.

Two commands, run in this order and never the other way round:

  select    Reads only run metadata (`gh run view --json` and the run's attempt jobs API), never an
            artifact, and writes selection.json: the decisive run and attempt, its head and merge base,
            the twelve required jobs with their conclusions and times, the budget accounting (B) and
            whether the single replacement allowance is used, and why. No performance value can reach it.
  evaluate  Reads selection.json and the downloaded comparison artifacts, checks run and artifact
            identity, selects perf-compare's final `valid` runs, validates each selected result, and
            prints every row with its operands. Exit 0 accepts, 1 rejects, 2 means the evidence is invalid.

The statistics and validation are perf-compare's own. Before anything else both commands read
`scripts/perf-compare.py` and `scripts/perf-critical-path.py` from the tree being evaluated, check each
file's SHA-256 against FROZEN_FILES, copy the verified bytes to a private temp directory and import only
those copies. A missing or changed file exits 2. perf-compare's `load_gate` is never called. This file's
own SHA-256 is published before any run, and the operator checks it before running it.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import re
import sys
import tempfile
from dataclasses import dataclass
from fractions import Fraction
from pathlib import Path
from typing import Callable, Mapping, Sequence

# The SHA-256 of each frozen file, as of the freeze commit. A later commit may not change either file.
PERF_COMPARE_SHA256 = "6d79d33ea70cff8e42cc124a57183428b2bc0b79c4483c3d7deafefc1e276620"
PERF_CRITICAL_PATH_SHA256 = "9658cb953ffb442ddc50da20c61b4260dcb4edfe46b5aeedabfd2436cf1d1c65"
FROZEN_FILES = {
    "scripts/perf-compare.py": PERF_COMPARE_SHA256,
    "scripts/perf-critical-path.py": PERF_CRITICAL_PATH_SHA256,
}
COMPARE = "scripts/perf-compare.py"
CRITICAL = "scripts/perf-critical-path.py"

EXIT_ACCEPT = 0
EXIT_REJECT = 1
EXIT_INVALID = 2

DEFAULT_REPOSITORY = "D0n9X1n/SonicTerm"
WORKFLOW_NAME = "Performance comparison"
EVENT = "pull_request"
VIEW_FIELDS = "databaseId,attempt,workflowName,event,headSha,createdAt,updatedAt,jobs"
PAGE_SIZE = 100
PRODUCER_JOB = "macOS perf binaries (base and head)"
RESULT_JOB = "Performance comparison result"
SHARDS = ("S1-S3-S6-S8-S12", "S2-S10sync", "S4-S5-S11", "S7", "S9-S10")
# The twelve required jobs, by their exact display names.
REQUIRED_JOBS = (PRODUCER_JOB,
                 *(f"macOS before/after comparison ({shard})" for shard in SHARDS),
                 *(f"Windows before/after comparison ({shard})" for shard in SHARDS),
                 RESULT_JOB)
PLATFORM_NAMES = {"macOS": "darwin", "Windows": "win32"}
# Each scenario directory's comparison artifact, per platform.
SCENARIO_SHARD = {
    ("macOS", "S3-default"): "S1-S3-S6-S8-S12", ("macOS", "S2-flood"): "S1-S3-S6-S8-S12",
    ("macOS", "S4-default"): "S4-S5-S11",
    ("Windows", "S3-default"): "S1-S3-S6-S8-S12", ("Windows", "S2-flood"): "S9-S10",
    ("Windows", "S4-default"): "S4-S5-S11",
}
SCENARIOS = ("S3-default", "S2-flood", "S4-default")
# The one phase each scenario's rows read.
RELEVANT_PHASE = {"S3-default": "flood", "S2-flood": "typing", "S4-default": "stream"}
# Selected timed runs per side; S2/flood is capped at two.
TIMED_RUNS = {"S3-default": 5, "S2-flood": 2, "S4-default": 5}
# Selected counters runs on the head, S3/default only.
COUNTERS_RUNS = 2

# The rule's thresholds, fixed before any run.
A1_MEDIAN_FACTOR = 2
A2_MIN_FRAME_RATIO = Fraction(4, 5)
G1_MEDIAN_FACTOR = Fraction(9, 10)
G3_P95_FACTOR = Fraction(11, 10)

ARTIFACT_NAME = re.compile(r"perf-comparison-(?P<pull>\d+)-(?P<head>[0-9a-f]{40})-(?P<platform>macOS|Windows)-"
                           r"(?P<shard>.+)-(?P<attempt>\d+)")
RAW_LOG_LINE = re.compile(r"- (?P<label>\S+) (?P<set>\S+) (?P<side>base|head) (?P<kind>\S+): "
                          r"`(?P<evidence>.+)/01-harness\.log`")
EVIDENCE_TAIL = re.compile(r"(?:^|/)runs/(?P<scenario>[^/]+)/(?P<set>[^/]+)/(?P<folder>\d{2,}-(?P<side>base|head))$")
BASE_LINE = re.compile(r"- Base: `[^`]*` = `(?P<sha>[0-9a-f]{40})`")
HEAD_LINE = re.compile(r"- Head: `[^`]*` = `(?P<sha>[0-9a-f]{40})`")
HARNESS_LINE = re.compile(r"- Harness hash \(both trees\): `(?P<digest>[0-9a-f]+)`")
SHA = re.compile(r"[0-9a-f]{40}")


class EvidenceInvalid(Exception):
    """The evidence cannot be evaluated: exit 2."""


def load_frozen(tree: Path, workdir: Path) -> dict:
    """Verify each frozen file in `tree`, copy the verified bytes into `workdir` and import only the copies."""
    modules = {}
    for relative, expected in FROZEN_FILES.items():
        source = tree / relative
        try:
            content = source.read_bytes()
        except OSError as error:
            raise EvidenceInvalid(f"frozen file {relative} cannot be read: {error}") from error
        digest = hashlib.sha256(content).hexdigest()
        if digest != expected:
            raise EvidenceInvalid(f"frozen file {relative} has SHA-256 {digest}, not {expected}")
        copy_path = workdir / Path(relative).name
        copy_path.write_bytes(content)
        name = "perf_1584_frozen_" + Path(relative).stem.replace("-", "_")
        spec = importlib.util.spec_from_file_location(name, copy_path)
        if spec is None or spec.loader is None:
            raise EvidenceInvalid(f"frozen file {relative} cannot be imported")
        module = importlib.util.module_from_spec(spec)
        # Dataclasses resolve their module through sys.modules while the class body runs.
        sys.modules[name] = module
        spec.loader.exec_module(module)
        modules[relative] = module
    return modules


# --- select: metadata only -------------------------------------------------------------------

def gh_json(runner: Callable[[Sequence[str]], bytes], argv: Sequence[str]) -> object:
    """One `gh` read, as JSON."""
    try:
        return json.loads(runner(list(argv)).decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvidenceInvalid(f"{' '.join(argv)} returned no JSON: {error}") from error


def view_run(runner, repository: str, run_id: int) -> dict:
    """`gh run view` of the run's latest attempt."""
    view = gh_json(runner, ["gh", "run", "view", str(run_id), "--repo", repository, "--json", VIEW_FIELDS])
    if not isinstance(view, dict):
        raise EvidenceInvalid(f"run {run_id}: gh run view returned no object")
    return view


def attempt_jobs(runner, repository: str, run_id: int, attempt: int) -> list:
    """Every job of one attempt, from the run's attempt jobs API, page by page."""
    jobs, page = [], 1
    while True:
        path = f"repos/{repository}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page={PAGE_SIZE}&page={page}"
        batch = gh_json(runner, ["gh", "api", path])
        if not isinstance(batch, dict) or not isinstance(batch.get("jobs"), list):
            raise EvidenceInvalid(f"run {run_id} attempt {attempt}: the jobs API returned no job list")
        jobs.extend(batch["jobs"])
        if len(batch["jobs"]) < PAGE_SIZE:
            return jobs
        page += 1


def required_jobs(jobs: Sequence[Mapping], where: str) -> dict:
    """Each required job by name; a required job missing or listed twice is invalid evidence."""
    found = {}
    for name in REQUIRED_JOBS:
        matches = [job for job in jobs if isinstance(job, dict) and job.get("name") == name]
        if len(matches) != 1:
            raise EvidenceInvalid(f"{where}: required job {name!r} appears {len(matches)} times")
        found[name] = matches[0]
    return found


def budget_of(critical, created_at: str, jobs: Mapping[str, Mapping]) -> dict:
    """B: from the run's creation to the last required job's completion, queue included."""
    completions = {name: job.get("completed_at") for name, job in jobs.items()}
    if any(not isinstance(stamp, str) or job.get("status") != "completed"
           for stamp, job in zip(completions.values(), jobs.values())):
        return {"measurable": False, "elapsed_s": None, "limit_s": critical.BUDGET_S, "within": False,
                "last_job": None}
    created_s = critical.parse_time(created_at)
    last_job = max(completions, key=lambda name: critical.parse_time(completions[name]))
    elapsed_s = critical.parse_time(completions[last_job]) - created_s
    return {"measurable": True, "elapsed_s": elapsed_s, "limit_s": critical.BUDGET_S,
            "within": elapsed_s <= critical.BUDGET_S, "last_job": last_job}


def check_identity(view: Mapping, run_id: int, head_sha: str) -> None:
    """The run is the comparison workflow's pull-request run on the frozen head."""
    if view.get("databaseId") != run_id:
        raise EvidenceInvalid(f"gh run view describes run {view.get('databaseId')!r}, not {run_id}")
    if view.get("workflowName") != WORKFLOW_NAME:
        raise EvidenceInvalid(f"run {run_id} is workflow {view.get('workflowName')!r}, not {WORKFLOW_NAME!r}")
    if view.get("event") != EVENT:
        raise EvidenceInvalid(f"run {run_id} was triggered by {view.get('event')!r}, not {EVENT}")
    if view.get("headSha") != head_sha:
        raise EvidenceInvalid(f"run {run_id} ran head {view.get('headSha')!r}, not the frozen head {head_sha}")


def check_replacement(args, runner, critical, view: Mapping) -> dict:
    """The single replacement allowance: absent, a same-run re-run of all jobs after a listed infrastructure
    failure, or a new run on the same head after an all-success run that missed B only by queueing."""
    attempt = view.get("attempt")
    if args.replaces is None:
        if attempt != 1:
            raise EvidenceInvalid(f"attempt {attempt} is a re-run; a replacement must be declared with --replaces")
        return {"used": False}
    match = re.fullmatch(r"(\d+):(\d+)", args.replaces)
    if match is None:
        raise EvidenceInvalid("--replaces takes RUN_ID:ATTEMPT")
    excluded_run, excluded_attempt = int(match[1]), int(match[2])
    evidence = (args.replacement_evidence or "").strip()
    if args.replacement_kind is None or not evidence:
        raise EvidenceInvalid("a replacement needs --replacement-kind and --replacement-evidence")
    if excluded_attempt != 1:
        raise EvidenceInvalid("only the first execution may be excluded; one replacement is allowed in total")
    excluded_jobs = attempt_jobs(runner, args.repo, excluded_run, excluded_attempt)
    record = {"used": True, "kind": args.replacement_kind, "excluded_run": excluded_run,
              "excluded_attempt": excluded_attempt, "evidence": evidence}
    if args.replacement_kind == "infrastructure":
        if excluded_run != args.run or attempt != 2:
            raise EvidenceInvalid("an infrastructure replacement is Re-run all jobs: the same run ID, attempt 2")
        conclusions = {job.get("name"): job.get("conclusion") for job in excluded_jobs if isinstance(job, dict)}
        failed = sorted(name for name in REQUIRED_JOBS if conclusions.get(name) != "success")
        if not failed:
            raise EvidenceInvalid("the excluded attempt has no failed required job to attribute to infrastructure")
        record["excluded_failures"] = failed
        return record
    if excluded_run == args.run or attempt != 1:
        raise EvidenceInvalid("a queue-budget replacement is the first new run ID, attempt 1")
    excluded_view = view_run(runner, args.repo, excluded_run)
    check_identity(excluded_view, excluded_run, args.head)
    if critical.parse_time(excluded_view.get("createdAt")) >= critical.parse_time(view.get("createdAt")):
        raise EvidenceInvalid("the replacement run must be created after the run it replaces")
    found = required_jobs(excluded_jobs, f"excluded run {excluded_run}")
    unsuccessful = sorted(name for name, job in found.items() if job.get("conclusion") != "success")
    if unsuccessful:
        raise EvidenceInvalid(f"a queue-budget replacement needs every excluded job to succeed: {unsuccessful}")
    excluded_budget = budget_of(critical, excluded_view.get("createdAt"), found)
    if not excluded_budget["measurable"] or excluded_budget["within"]:
        raise EvidenceInvalid(f"the excluded run satisfied B or cannot be measured: {excluded_budget}")
    record["excluded_budget"] = excluded_budget
    return record


def select(args, runner, critical) -> dict:
    """Build selection.json from metadata alone."""
    if not SHA.fullmatch(args.head or "") or not SHA.fullmatch(args.merge_base or ""):
        raise EvidenceInvalid("--head and --merge-base must be full 40-character SHAs")
    view = view_run(runner, args.repo, args.run)
    check_identity(view, args.run, args.head)
    attempt = view.get("attempt")
    if not isinstance(attempt, int) or attempt < 1:
        raise EvidenceInvalid(f"run {args.run} reports attempt {attempt!r}")
    replacement = check_replacement(args, runner, critical, view)
    jobs = required_jobs(attempt_jobs(runner, args.repo, args.run, attempt), f"run {args.run} attempt {attempt}")
    viewed = required_jobs(view.get("jobs") or [], f"gh run view of run {args.run}")
    for name in REQUIRED_JOBS:
        if str(viewed[name].get("conclusion")).lower() != str(jobs[name].get("conclusion")).lower():
            raise EvidenceInvalid(f"{name}: gh run view and the jobs API disagree on its conclusion")
    return {
        "schema_version": 1, "run_id": args.run, "attempt": attempt, "workflow": view.get("workflowName"),
        "event": view.get("event"), "head_sha": args.head, "merge_base": args.merge_base,
        "created_at": view.get("createdAt"), "updated_at": view.get("updatedAt"),
        "jobs": [{"name": name, "status": jobs[name].get("status"), "conclusion": jobs[name].get("conclusion"),
                  "started_at": jobs[name].get("started_at"), "completed_at": jobs[name].get("completed_at")}
                 for name in REQUIRED_JOBS],
        "budget": budget_of(critical, view.get("createdAt"), jobs),
        "replacement": replacement,
    }


# --- evaluate: identity, population, validation ----------------------------------------------

@dataclass
class SelectedRun:
    """One selected attempt: its directory and the documents it carries."""

    directory: Path
    outcome: dict
    result: dict


def read_json(path: Path) -> object:
    """A JSON document from an artifact; unreadable evidence is invalid."""
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvidenceInvalid(f"{path} cannot be read: {error}") from error


def check_selection(selection: object) -> dict:
    """Run identity from selection.json: the workflow, event, head, run, attempt, and twelve successes."""
    if not isinstance(selection, dict):
        raise EvidenceInvalid("selection.json is not an object")
    if selection.get("workflow") != WORKFLOW_NAME or selection.get("event") != EVENT:
        raise EvidenceInvalid("selection.json does not name a pull-request Performance comparison run")
    for key in ("head_sha", "merge_base"):
        if not SHA.fullmatch(str(selection.get(key))):
            raise EvidenceInvalid(f"selection.json {key} is not a SHA")
    if not isinstance(selection.get("run_id"), int) or not isinstance(selection.get("attempt"), int):
        raise EvidenceInvalid("selection.json lacks the run ID or attempt")
    jobs = {job.get("name"): job for job in selection.get("jobs") or [] if isinstance(job, dict)}
    if sorted(jobs) != sorted(REQUIRED_JOBS) or len(selection.get("jobs") or []) != len(REQUIRED_JOBS):
        raise EvidenceInvalid("selection.json does not record exactly the twelve required jobs")
    failed = sorted(name for name, job in jobs.items() if job.get("conclusion") != "success")
    if failed:
        raise EvidenceInvalid(f"required jobs did not succeed: {failed}")
    return selection


def selected_artifacts(root: Path, selection: Mapping) -> dict:
    """This attempt's comparison artifacts by (platform, shard); another attempt's are ignored."""
    found = {}
    try:
        entries = sorted(root.iterdir())
    except OSError as error:
        raise EvidenceInvalid(f"artifacts directory {root} cannot be read: {error}") from error
    for entry in entries:
        named = ARTIFACT_NAME.fullmatch(entry.name)
        if named is None or not entry.is_dir() or int(named["attempt"]) != selection["attempt"]:
            continue
        if named["head"] != selection["head_sha"]:
            raise EvidenceInvalid(f"artifact {entry.name} names head {named['head']}, not {selection['head_sha']}")
        key = (named["platform"], named["shard"])
        if key in found:
            raise EvidenceInvalid(f"two artifacts of attempt {selection['attempt']} for {key}")
        found[key] = entry
    return found


def check_artifact(artifact: Path, platform: str, shard: str, selection: Mapping) -> str:
    """Artifact identity; returns the artifact's harness hash."""
    timing = read_json(artifact / "timing.json")
    job = f"{platform} before/after comparison ({shard})"
    if not isinstance(timing, dict) or (str(timing.get("run_id")), str(timing.get("run_attempt")),
                                        timing.get("shard"), timing.get("job")) \
            != (str(selection["run_id"]), str(selection["attempt"]), shard, job):
        raise EvidenceInvalid(f"{artifact.name}: timing.json names another run, attempt, shard or job")
    if sum(1 for record in selection["jobs"] if record["name"] == job and record["conclusion"] == "success") != 1:
        raise EvidenceInvalid(f"{artifact.name}: job {job!r} is not one successful job of the selected attempt")
    try:
        lines = (artifact / "comparison.md").read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as error:
        raise EvidenceInvalid(f"{artifact.name}: comparison.md cannot be read: {error}") from error
    bases = [BASE_LINE.fullmatch(line) for line in lines if line.startswith("- Base:")]
    heads = [HEAD_LINE.fullmatch(line) for line in lines if line.startswith("- Head:")]
    harnesses = [HARNESS_LINE.fullmatch(line) for line in lines if line.startswith("- Harness hash")]
    if len(bases) != 1 or bases[0] is None or bases[0]["sha"] != selection["merge_base"]:
        raise EvidenceInvalid(f"{artifact.name}: comparison.md's Base is not the merge base")
    if len(heads) != 1 or heads[0] is None or heads[0]["sha"] != selection["head_sha"]:
        raise EvidenceInvalid(f"{artifact.name}: comparison.md's Head is not the frozen head")
    if len(harnesses) != 1 or harnesses[0] is None:
        raise EvidenceInvalid(f"{artifact.name}: comparison.md has no single harness hash")
    return harnesses[0]["digest"]


def raw_log_entries(artifact: Path) -> list:
    """Each raw-log line as (scenario, set, side, kind, attempt directory); each maps to a unique directory."""
    entries, seen = [], set()
    for line in (artifact / "comparison.md").read_text(encoding="utf-8").splitlines():
        parsed = RAW_LOG_LINE.fullmatch(line)
        if parsed is None:
            continue
        evidence = parsed["evidence"].replace("\\", "/")
        tail = EVIDENCE_TAIL.search(evidence)
        scenario = parsed["label"].replace("/", "-", 1)
        if tail is None or (tail["scenario"], tail["set"], tail["side"]) != (scenario, parsed["set"], parsed["side"]):
            raise EvidenceInvalid(f"{artifact.name}: raw log {evidence} does not name its run's directory")
        directory = artifact / "runs" / scenario / parsed["set"] / tail["folder"]
        if directory in seen:
            raise EvidenceInvalid(f"{artifact.name}: raw log {evidence} is listed more than once")
        if not directory.is_dir():
            raise EvidenceInvalid(f"{artifact.name}: raw log {evidence} has no attempt directory")
        seen.add(directory)
        entries.append((scenario, parsed["set"], parsed["side"], parsed["kind"], directory))
    return entries


def relevant_phase(result: Mapping, scenario: str) -> dict:
    """The single phase a scenario's rows read."""
    name = RELEVANT_PHASE[scenario]
    phases = [phase for phase in result.get("phases") or [] if isinstance(phase, dict) and phase.get("name") == name]
    if len(phases) != 1:
        raise EvidenceInvalid(f"exactly one {name} phase is required, found {len(phases)}")
    return phases[0]


def is_count(value: object) -> bool:
    """A non-negative integer that is not a boolean."""
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def check_result(compare, run: SelectedRun, platform: str, scenario: str, set_name: str, side: str,
                 harness: str) -> None:
    """Section 7.3's checks of one selected result; any failure is invalid evidence.

    A head counters run is validated as a whole contract (`partial_counters` false), so every A2 and G5
    field must be present as a non-negative integer; a base's may lack fields its older contract never had.
    """
    result, outcome, where = run.result, run.outcome, str(run.directory)
    problems = compare.validate_result(result, harness, outcome.get("exit_code"), counters=set_name == "counters",
                                       partial_counters=side == "base", platform_name=PLATFORM_NAMES[platform])
    if problems:
        raise EvidenceInvalid(f"{where}: {'; '.join(problems)}")
    if outcome.get("status") != "PASS":
        raise EvidenceInvalid(f"{where}: outcome status {outcome.get('status')!r} is not PASS")
    if outcome.get("exit_code") != 0 or result.get("exit_code") != 0:
        raise EvidenceInvalid(f"{where}: exit code {result.get('exit_code')} (result), "
                              f"{outcome.get('exit_code')} (process) is not zero")
    scenario_id, variant = scenario.split("-", 1)
    if (result.get("scenario"), result.get("variant")) != (scenario_id, variant):
        raise EvidenceInvalid(f"{where}: result describes {result.get('scenario')}/{result.get('variant')}")
    if result.get("managed") is not True or result.get("status") != "valid":
        raise EvidenceInvalid(f"{where}: result is not a managed valid run")
    if result.get("short") is not True:
        raise EvidenceInvalid(f"{where}: result short is {result.get('short')!r}, not true")
    expected_state = "on" if set_name == "counters" else "off"
    if result.get("frame_counters") != expected_state:
        raise EvidenceInvalid(f"{where}: frame_counters is {result.get('frame_counters')!r}, not {expected_state}")
    phase = relevant_phase(result, scenario)
    start, end = phase.get("start_unix_s"), phase.get("end_unix_s")
    if not all(isinstance(value, (int, float)) and math.isfinite(value) for value in (start, end)) or end - start <= 0:
        raise EvidenceInvalid(f"{where}: the {phase.get('name')} phase has no finite positive duration")
    if not is_count(phase.get("presented_frames")):
        raise EvidenceInvalid(f"{where}: presented_frames {phase.get('presented_frames')!r} "
                              "is not a non-negative integer")
    if scenario == "S3-default" and set_name == "timed":
        throughput = result.get("throughput")
        if not (isinstance(throughput, dict) and is_count(throughput.get("bytes"))
                and isinstance(throughput.get("seconds"), (int, float))
                and math.isfinite(throughput["seconds"]) and throughput["seconds"] > 0):
            raise EvidenceInvalid(f"{where}: throughput operands are invalid")
    if scenario == "S2-flood":
        latency = result.get("latency")
        if not (isinstance(latency, dict) and compare.latency_values(latency)
                and is_count(latency.get("attributed")) and is_count(latency.get("total"))):
            raise EvidenceInvalid(f"{where}: latency needs samples and integer attributed and total")


def collect(compare, selection: Mapping, root: Path) -> tuple[dict, list]:
    """Every selected run by (platform, scenario, set, side), and the report's notes."""
    artifacts = selected_artifacts(root, selection)
    notes, population = [], {}
    for platform in PLATFORM_NAMES:
        harnesses = set()
        for scenario in SCENARIOS:
            holders = [shard for (owner, shard), artifact in artifacts.items()
                       if owner == platform and (artifact / "runs" / scenario).is_dir()]
            if holders != [SCENARIO_SHARD[(platform, scenario)]]:
                raise EvidenceInvalid(f"{platform} {scenario} is held by {holders}, "
                                      f"not exactly {SCENARIO_SHARD[(platform, scenario)]}")
        shards = sorted({SCENARIO_SHARD[(platform, scenario)] for scenario in SCENARIOS})
        for shard in shards:
            if (platform, shard) not in artifacts:
                raise EvidenceInvalid(f"no {platform} {shard} artifact of attempt {selection['attempt']}")
            harnesses.add(check_artifact(artifacts[(platform, shard)], platform, shard, selection))
        if len(harnesses) != 1:
            raise EvidenceInvalid(f"{platform} artifacts disagree on the harness hash: {sorted(harnesses)}")
        harness = harnesses.pop()
        for shard in shards:
            for scenario, set_name, side, kind, directory in raw_log_entries(artifacts[(platform, shard)]):
                if scenario not in SCENARIOS or kind != "valid":
                    continue
                outcome = read_json(directory / "outcome.json")
                scenario_id, variant = scenario.split("-", 1)
                if not isinstance(outcome, dict) or (outcome.get("side"), outcome.get("scenario"),
                                                     outcome.get("variant")) != (side, scenario_id, variant):
                    raise EvidenceInvalid(f"{directory}: outcome.json disagrees with its raw-log line")
                result = read_json(directory / "scratch" / "result.json")
                if not isinstance(result, dict):
                    raise EvidenceInvalid(f"{directory}: result.json is not an object")
                run = SelectedRun(directory, outcome, result)
                try:
                    check_result(compare, run, platform, scenario, set_name, side, harness)
                except EvidenceInvalid as problem:
                    if set_name == "counters" and side == "base":
                        # A base counters run feeds only the report, so its failure is a note.
                        notes.append(f"base counters run excluded: {problem}")
                        continue
                    raise
                population.setdefault((platform, scenario, set_name, side), []).append(run)
        for scenario in SCENARIOS:
            wanted = [("timed", "base", TIMED_RUNS[scenario]), ("timed", "head", TIMED_RUNS[scenario])]
            if scenario == "S3-default":
                wanted.append(("counters", "head", COUNTERS_RUNS))
            for set_name, side, count in wanted:
                selected = len(population.get((platform, scenario, set_name, side), []))
                notes.append(f"{platform} {scenario} {set_name} {side}: {selected} selected")
                if selected != count:
                    raise EvidenceInvalid(f"{platform} {scenario} {set_name} {side}: "
                                          f"{selected} selected runs, not {count}")
    return population, notes


# --- rows --------------------------------------------------------------------------------------

@dataclass
class Row:
    """One row of the rule and its operands."""

    name: str
    passed: bool
    detail: str


def finite(value: object) -> float | None:
    """`value` when it is a finite number; else None, which fails its row."""
    return value if isinstance(value, (int, float)) and math.isfinite(value) else None


def presented_fps(run: SelectedRun, scenario: str) -> float | None:
    """The relevant phase's presented frames per second."""
    phase = relevant_phase(run.result, scenario)
    return finite(phase["presented_frames"] / (phase["end_unix_s"] - phase["start_unix_s"]))


def throughput_mbps(run: SelectedRun) -> float | None:
    """The run's throughput in MB/s."""
    throughput = run.result["throughput"]
    return finite(throughput["bytes"] / throughput["seconds"] / 1_000_000)


def summaries(compare, values_by_side: Mapping[str, list]) -> dict | None:
    """perf-compare's run summary per side; None when any selected run lacks a finite value."""
    if any(value is None for values in values_by_side.values() for value in values):
        return None
    return {side: compare.run_summary(values) for side, values in values_by_side.items()}


def window_counters(run: SelectedRun, scenario: str = "S3-default") -> dict:
    """The relevant phase's frame_counters."""
    return relevant_phase(run.result, scenario).get("frame_counters") or {}


def rows_of(compare, critical, selection: Mapping, population: Mapping) -> list:
    """Every row A1-G5 and B, with its operands."""
    def runs(platform, scenario, set_name, side):
        return population[(platform, scenario, set_name, side)]

    def by_side(platform, scenario, metric):
        return {side: [metric(run) for run in runs(platform, scenario, "timed", side)] for side in ("base", "head")}

    rows = []
    fps = summaries(compare, by_side("macOS", "S3-default", lambda run: presented_fps(run, "S3-default")))
    if fps is None:
        rows.append(Row("A1", False, "a selected run lacks a finite fps"))
    else:
        base, head = fps["base"], fps["head"]
        passed = Fraction(head.median) >= A1_MEDIAN_FACTOR * Fraction(base.median) \
            and Fraction(head.minimum) > Fraction(base.median)
        rows.append(Row("A1", passed, f"head median {head.median:.4f} vs 2 x base median {base.median:.4f}; "
                                      f"head min {head.minimum:.4f} vs base median {base.median:.4f}"))

    frames_total = lost_total = 0
    presence = []
    for run in runs("macOS", "S3-default", "counters", "head"):
        window = window_counters(run)["window"]
        wakes, frames, lost = (window["parser_yield_wakes"], window["parser_yield_frames"],
                               window["parser_yield_lost"])
        start, end = window["parser_yield_tokens_start"], window["parser_yield_tokens_end"]
        if wakes - frames - lost != end - start:
            raise EvidenceInvalid(f"{run.directory}: invariant W - F - L = end - start fails: "
                                  f"{wakes} - {frames} - {lost} != {end} - {start}")
        presence.append(wakes >= 1 and frames >= 1)
        frames_total += frames
        lost_total += lost
    resolved = frames_total + lost_total
    ratio_ok = resolved > 0 and Fraction(frames_total, resolved) >= A2_MIN_FRAME_RATIO
    rows.append(Row("A2", all(presence) and ratio_ok,
                    f"wakes and frames present in {sum(presence)}/{len(presence)} runs; "
                    f"ratio {frames_total}/{resolved} vs 4/5; invariant holds in every run"))

    for platform in PLATFORM_NAMES:
        throughput = summaries(compare, by_side(platform, "S3-default", throughput_mbps))
        if throughput is None:
            rows.append(Row(f"G1 {platform}", False, "a selected run lacks a finite throughput"))
            continue
        base, head = throughput["base"], throughput["head"]
        passed = Fraction(head.median) >= G1_MEDIAN_FACTOR * Fraction(base.median) \
            and Fraction(head.median) >= Fraction(base.minimum)
        rows.append(Row(f"G1 {platform}", passed,
                        f"head median {head.median:.4f} MB/s vs 0.9 x base median {base.median:.4f} "
                        f"and base min {base.minimum:.4f}"))

    windows_fps = summaries(compare, by_side("Windows", "S3-default", lambda run: presented_fps(run, "S3-default")))
    if windows_fps is None:
        rows.append(Row("G2", False, "a selected run lacks a finite fps"))
    else:
        base, head = windows_fps["base"], windows_fps["head"]
        rows.append(Row("G2", Fraction(head.median) >= Fraction(base.minimum),
                        f"head median {head.median:.4f} vs base min {base.minimum:.4f}"))

    for platform in PLATFORM_NAMES:
        coverage, pooled = {}, {}
        for side in ("base", "head"):
            selected = runs(platform, "S2-flood", "timed", side)
            coverage[side] = (sum(run.result["latency"]["attributed"] for run in selected),
                              sum(run.result["latency"]["total"] for run in selected))
            pooled[side] = [value for run in selected for value in compare.latency_values(run.result["latency"])]
        accepted = compare.latency_acceptance(coverage["base"], coverage["head"])
        base_p95, head_p95 = compare.percentile_95(pooled["base"]), compare.percentile_95(pooled["head"])
        passed = accepted and Fraction(head_p95) <= G3_P95_FACTOR * Fraction(base_p95)
        rows.append(Row(f"G3 {platform}", passed,
                        f"coverage base {coverage['base'][0]}/{coverage['base'][1]}, head "
                        f"{coverage['head'][0]}/{coverage['head'][1]} (acceptance {accepted}); "
                        f"head p95 {head_p95} vs 1.1 x base p95 {base_p95}"))

    for platform in PLATFORM_NAMES:
        stream = summaries(compare, by_side(platform, "S4-default", lambda run: presented_fps(run, "S4-default")))
        if stream is None:
            rows.append(Row(f"G4 {platform}", False, "a selected run lacks a finite fps"))
            continue
        base, head = stream["base"], stream["head"]
        rows.append(Row(f"G4 {platform}", Fraction(head.median) >= Fraction(base.minimum),
                        f"head median {head.median:.4f} vs base min {base.minimum:.4f}"))

    inert = [(window_counters(run)["vt"]["parser_yields"], window_counters(run)["window"]["parser_yield_requests"])
             for run in runs("Windows", "S3-default", "counters", "head")]
    rows.append(Row("G5", all(sends == 0 and requests == 0 for sends, requests in inert),
                    f"(parser_yields, parser_yield_requests) per head counters run: {inert}"))

    budget = selection.get("budget") or {}
    jobs = {job["name"]: job for job in selection["jobs"]}
    if budget.get("measurable"):
        recomputed = budget_of(critical, selection["created_at"], jobs)
        if recomputed["elapsed_s"] != budget.get("elapsed_s"):
            raise EvidenceInvalid("selection.json's budget does not match its recorded job times")
        rows.append(Row("B", recomputed["within"],
                        f"{recomputed['elapsed_s']} s to {recomputed['last_job']!r} vs {critical.BUDGET_S} s"))
    else:
        rows.append(Row("B", False, "the decisive run's required jobs did not all complete"))
    return rows


def histogram_sum(counters: Mapping, section: str, name: str) -> int | None:
    """A histogram's exact microsecond sum, when the run has it."""
    value = (counters.get(section) or {}).get(name)
    return value.get("sum_us") if isinstance(value, dict) else None


def report_lines(population: Mapping) -> list:
    """Section 6's per-run figures, never gated."""
    lines = []
    for (platform, scenario, set_name, side), selected in sorted(population.items(), key=lambda item: item[0]):
        if set_name != "counters":
            continue
        for run in selected:
            phase = relevant_phase(run.result, scenario)
            wall_s = phase["end_unix_s"] - phase["start_unix_s"]
            counters = phase.get("frame_counters") or {}
            figures = {}
            for label, (section, name) in (("worker_hold", ("vt", "parser_lock_hold_us")),
                                           ("parse", ("vt", "parse_us")),
                                           ("wait", ("vt", "parser_yield_wait_us"))):
                total_us = histogram_sum(counters, section, name)
                figures[label] = "n/a" if total_us is None else f"{total_us / (1e6 * wall_s):.4f}"
            sends = (counters.get("vt") or {}).get("parser_yields")
            figures["yields_per_s"] = "n/a" if sends is None else f"{sends / wall_s:.2f}"
            window = counters.get("window") or {}
            terms = [window.get(name, "n/a") for name in ("parser_yield_wakes", "parser_yield_frames",
                                                          "parser_yield_lost", "parser_yield_tokens_start",
                                                          "parser_yield_tokens_end")]
            text = ", ".join(f"{key} {value}" for key, value in figures.items())
            lines.append(f"report {platform} {scenario} {side} {run.directory.name}: wall_s {wall_s:.2f}, {text}, "
                         f"W F L start end {terms}")
    return lines


def evaluate(compare, critical, selection: object, root: Path) -> tuple[list, list]:
    """Rows and report lines; invalid evidence raises."""
    selection = check_selection(selection)
    population, notes = collect(compare, selection, root)
    return rows_of(compare, critical, selection, population), notes + report_lines(population)


def parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    """`select` or `evaluate`."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    tree_default = Path(__file__).resolve().parent.parent
    chooser = commands.add_parser("select", help="write selection.json from run metadata only")
    chooser.add_argument("--run", type=int, required=True, help="the candidate decisive run ID")
    chooser.add_argument("--head", required=True, help="the frozen head SHA")
    chooser.add_argument("--merge-base", required=True, help="the recorded merge base on main")
    chooser.add_argument("--output", type=Path, required=True, help="where selection.json goes")
    chooser.add_argument("--repo", default=DEFAULT_REPOSITORY, help=f"owner/name (default {DEFAULT_REPOSITORY})")
    chooser.add_argument("--tree", type=Path, default=tree_default, help="the tree holding the frozen files")
    chooser.add_argument("--replaces", help="RUN_ID:ATTEMPT of the one excluded execution")
    chooser.add_argument("--replacement-kind", choices=("infrastructure", "queue-budget"))
    chooser.add_argument("--replacement-evidence", help="the job evidence and accounting for the exclusion")
    judge = commands.add_parser("evaluate", help="judge the rule from selection.json and the artifacts")
    judge.add_argument("--selection", type=Path, required=True)
    judge.add_argument("--artifacts", type=Path, required=True, help="the downloaded artifacts, one folder each")
    judge.add_argument("--tree", type=Path, default=tree_default, help="the head tree holding the frozen files")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None, runner: Callable[[Sequence[str]], bytes] | None = None) -> int:
    """Run one command; see the module docstring for exit codes."""
    args = parse_args(argv)
    with tempfile.TemporaryDirectory(prefix="perf-1584-frozen-") as workdir:
        try:
            modules = load_frozen(args.tree, Path(workdir))
            compare, critical = modules[COMPARE], modules[CRITICAL]
            try:
                if args.command == "select":
                    selection = select(args, runner or critical.run_bounded, critical)
                    args.output.write_text(json.dumps(selection, indent=2, sort_keys=True) + "\n", encoding="utf-8")
                    print(f"selected run {selection['run_id']} attempt {selection['attempt']}; "
                          f"B {selection['budget']}; replacement {selection['replacement']}")
                    return EXIT_ACCEPT
                rows, report = evaluate(compare, critical, read_json(args.selection), args.artifacts)
            except critical.AccountingError as error:
                raise EvidenceInvalid(str(error)) from error
        except EvidenceInvalid as error:
            print(f"evidence invalid: {error}", file=sys.stderr)
            return EXIT_INVALID
    for row in rows:
        print(f"{row.name}: {'PASS' if row.passed else 'FAIL'}: {row.detail}")
    for line in report:
        print(line)
    accepted = all(row.passed for row in rows)
    print("ACCEPT" if accepted else "REJECT")
    return EXIT_ACCEPT if accepted else EXIT_REJECT


if __name__ == "__main__":
    sys.exit(main())
