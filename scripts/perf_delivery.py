"""Perf delivery."""

from __future__ import annotations
import json
from pathlib import Path
import re
import shutil
import sys
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_build_policy
import perf_latency_schema
import perf_metrics
import perf_outcomes
import perf_scenarios
import perf_sets
import perf_settings
# What smoke_main exports to the job when a replay was retried, so CI uploads a passing smoke's evidence.
REPLAY_RETRIED_ENV = "SONICTERM_PERF_REPLAY_RETRIED"
# A replay attempt's evidence file name, with its attempt number.
REPLAY_ATTEMPT_FILE = re.compile(r"delivery-.+-attempt(\d+)\.(?:json|txt|log)")


class DeliveryOutcome(NamedTuple):
    """A replay's result: the record it trusts (or None), the reason it blocks (or None), and its disclosure.

    `note` states the attempt count and each retried attempt's detail; None means no attempt was disclosed.
    """

    record: dict | None
    problem: str | None
    note: str | None = None


def delivery_replayed(scenario_id: str, platform_name: str) -> bool:
    """Whether a comparison on `platform_name` replays `scenario_id`'s delivery before its runs."""
    return platform_name == "win32" and scenario_id in perf_metrics.DELIVERY_SCENARIOS


def capture_delivery_argv(binary: Path, scenario_id: str, variant: str, scratch: Path, *,
                          short: bool = False) -> tuple[str, ...]:
    """Return the harness's delivery replay command line, which writes `delivery.json` into `scratch`."""
    flags = ("--short",) if short else ()
    return (str(binary), "--run", scenario_id, "--variant", variant, *flags, "--capture-delivery", str(scratch))


def _delivery_check_ok(check: object) -> bool:
    """Whether `check` has the shape the harness writes: a name, a boolean verdict and a detail."""
    return (isinstance(check, Mapping) and isinstance(check.get("name"), str) and isinstance(check.get("ok"), bool)
            and isinstance(check.get("detail"), str))


def frame_check_problem(check: Mapping, variant: str) -> str | None:
    """Why a schema 2 `sync brackets` check is malformed; None when its fields, detail and verdict all agree.

    `unseen` and `brackets` are non-negative ints, `unseen_markers` lists min(unseen, 8) plain strings, the detail
    is exactly sync_check's classification with the same unpainted count, and `ok` is sync_check's rule.
    """
    unseen, brackets, markers = check.get("unseen"), check.get("brackets"), check.get("unseen_markers")
    if not (perf_latency_schema._is_int(unseen) and unseen >= 0 and perf_latency_schema._is_int(brackets) and brackets >= 0):
        return "unseen and brackets are not non-negative integers"
    if not isinstance(markers, list) or len(markers) != min(unseen, perf_metrics.UNSEEN_MARKER_LIMIT):
        return f"unseen_markers does not list {min(unseen, perf_metrics.UNSEEN_MARKER_LIMIT)} markers"
    if not all(isinstance(marker, str) and marker and "`" not in marker and "|" not in marker for marker in markers):
        # When: a marker is empty or would break the table's code span or cell, the record is not the harness's.
        return "unseen_markers holds a marker that is not plain text"
    parsed = perf_metrics.FRAME_DETAIL.fullmatch(check["detail"])
    if parsed is None:
        return "the detail is not a frame classification"
    enclosed, empty_pair_ahead, _absent, painted = parsed.groups()
    if (painted is None) != (unseen == 0) or (painted is not None and int(painted) != unseen):
        return "the detail's never-painted count disagrees with unseen"
    if brackets == 0 and (int(enclosed) or int(empty_pair_ahead)):
        return "the detail places frames in brackets that never arrived"
    if variant not in ("default", "sync"):
        return f"S10 has no variant {variant}"
    if check["ok"] != (unseen == 0 and (variant == "sync" or brackets == 0)):
        return "the verdict disagrees with the counts"
    return None


