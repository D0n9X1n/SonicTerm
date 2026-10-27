"""Perf outcomes."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import json
from pathlib import Path
import platform
import re
import time
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_attribution_schema
import perf_latency_schema
import perf_processes
import perf_sessions
import perf_settings



# --- One run: execution and classification --------------------------------------------------

_NOT_EXERCISED = re.compile(r"\bNOT_EXERCISED\b")
# The smoke retries only an occlusion; a comparison retries every invalid run and stops on a schema failure.
_SMOKE_VERDICTS = {"valid": "pass", "occluded": "retry", "blocked": "blocked"}
# An unresolved cleanup may leave processes that disturb every later run, so it stops a comparison too.
_COMPARE_VERDICTS = {"valid": "valid", "blocked": "blocked", "schema": "stop", "refused": "stop", "cleanup": "stop"}


# SIGKILL is signal 9 on every POSIX host that runs the harness; signal.SIGKILL does not exist on Windows.
SIGKILL_EXIT_CODE = -9


def deadline_exit_code(platform_name: str) -> int:
    """The harness's exit code once the deadline case killed it: 124 from TerminateProcess, else -SIGKILL."""
    return perf_processes.WINDOWS_KILL_EXIT_CODE if platform_name == "win32" else SIGKILL_EXIT_CODE


@dataclass(frozen=True)
class RunPlan:
    """One harness run: what to run, on which side, and how it ends."""

    scenario: Scenario
    variant: str
    side: str
    binary: Path
    harness_hash: str
    short: bool = False
    laps: bool = False
    # A counters run: the harness forces the frame-counter gate on (--counters), and the result must say so.
    counters: bool = False
    smoke: bool = False
    # The smoke's deadline case: ended by a group SIGKILL as soon as `go/0` exists.
    kill_at_go: bool = False
    # The tree that built the binary; the run's cwd, so asset_dir() finds that tree's assets.
    source_root: Path = perf_settings.ROOT
    # The head harness's latency split schema; both sides run that harness, so both are held to it.
    latency_split_schema: int | None = None
    # The head harness's phase-kinds schema; both sides run that harness, so both are held to it.
    phase_kinds: int | None = None
    # Whether the comparison built both sides with `perf_s10_attribution_api`; None when nothing decided it.
    attribution_api: bool | None = None
    # The head harness's attribution schema; None for a harness that predates it.
    attribution_schema: int | None = None
    # The head harness's echo-timeline schema; both sides run that harness, so both are held to it.
    echo_timeline_schema: int | None = None
    # The head harness's guard-correlation contract; both sides run that harness, so both are held to it.
    guard_correlation_schema: int | None = None
    # Whether the comparison built both sides with `perf_guard_spans_api`; None when nothing decided it.
    guard_api: bool | None = None


@dataclass
class Host:
    """The collaborators of a run, so tests replace each with a fake."""

    gate: object
    table: object
    front_run: Callable[[Sequence[str], int], CommandRecord]
    home: Path
    temp_root: Path
    printed_forms: set
    environ: Mapping[str, str]
    clock: Callable[[], float] = time.time
    excluded_sids: tuple[int, ...] = ()
    # This host's sys.platform; production sets it, and Windows runs are proven by the gate's job custody.
    platform: str = "darwin"
    # Windows samples the foreground window through this instead of lsappinfo; None samples through front_run.
    front_sample: Callable[[], FrontReading] | None = None


@dataclass
class RunOutcome:
    """Everything one run produced, for its classification, the statistics and the evidence."""

    plan: RunPlan
    evidence: Path
    status: str
    exit_code: int | None
    result: dict | None
    schema_problems: list[str]
    not_exercised: bool
    focus: FocusVerdict
    watcher_problems: list[str]
    cleanup: CleanupResult
    home: list[str]
    deadline: dict[str, object]
    memory: list[MemorySample]
    laps: list[RenderTimingSample]
    footprints: dict[str, dict[str, object]]
    harness_pid: int | None
    # run_step's count of process-group members that outlived the harness; None when they could not be counted.
    leftover_processes: int | None = 0
    step_detail: str = ""
    # Lines in which the App reported that the configured primary font failed to load.
    font_errors: list[str] = field(default_factory=list)
    # The gate's Windows job custody record for the harness step; None off Windows or when none was recorded.
    custody: dict | None = None
    # The exit code run_step reports for the harness the deadline case killed.
    deadline_exit_code: int = SIGKILL_EXIT_CODE
    # The host's sys.platform; Windows table rows and pair checks key off it.
    platform: str = "darwin"
    # The wgpu adapter the App logged, as parse_adapter_line reads it; None off Windows or when none was logged.
    renderer: dict | None = None
    # A laps run's font operations; None for other runs or when the run left no logs directory.
    fallback_log: FallbackLog | None = None
    # A counters run's `sonic::perf_present` lines; empty for other runs.
    present_lines: list[PresentLine] = field(default_factory=list)


