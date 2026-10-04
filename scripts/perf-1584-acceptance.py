#!/usr/bin/env python3
"""The pre-registered acceptance rule for the renderer-waiting handshake, as a frozen evaluator.

Two commands, run in this order and never the other way round:

  select    Reads the operator's record (the first eligible run and, when one is excluded, the
            structured exclusion, written before any replacement was triggered) and only run metadata
            (`gh run view --json`, the attempt jobs API and the head's run list), never an artifact. It
            writes selection.json: the decisive run and attempt, its head and merge base, the twelve
            required jobs with their conclusions and times, the budget accounting (B), and the single
            replacement allowance with its checked exclusion. No performance value can reach it.
            Cause evidence, the label trigger and the publication are operator-attested: select checks
            their shape only, and selection.json and its summary label them so. The operator verifies
            the linked records of publication, cause and the labeled/perf event before triggering the
            replacement and before reading measurements; a successful select is not authenticated proof.
  evaluate  Reads selection.json and the downloaded comparison artifacts, checks run and artifact
            identity, selects perf-compare's final `valid` runs, validates each selected result, and
            prints every row with its operands. Exit 0 accepts, 1 rejects, 2 means the evidence is invalid.

The statistics and validation are perf-compare's own. Before anything else both commands read
`scripts/perf-compare.py` and `scripts/perf-critical-path.py` as committed at the evaluated head (`--head`
for select, selection.json's `head_sha` for evaluate) with `git cat-file`, never from a working tree, check
each file's SHA-256 against FROZEN_FILES, copy the verified bytes to a private temp directory and import only
those copies. A missing commit or file, or a changed file, exits 2. perf-compare's `load_gate` is never called. This file's
own SHA-256 is published before any run, and the operator checks it before running it.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from fractions import Fraction
from pathlib import Path
from typing import Callable, Mapping, Sequence

# The SHA-256 of each frozen file, as of the freeze commit. A later commit may not change either file.
PERF_COMPARE_SHA256 = "910982d3ffe72a931996f9d48bda2fadec3902d250b55a1ef535c2e284408cbb"
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
# A git read of one frozen file; the child is killed and reaped at this bound.
GIT_TIMEOUT_S = 60

# The listed external-infrastructure failures: the kind of cause evidence each needs, and the failed steps that
# evidence can be consistent with (None is a job that failed outside every step; a lost runner may fail in any
# step). The step map is only a consistency check; the evidence is the cause. Build, test, comparison,
# cancellation and skip causes are never listed, and `Resolve vcpkg commit` is a local `git rev-parse`.
ANY_STEP = "any step"
INFRASTRUCTURE_CAUSES = {
    "runner-lost": ("runner", ANY_STEP),
    "runner-provisioning": ("runner", frozenset({None, "Set up job"})),
    "actions-transfer": ("http", frozenset({"Upload the binaries", "Upload the build evidence", "Download the binaries",
                                            "Upload the comparison evidence", "Restore vcpkg binaries (Cairo)"})),
    "toolchain-fetch": ("fetch", frozenset({"Install Rust", "Install native dependencies",
                                            "Install Cairo for Windows"})),
}
QUEUE_CATEGORY = "queue"
# What select cannot check against GitHub: it reads run metadata only, so these record fields are shape-checked.
OPERATOR_ATTESTED_STATUS = ("operator-attested, shape-checked only: the operator verifies the linked records of "
                            "publication, cause and the labeled/perf event before triggering the replacement and "
                            "before reading measurements; a successful select is not authenticated proof")
OPERATOR_ATTESTED_FIELDS = {"infrastructure": ["exclusion.jobs[].evidence", "publication"],
                            QUEUE_CATEGORY: ["exclusion.replacement_trigger", "publication"]}
# The result job's own check, which fails whenever a comparison job did not succeed.
RESULT_STEP = "Require every comparison job to succeed"


class EvidenceInvalid(Exception):
    """The evidence cannot be evaluated: exit 2."""


def git_blob(tree: Path, commit: str, relative: str) -> bytes:
    """`relative`'s bytes as committed at `commit` in the repository at `tree`; a missing commit or file is invalid."""
    try:
        completed = subprocess.run(["git", "-C", str(tree), "cat-file", "blob", f"{commit}:{relative}"],
                                   capture_output=True, timeout=GIT_TIMEOUT_S, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise EvidenceInvalid(f"frozen file {relative} cannot be read at {commit}: {error}") from error
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        raise EvidenceInvalid(f"frozen file {relative} cannot be read at {commit}: {detail}")
    return completed.stdout


def load_frozen(tree: Path, commit: object, workdir: Path) -> dict:
    """Verify each frozen file as committed at `commit`, copy the verified bytes into `workdir` and import only
    the copies. A working tree is never read, so a checkout cannot hide a changed commit."""
    if not SHA.fullmatch(str(commit)):
        raise EvidenceInvalid(f"the evaluated head {commit!r} is not a full commit SHA")
    modules = {}
    for relative, expected in FROZEN_FILES.items():
        content = git_blob(tree, str(commit), relative)
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


def head_runs(runner, repository: str, head_sha: str) -> list:
    """Every workflow run on `head_sha`, page by page."""
    listed, page = [], 1
    while True:
        path = f"repos/{repository}/actions/runs?head_sha={head_sha}&per_page={PAGE_SIZE}&page={page}"
        batch = gh_json(runner, ["gh", "api", path])
        if not isinstance(batch, dict) or not isinstance(batch.get("workflow_runs"), list):
            raise EvidenceInvalid(f"head {head_sha}: the runs API returned no run list")
        listed.extend(batch["workflow_runs"])
        if len(batch["workflow_runs"]) < PAGE_SIZE:
            return listed
        page += 1


def read_record(args) -> dict:
    """The operator's record: this head and merge base, the first eligible run (attempt 1), when it was
    recorded, and the one exclusion or null."""
    record = read_json(args.record)
    if not isinstance(record, dict) or record.get("schema_version") != 1:
        raise EvidenceInvalid("the record is not a schema 1 object")
    if (record.get("head_sha"), record.get("merge_base")) != (args.head, args.merge_base):
        raise EvidenceInvalid("the record names another head or merge base")
    first = record.get("first_eligible_run")
    if not isinstance(first, dict) or not isinstance(first.get("run_id"), int) or first.get("attempt") != 1:
        raise EvidenceInvalid("the record's first eligible run must be a run ID at attempt 1")
    if not (record.get("exclusion") is None or isinstance(record.get("exclusion"), dict)):
        raise EvidenceInvalid("the record's exclusion is neither null nor an object")
    return record


def failed_steps(job: Mapping) -> list:
    """The names of a job's failed steps."""
    return [step.get("name") for step in job.get("steps") or []
            if isinstance(step, dict) and step.get("conclusion") == "failure"]


def is_text(value: object) -> bool:
    """Whether `value` is a string with content once stripped; null, booleans, objects and lists never are."""
    return isinstance(value, str) and bool(value.strip())


def check_cause(category: str, entry: Mapping) -> None:
    """The listed job's cause evidence proves `category`: an `actions/*` transfer's HTTP 5xx, a remote fetch that
    failed, or GitHub's runner-loss or provisioning annotation. A step name alone proves nothing."""
    kind, _steps = INFRASTRUCTURE_CAUSES[category]
    evidence = entry.get("evidence")
    if not isinstance(evidence, dict) or evidence.get("kind") != kind:
        raise EvidenceInvalid(f"{entry.get('name')}: a {category} failure needs {kind} cause evidence")
    status = evidence.get("status")
    proven = {
        "http": isinstance(status, int) and not isinstance(status, bool) and 500 <= status <= 599
        and str(evidence.get("source", "")).startswith("actions/"),
        "fetch": str(evidence.get("url", "")).startswith("https://") and is_text(evidence.get("error")),
        "runner": is_text(evidence.get("annotation")),
    }[kind]
    if not proven:
        raise EvidenceInvalid(f"{entry.get('name')}: the {kind} evidence {evidence!r} does not prove {category}")


def check_publication(critical, record: Mapping, recorded_s: int) -> int:
    """When the record was published, on GitHub, no earlier than it was written; the trigger must follow it."""
    publication = record.get("publication")
    if not isinstance(publication, dict) or not str(publication.get("url", "")).startswith("https://github.com/"):
        raise EvidenceInvalid("the record carries no evidence of where it was published before the trigger")
    published_s = critical.parse_time(publication.get("published_at"))
    if published_s < recorded_s:
        raise EvidenceInvalid("the record was published before it was written")
    return published_s


def check_infrastructure(args, runner, critical, view: Mapping, exclusion: Mapping, recorded_s: int,
                         published_s: int) -> dict:
    """An infrastructure exclusion: each listed job failed with evidence of a listed cause at a consistent step,
    every other unsuccessful job follows from a listed failure, and Re-run all jobs of the same run, triggered
    after the record was published, is decisive."""
    if args.run != exclusion["run_id"] or view.get("attempt") != 2:
        raise EvidenceInvalid("an infrastructure replacement is Re-run all jobs: the same run ID, attempt 2")
    _kind, allowed = INFRASTRUCTURE_CAUSES[exclusion["category"]]
    excluded = required_jobs(attempt_jobs(runner, args.repo, args.run, 1), f"excluded run {args.run} attempt 1")
    listed = exclusion.get("jobs")
    if not isinstance(listed, list) or not listed:
        raise EvidenceInvalid("an infrastructure exclusion lists no failed job")
    names = set()
    for entry in listed:
        job = excluded.get(entry.get("name")) if isinstance(entry, dict) else None
        if job is None or job.get("id") != entry.get("id") or entry["name"] in names:
            raise EvidenceInvalid(f"listed job {entry!r} is not one required job of the excluded attempt")
        step = entry.get("failed_step")
        consistent = allowed == ANY_STEP or step in allowed
        if job.get("conclusion") != "failure" or not consistent or failed_steps(job) != ([] if step is None else [step]):
            raise EvidenceInvalid(f"{entry['name']}: {job.get('conclusion')} at {failed_steps(job)} is not "
                                  f"a listed {exclusion['category']} failure")
        check_cause(exclusion["category"], entry)
        names.add(entry["name"])
    for name, job in excluded.items():
        if name in names or job.get("conclusion") == "success":
            continue
        follows_producer = name.startswith("macOS before/after") and job.get("conclusion") == "skipped" \
            and PRODUCER_JOB in names
        follows_result = name == RESULT_JOB and job.get("conclusion") == "failure" and failed_steps(job) == [RESULT_STEP]
        if not (follows_producer or follows_result):
            raise EvidenceInvalid(f"{name}: {job.get('conclusion')} is not explained by a listed failure")
    rerun = required_jobs(attempt_jobs(runner, args.repo, args.run, 2), f"run {args.run} attempt 2")
    inherited = sorted(name for name, job in rerun.items() if job.get("run_attempt") != 2)
    if inherited:
        raise EvidenceInvalid(f"attempt 2 inherited {inherited} instead of re-running all jobs")
    failed_end = max(critical.parse_time(job.get("completed_at")) for job in excluded.values())
    # The rerun's first job exists once the trigger has happened, so the record must precede its creation.
    rerun_created = min(critical.parse_time(job.get("created_at")) for job in rerun.values())
    if not (failed_end <= recorded_s and published_s < rerun_created):
        raise EvidenceInvalid("the exclusion must be recorded after the failed attempt and published before the "
                              "rerun's first job was created")
    return {"used": True, "category": exclusion["category"], "excluded_run": args.run, "excluded_attempt": 1,
            "jobs": sorted(names)}


def queue_counterfactual(critical, created_at: str, jobs: Mapping[str, Mapping], removed: Mapping[str, int]) -> tuple:
    """B through the frozen workflow graph with `removed` seconds of runner queue taken from each job: (B, each
    job's runner-queue window). A job is ready when every job it needs has finished (or at the run's creation);
    its runner queue is S - max(C, ready), perf-critical-path's rule, so dependency wait is never queue. Removing
    queue moves the job and everything after it, and B is the latest required completion."""
    created_s = critical.parse_time(created_at)
    holders = {"producer": [PRODUCER_JOB], "comparison": [name for name in REQUIRED_JOBS
                                                          if name not in (PRODUCER_JOB, RESULT_JOB)]}
    finished, windows = {}, {}
    pending = list(REQUIRED_JOBS)
    while pending:
        progressed = False
        for name in list(pending):
            needed = [holder for role in critical.needs_of(name, "new-design") for holder in holders[role]]
            if any(holder not in finished for holder in needed):
                continue
            job = jobs[name]
            ready_s = max([critical.parse_time(jobs[holder].get("completed_at")) for holder in needed], default=created_s)
            segment = critical.Segment(name, ready_s, ready_s, critical.parse_time(job.get("created_at")),
                                       critical.parse_time(job.get("started_at")),
                                       critical.parse_time(job.get("completed_at")))
            # The caller bounds `removed` by this job's disjoint intervals inside its runner-queue window.
            if segment.runner_queue_s < 0 or segment.runtime_s < 0:
                raise EvidenceInvalid(f"{name}: its times are inconsistent")
            windows[name] = (max(segment.created_s, ready_s), segment.started_s)
            moved_ready_s = max([finished[holder] for holder in needed], default=created_s)
            finished[name] = (moved_ready_s + segment.creation_wait_s + segment.runner_queue_s - removed.get(name, 0)
                              + segment.runtime_s)
            pending.remove(name)
            progressed = True
        if not progressed:
            raise EvidenceInvalid(f"the workflow graph does not resolve: {pending}")
    return max(finished.values()) - created_s, windows


def eligible(runner, critical, repository: str, run_id: object) -> bool:
    """Whether a run on the head is an eligible comparison: its result job took the real name. An ineligible run
    (another label) skips every job and shows the result job's unevaluated name expression."""
    results = [job for job in attempt_jobs(runner, repository, run_id, 1)
               if isinstance(job, dict) and critical.is_result_row(job.get("name"))]
    if len(results) != 1:
        raise EvidenceInvalid(f"run {run_id} lists {len(results)} result jobs, not one")
    return results[0].get("name") == RESULT_JOB


def check_queue(args, runner, critical, view: Mapping, exclusion: Mapping, recorded_s: int,
                published_s: int) -> dict:
    """A queue exclusion: every required job succeeded but B failed, and without the evidenced macOS runner-queue
    intervals the dependency graph meets B; the first later eligible run on the head, triggered by the perf label
    after the record was published, is decisive."""
    excluded_run = exclusion["run_id"]
    if args.run == excluded_run or view.get("attempt") != 1:
        raise EvidenceInvalid("a queue-budget replacement is the first new run ID, attempt 1")
    excluded_view = view_run(runner, args.repo, excluded_run)
    check_identity(excluded_view, excluded_run, args.head)
    if excluded_view.get("attempt") != 1:
        raise EvidenceInvalid(f"the excluded run is at attempt {excluded_view.get('attempt')}, not 1")
    found = required_jobs(attempt_jobs(runner, args.repo, excluded_run, 1), f"excluded run {excluded_run}")
    unsuccessful = sorted(name for name, job in found.items() if job.get("conclusion") != "success")
    if unsuccessful:
        raise EvidenceInvalid(f"a queue-budget replacement needs every excluded job to succeed: {unsuccessful}")
    budget = budget_of(critical, excluded_view.get("createdAt"), found)
    if not budget["measurable"] or budget["within"] or budget["elapsed_s"] != exclusion.get("elapsed_s"):
        raise EvidenceInvalid(f"the excluded run's B {budget} does not match a recorded over-budget run")
    # With nothing removed the walk reproduces every job's own finish, so only the queue windows are needed here.
    _measured_s, windows = queue_counterfactual(critical, excluded_view.get("createdAt"), found, {})
    spans, removed = {}, {}
    for entry in exclusion.get("intervals") or []:
        job = found.get(entry.get("name")) if isinstance(entry, dict) else None
        if job is None or job.get("id") != entry.get("id") or not entry["name"].startswith("macOS"):
            raise EvidenceInvalid(f"interval {entry!r} is not a required macOS job of the excluded run")
        start, end = critical.parse_time(entry.get("start")), critical.parse_time(entry.get("end"))
        window_start, window_end = windows[entry["name"]]
        if not window_start <= start < end <= window_end:
            raise EvidenceInvalid(f"interval {entry!r} is outside {entry['name']}'s runner-queue window")
        spans.setdefault(entry["name"], []).append((start, end))
        removed[entry["name"]] = removed.get(entry["name"], 0) + end - start
    for name, intervals in spans.items():
        intervals.sort()
        if any(later[0] < earlier[1] for earlier, later in zip(intervals, intervals[1:])):
            raise EvidenceInvalid(f"{name}'s queue intervals overlap")
    if not spans:
        raise EvidenceInvalid("the queue exclusion lists no interval")
    counterfactual_s, _windows = queue_counterfactual(critical, excluded_view.get("createdAt"), found, removed)
    if counterfactual_s != exclusion.get("counterfactual_s") or counterfactual_s > critical.BUDGET_S:
        raise EvidenceInvalid(f"without the queue intervals the run takes {counterfactual_s} s; the record says "
                              f"{exclusion.get('counterfactual_s')} and B is {critical.BUDGET_S} s")
    finished_s = max(critical.parse_time(job.get("completed_at")) for job in found.values())
    if not (finished_s <= recorded_s and published_s < critical.parse_time(view.get("createdAt"))):
        raise EvidenceInvalid("the exclusion must be recorded after the excluded run and published before its "
                              "replacement")
    trigger = exclusion.get("replacement_trigger")
    if not (isinstance(trigger, dict) and trigger.get("action") == "labeled" and trigger.get("label") == "perf"
            and str(trigger.get("url", "")).startswith("https://github.com/")):
        raise EvidenceInvalid(f"the replacement must be triggered by adding the perf label: {trigger!r}")
    excluded_created = critical.parse_time(excluded_view.get("createdAt"))
    later = sorted((critical.parse_time(run.get("created_at")), run.get("id"))
                   for run in head_runs(runner, args.repo, args.head)
                   if isinstance(run, dict) and run.get("name") == WORKFLOW_NAME and run.get("event") == EVENT
                   and critical.parse_time(run.get("created_at")) > excluded_created)
    first_eligible = next((run_id for _created, run_id in later if eligible(runner, critical, args.repo, run_id)), None)
    if first_eligible != args.run:
        raise EvidenceInvalid(f"run {args.run} is not the first later eligible {WORKFLOW_NAME} run on the head")
    return {"used": True, "category": QUEUE_CATEGORY, "excluded_run": excluded_run, "excluded_attempt": 1,
            "excluded_budget": budget, "queued_s": sum(removed.values()), "counterfactual_s": counterfactual_s}


def check_replacement(args, runner, critical, view: Mapping, record: Mapping) -> dict:
    """The single replacement allowance: unused, so the decisive run is the recorded first eligible run, or
    used once, on that run's first execution, for a listed infrastructure failure or a queue overrun."""
    first, exclusion = record["first_eligible_run"], record["exclusion"]
    if exclusion is None:
        if (args.run, view.get("attempt")) != (first["run_id"], 1):
            raise EvidenceInvalid(f"run {args.run} attempt {view.get('attempt')} is not the recorded first "
                                  f"eligible run {first['run_id']} attempt 1, and no exclusion is recorded")
        return {"used": False}
    if (exclusion.get("run_id"), exclusion.get("attempt")) != (first["run_id"], 1):
        raise EvidenceInvalid("only the first eligible run's first execution may be excluded, once")
    try:
        recorded_s = critical.parse_time(record.get("recorded_at"))
        published_s = check_publication(critical, record, recorded_s)
    except critical.AccountingError as error:
        raise EvidenceInvalid(f"the record's times are unreadable: {error}") from error
    if exclusion.get("category") in INFRASTRUCTURE_CAUSES:
        replacement = check_infrastructure(args, runner, critical, view, exclusion, recorded_s, published_s)
        kind = "infrastructure"
    elif exclusion.get("category") == QUEUE_CATEGORY:
        replacement = check_queue(args, runner, critical, view, exclusion, recorded_s, published_s)
        kind = QUEUE_CATEGORY
    else:
        raise EvidenceInvalid(f"exclusion category {exclusion.get('category')!r} is not listed")
    # These fields are the operator's word; selection.json says so rather than reading as GitHub-verified.
    replacement["operator_attested"] = {"fields": OPERATOR_ATTESTED_FIELDS[kind], "status": OPERATOR_ATTESTED_STATUS}
    return replacement


def select(args, runner, critical) -> dict:
    """Build selection.json from the operator's record and run metadata alone."""
    if not SHA.fullmatch(args.head or "") or not SHA.fullmatch(args.merge_base or ""):
        raise EvidenceInvalid("--head and --merge-base must be full 40-character SHAs")
    record = read_record(args)
    view = view_run(runner, args.repo, args.run)
    check_identity(view, args.run, args.head)
    attempt = view.get("attempt")
    if not isinstance(attempt, int) or attempt < 1:
        raise EvidenceInvalid(f"run {args.run} reports attempt {attempt!r}")
    replacement = check_replacement(args, runner, critical, view, record)
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
        "first_eligible_run": record["first_eligible_run"],
        "record": record,
        "replacement": replacement,
    }


