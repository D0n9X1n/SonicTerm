#!/usr/bin/env python3
"""Account for a Performance comparison run's time, from its creation to its last attempt's finish.

The PR pipeline's budget is 30 minutes measured that way, queue included. This report reads the run, each
attempt and each attempt's job rows and artifacts with `gh api` (or a recorded `--fixture`), and splits
the time into the attempts and the waits between them, then, inside each attempt, each chain of needs
into sibling wait, creation wait, runner queue and runtime classes. A comparison job's compare step is
split further by the `timing.json` perf-compare.py writes, in runs that have it.

Rerun rows. GitHub lists every job in every attempt. A row that started before its attempt did was
inherited: it keeps its executed origin's times, runner and steps under a new id and creation time.
Each inherited row maps to the one executed row of an earlier attempt with the same name, start,
finish, runner and conclusion; no match, or two, stops the report. Skipped rows are on no path.

Evidence. A run whose rows include `Performance comparison result`, or whose evidence artifacts end in
`-<attempt>`, is new-design: each comparison job that ran its compare step must have that attempt's
artifact with a `timing.json` naming this run, attempt, job and shard, its marks inside the step. Any
other run is historical: the evidence is the one same-name artifact created inside the job's window,
and the compare step stays one class.

Exit 0 within budget, 1 over it, 2 when the rows do not reconcile or cannot be read. The author runs
this for the PR's evidence; perf.yml does not.
"""

from __future__ import annotations

import argparse
import calendar
from dataclasses import dataclass, field
import io
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
from typing import Callable, Mapping, Sequence
import zipfile

DEFAULT_REPOSITORY = "D0n9X1n/SonicTerm"
BUDGET_S = 30 * 60
# Timestamps are whole seconds, so sums and window checks allow one second of rounding.
TOLERANCE_S = 1
GH_TIMEOUT_S = 120
PAGE_SIZE = 100
PRODUCER = "macOS perf binaries (base and head)"
RESULT = "Performance comparison result"
COMPARE_STEP = "Compare the base and the head"
TIMING_FILE = "timing.json"
TIMING_MARKS = ("start", "listed", "measure_start", "measure_end", "report_written")
_COMPARISON = re.compile(r"(macOS|Windows) before/after comparison \((.+)\)")
_STAMP = re.compile(r"(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?Z")
# A new-design evidence name ends in its attempt; a shard name never ends in bare digits after a dash.
_ATTEMPT_SUFFIX = re.compile(r"perf-comparison-.+-\d+")

# The runtime classes, in report order; together they are a job's start-to-finish time.
CLASSES = ("setup", "build", "package+upload", "download+extract", "compare", "evidence", "check", "teardown",
           "gap")
STEP_CLASSES = {
    "Set up job": "setup", "Install Rust": "setup", "Install native dependencies": "setup",
    "Resolve vcpkg commit": "setup", "Restore vcpkg binaries (Cairo)": "setup", "Install Cairo for Windows": "setup",
    "Choose the refs and the run length": "setup", "Relax the release profile for a pull request": "setup",
    "Check the producer's refs": "setup",
    "Build both refs once": "build",
    "Package the binaries": "package+upload", "Upload the binaries": "package+upload",
    "Download the binaries": "download+extract", "Unpack the binaries": "download+extract",
    COMPARE_STEP: "compare",
    "Publish the table in the job summary": "evidence", "Upload the comparison evidence": "evidence",
    "Upload the build evidence": "evidence",
    "Require every comparison job to succeed": "check",
    "Complete job": "teardown",
}


class AccountingError(Exception):
    """The run's rows, artifacts or timing do not reconcile, so no report is printed."""


def parse_time(text: object) -> int:
    """Seconds since the epoch of a GitHub UTC timestamp; fractions are dropped."""
    match = _STAMP.fullmatch(str(text))
    if match is None:
        raise AccountingError(f"unreadable timestamp {text!r}")
    return calendar.timegm(tuple(int(part) for part in match.groups()) + (0, 0, 0))


def fingerprint(data: Mapping) -> tuple:
    """What an inherited row shares with its executed origin."""
    return (data.get("name"), data.get("started_at"), data.get("completed_at"), data.get("runner_id"),
            data.get("conclusion"))