UNSETTLED_TEARDOWN = "finish_session did not settle, so the run fails before any retry"
# run_step statuses that end its wait early: what stopped it, for the reason.
STOPPED_WAITS = {"TIMEOUT": "run_step reached its deadline", "INTERRUPTED": "run_step was interrupted"}
# Why a stopped wait is unresolved cleanup. run_step stops before counting the group or while the output is still
# open, which a process outside the group can hold after the group was counted empty.
UNCOUNTED_STOP = ("so either the harness's process group was not counted or a process outside it held the output "
                  "open; either way the run's cleanup is unresolved")


def _harness_reasons(result: dict | None) -> list[str]:
    """The harness's own words for a run: its invalid_reason, which names the cause, then its fixed notes."""
    if result is None:
        return []
    reason = [str(result["invalid_reason"])] if result.get("invalid_reason") else []
    return reason + [str(note) for note in result.get("notes") or []]


def _teardown_unsettled(result: dict | None) -> bool:
    """Whether a result says finish_session did not settle; such a run may have left its sessions behind."""
    return result is not None and result.get("finish_session_settled") is not True


def _deliberately_killed(outcome: RunOutcome) -> bool:
    """Whether the deadline case ended as planned: run_step reaped the harness this script killed at GO.

    run_step reports FAIL with the kill's exit code (-SIGKILL, or 124 from TerminateProcess on
    Windows) only after it saw the leader exit and found its process group or job empty. A TIMEOUT,
    or no exit status, proves no termination.
    """
    return (outcome.plan.kill_at_go and bool(outcome.deadline.get("sent")) and outcome.status == "FAIL"
            and outcome.exit_code == outcome.deadline_exit_code and outcome.leftover_processes == 0)


def _fatal_outcome(outcome: RunOutcome) -> tuple[str, list[str]] | None:
    """Return a run's fatal kind and reasons, or None; classify_outcome explains the order."""
    result, code = outcome.result, outcome.exit_code
    cleanup = []
    if not outcome.cleanup.passed:
        survivors = [f"pid {member.pid} ({member.command})" for member in outcome.cleanup.survivors]
        cleanup += outcome.cleanup.problems + ([f"survivors: {', '.join(survivors)}"] if survivors else [])
    if outcome.status in STOPPED_WAITS:
        # A deadline or Ctrl-C ends run_step's wait without a final group count, so its leftover 0 is no measurement.
        cleanup.append(f"{STOPPED_WAITS[outcome.status]} ({outcome.step_detail or 'no detail'}), {UNCOUNTED_STOP}")
    if code is None and outcome.status != "LAUNCH":
        cleanup.append(f"run_step never collected the harness's exit ({outcome.status}: "
                       f"{outcome.step_detail or 'no detail'}), so the harness may still run and its "
                       f"process-group count is no measurement")
    elif outcome.leftover_processes != 0:
        # run_step killed these by group without identifying them, so the run's cleanup is unresolved.
        counted = "an unknown number of" if outcome.leftover_processes is None else str(outcome.leftover_processes)
        owner = "the harness's job" if outcome.custody is not None else "the harness's process group"
        cleanup.append(f"{counted} member(s) of {owner} outlived it: {outcome.step_detail}")
    if cleanup:
        return "cleanup", cleanup
    notes = [str(note) for note in (result or {}).get("notes") or []]
    if not _deliberately_killed(outcome):
        if outcome.schema_problems:
            return "schema", list(outcome.schema_problems)
        # Exit 0 is trusted only when run_step reported PASS; the exit-code mapping names any other status.
        if code == perf_settings.HARNESS_VALID and outcome.status == "PASS":
            if result is None and not outcome.not_exercised:
                return "schema", ["exit 0 without result.json"]
            if result is not None and result.get("status") != "valid":
                return "schema", [f"exit 0 with status {result.get('status')!r}"]
        if _teardown_unsettled(result):
            return "cleanup", [UNSETTLED_TEARDOWN] + notes
    if code == perf_settings.HARNESS_REFUSED:
        return "refused", ["the harness refused the run (exit 2)"] + notes
    return None