# --- evaluate: identity, population, validation ----------------------------------------------

@dataclass
class SelectedRun:
    """One selected attempt: its directory and the documents it carries."""

    directory: Path
    outcome: dict
    result: dict
    # The same result.json with every decimal read as an exact Fraction; the rows read only these operands.
    exact: dict


def read_json(path: Path) -> object:
    """A JSON document from an artifact; unreadable evidence is invalid."""
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvidenceInvalid(f"{path} cannot be read: {error}") from error


def read_exact(path: Path) -> object:
    """A JSON document whose decimals are exact Fractions, so a threshold compares the values as written."""
    try:
        return json.loads(path.read_text(encoding="utf-8"), parse_float=Fraction)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
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
        if not all(math.isfinite(value) for value in compare.latency_values(latency)):
            raise EvidenceInvalid(f"{where}: an attributed latency sample is not finite")


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
                run = SelectedRun(directory, outcome, result, read_exact(directory / "scratch" / "result.json"))
                try:
                    # Shape and schema failures stay inside this catch, so a base counters run only notes them.
                    if not isinstance(result, dict) or not isinstance(run.exact, dict):
                        raise EvidenceInvalid(f"{directory}: result.json is not an object")
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


def exact_number(value: object) -> Fraction | None:
    """`value` as an exact Fraction when it is a finite number; else None, which fails its row."""
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, Fraction)):
        return Fraction(value)
    if isinstance(value, float) and math.isfinite(value):
        return Fraction(value)
    return None


