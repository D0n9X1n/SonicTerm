"""Perf cell layout."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
from fractions import Fraction
import json
from pathlib import Path
import platform
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_memory


# The cell-layout decision: whether storing grid cells in 16 bytes instead of 24 is worth its cost. It reads S12's
# `end` checkpoint on the head side of a CI comparison, and goes only when grid cells are a large enough share of
# resident memory on one platform. Missing or partial evidence is inconclusive, never a no-go.
CELL_LAYOUT_PLATFORMS = ("macOS", "Windows")
CELL_LAYOUT_RUNS = 5
CELL_LAYOUT_PANES = 3
CELL_LAYOUT_CHECKPOINT = "end"
CELL_LAYOUT_MIN_SHARE = Fraction(3, 10)
CELL_LAYOUT_MIN_SAVING_BYTES = 8 * 1024 * 1024
CELL_LAYOUT_GRID_FIELDS = ("grid_visible_bytes", "grid_history_bytes", "grid_alternate_bytes")
# The shards whose artifacts hold S12 on each platform: macOS runs it in S9-S10sync, and its artifacts from the
# S10-S11release, S2-S3-S8-S12 and S1-S3-S6-S8-S12 layouts still read.
CELL_LAYOUT_SHARDS = {
    "macOS": ("S9-S10sync", "S10-S11release", "S2-S3-S8-S12", "S1-S3-S6-S8-S12"),
    "Windows": ("S1-S3-S6-S8-S12",),
}
# `perf-comparison-<pr>-<head sha>-<platform>-<shard>-<attempt>`, as perf.yml names each comparison artifact.
CELL_LAYOUT_ARTIFACT = re.compile(r"perf-comparison-\d+-(?P<head>[0-9a-f]{40})-(?P<platform>macOS|Windows)-"
                                  r"(?P<shard>.+)-(?P<attempt>\d+)")
ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")


@dataclass(frozen=True)
class CellLayoutRun:
    """One head S12 run's inputs to the cell-layout decision."""

    name: str
    # The CI workflow run the evidence came from; runs from two workflow runs are never pooled.
    workflow_run: str
    result: dict | None
    memory: list[MemorySample]
    # perf-compare's own classification of the run; anything but `valid` was rejected and replaced.
    classification: str = "valid"
    # The head commit the evidence measured; runs of two heads are never pooled.
    head_sha: str = ""
    # Why the evidence could not be read or does not belong to the requested run; None when it does.
    problem: str | None = None


@dataclass(frozen=True)
class CellLayoutReading:
    """A run's grid and resident bytes at `end`, or why the run cannot be used."""

    name: str
    grid_bytes: int | None = None
    resident_bytes: int | None = None
    problem: str | None = None


@dataclass(frozen=True)
class CellLayoutPlatform:
    """One platform's readings, its median share and saving, and whether it passes; `passes` is None when it
    has no decidable statistic."""

    platform: str
    readings: tuple[CellLayoutReading, ...]
    share: Fraction | None = None
    saving_bytes: Fraction | None = None
    passes: bool | None = None
    failed_clauses: tuple[str, ...] = ()
    reasons: tuple[str, ...] = ()


@dataclass(frozen=True)
class CellLayoutDecision:
    """`Go`, `NoGo` or `Inconclusive`, with every platform's statistics and, when inconclusive, why."""

    outcome: str
    platforms: dict[str, CellLayoutPlatform]
    reasons: tuple[str, ...] = ()


def cell_layout_reading(run: CellLayoutRun) -> CellLayoutReading:
    """Read `end`'s authoritative tagged sample of one run, or name the reason the run is invalid."""
    def invalid(problem: str) -> CellLayoutReading:
        return CellLayoutReading(run.name, problem=problem)

    if run.problem is not None:
        return invalid(run.problem)
    if run.classification != "valid":
        return invalid(f"run classified {run.classification}")
    if run.result is None:
        return invalid("no result.json")
    if not isinstance(run.result, dict):
        return invalid("result.json is not an object")
    if run.result.get("checkpoint_memory") != "supported":
        return invalid("checkpoint sampling unsupported")
    checkpoints = run.result.get("checkpoints")
    if not isinstance(checkpoints, list):
        return invalid("result.json checkpoints is not a list")
    point = next((point for point in checkpoints
                  if isinstance(point, dict) and point.get("label") == CELL_LAYOUT_CHECKPOINT), None)
    if point is None or not isinstance(point.get("index"), int):
        return invalid("no end checkpoint")
    if point.get("sampling") == "exhausted":
        return invalid("partial (sampling exhausted)")
    reading = perf_memory.checkpoint_memory(run.memory, point["index"])
    if reading is None:
        return invalid("no tagged end sample")
    if reading.problem is not None:
        return invalid(reading.problem)
    if reading.partial:
        return invalid("partial")
    sample = reading.sample
    for name in ("panes_total", "panes_sampled"):
        value = getattr(sample, name)
        if value != CELL_LAYOUT_PANES:
            return invalid(f"{name}={value}")
    if sample.panes_contended != 0:
        return invalid(f"panes_contended={sample.panes_contended}")
    if sample.process_resident_bytes is None:
        return invalid("process_resident_bytes unsupported")
    if sample.process_resident_bytes <= 0:
        return invalid(f"process_resident_bytes={sample.process_resident_bytes}")
    missing = [name for name in CELL_LAYOUT_GRID_FIELDS if getattr(sample, name) is None]
    if missing:
        return invalid(f"missing {', '.join(missing)}")
    grid_bytes = sum(getattr(sample, name) for name in CELL_LAYOUT_GRID_FIELDS)
    return CellLayoutReading(run.name, grid_bytes, sample.process_resident_bytes)