@dataclass
class Row:
    """One job row of one attempt: `skipped`, `executed` in that attempt, or `inherited` from `origin`."""

    kind: str
    attempt: int
    data: dict
    origin: "Row | None" = None

    @property
    def name(self) -> str:
        """The job's rendered name."""
        return self.data["name"]

    @property
    def created_s(self) -> int:
        """When this row was created."""
        return parse_time(self.data["created_at"])

    @property
    def started_s(self) -> int:
        """When the job started on its runner."""
        return parse_time(self.data["started_at"])

    @property
    def finished_s(self) -> int:
        """When the job finished."""
        return parse_time(self.data["completed_at"])

    @property
    def runtime_s(self) -> int:
        """F - S: the job's time on its runner."""
        return self.finished_s - self.started_s


def attempt_numbers(record: Mapping) -> list[int]:
    """The run's attempts, which must be 1..n."""
    numbers = sorted(int(number) for number in record["attempts"])
    if numbers != list(range(1, len(numbers) + 1)):
        raise AccountingError(f"attempts {numbers} are not 1..{len(numbers)}")
    return numbers


def _check_order(row: Row) -> None:
    """created <= started <= completed, and every step inside the job in order; executed rows only."""
    if not row.created_s <= row.started_s <= row.finished_s:
        raise AccountingError(f"attempt {row.attempt} {row.name}: created, started and completed are out of order")
    previous_s = row.started_s
    for item in row.data.get("steps") or []:
        # When: a step never started (a cancelled job), it has no times to order.
        if not item.get("started_at") or not item.get("completed_at"):
            continue
        started_s, finished_s = parse_time(item["started_at"]), parse_time(item["completed_at"])
        if (started_s + TOLERANCE_S < previous_s or finished_s < started_s
                or finished_s > row.finished_s + TOLERANCE_S):
            raise AccountingError(f"attempt {row.attempt} {row.name}: step {item.get('name')!r} is out of order")
        previous_s = finished_s


def resolve_rows(record: Mapping) -> dict[int, list[Row]]:
    """Classify every row of every attempt and map each inherited row to its one executed origin."""
    resolved: dict[int, list[Row]] = {}
    for number in attempt_numbers(record):
        attempt_start_s = parse_time(record["attempts"][str(number)]["run_started_at"])
        rows = []
        for data in record["jobs"][str(number)]:
            if data.get("conclusion") == "skipped":
                rows.append(Row("skipped", number, data))
                continue
            if not data.get("started_at") or not data.get("completed_at"):
                raise AccountingError(f"attempt {number} {data.get('name')}: the job has not finished")
            if parse_time(data["started_at"]) >= attempt_start_s:
                row = Row("executed", number, data)
                row.origin = row
                _check_order(row)
                rows.append(row)
                continue
            # Only rows that executed in an earlier attempt are candidates, never another inherited copy.
            candidates = [earlier for previous in range(1, number) for earlier in resolved[previous]
                          if earlier.kind == "executed" and fingerprint(earlier.data) == fingerprint(data)]
            if len(candidates) != 1:
                count = "no executed origin" if not candidates else f"{len(candidates)} executed origins"
                raise AccountingError(f"attempt {number} {data.get('name')}: inherited row has {count}")
            rows.append(Row("inherited", number, data, candidates[0]))
        resolved[number] = rows
    return resolved


def evidence_mode(record: Mapping) -> str:
    """`new-design` when any attempt lists the result row or an evidence name carries its attempt."""
    for rows in record["jobs"].values():
        if any(data.get("name") == RESULT for data in rows):
            return "new-design"
    if any(_ATTEMPT_SUFFIX.fullmatch(item["name"]) for item in record["artifacts"]):
        return "new-design"
    return "historical"


def needs_of(name: str, mode: str) -> tuple[str, ...]:
    """Which job roles a job needs: the producer, `comparison`, or nothing; an unknown job is an error."""
    if name == RESULT:
        return ("producer", "comparison")
    if name == PRODUCER:
        return ()
    match = _COMPARISON.fullmatch(name)
    if match is None:
        raise AccountingError(f"unknown job {name!r}")
    return ("producer",) if match.group(1) == "macOS" and mode == "new-design" else ()


def _role(name: str) -> str:
    """A job's role in the graph: producer, result or comparison."""
    if name == PRODUCER:
        return "producer"
    if name == RESULT:
        return "result"
    return "comparison"


