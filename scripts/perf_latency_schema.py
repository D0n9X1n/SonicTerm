"""Perf latency schema."""

from __future__ import annotations
import math



def _is_int(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _is_number(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _numbers(value: object) -> bool:
    return isinstance(value, list) and all(_is_number(item) for item in value)


def latency_values(latency: object) -> list[float] | None:
    """Return the attributed latencies, or None when the samples are malformed.

    A sample is a number, or an object whose `latency_ms` is a number or null; null marks an
    unattributed sample, which is never credited to any frame.
    """
    samples = latency.get("samples") if isinstance(latency, dict) else None
    if not isinstance(samples, list):
        return None
    values = []
    for sample in samples:
        value = sample.get("latency_ms") if isinstance(sample, dict) else sample
        if isinstance(sample, dict) and value is None:
            continue
        if not _is_number(value):
            return None
        values.append(value)
    return values


# Every split reason of a credited sample, in the harness's precedence order; record.rs's SPLIT_REASONS.
SPLIT_REASONS = (
    "unsupported", "arm-gate-off", "arm-no-pane", "arm-exhausted", "take-gate-off", "take-no-pane",
    "take-mismatch", "take-already-taken", "pane-not-shown", "row-identity-changed",
    "first-appearance-unobserved", "no-appearance-observed", "echo-overwritten",
    "no-publication-observed-by-take", "untargeted", "other-window", "send-refused",
    "delivery-not-observed-by-take", "presented-before-publication", "delivered-after-present", "clock-order",
    "split",
)
NOT_CREDITED = "not-credited"
# The three parts of a split, in milliseconds, which add up to the sample's latency.
SPLIT_PARTS = ("input_to_parse_ms", "parse_to_publication_ms", "publication_to_present_ms")
SPLIT_DELIVERIES = ("sent", "suppressed")
# Serialized parts are rounded per part, so their sum may differ from latency_ms by this much.
SPLIT_SUM_TOLERANCE_MS = 0.001
SPLIT_COVERAGE_TOLERANCE = 1e-9
# Keys a schema-1 sample and report must carry even when their value is null; absent is not null.
SPLIT_SAMPLE_KEYS = ("latency_ms", "split", "split_reason")
SPLIT_REPORT_KEYS = ("split_schema", "split_count", "split_reasons", "split_coverage")


def _finite_nonnegative(value: object) -> bool:
    return _is_number(value) and math.isfinite(value) and value >= 0


def _split_problem(split: object, latency_ms: float) -> str | None:
    """Why one split object breaks the contract, or None."""
    if not isinstance(split, dict):
        return "is not an object"
    for part in SPLIT_PARTS + ("delivery_lag_us",):
        if not _finite_nonnegative(split.get(part)):
            return f"{part} is not a finite number >= 0"
    if split.get("delivery") not in SPLIT_DELIVERIES:
        return f"delivery is {split.get('delivery')!r}, not sent or suppressed"
    for flag in ("coalesced", "sync_open"):
        if not isinstance(split.get(flag), bool):
            return f"{flag} is not a boolean"
    if not (_is_int(split.get("echo_generation")) and split["echo_generation"] >= 0):
        return "echo_generation is not an integer >= 0"
    total = sum(split[part] for part in SPLIT_PARTS)
    if abs(total - latency_ms) > SPLIT_SUM_TOLERANCE_MS:
        return f"parts sum to {total} ms, not latency_ms {latency_ms} within {SPLIT_SUM_TOLERANCE_MS} ms"
    return None


def latency_split_problems(latency: dict) -> list[str]:
    """Every way a schema-1 latency object breaks the split contract; the report's aggregates are recomputed
    from the samples, so mutually consistent but fabricated counts are refused too."""
    problems = [f"{key} is missing" for key in SPLIT_REPORT_KEYS if key not in latency]
    if not (_is_int(latency.get("split_schema")) and latency["split_schema"] == 1):
        problems.append(f"split_schema is {latency.get('split_schema')!r}, not 1")
    samples = latency.get("samples")
    if not isinstance(samples, list):
        return problems + ["samples is not a list"]
    credited_count, reasons = 0, {}
    for position, sample in enumerate(samples):
        if not isinstance(sample, dict):
            problems.append(f"sample {position} is not an object")
            continue
        missing = [key for key in SPLIT_SAMPLE_KEYS if key not in sample]
        if missing:
            # When: a key the schema allows to be null is absent, the sample's contract is incomplete.
            problems.append(f"sample {position} is missing {', '.join(missing)}")
            continue
        reason, latency_ms, split = sample.get("split_reason"), sample.get("latency_ms"), sample.get("split")
        credited = _finite_nonnegative(latency_ms)
        if not credited and latency_ms is not None:
            problems.append(f"sample {position} latency_ms is neither null nor a finite number >= 0")
            continue
        if reason not in SPLIT_REASONS and reason != NOT_CREDITED:
            problems.append(f"sample {position} split_reason {reason!r} is not a known reason")
            continue
        if credited == (reason == NOT_CREDITED):
            problems.append(f"sample {position} is {'credited' if credited else 'uncredited'} but reads {reason!r}")
            continue
        if (split is not None) != (reason == "split"):
            problems.append(f"sample {position} split is {'absent' if split is None else 'present'} for {reason!r}")
            continue
        if split is not None:
            problem = _split_problem(split, latency_ms)
            if problem:
                problems.append(f"sample {position} split {problem}")
        if credited:
            credited_count += 1
            reasons[reason] = reasons.get(reason, 0) + 1
    split_count = reasons.get("split", 0)
    if not (_is_int(latency.get("attributed")) and latency["attributed"] == credited_count):
        problems.append(f"attributed is {latency.get('attributed')!r}, but {credited_count} samples are credited")
    recorded = latency.get("split_reasons")
    if not (isinstance(recorded, dict) and all(_is_int(count) for count in recorded.values())
            and recorded == reasons):
        problems.append(f"split_reasons is {recorded!r}, but the samples give {reasons!r}")
    if not (_is_int(latency.get("split_count")) and latency["split_count"] == split_count):
        problems.append(f"split_count is {latency.get('split_count')!r}, but {split_count} samples are split")
    coverage = latency.get("split_coverage")
    if credited_count == 0:
        if coverage is not None:
            problems.append(f"split_coverage is {coverage!r}, but nothing is credited, so it must be null")
    elif not (_is_number(coverage)
              and abs(coverage - split_count / credited_count) <= SPLIT_COVERAGE_TOLERANCE):
        problems.append(f"split_coverage is {coverage!r}, not {split_count}/{credited_count}")
    return problems