def read_delivery(scratch: Path, scenario_id: str, variant: str) -> tuple[dict | None, str | None]:
    """Read a replay's record: (record, None) when every check passed; otherwise the reason the scenario is blocked.

    A failed check returns the record with its reason, so the table still shows every check's detail; an
    unreadable or malformed record returns no record. A schema 2 S10 record is validated whole, its frame check's
    structured fields included, before it can be admitted or retried.
    """
    try:
        record = json.loads((scratch / perf_metrics.DELIVERY_FILE).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None, f"{perf_metrics.DELIVERY_FILE} is missing from {scratch}"
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        return None, f"{perf_metrics.DELIVERY_FILE} is unreadable: {error}"
    version = record.get("schema_version") if isinstance(record, dict) else None
    if not (perf_latency_schema._is_int(version) and version in perf_metrics.DELIVERY_SCHEMA_VERSIONS):
        # When: the version is a float, a bool or another number, the record is not one this script reads.
        versions = " or ".join(str(version) for version in perf_metrics.DELIVERY_SCHEMA_VERSIONS)
        return None, f"{perf_metrics.DELIVERY_FILE} has no schema_version {versions}"
    if record.get("scenario") != scenario_id or record.get("variant") != variant:
        return None, (f"{perf_metrics.DELIVERY_FILE} names {record.get('scenario')}/{record.get('variant')}, "
                      f"not {scenario_id}/{variant}")
    checks = record.get("checks")
    if not isinstance(checks, list) or not checks or not all(_delivery_check_ok(check) for check in checks):
        return None, f"{perf_metrics.DELIVERY_FILE} has no well-formed checks"
    bytes_kept = record.get("bytes_kept")
    if not (perf_latency_schema._is_int(bytes_kept) and bytes_kept >= 0):
        return None, f"{perf_metrics.DELIVERY_FILE} is malformed: bytes_kept is not a non-negative integer"
    if version == perf_metrics.DELIVERY_SCHEMA_VERSION:
        frame_checks = [check for check in checks if check["name"] == perf_metrics.RETRYABLE_DELIVERY_CHECK]
        if scenario_id == "S10" and len(frame_checks) != 1:
            return None, f"{perf_metrics.DELIVERY_FILE} is malformed: S10 has {len(frame_checks)} {perf_metrics.RETRYABLE_DELIVERY_CHECK} checks"
        for check in frame_checks:
            malformed = frame_check_problem(check, variant)
            if malformed is not None:
                return None, f"{perf_metrics.DELIVERY_FILE} is malformed: {perf_metrics.RETRYABLE_DELIVERY_CHECK}: {malformed}"
    for check in checks:
        if not check["ok"]:
            return record, f"delivery check failed: {check['name']}: {check['detail']}"
    return record, None


def delivery_rows(label: str, record: Mapping | None, problem: str | None,
                  note: str | None = None) -> list[list[str]]:
    """One row per replay check, both sides holding the shared replay's detail; a problem fills the note.

    `note` is the replay's attempt disclosure: a passed row shows it, and a blocked row appends it to the reason.
    """
    blocked = None if problem is None else f"blocked: {problem}" + (f"; {note}" if note else "")
    if record is None:
        return [[label, "delivery", "blocked", "blocked", blocked]]
    rows = [[label, f"delivery: {check['name']}", check["detail"], check["detail"], note or perf_metrics.DELIVERY_NOTE]
            for check in record["checks"]]
    if blocked is not None:
        # When: a check failed, each row's note names the reason the scenario is blocked.
        for row in rows:
            row[4] = blocked
    return rows


def retryable_delivery(record: Mapping | None, result, variant: str) -> str | None:
    """The attempt's summary when its failure is one retryable missing frame; None for every other end.

    Retryable means: the step ended FAIL with the harness's blocked exit, the record is schema 2 (read_delivery has
    validated it whole), its only failed check is `sync brackets` with unseen frames, and a `default` replay
    delivered no bracket. Teardown is already proven, or the replay raised.
    """
    if result.status != "FAIL" or result.exit_code != perf_settings.HARNESS_BLOCKED:
        return None
    if not isinstance(record, Mapping) or not perf_latency_schema._is_int(record.get("schema_version")) \
            or record["schema_version"] != perf_metrics.DELIVERY_SCHEMA_VERSION:
        return None
    failed = [check for check in record["checks"] if not check["ok"]]
    if len(failed) != 1 or failed[0]["name"] != perf_metrics.RETRYABLE_DELIVERY_CHECK:
        return None
    check = failed[0]
    if check["unseen"] == 0 or (variant == "default" and check["brackets"] != 0):
        # When: no frame went missing, or one did alongside a bracket `default` must never get, it is not retried.
        return None
    markers = check["unseen_markers"]
    listed = ", ".join(f"`{marker}`" for marker in markers)
    more = check["unseen"] - len(markers)
    if more:
        listed += f", and {more} more"
    return f"{check['detail']} ({'marker' if check['unseen'] == 1 else 'markers'} {listed})"


def delivered_text_problem(path: Path, record: Mapping) -> str | None:
    """Why a failed attempt's kept text is not usable evidence; None when it reads whole, within the cap, at the
    length the record's `bytes_kept` states."""
    size = 0
    try:
        with path.open("rb") as stream:
            # Read in blocks and stop past the cap, so a runaway file proves its size without being held.
            while size <= perf_metrics.DELIVERY_TEXT_LIMIT_BYTES:
                block = stream.read(1 << 20)
                if not block:
                    break
                size += len(block)
    except FileNotFoundError:
        return f"{path.name} is missing"
    except OSError as error:
        return f"{path.name} is unreadable: {error}"
    if size > perf_metrics.DELIVERY_TEXT_LIMIT_BYTES:
        return f"{path.name} passes the {perf_metrics.DELIVERY_TEXT_LIMIT_BYTES}-byte cap"
    if size != record["bytes_kept"]:
        return f"{path.name} holds {size} bytes, not the record's bytes_kept {record['bytes_kept']}"
    return None


def delivery_evidence_name(scenario_id: str, variant: str, attempt: int) -> str:
    """The stem every file of one replay attempt shares: its step id, record, text and log."""
    return f"delivery-{scenario_id}-{variant}-attempt{attempt}"


def delivery_note(verdict: str, summaries: Sequence[str]) -> str:
    """The delivery row's disclosure: the shared replay, its verdict on attempt N, then each listed attempt."""
    parts = [perf_metrics.DELIVERY_NOTE, verdict]
    parts.extend(f"attempt {attempt}: {summary}" for attempt, summary in enumerate(summaries, start=1))
    return "; ".join(parts)


def replay_delivery_attempt(gate, binary: Path, scenario_id: str, variant: str, evidence: Path, index: int,
                            attempt: int, *, short: bool, timeout_s: int, temp_root: Path,
                            environ: Mapping[str, str], platform_name: str) -> tuple[dict | None, str | None, object]:
    """Run one replay attempt in a fresh scratch: (trusted record, blocked reason, the step's result).

    The record, the delivered text and the log are kept in `evidence` as `delivery-<id>-<variant>-attempt<N>.*`
    and the scratch is removed. A record is trusted only when it agrees with the replay's end: every check passed
    and the step passed, or a check failed and the step did not. An attempt whose teardown is unproven raises
    StopComparison: processes it may have left would disturb every later run.
    """
    name = delivery_evidence_name(scenario_id, variant, attempt)
    scratch = perf_scenarios.new_scratch_path(temp_root, scenario_id, variant)
    argv = capture_delivery_argv(binary, scenario_id, variant, scratch, short=short)
    step = gate.Step(name, argv, perf_build_policy.gate_hosts(sys.platform), timeout_s, "local", (), ())
    try:
        result = gate.run_step(step, index, perf_settings.ROOT, evidence, perf_scenarios.harness_environment(environ))
        record, problem = read_delivery(scratch, scenario_id, variant)
        for source, suffix in ((perf_metrics.DELIVERY_FILE, "json"), (perf_metrics.DELIVERY_TEXT_FILE, "txt")):
            if (scratch / source).is_file():
                shutil.copyfile(scratch / source, evidence / f"{name}.{suffix}")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    if platform_name == "win32":
        # When: the gate's job owns every process the replay started, so only its custody proves teardown.
        cleanup = perf_outcomes.custody_cleanup(getattr(result, "custody", None))
        if not cleanup.settled:
            raise perf_sets.StopComparison(f"delivery replay {scenario_id}/{variant}: unresolved cleanup: "
                                 f"{'; '.join(cleanup.problems)}")
    elif result.leftover_processes != 0:
        # When: without a job, the gate's leftover count proves teardown, and an unknown count proves nothing.
        counted = "an unknown number of" if result.leftover_processes is None else str(result.leftover_processes)
        raise perf_sets.StopComparison(f"delivery replay {scenario_id}/{variant}: {counted} process(es) outlived the harness")
    ended = f"the replay ended {result.status}, exit {result.exit_code}"
    if record is None:
        return None, f"{problem}; {ended}", result
    if problem is None and result.status != "PASS":
        # When: the record says every check passed, yet the harness did not, the record is not trusted.
        return None, f"{perf_metrics.DELIVERY_FILE} passed every check, but {ended}", result
    if problem is not None and result.status == "PASS":
        return None, f"{problem}, but {ended}", result
    if problem is not None and (result.status != "FAIL" or result.exit_code != perf_settings.HARNESS_BLOCKED):
        # When: a failed record's replay crashed, timed out or exited other than blocked, the reason says how it ended.
        return record, f"{problem}; {ended}", result
    return record, problem, result


def run_delivery_replay(gate, binary: Path, scenario_id: str, variant: str, evidence: Path, index: int, *,
                        short: bool, timeout_s: int, temp_root: Path,
                        environ: Mapping[str, str], platform_name: str | None = None) -> DeliveryOutcome:
    """Replay one scenario's delivery through the harness's `--capture-delivery`, retrying one failure shape.

    Up to DELIVERY_ATTEMPT_LIMIT attempts, each with its own scratch, evidence and `timeout_s`. Only an attempt
    retryable_delivery accepts, one frame marker never found, is retried; any other failure blocks on the
    attempt where it happens, and unproven teardown raises StopComparison. The note discloses the attempt count
    and every retried attempt's detail. Admitting a later attempt is a measurement policy, not proof that
    delivery is free of an intermittent defect; the disclosure and the kept evidence are how one would show.
    """
    platform_name = platform_name or sys.platform
    summaries: list[str] = []
    for attempt in range(1, perf_metrics.DELIVERY_ATTEMPT_LIMIT + 1):
        record, problem, result = replay_delivery_attempt(
            gate, binary, scenario_id, variant, evidence, index, attempt, short=short, timeout_s=timeout_s,
            temp_root=temp_root, environ=environ, platform_name=platform_name)
        of_limit = f"attempt {attempt} of {perf_metrics.DELIVERY_ATTEMPT_LIMIT}"
        if problem is None:
            return DeliveryOutcome(record, None, delivery_note(f"passed on {of_limit}", summaries))
        summary = retryable_delivery(record, result, variant)
        if summary is not None:
            # A failure is retried only with its classified text kept, since that text is its evidence.
            text_problem = delivered_text_problem(
                evidence / f"{delivery_evidence_name(scenario_id, variant, attempt)}.txt", record)
            if text_problem is not None:
                problem, summary = f"{problem}; {text_problem}", None
        if summary is None:
            # When: the failure is anything but one retryable missing frame, it blocks now with its own reason.
            return DeliveryOutcome(record, problem, delivery_note(f"blocked on {of_limit}; not retryable", summaries))
        summaries.append(summary)
        print(f"[perf-compare] delivery replay {scenario_id}/{variant} {of_limit}: {summary}", flush=True)
    return DeliveryOutcome(record, problem,
                           delivery_note(f"blocked on {of_limit}; no attempts left", summaries))