def _compare_step(row: Row) -> dict | None:
    """A comparison row's compare step, or None when the row lists none."""
    found = [item for item in row.data.get("steps") or [] if item.get("name") == COMPARE_STEP]
    if len(found) > 1:
        raise AccountingError(f"attempt {row.attempt} {row.name}: {len(found)} compare steps")
    return found[0] if found else None


@dataclass
class Evidence:
    """A comparison job's evidence artifact, with its timing.json in new-design runs."""

    artifact: dict
    timing: dict | None

    def __getitem__(self, key: str) -> object:
        return self.artifact[key]


def bind_evidence(record: Mapping, rows: Mapping[int, list[Row]], mode: str,
                  read_timing: Callable[[str], dict | None]) -> dict[tuple[int, str], Evidence]:
    """Bind each executed comparison row that ran its compare step to its one evidence artifact."""
    run_id = str(record["run"]["id"])
    bound: dict[tuple[int, str], Evidence] = {}
    for number, attempt_rows in rows.items():
        for row in attempt_rows:
            match = _COMPARISON.fullmatch(row.name)
            if row.kind != "executed" or match is None:
                continue
            platform_name, shard = match.groups()
            compare = _compare_step(row)
            # When: the shard skipped its comparison (a first release), it produced no evidence to bind.
            if compare is not None and compare.get("conclusion") == "skipped":
                continue
            where = f"attempt {number} {row.name}"
            if mode == "new-design":
                if compare is None:
                    raise AccountingError(f"{where}: no compare step for timing.json's marks")
                suffix = f"-{platform_name}-{shard}-{number}"
                found = [item for item in record["artifacts"]
                         if item["name"].startswith("perf-comparison-") and item["name"].endswith(suffix)]
                if len(found) != 1:
                    raise AccountingError(f"{where}: {len(found)} evidence artifacts end {suffix}")
                timing = read_timing(found[0]["name"])
                if timing is None:
                    raise AccountingError(f"{where}: {found[0]['name']} has no timing.json")
                _check_timing(timing, run_id, number, row.name, shard, compare, where)
                bound[(number, row.name)] = Evidence(found[0], timing)
            else:
                suffix = f"-{platform_name}-{shard}"
                found = [item for item in record["artifacts"]
                         if item["name"].startswith("perf-comparison-") and item["name"].endswith(suffix)
                         and row.started_s <= parse_time(item["created_at"]) <= row.finished_s + TOLERANCE_S]
                if len(found) != 1:
                    raise AccountingError(f"{where}: {len(found)} artifacts ending {suffix} inside its window")
                bound[(number, row.name)] = Evidence(found[0], None)
    return bound


def _check_timing(timing: Mapping, run_id: str, attempt: int, name: str, shard: str, compare: Mapping,
                  where: str) -> None:
    """timing.json must name this run, attempt, job and shard, with ordered marks inside the compare step."""
    expected = {"run_id": run_id, "run_attempt": str(attempt), "job": name, "shard": shard}
    for key, value in expected.items():
        if str(timing.get(key)) != value:
            raise AccountingError(f"{where}: timing.json {key} {timing.get(key)!r} is not {value!r}")
    marks = timing.get("marks")
    if not isinstance(marks, dict) or not all(isinstance(marks.get(mark), (int, float)) for mark in TIMING_MARKS):
        raise AccountingError(f"{where}: timing.json marks are missing {TIMING_MARKS}")
    values = [marks[mark] for mark in TIMING_MARKS]
    started_s, finished_s = parse_time(compare["started_at"]), parse_time(compare["completed_at"])
    if values != sorted(values):
        raise AccountingError(f"{where}: timing.json marks are out of order")
    # The step's times are whole seconds, so a mark may lie up to a second past either end.
    if values[0] < started_s - TOLERANCE_S or values[-1] > finished_s + TOLERANCE_S:
        raise AccountingError(f"{where}: timing.json marks lie outside the compare step")


@dataclass
class Segment:
    """One job on a chain: P (previous finish), R (needs ready), C (created), S (started), F (finished)."""

    name: str
    previous_s: int
    ready_s: int
    created_s: int
    started_s: int
    finished_s: int
    classes: dict = field(default_factory=dict)
    # prepare/scenarios/report/other from timing.json, or None when the run has none.
    compare: dict | None = None
    compare_note: str = ""

    @property
    def sibling_wait_s(self) -> int:
        """R - P: waiting on another need after this chain's previous job finished."""
        return self.ready_s - self.previous_s

    @property
    def creation_wait_s(self) -> int:
        """D - R, D = max(C, R): waiting for GitHub to create the job after its needs were done."""
        return max(self.created_s, self.ready_s) - self.ready_s

    @property
    def runner_queue_s(self) -> int:
        """S - D: waiting for a runner."""
        return self.started_s - max(self.created_s, self.ready_s)

    @property
    def runtime_s(self) -> int:
        """F - S."""
        return self.finished_s - self.started_s