def classify_outcome(outcome: RunOutcome) -> tuple[str, list[str]]:
    """Return a run's kind and reasons; `valid` only when every check passed.

    Fatal kinds come first and no retryable reason hides one, because a retry would rerun the
    scenario on a host that may still hold this run's processes, or trust a result that cannot
    be trusted. In order:

    1. `cleanup` from evidence independent of the result: an unsettled anchor cleanup; a run_step
       deadline or interruption (TIMEOUT, INTERRUPTED), which ends its wait before it counted the
       harness's process group or while a process outside the group held the output open, whatever
       exit it then collected; a harness exit run_step never collected (its process-group count is
       then no measurement); and process-group members that outlived the harness;
    2. `schema`: result.json unreadable, unmanaged or another harness's, and, once run_step
       reported PASS so that exit 0 can be trusted, exit 0 without result.json or with another status;
    3. `cleanup` from the result: finish_session did not settle. It is read only after the schema
       check, since a field of an untrusted result means nothing;
    4. `refused`: the harness refused the run (exit 2).

    The deadline case skips 2 and 3 only when run_step reaped the harness it killed at GO (FAIL,
    the kill's exit, -SIGKILL or 124 on Windows; an empty group or verified job custody): its result
    is expected to be missing or partial, and run_step and the anchor cleanup or the job's custody
    prove its teardown instead. Then come the retryable reasons (session
    records, home writes, focus and the primary font), the deadline case and the exit code; the
    harness's own deadline (exit 4, its group counted) is a retryable `timeout`. The reasons of an
    occlusion or an invalidation start with the harness's invalid_reason, then its notes.
    `compare_verdict` and `smoke_verdict` decide which kinds stop a comparison or the smoke.
    """
    fatal = _fatal_outcome(outcome)
    if fatal is not None:
        return fatal
    plan, result, code = outcome.plan, outcome.result, outcome.exit_code
    if outcome.watcher_problems:
        return "session", list(outcome.watcher_problems)
    if outcome.home:
        return "home", [f"write under the SonicTerm home: {violation}" for violation in outcome.home]
    if outcome.focus.problems:
        return "focus", list(outcome.focus.problems)
    if outcome.font_errors:
        return "font", [f"the App could not load the configured primary font, so it rendered another one: "
                        f"{outcome.font_errors[0]}"]
    if plan.kill_at_go:
        if outcome.deadline.get("problem"):
            return "deadline", [f"deadline case: {outcome.deadline['problem']}"]
        if outcome.deadline.get("sent"):
            if _deliberately_killed(outcome):
                return "valid", []
            return "deadline", [f"the harness was signalled at GO, but run_step reported {outcome.status} with exit "
                                f"{code}, not its own reap of the deadline kill (exit {outcome.deadline_exit_code})"]
        # An occlusion before GO invalidates the deadline case as it does any run, so it is retried.
        if code == perf_settings.HARNESS_INVALID and result is not None and perf_attribution_schema.occlusion_invalidated(result):
            return "occluded", _harness_reasons(result)
        if code == perf_settings.HARNESS_BLOCKED or (code == perf_settings.HARNESS_VALID and result is None and outcome.not_exercised):
            return "blocked", [f"deadline case not exercised (exit {code})"]
        return "unexpected", [f"the harness ended (status {outcome.status}, exit {code}) before the deadline kill"]
    notes = [str(note) for note in (result or {}).get("notes") or []]
    if code == perf_settings.HARNESS_VALID:
        # Exit 0 with another status (the final log write failed, or the output overflowed) cannot be trusted.
        if outcome.status != "PASS":
            return "launcher", [f"run_step reported {outcome.status} with exit 0: {outcome.step_detail or 'no detail'}"]
        if result is None:
            # Without NOT_EXERCISED this was a schema failure above.
            return "blocked", ["the harness printed NOT_EXERCISED"]
        if not outcome.focus.judged:
            return "focus", ["harness.pid never appeared, so focus safety could not be judged"]
        blocked = presenter_blocked(outcome)
        if blocked:
            # When: the run could not measure its variant's presenter, it is not exercised, not invalid.
            return "blocked", [blocked]
        if outcome.platform == "win32" and outcome.renderer is None:
            # When: only the App's adapter line proves which adapter drew a Windows run, so without it no pair holds.
            return "adapter", ["the App logged no `wgpu adapter selected` or `wgpu adapter reused` line, "
                               "so the run's adapter is unknown"]
        return "valid", []
    if code == perf_settings.HARNESS_INVALID:
        harness_reasons = _harness_reasons(result)
        if result is not None and perf_attribution_schema.occlusion_invalidated(result):
            return "occluded", harness_reasons
        return "invalid", harness_reasons or ["the harness invalidated the run (exit 3)"]
    if code == perf_settings.HARNESS_TIMEOUT:
        return "timeout", ["the harness timed out (exit 4)"] + notes
    if code == perf_settings.HARNESS_BLOCKED:
        return "blocked", ["the harness cannot run this scenario (exit 5)"] + notes
    return "unexpected", [f"unexpected harness exit {code} (status {outcome.status})"]