def shown(value: Fraction) -> str:
    """An exact operand as the report prints it."""
    return f"{float(value):.4f}"


def presented_fps(run: SelectedRun, scenario: str) -> Fraction | None:
    """The relevant phase's presented frames per second, exactly."""
    phase = relevant_phase(run.exact, scenario)
    frames, start, end = (exact_number(phase.get(key)) for key in ("presented_frames", "start_unix_s", "end_unix_s"))
    if frames is None or start is None or end is None or end == start:
        return None
    return frames / (end - start)


def throughput_mbps(run: SelectedRun) -> Fraction | None:
    """The run's throughput in MB/s, exactly, from its integer bytes and its seconds."""
    throughput = run.exact["throughput"]
    count, seconds = exact_number(throughput.get("bytes")), exact_number(throughput.get("seconds"))
    if count is None or seconds is None or seconds == 0:
        return None
    return count / seconds / 1_000_000


def exact_latencies(run: SelectedRun) -> list:
    """The run's attributed latencies, exactly; a non-finite one reads None and fails the row."""
    values = []
    for sample in run.exact["latency"]["samples"]:
        value = sample.get("latency_ms") if isinstance(sample, dict) else sample
        if isinstance(sample, dict) and value is None:
            continue
        values.append(exact_number(value))
    return values