@dataclass
class ChainPath:
    """A chain of needs from a job with no executed need to the attempt's terminal job."""

    segments: list

    @property
    def names(self) -> list[str]:
        """The jobs, first to terminal."""
        return [segment.name for segment in self.segments]

    @property
    def slack_s(self) -> int:
        """The chain's sibling waits: zero on the critical path."""
        return sum(segment.sibling_wait_s for segment in self.segments)

    @property
    def total_s(self) -> int:
        """F(terminal) - A_k, as the four intervals of every segment sum to it."""
        return sum(segment.sibling_wait_s + segment.creation_wait_s + segment.runner_queue_s + segment.runtime_s
                   for segment in self.segments)


@dataclass
class AttemptAccount:
    """One attempt's window [A_k, Z_k], its chains, its critical path and its tail Z_k - F(terminal)."""

    number: int
    start_s: int
    end_s: int
    paths: list
    critical: ChainPath | None
    tail_s: int
    executed: list


def classify(row: Row, evidence: Evidence | None, mode: str) -> tuple[dict, dict | None, str]:
    """Split a job's runtime into classes, and its compare step by timing.json when it has one."""
    steps = row.data.get("steps") or []
    if not steps:
        raise AccountingError(f"attempt {row.attempt} {row.name} has no steps to classify")
    classes = {name: 0 for name in CLASSES}
    for item in steps:
        name = item.get("name", "")
        if not item.get("started_at") or not item.get("completed_at"):
            continue
        if name in STEP_CLASSES:
            kind = STEP_CLASSES[name]
        elif name.startswith("Run actions/checkout@"):
            kind = "setup"
        elif name.startswith("Post "):
            kind = "teardown"
        else:
            raise AccountingError(f"attempt {row.attempt} {row.name}: unclassified step {name!r}")
        classes[kind] += parse_time(item["completed_at"]) - parse_time(item["started_at"])
    classes["gap"] = row.runtime_s - sum(classes.values())
    # Step and job times round separately, so the gap may read -1 s; anything lower means overlapping steps.
    if classes["gap"] < -TOLERANCE_S:
        raise AccountingError(f"attempt {row.attempt} {row.name}: steps sum past the job's runtime")
    compare, note = None, ""
    if evidence is not None and evidence.timing is not None:
        marks = evidence.timing["marks"]
        compare = {"prepare": marks["listed"] - marks["start"],
                   "scenarios": marks["measure_end"] - marks["measure_start"],
                   "report": marks["report_written"] - marks["measure_end"]}
        compare["other"] = classes["compare"] - sum(compare.values())
        compare = {key: round(value) for key, value in compare.items()}
    elif _COMPARISON.fullmatch(row.name) and mode == "historical":
        note = "unavailable (historical run: no timing.json)"
    return classes, compare, note