def presenter_blocked(outcome: RunOutcome) -> str | None:
    """Why a run cannot measure its variant's presenter: `gdi` or `wgpu` with no presenter record, `gdi`
    without GDI, or a degraded `wgpu`; else None."""
    presenter = (outcome.result or {}).get("presenter")
    if not isinstance(presenter, Mapping):
        if outcome.plan.variant in ("gdi", "wgpu"):
            # When: the variant exists to measure one presenter, a run that recorded none proves nothing.
            return f"the {outcome.plan.variant} variant's run recorded no presenter, so its presenter is unproven"
        return None
    if outcome.plan.variant == "gdi" and presenter.get("windows_gdi") is not True:
        return "the gdi variant did not present through Windows GDI (presenter.windows_gdi is false)"
    if outcome.plan.variant == "wgpu" and presenter.get("software_render_degraded") is True:
        return "the wgpu variant's presenter is degraded (presenter.software_render_degraded is true)"
    return None


def smoke_verdict(kind: str) -> str:
    """Map a run kind to the smoke's verdict: `pass`, `retry`, `blocked` or `fail`."""
    return _SMOKE_VERDICTS.get(kind, "fail")


def compare_verdict(kind: str) -> str:
    """Map a run kind to a comparison's verdict: `valid`, `invalid` (retried), `blocked` or `stop`."""
    return _COMPARE_VERDICTS.get(kind, "invalid")


PRIMARY_FONT_ERROR = "Unable to load the configured primary font"


def primary_font_errors(lines: Iterable[str]) -> list[str]:
    """Return the distinct lines in which the App reported that the configured primary font failed to load."""
    found: list[str] = []
    for line in lines:
        if PRIMARY_FONT_ERROR in line and line.strip() not in found:
            found.append(line.strip())
    return found


def custody_verified(custody: Mapping | None) -> bool:
    """The gate's own custody rule (local-gate.py `_verified_custody`): the job emptied, the bootstrap was
    reaped, the protocol and the capture completed, and nothing failed."""
    return bool(custody and custody.get("empty") is True and custody.get("bootstrap_reaped") is True
                and custody.get("protocol_complete") is True and custody.get("capture_complete") is True
                and custody.get("errors") == [])


def custody_cleanup(custody: Mapping | None) -> perf_sessions.CleanupResult:
    """A Windows run's cleanup: settled only when the gate's job custody is verified."""
    result = perf_sessions.CleanupResult()
    if custody is None:
        result.unresolved("run_step recorded no job custody, so the run's teardown is unproven")
    elif not custody_verified(custody):
        errors = "; ".join(str(error) for error in custody.get("errors") or []) or "no error recorded"
        result.unresolved(f"the job's custody is not verified (empty={custody.get('empty')!r}, "
                          f"cleanup={custody.get('cleanup')!r}): {errors}")
    return result


def windows_leftover_processes(custody: Mapping | None, *, deadline_case: bool) -> int | None:
    """Members of the harness's job alive when it exited; None when they could not be counted.

    The deadline case ends the harness while its programs live, so for it the count is 0 once the
    job's custody proves they were all ended; otherwise the count before cleanup stands.
    """
    if custody is None:
        return None
    if deadline_case and custody_verified(custody):
        return 0
    before = custody.get("before_cleanup")
    active = before.get("active_processes") if isinstance(before, Mapping) else None
    return active if perf_latency_schema._is_int(active) else None


# Members a smoke attempt's line lists before it counts the rest.
MEMBER_LINE_LIMIT = 16


def custody_member_text(custody: Mapping | None) -> str:
    """`members: <pid> <image> <created>; ...` from a custody record, at most 16, then how many more."""
    before = custody.get("before_cleanup") if isinstance(custody, Mapping) else None
    if not isinstance(before, Mapping):
        return "members: unknown (no custody record)"
    listing = before.get("members")
    if not isinstance(listing, Mapping):
        return f"members: unknown ({before.get('members_error') or 'not listed'})"
    processes = [process for process in listing.get("processes") or [] if isinstance(process, Mapping)]
    total = max(int(listing.get("count") or 0), len(processes))
    if total == 0:
        return "members: none"
    parts = []
    for process in processes[:MEMBER_LINE_LIMIT]:
        image = process.get("image")
        created = process.get("created")
        parts.append(f"{process.get('pid')} {image if image is not None else 'unknown'} "
                     f"{created if created is not None else 'unknown'}")
    rest = total - len(parts)
    if rest > 0:
        parts.append(f"and {rest} more")
    return "members: " + "; ".join(parts)


def _write_json(path: Path, data: object) -> None:
    path.write_text(json.dumps(data, indent=2, default=str) + "\n", encoding="utf-8")