def coverage_of(selected: Sequence[SelectedRun]) -> tuple[int, int] | None:
    """Pooled (attributed, total); None when no sample was counted, which never meets the coverage bound."""
    attributed = sum(run.result["latency"]["attributed"] for run in selected)
    total = sum(run.result["latency"]["total"] for run in selected)
    return (attributed, total) if total > 0 else None


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
        passed = head.median >= A1_MEDIAN_FACTOR * base.median and head.minimum > base.median
        rows.append(Row("A1", passed, f"head median {shown(head.median)} vs 2 x base median {shown(base.median)}; "
                                      f"head min {shown(head.minimum)} vs base median {shown(base.median)}"))

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
        passed = head.median >= G1_MEDIAN_FACTOR * base.median and head.median >= base.minimum
        rows.append(Row(f"G1 {platform}", passed,
                        f"head median {shown(head.median)} MB/s vs 0.9 x base median {shown(base.median)} "
                        f"and base min {shown(base.minimum)}"))

    windows_fps = summaries(compare, by_side("Windows", "S3-default", lambda run: presented_fps(run, "S3-default")))
    if windows_fps is None:
        rows.append(Row("G2", False, "a selected run lacks a finite fps"))
    else:
        base, head = windows_fps["base"], windows_fps["head"]
        rows.append(Row("G2", head.median >= base.minimum,
                        f"head median {shown(head.median)} vs base min {shown(base.minimum)}"))

    for platform in PLATFORM_NAMES:
        coverage, pooled = {}, {}
        for side in ("base", "head"):
            selected = runs(platform, "S2-flood", "timed", side)
            coverage[side] = coverage_of(selected)
            pooled[side] = [value for run in selected for value in exact_latencies(run)]
        accepted = compare.latency_acceptance(coverage["base"], coverage["head"])
        if any(value is None for values in pooled.values() for value in values):
            rows.append(Row(f"G3 {platform}", False, "a selected run has a non-finite latency"))
            continue
        base_p95, head_p95 = compare.percentile_95(pooled["base"]), compare.percentile_95(pooled["head"])
        passed = accepted and head_p95 <= G3_P95_FACTOR * base_p95
        rows.append(Row(f"G3 {platform}", passed,
                        f"coverage base {coverage['base']}, head {coverage['head']} (acceptance {accepted}); "
                        f"head p95 {shown(head_p95)} vs 1.1 x base p95 {shown(base_p95)}"))

    for platform in PLATFORM_NAMES:
        stream = summaries(compare, by_side(platform, "S4-default", lambda run: presented_fps(run, "S4-default")))
        if stream is None:
            rows.append(Row(f"G4 {platform}", False, "a selected run lacks a finite fps"))
            continue
        base, head = stream["base"], stream["head"]
        rows.append(Row(f"G4 {platform}", head.median >= base.minimum,
                        f"head median {shown(head.median)} vs base min {shown(base.minimum)}"))

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
    chooser.add_argument("--record", type=Path, required=True,
                         help="the operator's record of the first eligible run and any exclusion, written before "
                              "a replacement was triggered")
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
            # The frozen files are read at the evaluated head: --head for select, selection.json's for evaluate.
            selection_document = None if args.command == "select" else read_json(args.selection)
            if args.command == "select":
                commit = args.head
            else:
                commit = selection_document.get("head_sha") if isinstance(selection_document, dict) else None
            modules = load_frozen(args.tree, commit, Path(workdir))
            compare, critical = modules[COMPARE], modules[CRITICAL]
            try:
                if args.command == "select":
                    selection = select(args, runner or critical.run_bounded, critical)
                    args.output.write_text(json.dumps(selection, indent=2, sort_keys=True) + "\n", encoding="utf-8")
                    print(f"selected run {selection['run_id']} attempt {selection['attempt']}; "
                          f"B {selection['budget']}; replacement {selection['replacement']}")
                    attested = selection["replacement"].get("operator_attested")
                    if attested:
                        print(f"operator-attested {attested['fields']}: {attested['status']}")
                    return EXIT_ACCEPT
                rows, report = evaluate(compare, critical, selection_document, args.artifacts)
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