def account_attempt(number: int, start_s: int, end_s: int, rows: list[Row], mode: str,
                    evidence: Mapping[tuple[int, str], Evidence]) -> AttemptAccount:
    """Chains of needs among the jobs executed in one attempt; inherited needs count as done at A_k."""
    executed = {row.name: row for row in rows if row.kind == "executed"}
    present = {row.name: row for row in rows if row.kind != "skipped"}
    if not executed:
        return AttemptAccount(number, start_s, end_s, [], None, end_s - start_s, [])

    def executed_needs(name: str) -> list[Row]:
        roles = needs_of(name, mode)
        return [executed[other] for other in sorted(executed) if other != name and _role(other) in roles]

    def ready_s(name: str) -> int:
        return max([start_s] + [need.finished_s for need in executed_needs(name)])

    for name in present:
        needs_of(name, mode)
    if RESULT in executed:
        terminal = RESULT
    else:
        latest_s = max(row.finished_s for row in executed.values())
        terminal = min(name for name, row in executed.items() if row.finished_s == latest_s)

    def chains(name: str) -> list[list[str]]:
        needs = executed_needs(name)
        if not needs:
            return [[name]]
        return [chain + [name] for need in needs for chain in chains(need.name)]

    def segment(chain: list[str], position: int) -> Segment:
        row = executed[chain[position]]
        previous_s = start_s if position == 0 else executed[chain[position - 1]].finished_s
        part = Segment(row.name, previous_s, ready_s(row.name), row.created_s, row.started_s, row.finished_s)
        if part.runner_queue_s < 0:
            raise AccountingError(f"attempt {number} {row.name}: runner_queue {part.runner_queue_s} s is negative")
        part.classes, part.compare, part.compare_note = classify(row, evidence.get((number, row.name)), mode)
        return part

    paths = [ChainPath([segment(chain, position) for position in range(len(chain))]) for chain in chains(terminal)]
    terminal_s = executed[terminal].finished_s
    for path in paths:
        if abs(path.total_s - (terminal_s - start_s)) > TOLERANCE_S:
            raise AccountingError(f"attempt {number}: chain {path.names} sums to {path.total_s} s, "
                                  f"not {terminal_s - start_s} s")
    tail_s = end_s - terminal_s
    if tail_s < -TOLERANCE_S:
        raise AccountingError(f"attempt {number}: {terminal} finished after the attempt ended")
    # The critical path walks back through the latest-finishing executed need, ties to the first name.
    walk = [terminal]
    while executed_needs(walk[0]):
        needs = executed_needs(walk[0])
        latest_s = max(need.finished_s for need in needs)
        walk.insert(0, min(need.name for need in needs if need.finished_s == latest_s))
    critical = next(path for path in paths if path.names == walk)
    if critical.slack_s > TOLERANCE_S:
        raise AccountingError(f"attempt {number}: the critical path {walk} has {critical.slack_s} s of slack")
    return AttemptAccount(number, start_s, end_s, paths, critical, tail_s, sorted(executed.values(),
                                                                                  key=lambda row: row.finished_s))


@dataclass
class Budget:
    """elapsed = Z_last - created(run) = first wait + the attempts' spans + the waits between them."""

    elapsed_s: int
    first_wait_s: int
    spans_s: list
    rerun_waits_s: list


def budget(record: Mapping) -> Budget:
    """Partition the run's elapsed time into each attempt's Z_k - A_k and each rerun_wait A_{k+1} - Z_k."""
    numbers = attempt_numbers(record)
    attempts = [record["attempts"][str(number)] for number in numbers]
    created_s = parse_time(record["run"]["created_at"])
    starts = [parse_time(attempt["run_started_at"]) for attempt in attempts]
    ends = [parse_time(attempt["updated_at"]) for attempt in attempts]
    spans = [end_s - start_s for start_s, end_s in zip(starts, ends)]
    waits = [starts[index + 1] - ends[index] for index in range(len(ends) - 1)]
    if any(value < 0 for value in spans + waits) or starts[0] < created_s:
        raise AccountingError(f"attempt windows overlap: starts {starts}, ends {ends}")
    result = Budget(ends[-1] - created_s, starts[0] - created_s, spans, waits)
    if result.first_wait_s + sum(spans) + sum(waits) != result.elapsed_s:
        raise AccountingError("the attempts and rerun waits do not sum to the elapsed time")
    return result


@dataclass
class Report:
    """Everything the text report prints."""

    run_id: str
    mode: str
    budget: Budget
    attempts: list
    evidence: dict


def account(record: Mapping, read_timing: Callable[[str], dict | None]) -> Report:
    """Resolve the rows, bind the evidence and account for every attempt; raise if anything disagrees."""
    rows = resolve_rows(record)
    mode = evidence_mode(record)
    evidence = bind_evidence(record, rows, mode, read_timing)
    totals = budget(record)
    attempts = []
    for number in sorted(rows):
        attempt = record["attempts"][str(number)]
        attempts.append(account_attempt(number, parse_time(attempt["run_started_at"]),
                                        parse_time(attempt["updated_at"]), rows[number], mode, evidence))
    return Report(str(record["run"]["id"]), mode, totals, attempts, evidence)