def cell_layout_platform(platform: str, runs: Sequence[CellLayoutRun]) -> CellLayoutPlatform:
    """Judge one platform: its median per-run share and saving, from exactly five valid runs of one workflow run."""
    readings = tuple(cell_layout_reading(run) for run in runs)
    reasons = [f"{platform} {reading.name}: {reading.problem}" for reading in readings if reading.problem]
    valid = [reading for reading in readings if reading.problem is None]
    workflow_runs = sorted({run.workflow_run for run in runs})
    if len(workflow_runs) > 1:
        reasons.append(f"{platform}: runs from {len(workflow_runs)} workflow runs ({', '.join(workflow_runs)})")
    heads = sorted({run.head_sha for run in runs})
    if len(heads) > 1:
        reasons.append(f"{platform}: runs of {len(heads)} head commits ({', '.join(heads)})")
    names = [run.name for run in runs]
    repeated = sorted({name for name in names if names.count(name) > 1})
    if repeated:
        # The same run counted twice is one measurement, not two.
        reasons.append(f"{platform}: runs repeated ({', '.join(repeated)})")
    if len(valid) != CELL_LAYOUT_RUNS:
        reasons.append(f"{platform}: {len(valid)} valid runs, need exactly {CELL_LAYOUT_RUNS}")
    if len(valid) != CELL_LAYOUT_RUNS or len(workflow_runs) > 1 or len(heads) > 1 or repeated:
        return CellLayoutPlatform(platform, readings, reasons=tuple(reasons))
    # The median of the per-run values, in exact fractions: never a ratio of medians.
    shares = sorted(Fraction(reading.grid_bytes, reading.resident_bytes) for reading in valid)
    savings = sorted(Fraction(reading.grid_bytes * 8, 24) for reading in valid)
    share, saving = shares[CELL_LAYOUT_RUNS // 2], savings[CELL_LAYOUT_RUNS // 2]
    failed = tuple(clause for clause, holds in (("share", share >= CELL_LAYOUT_MIN_SHARE),
                                                ("size", saving >= CELL_LAYOUT_MIN_SAVING_BYTES)) if not holds)
    return CellLayoutPlatform(platform, readings, share, saving, not failed, failed, tuple(reasons))


def cell_layout_decision(runs_by_platform: Mapping[str, Sequence[CellLayoutRun]]) -> CellLayoutDecision:
    """Go when one platform passes both clauses; NoGo only when both platforms are decidable and neither passes;
    otherwise Inconclusive, naming every missing or rejected run."""
    platforms = {platform: cell_layout_platform(platform, runs_by_platform.get(platform, ()))
                 for platform in CELL_LAYOUT_PLATFORMS}
    if any(judged.passes for judged in platforms.values()):
        # One platform's own five runs, from one workflow run and head, decide a go on their own.
        return CellLayoutDecision("Go", platforms)
    reasons = [reason for judged in platforms.values() for reason in judged.reasons]
    every_run = [run for platform in CELL_LAYOUT_PLATFORMS for run in runs_by_platform.get(platform, ())]
    provenance = sorted({(run.workflow_run, run.head_sha) for run in every_run})
    if len(provenance) > 1:
        # A no-go needs both platforms, so both must be one measurement of one head.
        reasons.append("platforms measured in different workflow runs or heads: "
                       + "; ".join(f"run {workflow} head {head or '?'}" for workflow, head in provenance))
    if all(judged.passes is False for judged in platforms.values()) and len(provenance) == 1:
        return CellLayoutDecision("NoGo", platforms)
    return CellLayoutDecision("Inconclusive", platforms, tuple(reasons))


def _read_json_object(path: Path) -> tuple[object, str | None]:
    """Read one JSON file; returns (value, None), or (None, why) when it is missing or cannot be parsed."""
    if not path.is_file():
        return None, f"{path.name} missing"
    try:
        return json.loads(path.read_text(encoding="utf-8")), None
    except (OSError, ValueError, RecursionError) as error:
        # ValueError covers JSONDecodeError, UnicodeDecodeError and an over-long integer; RecursionError, deep nesting.
        return None, f"{path.name} unreadable: {error}"


def _cell_layout_artifact_problem(artifact: Path, head_sha: str, workflow_run: str) -> str | None:
    """Why an S12-holding artifact is not evidence of the requested workflow run and head, or None."""
    named = CELL_LAYOUT_ARTIFACT.fullmatch(artifact.name)
    if named["head"] != head_sha:
        return f"artifact measured head {named['head']}, expected {head_sha}"
    timing, problem = _read_json_object(artifact / "timing.json")
    if problem is not None:
        return problem
    if not isinstance(timing, dict):
        return "timing.json is not an object"
    if str(timing.get("run_id")) != workflow_run:
        return f"artifact from workflow run {timing.get('run_id')}, expected {workflow_run}"
    if timing.get("shard") != named["shard"]:
        return f"artifact shard {timing.get('shard')}, expected {named['shard']}"
    return None


def _cell_layout_memory(run_dir: Path) -> tuple[list[perf_memory.MemorySample], str | None]:
    """The App's memory lines from its own log files, or from the colored console output when those were not kept;
    returns ([], why) when a log cannot be read or a memory line cannot be parsed."""
    try:
        memory = perf_memory.read_memory_samples(run_dir / "scratch" / "logs")
        harness_log = run_dir / "01-harness.log"
        if not memory and harness_log.is_file():
            # When: the App's log files were not kept, its lines are read from the colored console output.
            lines = (ANSI_ESCAPE.sub("", line)
                     for line in harness_log.read_text(encoding="utf-8", errors="replace").splitlines())
            memory = sorted((sample for sample in map(perf_memory.parse_memory_line, lines) if sample),
                            key=lambda sample: sample.unix_s)
    except (OSError, ValueError, OverflowError) as error:
        # An unreadable log or an impossible timestamp makes this run invalid, not the whole decision.
        return [], f"memory log unreadable: {error}"
    return memory, None


def read_cell_layout_runs(artifact_root: Path, workflow_run: str, head_sha: str) -> dict[str, list[CellLayoutRun]]:
    """Read every head S12 timed run of one workflow run and head from downloaded `perf-comparison-*` artifacts.

    Only each platform's S12-holding shard (`CELL_LAYOUT_SHARDS`) is read. Each run carries the workflow run and head its artifact
    records, so evidence from another run or head is named as such rather than counted; a file that cannot be read
    or that describes another side, scenario or variant makes that run invalid with the reason.
    """
    runs: dict[str, list[CellLayoutRun]] = {platform: [] for platform in CELL_LAYOUT_PLATFORMS}
    try:
        artifacts = sorted(artifact_root.iterdir())
    except OSError:
        # An unavailable download holds no evidence: every platform then has no valid runs and is inconclusive.
        return runs
    for artifact in artifacts:
        named = CELL_LAYOUT_ARTIFACT.fullmatch(artifact.name)
        if (named is None or named["shard"] not in CELL_LAYOUT_SHARDS.get(named["platform"], ())
                or not artifact.is_dir()):
            continue
        artifact_problem = _cell_layout_artifact_problem(artifact, head_sha, workflow_run)
        timing, _ = _read_json_object(artifact / "timing.json")
        recorded_run = str(timing.get("run_id")) if isinstance(timing, dict) else "?"
        for run_dir in sorted(artifact.glob("runs/S12-default/timed/*-head")):
            outcome, problem = _read_json_object(run_dir / "outcome.json")
            if problem is None and not isinstance(outcome, dict):
                problem = "outcome.json is not an object"
            if problem is None:
                identity = (outcome.get("side"), outcome.get("scenario"), outcome.get("variant"))
                if identity != ("head", "S12", "default"):
                    problem = f"outcome.json describes side={identity[0]} scenario={identity[1]} variant={identity[2]}"
            result, result_problem = _read_json_object(run_dir / "scratch" / "result.json")
            classification = outcome.get("kind", "missing") if isinstance(outcome, dict) else "missing"
            memory, memory_problem = _cell_layout_memory(run_dir)
            runs[named["platform"]].append(CellLayoutRun(
                f"{artifact.name}/{run_dir.name}", recorded_run, result, memory, classification, named["head"],
                artifact_problem or problem
                or (result_problem if result_problem != "result.json missing" else None) or memory_problem))
    return runs


def memory_at(samples: Sequence[perf_memory.MemorySample], unix_s: float,
              fresh_after_unix_s: float | None = None) -> perf_memory.MemorySample | None:
    """Return the latest sample at or before a checkpoint, or None when memory is unavailable.

    With `fresh_after_unix_s` the sample must also be taken at or after that time: an older one
    predates what the checkpoint measures, so the reading is unavailable, never a stale figure.
    """
    earlier = [sample for sample in samples if sample.unix_s <= unix_s]
    if not earlier:
        return None
    latest = earlier[-1]
    if fresh_after_unix_s is not None and latest.unix_s < fresh_after_unix_s:
        # When: the latest sample predates fresh_after_unix_s, it cannot show the state being measured.
        return None
    return latest
