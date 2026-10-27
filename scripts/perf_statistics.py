"""Perf statistics."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_settings


# --- Statistics -----------------------------------------------------------------------------

def median(values: Sequence[float]) -> float:
    """Return the middle value of a nonempty sample set, or the mean of its two middle values."""
    if not values:
        raise ValueError("median of no samples")
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def nearest_rank(values: Sequence[float], percent: int) -> float:
    """Return the nearest-rank percentile: the observed sample at rank ceil(percent * n / 100)."""
    if not values:
        raise ValueError("percentile of no samples")
    ordered = sorted(values)
    # Integer arithmetic keeps the ceiling exact; 0.95 has no exact binary value.
    rank = max(1, (percent * len(ordered) + 99) // 100)
    return ordered[rank - 1]


def percentile_95(values: Sequence[float]) -> float:
    """Return the nearest-rank 95th percentile of a nonempty sample set."""
    return nearest_rank(values, 95)


@dataclass(frozen=True)
class FrameSummary:
    """Pooled frame statistics plus the per-run spread that serves as the noise floor."""

    median: float
    percentile_95: float
    run_median_range: tuple[float, float]
    run_p95_range: tuple[float, float]
    runs: int
    samples: int


def frame_summary(per_run: Iterable[Sequence[float]]) -> FrameSummary | None:
    """Pool frame samples across valid runs; the spread is the min-max of per-run medians and p95s.

    The extremes of the pooled frames are never the spread: one slow frame in one run
    would otherwise read as noise in every comparison. Runs without samples add nothing.
    """
    runs = [list(samples) for samples in per_run if samples]
    if not runs:
        return None
    pooled = [value for samples in runs for value in samples]
    medians = [median(samples) for samples in runs]
    tails = [percentile_95(samples) for samples in runs]
    return FrameSummary(median(pooled), percentile_95(pooled), (min(medians), max(medians)),
                        (min(tails), max(tails)), len(runs), len(pooled))


@dataclass(frozen=True)
class RunSummary:
    """The median and min-max of one value per valid run."""

    median: float
    minimum: float
    maximum: float
    runs: int


def run_summary(values: Iterable[float | None]) -> RunSummary | None:
    """Summarize a run-level metric; runs that lack the value are left out, never read as zero."""
    present = [value for value in values if value is not None]
    if not present:
        return None
    return RunSummary(median(present), min(present), max(present), len(present))


# --- Run order ------------------------------------------------------------------------------

class AbbaSchedule:
    """Order one scenario's runs A B B A until both sides have their valid runs.

    An invalid run does not count; its side runs again in its next slot. A side whose
    invalid runs exceed `retry_limit` stops the scenario. A side that has its valid runs
    gives its slots to the other, so the order stays A B B A while both sides run.
    """

    PATTERN = ("base", "head", "head", "base")

    def __init__(self, target_runs: int, retry_limit: int = perf_settings.RETRY_LIMIT) -> None:
        self.target_runs = target_runs
        self.retry_limit = retry_limit
        self.valid_runs = {side: 0 for side in perf_settings.SIDES}
        self.invalid_runs = {side: 0 for side in perf_settings.SIDES}
        self.failed_side: str | None = None
        self.retired: set[str] = set()
        self._slot = 0

    @property
    def complete(self) -> bool:
        """Whether every side that still runs has its valid runs."""
        return all(side in self.retired or self.valid_runs[side] >= self.target_runs for side in perf_settings.SIDES)

    def retire(self, side: str) -> None:
        """Stop scheduling a blocked or exhausted side; the other side keeps every slot."""
        self.retired.add(side)
        if self.failed_side == side:
            self.failed_side = None

    def next_side(self) -> str | None:
        """Return the side that runs next, or None once the scenario is complete or stopped."""
        if self.failed_side is not None or self.complete:
            return None
        while True:
            side = self.PATTERN[self._slot % len(self.PATTERN)]
            self._slot += 1
            if side not in self.retired and self.valid_runs[side] < self.target_runs:
                return side

    def record(self, side: str, valid: bool) -> None:
        """Count one finished run; a fourth invalid run on a side stops the scenario."""
        if valid:
            self.valid_runs[side] += 1
            return
        self.invalid_runs[side] += 1
        if self.invalid_runs[side] > self.retry_limit:
            self.failed_side = side