def render(report: Report) -> str:
    """The report as text: the budget line, each attempt's critical path and runtime classes, other paths."""
    totals = report.budget
    verdict = "within" if totals.elapsed_s <= BUDGET_S else "OVER"
    lines = [f"Run {report.run_id} ({report.mode} evidence)",
             f"Elapsed, creation to last finish: {totals.elapsed_s} s ({verdict} the {BUDGET_S} s budget)",
             f"  queue before attempt 1: {totals.first_wait_s} s"]
    for index, span_s in enumerate(totals.spans_s):
        lines.append(f"  attempt {index + 1}: {span_s} s")
        if index < len(totals.rerun_waits_s):
            lines.append(f"  rerun_wait {index + 1}→{index + 2}: {totals.rerun_waits_s[index]} s")
    for attempt in report.attempts:
        lines.append("")
        lines.append(f"Attempt {attempt.number}")
        if attempt.critical is None:
            lines.append("  no job executed in this attempt")
            lines.append(f"  attempt_tail: {attempt.tail_s} s")
            continue
        lines.append(f"  critical path (slack {attempt.critical.slack_s} s): {' → '.join(attempt.critical.names)}")
        for part in attempt.critical.segments:
            lines.append(f"    {part.name}: sibling_wait {part.sibling_wait_s} s, creation_wait "
                         f"{part.creation_wait_s} s, runner_queue {part.runner_queue_s} s, runtime {part.runtime_s} s")
            lines.append("      " + ", ".join(f"{name} {part.classes[name]} s" for name in CLASSES
                                              if part.classes.get(name)))
            if part.compare is not None:
                lines.append("      compare: " + ", ".join(f"{name} {value} s" for name, value in part.compare.items()))
            elif part.compare_note:
                lines.append(f"      compare prepare/scenarios/report: {part.compare_note}")
        lines.append(f"  attempt_tail: {attempt.tail_s} s")
        others = [path for path in attempt.paths if path is not attempt.critical]
        for path in sorted(others, key=lambda path: path.slack_s):
            lines.append(f"  other path (slack {path.slack_s} s): {' → '.join(path.names)}")
        on_path = {name for path in attempt.paths for name in path.names}
        terminal_s = attempt.critical.segments[-1].finished_s
        for row in attempt.executed:
            if row.name not in on_path:
                lines.append(f"  off the paths: {row.name} finished {terminal_s - row.finished_s} s before "
                             f"the terminal job (queue {row.started_s - max(row.created_s, attempt.start_s)} s, "
                             f"runtime {row.runtime_s} s)")
    return "\n".join(lines) + "\n"


def macos_timeline(records: Sequence[Mapping]) -> list[tuple[int, int, str]]:
    """Every start and finish of a macOS-labelled job across runs, with how many ran at once afterwards.

    Evidence of contention between the perf and CI runs, not of any account quota.
    """
    seen, events = set(), []
    for record in records:
        for rows in record["jobs"].values():
            for data in rows:
                labels = " ".join(data.get("labels") or []).lower()
                if (data.get("conclusion") == "skipped" or not data.get("started_at")
                        or not data.get("completed_at") or "macos" not in labels):
                    continue
                key = fingerprint(data)
                # When: a rerun lists an inherited copy, its job ran once, so it is counted once.
                if key in seen:
                    continue
                seen.add(key)
                events.append((parse_time(data["started_at"]), 1, f"start {data['name']}"))
                events.append((parse_time(data["completed_at"]), -1, f"end {data['name']}"))
    timeline, running = [], 0
    # A finish and a start in the same second: the finish first, so the count is not overstated.
    for moment_s, change, label in sorted(events, key=lambda event: (event[0], event[1])):
        running += change
        timeline.append((moment_s, running, label))
    return timeline


def render_timeline(timeline: Sequence[tuple[int, int, str]]) -> str:
    """The timeline as text, one event per line in UTC."""
    import time as time_module
    lines = ["macOS jobs running across the runs (contention evidence, not a quota):"]
    for moment_s, running, label in timeline:
        lines.append(f"  {time_module.strftime('%H:%M:%S', time_module.gmtime(moment_s))} {running} {label}")
    peak = max((running for _moment, running, _label in timeline), default=0)
    lines.append(f"  most at once: {peak}")
    return "\n".join(lines) + "\n"


def run_bounded(argv: Sequence[str], timeout_s: int = GH_TIMEOUT_S) -> bytes:
    """Run a command with a deadline; on timeout kill its whole process tree and reap it."""
    options: dict = {"stdout": subprocess.PIPE, "stderr": subprocess.PIPE}
    # When: on POSIX the child leads its own session, so the deadline can signal everything it started.
    if os.name != "nt":
        options["start_new_session"] = True
    else:
        options["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
    process = subprocess.Popen(list(argv), **options)
    try:
        stdout, stderr = process.communicate(timeout=timeout_s)
    except subprocess.TimeoutExpired:
        if os.name != "nt":
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass  # The group exited between the deadline and the kill.
        else:
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(process.pid)], capture_output=True, check=False,
                           timeout=30)
        process.kill()
        process.communicate()
        raise AccountingError(f"{' '.join(argv)} timed out after {timeout_s} s")
    if process.returncode != 0:
        raise AccountingError(f"{' '.join(argv)} exited {process.returncode}: "
                              f"{stderr.decode('utf-8', 'replace').strip()}")
    return stdout


def _gh_json(runner: Callable[[Sequence[str]], bytes], path: str) -> object:
    """One `gh api` read, as JSON."""
    return json.loads(runner(["gh", "api", path]).decode("utf-8"))


def _gh_pages(runner: Callable[[Sequence[str]], bytes], path: str, key: str) -> list:
    """Every item of a paginated list, page by page until a short page."""
    items, page = [], 1
    while True:
        batch = _gh_json(runner, f"{path}?per_page={PAGE_SIZE}&page={page}")[key]
        items.extend(batch)
        if len(batch) < PAGE_SIZE:
            return items
        page += 1


def fetch_record(repository: str, run_id: str, runner: Callable[[Sequence[str]], bytes] = run_bounded) -> dict:
    """Read a run, each attempt, each attempt's jobs and the artifacts; nothing is written to GitHub."""
    base = f"repos/{repository}/actions/runs/{run_id}"
    run = _gh_json(runner, base)
    record = {"run": run, "attempts": {}, "jobs": {}, "timing": {}}
    for number in range(1, int(run["run_attempt"]) + 1):
        record["attempts"][str(number)] = _gh_json(runner, f"{base}/attempts/{number}")
        record["jobs"][str(number)] = _gh_pages(runner, f"{base}/attempts/{number}/jobs", "jobs")
    record["artifacts"] = _gh_pages(runner, f"{base}/artifacts", "artifacts")
    return record


def live_timing(repository: str, record: Mapping,
                runner: Callable[[Sequence[str]], bytes] = run_bounded) -> Callable[[str], dict | None]:
    """Read an artifact's timing.json from its zip; None when the artifact holds none."""
    def read(name: str) -> dict | None:
        found = [item for item in record["artifacts"] if item["name"] == name]
        if len(found) != 1:
            raise AccountingError(f"{len(found)} artifacts are named {name}")
        archive = runner(["gh", "api", f"repos/{repository}/actions/artifacts/{found[0]['id']}/zip"])
        with zipfile.ZipFile(io.BytesIO(archive)) as bundle:
            members = [member for member in bundle.namelist() if member.rsplit("/", 1)[-1] == TIMING_FILE]
            if len(members) != 1:
                return None
            return json.loads(bundle.read(members[0]).decode("utf-8"))
    return read


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    """`--run ID` reads GitHub; `--fixture FILE` reads a recorded run."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--run", metavar="ID", help="the Performance comparison run to account for")
    source.add_argument("--fixture", type=Path, metavar="FILE",
                        help="a recorded run: run, attempts, jobs per attempt, artifacts and timing by artifact name")
    parser.add_argument("--repo", default=DEFAULT_REPOSITORY, help=f"owner/name (default: {DEFAULT_REPOSITORY})")
    parser.add_argument("--ci-run", metavar="ID",
                        help="also print the macOS jobs' concurrency across this CI run and the perf run")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    """Print the report; exit 0 within budget, 1 over it, 2 when the run does not reconcile."""
    args = parse_args(argv)
    try:
        if args.fixture is not None:
            record = json.loads(args.fixture.read_text(encoding="utf-8"))
            timing = record.get("timing") or {}
            read_timing = timing.get
        else:
            record = fetch_record(args.repo, args.run)
            read_timing = live_timing(args.repo, record)
        report = account(record, read_timing)
        print(render(report), end="")
        if args.ci_run:
            print()
            print(render_timeline(macos_timeline([record, fetch_record(args.repo, args.ci_run)])), end="")
    except (AccountingError, OSError, ValueError, KeyError) as error:
        print(f"perf-critical-path: {error}", file=sys.stderr)
        return 2
    return 0 if report.budget.elapsed_s <= BUDGET_S else 1


if __name__ == "__main__":
    raise SystemExit(main())
