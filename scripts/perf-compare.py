#!/usr/bin/env python3
"""Compare SonicTerm performance between two Git refs with the perf_scenarios harness.

`--base <ref> --head <ref>` builds each ref in its own worktree and target
directory with the head's harness overlaid, runs every selected scenario in
alternating A B B A order until each side has `--runs` valid runs, and prints
the comparison table, the host block, both SHAs and the harness hash.
`--smoke` is the `macos-perf-smoke` gate step: a debug build of the current
tree, short S1 and S3 runs and one S1 run ended the way a step deadline ends
it, checking the result schema, focus safety and cleanup, with no timing
assertion.

Every child process for a build, a harness run or a `footprint` sample goes
through local-gate.py's `run_step`, which bounds it and reaps its process
tree. PTY sessions leave that tree through `setsid`, so each run's sessions
are cleaned through the anchor records the scenario scripts write: the anchor
is revalidated, every live member of the recorded session is enumerated and
rechecked immediately before it is signalled, and the anchor is signalled
last. No pid that failed validation is ever signalled.

On Windows the gate's job object owns every process a run starts, so the
job's custody record, not anchor records, proves a run's teardown. Each role
program's record is acknowledged once its process is the harness's child,
runs the harness image and was created after the launch, and the deadline
case ends the harness with TerminateProcess after rechecking its image and
creation time.

The module imports on every host. POSIX-only calls (`os.getsid`,
`os.killpg`, `signal.SIGKILL`), the macOS libproc binding and the Windows
kernel32 binding are resolved inside the code paths that need them, so the
tests drive fakes everywhere.
"""

from __future__ import annotations

import argparse
import calendar
from dataclasses import asdict, dataclass, field
from fractions import Fraction
import errno
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

ROOT = Path(__file__).resolve().parent.parent
SCHEMA_VERSION = 1
SIDES = ("base", "head")
DEFAULT_RUNS = 5
# Invalid runs retried per side and scenario before the scenario stops.
RETRY_LIMIT = 3
HARNESS_EXAMPLE = "perf_scenarios"
ALLOC_EXAMPLE = "perf_scenarios_alloc"
HARNESS_DIRECTORY = "crates/sonicterm-app/examples/perf_scenarios"
APP_MANIFEST = "crates/sonicterm-app/Cargo.toml"

# Harness exit codes.
HARNESS_VALID = 0
HARNESS_REFUSED = 2
HARNESS_INVALID = 3
HARNESS_TIMEOUT = 4
HARNESS_BLOCKED = 5
NOT_EXERCISED = "NOT_EXERCISED"

# This script's exit codes; BLOCKED matches the gate's convention for a host that cannot exercise a step.
EXIT_PASS = 0
EXIT_FAIL = 1
EXIT_BLOCKED = 3


def load_gate():
    """Load local-gate.py so every child runs through its bounded, tree-reaping `run_step`."""
    spec = importlib.util.spec_from_file_location("perf_local_gate", ROOT / "scripts" / "local-gate.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


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

    def __init__(self, target_runs: int, retry_limit: int = RETRY_LIMIT) -> None:
        self.target_runs = target_runs
        self.retry_limit = retry_limit
        self.valid_runs = {side: 0 for side in SIDES}
        self.invalid_runs = {side: 0 for side in SIDES}
        self.failed_side: str | None = None
        self.retired: set[str] = set()
        self._slot = 0

    @property
    def complete(self) -> bool:
        """Whether every side that still runs has its valid runs."""
        return all(side in self.retired or self.valid_runs[side] >= self.target_runs for side in SIDES)

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


# --- Front application ----------------------------------------------------------------------

FRONT_ARGV = ("lsappinfo", "front")
FRONT_SAMPLE_INTERVAL_S = 1.0
FRONT_COMMAND_TIMEOUT_S = 5
# A front application's ASN. macOS 26 and the macos-15-intel runner print both halves with `0x`
# (`ASN:0x0-0x6ba4b9e:`). The macos-14 runner prints the low half without it (`ASN:0x0-c00c:`) and
# finds no application when that spelling is looked up, so the lookup spells both halves with `0x`.
_FRONT_ASN = re.compile(r"ASN:0x([0-9A-Fa-f]+)-(?:0x)?([0-9A-Fa-f]+):")
_FRONT_PID = re.compile(r'"pid"=([0-9]+)')
_NULL_ASN_PREFIX = "ASN:0x0-0x0"
_HEX_DIGITS = frozenset("0123456789abcdefABCDEF")


@dataclass(frozen=True)
class CommandRecord:
    """One raw command sample: when it started, its argv, exit status and output."""

    unix_s: float
    argv: tuple[str, ...]
    exit_code: int | None
    stdout: str
    stderr: str
    timed_out: bool = False

    def as_json(self) -> dict[str, object]:
        """Return the record as the evidence log stores it."""
        return {"unix_s": self.unix_s, "argv": list(self.argv), "exit_code": self.exit_code,
                "timed_out": self.timed_out, "stdout": self.stdout, "stderr": self.stderr}


@dataclass(frozen=True)
class FrontReading:
    """One classified sample: `app` with its pid, `none` for no front application, or `failed`."""

    kind: str
    pid: int | None
    detail: str
    records: tuple[CommandRecord, ...]


def front_pid_argv(asn: str) -> tuple[str, ...]:
    """Return the lookup that prints the front application's `"pid"=<n>`."""
    return ("lsappinfo", "info", "-only", "pid", asn)


def _command_failure(record: CommandRecord) -> str | None:
    """Describe a timed-out or nonzero command, or return None for a clean exit."""
    if record.timed_out:
        return f"{' '.join(record.argv)} timed out"
    if record.exit_code != 0:
        return f"{' '.join(record.argv)} exited {record.exit_code}"
    return None


def is_null_front(text: str) -> bool:
    """Return whether trimmed `lsappinfo front` output means that no application is front.

    These forms come from the lsappinfo binary's strings (`[ NULL ] ` and the null ASN
    `ASN:0x0-0x0-NULL`), not from an observed run: nobody has seen what it prints with no
    front application. Only exactly `[ NULL ]`, or `ASN:0x0-0x0` followed by a character that is
    not a hex digit, qualifies. `ASN:0x0-0x0a1:` is a real application, and a bare `ASN:0x0-0x0`
    is cut-off output that fails the sample.
    """
    if text == "[ NULL ]":
        return True
    if not text.startswith(_NULL_ASN_PREFIX):
        return False
    rest = text[len(_NULL_ASN_PREFIX):]
    return bool(rest) and rest[0] not in _HEX_DIGITS


def classify_front(front: CommandRecord, lookup: Callable[[str], CommandRecord]) -> FrontReading:
    """Classify one front sample; anything but a null form or a resolved ASN is a failed sample.

    The pid lookup spells the ASN with `0x` on both halves, whichever way `front` printed it.

    A nonzero exit, a timeout, empty output or other text fails the sample before any
    text is read, so a failure can never read as no front application.
    """
    failure = _command_failure(front)
    if failure:
        return FrontReading("failed", None, failure, (front,))
    text = front.stdout.strip()
    if is_null_front(text):
        return FrontReading("none", None, "", (front,))
    asn_match = _FRONT_ASN.fullmatch(text)
    if not asn_match:
        detail = f"unparseable front output {text!r}" if text else "empty front output"
        return FrontReading("failed", None, detail, (front,))
    pid_record = lookup(f"ASN:0x{asn_match[1]}-0x{asn_match[2]}:")
    records = (front, pid_record)
    failure = _command_failure(pid_record)
    if failure:
        return FrontReading("failed", None, failure, records)
    match = _FRONT_PID.fullmatch(pid_record.stdout.strip())
    if not match or int(match[1]) <= 0:
        return FrontReading("failed", None, f"unparseable pid lookup {pid_record.stdout.strip()!r}", records)
    return FrontReading("app", int(match[1]), "", records)


@dataclass(frozen=True)
class FocusVerdict:
    """Focus safety over one run's samples; `judged` is False when harness.pid never appeared.

    `notes` records activations that are not theft because the host has no user session.
    """

    theft: bool
    failed: tuple[FrontReading, ...]
    problems: list[str]
    judged: bool
    notes: list[str] = field(default_factory=list)
    # Windows only: every foreground pid change after the baseline, as {from_pid, to_pid, unix_s}.
    changes: list[dict] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        """Whether the run kept focus safety: judged, no theft and no failed sample."""
        return self.judged and not self.problems


def has_user_session(environ: Mapping[str, str]) -> bool:
    """Return whether a user may hold focus: False only on a GitHub-hosted runner, for the smoke and a comparison.

    A GitHub macOS runner still reports a front application, so the samples alone cannot tell a
    runner from a desk. GITHUB_ACTIONS=true alone does not prove there is no user: a self-hosted
    runner may have one, so RUNNER_ENVIRONMENT must be `github-hosted`. A desk run is always strict.
    """
    return not (environ.get("GITHUB_ACTIONS") == "true" and environ.get("RUNNER_ENVIRONMENT") == "github-hosted")


def focus_rule_line(environ: Mapping[str, str]) -> str:
    """Name the focus rule a run is judged by and the runner variables it read, for the run's log."""
    seen = (f"GITHUB_ACTIONS={environ.get('GITHUB_ACTIONS', 'unset')} "
            f"RUNNER_ENVIRONMENT={environ.get('RUNNER_ENVIRONMENT', 'unset')}")
    if not has_user_session(environ):
        return (f"focus rule: {seen}: a GitHub-hosted runner has no user session, so the harness becoming the "
                f"front application is recorded, not theft; a failed sample still fails")
    return f"focus rule: {seen}: strict; the harness becoming the front application while another was front is theft"


def judge_focus(readings: Sequence[FrontReading], harness_pid: int | None, *,
                user_session: bool = True) -> FocusVerdict:
    """Judge a run's samples, taken in order from just before launch.

    Theft is the harness pid becoming the front application right after another
    application was front; after no front application it is not theft. Without a user
    session (a GitHub-hosted runner) there is no focus to take, so that activation is only noted.
    A failed sample is a problem of its own, and theft is not judged across it.
    """
    failed = tuple(sample for sample in readings if sample.kind == "failed")
    problems = [f"failed front-application sample: {sample.detail}" for sample in failed]
    notes: list[str] = []
    theft = False
    if harness_pid is not None:
        previous = None
        for current in readings:
            if (current.kind == "app" and current.pid == harness_pid and previous is not None
                    and previous.kind == "app" and previous.pid != harness_pid):
                if user_session:
                    theft = True
                    problems.append(f"focus theft: harness pid {harness_pid} became the front "
                                    f"application while pid {previous.pid} was front")
                else:
                    notes.append(f"harness pid {harness_pid} became the front application while pid "
                                 f"{previous.pid} was front; this host has no user session, so it is not theft")
            previous = current
    return FocusVerdict(theft, failed, problems, harness_pid is not None, notes)


def append_front_samples(log_path: Path, records: Iterable[CommandRecord]) -> None:
    """Append raw samples to the run's `front-samples.log`, one JSON object per command."""
    with log_path.open("a", encoding="utf-8") as stream:
        for record in records:
            stream.write(json.dumps(record.as_json()) + "\n")


def describe_sample(sample: FrontReading) -> str:
    """Render a failed sample with its raw records, so the first CI run shows the real output."""
    lines = [f"front-application sample failed: {sample.detail}"]
    lines.extend("  " + json.dumps(record.as_json()) for record in sample.records)
    return "\n".join(lines)


def bounded_command(run_command: Callable, argv: Sequence[str], timeout_s: int,
                    clock: Callable[[], float] = time.time, cwd: Path = ROOT) -> CommandRecord:
    """Run a short command through the smoke runner's bounded, tree-reaping `run_command`."""
    unix_s = clock()
    completed = run_command(list(argv), cwd, timeout_s, dict(os.environ))
    stderr = completed.stderr.decode("utf-8", errors="replace")
    # The smoke runner reports a deadline as exit 124 plus its own message on stderr.
    timed_out = completed.returncode == 124 and "timed out after" in stderr
    return CommandRecord(unix_s, tuple(argv), None if timed_out else completed.returncode,
                         completed.stdout.decode("utf-8", errors="replace"), stderr, timed_out)


def sample_front(run: Callable[[Sequence[str], int], CommandRecord]) -> FrontReading:
    """Take one front-application sample: `lsappinfo front`, then the pid lookup for a real ASN."""
    return classify_front(run(FRONT_ARGV, FRONT_COMMAND_TIMEOUT_S),
                          lambda asn: run(front_pid_argv(asn), FRONT_COMMAND_TIMEOUT_S))


# A Windows foreground sample's argv in front-samples.log; it names the call, since no command runs.
FOREGROUND_ARGV = ("GetForegroundWindow",)


def classify_foreground(hwnd: int, pid: int, error: str | None, *, unix_s: float | None = None) -> FrontReading:
    """Classify one Windows foreground sample: no window is `none`; a failed call, or a window without
    a process id, is `failed`; anything else is `app`.

    The sample is kept as a CommandRecord, so front-samples.log holds one schema on every host.
    """
    record = CommandRecord(time.time() if unix_s is None else unix_s, FOREGROUND_ARGV, 1 if error else 0,
                           f"hwnd={hwnd:#x} pid={pid}\n", error or "")
    if error:
        return FrontReading("failed", None, f"GetForegroundWindow: {error}", (record,))
    if hwnd == 0:
        return FrontReading("none", None, "", (record,))
    if pid == 0:
        return FrontReading("failed", None, f"foreground window {hwnd:#x} has no process id", (record,))
    return FrontReading("app", pid, "", (record,))


def sample_foreground() -> FrontReading:
    """Take one Windows foreground sample through user32: the foreground window, then its process id."""
    unix_s = time.time()
    try:
        import ctypes
        from ctypes import wintypes
        user32 = ctypes.WinDLL("user32", use_last_error=True)
        user32.GetForegroundWindow.argtypes = ()
        user32.GetForegroundWindow.restype = wintypes.HWND
        user32.GetWindowThreadProcessId.argtypes = (wintypes.HWND, ctypes.POINTER(wintypes.DWORD))
        user32.GetWindowThreadProcessId.restype = wintypes.DWORD
        hwnd = user32.GetForegroundWindow()
        if not hwnd:
            return classify_foreground(0, 0, None, unix_s=unix_s)
        pid = wintypes.DWORD(0)
        thread = user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        error = None if thread else f"GetWindowThreadProcessId failed: Win32 error {ctypes.get_last_error()}"
        return classify_foreground(int(hwnd), int(pid.value), error, unix_s=unix_s)
    except (ImportError, OSError, AttributeError) as error:
        # When: user32 cannot be bound (off Windows), the sample fails rather than reading as no window.
        return classify_foreground(0, 0, f"{type(error).__name__}: {error}", unix_s=unix_s)


def judge_foreground(readings: Sequence[FrontReading], *, user_session: bool = True) -> FocusVerdict:
    """Judge a Windows run's foreground samples, taken in order from just before launch.

    The baseline is the first application in the foreground. Any later application with another pid
    is a change, the harness included, and a sample with no foreground window between two is
    bridged. At a desk a change invalidates the run; without a user session (a GitHub-hosted
    runner) it is only noted. A failed sample is a problem of its own.
    """
    failed = tuple(sample for sample in readings if sample.kind == "failed")
    problems = [f"failed foreground sample: {sample.detail}" for sample in failed]
    notes: list[str] = []
    changes: list[dict] = []
    current = None
    for reading in readings:
        if reading.kind != "app":
            continue
        if current is not None and reading.pid != current:
            unix_s = reading.records[0].unix_s if reading.records else None
            changes.append({"from_pid": current, "to_pid": reading.pid, "unix_s": unix_s})
            change = f"the foreground moved from pid {current} to pid {reading.pid}"
            if user_session:
                problems.append(f"foreground change: {change}")
            else:
                notes.append(f"{change}; this host has no user session, so it is recorded, not a failure")
        current = reading.pid
    return FocusVerdict(bool(changes) and user_session, failed, problems, True, notes, changes)


# --- Process table and session cleanup ------------------------------------------------------

# How long cleanup keeps signalling a session's members before it reports them as survivors.
CLEANUP_BOUND_S = 5.0
CLEANUP_PAUSE_S = 0.05
# How long the anchor may take to disappear after its SIGKILL.
ANCHOR_EXIT_BOUND_S = 5.0
# Linux reports boot time in whole seconds, so a computed start time can read up to a second early.
START_TOLERANCE_S = 1.0


class ProcessUnreadable(Exception):
    """A live process whose identity cannot be read, so it can be neither signalled nor ignored."""


@dataclass(frozen=True)
class ProcessInfo:
    """One read of a process; the start token identifies it, so a reused pid reads differently."""

    pid: int
    pgid: int
    sid: int
    start: str
    start_unix_s: float
    command: str
    ppid: int = 0


@dataclass(frozen=True)
class SessionRecord:
    """A scenario script's `sessions/<role>.json`: its session leader, anchor and tty (diagnostic only)."""

    role: str
    leader_pid: int
    anchor_pid: int
    tty: str


@dataclass(frozen=True)
class AckedSession:
    """A validated session: the leader's pid is the session id, and both start times are stored."""

    role: str
    leader_pid: int
    anchor_pid: int
    leader_start: str
    anchor_start: str


def _positive_pid(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value > 0


def parse_session_record(text: str, role: str) -> SessionRecord:
    """Parse one session record; anything but two distinct positive pids for `role` is refused."""
    data = json.loads(text)
    named = data.get("role") if isinstance(data, dict) else None
    # The generated role script writes `"role":%s`, so a bare integer matching the file name also counts.
    if not ((isinstance(named, str) and named == role) or (_is_int(named) and str(named) == role)):
        raise ValueError(f"session record does not name role {role!r}")
    leader_pid, anchor_pid, tty = data.get("leader_pid"), data.get("anchor_pid"), data.get("tty")
    if not (_positive_pid(leader_pid) and _positive_pid(anchor_pid)) or leader_pid == anchor_pid:
        raise ValueError("session record needs two distinct positive pids")
    if not isinstance(tty, str):
        raise ValueError("session record tty is not a string")
    return SessionRecord(role, leader_pid, anchor_pid, tty)


def validate_session(record: SessionRecord, table, launch_unix_s: float,
                     excluded_sids: Iterable[int] = ()) -> tuple[AckedSession | None, str | None]:
    """Validate a record before it is acknowledged; return the stored identity or the reason it fails.

    Both processes are alive and started after this run's launch, the leader leads its
    session and process group, and the anchor's session and process group are the leader's
    pid. A session that is this script's own or the harness's is refused.
    """
    label = f"session {record.role}"
    try:
        leader = table.read(record.leader_pid)
        anchor = table.read(record.anchor_pid)
    except ProcessUnreadable as error:
        return None, f"{label}: {error}"
    if leader is None or anchor is None:
        return None, f"{label}: leader {record.leader_pid} or anchor {record.anchor_pid} is not alive"
    if not leader.sid == leader.pgid == record.leader_pid:
        return None, f"{label}: leader {record.leader_pid} does not lead its session and process group"
    if anchor.sid != record.leader_pid or anchor.pgid != record.leader_pid:
        return None, f"{label}: anchor {record.anchor_pid} is not in the leader's session and process group"
    if record.leader_pid in set(excluded_sids):
        return None, f"{label}: session {record.leader_pid} belongs to this script or the harness"
    earliest = launch_unix_s - START_TOLERANCE_S
    if leader.start_unix_s < earliest or anchor.start_unix_s < earliest:
        return None, f"{label}: the leader or the anchor started before this run"
    return AckedSession(record.role, record.leader_pid, record.anchor_pid, leader.start, anchor.start), None


def session_members(table, sid: int) -> list[ProcessInfo] | None:
    """List every live process whose session id is `sid`, whatever its process group or tty.

    None means the enumeration is incomplete: the process list could not be read, or a
    process in the session could not be identified, so cleanup can never call it empty.
    """
    pids = table.pids()
    if pids is None:
        return None
    members = []
    for pid in pids:
        try:
            if table.session_of(pid) != sid:
                continue
            info = table.read(pid)
        except ProcessUnreadable:
            return None
        if info is not None and info.sid == sid:
            members.append(info)
    return members


@dataclass(frozen=True)
class ProgramRecord:
    """A Windows role program's `sessions/<role>.json`: its role and process id; tty is always `none`."""

    role: str
    program_pid: int
    tty: str


@dataclass(frozen=True)
class AckedProgram:
    """A validated role program and its creation time, which identifies it if its pid is reused."""

    role: str
    program_pid: int
    start: str


def parse_program_record(text: str, role: str) -> ProgramRecord:
    """Parse one role program's record; anything but `role`, a positive pid and a string tty is refused."""
    data = json.loads(text)
    named = data.get("role") if isinstance(data, dict) else None
    # The program writes the role as a JSON integer, so one matching the file name counts as a string would.
    if not ((isinstance(named, str) and named == role) or (_is_int(named) and str(named) == role)):
        raise ValueError(f"program record does not name role {role!r}")
    program_pid, tty = data.get("program_pid"), data.get("tty")
    if not _positive_pid(program_pid):
        raise ValueError("program record needs a positive program_pid")
    if not isinstance(tty, str):
        raise ValueError("program record tty is not a string")
    return ProgramRecord(role, program_pid, tty)


def validate_program(record: ProgramRecord, table, harness_pid: int | None, launch_unix_s: float, *,
                     harness_command: str = HARNESS_EXAMPLE + ".exe") -> tuple[AckedProgram | None, str | None]:
    """Validate a role program before it is acknowledged; return its identity or the reason it fails.

    The process is alive, runs the harness image (Windows names compare without case), its parent
    is the harness, and it was created after this run's launch.
    """
    label = f"program {record.role}"
    pid = record.program_pid
    if table is None or harness_pid is None or pid in (harness_pid, os.getpid()):
        return None, f"{label}: pid {pid} cannot be validated as a role program of harness {harness_pid}"
    try:
        info = table.read(pid)
    except ProcessUnreadable as error:
        return None, f"{label}: {error}"
    if info is None:
        return None, f"{label}: pid {pid} is not alive"
    if info.command.lower() != harness_command.lower():
        return None, f"{label}: pid {pid} runs {info.command!r}, not the harness image {harness_command}"
    if info.ppid != harness_pid:
        return None, f"{label}: the parent of pid {pid} is {info.ppid}, not the harness {harness_pid}"
    if info.start_unix_s < launch_unix_s - START_TOLERANCE_S:
        return None, f"{label}: pid {pid} was created before this run's launch"
    return AckedProgram(record.role, pid, info.start), None


@dataclass
class CleanupResult:
    """The outcome of cleaning one run's sessions; `settled` is False while members are left."""

    settled: bool = True
    signalled: list[int] = field(default_factory=list)
    survivors: list[ProcessInfo] = field(default_factory=list)
    problems: list[str] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        """Whether cleanup settled with no stale record, reused identity or survivor."""
        return self.settled and not self.problems

    def unresolved(self, problem: str, survivors: Iterable[ProcessInfo] = ()) -> None:
        """Record that cleanup could not settle, listing the members it left alone."""
        self.settled = False
        self.survivors.extend(survivors)
        self.problems.append(problem)

    def note_signal(self, pid: int) -> None:
        """Record a SIGKILL the kernel accepted, once per pid."""
        if pid not in self.signalled:
            self.signalled.append(pid)


def _anchor_state(table, session: AckedSession) -> str:
    """Reread the anchor: `valid`, `gone`, `stale` (its pid names another process) or `unreadable`."""
    try:
        info = table.read(session.anchor_pid)
    except ProcessUnreadable:
        return "unreadable"
    if info is None:
        return "gone"
    if (info.start != session.anchor_start or info.sid != session.leader_pid
            or info.pgid != session.leader_pid):
        return "stale"
    return "valid"


def _recheck_member(table, member: ProcessInfo, sid: int) -> str:
    """Reread a member immediately before its signal: `same`, `exited`, `reused`, `left` or `unreadable`."""
    try:
        info = table.read(member.pid)
    except ProcessUnreadable:
        return "unreadable"
    if info is None:
        return "exited"
    if info.start != member.start:
        return "reused"
    return "same" if info.sid == sid else "left"


def cleanup_session(table, session: AckedSession, result: CleanupResult, clock: Callable[[], float],
                    sleep: Callable[[float], None], bound_s: float) -> None:
    """End one session's members, then its anchor, without ever signalling an unvalidated pid.

    While the validated anchor lives its process group, whose id is the session id, exists,
    so the id cannot be reused and every process holding it belongs to this run. Members
    are therefore signalled only while the anchor revalidates, each after a recheck of its
    session and start time; the anchor goes last. With the anchor gone or stale, members
    left are listed and nothing is signalled.
    """
    sid = session.leader_pid
    label = f"session {session.role} (sid {sid})"
    deadline = clock() + bound_s
    while True:
        members = session_members(table, sid)
        if members is None:
            result.unresolved(f"{label}: process enumeration incomplete; nothing more was signalled")
            return
        state = _anchor_state(table, session)
        others = [member for member in members
                  if not (member.pid == session.anchor_pid and member.start == session.anchor_start)]
        if state != "valid":
            if others:
                result.unresolved(f"{label}: anchor {state} while members are left; nothing was signalled", others)
            elif state != "gone":
                result.problems.append(f"{label}: anchor pid {session.anchor_pid} was reused ({state}); not signalled")
            return
        if not others:
            break
        if clock() >= deadline:
            result.unresolved(f"{label}: members outlived {bound_s:g}s of SIGKILL; the anchor was kept", others)
            return
        for member in others:
            verdict = _recheck_member(table, member, sid)
            if verdict == "same":
                if table.kill(member.pid) == "sent":
                    result.note_signal(member.pid)
            elif verdict != "exited":
                result.problems.append(f"{label}: member pid {member.pid} {verdict} before its signal; not signalled")
        sleep(CLEANUP_PAUSE_S)
    # Only the anchor remains; it is rechecked immediately before its own signal.
    state = _anchor_state(table, session)
    if state == "valid":
        if table.kill(session.anchor_pid) == "sent":
            result.note_signal(session.anchor_pid)
        anchor_deadline = clock() + ANCHOR_EXIT_BOUND_S
        while _anchor_state(table, session) == "valid":
            if clock() >= anchor_deadline:
                result.unresolved(f"{label}: anchor {session.anchor_pid} outlived its SIGKILL")
                return
            sleep(CLEANUP_PAUSE_S)
    elif state != "gone":
        result.problems.append(f"{label}: anchor pid {session.anchor_pid} was reused ({state}); not signalled")
    final = session_members(table, sid)
    if final is None:
        result.unresolved(f"{label}: process enumeration incomplete after the anchor's signal")
    elif final:
        result.unresolved(f"{label}: members appeared after the anchor's signal", final)


def cleanup_sessions(table, sessions: Iterable[AckedSession], *, unanchored: Iterable[SessionRecord] = (),
                     clock: Callable[[], float] = time.monotonic, sleep: Callable[[float], None] = time.sleep,
                     bound_s: float = CLEANUP_BOUND_S) -> CleanupResult:
    """Clean every validated session of one run, including a run that crashed or hit its deadline.

    A record whose anchor never validated is only listed: without the anchor its session id may
    name another session, so its members are reported as survivors and none is signalled.
    """
    result = CleanupResult()
    for session in sessions:
        cleanup_session(table, session, result, clock, sleep, bound_s)
    for record in unanchored:
        label = f"session {record.role} (sid {record.leader_pid})"
        members = session_members(table, record.leader_pid)
        if members is None:
            result.unresolved(f"{label}: no valid anchor, and the process enumeration is incomplete")
        elif members:
            result.unresolved(f"{label}: members are left without a valid anchor; nothing was signalled", members)
    return result


# `proc_bsdinfo` from <sys/proc_info.h>: flags, status, xstatus, pid, ppid, uid, gid, ruid, rgid,
# svuid, svgid, rfu_1, comm[16], name[32], nfiles, pgid, pjobc, e_tdev, e_tpgid, nice,
# start seconds and microseconds; 136 bytes with natural alignment.
BSDINFO_FORMAT = "=IIIIIIIIIII I16s32sIIIIIiQQ".replace(" ", "")
BSDINFO_SIZE = struct.calcsize(BSDINFO_FORMAT)
PROC_PIDTBSDINFO = 3
# `SZOMB` from <sys/proc.h>.
MACOS_ZOMBIE_STATUS = 5


def decode_bsdinfo(raw: bytes, sid: int) -> ProcessInfo | None:
    """Decode one `proc_bsdinfo` record; a zombie reads as gone because it runs nothing."""
    fields = struct.unpack(BSDINFO_FORMAT, raw[:BSDINFO_SIZE])
    if fields[1] == MACOS_ZOMBIE_STATUS:
        return None
    start_s, start_us = fields[20], fields[21]
    command = fields[12].split(b"\0", 1)[0].decode("utf-8", errors="replace")
    return ProcessInfo(fields[3], fields[15], sid, f"{start_s}.{start_us:06d}",
                       start_s + start_us / 1_000_000, command, fields[4])


class MacProcessTable:
    """The macOS process table through libproc; session ids come from `getsid`."""

    def __init__(self) -> None:
        import ctypes
        self._ctypes = ctypes
        self._libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        self._libproc.proc_listallpids.argtypes = (ctypes.c_void_p, ctypes.c_int)
        self._libproc.proc_listallpids.restype = ctypes.c_int
        self._libproc.proc_pidinfo.argtypes = (ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                               ctypes.c_void_p, ctypes.c_int)
        self._libproc.proc_pidinfo.restype = ctypes.c_int

    def pids(self) -> list[int] | None:
        """List every pid, or None when the list cannot be read whole."""
        ctypes = self._ctypes
        hint = self._libproc.proc_listallpids(None, 0)
        if hint <= 0:
            return None
        capacity = hint + 64
        for _attempt in range(4):
            buffer = (ctypes.c_int * capacity)()
            count = self._libproc.proc_listallpids(buffer, ctypes.sizeof(buffer))
            if count < 0:
                return None
            # A full buffer may have been truncated, so it is read again with more room.
            if count < capacity:
                return [pid for pid in buffer[:count] if pid > 0]
            capacity *= 2
        return None

    def session_of(self, pid: int) -> int | None:
        """Return a pid's session id, or None once it has exited (a zombie reports ESRCH)."""
        try:
            return os.getsid(pid)
        except ProcessLookupError:
            return None
        except OSError as error:
            raise ProcessUnreadable(f"getsid({pid}) failed: {error}") from None

    def read(self, pid: int) -> ProcessInfo | None:
        """Read a same-user process, None when it is gone; another user's process is unreadable."""
        ctypes = self._ctypes
        buffer = ctypes.create_string_buffer(BSDINFO_SIZE)
        written = self._libproc.proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, buffer, BSDINFO_SIZE)
        if written != BSDINFO_SIZE:
            code = ctypes.get_errno()
            if written <= 0 and code == errno.ESRCH:
                return None
            raise ProcessUnreadable(f"proc_pidinfo({pid}) wrote {written} bytes, errno {code}")
        sid = self.session_of(pid)
        if sid is None:
            return None
        info = decode_bsdinfo(buffer.raw, sid)
        if info is not None and info.pid != pid:
            raise ProcessUnreadable(f"proc_pidinfo({pid}) returned pid {info.pid}")
        return info

    def kill(self, pid: int) -> str:
        """SIGKILL one validated pid."""
        return send_kill(pid)

    def kill_group(self, pgid: int) -> str:
        """SIGKILL one validated process group."""
        return send_kill(pgid, group=True)


def parse_proc_stat(text: str, pid: int, boot_unix_s: float, ticks_per_s: int) -> ProcessInfo | None:
    """Parse `/proc/<pid>/stat`; the command name ends at the last `)`, and a zombie reads as gone."""
    open_index, close_index = text.find("("), text.rfind(")")
    if open_index < 0 or close_index < open_index:
        raise ProcessUnreadable(f"/proc/{pid}/stat cannot be parsed")
    fields = text[close_index + 2:].split()
    if len(fields) < 20:
        raise ProcessUnreadable(f"/proc/{pid}/stat has too few fields")
    if fields[0] in ("Z", "X", "x"):
        return None
    try:
        ppid, pgid, sid, start_ticks = int(fields[1]), int(fields[2]), int(fields[3]), int(fields[19])
    except ValueError:
        raise ProcessUnreadable(f"/proc/{pid}/stat holds a non-numeric field") from None
    return ProcessInfo(pid, pgid, sid, str(start_ticks), boot_unix_s + start_ticks / ticks_per_s,
                       text[open_index + 1:close_index], ppid)


class LinuxProcessTable:
    """The Linux process table through /proc, which holds each process's session id."""

    def __init__(self, proc: Path = Path("/proc")) -> None:
        self._proc = proc
        self._ticks_per_s = os.sysconf("SC_CLK_TCK")
        self._boot_unix_s = _linux_boot_unix_s(proc)

    def pids(self) -> list[int] | None:
        """List every pid, or None when /proc cannot be read."""
        try:
            return sorted(int(entry.name) for entry in self._proc.iterdir() if entry.name.isdigit())
        except OSError:
            return None

    def _stat(self, pid: int) -> ProcessInfo | None:
        try:
            text = (self._proc / str(pid) / "stat").read_text(encoding="utf-8", errors="replace")
        except (FileNotFoundError, ProcessLookupError):
            return None
        except OSError as error:
            raise ProcessUnreadable(f"/proc/{pid}/stat: {error}") from None
        return parse_proc_stat(text, pid, self._boot_unix_s, self._ticks_per_s)

    def session_of(self, pid: int) -> int | None:
        """Return a live pid's session id, or None once it is gone or a zombie."""
        info = self._stat(pid)
        return None if info is None else info.sid

    def read(self, pid: int) -> ProcessInfo | None:
        """Read one process, or None once it is gone or a zombie."""
        return self._stat(pid)

    def kill(self, pid: int) -> str:
        """SIGKILL one validated pid."""
        return send_kill(pid)

    def kill_group(self, pgid: int) -> str:
        """SIGKILL one validated process group."""
        return send_kill(pgid, group=True)


def _linux_boot_unix_s(proc: Path) -> float:
    """Return the boot time /proc/stat records, in whole Unix seconds."""
    for line in (proc / "stat").read_text(encoding="utf-8").splitlines():
        if line.startswith("btime "):
            return float(line.split()[1])
    raise OSError("/proc/stat has no btime line")


def send_kill(target: int, group: bool = False) -> str:
    """SIGKILL a validated pid or process group: `sent`, `gone` or `refused`.

    SIGKILL and killpg are resolved here, not at import, so the module loads on Windows.
    """
    if target <= 1:
        raise ValueError(f"refusing to signal {target}")
    try:
        if group:
            os.killpg(target, signal.SIGKILL)
        else:
            os.kill(target, signal.SIGKILL)
    except ProcessLookupError:
        return "gone"
    except PermissionError:
        return "refused"
    return "sent"


# Windows access rights, Toolhelp flags, wait results and errors the process table uses.
PROCESS_TERMINATE = 0x0001
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
SYNCHRONIZE = 0x0010_0000
TH32CS_SNAPPROCESS = 0x2
WAIT_OBJECT_0 = 0x0
WAIT_TIMEOUT = 0x102
ERROR_ACCESS_DENIED = 5
ERROR_INVALID_PARAMETER = 87
ERROR_NO_MORE_FILES = 18
# A FILETIME counts 100 ns intervals since 1601-01-01; this many seconds separate that from the Unix epoch.
FILETIME_EPOCH_OFFSET_S = 11_644_473_600
# The exit code windows-process-job.py and the deadline case give TerminateProcess, as a step deadline does.
WINDOWS_KILL_EXIT_CODE = 124


def filetime_to_unix_s(filetime: int) -> float:
    """Convert a raw FILETIME value to seconds since the Unix epoch."""
    return filetime / 10_000_000 - FILETIME_EPOCH_OFFSET_S


class Kernel32:
    """The kernel32 calls WindowsProcessTable makes; it is built only on Windows, so tests inject a fake."""

    def __init__(self) -> None:
        import ctypes
        from ctypes import wintypes
        self._ctypes = ctypes

        class ProcessEntry(ctypes.Structure):
            """PROCESSENTRY32W from <tlhelp32.h>."""

            _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
                        ("th32ProcessID", wintypes.DWORD), ("th32DefaultHeapID", ctypes.c_size_t),
                        ("th32ModuleID", wintypes.DWORD), ("cntThreads", wintypes.DWORD),
                        ("th32ParentProcessID", wintypes.DWORD), ("pcPriClassBase", ctypes.c_long),
                        ("dwFlags", wintypes.DWORD), ("szExeFile", ctypes.c_wchar * 260)]

        self._entry = ProcessEntry
        self._filetime = wintypes.FILETIME
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        handle = wintypes.HANDLE
        signatures = {
            "CreateToolhelp32Snapshot": ((wintypes.DWORD, wintypes.DWORD), handle),
            "Process32FirstW": ((handle, ctypes.POINTER(ProcessEntry)), wintypes.BOOL),
            "Process32NextW": ((handle, ctypes.POINTER(ProcessEntry)), wintypes.BOOL),
            "OpenProcess": ((wintypes.DWORD, wintypes.BOOL, wintypes.DWORD), handle),
            "GetProcessTimes": ((handle,) + (ctypes.POINTER(wintypes.FILETIME),) * 4, wintypes.BOOL),
            "WaitForSingleObject": ((handle, wintypes.DWORD), wintypes.DWORD),
            "TerminateProcess": ((handle, wintypes.UINT), wintypes.BOOL),
            "CloseHandle": ((handle,), wintypes.BOOL),
        }
        for name, (argtypes, restype) in signatures.items():
            function = getattr(kernel, name)
            function.argtypes, function.restype = argtypes, restype
        self._kernel = kernel

    def snapshot(self) -> list[tuple[int, int, str]]:
        """Every process Toolhelp lists, as (pid, parent pid, image name); OSError when the list is incomplete."""
        ctypes = self._ctypes
        snapshot = self._kernel.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
        if snapshot is None or snapshot == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            entry = self._entry()
            entry.dwSize = ctypes.sizeof(entry)
            rows = []
            listed = self._kernel.Process32FirstW(snapshot, ctypes.byref(entry))
            while listed:
                rows.append((int(entry.th32ProcessID), int(entry.th32ParentProcessID), entry.szExeFile))
                listed = self._kernel.Process32NextW(snapshot, ctypes.byref(entry))
            error = ctypes.get_last_error()
            if error != ERROR_NO_MORE_FILES:
                # When: the walk stopped for another reason, the list may miss processes.
                raise ctypes.WinError(error)
            return rows
        finally:
            self._kernel.CloseHandle(snapshot)

    def open_process(self, access: int, pid: int):
        """Open `pid`: a handle, None when no such process exists, PermissionError when it is refused."""
        ctypes = self._ctypes
        handle = self._kernel.OpenProcess(access, False, pid)
        if handle:
            return handle
        error = ctypes.get_last_error()
        if error == ERROR_INVALID_PARAMETER:
            return None
        if error == ERROR_ACCESS_DENIED:
            raise PermissionError(f"OpenProcess({pid}) was denied")
        raise ctypes.WinError(error)

    def creation_time(self, handle) -> int:
        """The raw FILETIME creation time of an open process."""
        ctypes = self._ctypes
        times = [self._filetime() for _ in range(4)]
        if not self._kernel.GetProcessTimes(handle, *(ctypes.byref(value) for value in times)):
            raise ctypes.WinError(ctypes.get_last_error())
        return (int(times[0].dwHighDateTime) << 32) | int(times[0].dwLowDateTime)

    def is_alive(self, handle) -> bool:
        """Whether an open process is still running: its handle is not yet signalled."""
        result = self._kernel.WaitForSingleObject(handle, 0)
        if result == WAIT_TIMEOUT:
            return True
        if result == WAIT_OBJECT_0:
            return False
        raise self._ctypes.WinError(self._ctypes.get_last_error())

    def terminate(self, handle, exit_code: int) -> bool:
        """TerminateProcess on an open process; whether the kernel accepted it."""
        return bool(self._kernel.TerminateProcess(handle, exit_code))

    def close(self, handle) -> None:
        """Close a handle this table opened."""
        self._kernel.CloseHandle(handle)


class WindowsProcessTable:
    """The Windows process table: Toolhelp for pids, parents and images, GetProcessTimes for identity.

    Windows has no sessions or process groups, so `sid` and `pgid` read 0; the raw creation time is
    the start token. The kernel32 calls are injectable, so the tests drive a fake on every host.
    """

    def __init__(self, api=None) -> None:
        self._api = api

    @property
    def api(self):
        """The kernel32 binding, built on first use so the table can be created on any host."""
        if self._api is None:
            self._api = Kernel32()
        return self._api

    def pids(self) -> list[int] | None:
        """List every pid, or None when the list cannot be read whole."""
        try:
            return sorted(pid for pid, _parent, _image in self.api.snapshot())
        except OSError:
            return None

    def read(self, pid: int) -> ProcessInfo | None:
        """Read one process; None when it is not listed or has exited, ProcessUnreadable when it is refused."""
        try:
            rows = self.api.snapshot()
        except OSError as error:
            raise ProcessUnreadable(f"the process list cannot be read: {error}") from error
        row = next((row for row in rows if row[0] == pid), None)
        if row is None:
            return None
        _pid, parent, image = row
        try:
            handle = self.api.open_process(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, pid)
        except OSError as error:
            raise ProcessUnreadable(f"pid {pid} cannot be opened: {error}") from error
        if handle is None:
            return None
        try:
            if not self.api.is_alive(handle):
                return None
            created = self.api.creation_time(handle)
        except OSError as error:
            raise ProcessUnreadable(f"pid {pid} cannot be read: {error}") from error
        finally:
            self.api.close(handle)
        return ProcessInfo(pid, 0, 0, str(created), filetime_to_unix_s(created), image, parent)

    def terminate(self, pid: int, start: str) -> str:
        """TerminateProcess(124) on `pid` only if it still has creation time `start`.

        Returns `sent`, `gone`, `stale` (the pid now names another process) or `refused`.
        """
        if pid <= 0 or pid == os.getpid():
            raise ValueError(f"refusing to terminate {pid}")
        try:
            handle = self.api.open_process(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, pid)
        except OSError:
            return "refused"
        if handle is None:
            return "gone"
        try:
            # The creation time is read on the handle that terminates, so the pid cannot be reused in between.
            if str(self.api.creation_time(handle)) != start:
                return "stale"
            return "sent" if self.api.terminate(handle, WINDOWS_KILL_EXIT_CODE) else "refused"
        except OSError:
            return "refused"
        finally:
            self.api.close(handle)


def make_process_table():
    """Return this host's process table, or None where runs are not supported."""
    if sys.platform == "darwin":
        return MacProcessTable()
    if sys.platform.startswith("linux"):
        return LinuxProcessTable()
    if sys.platform == "win32":
        return WindowsProcessTable()
    return None



# --- Log lines and result.json --------------------------------------------------------------

# The file layer is tracing_subscriber's default fmt layer without ANSI: an RFC 3339 UTC stamp,
# the level, any span context, the target, the message and the fields.
_STAMP = re.compile(r"(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?Z")
# Other events share the `memory` target, so the snapshot is selected by its message.
MEMORY_MARKER = " memory: memory snapshot "
RENDER_TIMING_MARKER = " render_timing: line=[render_timing] "
_LAP = re.compile(r"([A-Za-z0-9_]+)=([0-9]+(?:\.[0-9]+)?)ms")


def parse_utc_stamp(line: str) -> float | None:
    """Return the Unix time of the fmt layer's leading UTC stamp, or None."""
    match = _STAMP.match(line)
    if not match:
        return None
    whole = calendar.timegm(tuple(int(match[index]) for index in range(1, 7)) + (0, 0, 0))
    fraction = match[7] or "0"
    return whole + int(fraction) / 10 ** len(fraction)


def _field(text: str, name: str) -> str | None:
    match = re.search(r"(?:^|\s)" + re.escape(name) + r"=(\S*)", text)
    return None if match is None else match[1]


@dataclass(frozen=True)
class MemorySample:
    """One `memory snapshot` line; a process figure the host does not report is None."""

    unix_s: float
    process_resident_bytes: int | None
    renderer_total_bytes: int
    session_total_bytes: int
    # A checkpoint sample's identity and completeness; all None on a periodic sample.
    checkpoint_index: int | None = None
    checkpoint_label: str | None = None
    checkpoint_attempt: int | None = None
    checkpoint_complete: bool | None = None
    panes_total: int | None = None
    panes_sampled: int | None = None
    panes_contended: int | None = None
    grid_visible_bytes: int | None = None
    grid_history_bytes: int | None = None
    grid_alternate_bytes: int | None = None
    # Each renderer's glyph atlas facts, in breakdown order; empty on a line from an older build.
    glyph_atlases: tuple = ()
    # Row glyph cache storage summed over every renderer; None on a line from a build that lacks the field.
    renderer_row_glyph_cache_bytes: int | None = None
    # A checkpoint line's trim tags, written only by a build with the trim hook; None when absent.
    trimmed: bool | None = None
    trim_source: str | None = None
    trim_seq: int | None = None
    # Trim tags present on the line whose value did not parse, by name; a malformed tag is never read as absent.
    malformed_trim_tags: tuple = ()

    def totals(self) -> tuple:
        """The figures a checkpoint reading compares: two samples with equal totals are the same reading."""
        return (self.process_resident_bytes, self.renderer_total_bytes, self.session_total_bytes,
                self.panes_total, self.panes_sampled, self.panes_contended, self.grid_visible_bytes,
                self.grid_history_bytes, self.grid_alternate_bytes, self.renderer_row_glyph_cache_bytes)

    def grid_bytes_per_pane(self) -> float | None:
        """Visible, history and alternate grid bytes divided by the panes sampled, or None when a
        field is missing or no pane was sampled."""
        parts = (self.grid_visible_bytes, self.grid_history_bytes, self.grid_alternate_bytes)
        if any(part is None for part in parts) or not self.panes_sampled:
            return None
        return sum(parts) / self.panes_sampled


def _optional_count(fields: str, name: str) -> int | None:
    """An optional integer field: its value when present and all digits, otherwise None."""
    value = _field(fields, name)
    return int(value) if value and value.isdigit() else None


def _optional_text(fields: str, name: str) -> str | None:
    """An optional text field, without the quotes the log layer puts around a string value."""
    value = _field(fields, name)
    if value is None:
        return None
    return value[1:-1] if len(value) >= 2 and value[0] == value[-1] == '"' else value


# One renderer's `role[label]` identity and its six glyph atlas facts inside the quoted renderer breakdown.
# The identity opens each renderer's entry; the lazy gap stops at that renderer's own facts.
_GLYPH_ATLAS_FACTS = re.compile(
    r"([A-Za-z_][\w-]*\[[^\]]*\])[^;]*? "
    r"glyph_atlas_dim=(\d+) glyph_atlas_packed_pixels=(\d+) glyph_atlas_growths=(\d+) "
    r"glyph_atlas_evictions=(\d+) glyph_atlas_fit=(256|512|1024|2048|no_headroom|does_not_fit|evicted) "
    r"glyph_atlas_max_tile=(\d+)x(\d+)")


@dataclass(frozen=True)
class GlyphAtlasFacts:
    """One renderer's glyph atlas: its `role[label]` identity, dimension, packed area, growths, evictions,
    fit and largest tile."""

    renderer: str
    dim: int
    packed_pixels: int
    growths: int
    evictions: int
    fit: str
    max_tile: tuple


def parse_glyph_atlases(fields: str) -> tuple:
    """Every renderer's glyph atlas facts on a memory line, in order; empty when none are reported."""
    return tuple(
        GlyphAtlasFacts(renderer, int(dim), int(packed), int(growths), int(evictions), fit,
                        (int(width), int(height)))
        for renderer, dim, packed, growths, evictions, fit, width, height in _GLYPH_ATLAS_FACTS.findall(fields))


def parse_memory_line(line: str) -> MemorySample | None:
    """Parse one `memory snapshot` line; any other line, or one with a malformed total, is None."""
    position = line.find(MEMORY_MARKER)
    unix_s = parse_utc_stamp(line)
    if position < 0 or unix_s is None:
        return None
    fields = line[position + len(MEMORY_MARKER):]
    renderer, session = _field(fields, "renderer_total_bytes"), _field(fields, "session_total_bytes")
    if not (renderer and renderer.isdigit() and session and session.isdigit()):
        return None
    resident = _field(fields, "process_resident_bytes")
    if resident is not None and resident != "unsupported" and not resident.isdigit():
        return None
    resident_bytes = int(resident) if resident and resident.isdigit() else None
    complete = _field(fields, "checkpoint_complete")
    trimmed = _field(fields, "trimmed")
    raw_source, raw_seq = _field(fields, "trim_source"), _field(fields, "trim_seq")
    malformed = tuple(name for name, present, valid in (
        ("trimmed", trimmed is not None, trimmed in ("true", "false")),
        ("trim_source", raw_source is not None, bool(_optional_text(fields, "trim_source"))),
        ("trim_seq", raw_seq is not None, bool(raw_seq) and raw_seq.isdigit()),
    ) if present and not valid)
    return MemorySample(
        unix_s, resident_bytes, int(renderer), int(session),
        checkpoint_index=_optional_count(fields, "checkpoint_index"),
        checkpoint_label=_optional_text(fields, "checkpoint_label"),
        checkpoint_attempt=_optional_count(fields, "checkpoint_attempt"),
        checkpoint_complete={"true": True, "false": False}.get(complete) if complete else None,
        panes_total=_optional_count(fields, "panes_total"),
        panes_sampled=_optional_count(fields, "panes_sampled"),
        panes_contended=_optional_count(fields, "panes_contended"),
        grid_visible_bytes=_optional_count(fields, "grid_visible_bytes"),
        grid_history_bytes=_optional_count(fields, "grid_history_bytes"),
        grid_alternate_bytes=_optional_count(fields, "grid_alternate_bytes"),
        glyph_atlases=parse_glyph_atlases(fields),
        renderer_row_glyph_cache_bytes=_optional_count(fields, "renderer_row_glyph_cache_bytes"),
        trimmed={"true": True, "false": False}.get(trimmed) if trimmed else None,
        trim_source=_optional_text(fields, "trim_source"),
        trim_seq=_optional_count(fields, "trim_seq"),
        malformed_trim_tags=malformed)


@dataclass(frozen=True)
class CheckpointReading:
    """A checkpoint's authoritative memory sample, or why it has none it can report."""

    sample: MemorySample | None
    # Whether no attempt measured every pane; the sample is then the last partial attempt.
    partial: bool = False
    # Why the checkpoint has no reading, such as `conflicting samples`; None when it has one.
    problem: str | None = None


def checkpoint_memory(samples: Sequence[MemorySample], index: int) -> CheckpointReading | None:
    """Return checkpoint `index`'s authoritative sample, or None when no sample is tagged with it.

    Every complete attempt is checked first: two complete samples of one attempt with different
    totals make the whole index conflicting, whichever attempt would have been chosen, so a later
    attempt cannot hide an earlier conflict. Then the complete sample with the highest attempt wins;
    with none complete, the partial one with the highest attempt, marked `partial`, and two partial
    samples of that attempt with different totals are conflicting too. A line repeated with the same
    totals counts once. An untagged periodic sample is never returned.
    """
    tagged = [sample for sample in samples if sample.checkpoint_index == index
              and sample.checkpoint_attempt is not None and sample.checkpoint_complete is not None]
    if not tagged:
        return None
    complete = [sample for sample in tagged if sample.checkpoint_complete]
    complete_totals: dict[int, set[tuple]] = {}
    for sample in complete:
        complete_totals.setdefault(sample.checkpoint_attempt, set()).add(sample.totals())
    if any(len(totals) > 1 for totals in complete_totals.values()):
        return CheckpointReading(None, problem="conflicting samples")
    pool = complete or tagged
    attempt = max(sample.checkpoint_attempt for sample in pool)
    chosen = [sample for sample in pool if sample.checkpoint_attempt == attempt]
    if len({sample.totals() for sample in chosen}) > 1:
        return CheckpointReading(None, problem="conflicting samples")
    return CheckpointReading(chosen[0], partial=not complete)


def _log_lines(log_dir: Path) -> Iterable[str]:
    """Yield every line of the regular files directly under a run's `logs/` directory."""
    if not log_dir.is_dir():
        return
    for path in sorted(log_dir.iterdir()):
        if path.is_file() and not path.is_symlink():
            yield from path.read_text(encoding="utf-8", errors="replace").splitlines()


def read_memory_samples(log_dir: Path) -> list[MemorySample]:
    """Return a run's memory samples in time order; a short run may have none."""
    samples = [sample for sample in map(parse_memory_line, _log_lines(log_dir)) if sample]
    return sorted(samples, key=lambda sample: sample.unix_s)


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
CELL_LAYOUT_SHARD = "S1-S3-S6-S8-S12"
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
    reading = checkpoint_memory(run.memory, point["index"])
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
    """Why an S1-S3-S6-S8-S12 artifact is not evidence of the requested workflow run and head, or None."""
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
    if timing.get("shard") != CELL_LAYOUT_SHARD:
        return f"artifact shard {timing.get('shard')}, expected {CELL_LAYOUT_SHARD}"
    return None


def _cell_layout_memory(run_dir: Path) -> tuple[list[MemorySample], str | None]:
    """The App's memory lines from its own log files, or from the colored console output when those were not kept;
    returns ([], why) when a log cannot be read or a memory line cannot be parsed."""
    try:
        memory = read_memory_samples(run_dir / "scratch" / "logs")
        harness_log = run_dir / "01-harness.log"
        if not memory and harness_log.is_file():
            # When: the App's log files were not kept, its lines are read from the colored console output.
            lines = (ANSI_ESCAPE.sub("", line)
                     for line in harness_log.read_text(encoding="utf-8", errors="replace").splitlines())
            memory = sorted((sample for sample in map(parse_memory_line, lines) if sample),
                            key=lambda sample: sample.unix_s)
    except (OSError, ValueError, OverflowError) as error:
        # An unreadable log or an impossible timestamp makes this run invalid, not the whole decision.
        return [], f"memory log unreadable: {error}"
    return memory, None


def read_cell_layout_runs(artifact_root: Path, workflow_run: str, head_sha: str) -> dict[str, list[CellLayoutRun]]:
    """Read every head S12 timed run of one workflow run and head from downloaded `perf-comparison-*` artifacts.

    Only the S1-S3-S6-S8-S12 shard's artifacts are read. Each run carries the workflow run and head its artifact
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
        if named is None or named["shard"] != CELL_LAYOUT_SHARD or not artifact.is_dir():
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


def memory_at(samples: Sequence[MemorySample], unix_s: float,
              fresh_after_unix_s: float | None = None) -> MemorySample | None:
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


@dataclass(frozen=True)
class RenderTimingSample:
    """One `render_timing` line from a `--laps` run: its window and each lap in milliseconds."""

    unix_s: float
    window: str
    laps: dict[str, float]


def parse_render_timing(line: str) -> RenderTimingSample | None:
    """Parse the `line` field of a debug `render_timing` event; a malformed lap drops the line."""
    position = line.find(RENDER_TIMING_MARKER)
    unix_s = parse_utc_stamp(line)
    if position < 0 or unix_s is None:
        return None
    tokens = line[position + len(RENDER_TIMING_MARKER):].split()
    if not tokens or not tokens[0].startswith("window=") or len(tokens[0]) == len("window="):
        return None
    laps = {}
    for token in tokens[1:]:
        match = _LAP.fullmatch(token)
        if not match:
            return None
        laps[match[1]] = float(match[2])
    return RenderTimingSample(unix_s, tokens[0][len("window="):], laps) if laps else None


def read_render_timing(log_dir: Path) -> list[RenderTimingSample]:
    """Return a `--laps` run's render_timing samples."""
    return [sample for sample in map(parse_render_timing, _log_lines(log_dir)) if sample]


# The font crate's debug timing event: `render_timing: font operation operation="..." phase="..." ...`.
FONT_OPERATION_MARKER = " render_timing: font operation"
# The event-loop wait for a fallback face; only its returns are measured waits.
FALLBACK_RECEIVE = "fallback_receive"
# One `key=value` or `key="value"` field of an event.
_EVENT_FIELD = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)=("(?:[^"\\]|\\.)*"|\S+)')


@dataclass(frozen=True)
class FallbackWait:
    """One fallback_receive return: when it ended (the log stamp), how long it waited, and the stamp resolution."""

    end_unix_s: float
    elapsed_ms: float
    resolution_s: float


@dataclass
class FallbackLog:
    """A laps run's font operations, each classified once.

    `waits` are well-formed fallback_receive returns, `entries` well-formed fallback_receive enters, `other` the
    valid records of every other operation by name, and `unparsed` the malformed fallback_receive records.
    """

    waits: list[FallbackWait] = field(default_factory=list)
    entries: int = 0
    other: dict[str, int] = field(default_factory=dict)
    unparsed: int = 0
    unmatched_enter: int = 0
    unmatched_return: int = 0


def _event_fields(text: str) -> dict[str, str]:
    """An event's fields after its message, with quotes removed."""
    return {match[1]: match[2][1:-1] if match[2].startswith('"') else match[2]
            for match in _EVENT_FIELD.finditer(text)}


def _stamp_resolution_s(line: str) -> float:
    """The resolution of a line's UTC stamp: one unit of its last fraction digit, or one second without one."""
    match = _STAMP.match(line)
    digits = len(match[7]) if match and match[7] else 0
    return 10.0 ** -digits


def _elapsed_ms(text: str | None) -> float | None:
    """A return's elapsed_ms when it is a finite number of at least zero, else None."""
    try:
        value = float(text) if text is not None else math.nan
    except ValueError:
        return None
    return value if math.isfinite(value) and value >= 0 else None


def parse_fallback_log(lines: Iterable[str]) -> FallbackLog:
    """Classify every `font operation` record of one run, in log order, and pair fallback_receive records.

    A fallback_receive enter closes at the next return; an enter followed by another enter, or by the log's end,
    is an unmatched enter, and a return with no open enter an unmatched return. Pairing never drops a wait.
    """
    log = FallbackLog()
    open_enter = False
    for line in lines:
        position = line.find(FONT_OPERATION_MARKER)
        if position < 0:
            continue
        fields = _event_fields(line[position + len(FONT_OPERATION_MARKER):])
        operation, phase = fields.get("operation"), fields.get("phase")
        if operation != FALLBACK_RECEIVE:
            if operation is not None and phase in ("enter", "return"):
                # When: another operation's record is well formed, it is counted by name, never unparsed.
                log.other[operation] = log.other.get(operation, 0) + 1
            continue
        stamp = parse_utc_stamp(line)
        elapsed = _elapsed_ms(fields.get("elapsed_ms"))
        if stamp is None or phase not in ("enter", "return") or (phase == "return" and elapsed is None):
            # When: the stamp, phase or a return's elapsed_ms is missing or malformed, the record is unparsed.
            log.unparsed += 1
            continue
        if phase == "enter":
            log.entries += 1
            log.unmatched_enter += open_enter
            open_enter = True
            continue
        log.waits.append(FallbackWait(stamp, elapsed, _stamp_resolution_s(line)))
        if open_enter:
            open_enter = False
        else:
            # When: no enter is open, this return pairs with nothing, but it is still a measured wait.
            log.unmatched_return += 1
    log.unmatched_enter += open_enter
    return log


def read_fallback_log(log_dir: Path) -> FallbackLog | None:
    """A laps run's font operations, its log files read in name order; None when the run left no logs directory."""
    if not log_dir.is_dir():
        return None
    return parse_fallback_log(_log_lines(log_dir))


# The App's adapter messages and the event each names.
ADAPTER_EVENTS = {"wgpu adapter selected": "selected", "wgpu adapter reused": "reused"}
# The adapter fields in the order the App logs them; values hold spaces, so a line splits at these keys.
ADAPTER_KEYS = ("backend", "name", "driver", "device_type", "software_rendering")
# Logged after the identity fields; it ends the last value and is not kept.
ADAPTER_TRAILING_KEYS = ("device_memory_policy",)


def parse_adapter_line(line: str) -> dict[str, object] | None:
    """Read the App's `wgpu adapter selected` or `wgpu adapter reused` line, or None for any other line.

    Each value runs from its key to the next known key, so a name or driver with spaces stays whole.
    A line missing a key, or whose software_rendering is not true or false, is None.
    """
    for message, event in ADAPTER_EVENTS.items():
        index = line.find(message + " ")
        if index >= 0:
            break
    else:
        return None
    rest = line[index + len(message):]
    starts: list[tuple[str, int]] = []
    position = 0
    for key in ADAPTER_KEYS + ADAPTER_TRAILING_KEYS:
        found = rest.find(f" {key}=", position)
        if found < 0:
            if key in ADAPTER_TRAILING_KEYS:
                continue
            return None
        starts.append((key, found))
        position = found + len(key) + 2
    fields = {key: rest[start + len(key) + 2:end].strip()
              for (key, start), (_next, end) in zip(starts, starts[1:] + [("", len(rest))])}
    if fields["software_rendering"] not in ("true", "false"):
        return None
    return {"event": event, "backend": fields["backend"], "name": fields["name"], "driver": fields["driver"],
            "device_type": fields["device_type"], "software_rendering": fields["software_rendering"] == "true"}


def read_renderer(log_dir: Path) -> dict[str, object] | None:
    """A run's wgpu adapter: its first `selected` line, else its first `reused` one; None when none was logged."""
    adapters = [adapter for adapter in map(parse_adapter_line, _log_lines(log_dir)) if adapter is not None]
    selected = [adapter for adapter in adapters if adapter["event"] == "selected"]
    return (selected or adapters or [None])[0]


def describe_renderer(renderer: Mapping | None) -> str:
    """Name an adapter as `<name> (<backend> <device type>, driver <driver>)`; none reads `unknown`."""
    if renderer is None:
        return "unknown"
    software = ", software rendering" if renderer.get("software_rendering") else ""
    return (f"{renderer.get('name')} ({renderer.get('backend')} {renderer.get('device_type')}, "
            f"driver {renderer.get('driver')}{software})")


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


def _monitor_ok(monitor: object) -> bool:
    """A measurement display gives its name or null, its refresh in millihertz or null, and its scale factor."""
    return (isinstance(monitor, dict)
            and all(key in monitor for key in ("name", "refresh_rate_millihertz", "scale_factor"))
            and (monitor["name"] is None or isinstance(monitor["name"], str))
            and (monitor["refresh_rate_millihertz"] is None or _is_int(monitor["refresh_rate_millihertz"]))
            and _is_number(monitor["scale_factor"]))


# The presenter's boolean fields; `software_render_mode` is a string or null.
PRESENTER_FLAGS = ("software_rendering", "software_render_degraded", "windows_gdi")


def _presenter_ok(presenter: object) -> bool:
    """A presenter names its software render mode or null and three booleans."""
    return (isinstance(presenter, dict) and "software_render_mode" in presenter
            and (presenter["software_render_mode"] is None or isinstance(presenter["software_render_mode"], str))
            and all(isinstance(presenter.get(key), bool) for key in PRESENTER_FLAGS))


def _finite_non_negative(value: object) -> bool:
    """A number that is finite and at least zero; NaN, infinity, a negative and a boolean are not."""
    return _is_number(value) and math.isfinite(value) and value >= 0


def _checkpoint_ok(point: object) -> bool:
    """A checkpoint names its index, label and time; its footprint file is a path or null.

    Its optional `fresh_after_unix_s` is a finite, non-negative time, its optional
    `frame_texture_bytes` a non-negative integer, and its optional sampling record a known state, a
    non-negative attempt count and a boolean.
    """
    return (isinstance(point, dict) and _is_int(point.get("index")) and isinstance(point.get("label"), str)
            and _is_number(point.get("unix_s"))
            and (point.get("footprint_file") is None or isinstance(point.get("footprint_file"), str))
            and ("fresh_after_unix_s" not in point or _finite_non_negative(point["fresh_after_unix_s"]))
            and ("frame_texture_bytes" not in point
                 or (_is_int(point["frame_texture_bytes"]) and point["frame_texture_bytes"] >= 0))
            and ("sampling" not in point or point["sampling"] in CHECKPOINT_SAMPLING_STATES)
            and ("attempts" not in point or (_is_int(point["attempts"]) and point["attempts"] >= 0))
            and ("last_attempt_complete" not in point or isinstance(point["last_attempt_complete"], bool))
            and ("atlas_readings" not in point
                 or (isinstance(point["atlas_readings"], list)
                     and all(map(_atlas_reading_ok, point["atlas_readings"])))))


def _atlas_reading_ok(reading: object) -> bool:
    """One sampling attempt's atlas reading: its attempt, the main window's native label or null, each live
    window's counted glyph atlas growths by native label or null, and the closed windows' count or null."""
    if not isinstance(reading, dict) or not _is_int(reading.get("attempt")):
        return False
    counted = reading.get("counted_glyph_atlas_growths")
    closed = reading.get("closed_glyph_atlas_growths")
    return ((reading.get("main_window") is None or isinstance(reading.get("main_window"), str))
            and (counted is None or (isinstance(counted, dict)
                                     and all(isinstance(label, str) and _count_ok(value)
                                             for label, value in counted.items())))
            and (closed is None or _count_ok(closed)))


# The longest dispatches a phase records with their times; a harness that predates them records none.
SLOW_DISPATCH_LIMIT = 64
SLOW_DISPATCH_KEYS = ("start_unix_s", "end_unix_s", "ms")
# Each phase field's check; a field the result leaves out is a metric the harness does not have.
_PHASE_FIELDS = {
    "cpu_user_s": _is_number, "cpu_system_s": _is_number, "presented_frames": _is_int,
    "redraw_requested": _is_int, "dispatch_ms": _numbers, "present_interval_ms": _numbers,
    "allocations_per_frame": lambda value: value is None or _numbers(value),
    "slow_dispatches": lambda value: isinstance(value, list) and all(
        isinstance(item, dict) and all(_finite_non_negative(item.get(key)) for key in SLOW_DISPATCH_KEYS)
        for item in value),
    "dispatch_count": lambda value: _is_int(value) and value >= 0,
    # The logical updates S10's stream phase played; a harness older than the field leaves it out.
    "updates": lambda value: _is_int(value) and value > 0,
}

# result.json's `frame_counters`: whether the binary has the perf-counters feature and the run forced the gate on.
FRAME_COUNTER_STATES = ("unsupported", "off", "on")
# With the gate on, each phase's deltas: per section, its integer counts, then its histograms. A histogram's
# unit is its name's suffix, and the unit fixes its bucket bounds; the last bucket is the overflow.
FRAME_COUNTER_FIELDS = {
    "window": (("attempts", "presented", "cached", "settled", "retry", "surface_retry", "stopped", "failed",
                "contention_parser", "contention_images", "defer_timeout", "defer_contention",
                # defer_sync counts frames held by a visible open synchronized update; a base older than it shows n/a.
                "defer_sync", "defer_streaming",
                # stream_clock_exempt counts settled hardware keypress attempts that kept the streaming clock; a
                # base older than it shows n/a.
                "stream_clock_exempt",
                # The display-link counts: accepted ticks, tick-authorized admissions and ceiling fallbacks. They
                # read 0 off macOS and never prove display-phase alignment; a base older than them shows n/a.
                "display_link_ticks", "display_link_admissions", "display_link_fallbacks",
                "contention_retry_armed",
                # dirt_ack_dropped counts receipts dropped at a collection; a base older than it shows n/a.
                "dirt_ack_dropped", "native_request_redraw", "user_request_redraw", "redraw_requested"),
               ("present_interval_ms", "handler_ms", "flush_to_redraw_ms")),
    "app": (("wake_init", "wake_poll", "wake_wait_cancelled", "wake_resume_time", "wake_user", "ui_parser_locks",
             "fg_probe_calls", "fg_probe_panes", "fg_worker_probes", "fg_worker_panes", "fg_results_stale",
             "native_request_redraw_unregistered"),
            # fg_probe_* is the retired event-loop probe, a real 0 on a head that probes on the worker.
            ("about_to_wait_ms", "user_event_ms", "new_events_ms", "ui_parser_wait_us", "fg_probe_us",
             "fg_worker_probe_us")),
    "vt": (("parse_bytes", "batches", "flushes", "flushes_untargeted", "flushes_coalesced", "flushes_suppressed",
            # sync_timeouts counts synchronized updates released at the 150 ms bound; a base older than it shows n/a.
            "sync_timeouts"),
           ("parser_lock_wait_us", "parser_lock_hold_us", "parse_us")),
    "renderer": (("vertex_bytes", "index_bytes", "damage_permille_sum", "damaged_frames",
                  # damage_waste_permille_sum is the union rect's share minus what its parts cover, over
                  # damaged_frames; a base older than it shows n/a.
                  "damage_waste_permille_sum", "software_frames",
                  "gpu_frames", "row_cache_hits", "row_cache_misses", "shape_requests", "full_frames",
                  # Partial-assembly counters: presented partial frames, partial plans reassembled Full and
                  # cells hashed into row-cache keys; a base older than them shows n/a.
                  "partial_frames", "partial_fallbacks", "row_cells_hashed",
                  # row_cache_invalidate_us is summed microseconds kept as a plain count, not a histogram.
                  "row_cache_invalidate_visits", "row_cache_invalidate_us", "recolor_glyphs_visited",
                  # font_fallback_applies is supporting evidence; a base older than the counter shows n/a.
                  "font_fallback_applies",
                  # Render-attempt and font-preparation counters; every _ns field is summed nanoseconds.
                  "shape_ns", "raster_ns", "raster_calls", "raster_tiles", "font_generation_applies",
                  "font_prepare_ns", "font_generation_prepare_ns", "render_attempts", "render_attempts_presented",
                  "render_attempt_ns", "render_attempt_shape_ns", "render_attempt_raster_ns",
                  "render_attempt_shape_requests", "render_attempt_raster_calls", "render_attempt_raster_tiles",
                  "apply_attempts", "apply_attempts_presented", "apply_attempt_ns", "apply_attempt_shape_ns",
                  "apply_attempt_raster_ns", "apply_attempt_shape_requests", "apply_attempt_raster_calls",
                  "apply_attempt_raster_tiles",
                  # Glyph atlas growths and growths no frame presented; a base older than them shows n/a.
                  "glyph_atlas_growths", "atlas_growth_abandoned",
                  # Tab titles and chrome runs drawn from their caches (reuses) and shaped on a miss
                  # (prepares, whose requests also count in shape_requests); a base older than them shows n/a.
                  "tab_title_reuses", "tab_title_prepares", "chrome_run_reuses", "chrome_run_prepares",
                  # The row-run shaping diagnostic's 16 counters; a base older than them shows n/a.
                  "row_run_shape_calls", "row_run_shape_ok", "row_run_shape_failed", "row_run_shape_ns",
                  "row_run_shape_first", "row_run_shape_repeats", "row_run_shape_same_pass_repeats",
                  "row_run_shape_repeat_ns", "row_run_shape_unstable", "row_run_shape_retry_repeats",
                  "row_run_unpresented_calls", "row_run_unpresented_ns", "row_run_identity_resets",
                  "row_run_shape_overflows", "row_run_pass_overflows", "row_run_diag_ns"),
                 ("assembly_us", "atlas_growth_to_present_ms")),
}
HISTOGRAM_BOUNDS = {"ms": [4, 7, 9, 12, 17, 25, 34, 50, 100], "us": [10, 50, 100, 500, 1000, 5000]}
# A histogram's `sum_us` is an exact integer in microseconds whatever its unit; this converts it to the unit.
MICROSECONDS_PER_UNIT = {"ms": 1000, "us": 1}


def frame_counter_state(data: Mapping) -> object:
    """The result's frame_counters state; a result without the key comes from a harness built without the feature."""
    return data.get("frame_counters", "unsupported")


def _count_ok(value: object) -> bool:
    return _is_int(value) and value >= 0


def _histogram_problem(value: object, unit: str) -> str | None:
    """Why a value is not a histogram in `unit`, or None when it is one."""
    if not isinstance(value, dict):
        return "is not a histogram object"
    bounds = HISTOGRAM_BOUNDS[unit]
    if value.get("unit") != unit:
        return f"has unit {value.get('unit')!r}, not {unit!r}"
    if value.get("bounds") != bounds:
        return f"has bounds {value.get('bounds')!r}, not {bounds}"
    counts = value.get("counts")
    if not isinstance(counts, list) or len(counts) != len(bounds) + 1 or not all(_count_ok(item) for item in counts):
        return f"counts is not {len(bounds) + 1} non-negative integers"
    if "sum" in value:
        return "has the retired key sum; the contract's sum is sum_us, in microseconds"
    if not _count_ok(value.get("sum_us")):
        return "sum_us is not a non-negative integer of microseconds"
    return None


def frame_counter_problems(counters: object, partial: bool = False) -> list[str]:
    """Check one phase's frame_counters object: every section and field, each of its type; [] when it is whole.

    `partial` accepts an absent section or field key, for a base built before the contract gained it; a key
    that is present must hold a value of its type, so a present null is always a problem.
    """
    if not isinstance(counters, dict):
        return ["is not an object"]
    problems = []
    for section, (counts, histograms) in FRAME_COUNTER_FIELDS.items():
        # When: the key is absent, an older base never had the section; a present null is malformed.
        if section not in counters:
            if not partial:
                problems.append(f"lacks the {section} section")
            continue
        body = counters[section]
        if not isinstance(body, dict):
            problems.append(f"the {section} section is {type(body).__name__}, not an object")
            continue
        for field_name in counts + histograms:
            if field_name not in body:
                if not partial:
                    problems.append(f"lacks {section}.{field_name}")
            elif field_name in counts and not _count_ok(body[field_name]):
                problems.append(f"{section}.{field_name} is not a non-negative integer")
            elif field_name in histograms:
                problem = _histogram_problem(body[field_name], field_name.rsplit("_", 1)[1])
                if problem:
                    problems.append(f"{section}.{field_name} {problem}")
    return problems


# Variants measured only in the counters set: their evidence is the counter record of injected episodes,
# so a timed, laps or alloc run of them would measure nothing comparable.
COUNTERS_ONLY_VARIANTS = frozenset({("S1", "atlas-retry")})
# S1/atlas-retry's recovery episodes: eight of frames A-D each.
ATLAS_RECOVERY_EPISODES = 8
ATLAS_RECOVERY_FRAMES = ("A", "B", "C", "D")
ATLAS_RECOVERY_FIELDS = ("attempts", "presented", "resets", "hits", "misses", "shapes", "atlas_dim")


def variant_sets(scenario_id: str, variant: str, sets: Sequence[tuple]) -> list[tuple]:
    """The run sets one selected variant takes: every set, or only the counters set for a counters-only variant."""
    if (scenario_id, variant) in COUNTERS_ONLY_VARIANTS:
        return [entry for entry in sets if entry[0] == "counters"]
    return list(sets)


def counters_only_problem(selected: Sequence[tuple[str, str]], counters_set: bool) -> str | None:
    """Why the selection cannot run: a counters-only variant selected when no counters set runs."""
    named = [f"{scenario_id}/{variant}" for scenario_id, variant in selected
             if (scenario_id, variant) in COUNTERS_ONLY_VARIANTS]
    if named and not counters_set:
        return (f"{', '.join(named)} runs only in the counters set; pass --counters with a head that "
                "declares perf-counters")
    return None


def atlas_recovery_problem(data: Mapping) -> str | None:
    """Why result.json's `atlas_recovery` cannot be read, or None when it can.

    Only a valid S1/atlas-retry run with frame counters on carries it, and such a run must: 8 episodes of
    A, B, C, D in order, one attempt each, A resetting without presenting, B-D presenting without a reset,
    and B-D at one atlas dimension. Every number is an exact integer (never a float or a boolean that
    compares equal), every counter is nonnegative, and the distinct key count and the atlas dimension are
    positive. Any other result must not carry it.
    """
    counters_only = (data.get("scenario"), data.get("variant")) in COUNTERS_ONLY_VARIANTS
    recovery = data.get("atlas_recovery")
    if not counters_only or data.get("frame_counters") != "on":
        if counters_only and data.get("status") == "valid":
            # When: a valid S1/atlas-retry result ran without counters, it measured nothing.
            return "S1/atlas-retry ran without frame counters"
        return None if recovery is None else "atlas_recovery on a result that is not an S1/atlas-retry counters run"
    if recovery is None:
        return "a valid S1/atlas-retry counters run has no atlas_recovery" if data.get("status") == "valid" else None
    if not isinstance(recovery, dict) or not _is_int(recovery.get("episodes")) \
            or recovery["episodes"] != ATLAS_RECOVERY_EPISODES or not _is_int(recovery.get("distinct_keys")) \
            or recovery["distinct_keys"] < 1 or not isinstance(recovery.get("records"), list):
        return "atlas_recovery needs integer episodes 8, a positive integer distinct_keys and a records list"
    records = recovery["records"]
    if len(records) != ATLAS_RECOVERY_EPISODES * len(ATLAS_RECOVERY_FRAMES):
        return f"atlas_recovery has {len(records)} records, not 32"
    recovered_dims = set()
    for index, record in enumerate(records):
        frame = ATLAS_RECOVERY_FRAMES[index % 4]
        if not isinstance(record, dict) or not _is_int(record.get("episode")) \
                or record["episode"] != index // 4 or record.get("frame") != frame:
            return f"atlas_recovery record {index} is not episode {index // 4} {frame}"
        if not all(_is_int(record.get(name)) for name in ATLAS_RECOVERY_FIELDS):
            return f"atlas_recovery record {index} lacks an integer field"
        if any(record[name] < 0 for name in ATLAS_RECOVERY_FIELDS) or record["atlas_dim"] < 1:
            return f"atlas_recovery record {index} has a negative count or a zero atlas dimension"
        if record["attempts"] != 1:
            return f"atlas_recovery record {index} has {record['attempts']} attempts"
        expected = (1, 0) if frame == "A" else (0, 1)
        if (record["resets"], record["presented"]) != expected:
            return f"atlas_recovery record {index} ({frame}) has resets {record['resets']}, presented {record['presented']}"
        if frame != "A":
            recovered_dims.add(record["atlas_dim"])
    if len(recovered_dims) != 1:
        return "atlas_recovery frames B-D draw at more than one atlas dimension"
    return None


def validate_result(data: object, harness_hash: str, process_exit_code: int | None, *,
                    counters: bool = False, partial_counters: bool = False,
                    platform_name: str = "darwin", latency_split_schema: int | None = None) -> list[str]:
    """Check result.json against schema version 1; an empty list means it can be read.

    A result whose `managed` is not true, or whose harness hash differs from the one this
    script passed, is a schema failure: it is a standalone run or another harness's run.
    `counters` says whether the run passed --counters: only then must the gate be on, and only
    with the gate on does every phase carry a whole frame_counters object. `partial_counters` lets a
    base's counters lack fields its older contract never had. `latency_split_schema` is the harness's
    declared split schema: with 1, every non-null latency object must carry the whole split contract; with
    None (an older harness) the split fields are neither required nor trusted.
    """
    if not isinstance(data, dict):
        return ["result.json is not an object"]
    problems = []
    version = data.get("schema_version")
    if not _is_int(version) or version != SCHEMA_VERSION:
        problems.append(f"schema_version is {version!r}, not {SCHEMA_VERSION}")
    if data.get("managed") is not True:
        problems.append(f"managed is {data.get('managed')!r}, not true")
    if data.get("harness_hash") != harness_hash:
        problems.append(f"harness_hash {data.get('harness_hash')!r} is not the hash passed, {harness_hash}")
    status = data.get("status")
    if not isinstance(status, str):
        problems.append("status is not a string")
    exit_code = data.get("exit_code")
    if not _is_int(exit_code):
        problems.append("exit_code is not an integer")
    elif process_exit_code is not None and exit_code != process_exit_code:
        problems.append(f"exit_code {exit_code} differs from the process exit {process_exit_code}")
    if status == "valid" and not isinstance(data.get("grid"), (dict, list)):
        problems.append("a valid result has no grid")
    state = frame_counter_state(data)
    if not isinstance(state, str) or state not in FRAME_COUNTER_STATES:
        problems.append(f"frame_counters is {state!r}, not one of {', '.join(FRAME_COUNTER_STATES)}")
    elif counters and state != "on":
        problems.append(f"frame_counters is {state!r}, but the run passed --counters")
    elif not counters and state == "on":
        problems.append("frame_counters is 'on', but the run did not pass --counters")
    phases = data.get("phases")
    if not isinstance(phases, list) or not all(isinstance(phase, dict) for phase in phases):
        problems.append("phases is not a list of objects")
        phases = []
    for phase in phases:
        name = phase.get("name")
        if not isinstance(name, str) or not _is_number(phase.get("start_unix_s")) \
                or not _is_number(phase.get("end_unix_s")):
            problems.append(f"phase {name!r} lacks a name or its start and end times")
        problems.extend(f"phase {name!r} field {key} has the wrong type"
                        for key, check in _PHASE_FIELDS.items() if key in phase and not check(phase[key]))
        # When: the gate was on, a phase's counters must be whole; otherwise a phase carries none.
        if state == "on":
            problems.extend(f"phase {name!r} frame_counters {problem}"
                            for problem in frame_counter_problems(phase.get("frame_counters"), partial_counters))
        elif "frame_counters" in phase:
            problems.append(f"phase {name!r} carries frame_counters, but the gate is {state!r}")
    latency = data.get("latency")
    if latency is not None and not (isinstance(latency, dict) and latency_values(latency) is not None
                                    and _is_int(latency.get("attributed")) and _is_int(latency.get("total"))
                                    and "coverage" in latency
                                    and (latency["coverage"] is None or _is_number(latency["coverage"]))):
        problems.append("latency needs samples, attributed, total and coverage")
    elif latency is not None and latency_split_schema == 1:
        problems.extend(f"latency {problem}" for problem in latency_split_problems(latency))
    throughput = data.get("throughput")
    if throughput is not None and not (isinstance(throughput, dict) and _is_int(throughput.get("bytes"))
                                       and _is_number(throughput.get("seconds"))):
        problems.append("throughput needs bytes and seconds")
    if data.get("uncover_ms") is not None and not _is_number(data.get("uncover_ms")):
        problems.append("uncover_ms is not a number")
    if data.get("scrollback_rows_retained") is not None and not _is_int(data.get("scrollback_rows_retained")):
        problems.append("scrollback_rows_retained is not an integer")
    if "checkpoint_memory" in data and data["checkpoint_memory"] not in CHECKPOINT_MEMORY_STATES:
        problems.append("checkpoint_memory is not supported or unsupported")
    if "hooks" in data and not (isinstance(data["hooks"], dict)
                                and data["hooks"].get("trim") in TRIM_HOOK_OUTCOMES):
        # When: a harness that records its hooks names an outcome this script does not know, it cannot be read.
        problems.append(f"hooks is {data['hooks']!r}, not {{'trim': one of {', '.join(TRIM_HOOK_OUTCOMES)}}}")
    checkpoints = data.get("checkpoints")
    if not isinstance(checkpoints, list) or not all(_checkpoint_ok(point) for point in checkpoints):
        problems.append("checkpoints is not a list of {index, label, unix_s, footprint_file}")
    elif status == "valid" and not any(point["label"] == "end" for point in checkpoints):
        problems.append("a valid result has no checkpoint labelled end")
    # An early or timed-out run writes null; only a valid run must also report true.
    if "finish_session_settled" not in data or not (data["finish_session_settled"] is None
                                                    or isinstance(data["finish_session_settled"], bool)):
        problems.append("finish_session_settled is not a boolean or null")
    # The measurement window's display after startup; absent or null when the harness did not report one.
    if data.get("monitor") is not None and not _monitor_ok(data["monitor"]):
        problems.append("monitor needs name, refresh_rate_millihertz and scale_factor of the documented types")
    # Windows and macOS runs record their presenter; when present each field has its type.
    presenter = data.get("presenter")
    if presenter is not None and not _presenter_ok(presenter):
        problems.append("presenter needs software_render_mode (a string or null) and the booleans "
                        "software_rendering, software_render_degraded and windows_gdi")
    if platform_name in ("win32", "darwin") and status == "valid" and presenter is None:
        # When: every Windows and macOS run records how it presented, so a valid one without the record cannot be
        # trusted; a macOS row counts only when that record shows the hardware path.
        host = "Windows" if platform_name == "win32" else "macOS"
        problems.append(f"a valid {host} result has no presenter")
    if data.get("trim_experiment") not in (None, TRIM_EXPERIMENT):
        problems.append(f"trim_experiment is {data.get('trim_experiment')!r}, not null or {TRIM_EXPERIMENT!r}")
    after_hook = data.get("trim_seq_after_hook")
    hooks = data.get("hooks")
    trim_outcome = hooks.get("trim") if isinstance(hooks, dict) else None
    if trim_outcome == "trimmed" and not (_is_int(after_hook) and after_hook > 0):
        # When: a trimmed hook must name its trim, or no covered sample can be checked against it.
        problems.append("hooks.trim is trimmed but trim_seq_after_hook is not a positive integer")
    elif trim_outcome != "trimmed" and after_hook is not None:
        problems.append(f"hooks.trim is {trim_outcome!r} but trim_seq_after_hook is {after_hook!r}, not null")
    if data.get("trim_experiment") is not None and "hooks" not in data:
        problems.append("a trim experiment result records no hooks")
    recovery_problem = atlas_recovery_problem(data)
    if recovery_problem is not None:
        problems.append(recovery_problem)
    if platform_name == "win32" and data.get("synthetic_occlusion") is True and not trim_experiment_run(data):
        # When: Windows reports no occlusion, so only the short S12 trim experiment may deliver one there.
        problems.append("synthetic_occlusion is true, but Windows reports no occlusion")
    notes = data.get("notes")
    if not isinstance(notes, list) or not all(isinstance(note, str) for note in notes):
        problems.append("notes is not a list of strings")
    return problems


def occlusion_invalidated(data: dict) -> bool:
    """Whether an invalid result names an occlusion, the one environmental invalidation the smoke retries.

    The harness writes its reason to `invalid_reason`, which is read with `notes` and `reason`;
    any mention of occlusion counts.
    """
    if data.get("status") == "valid":
        return False
    texts = [note for note in data.get("notes") or [] if isinstance(note, str)]
    texts.extend(str(data[key]) for key in ("reason", "invalid_reason") if data.get(key))
    return any("occlu" in text.lower() for text in texts)


# --- Home-write check -----------------------------------------------------------------------

# `breadcrumbs-<stamp>-<pid>-<n>.log`; the stamp may hold hyphens, so the pid is the second-to-last field.
_BREADCRUMB = re.compile(r"breadcrumbs-.+-([0-9]+)-[0-9]+\.(?:log|tmp)")
_MAIN_LOG = re.compile(r"sonicterm\.log(?:\..+)?")
OTHER_INSTANCE_COMMANDS = frozenset(("sonicterm-mac", "sonicterm-linux", "sonicterm-windows.exe"))
# Appended bytes beyond this cannot all be read, so such growth is never attributed elsewhere.
APPENDED_READ_LIMIT_BYTES = 64 * 1024 * 1024


# The home check reads at most this many entries and directory levels; past either bound it is unresolved.
HOME_MAX_ENTRIES = 200_000
HOME_MAX_DEPTH = 32


class HomeSnapshotUnresolved(Exception):
    """The home could not be read whole, so the check cannot clear a run of a write; the run is invalid."""


def snapshot_home(home: Path, max_entries: int = HOME_MAX_ENTRIES,
                  max_depth: int = HOME_MAX_DEPTH) -> dict[str, tuple] | None:
    """Map every file and link under the SonicTerm home to (size, mtime_ns, link text); None when absent.

    A symlink records its target text and, following it, the target's size and mtime, so a write
    through the link or a retargeted link is a change; a dangling link records only its text.
    Symlinked directories are walked too, each real directory once, so a link cycle ends. Past a
    bound, or at an entry or target it cannot read, it raises HomeSnapshotUnresolved instead of
    skipping anything. It only reads: nothing is created there, not even the directory.
    """
    if not home.is_dir():
        return None
    snapshot: dict[str, tuple] = {}
    visited = {os.path.realpath(home)}
    pending = [(home, "")]
    examined = 0
    while pending:
        directory, prefix = pending.pop()
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except FileNotFoundError:
            continue  # Removed while the walk ran; the later snapshot records the removal.
        except OSError as error:
            raise HomeSnapshotUnresolved(f"cannot list {directory}: {error}") from None
        for entry in entries:
            if examined >= max_entries:
                raise HomeSnapshotUnresolved(f"more than {max_entries} entries under {home}")
            examined += 1
            relative, path = prefix + entry.name, Path(entry.path)
            try:
                link_text = os.readlink(path) if entry.is_symlink() else None
            except FileNotFoundError:
                continue  # Removed while the walk ran.
            except OSError as error:
                raise HomeSnapshotUnresolved(f"cannot read the link {path}: {error}") from None
            try:
                # Follows a link, so a write through it shows in its target's size and time.
                info = os.stat(path)
            except OSError as error:
                if link_text is not None and error.errno in (errno.ENOENT, errno.ENOTDIR, errno.ELOOP):
                    snapshot[relative] = (None, None, link_text)  # Dangling: no target a write could reach.
                    continue
                if link_text is None and isinstance(error, FileNotFoundError):
                    continue  # Removed while the walk ran.
                raise HomeSnapshotUnresolved(f"cannot read {path}: {error}") from None
            if not stat.S_ISDIR(info.st_mode):
                snapshot[relative] = (info.st_size, info.st_mtime_ns, link_text)
                continue
            if link_text is not None:
                snapshot[relative] = (None, None, link_text)
            real = os.path.realpath(path)
            if real in visited:
                continue  # A link back into a directory already walked: recorded, not walked again.
            if relative.count("/") + 1 > max_depth:
                raise HomeSnapshotUnresolved(f"{path} is deeper than {max_depth} levels")
            visited.add(real)
            pending.append((path, relative + "/"))
    return snapshot


def _appended_mentions(path: Path, start: int, end: int, texts: Sequence[str]) -> bool | None:
    """Whether the bytes appended to a log mention this run; None when they cannot all be read."""
    if end - start > APPENDED_READ_LIMIT_BYTES:
        return None
    try:
        with path.open("rb") as stream:
            stream.seek(start)
            appended = stream.read(end - start)
    except OSError:
        return None
    return any(text.encode("utf-8") in appended for text in texts if text)


def home_violations(home: Path, before: dict[str, tuple] | None,
                    after: dict[str, tuple] | None, sentinel_ns: int, harness_pid: int | None,
                    other_instance: bool, scratch_texts: Sequence[str]) -> list[str]:
    """List the writes under the SonicTerm home this run cannot be cleared of; empty means none.

    A path is a candidate when it was added, removed or changed, or is newer than the sentinel;
    `.DS_Store` is ignored. A link's entry follows it to its target, so a write through it, or a
    retargeted link, is a candidate. A breadcrumb belongs to the pid in its name, so one naming the
    harness is a violation (the harness starts no breadcrumb writer). Growth of
    `logs/sonicterm.log*`, or a removal under `logs/`, belongs to another instance only when one
    is alive and the appended bytes lack this run's scratch path. Every other candidate is a violation.
    """
    if before is None and after is None:
        return []
    violations = []
    if before is None:
        violations.append(f"{home} was created")
    elif after is None:
        violations.append(f"{home} was removed")
    old, new = before or {}, after or {}
    for relative in sorted(set(old) | set(new)):
        name = relative.rsplit("/", 1)[-1]
        previous, current = old.get(relative), new.get(relative)
        newer = current is not None and current[1] is not None and current[1] > sentinel_ns
        if name == ".DS_Store" or (previous == current and not newer):
            continue
        change = ("added" if previous is None else "removed" if current is None
                  else "changed" if previous != current else "newer than the sentinel")
        breadcrumb = _BREADCRUMB.fullmatch(name) if relative.startswith("logs/breadcrumbs/") else None
        if breadcrumb:
            if harness_pid is None or int(breadcrumb[1]) == harness_pid:
                violations.append(f"{relative}: {change}; a breadcrumb that may be the harness's")
            continue
        under_logs = relative.startswith("logs/")
        if other_instance and under_logs and current is None:
            continue  # Another instance's log retention removed it.
        if other_instance and under_logs and current is not None and _MAIN_LOG.fullmatch(name) \
                and relative.count("/") == 1:
            start = previous[0] if previous is not None and previous[0] is not None else 0
            if current[0] is not None and current[0] > start and _appended_mentions(home / relative, start, current[0],
                                                         scratch_texts) is False:
                continue  # Appended by the live instance; this run's scratch path is not in it.
        violations.append(f"{relative}: {change}")
    return violations


def sonicterm_home(environ: Mapping[str, str]) -> Path:
    """The SonicTerm home the App uses: `HOME`, then `USERPROFILE`, then the account's home, plus `.sonicterm`.

    Like the App's `dirs_home`, a set variable is taken as it is, even when empty.
    """
    for name in ("HOME", "USERPROFILE"):
        if name in environ:
            return Path(environ[name]) / ".sonicterm"
    return Path.home() / ".sonicterm"


def other_instance_alive(table, harness_pid: int | None) -> bool:
    """Whether a readable live process other than the harness is a SonicTerm binary.

    A process list that cannot be read, or a process that cannot be identified, attributes
    nothing, so the stricter verdict stands.
    """
    if table is None:
        return False
    pids = table.pids()
    if pids is None:
        return False
    for pid in pids:
        if pid in (harness_pid, os.getpid()):
            continue
        try:
            info = table.read(pid)
        except ProcessUnreadable:
            continue
        if info is not None and info.command in OTHER_INSTANCE_COMMANDS:
            return True
    return False


# --- Harness hash and overlay ---------------------------------------------------------------

HARNESS_EXAMPLES = (HARNESS_EXAMPLE, ALLOC_EXAMPLE)
_EXAMPLE_HEADER = re.compile(r"\[\[\s*example\s*\]\]\s*(?:#.*)?")
_EXAMPLE_NAME = re.compile(r'\s*name\s*=\s*"([^"]+)"\s*(?:#.*)?')


def _table_spans(lines: Sequence[str]) -> list[tuple[int, int]]:
    """Return each table's line span: its header through its last key line, so comments before the next header stay put."""
    headers = [index for index, line in enumerate(lines) if line.startswith("[")]
    spans = []
    for position, start in enumerate(headers):
        limit = headers[position + 1] if position + 1 < len(headers) else len(lines)
        end = start + 1
        for index in range(start + 1, limit):
            stripped = lines[index].strip()
            if stripped and not stripped.startswith("#"):
                end = index + 1
        spans.append((start, end))
    return spans


def _example_name(lines: Sequence[str], start: int, end: int) -> str | None:
    """Return the name of the `[[example]]` table at a span, or None for any other table."""
    if not _EXAMPLE_HEADER.fullmatch(lines[start].strip()):
        return None
    for line in lines[start + 1:end]:
        match = _EXAMPLE_NAME.fullmatch(line.rstrip("\r\n"))
        if match:
            return match[1]
    return None


def harness_entries(manifest: str) -> str:
    """Return the text of the two harness `[[example]]` tables, in fixed order and with `\\n` line ends."""
    lines = manifest.splitlines(keepends=True)
    found = {}
    for start, end in _table_spans(lines):
        name = _example_name(lines, start, end)
        if name in HARNESS_EXAMPLES:
            if name in found:
                raise ValueError(f"[[example]] {name} is declared twice")
            found[name] = "".join(lines[start:end]).replace("\r\n", "\n").rstrip("\n") + "\n"
    missing = [name for name in HARNESS_EXAMPLES if name not in found]
    if missing:
        raise ValueError(f"{APP_MANIFEST} does not declare [[example]] {', '.join(missing)}")
    return "\n".join(found[name] for name in HARNESS_EXAMPLES)


def overlay_manifest(manifest: str, entries: str) -> str:
    """Give a manifest exactly the head's harness entries: unchanged when they match, else replaced or inserted.

    Every existing harness table is removed and the head's are appended; an array-of-tables
    element may follow any table, and every other line is kept as it was.
    """
    try:
        if harness_entries(manifest) == entries:
            return manifest
    except ValueError:
        pass  # Missing or duplicated entries are replaced below.
    lines = manifest.splitlines(keepends=True)
    removed = set()
    for start, end in _table_spans(lines):
        if _example_name(lines, start, end) in HARNESS_EXAMPLES:
            removed.update(range(start, end))
    kept = "".join(line for index, line in enumerate(lines) if index not in removed)
    return kept.rstrip() + "\n\n" + entries


def harness_hash(directory: Path, entries: str) -> str:
    """sha256 over the harness directory's sorted relative paths and bytes, then the entries' text.

    Every path, file and the entries are length-prefixed, so no two layouts share a byte
    stream. A symlink or an empty directory is refused: neither can be overlaid faithfully.
    """
    files = []
    for current, subdirectories, names in os.walk(directory):
        for name in subdirectories + names:
            if (Path(current) / name).is_symlink():
                raise ValueError(f"harness path {Path(current) / name} is a symlink")
        files.extend(Path(current) / name for name in names)
    if not files:
        raise ValueError(f"harness directory {directory} holds no files")
    digest = hashlib.sha256()
    for path in sorted(files, key=lambda path: path.relative_to(directory).as_posix()):
        relative = path.relative_to(directory).as_posix().encode("utf-8")
        data = path.read_bytes()
        digest.update(len(relative).to_bytes(8, "big") + relative)
        digest.update(len(data).to_bytes(8, "big") + data)
    text = entries.replace("\r\n", "\n").encode("utf-8")
    digest.update(len(text).to_bytes(8, "big") + text)
    return digest.hexdigest()


def _read_manifest(root: Path) -> str:
    """Read the app manifest without translating its line ends."""
    return (root / APP_MANIFEST).read_bytes().decode("utf-8")


COUNTERS_FEATURE = "perf-counters"
# Marks a tree whose renderer reports its frame texture's extent; the harness then writes frame_texture_bytes.
FRAME_TEXTURE_FEATURE = "perf-frame-texture"
# Marks a tree whose App can take a memory sample tagged with a perf checkpoint; the harness then samples at each.
CHECKPOINT_MEMORY_FEATURE = "perf-hook-checkpoint-memory"
# Marks a tree whose App has the S2 echo watch; the harness then splits S2/default latency at the flush.
ECHO_TRACE_FEATURE = "perf-echo-trace"
# Marks a tree whose App has the covered-window trim hook; the harness then asks it to trim at the cover.
TRIM_HOOK_FEATURE = "perf-hook-trim"
# Every perf feature a tree may declare, in the order a build passes them; later hooks append.
PERF_FEATURES = (COUNTERS_FEATURE, FRAME_TEXTURE_FEATURE, CHECKPOINT_MEMORY_FEATURE, ECHO_TRACE_FEATURE,
                 TRIM_HOOK_FEATURE)
# The app source a hook's method lives in, and the definition each hook feature needs there.
APP_SOURCE_DIRECTORY = "crates/sonicterm-app/src"
HOOK_METHODS = {CHECKPOINT_MEMORY_FEATURE: re.compile(r"^\s*pub fn __perf_checkpoint_memory\b", re.M),
                TRIM_HOOK_FEATURE: re.compile(r"^\s*pub fn __trim_covered_now\b", re.M)}
_TABLE_HEADER = re.compile(r"\s*\[\s*([^\[\]]+?)\s*\]\s*(?:#.*)?")


def declares_feature(manifest: str, feature: str) -> bool:
    """Whether the manifest's `[features]` table declares `feature`; a comment or another table does not."""
    key = re.compile(rf'\s*(?:{re.escape(feature)}|"{re.escape(feature)}")\s*=')
    table = None
    for line in manifest.splitlines():
        header = _TABLE_HEADER.fullmatch(line)
        if header:
            table = header[1]
        elif line.lstrip().startswith("["):
            # An array-of-tables header such as [[example]] ends the previous table.
            table = None
        elif table == "features" and key.match(line):
            return True
    return False


def declares_perf_counters(manifest: str) -> bool:
    """Whether the manifest's `[features]` table declares perf-counters; a comment or another table does not."""
    return declares_feature(manifest, COUNTERS_FEATURE)


LOGGING_LIB = "crates/sonicterm-logging/src/lib.rs"
# The logging API the counters harness calls; a definition at the start of a line, so a comment does not count.
_FILTERED_LOGGING_INIT = re.compile(r"^pub fn init_in_with_filter\b", re.M)


def tree_supports_counters(root: Path) -> bool:
    """Whether a worktree can build the overlaid harness with the perf-counters feature.

    The tree's app manifest must declare the feature, and its logging crate must define
    `pub fn init_in_with_filter`: the head's harness, which every tree builds, calls it in its
    perf-counters code. A tree that declares the feature without that function (an early counters
    commit) cannot compile the harness with it, so it is treated as having no counters, builds
    without the feature, and leaves the counters set to the head. Both checks read source text.
    """
    if not declares_perf_counters(_read_manifest(root)):
        return False
    try:
        logging_source = (root / LOGGING_LIB).read_bytes().decode("utf-8")
    except (OSError, UnicodeDecodeError):
        return False  # No readable logging crate: the harness's logging call cannot resolve either.
    return _FILTERED_LOGGING_INIT.search(logging_source) is not None


# Where something other than code starts in Rust source: a line or block comment, a raw or byte or
# plain string literal, or a quote that opens a character literal or a lifetime.
_RUST_NON_CODE = re.compile(r"""//|/\*|(?<![\w])b?r#*"|(?<![\w])b"|"|'""")
# A nested block comment's openings and closings.
_BLOCK_COMMENT_EDGE = re.compile(r"/\*|\*/")
# Inside a string: an escape (which may escape a quote) or the closing quote.
_STRING_EDGE = re.compile(r'\\.|"', re.S)
# A character literal starting at a quote; a quote that does not start one opens a lifetime.
_CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F]{1,6}\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")


def rust_code_only(source: str) -> str:
    """`source` with every comment (line, doc and nested block) and every string, byte-string,
    raw-string and character literal replaced by spaces. Line breaks are kept, so a line-anchored
    pattern still sees the code's lines, and only code can match it."""
    pieces = []
    position = 0
    length = len(source)
    while True:
        found = _RUST_NON_CODE.search(source, position)
        if found is None:
            pieces.append(source[position:])
            break
        start = found.start()
        pieces.append(source[position:start])
        token = found.group()
        if token == "//":
            end = source.find("\n", start)
            end = length if end < 0 else end
        elif token == "/*":
            depth, end = 0, length
            cursor = start
            while (edge := _BLOCK_COMMENT_EDGE.search(source, cursor)) is not None:
                depth += 1 if edge.group() == "/*" else -1
                cursor = edge.end()
                if depth == 0:
                    end = cursor
                    break
        elif token.endswith('"') and "r" in token:
            closer = '"' + "#" * token.count("#")
            close_at = source.find(closer, found.end())
            end = length if close_at < 0 else close_at + len(closer)
        elif token.endswith('"'):
            end = length
            cursor = found.end()
            while (edge := _STRING_EDGE.search(source, cursor)) is not None:
                cursor = edge.end()
                if edge.group() == '"':
                    end = cursor
                    break
        else:
            literal = _CHAR_LITERAL.match(source, start)
            if literal is None:
                pieces.append("'")  # A lifetime: the quote is code.
                position = start + 1
                continue
            end = literal.end()
        pieces.append(re.sub(r"[^\n]", " ", source[start:end]))
        position = end
    return "".join(pieces)


def tree_supports_hook(root: Path, feature: str, method_regex: re.Pattern) -> bool:
    """Whether a worktree can build the harness with the hook `feature`.

    The app manifest must declare the feature, and some app source file must define the method the
    harness calls under it (`method_regex`, anchored at a line start). Comments and string literals
    are blanked first (`rust_code_only`), so a mention in a comment, a doc line or a string does not
    count. A tree that declares the feature without the method cannot compile the harness with it.
    """
    if not declares_feature(_read_manifest(root), feature):
        return False
    for source in sorted((root / APP_SOURCE_DIRECTORY).rglob("*.rs")):
        try:
            text = source.read_bytes().decode("utf-8")
        except (OSError, UnicodeDecodeError):
            continue  # An unreadable file cannot define the method.
        if method_regex.search(rust_code_only(text)):
            return True
    return False


def tree_features(root: Path) -> tuple[str, ...]:
    """The perf features a worktree builds the harness with, in `PERF_FEATURES` order.

    perf-counters needs the logging API too (`tree_supports_counters`); perf-frame-texture needs
    only its declaration, since its accessor ships in the same change as the feature; each hook
    needs its method (`tree_supports_hook`). perf-echo-trace implies perf-counters, so a tree adds it
    only when it declares it and supports counters.
    """
    manifest = _read_manifest(root)
    features = []
    if tree_supports_counters(root):
        features.append(COUNTERS_FEATURE)
    if declares_feature(manifest, FRAME_TEXTURE_FEATURE):
        features.append(FRAME_TEXTURE_FEATURE)
    for feature, method in HOOK_METHODS.items():
        if tree_supports_hook(root, feature, method):
            features.append(feature)
    if COUNTERS_FEATURE in features and declares_feature(manifest, ECHO_TRACE_FEATURE):
        features.append(ECHO_TRACE_FEATURE)
    return tuple(feature for feature in PERF_FEATURES if feature in features)


def tree_harness_hash(root: Path) -> str:
    """Return the harness hash of one tree: its example directory and its two manifest entries."""
    return harness_hash(root / HARNESS_DIRECTORY, harness_entries(_read_manifest(root)))


def overlay_harness(head_root: Path, base_root: Path) -> None:
    """Copy the head's harness directory and `[[example]]` entries onto another tree; nothing under src/.

    The target's harness is deleted before the copy, so a target that resolves to the source
    or to the main checkout, which holds uncommitted work, is refused before anything is touched.
    """
    target_root = base_root.resolve()
    if target_root == head_root.resolve():
        raise ValueError(f"overlay target {base_root} is its own source {head_root}")
    if target_root == ROOT.resolve():
        raise ValueError(f"overlay target {base_root} is the main checkout {ROOT}")
    entries = harness_entries(_read_manifest(head_root))
    target = base_root / HARNESS_DIRECTORY
    if target.is_symlink():
        target.unlink()
    elif target.exists():
        shutil.rmtree(target)
    shutil.copytree(head_root / HARNESS_DIRECTORY, target, symlinks=True)
    manifest = _read_manifest(base_root)
    overlaid = overlay_manifest(manifest, entries)
    if overlaid != manifest:
        (base_root / APP_MANIFEST).write_bytes(overlaid.encode("utf-8"))



# --- Builds, scenario list and run command --------------------------------------------------

# Seconds a harness run may exceed its scenario timeout, for startup, finish_session and exit. Reaching
# the bound is unresolved cleanup whatever the harness's exit; classify_outcome says why.
RUN_MARGIN_S = 30
# The smoke caps every run's bound at this many seconds; reaching it is a run_step deadline like any other.
SMOKE_RUN_CAP_S = 100
LIST_TIMEOUT_S = 60
GIT_TIMEOUT_S = 300
_NAME = re.compile(r"[A-Za-z0-9_.-]+")


@dataclass(frozen=True)
class Scenario:
    """One entry of the harness's `--list` output."""

    id: str
    variants: tuple[str, ...]
    title: str
    timeout_s: int
    short_timeout_s: int
    # Per-variant caps on a --short comparison's valid runs, as (variant, cap) pairs; empty for none.
    run_caps: tuple[tuple[str, int], ...] = ()
    # The latency split schema the harness declares in `capabilities`; None for a harness that predates it.
    latency_split_schema: int | None = None

    def cap(self, variant: str) -> int | None:
        """This variant's short-mode run cap, or None when it has none."""
        return dict(self.run_caps).get(variant)


def capped_runs(scenario: Scenario, variant: str, requested: int, short: bool) -> int:
    """Valid runs a set takes: min(requested, cap) under --short (the PR budget), else every requested run."""
    cap = scenario.cap(variant)
    if not short or cap is None:
        # When: a release comparison, or an uncapped variant, runs everything it asked for.
        return requested
    return min(requested, cap)


def _run_caps(entry: dict, variants: Sequence[str]) -> tuple[tuple[str, int], ...] | None:
    """An entry's `run_caps` as pairs: absent is none; anything but listed variants mapped to counts >= 1 is None."""
    caps = entry.get("run_caps", {})
    if not isinstance(caps, dict):
        return None
    if not all(variant in variants and _is_int(cap) and cap >= 1 for variant, cap in caps.items()):
        return None
    return tuple(caps.items())


def build_argv(example: str, release: bool, counters: bool = False,
               features: Sequence[str] = ()) -> tuple[str, ...]:
    """Return the locked build of one harness example; Cargo's JSON messages name the built binary.

    `features` are the perf features a tree supports, in `PERF_FEATURES` order; `counters` alone is
    perf-counters, for a tree that declares only it.
    """
    profile = ("--release",) if release else ()
    chosen = tuple(features) or ((COUNTERS_FEATURE,) if counters else ())
    features = ("--features", ",".join(chosen)) if chosen else ()
    return ("cargo", "build", "--locked", *profile, "-p", "sonicterm-app", "--example", example,
            "--message-format=json-render-diagnostics", *features)


def build_passed(result) -> bool:
    """Whether a build step finished: PASS, or exit 0 after the gate cleaned a compiler's lingering helper.

    The gate reports CLEANED_NOT_NATURAL only for its reviewed compile-only steps, with verified custody.
    """
    return result.status == "PASS" or (result.status == "CLEANED_NOT_NATURAL" and result.exit_code == 0)


def artifact_executable(log_text: str, example: str) -> Path | None:
    """Return the executable Cargo reported for an example, whatever target directory it chose."""
    found = None
    for line in log_text.splitlines():
        if not line.startswith("{"):
            continue
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if not isinstance(message, dict) or message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target")
        if (isinstance(target, dict) and target.get("name") == example
                and "example" in (target.get("kind") or []) and isinstance(message.get("executable"), str)):
            found = Path(message["executable"])
    return found


def read_log(log_path: Path) -> str:
    """Read a step log as text; undecodable bytes are replaced, never fatal."""
    try:
        return log_path.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        return f"[perf-compare] cannot read {log_path}: {error}"


def log_tail(text: str, count: int = 12) -> str:
    """Return the last human-readable lines of a step log, without Cargo's JSON messages."""
    lines = [line for line in text.splitlines() if line.strip() and not line.startswith("{")]
    return "\n".join(lines[-count:])


# The latency split schemas this script can validate.
LATENCY_SPLIT_SCHEMAS = (1,)


def _latency_split_capability(data: dict) -> int | None:
    """The list's `capabilities.latency_split_schema`: None when the list has no capabilities (a harness that
    predates the split), else exactly a known schema; any other shape or value is refused, because a contract
    this script does not know cannot be validated."""
    if "capabilities" not in data:
        return None
    capabilities = data["capabilities"]
    if not isinstance(capabilities, dict) or set(capabilities) != {"latency_split_schema"}:
        raise ValueError(f"scenario list capabilities {capabilities!r} are not {{'latency_split_schema': 1}}")
    schema = capabilities["latency_split_schema"]
    if not _is_int(schema) or schema not in LATENCY_SPLIT_SCHEMAS:
        raise ValueError(f"scenario list latency_split_schema is {schema!r}, not one of {LATENCY_SPLIT_SCHEMAS}")
    return schema


def parse_scenario_list(text: str) -> list[Scenario]:
    """Parse `--list` output: the JSON object line among the launcher's header and footer."""
    data = None
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped.startswith("{"):
            continue
        try:
            candidate = json.loads(stripped)
        except ValueError:
            continue
        if isinstance(candidate, dict) and "scenarios" in candidate:
            data = candidate
    if data is None:
        raise ValueError("the harness printed no scenario list")
    if not _is_int(data.get("schema_version")) or data["schema_version"] != SCHEMA_VERSION:
        raise ValueError(f"scenario list schema_version is {data.get('schema_version')!r}")
    split_schema = _latency_split_capability(data)
    entries = data.get("scenarios")
    if not isinstance(entries, list) or not entries:
        raise ValueError("the scenario list is empty")
    scenarios = []
    for entry in entries:
        variants = entry.get("variants") if isinstance(entry, dict) else None
        if (not isinstance(entry, dict) or not isinstance(entry.get("id"), str) or not _NAME.fullmatch(entry["id"])
                or not isinstance(variants, list) or not variants
                or not all(isinstance(variant, str) and _NAME.fullmatch(variant) for variant in variants)
                or not isinstance(entry.get("title"), str)
                or not (_is_int(entry.get("timeout_s")) and entry["timeout_s"] > 0)
                or not (_is_int(entry.get("short_timeout_s")) and entry["short_timeout_s"] > 0)):
            raise ValueError(f"malformed scenario entry {entry!r}")
        caps = _run_caps(entry, variants)
        if caps is None:
            raise ValueError(f"malformed scenario entry {entry!r}")
        scenarios.append(Scenario(entry["id"], tuple(variants), entry["title"], entry["timeout_s"],
                                  entry["short_timeout_s"], caps, split_schema))
    if len({scenario.id for scenario in scenarios}) != len(scenarios):
        raise ValueError("the scenario list repeats an id")
    return scenarios


def select_scenarios(requested: Sequence[str], scenarios: Sequence[Scenario]) -> list[tuple[str, str]]:
    """Expand `--scenario` values in the order given; `all` is every default variant, and repeats are dropped."""
    if not requested:
        raise ValueError("no scenario selected")
    by_id = {scenario.id: scenario for scenario in scenarios}
    selected: list[tuple[str, str]] = []
    for value in requested:
        if value == "all":
            missing = [scenario.id for scenario in scenarios if "default" not in scenario.variants]
            if missing:
                raise ValueError(f"scenarios without a default variant: {', '.join(missing)}")
            pairs = [(scenario.id, "default") for scenario in scenarios]
        else:
            scenario_id, _separator, variant = value.partition("/")
            variant = variant or "default"
            if scenario_id not in by_id or variant not in by_id[scenario_id].variants:
                raise ValueError(f"unknown scenario {value!r}")
            pairs = [(scenario_id, variant)]
        selected.extend(pair for pair in pairs if pair not in selected)
    return selected


def select_laps_scenarios(requested: Sequence[str], scenarios: Sequence[Scenario],
                          selected: Sequence[tuple[str, str]]) -> list[tuple[str, str]]:
    """Expand `--laps-scenario` values: a bare ID is its default variant, and each must be listed and selected."""
    by_id = {scenario.id: scenario for scenario in scenarios}
    chosen: list[tuple[str, str]] = []
    for value in requested:
        scenario_id, _separator, variant = value.partition("/")
        pair = (scenario_id, variant or "default")
        if scenario_id not in by_id or pair[1] not in by_id[scenario_id].variants:
            raise ValueError(f"--laps-scenario names an unknown scenario {value!r}")
        if pair not in selected:
            raise ValueError(f"--laps-scenario {value!r} is not selected by --scenario")
        if pair not in chosen:
            chosen.append(pair)
    return chosen


def harness_argv(binary: Path, scenario_id: str, variant: str, harness_hash: str, scratch: Path, *,
                 short: bool = False, laps: bool = False, counters: bool = False) -> tuple[str, ...]:
    """Return one managed harness run's command line; `counters` makes the harness force the gate on."""
    flags = (("--short",) if short else ()) + (("--laps",) if laps else ()) + (("--counters",) if counters else ())
    return (str(binary), "--run", scenario_id, "--variant", variant, *flags, "--managed",
            "--harness-hash", harness_hash, str(scratch))


def harness_environment(base: Mapping[str, str]) -> dict[str, str]:
    """Return a run's environment: NO_COLOR and RUST_LOG are dropped; HOME and the rest are kept."""
    return {key: value for key, value in base.items() if key not in ("NO_COLOR", "RUST_LOG")}


def new_scratch_path(temp_root: Path, scenario_id: str, variant: str) -> Path:
    """Return a path under the OS temp directory that does not exist yet; the harness creates it."""
    for _attempt in range(100):
        candidate = temp_root / f"sonicterm-perf-{scenario_id}-{variant}-{os.urandom(6).hex()}"
        if not os.path.lexists(candidate):
            return candidate
    raise OSError(f"no unused scratch path under {temp_root}")


def run_timeout_s(scenario: Scenario, smoke: bool, short: bool = False) -> int:
    """Return a run's run_step bound: the scenario timeout plus a margin, capped in the smoke.

    A short comparison run (`--short`) uses the scenario's short timeout, without the smoke's cap.
    """
    if smoke:
        return min(scenario.short_timeout_s + RUN_MARGIN_S, SMOKE_RUN_CAP_S)
    if short:
        return scenario.short_timeout_s + RUN_MARGIN_S
    return scenario.timeout_s + RUN_MARGIN_S


FONT_SUFFIXES = (".ttf", ".otf", ".ttc")
LINUX_SHARED_ASSETS = Path("/usr/share/sonicterm/assets")


def resolve_asset_dir(binary: Path, cwd: Path, platform: str = sys.platform,
                      exists: Callable[[Path], bool] = os.path.exists) -> Path:
    """Mirror sonicterm-cfg's asset_dir() for a harness binary started in `cwd`.

    Packaged paths win: a macOS bundle's Contents/Resources/assets, then assets beside the
    executable, then Linux's /usr/share/sonicterm/assets. Only then the nearest `assets` among
    the working directory and its ancestors, or `<cwd>/assets` when there is none.
    """
    executable_dir = binary.parent
    candidates = [executable_dir / "assets"]
    if executable_dir.parent != executable_dir:
        candidates.insert(0, executable_dir.parent / "Resources" / "assets")
    for candidate in candidates:
        if exists(candidate):
            return candidate
    if platform.startswith("linux") and exists(LINUX_SHARED_ASSETS):
        return LINUX_SHARED_ASSETS
    for directory in (cwd, *cwd.parents):
        if exists(directory / "assets"):
            return directory / "assets"
    return cwd / "assets"


def asset_problem(binary: Path, source_root: Path, platform: str = sys.platform) -> str | None:
    """Explain why a harness started in its source tree would not load that tree's assets, or return None.

    The App loads fonts, themes and keymaps from asset_dir(), so the directory it resolves must be
    the tree's own `assets` and hold its tracked fonts; an ancestor's or a packaged one belongs to
    another tree, and none at all makes the App substitute another font.
    """
    tree = Path(os.path.realpath(source_root))
    expected = tree / "assets"
    resolved = resolve_asset_dir(binary, tree, platform)
    if resolved != expected:
        return f"asset_dir() would resolve {resolved}, not the tree's own {expected}"
    fonts = expected / "fonts"
    if not fonts.is_dir() or not any(entry.suffix.lower() in FONT_SUFFIXES for entry in fonts.iterdir()):
        return f"{fonts} holds no font, so the App would load another font"
    return None


# --- Run watcher: harness pid, sessions, checkpoints and the deadline case -----------------

FOOTPRINT_BINARY = "/usr/bin/footprint"
# The harness waits CHECKPOINT_WAIT (60 s, perf_scenarios/probe.rs) for `.done` and then measures again,
# so footprint's bound plus run_step's reap ends well inside that wait.
FOOTPRINT_TIMEOUT_S = 40
# After a deadline, run_step takes up to 10 s to reap the killed leader and 5 s to drain its output.
RUN_STEP_REAP_BOUND_S = 15
WATCH_INTERVAL_S = 0.2
_CHECKPOINT_REQUEST = re.compile(r"[0-9]+-[A-Za-z0-9_.-]+\.request")
_ROLE = re.compile(r"[A-Za-z0-9_-]+")
_PID_TEXT = re.compile(r"[0-9]+")
# The `footprint -j` layout was not observed on a run, so these keys are searched for defensively.
FOOTPRINT_KEYS = ("phys_footprint", "footprint", "total_footprint")
FORM_LABELS = {"app": "front application", "none": "no front application", "failed": "failed"}


def read_pid_file(path: Path) -> int | None:
    """Read a positive pid the harness wrote; a missing or partial file reads as None."""
    try:
        text = path.read_text(encoding="utf-8").strip()
    except (OSError, UnicodeDecodeError):
        return None
    return int(text) if _PID_TEXT.fullmatch(text) and int(text) > 0 else None


def checkpoint_paths(request: Path) -> tuple[str, Path, Path]:
    """Derive a checkpoint's stem, footprint JSON and done file from its request file name alone."""
    stem = request.name[:-len(".request")]
    return stem, request.with_name(stem + ".json"), request.with_name(stem + ".done")


def footprint_bytes(data: object, pid: int | None) -> int | None:
    """Find the target's footprint in `footprint -j` output, or None when no figure is recognisable.

    An object carrying one of FOOTPRINT_KEYS as an integer counts; the one naming the pid
    wins, and a lone candidate is taken as the target's.
    """
    candidates = []

    def walk(node: object) -> None:
        if isinstance(node, dict):
            if any(_is_int(node.get(key)) for key in FOOTPRINT_KEYS):
                candidates.append(node)
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for value in node:
                walk(value)

    walk(data)
    chosen = [node for node in candidates if node.get("pid") == pid] or (candidates if len(candidates) == 1 else [])
    if not chosen:
        return None
    return next(chosen[0][key] for key in FOOTPRINT_KEYS if _is_int(chosen[0].get(key)))


def command_matches(command: str, binary_name: str) -> bool:
    """Whether a kernel command name names a binary; Linux keeps 15 bytes of the name and macOS 16."""
    return bool(command) and len(command) >= min(15, len(binary_name)) and binary_name.startswith(command)


def validate_harness_leader(table, pid: int | None, launch_unix_s: float,
                            expected_command: str = HARNESS_EXAMPLE, expected_start: str | None = None) -> str | None:
    """Check that a pid is this run's harness: alive, leading its session and group, started after
    launch, running the binary this script launched and, given `expected_start`, still the accepted process."""
    if table is None or pid is None or pid <= 1 or pid == os.getpid():
        return f"harness pid {pid} cannot be validated"
    try:
        info = table.read(pid)
    except ProcessUnreadable as error:
        return f"harness pid {pid} is unreadable: {error}"
    if info is None:
        return f"harness pid {pid} is not alive"
    if expected_start is not None and info.start != expected_start:
        return (f"harness pid {pid} now names another process (start {info.start}, accepted {expected_start}); "
                f"its identity changed, so it was not signalled")
    if not info.sid == info.pgid == pid:
        return f"harness pid {pid} does not lead its session and process group"
    if info.start_unix_s < launch_unix_s - START_TOLERANCE_S:
        return f"harness pid {pid} started before this run's launch"
    if not command_matches(info.command, expected_command):
        return f"harness pid {pid} runs {info.command!r}, not {expected_command}"
    return None


def validate_windows_harness(table, pid: int | None, launch_unix_s: float, expected_command: str,
                             expected_start: str | None) -> str | None:
    """Check that a pid is still this run's accepted harness on Windows: alive, the same creation time,
    created after launch and running the launched image. Windows has no session or group to check."""
    if table is None or pid is None or pid <= 0 or pid == os.getpid() or expected_start is None:
        return f"harness pid {pid} cannot be validated"
    try:
        info = table.read(pid)
    except ProcessUnreadable as error:
        return f"harness pid {pid} is unreadable: {error}"
    if info is None:
        return f"harness pid {pid} is not alive"
    if info.start != expected_start:
        return (f"harness pid {pid} now names another process (created {info.start}, accepted {expected_start}); "
                f"its identity changed, so it was not terminated")
    if info.start_unix_s < launch_unix_s - START_TOLERANCE_S:
        return f"harness pid {pid} was created before this run's launch"
    if info.command.lower() != expected_command.lower():
        return f"harness pid {pid} runs {info.command!r}, not {expected_command}"
    return None


def validate_anchor(record: SessionRecord, table, launch_unix_s: float,
                    excluded_sids: Iterable[int] = ()) -> AckedSession | None:
    """Validate only a late record's anchor, so cleanup can still reach a session whose leader is gone.

    The live anchor keeps the leader's process group, whose id is the session id, so the id stays reserved.
    """
    try:
        anchor = table.read(record.anchor_pid)
    except ProcessUnreadable:
        return None
    if (anchor is None or anchor.sid != record.leader_pid or anchor.pgid != record.leader_pid
            or record.leader_pid in set(excluded_sids) or anchor.start_unix_s < launch_unix_s - START_TOLERANCE_S):
        return None
    return AckedSession(record.role, record.leader_pid, record.anchor_pid, "", anchor.start)


@dataclass
class RunContext:
    """What a run's watcher needs: scratch and evidence paths, the launch time, the process table and the gate."""

    scratch: Path
    evidence: Path
    launch_unix_s: float
    table: object
    gate: object
    excluded_sids: tuple[int, ...] = ()
    kill_at_go: bool = False
    # The launched binary's file name, which the deadline case checks against the kernel's command name.
    harness_command: str = HARNESS_EXAMPLE
    # The host's sys.platform: Windows records role programs, has no footprint and ends the harness by handle.
    platform: str = "darwin"


class RunWatcher:
    """Serve one run while it executes: read harness.pid, acknowledge sessions and answer checkpoints.

    In the deadline case it also ends the run as a step deadline would, once `go/0` exists.
    Each pass only reads files and the process table, so tests drive `poll` directly.
    """

    def __init__(self, context: RunContext) -> None:
        self.context = context
        self.harness_pid: int | None = None
        # The accepted harness's start token; the deadline kill signals only this exact process.
        self.harness_start: str | None = None
        self.acked: dict[str, AckedSession] = {}
        self.late: dict[str, AckedSession] = {}
        # Records the final scan found without a valid anchor: their members are listed, never signalled.
        self.unanchored: dict[str, SessionRecord] = {}
        # Windows role programs acknowledged by role.
        self.programs: dict[str, AckedProgram] = {}
        self.rejected: set[str] = set()
        self.problems: list[str] = []
        self.footprints: dict[str, dict[str, object]] = {}
        self.deadline: dict[str, object] = {"sent": False, "problem": None, "pid": None}
        # Step log 01 is the harness run's own.
        self._step_index = 1

    def poll(self) -> None:
        """One pass: accept harness.pid, then serve sessions and checkpoints, then the deadline case."""
        if self.harness_pid is None:
            self.harness_pid = self._accept_harness_pid()
        if self.harness_pid is not None:
            self._records(final=False)
            self._checkpoints()
        if (self.context.kill_at_go and not self.deadline["sent"] and self.deadline["problem"] is None
                and (self.context.scratch / "go" / "0").exists()):
            self._deadline_kill()

    def final_scan(self) -> None:
        """After the run: read the records once more; one that never parsed, or was never acknowledged, fails the run."""
        if self.harness_pid is None:
            self.harness_pid = read_pid_file(self.context.scratch / "harness.pid")
        self._records(final=True)
        directory = self.context.scratch / "checkpoints"
        if directory.is_dir():
            for request in sorted(directory.glob("*.request")):
                stem, _json_path, done_path = checkpoint_paths(request)
                if stem not in self.footprints:
                    self.footprints[stem] = {"checkpoint": stem, "bytes": None, "status": None, "exit_code": None,
                                             "output": "", "detail": "the run ended before this checkpoint was served"}
                    done_path.write_bytes(b"")

    def sessions_for_cleanup(self) -> list[AckedSession]:
        """Every session cleanup may act on: those acknowledged, then late ones whose anchor validated."""
        return list(self.acked.values()) + list(self.late.values())

    def _accept_harness_pid(self) -> int | None:
        """Accept harness.pid once it names a live process started after launch; a partial write reads as another pid."""
        pid = read_pid_file(self.context.scratch / "harness.pid")
        if pid is None or self.context.table is None:
            return pid
        try:
            info = self.context.table.read(pid)
        except ProcessUnreadable:
            return None
        if info is None or info.start_unix_s < self.context.launch_unix_s - START_TOLERANCE_S:
            return None
        self.harness_start = info.start
        return pid

    def _reject(self, role: str, problem: str) -> None:
        """Record a role's first problem; the final scan may revisit a role without repeating it."""
        if role not in self.rejected:
            self.rejected.add(role)
            self.problems.append(problem)

    def _records(self, final: bool) -> None:
        """Serve the records this host's panes write: role programs on Windows, PTY sessions elsewhere."""
        if self.context.platform == "win32":
            self._programs(final)
        else:
            self._sessions(final)

    def _programs(self, final: bool) -> None:
        """Acknowledge each validated role program; on the final scan an unacknowledged record fails the run.

        The job ends every program whatever is acknowledged, so a refused record is only a problem, never cleanup.
        """
        directory = self.context.scratch / "sessions"
        if not directory.is_dir():
            return
        for path in sorted(directory.glob("*.json")):
            role = path.stem
            if role in self.programs or role in self.rejected:
                continue
            if not _ROLE.fullmatch(role):
                self._reject(role, f"program record {path.name} has an unusable role name")
                continue
            try:
                record = parse_program_record(path.read_text(encoding="utf-8"), role)
            except (OSError, UnicodeDecodeError, ValueError) as error:
                # A record may still be being written; only the final scan treats it as broken.
                if final:
                    self._reject(role, f"program record {path.name} cannot be read: {error}")
                continue
            if final:
                self._reject(role, f"program {role} was not acknowledged before the run ended")
                continue
            acked, problem = validate_program(record, self.context.table, self.harness_pid,
                                              self.context.launch_unix_s,
                                              harness_command=self.context.harness_command)
            if acked is None:
                self._reject(role, problem or f"program {role} failed validation")
                continue
            self.programs[role] = acked
            acks = self.context.scratch / "acks"
            acks.mkdir(exist_ok=True)
            (acks / role).write_bytes(b"")

    def _sessions(self, final: bool) -> None:
        directory = self.context.scratch / "sessions"
        if not directory.is_dir():
            return
        excluded = set(self.context.excluded_sids) | ({self.harness_pid} if self.harness_pid else set())
        for path in sorted(directory.glob("*.json")):
            role = path.stem
            if role in self.acked or role in self.late or role in self.unanchored:
                continue
            # During the run a rejected record stays rejected; the final scan revalidates it, so a valid
            # anchor still lets cleanup reach that session.
            if role in self.rejected and not final:
                continue
            if not final and not _ROLE.fullmatch(role):
                self._reject(role, f"session record {path.name} has an unusable role name")
                continue
            try:
                record = parse_session_record(path.read_text(encoding="utf-8"), role)
            except (OSError, UnicodeDecodeError, ValueError) as error:
                # A record may still be being written; only the final scan treats it as broken.
                if final:
                    self._reject(role, f"session record {path.name} cannot be read: {error}")
                continue
            acked, problem = validate_session(record, self.context.table, self.context.launch_unix_s, excluded)
            if final:
                late = acked or validate_anchor(record, self.context.table, self.context.launch_unix_s, excluded)
                if late is not None:
                    self.late[role] = late
                elif record.leader_pid not in set(self.context.excluded_sids):
                    self.unanchored[role] = record
                self._reject(role, f"session {role} was not acknowledged before the run ended"
                             + (f": {problem}" if problem else ""))
            elif acked is None:
                self._reject(role, problem or f"session {role} failed validation")
            else:
                self.acked[role] = acked
                acks = self.context.scratch / "acks"
                acks.mkdir(exist_ok=True)
                (acks / role).write_bytes(b"")

    def _checkpoints(self) -> None:
        directory = self.context.scratch / "checkpoints"
        if not directory.is_dir():
            return
        for request in sorted(directory.glob("*.request")):
            stem, json_path, done_path = checkpoint_paths(request)
            if stem in self.footprints:
                continue
            self.footprints[stem] = self._footprint(stem, json_path)
            # Written whatever footprint found, but only once its process has exited and been reaped, so the
            # harness never measures while footprint still samples it.
            if self.footprints[stem]["reaped"]:
                done_path.write_bytes(b"")

    def _footprint(self, stem: str, json_path: Path) -> dict[str, object]:
        """Run `footprint` on the harness through run_step; a failure is recorded, never fatal to the run."""
        record: dict[str, object] = {"checkpoint": stem, "json": str(json_path), "bytes": None, "status": None,
                                     "exit_code": None, "output": "", "detail": "", "reaped": True}
        if self.context.platform != "darwin":
            # When: off macOS there is no footprint, so the checkpoint is answered at once and only logs measure it.
            record["detail"] = "no footprint on this host"
            return record
        if not _CHECKPOINT_REQUEST.fullmatch(stem + ".request"):
            record["detail"] = "unrecognised checkpoint request name; no footprint was taken"
            return record
        self._step_index += 1
        # The live task is measured, not a `--forkCorpse` copy: the harness waits for `.done` before its next
        # timed phase, so any pause of the target falls outside every timed phase, and nothing extra is forked.
        argv = (FOOTPRINT_BINARY, "-p", str(self.harness_pid), "-j", str(json_path))
        step = self.context.gate.Step(f"footprint-{stem}", argv, ("macos",), FOOTPRINT_TIMEOUT_S, "local", (), ())
        result = self.context.gate.run_step(step, self._step_index, self.context.evidence,
                                            self.context.evidence, dict(os.environ))
        # An exit status exists only once run_step reaped the process; a launch failure started none.
        reaped = result.exit_code is not None or result.status == "LAUNCH"
        record.update(status=result.status, exit_code=result.exit_code, log=str(result.log_path),
                      output=log_tail(read_log(result.log_path), 20), reaped=reaped)
        if not reaped:
            record["detail"] = (f"footprint {result.status} and its exit was never collected, so `.done` is withheld "
                                f"and the harness's checkpoint wait ends the run")
            return record
        if result.status != "PASS" or not json_path.is_file():
            record["detail"] = f"footprint {result.status}, exit {result.exit_code}"
            return record
        try:
            record["bytes"] = footprint_bytes(json.loads(json_path.read_text(encoding="utf-8")), self.harness_pid)
        except (OSError, ValueError) as error:
            record["detail"] = f"footprint JSON unreadable: {error}"
        if record["bytes"] is None and not record["detail"]:
            record["detail"] = "footprint JSON holds no recognisable footprint figure"
        return record

    def _deadline_kill(self) -> None:
        """End the run as a step deadline does: one group SIGKILL of the accepted harness leader, or on
        Windows one TerminateProcess of the accepted harness, after which the gate's job ends the rest.

        The pid and start token accepted from harness.pid are rechecked immediately before the
        signal, as each PTY member is, so a pid the launcher reaped and the kernel reused is never signalled.
        """
        pid = self.harness_pid
        windows = self.context.platform == "win32"
        if pid is None or self.harness_start is None:
            problem = "the harness identity was never accepted, so nothing was signalled"
        elif windows:
            problem = validate_windows_harness(self.context.table, pid, self.context.launch_unix_s,
                                               self.context.harness_command, self.harness_start)
        else:
            problem = validate_harness_leader(self.context.table, pid, self.context.launch_unix_s,
                                              self.context.harness_command, self.harness_start)
        self.deadline.update(pid=pid, problem=problem)
        if problem is None:
            if windows:
                # The table rechecks the creation time on the handle it terminates through.
                outcome = self.context.table.terminate(pid, self.harness_start)
                action = "TerminateProcess"
            else:
                outcome = self.context.table.kill_group(pid)
                action = "group SIGKILL"
            self.deadline["sent"] = outcome == "sent"
            if outcome != "sent":
                self.deadline["problem"] = f"{action} of {pid} was {outcome}"


class FrontSampler:
    """Take front-application samples, append each raw command to the evidence log and print each form once."""

    def __init__(self, log_path: Path, run: Callable[[Sequence[str], int], CommandRecord] | None,
                 printed_forms: set[str], sample: Callable[[], FrontReading] | None = None) -> None:
        self.log_path = log_path
        self.run = run
        self.printed_forms = printed_forms
        # Windows samples the foreground window instead; None samples lsappinfo through `run`.
        self.sample_function = sample
        self.readings: list[FrontReading] = []

    def sample(self) -> FrontReading:
        """Take one sample; the first of each form prints its raw text, so even a passing log shows it."""
        reading = self.sample_function() if self.sample_function is not None else sample_front(self.run)
        append_front_samples(self.log_path, reading.records)
        self.readings.append(reading)
        if reading.kind not in self.printed_forms:
            self.printed_forms.add(reading.kind)
            raw = " | ".join(json.dumps(record.as_json()) for record in reading.records)
            source = "lsappinfo" if self.sample_function is None else "foreground"
            print(f"[perf-compare] {source} sample form={FORM_LABELS[reading.kind]} raw={raw}", flush=True)
        return reading


def run_periodically(action: Callable[[], object], interval_s: float, stop: threading.Event,
                     problems: list[str], name: str) -> threading.Thread:
    """Repeat an action until stop is set; an exception is recorded as a problem, so it invalidates the run."""
    def loop() -> None:
        while not stop.is_set():
            try:
                action()
            except Exception as error:  # Any failure invalidates the run instead of ending the thread silently.
                problems.append(f"{name} failed: {type(error).__name__}: {error}")
                return
            stop.wait(interval_s)

    thread = threading.Thread(target=loop, name=name, daemon=True)
    thread.start()
    return thread



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
    return WINDOWS_KILL_EXIT_CODE if platform_name == "win32" else SIGKILL_EXIT_CODE


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
    source_root: Path = ROOT
    # The head harness's latency split schema; both sides run that harness, so both are held to it.
    latency_split_schema: int | None = None


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
        if code == HARNESS_VALID and outcome.status == "PASS":
            if result is None and not outcome.not_exercised:
                return "schema", ["exit 0 without result.json"]
            if result is not None and result.get("status") != "valid":
                return "schema", [f"exit 0 with status {result.get('status')!r}"]
        if _teardown_unsettled(result):
            return "cleanup", [UNSETTLED_TEARDOWN] + notes
    if code == HARNESS_REFUSED:
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
        if code == HARNESS_INVALID and result is not None and occlusion_invalidated(result):
            return "occluded", _harness_reasons(result)
        if code == HARNESS_BLOCKED or (code == HARNESS_VALID and result is None and outcome.not_exercised):
            return "blocked", [f"deadline case not exercised (exit {code})"]
        return "unexpected", [f"the harness ended (status {outcome.status}, exit {code}) before the deadline kill"]
    notes = [str(note) for note in (result or {}).get("notes") or []]
    if code == HARNESS_VALID:
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
    if code == HARNESS_INVALID:
        harness_reasons = _harness_reasons(result)
        if result is not None and occlusion_invalidated(result):
            return "occluded", harness_reasons
        return "invalid", harness_reasons or ["the harness invalidated the run (exit 3)"]
    if code == HARNESS_TIMEOUT:
        return "timeout", ["the harness timed out (exit 4)"] + notes
    if code == HARNESS_BLOCKED:
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


def custody_cleanup(custody: Mapping | None) -> CleanupResult:
    """A Windows run's cleanup: settled only when the gate's job custody is verified."""
    result = CleanupResult()
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
    return active if _is_int(active) else None


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


def _keep_scratch(scratch: Path, kept: Path) -> None:
    """Copy a run's result, logs and records into its evidence; the scratch's fixtures are left out.

    `progress.json`, which the harness rewrites after each completed phase, is kept so a killed run
    still shows its earlier phases; it is evidence only, never read as a result or for the table.
    """
    kept.mkdir()
    for name in ("result.json", "progress.json", "harness.pid", "logs", "sessions", "acks", "checkpoints", "go"):
        source = scratch / name
        if source.is_symlink():
            continue
        if source.is_dir():
            shutil.copytree(source, kept / name, symlinks=True)
        elif source.is_file():
            shutil.copy2(source, kept / name)


def execute_run(plan: RunPlan, host: Host, evidence: Path) -> RunOutcome:
    """Run the harness once through run_step, serve it while it runs, clean up and record the evidence."""
    evidence.mkdir(parents=True)
    scratch = new_scratch_path(host.temp_root, plan.scenario.id, plan.variant)
    # The snapshot comes before the sentinel, so a write between them reads as a change, never as an old file.
    home_unresolved: list[str] = []
    try:
        before = snapshot_home(host.home)
    except HomeSnapshotUnresolved as error:
        before = None
        home_unresolved.append(f"before the run: {error}")
    sentinel = evidence / "sentinel"
    sentinel.write_bytes(b"")
    sentinel_ns = sentinel.stat().st_mtime_ns
    sampler = FrontSampler(evidence / "front-samples.log", host.front_run, host.printed_forms,
                           sample=host.front_sample)
    sampler.sample()  # The baseline, so the first sample after launch has a predecessor.
    launch_unix_s = host.clock()
    watcher = RunWatcher(RunContext(scratch, evidence, launch_unix_s, host.table, host.gate,
                                    host.excluded_sids, plan.kill_at_go, plan.binary.name, platform=host.platform))
    thread_problems: list[str] = []
    stop = threading.Event()
    threads = [run_periodically(sampler.sample, FRONT_SAMPLE_INTERVAL_S, stop, thread_problems, "front sampler"),
               run_periodically(watcher.poll, WATCH_INTERVAL_S, stop, thread_problems, "run watcher")]
    argv = harness_argv(plan.binary, plan.scenario.id, plan.variant, plan.harness_hash, scratch,
                        short=plan.short, laps=plan.laps, counters=plan.counters)
    step = host.gate.Step("harness", argv, gate_hosts(host.platform),
                          run_timeout_s(plan.scenario, plan.smoke, plan.short), "local", (), ())
    try:
        # The cwd is the tree that built the binary, so the App loads that tree's fonts; logs stay in the evidence.
        step_result = host.gate.run_step(step, 1, plan.source_root, evidence, harness_environment(host.environ))
    finally:
        stop.set()
        for thread in threads:
            thread.join(FOOTPRINT_TIMEOUT_S + 30)
    if any(thread.is_alive() for thread in threads):
        thread_problems.append("a watcher thread did not stop, so the session records were not rescanned")
    else:
        watcher.final_scan()
    sessions = watcher.sessions_for_cleanup()
    unanchored = list(watcher.unanchored.values())
    custody = getattr(step_result, "custody", None)
    leftover = step_result.leftover_processes
    if host.platform == "win32":
        # The gate's job owns every process the harness started, so its custody, not anchors, proves teardown.
        cleanup = custody_cleanup(custody)
        leftover = windows_leftover_processes(custody, deadline_case=plan.kill_at_go and bool(watcher.deadline["sent"]))
    elif host.table is None:
        cleanup = CleanupResult()
        if sessions or unanchored:
            cleanup.unresolved("this host has no process table, so the sessions were not cleaned")
    else:
        cleanup = cleanup_sessions(host.table, sessions, unanchored=unanchored)
    try:
        after = snapshot_home(host.home)
    except HomeSnapshotUnresolved as error:
        after = None
        home_unresolved.append(f"after the run: {error}")
    if home_unresolved:
        # A check that could not read the home whole never clears the run, whatever the readable part shows.
        other = False
        home = [f"cannot be ruled out: the home check is unresolved {problem}" for problem in home_unresolved]
    else:
        other = after is not None and other_instance_alive(host.table, watcher.harness_pid)
        home = home_violations(host.home, before, after, sentinel_ns, watcher.harness_pid, other,
                               [str(scratch), os.path.realpath(scratch)])
    kept = evidence / "scratch"
    _keep_scratch(scratch, kept)
    if scratch.is_dir() and not scratch.is_symlink():
        shutil.rmtree(scratch, ignore_errors=True)
    log_text = read_log(step_result.log_path)
    # The font crate logs on its `config` target, which only the stderr layer keeps, so the run_step log
    # shows the failure; the run's own logs are read as well.
    font_failures = primary_font_errors(log_text.splitlines() + list(_log_lines(kept / "logs")))
    not_exercised = any(_NOT_EXERCISED.search(line) for line in log_text.splitlines()
                        if not line.startswith("[local-gate]"))
    data, schema = None, []
    if (kept / "result.json").is_file():
        try:
            parsed = json.loads((kept / "result.json").read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            schema = [f"result.json cannot be parsed: {error}"]
        else:
            # A run ended at GO is expected to leave no complete result, so its result is not judged.
            if not (plan.kill_at_go and watcher.deadline["sent"]):
                exit_code = step_result.exit_code if step_result.status != "TIMEOUT" else None
                # A base may predate a contract field; the head is the contract under test.
                schema = validate_result(parsed, plan.harness_hash, exit_code, counters=plan.counters,
                                         partial_counters=plan.side == "base", platform_name=host.platform,
                                         latency_split_schema=plan.latency_split_schema)
            data = parsed if isinstance(parsed, dict) else None
    if host.platform == "win32":
        focus = judge_foreground(sampler.readings, user_session=has_user_session(host.environ))
    else:
        focus = judge_focus(sampler.readings, watcher.harness_pid, user_session=has_user_session(host.environ))
    # Windows runs record the adapter they drew through, so a pair on two adapters is never compared.
    renderer = read_renderer(kept / "logs") if host.platform == "win32" else None
    for failed in focus.failed:
        print(describe_sample(failed), flush=True)
    for note in focus.notes:
        print(f"[perf-compare] focus: {note}", flush=True)
    outcome = RunOutcome(plan, evidence, step_result.status, step_result.exit_code, data, schema, not_exercised,
                         focus, thread_problems + watcher.problems, cleanup, home, dict(watcher.deadline),
                         read_memory_samples(kept / "logs"),
                         read_render_timing(kept / "logs") if plan.laps else [], watcher.footprints,
                         watcher.harness_pid, leftover, step_result.detail, font_failures,
                         custody=custody if host.platform == "win32" else None,
                         deadline_exit_code=deadline_exit_code(host.platform), platform=host.platform,
                         renderer=renderer,
                         fallback_log=read_fallback_log(kept / "logs") if plan.laps else None)
    kind, reasons = classify_outcome(outcome)
    cleanup_record = {
        "settled": cleanup.settled, "signalled": cleanup.signalled, "problems": cleanup.problems,
        "survivors": [asdict(member) for member in cleanup.survivors],
        "sessions": [asdict(session) for session in sessions],
        "unanchored": [asdict(record) for record in unanchored]}
    if host.platform == "win32":
        # The job's counts and members before the gate ended it, and whether it had to.
        cleanup_record.update(cleanup=(custody or {}).get("cleanup"), before_cleanup=(custody or {}).get("before_cleanup"),
                              programs=[asdict(program) for program in watcher.programs.values()])
    _write_json(evidence / "cleanup.json", cleanup_record)
    _write_json(evidence / "home-check.json", {
        "home": str(host.home), "unresolved": bool(home_unresolved),
        "absent_before": before is None, "absent_after": after is None,
        "sentinel_mtime_ns": sentinel_ns, "other_instance_alive": other, "violations": home})
    _write_json(evidence / "footprints.json", watcher.footprints)
    _write_json(evidence / "outcome.json", {
        "kind": kind, "reasons": reasons, "side": plan.side, "scenario": plan.scenario.id, "variant": plan.variant,
        "argv": list(argv), "status": step_result.status, "exit_code": step_result.exit_code,
        "launch_unix_s": launch_unix_s, "harness_pid": watcher.harness_pid, "deadline": watcher.deadline,
        "focus_problems": focus.problems, "focus_notes": focus.notes, "foreground_changes": focus.changes,
        "renderer": renderer, "watcher_problems": outcome.watcher_problems,
        "schema_problems": schema, "log": str(step_result.log_path)})
    return outcome


# --- Statistics per scenario and the tables -------------------------------------------------

MIB = 1024 * 1024
TABLE_HEADER = "| Scenario | Metric (unit) | Baseline | PR | Change |\n| --- | --- | --- | --- | --- |\n"
# Latency acceptance: each side attributes at least this share of its samples, and the sides differ by at most the gap.
LATENCY_MIN_PERCENT = 80
LATENCY_MAX_GAP_POINTS = 10


@dataclass
class SideRuns:
    """One side's valid runs of a scenario, or why it has none: `blocked` (cannot run) or `failed` (head only)."""

    outcomes: list = field(default_factory=list)
    blocked: str | None = None
    failed: str | None = None


@dataclass(frozen=True)
class NotAvailable:
    """A run metric the run cannot report, with the reason its cell names: `n/a: <reason>`."""

    reason: str


class PartialValue(float):
    """A checkpoint memory figure read from a partial sample: it counts, and its cell says how many runs."""


# A side whose harness has no checkpoint-memory hook: result.json says so, or an older harness leaves it out.
UNSUPPORTED_CHECKPOINT_MEMORY = "unsupported"
CHECKPOINT_MEMORY_STATES = ("supported", "unsupported")
# result.json's `hooks.trim`. `unsupported` is an untrimmed baseline whose memory readings stay figures; an
# older harness writes no `hooks` at all.
TRIM_HOOK_OUTCOMES = ("not-reached", "unsupported", "skipped", "trimmed")
# The short S12 plan's covered-window trim experiment, as result.json's `trim_experiment` names it, and the
# checkpoint its trim rules govern.
TRIM_EXPERIMENT = "s12-short-trim"
TRIM_CHECKPOINT = "covered"
# Why a trim experiment's covered reading is not compared: a hooked sample taken before the hook's trim, or
# trim tags on a side whose harness says it cannot trim.
TRIM_STALE = "stale"
TRIM_SCHEMA = "schema"
# A supported trim experiment whose hook did not trim: its covered reading is never compared as a trimmed one,
# and its raw figure is kept in a separate row.
TRIM_NOT_RUN = {"skipped": "trim skipped", "not-reached": "trim not reached"}
TRIM_NOT_RECORDED = "trim not recorded"
# The sources a successful trim names on a checkpoint line.
TRIM_SOURCES = ("hook", "scheduler")


def trim_experiment_run(result: Mapping) -> bool:
    """Whether a result is the short S12 trim experiment, the one run whose covered phase is held covered by
    the harness on every host and whose covered window may be trimmed on request."""
    return (result.get("scenario") == "S12" and result.get("short") is True
            and result.get("trim_experiment") == TRIM_EXPERIMENT)


def trim_reading_problem(result: Mapping, label: str, sample: MemorySample) -> str | None:
    """Why a trim experiment's covered sample cannot be compared, or None when it can.

    An unsupported hook is an untrimmed baseline and must carry no trim tags. A trimmed hook's sample counts
    only when it says the window is trimmed, names a `hook` or `scheduler` source, and carries a `trim_seq`
    at least the number the hook reported, so it was taken after that trim. A supported experiment that did
    not trim (skipped, not reached, or no recorded hook) is never compared as a trimmed reading. Every other
    checkpoint and run keeps the ordinary rules.
    """
    if label != TRIM_CHECKPOINT or not trim_experiment_run(result):
        return None
    hooks = result.get("hooks")
    trim = hooks.get("trim") if isinstance(hooks, dict) else None
    if sample.malformed_trim_tags:
        # When: a trim tag is present but unreadable, the sample cannot count as tagged or untagged.
        return TRIM_SCHEMA
    tagged = sample.trimmed is not None or sample.trim_source is not None or sample.trim_seq is not None
    if trim == "unsupported":
        return TRIM_SCHEMA if tagged else None
    if trim != "trimmed":
        return TRIM_NOT_RUN.get(trim, TRIM_NOT_RECORDED)
    after_hook = result.get("trim_seq_after_hook")
    if not _is_int(after_hook):
        # When: a trimmed hook without its number cannot be checked against any sample.
        return TRIM_SCHEMA
    if not sample.trim_seq or sample.trim_seq < after_hook:
        # When: trim_seq is missing, zero or older than the hook's, the sample predates that trim.
        return TRIM_STALE
    if sample.trimmed is None or sample.trim_source not in TRIM_SOURCES:
        # When: a current sample lacks its trim state or names no trimming source, its tags are malformed.
        return TRIM_SCHEMA
    if sample.trimmed is not True:
        # When: trimmed is false, the window was no longer trimmed when the sample was taken.
        return TRIM_STALE
    return None


CHECKPOINT_SAMPLING_STATES = ("complete", "exhausted", "active")


def run_metrics(outcome: RunOutcome) -> dict[tuple[str, str, str], object]:
    """Extract one valid run's metrics, keyed (name, unit, kind).

    `frame` metrics hold that run's samples, pooled across runs; `run` and `footprint` metrics hold
    one value per run. A field the result lacks yields no key, so it prints `n/a`, never zero.
    """
    result = outcome.result or {}
    metrics: dict[tuple[str, str, str], object] = {}
    trim_run = trim_experiment_run(result)
    for phase in result.get("phases") or []:
        name = phase.get("name")
        start, end = phase.get("start_unix_s"), phase.get("end_unix_s")
        wall_s = end - start if _is_number(start) and _is_number(end) else None
        if wall_s is not None:
            metrics[(f"{name} wall", "s", "run")] = wall_s
        if trim_run and name == TRIM_CHECKPOINT:
            # The trim experiment holds this phase covered on every host, so it draws almost nothing: a rate or
            # an interval would describe the harness, not the renderer. It reports activity counts instead.
            if _is_int(phase.get("presented_frames")):
                metrics[(f"{name} presented frames", "count", "run")] = phase["presented_frames"]
            if _is_int(phase.get("redraw_requested")):
                metrics[(f"{name} redraws requested", "count", "run")] = phase["redraw_requested"]
            if _is_number(phase.get("cpu_user_s")) and _is_number(phase.get("cpu_system_s")):
                metrics[(f"{name} CPU", "s", "run")] = phase["cpu_user_s"] + phase["cpu_system_s"]
            continue
        if wall_s and _is_int(phase.get("presented_frames")):
            metrics[(f"{name} presented frames", "fps", "run")] = phase["presented_frames"] / wall_s
        # Presented frames per logical update, divided by this run's own recorded count, never a constant;
        # a phase without `updates` (every phase but S10's stream, or an older harness) has no figure.
        if _is_int(phase.get("presented_frames")) and _is_int(phase.get("updates")) and phase["updates"] > 0:
            metrics[(f"{name} presented frames per update", "ratio", "run")] = \
                phase["presented_frames"] / phase["updates"]
        if wall_s and _is_int(phase.get("redraw_requested")):
            metrics[(f"{name} redraws requested", "per s", "run")] = phase["redraw_requested"] / wall_s
        if _is_number(phase.get("cpu_user_s")) and _is_number(phase.get("cpu_system_s")):
            metrics[(f"{name} CPU", "s", "run")] = phase["cpu_user_s"] + phase["cpu_system_s"]
        for key, label in (("dispatch_ms", "dispatch"), ("present_interval_ms", "present interval")):
            if _numbers(phase.get(key)):
                metrics[(f"{name} {label}", "ms", "frame")] = phase[key]
        if _numbers(phase.get("allocations_per_frame")):
            metrics[(f"{name} allocations per frame", "count", "frame")] = phase["allocations_per_frame"]
    attributed = latency_values(result.get("latency"))
    if attributed:
        metrics[("keypress-to-present latency", "ms", "frame")] = attributed
    throughput = result.get("throughput")
    if isinstance(throughput, dict) and _is_number(throughput.get("seconds")) and throughput["seconds"] > 0 \
            and _is_int(throughput.get("bytes")):
        metrics[("throughput", "MB/s", "run")] = throughput["bytes"] / throughput["seconds"] / 1_000_000
    if _is_number(result.get("uncover_ms")):
        metrics[("uncover", "ms", "run")] = result["uncover_ms"]
    if _is_int(result.get("scrollback_rows_retained")):
        metrics[("scrollback rows retained", "rows", "run")] = result["scrollback_rows_retained"]
    supported = result.get("checkpoint_memory") == "supported"
    for point in result.get("checkpoints") or []:
        if not _checkpoint_ok(point):
            continue
        if "frame_texture_bytes" in point:
            # The frame texture's own reading, not a memory sample; a base without the feature has none.
            metrics[(f"{point['label']} frame_texture_bytes", "B", "run")] = point["frame_texture_bytes"]
        keys = (f"{point['label']} renderer_total_bytes", f"{point['label']} process_resident_bytes",
                f"{point['label']} grid bytes per pane", f"{point['label']} renderer_row_glyph_cache_bytes")
        if not supported:
            # A harness without the hook takes no checkpoint sample; a periodic one is never substituted.
            for key in keys:
                metrics[(key, "MiB", "run")] = NotAvailable(UNSUPPORTED_CHECKPOINT_MEMORY)
            continue
        reading = checkpoint_memory(outcome.memory, point["index"])
        if reading is None:
            continue  # No tagged line reached the log: memory is unavailable, not a failure.
        if reading.problem is not None:
            for key in keys:
                metrics[(key, "MiB", "run")] = NotAvailable(reading.problem)
            continue
        sample = reading.sample
        fresh_after = point.get("fresh_after_unix_s")
        if fresh_after is not None and sample.unix_s < fresh_after:
            continue  # Taken before the checkpoint's reading is fresh: unavailable, never a stale figure.
        trim_problem = trim_reading_problem(result, point["label"], sample)
        if trim_problem is not None:
            for key in keys:
                metrics[(key, "MiB", "run")] = NotAvailable(trim_problem)
            if trim_problem != TRIM_SCHEMA:
                # When: the reading is real but not a credited trim, its figure stays visible on its own row.
                metrics[(f"{point['label']} renderer_total_bytes, uncredited trim", "MiB", "run")] = \
                    sample.renderer_total_bytes / MIB
            continue
        figure = PartialValue if reading.partial else float
        metrics[(keys[0], "MiB", "run")] = figure(sample.renderer_total_bytes / MIB)
        if sample.process_resident_bytes is not None:
            metrics[(keys[1], "MiB", "run")] = figure(sample.process_resident_bytes / MIB)
        grid_per_pane = sample.grid_bytes_per_pane()
        if grid_per_pane is not None:
            metrics[(keys[2], "MiB", "run")] = figure(grid_per_pane / MIB)
        if sample.renderer_row_glyph_cache_bytes is not None:
            # Summed over every renderer, so it is read against live renderers x 512 MiB; never gated.
            metrics[(keys[3], "MiB", "run")] = figure(sample.renderer_row_glyph_cache_bytes / MIB)
    for stem, record in outcome.footprints.items():
        if _is_int(record.get("bytes")):
            metrics[(f"{stem.partition('-')[2] or stem} footprint", "MiB", "footprint")] = record["bytes"] / MIB
    return metrics


def lap_metrics(outcome: RunOutcome) -> dict[tuple[str, str, str], object]:
    """Extract one `--laps` run's render_timing laps, one frame metric per window and lap."""
    metrics: dict[tuple[str, str, str], list[float]] = {}
    for sample in outcome.laps:
        for lap, value in sample.laps.items():
            metrics.setdefault((f"{sample.window} {lap}", "ms", "frame"), []).append(value)
    return metrics


def percent_change(base_value: float | None, head_value: float | None) -> str:
    """Return the head's change from the base as a signed percentage, or `n/a`."""
    if base_value is None or head_value is None or base_value == 0:
        return "n/a"
    return f"{(head_value - base_value) / base_value * 100:+.1f}%"


def _missing_cell(side: SideRuns) -> str:
    """The cell of a metric a side cannot report."""
    return "blocked" if side.blocked else "failed" if side.failed else "n/a"


def _status_cell(side: SideRuns) -> str:
    if side.blocked:
        return f"blocked: {side.blocked}"
    if side.failed:
        return f"failed: {side.failed}"
    return f"{len(side.outcomes)} valid run" + ("" if len(side.outcomes) == 1 else "s")


def _rows(label: str, base: SideRuns, head: SideRuns, extract: Callable[[RunOutcome], dict],
          include: Callable[[tuple[str, str, str]], bool] | None) -> list[list[str]]:
    """Build a scenario's rows: a status row, then two rows per frame metric and one per run metric."""
    sides = (base, head)
    rows = [[label, "status", _status_cell(base), _status_cell(head), ""]]
    per_side = [[extract(outcome) for outcome in side.outcomes] for side in sides]
    keys: list[tuple[str, str, str]] = []
    for metrics in per_side[0] + per_side[1]:
        keys.extend(key for key in metrics if key not in keys and (include is None or include(key)))
    for key in keys:
        name, unit, kind = key
        values = [[metrics[key] for metrics in side_metrics if key in metrics] for side_metrics in per_side]
        if kind == "frame":
            summaries = [frame_summary(side_values) for side_values in values]
            for statistic, attribute in (("median", "median"), ("p95", "percentile_95")):
                cells, figures = [], []
                for side, summary in zip(sides, summaries):
                    figure = None if side.blocked or summary is None else getattr(summary, attribute)
                    if figure is None:
                        cells.append(_missing_cell(side))
                    else:
                        spread = summary.run_median_range if statistic == "median" else summary.run_p95_range
                        cells.append(f"{figure:.2f} ({spread[0]:.2f}–{spread[1]:.2f})")
                    figures.append(figure)
                rows.append([label, f"{name} {statistic} ({unit})", cells[0], cells[1], percent_change(*figures)])
            continue
        cells, figures = [], []
        for side, side_values in zip(sides, values):
            reasons = [value.reason for value in side_values if isinstance(value, NotAvailable)]
            numbers = [value for value in side_values if not isinstance(value, NotAvailable)]
            summary = None if side.blocked else run_summary(numbers)
            if summary is None:
                # A side that ran but cannot report the metric says why, as `n/a: <reason>`.
                usable = reasons and not side.blocked and not side.failed
                cells.append(f"n/a: {reasons[0]}" if usable else _missing_cell(side))
                figures.append(None)
                continue
            cell = f"{summary.median:.2f} ({summary.minimum:.2f}–{summary.maximum:.2f})"
            # A footprint can fail without failing its run, so its cell says how many runs have it.
            if kind == "footprint" or summary.runs < len(side.outcomes):
                cell += f", {summary.runs}/{len(side.outcomes)} runs"
            partial = sum(isinstance(value, PartialValue) for value in numbers)
            if partial:
                cell += f", {partial} partial"
            cells.append(cell)
            figures.append(summary.median)
        rows.append([label, f"{name} ({unit})", cells[0], cells[1], percent_change(*figures)])
    return rows


def latency_coverage(side: SideRuns) -> tuple[int, int] | None:
    """Pool a side's attributed and total latency samples, or None when no run measured latency."""
    attributed = total = 0
    for outcome in side.outcomes:
        latency = (outcome.result or {}).get("latency")
        if isinstance(latency, dict) and _is_int(latency.get("attributed")) and _is_int(latency.get("total")):
            attributed += latency["attributed"]
            total += latency["total"]
    return (attributed, total) if total > 0 else None


def latency_acceptance(base: tuple[int, int] | None, head: tuple[int, int] | None) -> bool:
    """Whether latency can be judged: each side attributes >= 80% of samples, within 10 points of the other."""
    if base is None or head is None:
        return False
    (base_attributed, base_total), (head_attributed, head_total) = base, head
    # Integer cross-multiplication keeps the 80% and 10-point bounds exact.
    if base_attributed * 100 < LATENCY_MIN_PERCENT * base_total or head_attributed * 100 < LATENCY_MIN_PERCENT * head_total:
        return False
    gap = abs(base_attributed * head_total - head_attributed * base_total) * 100
    return gap <= LATENCY_MAX_GAP_POINTS * base_total * head_total


def presenter_text(outcome: RunOutcome) -> str | None:
    """Name a run's presenter and adapter, or None when it reported neither (a base older than the macOS record)."""
    presenter = (outcome.result or {}).get("presenter")
    if outcome.renderer is None and not isinstance(presenter, Mapping):
        return None
    if isinstance(presenter, Mapping) and presenter.get("windows_gdi"):
        text = "GDI"
    else:
        text = "wgpu"
        if isinstance(presenter, Mapping) and presenter.get("software_render_degraded"):
            text += ", degraded"
    if outcome.renderer is not None:
        text += f" on {describe_renderer(outcome.renderer)}"
    return text


def presenter_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """One row naming each side's presenter, when any run reported one."""
    cells = []
    for side in (base, head):
        texts: list[str] = []
        for outcome in side.outcomes:
            text = presenter_text(outcome)
            if text is not None and text not in texts:
                texts.append(text)
        cells.append("; ".join(texts) or None)
    if not any(cells):
        return []
    return [[label, "presenter", cells[0] or _missing_cell(base), cells[1] or _missing_cell(head), ""]]


def grid_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """One row naming each side's grids as `<cols>x<rows>`, when any run reported one; a pair shares one grid."""
    cells = []
    for side in (base, head):
        texts: list[str] = []
        for outcome in side.outcomes:
            size = grid_size((outcome.result or {}).get("grid"))
            if size is not None and grid_text(size) not in texts:
                texts.append(grid_text(size))
        cells.append("; ".join(texts) or None)
    if not any(cells):
        return []
    return [[label, "grid", cells[0] or _missing_cell(base), cells[1] or _missing_cell(head), ""]]


NO_OCCLUSION_NOTE = "Windows reports no occlusion"
NO_FOOTPRINT_NOTE = "Windows has no `footprint`"


def windows_na_rows(label: str, base: SideRuns, head: SideRuns, existing: Sequence[Sequence[str]]) -> list[list[str]]:
    """Rows a Windows comparison states as `n/a`: S12's occlusion figures and every checkpoint's footprint."""
    outcomes = base.outcomes + head.outcomes
    if not any(outcome.platform == "win32" for outcome in outcomes):
        return []
    present = {row[1] for row in existing}
    rows = []
    if label.split("/")[0] == "S12":
        for metric in ("uncover (ms)", "memory released while covered (MiB)"):
            if metric not in present:
                rows.append([label, metric, "n/a", "n/a", NO_OCCLUSION_NOTE])
    labels: list[str] = []
    for outcome in outcomes:
        for point in (outcome.result or {}).get("checkpoints") or []:
            if _checkpoint_ok(point) and point["label"] not in labels:
                labels.append(point["label"])
    for checkpoint in labels:
        metric = f"{checkpoint} footprint (MiB)"
        if metric not in present:
            rows.append([label, metric, "n/a", "n/a", NO_FOOTPRINT_NOTE])
    return rows


DELIVERY_FILE = "delivery.json"
# The delivered text the harness classified, which it keeps beside its record.
DELIVERY_TEXT_FILE = "delivery.txt"
# The record schema the harness writes: 2 adds a frame check's `unseen`, `brackets` and `unseen_markers`.
DELIVERY_SCHEMA_VERSION = 2
# Schemas read_delivery accepts; only DELIVERY_SCHEMA_VERSION carries the fields a retry is decided on.
DELIVERY_SCHEMA_VERSIONS = (1, DELIVERY_SCHEMA_VERSION)
# The scenarios whose bytes a Windows comparison replays through ConPTY before the measured runs.
DELIVERY_SCENARIOS = frozenset(("S3", "S9", "S10", "S11"))
DELIVERY_NOTE = "untimed ConPTY replay by the head build, shared by both sides"
# Attempts a replay gets when every failure before the last is one retryable missing frame.
DELIVERY_ATTEMPT_LIMIT = 3
# The one check a retry may follow, and the most missing markers the harness names in it.
RETRYABLE_DELIVERY_CHECK = "sync brackets"
UNSEEN_MARKER_LIMIT = 8
# The exact detail the harness's sync_check writes: three placement counts, then any unpainted count.
FRAME_DETAIL = re.compile(r"enclosed (\d+), empty pair ahead (\d+), absent (\d+)(?:, never painted (\d+))?")
# The most delivered text a replay keeps: the harness's DELIVERY_LIMIT_BYTES.
DELIVERY_TEXT_LIMIT_BYTES = 64 << 20
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
    return platform_name == "win32" and scenario_id in DELIVERY_SCENARIOS


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
    if not (_is_int(unseen) and unseen >= 0 and _is_int(brackets) and brackets >= 0):
        return "unseen and brackets are not non-negative integers"
    if not isinstance(markers, list) or len(markers) != min(unseen, UNSEEN_MARKER_LIMIT):
        return f"unseen_markers does not list {min(unseen, UNSEEN_MARKER_LIMIT)} markers"
    if not all(isinstance(marker, str) and marker and "`" not in marker and "|" not in marker for marker in markers):
        # When: a marker is empty or would break the table's code span or cell, the record is not the harness's.
        return "unseen_markers holds a marker that is not plain text"
    parsed = FRAME_DETAIL.fullmatch(check["detail"])
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
        record = json.loads((scratch / DELIVERY_FILE).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None, f"{DELIVERY_FILE} is missing from {scratch}"
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        return None, f"{DELIVERY_FILE} is unreadable: {error}"
    version = record.get("schema_version") if isinstance(record, dict) else None
    if not (_is_int(version) and version in DELIVERY_SCHEMA_VERSIONS):
        # When: the version is a float, a bool or another number, the record is not one this script reads.
        versions = " or ".join(str(version) for version in DELIVERY_SCHEMA_VERSIONS)
        return None, f"{DELIVERY_FILE} has no schema_version {versions}"
    if record.get("scenario") != scenario_id or record.get("variant") != variant:
        return None, (f"{DELIVERY_FILE} names {record.get('scenario')}/{record.get('variant')}, "
                      f"not {scenario_id}/{variant}")
    checks = record.get("checks")
    if not isinstance(checks, list) or not checks or not all(_delivery_check_ok(check) for check in checks):
        return None, f"{DELIVERY_FILE} has no well-formed checks"
    bytes_kept = record.get("bytes_kept")
    if not (_is_int(bytes_kept) and bytes_kept >= 0):
        return None, f"{DELIVERY_FILE} is malformed: bytes_kept is not a non-negative integer"
    if version == DELIVERY_SCHEMA_VERSION:
        frame_checks = [check for check in checks if check["name"] == RETRYABLE_DELIVERY_CHECK]
        if scenario_id == "S10" and len(frame_checks) != 1:
            return None, f"{DELIVERY_FILE} is malformed: S10 has {len(frame_checks)} {RETRYABLE_DELIVERY_CHECK} checks"
        for check in frame_checks:
            malformed = frame_check_problem(check, variant)
            if malformed is not None:
                return None, f"{DELIVERY_FILE} is malformed: {RETRYABLE_DELIVERY_CHECK}: {malformed}"
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
    rows = [[label, f"delivery: {check['name']}", check["detail"], check["detail"], note or DELIVERY_NOTE]
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
    if result.status != "FAIL" or result.exit_code != HARNESS_BLOCKED:
        return None
    if not isinstance(record, Mapping) or not _is_int(record.get("schema_version")) \
            or record["schema_version"] != DELIVERY_SCHEMA_VERSION:
        return None
    failed = [check for check in record["checks"] if not check["ok"]]
    if len(failed) != 1 or failed[0]["name"] != RETRYABLE_DELIVERY_CHECK:
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
            while size <= DELIVERY_TEXT_LIMIT_BYTES:
                block = stream.read(1 << 20)
                if not block:
                    break
                size += len(block)
    except FileNotFoundError:
        return f"{path.name} is missing"
    except OSError as error:
        return f"{path.name} is unreadable: {error}"
    if size > DELIVERY_TEXT_LIMIT_BYTES:
        return f"{path.name} passes the {DELIVERY_TEXT_LIMIT_BYTES}-byte cap"
    if size != record["bytes_kept"]:
        return f"{path.name} holds {size} bytes, not the record's bytes_kept {record['bytes_kept']}"
    return None


def delivery_evidence_name(scenario_id: str, variant: str, attempt: int) -> str:
    """The stem every file of one replay attempt shares: its step id, record, text and log."""
    return f"delivery-{scenario_id}-{variant}-attempt{attempt}"


def delivery_note(verdict: str, summaries: Sequence[str]) -> str:
    """The delivery row's disclosure: the shared replay, its verdict on attempt N, then each listed attempt."""
    parts = [DELIVERY_NOTE, verdict]
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
    scratch = new_scratch_path(temp_root, scenario_id, variant)
    argv = capture_delivery_argv(binary, scenario_id, variant, scratch, short=short)
    step = gate.Step(name, argv, gate_hosts(sys.platform), timeout_s, "local", (), ())
    try:
        result = gate.run_step(step, index, ROOT, evidence, harness_environment(environ))
        record, problem = read_delivery(scratch, scenario_id, variant)
        for source, suffix in ((DELIVERY_FILE, "json"), (DELIVERY_TEXT_FILE, "txt")):
            if (scratch / source).is_file():
                shutil.copyfile(scratch / source, evidence / f"{name}.{suffix}")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    if platform_name == "win32":
        # When: the gate's job owns every process the replay started, so only its custody proves teardown.
        cleanup = custody_cleanup(getattr(result, "custody", None))
        if not cleanup.settled:
            raise StopComparison(f"delivery replay {scenario_id}/{variant}: unresolved cleanup: "
                                 f"{'; '.join(cleanup.problems)}")
    elif result.leftover_processes != 0:
        # When: without a job, the gate's leftover count proves teardown, and an unknown count proves nothing.
        counted = "an unknown number of" if result.leftover_processes is None else str(result.leftover_processes)
        raise StopComparison(f"delivery replay {scenario_id}/{variant}: {counted} process(es) outlived the harness")
    ended = f"the replay ended {result.status}, exit {result.exit_code}"
    if record is None:
        return None, f"{problem}; {ended}", result
    if problem is None and result.status != "PASS":
        # When: the record says every check passed, yet the harness did not, the record is not trusted.
        return None, f"{DELIVERY_FILE} passed every check, but {ended}", result
    if problem is not None and result.status == "PASS":
        return None, f"{problem}, but {ended}", result
    if problem is not None and (result.status != "FAIL" or result.exit_code != HARNESS_BLOCKED):
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
    for attempt in range(1, DELIVERY_ATTEMPT_LIMIT + 1):
        record, problem, result = replay_delivery_attempt(
            gate, binary, scenario_id, variant, evidence, index, attempt, short=short, timeout_s=timeout_s,
            temp_root=temp_root, environ=environ, platform_name=platform_name)
        of_limit = f"attempt {attempt} of {DELIVERY_ATTEMPT_LIMIT}"
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


def blocked_set_results(label: str, reason: str, set_names: Sequence[str]) -> list:
    """Results for a scenario that runs no set: each set's two sides are blocked for `reason`."""
    return [SetResult(label, set_name, SideRuns(blocked=reason), SideRuns(blocked=reason)) for set_name in set_names]


def comparison_rows(label: str, base: SideRuns, head: SideRuns,
                    include: Callable[[tuple[str, str, str]], bool] | None = None) -> list[list[str]]:
    """Rows of the PR table for one scenario, with the latency attribution row where latency was measured."""
    rows = _rows(label, base, head, run_metrics, include)
    coverage = [latency_coverage(side) for side in (base, head)]
    if include is None and any(coverage):
        cells = [_missing_cell(side) if side.blocked or pair is None else
                 f"{pair[0] / pair[1] * 100:.1f}% ({pair[0]}/{pair[1]})" for side, pair in zip((base, head), coverage)]
        verdict = ("accepted" if latency_acceptance(*coverage) else
                   f"open: needs ≥{LATENCY_MIN_PERCENT}% attributed on each side, within {LATENCY_MAX_GAP_POINTS} points")
        rows.append([label, "latency attribution coverage (%)", cells[0], cells[1], verdict])
    if include is None:
        # The presenter and grid rows follow the status row; Windows n/a rows close the scenario.
        rows[1:1] = presenter_rows(label, base, head) + grid_rows(label, base, head)
        rows.extend(glyph_atlas_rows(label, base, head))
        rows.extend(windows_na_rows(label, base, head, rows))
    return rows


# Each glyph atlas fact a checkpoint row reports: its field, unit, the value it reads from the facts, and
# whether it is numeric (a change compares the sides' medians) or categorical (no change).
GLYPH_ATLAS_ROW_FACTS = (
    ("glyph_atlas_dim", "px", lambda facts: facts.dim, True),
    ("glyph_atlas_packed_pixels", "px", lambda facts: facts.packed_pixels, True),
    ("glyph_atlas_fit", "outcome", lambda facts: facts.fit, False),
    ("glyph_atlas_growths", "count", lambda facts: facts.growths, True),
    ("glyph_atlas_evictions", "count", lambda facts: facts.evictions, True),
    ("glyph_atlas_max_tile", "px", lambda facts: f"{facts.max_tile[0]}x{facts.max_tile[1]}", False),
)


# The per-run native renderer id row each logical renderer carries after its facts: categorical evidence,
# read by `glyph_atlas_rows` itself rather than through a facts reader.
NATIVE_ID_FIELD = "renderer_native_id"
NATIVE_ID_ROW = (NATIVE_ID_FIELD, "id", None, False)


def _checkpoint_sample(outcome: RunOutcome, point: Mapping) -> MemorySample | None:
    """A checkpoint's own memory sample by the rules `run_metrics` reads it with, or None when the run has no
    usable one: no hook, no tagged line, a conflicting reading, or a sample older than its freshness time."""
    if (outcome.result or {}).get("checkpoint_memory") != "supported":
        return None
    reading = checkpoint_memory(outcome.memory, point["index"])
    if reading is None or reading.problem is not None:
        return None
    fresh_after = point.get("fresh_after_unix_s")
    if fresh_after is not None and reading.sample.unix_s < fresh_after:
        return None
    return reading.sample


def _checkpoint_atlases(outcome: RunOutcome) -> list[tuple[Mapping, tuple, Mapping | None]]:
    """Each well-formed checkpoint of a run with its sample's glyph atlas facts (empty when it has none) and the
    harness's atlas reading taken at the same sampling attempt, or None when the harness recorded none."""
    found = []
    for point in (outcome.result or {}).get("checkpoints") or []:
        if not _checkpoint_ok(point):
            continue
        sample = _checkpoint_sample(outcome, point)
        if sample is None:
            found.append((point, (), None))
            continue
        # The reading belongs to the sample only when both come from the same attempt.
        reading = next((reading for reading in point.get("atlas_readings") or []
                        if reading["attempt"] == sample.checkpoint_attempt), None)
        found.append((point, sample.glyph_atlases, reading))
    return found


# A renderer breakdown identity: `role[native label]`.
_RENDERER_IDENTITY = re.compile(r"([A-Za-z_][\w-]*)\[(.*)\]")


def logical_renderers(atlases: Sequence[GlyphAtlasFacts], reading: Mapping | None) -> list[tuple[str, GlyphAtlasFacts]]:
    """Each renderer's facts under a logical identity that is the same in every run and on both sides.

    A visible renderer's label is its native window id (an object address on macOS, an HWND on Windows), new
    in every run, so the visible renderer the harness's reading names as its main window is `main`. A warm
    renderer keeps its pool slot, `warm[slot]`, which is already stable. Any other visible renderer is
    `visible#k`, numbered in label order, which is stable only while a run has one of them; a run without a
    reading has no `main`, so its visible renderers are all numbered rather than guessed. Any other role keeps
    its identity as reported.
    """
    main_window = (reading or {}).get("main_window")
    named, others = [], []
    for facts in atlases:
        parsed = _RENDERER_IDENTITY.fullmatch(facts.renderer)
        if parsed is None or parsed.group(1) != "visible":
            named.append((facts.renderer, facts))
        elif main_window is not None and parsed.group(2) == main_window:
            named.append(("main", facts))
        else:
            others.append(facts)
    others.sort(key=lambda facts: facts.renderer)
    named.extend((f"visible#{number}", facts) for number, facts in enumerate(others, start=1))
    return named


def _native_label(facts: GlyphAtlasFacts) -> str:
    """The native label inside a renderer's `role[label]` identity, or the whole identity when it has none."""
    parsed = _RENDERER_IDENTITY.fullmatch(facts.renderer)
    return parsed.group(2) if parsed else facts.renderer


def _distinct_cell(values: Sequence[object]) -> str:
    """One value when every run agrees, otherwise each distinct value with how many runs reported it."""
    counts: dict[str, int] = {}
    for value in values:
        counts[str(value)] = counts.get(str(value), 0) + 1
    if len(counts) == 1:
        return next(iter(counts))
    return "; ".join(f"{text} ×{count}" for text, count in sorted(counts.items()))


def glyph_atlas_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """Per checkpoint and logical renderer, each glyph atlas fact on each side, from the checkpoint's memory sample.

    Rows are keyed by `logical_renderers`, so the same window lines up across runs and sides although its
    native id differs in each; a `renderer_native_id` row lists those ids per run as evidence. A cell shows the value every run reported, or each distinct value with its run count (a categorical fit
    such as `no_headroom`, `does_not_fit` or `evicted` included), and how many runs had it when not all did. A
    side without the facts, such as a base built before them, reads `n/a`. A numeric fact's change compares the
    sides' medians; a categorical one has none. No row is added when neither side reports facts.
    """
    sides = (base, head)
    # values[side][(checkpoint label, logical renderer, field)] = one value per run that reported it
    values: list[dict[tuple[str, str, str], list[object]]] = [{}, {}]
    order: list[tuple[str, str]] = []
    for side_values, side in zip(values, sides):
        for outcome in side.outcomes:
            for point, atlases, reading in _checkpoint_atlases(outcome):
                for renderer, facts in logical_renderers(atlases, reading):
                    if (point["label"], renderer) not in order:
                        order.append((point["label"], renderer))
                    for field_name, _unit, read, _numeric in GLYPH_ATLAS_ROW_FACTS:
                        side_values.setdefault((point["label"], renderer, field_name), []).append(read(facts))
                    # The native id stays as per-run evidence; it never keys a row.
                    side_values.setdefault((point["label"], renderer, NATIVE_ID_FIELD), []).append(
                        _native_label(facts))
    rows = []
    for checkpoint_label, renderer in order:
        for field_name, unit, _read, numeric in (*GLYPH_ATLAS_ROW_FACTS, NATIVE_ID_ROW):
            key = (checkpoint_label, renderer, field_name)
            cells, figures = [], []
            for side, side_values in zip(sides, values):
                reported = side_values.get(key, [])
                if not reported or side.blocked or side.failed:
                    cells.append(_missing_cell(side))
                    figures.append(None)
                    continue
                cell = _distinct_cell(reported)
                if len(reported) < len(side.outcomes):
                    cell += f", {len(reported)}/{len(side.outcomes)} runs"
                cells.append(cell)
                figures.append(median(reported) if numeric else None)
            change = percent_change(*figures) if numeric else ""
            rows.append([label, f"{checkpoint_label} {renderer} {field_name} ({unit})", cells[0], cells[1], change])
    return rows


def _counted_growths_by(outcome: RunOutcome, unix_s: float) -> int | None:
    """The `renderer.glyph_atlas_growths` deltas of every phase that ended by `unix_s`, summed; None when no
    such phase counted, or one that ended by then has no count."""
    counted = None
    for phase in (outcome.result or {}).get("phases") or []:
        if not isinstance(phase, dict) or not _is_number(phase.get("end_unix_s")) or phase["end_unix_s"] > unix_s:
            continue
        renderer = (phase.get("frame_counters") or {}).get("renderer")
        if not isinstance(renderer, Mapping) or not _count_ok(renderer.get("glyph_atlas_growths")):
            return None
        counted = (counted or 0) + renderer["glyph_atlas_growths"]
    return counted


# A run's reconciliation verdicts, from the one that decides a cell to the one that decides it last.
RECONCILE_MISMATCH, RECONCILE_INCONCLUSIVE, RECONCILE_CONSISTENT = "mismatch", "inconclusive", "consistent"


def _reconcile_run(atlases: Sequence[GlyphAtlasFacts], reading: Mapping | None,
                   phase_counted: int | None) -> tuple[str, str] | None:
    """One run's verdict at one checkpoint and the figures behind it; None when it has nothing to compare.

    The comparison is an equality at one boundary: a renderer's snapshot growths count since it was built, and
    the reading's per-window counted growths, taken in the same sampling attempt, count since its window was
    created, so each live window's two figures must be equal. Its figures read `logical snapshot/counted`. A
    warm renderer has no window and draws nothing, so it is not compared. A visible renderer the reading did
    not count, a counted window the snapshot does not hold, or a run without a per-window reading is
    inconclusive: the summed figures alone differ by startup growth and closed windows, so neither their
    equality nor their inequality proves anything, and both are shown. Closed windows' counted growths are
    listed apart; their renderers are no longer in the snapshot.
    """
    if not atlases:
        return None
    counted = (reading or {}).get("counted_glyph_atlas_growths")
    if counted is None:
        if phase_counted is None:
            return None
        snapshot = sum(facts.growths for facts in atlases)
        return RECONCILE_INCONCLUSIVE, f"snapshot {snapshot}, counted {phase_counted}"
    verdict = RECONCILE_CONSISTENT
    figures = []
    held = set()
    for renderer, facts in logical_renderers(atlases, reading):
        if renderer.startswith("warm["):
            continue
        native = _native_label(facts)
        held.add(native)
        window_counted = counted.get(native)
        if window_counted is None:
            # When: the window has no counted figure, nothing the snapshot holds can be checked against it.
            figures.append(f"{renderer} {facts.growths}/none")
            verdict = RECONCILE_MISMATCH if verdict == RECONCILE_MISMATCH else RECONCILE_INCONCLUSIVE
            continue
        figures.append(f"{renderer} {facts.growths}/{window_counted}")
        if window_counted != facts.growths:
            verdict = RECONCILE_MISMATCH
    for native in sorted(set(counted) - held):
        figures.append(f"unheld {native} none/{counted[native]}")
        verdict = RECONCILE_MISMATCH if verdict == RECONCILE_MISMATCH else RECONCILE_INCONCLUSIVE
    closed = reading.get("closed_glyph_atlas_growths")
    if closed:
        figures.append(f"closed {closed}")
    return verdict, ", ".join(figures)


def glyph_atlas_reconciliation_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """Counters-table rows reconciling each checkpoint's snapshot growths with the frame counters' growths.

    Each run's verdict comes from `_reconcile_run`; a cell names the worst verdict across its runs (a mismatch,
    then an inconclusive run, else consistent), how many runs had it when it is not consistent, and every run's
    figures separated by `; `. "Consistent" is printed only when every live window's figures are equal. A side
    with nothing to compare reads `n/a`, and no row is added when neither side has anything.
    """
    sides = (base, head)
    verdicts: list[dict[str, list[tuple[str, str]]]] = [{}, {}]
    order: list[str] = []
    for side_verdicts, side in zip(verdicts, sides):
        if side.blocked or side.failed:
            continue
        for outcome in side.outcomes:
            for point, atlases, reading in _checkpoint_atlases(outcome):
                found = _reconcile_run(atlases, reading, _counted_growths_by(outcome, point["unix_s"]))
                if found is None:
                    continue
                if point["label"] not in order:
                    order.append(point["label"])
                side_verdicts.setdefault(point["label"], []).append(found)
    rows = []
    for checkpoint_label in order:
        cells = []
        for side, side_verdicts in zip(sides, verdicts):
            found = side_verdicts.get(checkpoint_label)
            if not found:
                cells.append("n/a" if side.blocked == COUNTERS_HEAD_ONLY else _missing_cell(side))
                continue
            figures = "; ".join(figure for _verdict, figure in found)
            for verdict in (RECONCILE_MISMATCH, RECONCILE_INCONCLUSIVE):
                count = sum(run_verdict == verdict for run_verdict, _figure in found)
                if count:
                    cells.append(f"{verdict} in {count} of {len(found)} runs: {figures}")
                    break
            else:
                cells.append(f"{RECONCILE_CONSISTENT}: {figures}")
        rows.append([label, checkpoint_label, "glyph_atlas_growths, snapshot/counted", cells[0], cells[1], ""])
    return rows


def laps_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """Rows of the separate laps table: each render_timing lap pooled, with its per-run spread."""
    return _rows(label, base, head, lap_metrics, None)


# The only verdicts on whether fallback_receive waits account for the slowest dispatches.
FALLBACK_VERDICTS = ("supported", "inconclusive")
# One millisecond, added to a stamp's resolution as the matching tolerance.
MATCH_SLACK_S = 0.001


@dataclass(frozen=True)
class FallbackRunVerdict:
    """One laps run's verdict, its coverage, why it is inconclusive, and its waits inside and outside the
    examined slow dispatches."""

    verdict: str
    coverage: str
    reasons: tuple[str, ...]
    inside_ms: list[float]
    outside_ms: list[float]
    unparsed: int
    unmatched_enter: int
    unmatched_return: int


def fallback_run_verdict(result: dict | None, log: FallbackLog | None) -> FallbackRunVerdict:
    """Whether one laps run's fallback_receive waits account for its slowest dispatches.

    Per phase, the examined dispatches are the recorded slow dispatches at or above the phase's p95 of
    dispatch_ms. A wait `[t - elapsed, t]` matches one when it lies inside `[start - ε, end + ε]` and inside
    the same phase, ε being its stamp's resolution plus 1 ms. The run is supported when one examined dispatch's
    matched waits sum to at least half its duration, and inconclusive otherwise. Coverage is complete when no
    phase has more dispatches at or above p95 than it recorded.
    """
    phases = [phase for phase in (result or {}).get("phases") or [] if isinstance(phase, dict)]
    if log is None:
        return FallbackRunVerdict("inconclusive", "unavailable", ("no log",), [], [], 0, 0, 0)
    inside = [False] * len(log.waits)
    coverage = []
    supported = False
    for phase in phases:
        durations = phase.get("dispatch_ms") or []
        slow = phase.get("slow_dispatches")
        if not durations:
            continue
        if not isinstance(slow, list):
            # When: the harness recorded no slow dispatches, nothing in this phase can be examined.
            coverage.append("unavailable")
            continue
        threshold = percentile_95(durations)
        coverage.append("complete" if sum(duration_ms >= threshold for duration_ms in durations) <= len(slow) else "incomplete")
        span = (phase.get("start_unix_s", -math.inf), phase.get("end_unix_s", math.inf))
        for dispatch in (item for item in slow if item["ms"] >= threshold):
            matched = 0.0
            for index, wait in enumerate(log.waits):
                slack = wait.resolution_s + MATCH_SLACK_S
                begin, end = wait.end_unix_s - wait.elapsed_ms / 1000, wait.end_unix_s
                in_phase = span[0] - slack <= begin and end <= span[1] + slack
                if in_phase and dispatch["start_unix_s"] - slack <= begin and end <= dispatch["end_unix_s"] + slack:
                    inside[index] = True
                    matched += wait.elapsed_ms
            # A dispatch counts only when real waiting was matched: a zero-length dispatch with no
            # wait would otherwise satisfy 0 >= 0 and claim support from an empty log.
            supported = supported or (matched > 0 and matched >= dispatch["ms"] / 2)
    overall = ("unavailable" if "unavailable" in coverage or not coverage else
               "incomplete" if "incomplete" in coverage else "complete")
    reasons = []
    if not supported:
        reasons += ["no waits"] if not log.waits else ["no examined slow dispatch is half waiting"]
        if overall != "complete":
            reasons.append(f"coverage {overall}")
        if log.unparsed:
            reasons.append(f"{log.unparsed} unparsed")
    return FallbackRunVerdict(
        "supported" if supported else "inconclusive", overall, tuple(reasons),
        [wait.elapsed_ms for wait, hit in zip(log.waits, inside) if hit],
        [wait.elapsed_ms for wait, hit in zip(log.waits, inside) if not hit],
        log.unparsed, log.unmatched_enter, log.unmatched_return)


def _side_run_verdicts(side: SideRuns) -> list[FallbackRunVerdict]:
    return [fallback_run_verdict(outcome.result, outcome.fallback_log) for outcome in side.outcomes]


def fallback_side_verdict(side: SideRuns) -> str:
    """A side is supported when one of its laps runs is; a side with no valid run is inconclusive."""
    verdicts = [] if side.blocked or side.failed else _side_run_verdicts(side)
    return "supported" if any(verdict.verdict == "supported" for verdict in verdicts) else "inconclusive"


def _waits_cell(values: Sequence[float]) -> str:
    peak = max(values, default=0.0)
    return f"{len(values)} waits, {sum(values):.1f} ms, max {peak:.1f} ms"


def fallback_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """The laps table's fallback_receive rows: waits inside and outside the examined slow dispatches, pooled
    over a side's runs, then each side's verdict with its coverage, unparsed and unmatched counts."""
    sides = (base, head)
    per_side = [None if side.blocked or side.failed else _side_run_verdicts(side) for side in sides]
    rows = []
    for name, attribute in (("inside", "inside_ms"), ("outside", "outside_ms")):
        cells = [_missing_cell(side) if verdicts is None
                 else _waits_cell([wait_ms for verdict in verdicts for wait_ms in getattr(verdict, attribute)])
                 for side, verdicts in zip(sides, per_side)]
        rows.append([label, f"fallback_receive waits {name} slow dispatches", *cells, ""])
    cells = []
    for side, verdicts in zip(sides, per_side):
        if verdicts is None:
            cells.append(_missing_cell(side))
            continue
        coverages = [verdict.coverage for verdict in verdicts]
        coverage = ("unavailable" if "unavailable" in coverages or not coverages else
                    "incomplete" if "incomplete" in coverages else "complete")
        counts = [sum(getattr(verdict, name) for verdict in verdicts)
                  for name in ("unparsed", "unmatched_enter", "unmatched_return")]
        reasons = sorted({reason for verdict in verdicts for reason in verdict.reasons})
        cells.append(f"{fallback_side_verdict(side)} (coverage {coverage}, unparsed {counts[0]}, "
                     f"unmatched_enter {counts[1]}, unmatched_return {counts[2]})"
                     + (f": {'; '.join(reasons)}" if fallback_side_verdict(side) == "inconclusive" and reasons else ""))
    rows.append([label, "fallback_receive verdict", *cells, ""])
    return rows


COUNTERS_HEADER = ("| Scenario | Phase | Counter (unit) | Baseline | PR | Change |\n"
                   "| --- | --- | --- | --- | --- | --- |\n")
# Why a counters set has no base side: the base declares no perf-counters feature.
COUNTERS_HEAD_ONLY = "n/a: the base does not declare perf-counters, so counters run on the head only"
OVERHEAD_HEADER = ("| Scenario | Metric (unit) | Counters off | Counters on | Change |\n"
                   "| --- | --- | --- | --- | --- |\n")
# The scenarios whose timed metrics would show the counters' own cost: typing and the output flood.
OVERHEAD_SCENARIOS = ("S2", "S3")
OVERHEAD_NOTE = "counters-on vs counters-off on the head; sequential sets, not interleaved"
COUNTERS_UNSUPPORTED = ("counters: head does not support them (its sonicterm-app declares no perf-counters "
                        "feature), so the counters set was skipped")


def overhead_applies(label: str) -> bool:
    """Whether a scenario/variant label gets an overhead row set."""
    return label.partition("/")[0] in OVERHEAD_SCENARIOS


def _figure(value: float) -> str:
    """A count's median: whole numbers without a fraction, others with one decimal."""
    return f"{value:.0f}" if float(value).is_integer() else f"{value:.1f}"


def _bucket_label(index: int, bounds: Sequence[int], unit: str) -> str:
    """A bucket as its upper bound, or `>last` for the overflow bucket; never an exact value."""
    return f"≤{bounds[index]} {unit}" if index < len(bounds) else f">{bounds[-1]} {unit}"


def _histogram_cell(histograms: Sequence[Mapping], unit: str) -> tuple[str, float] | None:
    """Pool a field's histograms across runs: the cell (p95 and max as bucket bounds, the mean from sum_us/count
    in the histogram's unit) and its mean; None when it has no events."""
    bounds = HISTOGRAM_BOUNDS[unit]
    pooled = [sum(histogram["counts"][index] for histogram in histograms) for index in range(len(bounds) + 1)]
    events = sum(pooled)
    if events == 0:
        return None
    # Nearest rank, in integers so the 95% boundary is exact.
    rank = (95 * events + 99) // 100
    running, percentile_index = 0, len(pooled) - 1
    for index, count in enumerate(pooled):
        running += count
        if running >= rank:
            percentile_index = index
            break
    max_index = max(index for index, count in enumerate(pooled) if count)
    mean = sum(histogram["sum_us"] for histogram in histograms) / MICROSECONDS_PER_UNIT[unit] / events
    return (f"p95 {_bucket_label(percentile_index, bounds, unit)}, max {_bucket_label(max_index, bounds, unit)}, "
            f"mean {mean:.2f} {unit} ({events} events)"), mean


def _counter_phases(side: SideRuns) -> dict[str, list[dict]]:
    """A side's frame_counters objects per phase name, one per valid run that reported the phase."""
    phases: dict[str, list[dict]] = {}
    for outcome in side.outcomes:
        for phase in (outcome.result or {}).get("phases") or []:
            if isinstance(phase.get("frame_counters"), dict):
                phases.setdefault(str(phase.get("name")), []).append(phase["frame_counters"])
    return phases


def _counter_cell(per_run: Sequence[dict], run_count: int, section: str, field_name: str,
                  histogram_unit: str | None) -> tuple[str | None, float | None, bool]:
    """One side's cell for a field: its text (None when no run reported it), the figure a change compares
    (a count's median, a histogram's mean) and whether any run saw an event."""
    present = [sections[section][field_name] for sections in per_run
               if isinstance(sections.get(section), dict) and field_name in sections[section]]
    if not present:
        return None, None, False
    if histogram_unit is None and field_name.endswith("_ns"):
        # Nanoseconds stay exact through every delta; only the display converts to microseconds.
        figure = median(present)
        text = f"{figure / 1000:.2f} ({min(present) / 1000:.2f}–{max(present) / 1000:.2f})"
        active = any(present)
    elif histogram_unit is None:
        figure = median(present)
        text, active = f"{_figure(figure)} ({min(present)}–{max(present)})", any(present)
    else:
        summary = _histogram_cell(present, histogram_unit)
        text, figure, active = (("no events", None, False) if summary is None else (summary[0], summary[1], True))
    if len(present) < run_count:
        text += f", {len(present)}/{run_count} runs"
    return text, figure, active


def _counter_label(field_name: str, histogram_unit: str | None) -> str:
    """A field's unit label: a histogram's unit; for an integer, `us, summed` when its name ends in _us
    (a summed duration in microseconds), `us, summed from ns` when it ends in _ns, otherwise `count`."""
    if histogram_unit is not None:
        return histogram_unit
    if field_name.endswith("_ns"):
        return "us, summed from ns"
    return "us, summed" if field_name.endswith("_us") else "count"


# The matched fields of one render-attempt class, by role; a class's prefix is `render_` or `apply_`.
ATTEMPT_SPLIT_ROLES = ("attempts", "attempt_ns", "attempt_shape_ns", "attempt_raster_ns", "attempt_shape_requests",
                       "attempt_raster_calls", "attempt_raster_tiles")


def attempt_split(per_run: Sequence[dict], prefix: str) -> tuple[str | None, int]:
    """One side's pooled split of a render-attempt class in one phase, and its pooled attempt count.

    Only runs that carry every field of the class count, and their raw totals are summed before any division,
    so the shares come from one population and the shaping, rasterizing and remaining shares add up to 100%.
    The text is None when no run carries the class.
    """
    fields = [prefix + role for role in ATTEMPT_SPLIT_ROLES]
    runs = [sections["renderer"] for sections in per_run
            if isinstance(sections.get("renderer"), dict)
            and all(_is_int(sections["renderer"].get(field)) for field in fields)]
    if not runs:
        return None, 0
    pooled = {role: sum(run[prefix + role] for run in runs) for role in ATTEMPT_SPLIT_ROLES}
    attempts, attempt_ns = pooled["attempts"], pooled["attempt_ns"]
    scope = f"{len(runs)}/{len(per_run)} runs"
    if attempts == 0:
        return f"no {prefix.rstrip('_')} attempts ({scope})", 0
    if attempt_ns == 0:
        return f"{attempts} attempts with no measured time ({scope})", attempts
    shape, raster = pooled["attempt_shape_ns"], pooled["attempt_raster_ns"]
    other = attempt_ns - shape - raster
    text = (f"{attempts} attempts ({scope}): shaping {100 * shape / attempt_ns:.1f}%, rasterizing "
            f"{100 * raster / attempt_ns:.1f}%, other {100 * other / attempt_ns:.1f}%; per attempt "
            f"{attempt_ns / attempts / 1000:.2f} us, {pooled['attempt_shape_requests'] / attempts:.1f} shape "
            f"requests, {pooled['attempt_raster_calls'] / attempts:.1f} raster calls, "
            f"{pooled['attempt_raster_tiles'] / attempts:.1f} tiles")
    return text, attempts


def presenter_counter_notes(label: str, side_name: str, side: SideRuns) -> list[str]:
    """Notes for each valid counters run whose renderer frame counts disagree with its recorded presenter.

    result.json records the presenter on Windows and macOS: frames drawn through GDI count as software_frames and
    frames presented through wgpu as gpu_frames. A run that recorded no presenter (a base older than the macOS
    record) is not checked.
    """
    notes = []
    for index, outcome in enumerate(side.outcomes, 1):
        presenter = (outcome.result or {}).get("presenter")
        if not isinstance(presenter, Mapping):
            continue
        totals = {"software_frames": 0, "gpu_frames": 0}
        for phase in (outcome.result or {}).get("phases") or []:
            renderer = (phase.get("frame_counters") or {}).get("renderer")
            for name in totals:
                if isinstance(renderer, Mapping) and _is_int(renderer.get(name)):
                    totals[name] += renderer[name]
        gdi = presenter.get("windows_gdi") is True
        unexpected, presenter_name = (("gpu_frames", "GDI") if gdi else ("software_frames", "wgpu"))
        if totals[unexpected]:
            notes.append(f"{label} {side_name} run {index} presented through {presenter_name}, but its counters "
                         f"report {totals[unexpected]} {unexpected}")
    return notes


def counter_rows(label: str, base: SideRuns, head: SideRuns) -> tuple[list[list[str]], int]:
    """Rows of the counters table, and how many fields that were 0 on both sides were left out.

    Per phase, a count is the median of its per-run deltas with their range, and a change compares the
    medians; a histogram pools every run's buckets, and a change compares the means. A base without the
    perf-counters feature, or a field the base's contract lacks, reads `n/a`, with no change.
    """
    head_only = base.blocked == COUNTERS_HEAD_ONLY
    rows = [[label, "", "status", "n/a" if head_only else _status_cell(base), _status_cell(head), ""]]
    if head.blocked or head.failed:
        return rows, 0
    sides = (base, head)
    # A head-only set has no base counters; a blocked or failed base has none to read either.
    per_side = [{} if head_only or base.blocked or base.failed else _counter_phases(base), _counter_phases(head)]
    phase_names = list(per_side[1]) + [name for name in per_side[0] if name not in per_side[1]]
    omitted = 0
    for phase_name in phase_names:
        for section, (counts, histograms) in FRAME_COUNTER_FIELDS.items():
            for field_name in counts + histograms:
                unit = None if field_name in counts else field_name.rsplit("_", 1)[1]
                cells = [_counter_cell(phases.get(phase_name, []), len(side.outcomes), section, field_name, unit)
                         for side, phases in zip(sides, per_side)]
                if not any(active for _text, _figure, active in cells):
                    omitted += 1
                    continue
                texts = [text if text is not None else ("n/a" if head_only else _missing_cell(side))
                         for (text, _figure, _active), side in zip(cells, sides)]
                rows.append([label, phase_name, f"{section}.{field_name} ({_counter_label(field_name, unit)})",
                             texts[0], texts[1],
                             percent_change(cells[0][1], cells[1][1])])
        rows.extend(_attempt_split_rows(label, phase_name, sides, per_side, head_only))
        rows.extend(_derived_counter_rows(label, phase_name, sides, per_side, head_only))
    return rows, omitted


# The only variant whose samples are split; S2/flood types beside a flood and is never armed.
SPLIT_LABEL = "S2/default"


def _side_latency_samples(side: SideRuns) -> list[dict] | None:
    """Every latency sample of a side's valid runs, or None when the side has no valid run to read."""
    if side.blocked or side.failed or not side.outcomes:
        return None
    samples = []
    for outcome in side.outcomes:
        latency = (outcome.result or {}).get("latency")
        if isinstance(latency, dict) and isinstance(latency.get("samples"), list):
            samples.extend(sample for sample in latency["samples"] if isinstance(sample, dict))
    return samples


def _split_cells(samples: list[dict] | None, side: SideRuns) -> dict[str, tuple[str, float | None]]:
    """One side's split rows: each row's cell and the figure its change compares."""
    if samples is None:
        missing = "n/a" if side.blocked == COUNTERS_HEAD_ONLY else _missing_cell(side)
        return {"missing": (missing, None)}
    credited = [sample for sample in samples if sample.get("split_reason") not in (None, NOT_CREDITED)]
    splits = [sample["split"] for sample in credited if isinstance(sample.get("split"), dict)]
    reasons: dict[str, int] = {}
    for sample in credited:
        reasons[sample["split_reason"]] = reasons.get(sample["split_reason"], 0) + 1
    flags = {name: sum(1 for split in splits if test(split)) for name, test in (
        ("suppressed", lambda split: split.get("delivery") == "suppressed"),
        ("coalesced", lambda split: split.get("coalesced") is True),
        ("sync_open", lambda split: split.get("sync_open") is True))}
    reason_text = ", ".join(f"{reason} {count}" for reason, count in sorted(reasons.items())) or "none"
    cells = {"reasons": (f"{reason_text}; " + ", ".join(f"{name} {count}" for name, count in flags.items()), None)}
    if not splits:
        # A base built without the feature splits nothing, so every credited sample reads unsupported. An empty
        # reason set is no such evidence: nothing credited is unavailable, and supported samples are 0% covered.
        unsupported = bool(reasons) and set(reasons) == {"unsupported"}
        empty = "n/a (unsupported)" if unsupported else "n/a (no split)"
        if unsupported:
            cells["coverage"] = (empty, None)
        elif credited:
            cells["coverage"] = (f"0.0% (0/{len(credited)})", 0.0)
        else:
            cells["coverage"] = ("unavailable", None)
        return {"empty": (empty, None), **cells}
    for part in SPLIT_PARTS:
        values = [split[part] for split in splits]
        cells[f"{part} median"] = (f"{median(values):.3f} ms ({len(values)} splits)", median(values))
        cells[f"{part} p95"] = (f"{percentile_95(values):.3f} ms", percentile_95(values))
    lags = [split["delivery_lag_us"] for split in splits]
    cells["delivery_lag_us p95"] = (f"{percentile_95(lags):.1f} us", percentile_95(lags))
    coverage = len(splits) / len(credited)
    cells["coverage"] = (f"{coverage * 100:.1f}% ({len(splits)}/{len(credited)})", coverage * 100)
    return cells


# The split rows' labels, in order, and the key of each row's cell.
SPLIT_ROWS = (
    ("split input to parse, median (ms)", "input_to_parse_ms median"),
    ("split input to parse, p95 (ms)", "input_to_parse_ms p95"),
    ("split parse to publication, median (ms)", "parse_to_publication_ms median"),
    ("split parse to publication, p95 (ms)", "parse_to_publication_ms p95"),
    ("split publication to present, median (ms)", "publication_to_present_ms median"),
    ("split publication to present, p95 (ms)", "publication_to_present_ms p95"),
    ("split delivery lag, p95 (us)", "delivery_lag_us p95"),
    ("split coverage (%)", "coverage"),
    ("split reasons (credited samples)", "reasons"),
)


def split_rows(label: str, base: SideRuns, head: SideRuns, latency_split_schema: int | None) -> list[list[str]]:
    """The counters table's split rows for S2/default's typing phase, only when the head harness declares split
    schema 1. A side without the split reads `n/a (unsupported)`; a change compares the two figures."""
    if label != SPLIT_LABEL or latency_split_schema != 1:
        return []
    sides = [_split_cells(_side_latency_samples(side), side) for side in (base, head)]
    rows = []
    for row_label, key in SPLIT_ROWS:
        texts, figures = [], []
        for cells in sides:
            text, figure = cells.get(key) or cells.get("empty") or cells["missing"]
            texts.append(text)
            figures.append(figure)
        rows.append([label, "typing", row_label, texts[0], texts[1], percent_change(figures[0], figures[1])])
    return rows


def _renderer_runs(per_run: Sequence[dict], fields: Sequence[str]) -> list[dict]:
    """The renderer sections of the runs that carry every integer field in `fields`."""
    return [sections["renderer"] for sections in per_run
            if isinstance(sections.get("renderer"), dict)
            and all(_is_int(sections["renderer"].get(field)) for field in fields)]


def _pooled_ratio(per_run: Sequence[dict], numerator: Sequence[str],
                  denominator: Sequence[str]) -> tuple[str | None, float | None]:
    """A pooled ratio over the runs carrying every field: the sums of `numerator` over the sums of
    `denominator`. None text when no run carries the fields; `n/a` when the pooled denominator is 0."""
    runs = _renderer_runs(per_run, list(numerator) + list(denominator))
    if not runs:
        return None, None
    top = sum(run[field] for run in runs for field in numerator)
    bottom = sum(run[field] for run in runs for field in denominator)
    scope = f"{len(runs)}/{len(per_run)} runs"
    if bottom == 0:
        return f"n/a (denominator 0, {scope})", None
    return f"{top / bottom:.3f} ({top}/{bottom}, {scope})", top / bottom


def _pooled_per_assembly(per_run: Sequence[dict], numerator: str) -> tuple[str | None, float | None]:
    """`numerator` per assembled frame, pooled over the runs carrying it and the assembly histogram: the
    sum of `numerator` over the sum of the histogram's sample counts. None text when no run carries both;
    `n/a` when no run assembled a frame."""
    runs = [sections["renderer"] for sections in per_run
            if isinstance(sections.get("renderer"), dict)
            and _is_int(sections["renderer"].get(numerator))
            and isinstance(sections["renderer"].get("assembly_us"), dict)]
    if not runs:
        return None, None
    top = sum(run[numerator] for run in runs)
    bottom = sum(sum(run["assembly_us"]["counts"]) for run in runs)
    scope = f"{len(runs)}/{len(per_run)} runs"
    if bottom == 0:
        return f"n/a (no assembly, {scope})", None
    return f"{top / bottom:.3f} ({top}/{bottom}, {scope})", top / bottom


def _assembly_means(per_run: Sequence[dict]) -> tuple[str | None, float | None]:
    """Each counters run's exact assembly mean, `assembly_sum_us / samples`, at its run position, and the
    pooled mean. A run without the histogram reads `n/a (no histogram)` and one that assembled nothing
    `n/a (no assembly)`, so every run keeps its place. None text when no run carries the histogram; the
    pooled figure is None when no run assembled a frame."""
    histograms = [sections["renderer"].get("assembly_us") if isinstance(sections.get("renderer"), dict) else None
                  for sections in per_run]
    if not any(isinstance(histogram, dict) for histogram in histograms):
        return None, None
    cells = []
    for position, histogram in enumerate(histograms, 1):
        if not isinstance(histogram, dict):
            cells.append(f"run {position} n/a (no histogram)")
        elif not sum(histogram["counts"]):
            cells.append(f"run {position} n/a (no assembly)")
        else:
            cells.append(f"run {position} {histogram['sum_us'] / sum(histogram['counts']):.2f} us")
    sampled = [histogram for histogram in histograms if isinstance(histogram, dict) and sum(histogram["counts"])]
    samples = sum(sum(histogram["counts"]) for histogram in sampled)
    if not samples:
        return f"{', '.join(cells)}; pooled n/a ({len(sampled)}/{len(per_run)} runs)", None
    pooled = sum(histogram["sum_us"] for histogram in sampled) / samples
    return f"{', '.join(cells)}; pooled {pooled:.2f} us ({len(sampled)}/{len(per_run)} runs)", pooled


# Derived rows: a label naming the formula and pooling, and the function computing one side's cell.
DERIVED_COUNTER_ROWS = (
    ("row-cache hit ratio = hits / (hits + misses), counters runs pooled",
     lambda per_run: _pooled_ratio(per_run, ("row_cache_hits",), ("row_cache_hits", "row_cache_misses"))),
    ("assembly mean per counters run = assembly_sum_us / Σ assembly_buckets, per run", _assembly_means),
    ("shape+measure requests per drawn frame = shape_requests / (gpu_frames + software_frames), context only",
     lambda per_run: _pooled_ratio(per_run, ("shape_requests",), ("gpu_frames", "software_frames"))),
    ("partial fallback ratio = partial_fallbacks / (partial_frames + partial_fallbacks), context only",
     lambda per_run: _pooled_ratio(per_run, ("partial_fallbacks",), ("partial_frames", "partial_fallbacks"))),
    ("tab-title reuses per assembly = tab_title_reuses / Σ assembly_buckets, counters runs pooled",
     lambda per_run: _pooled_per_assembly(per_run, "tab_title_reuses")),
    ("chrome-run reuses per assembly = chrome_run_reuses / Σ assembly_buckets, counters runs pooled",
     lambda per_run: _pooled_per_assembly(per_run, "chrome_run_reuses")),
)


def _derived_counter_rows(label: str, phase_name: str, sides: Sequence[SideRuns], per_side: Sequence[dict],
                          head_only: bool) -> list[list[str]]:
    """One phase's derived rows, each printed only when some side has a nonzero denominator; a side whose runs
    lack the fields, or whose denominator is 0, reads `n/a`, and the change compares the two figures."""
    rows = []
    for row_label, compute in DERIVED_COUNTER_ROWS:
        cells = [compute(phases.get(phase_name, [])) for phases in per_side]
        if all(figure is None for _text, figure in cells):
            continue
        texts = [text if text is not None else ("n/a" if head_only else _missing_cell(side))
                 for (text, _figure), side in zip(cells, sides)]
        rows.append([label, phase_name, row_label, texts[0], texts[1], percent_change(cells[0][1], cells[1][1])])
    return rows


def _attempt_split_rows(label: str, phase_name: str, sides: Sequence[SideRuns], per_side: Sequence[dict],
                        head_only: bool) -> list[list[str]]:
    """The pooled render-attempt split rows of one phase, all attempts then apply attempts. A phase where no
    side drew an attempt reads as one row saying so; none when no side carries the fields. A side whose runs
    lack the fields reads `n/a`."""
    splits = {prefix: [attempt_split(phases.get(phase_name, []), prefix) for phases in per_side]
              for prefix in ("render_", "apply_")}
    if all(text is None for text, _attempts in splits["render_"]):
        return []
    if not any(attempts for _text, attempts in splits["render_"]):
        # No side drew an attempt: one explicit row, rather than an omitted phase or two empty rows.
        texts = [text if text is not None else ("n/a" if head_only else _missing_cell(side))
                 for (text, _attempts), side in zip(splits["render_"], sides)]
        return [[label, phase_name, "renderer attempt split (pooled)", texts[0], texts[1], ""]]
    rows = []
    for prefix, name in (("render_", "every attempt"), ("apply_", "fallback apply attempts")):
        texts = [text if text is not None else ("n/a" if head_only else _missing_cell(side))
                 for (text, _attempts), side in zip(splits[prefix], sides)]
        rows.append([label, phase_name, f"renderer attempt split: {name} (pooled)", texts[0], texts[1], ""])
    return rows


def attempt_split_details(label: str, base: SideRuns, head: SideRuns) -> list[str]:
    """Each run's own split, for every phase in which any run on either side drew a render attempt, so both
    pooled rows' whole populations can be read run by run: every run of that phase is listed, including runs
    that applied nothing and runs that lack the fields (`n/a`). Lines for the details block."""
    sides = (("base", base), ("head", head))
    applying = {name for _side_name, side in sides for name, phases in _counter_phases(side).items()
                if any(attempt_split([counters], "render_")[1] for counters in phases)}
    lines = []
    for side_name, side in sides:
        for index, outcome in enumerate(side.outcomes, 1):
            for phase in (outcome.result or {}).get("phases") or []:
                if str(phase.get("name")) not in applying:
                    continue
                counters = phase.get("frame_counters")
                texts = [attempt_split([counters], prefix)[0] if isinstance(counters, dict) else None
                         for prefix in ("render_", "apply_")]
                every_text, apply_text = (text or "n/a" for text in texts)
                lines.append(f"- {label} {side_name} run {index} {phase.get('name')}: every attempt {every_text}; "
                             f"fallback apply attempts {apply_text}")
    return lines


# Row-run shaping diagnostic: the frozen measurement protocol's metrics and decision. Each decision phase is a
# (scenario/variant, phase) pair; the positive workloads must repeat, the negative control must not.
ROW_RUN_DECISION_PHASES = (("S4/default", "stream"), ("S10/default", "stream"), ("S10/sync", "stream"),
                           ("S10/powerline", "stream"), ("S10/cjk-tui", "stream"), ("S10/unique", "stream"))
ROW_RUN_POSITIVE_LABELS = ("S10/powerline", "S10/cjk-tui")
ROW_RUN_NEGATIVE_LABEL = "S10/unique"
# The phases whose overhead is bounded, and the reference workloads one of which must show the opportunity.
ROW_RUN_OVERHEAD_LABELS = ("S4/default", "S10/default", "S10/unique")
ROW_RUN_REFERENCE_LABELS = ("S4/default", "S10/default", "S10/sync")
ROW_RUN_PLATFORMS = ("macos", "windows")
ROW_RUN_MIN_RUNS = 2
ROW_RUN_MIN_CLASSIFIED = 1000
# Thresholds as exact fractions, so a boundary value is compared without float rounding.
ROW_RUN_MAX_FAILED_SHARE = Fraction(1, 100)
ROW_RUN_MAX_UNSTABLE_SHARE = Fraction(1, 100)
ROW_RUN_MAX_NEGATIVE_REPEAT = Fraction(5, 1000)
ROW_RUN_MIN_POSITIVE_REPEAT = Fraction(30, 100)
ROW_RUN_MAX_OVERHEAD = Fraction(3, 100)
ROW_RUN_MIN_OPPORTUNITY = Fraction(5, 100)
# After this many restarts on a changed diagnostic the decision is posted as PENDING.
ROW_RUN_MAX_RESTARTS = 2
# The head's sixteen diagnostic counters, and the attempt fields both sides need.
ROW_RUN_HEAD_FIELDS = ("row_run_shape_calls", "row_run_shape_ok", "row_run_shape_failed", "row_run_shape_ns",
                       "row_run_shape_first", "row_run_shape_repeats", "row_run_shape_same_pass_repeats",
                       "row_run_shape_repeat_ns", "row_run_shape_unstable", "row_run_shape_retry_repeats",
                       "row_run_unpresented_calls", "row_run_unpresented_ns", "row_run_identity_resets",
                       "row_run_shape_overflows", "row_run_pass_overflows", "row_run_diag_ns")
ROW_RUN_ATTEMPT_FIELDS = ("render_attempts", "render_attempt_ns")
PENDING_EVIDENCE = "PENDING (evidence)"
PENDING_DIAGNOSTIC = "PENDING (diagnostic)"
PENDING_OVERHEAD = "PENDING (overhead)"
PENDING_RERUN = "PENDING (rerun due)"
PENDING_RESTARTS = "PENDING"
ROW_RUN_HEADER = ("| Platform | Scenario | Phase | Metric | Baseline | PR |\n"
                  "| --- | --- | --- | --- | --- | --- |\n")
ROW_RUN_NOTE = ("Each figure sums the accepted counters runs of the phase before any division. R = repeats / "
                "(first + repeats) and T = repeat_ns / (assembly sum_us x 1,000) are head only; O_asm and O_att "
                "compare the head's pooled assembly and per-attempt means with the base's. The baseline reads n/a "
                "for a counter the base does not report. This is partial shard evidence: the shard measures only some "
                "decision phases and makes no decision; `--row-run-decide` decides over every shard of one "
                "workflow execution from row-run-evidence.json.")


@dataclass(frozen=True)
class RowRunPhase:
    """One decision phase's accepted counters runs: each run's renderer section, base and head."""

    base: tuple
    head: tuple


@dataclass(frozen=True)
class RowRunDecision:
    """The step that decided (1-4), the outcome, and the reason printed beside it."""

    step: int
    outcome: str
    reason: str


def row_run_platform(platform_name: str = sys.platform) -> str:
    """The decision's platform key for a host's sys.platform."""
    return {"darwin": "macos", "win32": "windows"}.get(platform_name, platform_name)


def _accepted_renderers(side: SideRuns, phase_name: str) -> tuple:
    """The renderer counters of `side`'s accepted runs in `phase_name`; a blocked or failed side has none.
    SideRuns holds only valid runs, so every run here passed the schema and focus checks."""
    if side.blocked or side.failed:
        return ()
    renderers = []
    for counters in _counter_phases(side).get(phase_name, []):
        renderer = counters.get("renderer")
        renderers.append(dict(renderer) if isinstance(renderer, Mapping) else {})
    return tuple(renderers)


def row_run_phase(base: SideRuns, head: SideRuns, phase_name: str) -> RowRunPhase:
    """One decision phase's accepted runs on each side."""
    return RowRunPhase(_accepted_renderers(base, phase_name), _accepted_renderers(head, phase_name))


def _row_run_total(runs: Sequence[Mapping], field_name: str) -> int | None:
    """`field_name` summed over every run, or None when there is no run or one lacks it."""
    if not runs or not all(_is_int(run.get(field_name)) for run in runs):
        return None
    return sum(run[field_name] for run in runs)


def _assembly_totals(runs: Sequence[Mapping]) -> tuple[int, int] | None:
    """The assembly histogram's pooled sum_us and event count, or None when a run lacks the histogram."""
    histograms = [run.get("assembly_us") for run in runs]
    if not runs or not all(isinstance(histogram, Mapping) and _is_int(histogram.get("sum_us"))
                           and isinstance(histogram.get("counts"), list) for histogram in histograms):
        return None
    return (sum(histogram["sum_us"] for histogram in histograms),
            sum(sum(histogram["counts"]) for histogram in histograms))


def _pooled_fraction(numerator: int | None, denominator: int | None) -> Fraction | None:
    """numerator / denominator of pooled sums, or None when either is missing or the denominator is 0."""
    if numerator is None or denominator is None or denominator == 0:
        return None
    return Fraction(numerator, denominator)


def _relative_change(head_mean: Fraction | None, base_mean: Fraction | None) -> Fraction | None:
    """head / base - 1, or None when either mean is missing or the base mean is 0."""
    if head_mean is None or base_mean is None or base_mean == 0:
        return None
    return head_mean / base_mean - 1


def row_run_metrics(phase: RowRunPhase) -> dict[str, Fraction | None]:
    """R, T, O_asm and O_att of one phase; each sums its numerator and denominator over the accepted runs
    before dividing, and is None when a run lacks a field or a pooled denominator is 0."""
    first, repeats = (_row_run_total(phase.head, name) for name in ("row_run_shape_first", "row_run_shape_repeats"))
    classified = None if first is None or repeats is None else first + repeats
    head_assembly, base_assembly = _assembly_totals(phase.head), _assembly_totals(phase.base)
    assembly_ns = None if head_assembly is None else head_assembly[0] * 1000
    means = []
    for runs, assembly in ((phase.head, head_assembly), (phase.base, base_assembly)):
        assembly_mean = None if assembly is None else _pooled_fraction(*assembly)
        attempt_mean = _pooled_fraction(_row_run_total(runs, "render_attempt_ns"),
                                        _row_run_total(runs, "render_attempts"))
        means.append((assembly_mean, attempt_mean))
    return {"R": _pooled_fraction(repeats, classified),
            "T": _pooled_fraction(_row_run_total(phase.head, "row_run_shape_repeat_ns"), assembly_ns),
            "O_asm": _relative_change(means[0][0], means[1][0]),
            "O_att": _relative_change(means[0][1], means[1][1])}


def row_run_evidence_problems(where: str, phase: RowRunPhase) -> list[str]:
    """Why one phase is not evidence enough: too few accepted runs, a missing field, too few classified
    calls on the head, or a zero assembly or attempt figure on either side."""
    problems = []
    for side_name, runs, needed in (("base", phase.base, ()), ("head", phase.head, ROW_RUN_HEAD_FIELDS)):
        if len(runs) < ROW_RUN_MIN_RUNS:
            problems.append(f"{where} {side_name}: {len(runs)} accepted counters runs, needs {ROW_RUN_MIN_RUNS}")
            continue
        missing = sorted({name for run in runs for name in needed + ROW_RUN_ATTEMPT_FIELDS
                          if not _is_int(run.get(name))})
        if _assembly_totals(runs) is None:
            missing.append("assembly_us")
        if missing:
            problems.append(f"{where} {side_name}: missing {', '.join(missing)}")
            continue
        sum_us, events = _assembly_totals(runs)
        totals = {"assembly events": events, "assembly sum_us": sum_us,
                  "render_attempts": _row_run_total(runs, "render_attempts"),
                  "render_attempt_ns": _row_run_total(runs, "render_attempt_ns")}
        problems.extend(f"{where} {side_name}: {name} is 0" for name, total in totals.items() if total == 0)
        if side_name == "head":
            classified = _row_run_total(runs, "row_run_shape_first") + _row_run_total(runs, "row_run_shape_repeats")
            if classified < ROW_RUN_MIN_CLASSIFIED:
                problems.append(f"{where} head: first + repeats = {classified}, needs {ROW_RUN_MIN_CLASSIFIED}")
    return problems


def _percent(value: Fraction | None) -> str:
    """A fraction as a percentage with two decimals, or n/a."""
    return "n/a" if value is None else f"{float(value) * 100:.2f}%"


def row_run_decision(evidence: Mapping[tuple[str, str, str], RowRunPhase]) -> RowRunDecision:
    """Apply the frozen decision steps to one run's evidence, keyed (platform, label, phase); the first step
    that fails decides, with its reason."""
    problems = []
    for platform_key in ROW_RUN_PLATFORMS:
        for label, phase_name in ROW_RUN_DECISION_PHASES:
            where = f"{platform_key} {label} {phase_name}"
            phase = evidence.get((platform_key, label, phase_name))
            if phase is None:
                problems.append(f"{where}: not measured")
            else:
                problems.extend(row_run_evidence_problems(where, phase))
    if problems:
        return RowRunDecision(1, PENDING_EVIDENCE, "; ".join(problems))
    metrics = {key: row_run_metrics(phase) for key, phase in evidence.items()}
    for platform_key in ROW_RUN_PLATFORMS:
        for label, phase_name in ROW_RUN_DECISION_PHASES:
            where = f"{platform_key} {label} {phase_name}"
            head = evidence[(platform_key, label, phase_name)].head
            calls = _row_run_total(head, "row_run_shape_calls")
            for name in ("row_run_shape_overflows", "row_run_pass_overflows"):
                if _row_run_total(head, name):
                    problems.append(f"{where}: {name} = {_row_run_total(head, name)}, must be 0")
            for name, bound in (("row_run_shape_failed", ROW_RUN_MAX_FAILED_SHARE),
                                ("row_run_shape_unstable", ROW_RUN_MAX_UNSTABLE_SHARE)):
                share = _pooled_fraction(_row_run_total(head, name), calls)
                if share is None or share > bound:
                    problems.append(f"{where}: {name} / calls = {_percent(share)}, must be <= {_percent(bound)}")
            repeat = metrics[(platform_key, label, phase_name)]["R"]
            if label == ROW_RUN_NEGATIVE_LABEL and (repeat is None or repeat > ROW_RUN_MAX_NEGATIVE_REPEAT):
                problems.append(f"{where}: R = {_percent(repeat)}, the negative control must be "
                                f"<= {_percent(ROW_RUN_MAX_NEGATIVE_REPEAT)}")
            if label in ROW_RUN_POSITIVE_LABELS and (repeat is None or repeat < ROW_RUN_MIN_POSITIVE_REPEAT):
                problems.append(f"{where}: R = {_percent(repeat)}, a positive workload must be "
                                f">= {_percent(ROW_RUN_MIN_POSITIVE_REPEAT)}")
    if problems:
        return RowRunDecision(2, PENDING_DIAGNOSTIC, "; ".join(problems))
    for platform_key in ROW_RUN_PLATFORMS:
        for label in ROW_RUN_OVERHEAD_LABELS:
            for name in ("O_asm", "O_att"):
                overhead = metrics[(platform_key, label, "stream")][name]
                if overhead is None or overhead > ROW_RUN_MAX_OVERHEAD:
                    problems.append(f"{platform_key} {label} stream: {name} = {_percent(overhead)}, must be "
                                    f"<= +{_percent(ROW_RUN_MAX_OVERHEAD)}")
    if problems:
        return RowRunDecision(3, PENDING_OVERHEAD, "reduce the diagnostic's cost before merge; "
                              + "; ".join(problems))
    shares = {label: [metrics[(platform_key, label, "stream")]["T"] for platform_key in ROW_RUN_PLATFORMS]
              for label in ROW_RUN_REFERENCE_LABELS}
    summary = "; ".join(f"{label} T = " + ", ".join(f"{platform_key} {_percent(share)}" for platform_key, share
                                                   in zip(ROW_RUN_PLATFORMS, values))
                        for label, values in shares.items())
    winners = [label for label, values in shares.items()
               if all(share is not None and share >= ROW_RUN_MIN_OPPORTUNITY for share in values)]
    if winners:
        return RowRunDecision(4, "BUILD", f"{', '.join(winners)} spends >= {_percent(ROW_RUN_MIN_OPPORTUNITY)} of "
                              f"assembly on repeated row-run shaping on both platforms; {summary}")
    return RowRunDecision(4, "CLOSE", f"no one reference workload reaches {_percent(ROW_RUN_MIN_OPPORTUNITY)} on "
                          f"both platforms; {summary}")


def row_run_final(first: Mapping[tuple[str, str, str], RowRunPhase], first_run_id: str,
                  rerun: Mapping[tuple[str, str, str], RowRunPhase] | None = None, rerun_id: str = "",
                  restarts: int = 0) -> RowRunDecision:
    """The decision for one head under the rerun rule. A first run that fails step 1, 2 or 3 gets one same-head
    rerun, which replaces it (never pooled); the rerun's BUILD or CLOSE stands. A rerun that fails step 1 is
    PENDING (evidence) with both run ids; one that fails step 2 or 3 restarts on a changed diagnostic, and after
    `restarts` reaches the limit the decision is PENDING."""
    decision = row_run_decision(first)
    if decision.step == 4:
        return decision
    if rerun is None:
        return RowRunDecision(decision.step, PENDING_RERUN,
                              f"run {first_run_id} failed step {decision.step}; one same-head rerun is due: "
                              f"{decision.reason}")
    replaced = row_run_decision(rerun)
    if replaced.step == 4:
        return replaced
    runs = f"runs {first_run_id} and {rerun_id}"
    if replaced.step == 1:
        return RowRunDecision(1, PENDING_EVIDENCE, f"{runs}: {replaced.reason}")
    if restarts >= ROW_RUN_MAX_RESTARTS:
        return RowRunDecision(replaced.step, PENDING_RESTARTS,
                              f"{runs} failed step {replaced.step} after {restarts} restarts: {replaced.reason}")
    return RowRunDecision(replaced.step, replaced.outcome,
                          f"{runs} failed step {replaced.step}; change the diagnostic, not the thresholds, and "
                          f"take a fresh eligible run on the new head: {replaced.reason}")


# The table's per-phase sums, in order; the base reports only what its tree counts.
ROW_RUN_TABLE_FIELDS = ROW_RUN_HEAD_FIELDS + ROW_RUN_ATTEMPT_FIELDS


def _row_run_sum_cell(runs: Sequence[Mapping], field_name: str) -> str:
    """A pooled sum with its run count, or n/a when there is no run or one lacks the field."""
    total = _row_run_total(runs, field_name)
    return "n/a" if total is None else f"{total} ({len(runs)} runs)"


def row_run_rows(evidence: Mapping[tuple[str, str, str], RowRunPhase]) -> list[list[str]]:
    """The Row-run shaping table: each measured decision phase's pooled sums and R, T, O_asm and O_att. A shard
    measures only some phases, so it prints no decision; --row-run-decide decides over the whole execution."""
    rows = []
    for (platform_key, label, phase_name), phase in evidence.items():
        prefix = [platform_key, label, phase_name]
        for field_name in ROW_RUN_TABLE_FIELDS:
            rows.append(prefix + [field_name, _row_run_sum_cell(phase.base, field_name),
                                  _row_run_sum_cell(phase.head, field_name)])
        assemblies = [_assembly_totals(runs) for runs in (phase.base, phase.head)]
        rows.append(prefix + ["assembly sum_us / events"]
                    + ["n/a" if totals is None else f"{totals[0]} / {totals[1]}" for totals in assemblies])
        metrics = row_run_metrics(phase)
        for name in ("R", "T"):
            rows.append(prefix + [name, "n/a", _percent(metrics[name])])
        for name in ("O_asm", "O_att"):
            rows.append(prefix + [name, "", _percent(metrics[name])])
    return rows


# Row-run evidence: each comparison shard's machine-readable record of the decision phases it measured. The
# decision is never made per shard; --row-run-decide combines the shards of one workflow execution.
ROW_RUN_EVIDENCE_FILE = "row-run-evidence.json"
ROW_RUN_EVIDENCE_SCHEMA = 1
ROW_RUN_PROTOCOL_VERSION = 1
# The PR pipeline's budget: from the run's creation to the attempt's final update, queueing included.
ROW_RUN_MAX_ELAPSED_S = 30 * 60
ROW_RUN_ARTIFACT = re.compile(r"perf-comparison-(?P<ref>.+)-(?P<head>[0-9a-f]{40})-(?P<platform>macOS|Windows)-"
                              r"(?P<shard>.+)-(?P<attempt>\d+)")
ROW_RUN_COMPARISON_JOB = re.compile(r"(?P<platform>macOS|Windows) before/after comparison \((?P<shard>.+)\)")
ROW_RUN_RESULT_JOB = "Performance comparison result"
ROW_RUN_PLATFORM_KEYS = {"macOS": "macos", "Windows": "windows"}
ROW_RUN_DEFAULT_REPOSITORY = "D0n9X1n/SonicTerm"


def row_run_protocol_digest() -> str:
    """The sha256 of the frozen decision protocol (phases, labels, bounds and limits); evidence measured under
    another protocol cannot be decided by this one."""
    protocol = {"version": ROW_RUN_PROTOCOL_VERSION, "phases": ROW_RUN_DECISION_PHASES,
                "positive": ROW_RUN_POSITIVE_LABELS, "negative": ROW_RUN_NEGATIVE_LABEL,
                "overhead": ROW_RUN_OVERHEAD_LABELS, "reference": ROW_RUN_REFERENCE_LABELS,
                "platforms": ROW_RUN_PLATFORMS, "min_runs": ROW_RUN_MIN_RUNS, "min_classified": ROW_RUN_MIN_CLASSIFIED,
                "bounds": [ROW_RUN_MAX_FAILED_SHARE, ROW_RUN_MAX_UNSTABLE_SHARE, ROW_RUN_MAX_NEGATIVE_REPEAT,
                           ROW_RUN_MIN_POSITIVE_REPEAT, ROW_RUN_MAX_OVERHEAD, ROW_RUN_MIN_OPPORTUNITY],
                "max_restarts": ROW_RUN_MAX_RESTARTS, "head_fields": ROW_RUN_HEAD_FIELDS,
                "attempt_fields": ROW_RUN_ATTEMPT_FIELDS}
    return hashlib.sha256(json.dumps(protocol, sort_keys=True, default=str).encode("utf-8")).hexdigest()


def _row_run_identity(evidence: object, out: Path) -> str:
    """An attempt directory's identity: its path under the comparison's output, with forward slashes."""
    try:
        return Path(str(evidence)).resolve().relative_to(out.resolve()).as_posix()
    except ValueError:
        # When: the attempt directory lies outside `out` (a test's fixed path), its own path names it.
        return Path(str(evidence)).as_posix()


def _row_run_renderer(outcome: RunOutcome, phase_name: str) -> dict | None:
    """The renderer section a run reported for `phase_name`, its fields as written (none filled in), or None
    when the run reported no counters for that phase, as the shard table reads it."""
    for phase in (outcome.result or {}).get("phases") or []:
        if str(phase.get("name")) == phase_name and isinstance(phase.get("frame_counters"), dict):
            renderer = phase["frame_counters"].get("renderer")
            return dict(renderer) if isinstance(renderer, Mapping) else {}
    return None


def row_run_audit(renderers: Sequence[Mapping]) -> dict:
    """Derived figures for audit only, recomputed from the records: each field's sum (None when a record lacks
    it), the assembly sum_us and event count, and the accepted-run count."""
    assembly = _assembly_totals(renderers)
    return {"accepted_runs": len(renderers),
            "sums": {name: _row_run_total(renderers, name) for name in ROW_RUN_TABLE_FIELDS},
            "assembly": None if assembly is None else {"sum_us": assembly[0], "events": assembly[1]}}


def row_run_export_side(result: SetResult, side_name: str, phase_name: str, out: Path) -> dict:
    """One side's accepted execution records for one phase, and its attempts that were not accepted.

    Only runs the final run-set classification kept (after the schema, focus, display and presenter checks) are
    records, each with its attempt-directory identity and renderer fields; every other attempt is disclosed with
    its kind and reasons and carries no value. A blocked or failed side has no accepted run."""
    side = getattr(result, side_name)
    accepted_runs = [] if side.blocked or side.failed else side.outcomes
    valid = [evidence for name, evidence, kind, _why in result.attempts if name == side_name and kind == "valid"]
    records, accepted_ids = [], set()
    # run_set appends a side's outcome exactly when its attempt ends valid, so the two lists align in order.
    for evidence, outcome in zip(valid, accepted_runs):
        identity = _row_run_identity(evidence, out)
        accepted_ids.add(identity)
        renderer = _row_run_renderer(outcome, phase_name)
        if renderer is not None:
            records.append({"execution": identity, "renderer": renderer})
    rejected = [{"execution": _row_run_identity(evidence, out), "kind": kind, "reasons": list(why)}
                for name, evidence, kind, why in result.attempts
                if name == side_name and _row_run_identity(evidence, out) not in accepted_ids]
    return {"status": side.blocked or side.failed or "", "accepted": records, "rejected": rejected,
            "audit": row_run_audit([record["renderer"] for record in records])}


def row_run_evidence_document(results: Iterable[SetResult], out: Path, shas: Mapping[str, str], harness_hash: str,
                              features: Mapping[str, Sequence[str]], short: bool, profile: Mapping,
                              counters: bool, environ: Mapping[str, str]) -> dict:
    """row-run-evidence.json: this shard's identity and settings, and per decision phase it measured, each side's
    accepted execution records and disclosed rejected attempts."""
    phases = []
    for result in results:
        if result.set_name != "counters":
            # When: the set is not a counters set, it measured no row-run counters.
            continue
        for label, phase_name in ROW_RUN_DECISION_PHASES:
            if label == result.label:
                phases.append({"label": label, "phase": phase_name,
                               **{side: row_run_export_side(result, side, phase_name, out) for side in SIDES}})
    return {"schema_version": ROW_RUN_EVIDENCE_SCHEMA, "repository": environ.get("GITHUB_REPOSITORY", ""),
            "run_id": environ.get("GITHUB_RUN_ID", ""), "run_attempt": environ.get("GITHUB_RUN_ATTEMPT", ""),
            "job": environ.get("PERF_JOB_NAME", ""), "shard": environ.get("PERF_SHARD", ""),
            "platform": row_run_platform(), "base_sha": shas["base"], "head_sha": shas["head"],
            "harness_hash": harness_hash, "dataset": "counters" if counters else "none", "short": bool(short),
            "features": {side: sorted(features[side]) for side in SIDES}, "profile": dict(profile),
            "protocol_version": ROW_RUN_PROTOCOL_VERSION, "protocol_digest": row_run_protocol_digest(),
            "phases": phases}


class RowRunEvidenceError(Exception):
    """The supplied evidence cannot be bound to the selected workflow execution: a validation error, never a
    decision."""


@dataclass
class RowRunExecution:
    """One validated workflow execution: its identity, evidence map, settings per platform and missing shards."""

    identity: str
    evidence: dict
    settings: dict
    missing: list
    claimed: set


def _critical_path_module():
    """perf-critical-path.py, whose row resolution maps an inherited job to its one executed origin."""
    spec = importlib.util.spec_from_file_location("perf_critical_path",
                                                  Path(__file__).resolve().parent / "perf-critical-path.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _row_run_timing_run(directory: Path) -> str:
    """The workflow run a directory's timing.json names, or '' when it names none."""
    timing, _problem = _read_json_object(directory / TIMING_FILE)
    return str(timing.get("run_id")) if isinstance(timing, dict) else ""


def _row_run_check_record(directory: Path, identity: object, side_name: str, label: str, phase_name: str,
                          renderer: Mapping) -> None:
    """An accepted record must name an attempt directory whose outcome.json is this valid run and whose kept
    result.json reported exactly these renderer fields for the phase."""
    if not isinstance(identity, str) or not identity or Path(identity).is_absolute() or ".." in Path(identity).parts:
        raise RowRunEvidenceError(f"{directory.name}: execution identity {identity!r} is not a directory under it")
    run_dir = directory / identity
    outcome, problem = _read_json_object(run_dir / "outcome.json")
    scenario, _, variant = label.partition("/")
    if problem is not None or not isinstance(outcome, dict):
        raise RowRunEvidenceError(f"{directory.name}/{identity}: {problem or 'outcome.json is not an object'}")
    if (outcome.get("kind"), outcome.get("side"), outcome.get("scenario"), outcome.get("variant")) != \
            ("valid", side_name, scenario, variant):
        raise RowRunEvidenceError(f"{directory.name}/{identity}: outcome.json is not an accepted {side_name} run "
                                  f"of {label}")
    result, problem = _read_json_object(run_dir / "scratch" / "result.json")
    reported = [phase.get("frame_counters", {}).get("renderer") for phase in (result or {}).get("phases") or []
                if isinstance(phase, dict) and str(phase.get("name")) == phase_name
                and isinstance(phase.get("frame_counters"), dict)] if isinstance(result, dict) else []
    if problem is not None or reported[:1] != [renderer]:
        raise RowRunEvidenceError(f"{directory.name}/{identity}: result.json does not report the record's "
                                  f"{phase_name} renderer fields ({problem or 'they differ'})")


def _row_run_bind_phase(directory: Path, platform_key: str, entry: Mapping) -> tuple[tuple, RowRunPhase]:
    """Validate one phase entry of a shard's evidence and rebuild its RowRunPhase from the accepted records."""
    key = (platform_key, entry.get("label"), entry.get("phase"))
    if key[1:] not in ROW_RUN_DECISION_PHASES:
        raise RowRunEvidenceError(f"{directory.name}: {key[1]} {key[2]} is not a decision phase")
    sides = []
    for side_name in SIDES:
        side = entry.get(side_name)
        if not isinstance(side, dict) or not isinstance(side.get("accepted"), list):
            raise RowRunEvidenceError(f"{directory.name}: {key[1]} {side_name} has no accepted records")
        records = side["accepted"]
        identities = [record.get("execution") if isinstance(record, dict) else None for record in records]
        if len(set(identities)) != len(identities):
            raise RowRunEvidenceError(f"{directory.name}: {key[1]} {side_name} lists one accepted execution twice")
        rejected = {item.get("execution") for item in side.get("rejected") or [] if isinstance(item, dict)}
        if rejected & set(identities):
            raise RowRunEvidenceError(f"{directory.name}: {key[1]} {side_name} execution both accepted and rejected")
        renderers = []
        for identity, record in zip(identities, records):
            renderer = record.get("renderer")
            if not isinstance(renderer, dict):
                raise RowRunEvidenceError(f"{directory.name}: {key[1]} {side_name} {identity}: no renderer fields")
            _row_run_check_record(directory, identity, side_name, key[1], key[2], renderer)
            renderers.append(renderer)
        if side.get("audit") != row_run_audit(renderers):
            raise RowRunEvidenceError(f"{directory.name}: {key[1]} {side_name} sums or counts disagree with its "
                                      "individual records")
        sides.append(tuple(renderers))
    return key, RowRunPhase(sides[0], sides[1])


def _row_run_bind_artifact(directory: Path, row_name: str, origin: int, run_id: str, repository: str,
                           head_sha: str, base_sha: str) -> tuple[str, dict, list]:
    """Check one claimed artifact's name, timing.json and evidence identity; return its platform key, settings
    and phase entries."""
    named = ROW_RUN_ARTIFACT.fullmatch(directory.name)
    job = ROW_RUN_COMPARISON_JOB.fullmatch(row_name)
    if named is None or (named["head"], named["platform"], named["shard"], named["attempt"]) != \
            (head_sha, job["platform"], job["shard"], str(origin)):
        raise RowRunEvidenceError(f"{directory.name}: the artifact name does not match {row_name} attempt {origin}")
    timing, problem = _read_json_object(directory / TIMING_FILE)
    evidence, evidence_problem = _read_json_object(directory / ROW_RUN_EVIDENCE_FILE)
    if problem or evidence_problem or not isinstance(timing, dict) or not isinstance(evidence, dict):
        raise RowRunEvidenceError(f"{directory.name}: {problem or evidence_problem or 'not a JSON object'}")
    expected = {"run_id": run_id, "run_attempt": str(origin), "job": row_name, "shard": job["shard"]}
    for document_name, document in (("timing.json", timing), (ROW_RUN_EVIDENCE_FILE, evidence)):
        for field_name, value in expected.items():
            if str(document.get(field_name)) != value:
                raise RowRunEvidenceError(f"{directory.name}: {document_name} {field_name} "
                                          f"{document.get(field_name)!r} is not {value!r}")
    platform_key = ROW_RUN_PLATFORM_KEYS[job["platform"]]
    identity = {"schema_version": ROW_RUN_EVIDENCE_SCHEMA, "repository": repository, "platform": platform_key,
                "head_sha": head_sha, "base_sha": base_sha, "protocol_version": ROW_RUN_PROTOCOL_VERSION,
                "protocol_digest": row_run_protocol_digest(), "dataset": "counters"}
    for field_name, value in identity.items():
        if evidence.get(field_name) != value:
            raise RowRunEvidenceError(f"{directory.name}: evidence {field_name} {evidence.get(field_name)!r} "
                                      f"is not {value!r}")
    lto = (evidence.get("profile") or {}).get("lto") or {}
    if lto.get("base") != lto.get("head"):
        raise RowRunEvidenceError(f"{directory.name}: base and head built with different profiles {lto}")
    settings = {name: evidence.get(name) for name in ("harness_hash", "short", "features", "profile")}
    if not isinstance(evidence.get("phases"), list):
        raise RowRunEvidenceError(f"{directory.name}: evidence has no phase list")
    return platform_key, settings, evidence["phases"]


def row_run_load_execution(record: Mapping, attempt: int, directories: Sequence[Path], head_sha: str,
                           base_sha: str, repository: str) -> RowRunExecution:
    """Bind supplied artifact directories to one workflow execution (run and attempt) and rebuild its evidence.

    The run must be the reviewed head; every job of the attempt must have succeeded, the result job included;
    creation to the attempt's final update must fit the budget. Each comparison job, executed or inherited from
    its validated origin, claims the one artifact of its origin attempt; that artifact's name, timing.json and
    evidence must agree with it. A shard whose artifact was not supplied is missing coverage. Each decision
    phase may be owned once per platform; its records are rebuilt into RowRunPhase, never summed across owners."""
    critical = _critical_path_module()
    run = record.get("run") or {}
    run_id = str(run.get("id"))
    if run.get("head_sha") != head_sha:
        raise RowRunEvidenceError(f"run {run_id} measured head {run.get('head_sha')}, expected {head_sha}")
    if (run.get("repository") or {}).get("full_name") != repository:
        raise RowRunEvidenceError(f"run {run_id} belongs to {(run.get('repository') or {}).get('full_name')}")
    try:
        rows = critical.resolve_rows(record)
        elapsed_s = (critical.parse_time(record["attempts"][str(attempt)]["updated_at"])
                     - critical.parse_time(run["created_at"]))
    except critical.AccountingError as error:
        raise RowRunEvidenceError(f"run {run_id}: {error}") from error
    identity = f"run {run_id} attempt {attempt}"
    if attempt not in rows:
        raise RowRunEvidenceError(f"{identity} is not an attempt of the run")
    if elapsed_s > ROW_RUN_MAX_ELAPSED_S:
        raise RowRunEvidenceError(f"{identity} took {elapsed_s} s from creation, over {ROW_RUN_MAX_ELAPSED_S} s")
    if not any(row.name == ROW_RUN_RESULT_JOB for row in rows[attempt]):
        raise RowRunEvidenceError(f"{identity} has no {ROW_RUN_RESULT_JOB!r} job")
    for row in rows[attempt]:
        if row.data.get("conclusion") != "success":
            raise RowRunEvidenceError(f"{identity}: required job {row.name!r} is {row.data.get('conclusion')}")
    evidence, settings, missing, claimed = {}, {}, [], set()
    for row in rows[attempt]:
        if ROW_RUN_COMPARISON_JOB.fullmatch(row.name) is None:
            # When: the job is the producer or the result job, it owns no comparison artifact.
            continue
        job = ROW_RUN_COMPARISON_JOB.fullmatch(row.name)
        origin = row.origin.attempt
        suffix = f"-{head_sha}-{job['platform']}-{job['shard']}-{origin}"
        listed = [item["name"] for item in record.get("artifacts") or []
                  if item["name"].startswith("perf-comparison-") and item["name"].endswith(suffix)]
        if len(listed) != 1:
            raise RowRunEvidenceError(f"{identity}: {len(listed)} listed artifacts end {suffix}")
        candidates = [directory for directory in directories
                      if directory.name == listed[0] and _row_run_timing_run(directory) == run_id]
        if len(candidates) > 1:
            raise RowRunEvidenceError(f"{identity}: duplicate artifact {listed[0]}")
        if not candidates:
            missing.append(listed[0])
            continue
        claimed.add(candidates[0])
        platform_key, artifact_settings, phases = _row_run_bind_artifact(
            candidates[0], row.name, origin, run_id, repository, head_sha, base_sha)
        if settings.setdefault(platform_key, artifact_settings) != artifact_settings:
            raise RowRunEvidenceError(f"{identity}: {candidates[0].name} settings differ from another "
                                      f"{platform_key} shard's")
        for entry in phases:
            key, phase = _row_run_bind_phase(candidates[0], platform_key, entry)
            if key in evidence:
                raise RowRunEvidenceError(f"{identity}: {' '.join(key)} is owned by two shards")
            evidence[key] = phase
    return RowRunExecution(identity, evidence, settings, missing, claimed)


def row_run_step_lines(decision: RowRunDecision) -> list[str]:
    """Each decision step's status: passed before the deciding step, its outcome at it, not evaluated after it."""
    lines = []
    for number, name in enumerate(("evidence", "classification", "overhead", "opportunity"), 1):
        if number < decision.step:
            status = "passed"
        elif number == decision.step:
            status = f"{decision.outcome}: {decision.reason}"
        else:
            status = "not evaluated"
        lines.append(f"- step {number} ({name}): {status}")
    gate = "passed" if decision.step > 3 else "failed" if decision.step == 3 else "not evaluated"
    lines.append(f"- overhead gate (manual merge gate): {gate}")
    return lines


def row_run_decide(executions: Sequence[tuple[Mapping, int]], directories: Sequence[Path], head_sha: str,
                   base_sha: str, restarts: int, repository: str) -> str:
    """Validate one eligible execution and its optional same-head replacement, call row_run_final once on their
    separately rebuilt evidence maps, and return the report: both identities, the selected execution, every
    step's status and reason, the overhead gate and the outcome."""
    if len(set(directories)) != len(directories):
        raise RowRunEvidenceError("duplicate artifact: one directory was supplied twice")
    loaded = [row_run_load_execution(record, attempt, directories, head_sha, base_sha, repository)
              for record, attempt in executions]
    unclaimed = sorted(str(directory) for directory in directories
                       if not any(directory in execution.claimed for execution in loaded))
    if unclaimed:
        raise RowRunEvidenceError(f"artifacts belong to no selected execution: {', '.join(unclaimed)}")
    first, rerun = loaded[0], (loaded[1] if len(loaded) > 1 else None)
    if rerun is not None:
        if rerun.identity == first.identity:
            raise RowRunEvidenceError(f"the replacement is the first execution again: {first.identity}")
        for platform_key in set(first.settings) & set(rerun.settings):
            if first.settings[platform_key] != rerun.settings[platform_key]:
                raise RowRunEvidenceError(f"{platform_key} settings differ between {first.identity} and "
                                          f"{rerun.identity}")
    final = row_run_final(first.evidence, first.identity, None if rerun is None else rerun.evidence,
                          "" if rerun is None else rerun.identity, restarts)
    first_decision = row_run_decision(first.evidence)
    selected = first if rerun is None or first_decision.step == 4 else rerun
    lines = ["## Row-run shaping decision", "",
             f"- protocol: version {ROW_RUN_PROTOCOL_VERSION}, digest `{row_run_protocol_digest()}`",
             f"- head `{head_sha}`, base `{base_sha}`, restarts {restarts}",
             f"- first execution: {first.identity}",
             f"- replacement execution: {'none' if rerun is None else rerun.identity}",
             f"- selected execution: {selected.identity}"]
    lines += [f"- missing shard artifact of {selected.identity}: {name}" for name in selected.missing]
    lines += row_run_step_lines(row_run_decision(selected.evidence))
    lines.append(f"- outcome: {final.outcome} (step {final.step}): {final.reason}")
    lines.append("- exit status 0 means the evidence validated, not that the diagnostic may merge: read the "
                 "outcome and the overhead gate.")
    return "\n".join(lines) + "\n"


def row_run_decide_main(args: argparse.Namespace) -> int:
    """Run --row-run-decide: print the decision and return 0, or print the validation error and return 2."""
    try:
        executions = [(json.loads(Path(path).read_text(encoding="utf-8")), int(attempt))
                      for path, attempt in args.row_run_execution]
        report = row_run_decide(executions, [Path(directory) for directory in args.row_run_decide],
                                args.row_run_head, args.row_run_base, args.row_run_restarts, args.row_run_repo)
    except (RowRunEvidenceError, OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        print(f"row-run evidence is invalid, so no decision is made: {error}")
        return EXIT_USAGE
    print(report, end="")
    return 0


def render_table(rows: Iterable[Sequence[str]], header: str = TABLE_HEADER) -> str:
    """Render rows as a Markdown table under `header`; a `|` or newline in a cell cannot break it."""
    def cell(text: str) -> str:
        return str(text).replace("|", "\\|").replace("\n", " ")
    return header + "".join("| " + " | ".join(cell(text) for text in row) + " |\n" for row in rows)


# --- The smoke ------------------------------------------------------------------------------

@dataclass(frozen=True)
class SmokeCase:
    """One smoke case: a scenario variant, whether it is ended at GO, and the kind it must end as."""

    scenario: str
    variant: str = "default"
    kill_at_go: bool = False
    expected: str = "valid"

    @property
    def name(self) -> str:
        """The case's name in the log: `S1`, `S1/wgpu` or `S1-deadline`."""
        return (self.scenario + ("" if self.variant == "default" else f"/{self.variant}")
                + ("-deadline" if self.kill_at_go else ""))


# S1 and S3, then S1 ended exactly as a step deadline ends a run.
SMOKE_CASES = (SmokeCase("S1"), SmokeCase("S3"), SmokeCase("S1", kill_at_go=True))
# Windows adds the wgpu presenter, which must not be degraded, and a role program that exits right
# after GO, which must end the run invalid with a reason naming its pane.
WINDOWS_SMOKE_CASES = SMOKE_CASES + (SmokeCase("S1", "wgpu"), SmokeCase("S1", "role-exit", expected="invalid"))
# The word an invalid role-exit run's reason must hold, naming the pane whose program exited.
PANE_EXIT_WORD = "pane"


# The Windows smoke replays this S10 variant's delivery: the synchronized frames whose brackets it classifies.
SMOKE_REPLAY_VARIANT = "sync"


def smoke_case_list(platform_name: str) -> tuple[SmokeCase, ...]:
    """The smoke's cases on this host."""
    return WINDOWS_SMOKE_CASES if platform_name == "win32" else SMOKE_CASES


def case_verdict(case: SmokeCase, kind: str, reasons: Sequence[str]) -> tuple[str, list[str]]:
    """The smoke's verdict for one attempt of `case`, with the reasons it reports.

    A case that expects `invalid` passes only as `invalid` with a reason naming the pane; a valid end fails.
    """
    if case.expected == "invalid" and kind == "invalid":
        if any(PANE_EXIT_WORD in reason for reason in reasons):
            return "pass", list(reasons)
        return "fail", list(reasons) + ["the run was invalid, but no reason names the pane whose program exited"]
    if case.expected == "invalid" and kind == "valid":
        return "fail", ["the role program exited, yet the run ended valid"]
    return smoke_verdict(kind), list(reasons)
EVIDENCE_PREFIX = "sonicterm-perf-evidence-"


def smoke_cases(scenarios: Mapping[str, Scenario], binary: Path, harness_hash: str,
                run_case: Callable[[RunPlan, Path], RunOutcome], evidence: Path,
                host_platform: str | None = None,
                cases: Sequence[SmokeCase] | None = None,
                replay: Callable[[Scenario, str, Path], DeliveryOutcome] | None = None,
                ) -> tuple[int, list[str]]:
    """Run the smoke's cases: exit 1 at once on a failure, 3 when a case has no valid exercised run, else 0.

    Each case's variant must be one the harness lists. Only an occlusion is retried, at most
    RETRY_LIMIT times; no timing is asserted. On Windows each attempt also prints its job's members,
    since a passing smoke deletes the evidence that holds them. With `replay` on Windows, S10/sync's
    delivery is replayed first, and a blocked replay makes the smoke exit 3.
    """
    host_platform = host_platform or sys.platform
    cases = smoke_case_list(host_platform) if cases is None else cases
    missing = [case.name for case in cases
               if case.scenario not in scenarios or case.variant not in scenarios[case.scenario].variants]
    replays = replay is not None and host_platform == "win32"
    if replays and ("S10" not in scenarios or SMOKE_REPLAY_VARIANT not in scenarios["S10"].variants):
        missing.append(f"S10/{SMOKE_REPLAY_VARIANT}")
    if missing:
        return EXIT_FAIL, [f"the harness does not list the variant of {', '.join(missing)}"]
    blocked = []
    if replays:
        try:
            _record, problem, note = replay(scenarios["S10"], SMOKE_REPLAY_VARIANT, evidence)
        except StopComparison as error:
            # When: the replay's teardown is unproven, its processes could disturb every case after it.
            return EXIT_FAIL, [str(error)]
        disclosed = f" ({note})" if note else ""
        print(f"[perf-smoke] S10/{SMOKE_REPLAY_VARIANT} delivery: " + (problem or "every check passed") + disclosed,
              flush=True)
        if problem is not None:
            # When: the replay could not show how ConPTY delivered the frames, the smoke did not exercise it.
            blocked.append(f"S10/{SMOKE_REPLAY_VARIANT} delivery: not exercised: {problem}"
                           + (f"; {note}" if note else ""))
    for case in cases:
        name = case.name
        plan = RunPlan(scenarios[case.scenario], case.variant, "smoke", binary, harness_hash, short=True,
                       smoke=True, kill_at_go=case.kill_at_go, source_root=ROOT,
                       latency_split_schema=scenarios[case.scenario].latency_split_schema)
        reasons: list[str] = []
        for attempt in range(1, RETRY_LIMIT + 2):
            # A variant's `/` would make a subdirectory, so evidence names use `-`.
            outcome = run_case(plan, evidence / f"{name.replace('/', '-')}-{attempt}")
            kind, reasons = classify_outcome(outcome)
            verdict, reasons = case_verdict(case, kind, reasons)
            print(f"[perf-smoke] {name} attempt {attempt}: {kind}" + (f": {'; '.join(reasons)}" if reasons else ""),
                  flush=True)
            if host_platform == "win32":
                print(f"[perf-smoke] {name} attempt {attempt} {custody_member_text(outcome.custody)}", flush=True)
            if verdict == "fail":
                return EXIT_FAIL, [f"{name}: {kind}: {reason}" for reason in reasons] or [f"{name}: {kind}"]
            if verdict in ("pass", "blocked"):
                if verdict == "blocked":
                    blocked.append(f"{name}: not exercised: {'; '.join(reasons)}")
                break
        else:
            blocked.append(f"{name}: no valid run after {RETRY_LIMIT} retries: {'; '.join(reasons)}")
    return (EXIT_BLOCKED, blocked) if blocked else (EXIT_PASS, [])


def list_scenarios(gate, binary: Path, log_dir: Path, index: int) -> list[Scenario]:
    """Run a harness binary's `--list` through run_step and parse the scenario set."""
    step = gate.Step("list-scenarios", (str(binary), "--list"), gate_hosts(sys.platform), LIST_TIMEOUT_S, "local",
                     (), ())
    result = gate.run_step(step, index, log_dir, log_dir, harness_environment(os.environ))
    text = read_log(result.log_path)
    if result.status != "PASS":
        raise ValueError(f"`{binary} --list` {result.status}, exit {result.exit_code}:\n{log_tail(text)}")
    return parse_scenario_list(text)


def production_host(gate) -> Host:
    """The real collaborators: this host's process table, bounded lsappinfo and the user's SonicTerm home."""
    runner = gate.SMOKE_RUNNER.run_command
    excluded = (os.getsid(0),) if hasattr(os, "getsid") else ()
    return Host(gate, make_process_table(), lambda argv, timeout_s: bounded_command(runner, argv, timeout_s),
                sonicterm_home(os.environ), Path(tempfile.gettempdir()), set(), dict(os.environ),
                excluded_sids=excluded, platform=sys.platform,
                front_sample=sample_foreground if sys.platform == "win32" else None)


def run_smoke(evidence: Path) -> tuple[int, list[str]]:
    """Build the current tree's debug harness (no worktree), list its scenarios and run the smoke's cases."""
    gate = load_gate()
    problem = gate_problem(gate)
    if problem:
        return EXIT_FAIL, [problem]
    # The gate's own compile-only step, so a compiler helper it cleaned does not fail the build.
    step = gate.PERF_BUILDS["build-perf_scenarios"]
    build = gate.run_step(step, 1, ROOT, evidence, dict(os.environ))
    build_log = read_log(build.log_path)
    binary = artifact_executable(build_log, HARNESS_EXAMPLE)
    if not build_passed(build) or binary is None:
        return EXIT_FAIL, [f"debug build {build.status}, exit {build.exit_code}:\n{log_tail(build_log)}"]
    problem = asset_problem(binary, ROOT)
    if problem:
        return EXIT_FAIL, [f"the smoke runs from {ROOT}, but {problem}"]
    try:
        digest = tree_harness_hash(ROOT)
        scenarios = list_scenarios(gate, binary, evidence, 2)
    except (OSError, ValueError) as error:
        return EXIT_FAIL, [str(error)]
    print(f"[perf-smoke] harness_hash={digest} binary={binary}", flush=True)
    host = production_host(gate)
    print(f"[perf-smoke] {focus_rule_line(host.environ)}", flush=True)
    def replay(scenario: Scenario, variant: str, replay_evidence: Path) -> DeliveryOutcome:
        return run_delivery_replay(gate, binary, scenario.id, variant, replay_evidence, 3, short=True,
                                   timeout_s=run_timeout_s(scenario, True, True), temp_root=host.temp_root,
                                   environ=host.environ)
    return smoke_cases({scenario.id: scenario for scenario in scenarios}, binary, digest,
                       lambda plan, case_evidence: execute_run(plan, host, case_evidence), evidence,
                       replay=replay if sys.platform == "win32" else None)


def replay_retried(evidence: Path) -> bool:
    """Whether `evidence` holds a delivery replay attempt after the first, so a retry was made."""
    return any((matched := REPLAY_ATTEMPT_FILE.search(path.name)) and int(matched.group(1)) > 1
               for path in evidence.rglob("*") if path.is_file())


def smoke_main(environ: Mapping[str, str], runner: Callable[[Path], tuple[int, list[str]]] | None = None) -> int:
    """The perf smoke step: export the evidence directory, run the smoke, and keep the evidence on failure.

    A pass also keeps it when a delivery replay was retried, and exports REPLAY_RETRIED_ENV so CI uploads every
    attempt's record, text and log; only an unretried pass removes the evidence.
    """
    if sys.platform not in RUN_PLATFORMS:
        print("[perf-smoke] BLOCKED: the harness runs only on macOS and Windows", flush=True)
        return EXIT_BLOCKED
    evidence = Path(tempfile.mkdtemp(prefix=EVIDENCE_PREFIX))
    github_env = environ.get("GITHUB_ENV")
    if github_env:
        # CI uploads this directory when the job fails.
        with Path(github_env).open("a", encoding="utf-8") as stream:
            stream.write(f"SONICTERM_PERF_EVIDENCE_DIR={evidence}\n")
    print(f"[perf-smoke] evidence={evidence}", flush=True)
    code, reasons = (runner or run_smoke)(evidence)
    for reason in reasons:
        print(f"[perf-smoke] {reason}", file=sys.stderr, flush=True)
    if code == EXIT_PASS and replay_retried(evidence):
        if github_env:
            # A green step uploads nothing by default; this tells the upload step a retry is worth keeping.
            with Path(github_env).open("a", encoding="utf-8") as stream:
                stream.write(f"{REPLAY_RETRIED_ENV}=1\n")
        print(f"[perf-smoke] verdict=PASS; a delivery replay was retried, so evidence is kept at {evidence}",
              flush=True)
    elif code == EXIT_PASS:
        shutil.rmtree(evidence, ignore_errors=True)
        print(f"[perf-smoke] verdict=PASS; evidence {evidence} removed", flush=True)
    else:
        verdict = "BLOCKED" if code == EXIT_BLOCKED else "FAIL"
        print(f"[perf-smoke] verdict={verdict}; evidence kept at {evidence}", flush=True)
    return code


# --- Comparison: run sets, worktrees, host block and document --------------------------------

# The harness reports these fields of the measurement display; a null field is one it could not read.
DISPLAY_FIELDS = ("name", "refresh_rate_millihertz", "scale_factor")


def display_of(result: Mapping | None) -> dict | None:
    """Return a result's measurement display, or None when it reported none; unreported fields stay null."""
    monitor = (result or {}).get("monitor")
    return monitor if _monitor_ok(monitor) else None


def describe_display(monitor: Mapping | None) -> str:
    """Name a display as `<name>, <Hz> Hz, scale <s>`; no display reads `unknown`, and so does an unreported rate."""
    if monitor is None:
        return "unknown"
    rate = monitor.get("refresh_rate_millihertz")
    rate_text = "unknown" if rate is None else f"{rate / 1000:g}"
    return f"{monitor.get('name') or 'unnamed display'}, {rate_text} Hz, scale {monitor['scale_factor']:g}"


def display_differences(first: Mapping, other: Mapping) -> list[str]:
    """List the display fields both runs reported that differ; a field either run left null is not compared."""
    return [key for key in DISPLAY_FIELDS
            if first.get(key) is not None and other.get(key) is not None and first[key] != other[key]]


@dataclass
class DisplayReference:
    """The display every valid run of a comparison must share.

    The first valid run that reported a display sets it; a later passing run fills in any field it
    lacked, so a rate first learned later still binds every run after it.
    """

    monitor: dict | None = None

    def learn(self, measured: Mapping | None) -> None:
        """Adopt a passing run's display, filling in only the fields the reference did not know yet."""
        if measured is None:
            return
        if self.monitor is None:
            self.monitor = dict(measured)
            return
        for key in DISPLAY_FIELDS:
            if self.monitor.get(key) is None and measured.get(key) is not None:
                self.monitor[key] = measured[key]


class StopComparison(Exception):
    """A schema failure, a refusal or an unresolved cleanup: the comparison stops with exit 1 and the reason."""


@dataclass
class SetResult:
    """One run set (timed, laps or alloc) of one scenario: each side's valid runs and every attempt."""

    label: str
    set_name: str
    base: SideRuns
    head: SideRuns
    attempts: list = field(default_factory=list)
    # The valid runs each side needed; a set that never ran (a blocked delivery) keeps 0 and is blocked instead.
    target_runs: int = 0
    # The runs asked for before a short-mode cap; above target_runs only for a capped variant.
    requested_runs: int = 0


def grid_size(grid: object) -> tuple[int, int] | None:
    """A result's grid as (columns, rows); the harness writes `{cols, rows}`. Anything else is unknown."""
    if isinstance(grid, dict):
        columns = grid.get("cols", grid.get("columns"))
        if _is_int(columns) and _is_int(grid.get("rows")):
            return columns, grid["rows"]
    return None


def grid_text(size: tuple[int, int] | None) -> str:
    """Name a grid as `<columns>x<rows>`, or `unknown`."""
    return "unknown" if size is None else f"{size[0]}x{size[1]}"


def renderer_identity(renderer: Mapping | None) -> dict | None:
    """The adapter fields a pair must share; whether the adapter was selected or reused is not identity."""
    return None if renderer is None else {key: value for key, value in renderer.items() if key != "event"}


def run_set(label: str, plans: Mapping[str, RunPlan], base_blocked: str | None, runs: int,
            run_case: Callable[[RunPlan, Path], RunOutcome], evidence: Path, set_name: str = "timed",
            display: DisplayReference | None = None) -> SetResult:
    """Run one set A B B A until each side has `runs` valid runs.

    An invalid run is retried, at most RETRY_LIMIT times per side. A grid that differs from
    the first valid run's makes the pair invalid, and so does a display that differs from
    `display`, the comparison's reference, in any field both reported: name, refresh rate or
    scale. Only a field a run did not report goes unchecked. On Windows, where the window opens at
    whatever grid its display allows, an adapter or presenter that differs from the first valid
    run's makes the pair invalid too. A base that cannot build or run is
    `blocked` and the head still runs; a head that cannot is blocked, and one that exhausts
    its retries fails. A schema failure, a refusal or an unresolved cleanup stops the comparison.
    """
    sides = {side: SideRuns() for side in SIDES}
    result = SetResult(label, set_name, sides["base"], sides["head"], target_runs=runs)
    reasons: dict[str, list[str]] = {side: [] for side in SIDES}
    schedule = AbbaSchedule(runs)
    if base_blocked:
        sides["base"].blocked = base_blocked
        schedule.retire("base")
    reference_grid = None
    # The first valid run's adapter and presenter; every later run of the set must match them.
    reference_renderer = reference_presenter = None
    if display is None:
        display = DisplayReference()
    attempt = 0
    while True:
        side = schedule.next_side()
        if side is None:
            return result
        attempt += 1
        run_evidence = evidence / f"{attempt:02d}-{side}"
        outcome = run_case(plans[side], run_evidence)
        kind, why = classify_outcome(outcome)
        if kind == "valid":
            grid = (outcome.result or {}).get("grid")
            measured = display_of(outcome.result)
            renderer = renderer_identity(outcome.renderer)
            presenter = (outcome.result or {}).get("presenter")
            if reference_grid is not None and grid != reference_grid:
                kind, why = "grid", [f"grid {grid} differs from the pair's {reference_grid}"]
            elif (measured is not None and display.monitor is not None
                  and display_differences(display.monitor, measured)):
                fields = ", ".join(display_differences(display.monitor, measured))
                kind, why = "display", [f"display {describe_display(measured)} differs from the comparison's "
                                        f"{describe_display(display.monitor)} in {fields}"]
            elif renderer is not None and reference_renderer is not None and renderer != reference_renderer:
                kind, why = "renderer", [f"renderer {describe_renderer(renderer)} differs from the pair's "
                                         f"{describe_renderer(reference_renderer)}"]
            elif presenter is not None and reference_presenter is not None and presenter != reference_presenter:
                kind, why = "presenter", [f"presenter {presenter} differs from the pair's {reference_presenter}"]
            else:
                # Only a run that passed every check sets the grid, adapter and presenter or teaches the display.
                if reference_grid is None:
                    reference_grid = grid
                if reference_renderer is None:
                    reference_renderer = renderer
                if reference_presenter is None:
                    reference_presenter = presenter
                display.learn(measured)
        result.attempts.append((side, str(run_evidence), kind, why))
        print(f"[perf-compare] {label} {set_name} {side} run {attempt}: {kind}"
              + (f": {'; '.join(why)}" if why else ""), flush=True)
        verdict = compare_verdict(kind)
        if verdict == "stop":
            raise StopComparison(f"{label} {set_name} {side}: {kind}: {'; '.join(why)}")
        if verdict == "blocked":
            sides[side].blocked = "; ".join(why) or kind
            sides[side].outcomes = []
            schedule.retire(side)
            if side == "head":
                return result
            continue
        if verdict == "valid":
            sides[side].outcomes.append(outcome)
        else:
            reasons[side].append(f"{kind}: {'; '.join(why)}")
        schedule.record(side, verdict == "valid")
        failed = schedule.failed_side
        if failed is not None:
            summary = f"no {runs} valid runs after {RETRY_LIMIT} retries: " + " | ".join(reasons[failed])
            sides[failed].outcomes = []
            if failed == "head":
                sides[failed].failed = summary
                return result
            sides[failed].blocked = summary
            schedule.retire(failed)


def strict_problems(results: Iterable[SetResult]) -> list[str]:
    """Name every side of every set without exactly its valid runs: blocked, failed, or not `target_runs`.

    An empty result set and a set without a positive target are problems too.

    The one gap allowed is a counters set whose base has no perf-counters (COUNTERS_HEAD_ONLY), which
    the table reports as n/a. `--require-base` turns any other problem into a failed comparison.
    """
    results = list(results)
    # When: no set ran, nothing was measured, so there is no comparison to pass.
    if not results:
        return ["no scenario set ran"]
    problems = []
    for result in results:
        for side_name, side in (("base", result.base), ("head", result.head)):
            where = f"{result.label} {result.set_name} {side_name}"
            # When: the base has no counters feature, its counters cells are n/a by design, not missing runs.
            if side_name == "base" and result.set_name == "counters" and side.blocked == COUNTERS_HEAD_ONLY:
                continue
            if side.blocked:
                problems.append(f"{where}: blocked: {side.blocked}")
            elif side.failed:
                problems.append(f"{where}: failed: {side.failed}")
            elif result.target_runs < 1:
                problems.append(f"{where}: target {result.target_runs} is not positive")
            elif len(side.outcomes) != result.target_runs:
                # Counts are exact: more valid runs than planned is not the comparison the table describes.
                problems.append(f"{where}: {len(side.outcomes)} of {result.target_runs} valid runs")
    return problems


def capped_label(result: SetResult) -> str:
    """A set's row label: `<label> (runs N of M)` when a short-mode cap lowered its runs, else the label."""
    if 0 < result.target_runs < result.requested_runs:
        return f"{result.label} (runs {result.target_runs} of {result.requested_runs})"
    return result.label


def comparison_exit(results: Iterable[SetResult], require_base: bool = False) -> int:
    """Exit 1 when a head set failed, 3 when the head was blocked, else 0; a blocked base is reported, not failed.

    With `require_base`, any strict problem (either side blocked, failed or short) exits 1 instead.
    """
    results = list(results)
    if require_base and strict_problems(results):
        return EXIT_FAIL
    if any(result.head.failed for result in results):
        return EXIT_FAIL
    if any(result.head.blocked for result in results):
        return EXIT_BLOCKED
    return EXIT_PASS


def work_directory(stamp: str) -> Path:
    """Return a comparison's directory for worktrees and target directories, under the ignored target/ tree."""
    return ROOT / "target" / "perf-compare" / f"work-{stamp}"


class Worktrees:
    """The detached worktrees this comparison created; only these are ever removed.

    `git worktree add` and `remove` change the repository's worktree list, never the main
    checkout's index, branch or working tree.
    """

    def __init__(self, git: Callable[[Sequence[str]], CommandRecord], directory: Path) -> None:
        self.git = git
        self.directory = directory
        self.created: list[Path] = []

    def create(self, side: str, sha: str) -> Path:
        """Add a detached worktree for one side at a path that does not exist yet; one per side, even for one SHA."""
        path = self.directory / side
        if os.path.lexists(path):
            raise ValueError(f"worktree path {path} already exists; a comparison never reuses one")
        self.directory.mkdir(parents=True, exist_ok=True)
        record = self.git(("git", "worktree", "add", "--detach", str(path), sha))
        failure = _command_failure(record)
        if failure:
            raise ValueError(f"{failure}: {record.stderr.strip() or record.stdout.strip()}")
        self.created.append(path)
        return path

    def remove(self) -> list[str]:
        """Force-remove each created worktree, newest first, since the overlay modified them; return failures."""
        problems = []
        for path in reversed(self.created):
            record = self.git(("git", "worktree", "remove", "--force", str(path)))
            failure = _command_failure(record)
            if failure:
                problems.append(f"{failure}: {record.stderr.strip()}")
        self.created = []
        return problems


def build_error(text: str) -> str:
    """Name a failed build by its first compiler error, or by its last lines when there is none."""
    errors = [line.strip() for line in text.splitlines() if line.startswith("error")]
    return errors[0] if errors else log_tail(text, 3)


HOST_COMMANDS = {
    "model": ("sysctl", "-n", "hw.model"),
    "cpu": ("sysctl", "-n", "machdep.cpu.brand_string"),
    "memory": ("sysctl", "-n", "hw.memsize"),
    "os": ("sw_vers",),
    "displays": ("system_profiler", "SPDisplaysDataType"),
    "power": ("pmset", "-g", "batt"),
    "power_settings": ("pmset", "-g"),
}
HOST_COMMAND_TIMEOUT_S = 60
UNAVAILABLE = "unavailable"


def _first_line(text: str | None) -> str:
    lines = [line.strip() for line in (text or "").splitlines() if line.strip()]
    return lines[0] if lines else UNAVAILABLE


def _profile_values(text: str, key: str) -> list[str]:
    """Return every `key: value` value of a system_profiler report, in order."""
    prefix = key + ":"
    return [line.strip()[len(prefix):].strip() for line in text.splitlines() if line.strip().startswith(prefix)]


def host_block(outputs: Mapping[str, str], monitor: Mapping | None = None) -> list[str]:
    """Render the host block from raw command outputs; a figure a command did not give reads `unavailable`.

    `monitor` is the display the valid runs shared, which reads `unknown` when none reported one.
    """
    memory = _first_line(outputs.get("memory"))
    memory_text = f"{int(memory) / 1024 ** 3:.0f} GiB" if memory.isdigit() else UNAVAILABLE
    os_fields = dict(re.findall(r"^[ \t]*(\w+):[ \t]*(.+?)[ \t]*$", outputs.get("os") or "", re.M))
    os_text = (f"{os_fields.get('ProductName', 'macOS')} {os_fields['ProductVersion']} "
               f"({os_fields.get('BuildVersion', '?')})" if "ProductVersion" in os_fields else UNAVAILABLE)
    displays = outputs.get("displays") or ""
    gpus = _profile_values(displays, "Chipset Model")
    looks = _profile_values(displays, "UI Looks like")
    display_lines = []
    for position, resolution in enumerate(_profile_values(displays, "Resolution")):
        text = resolution
        if position < len(looks):
            text += f", looks like {looks[position]}"
            native, logical = re.match(r"(\d+) x \d+", resolution), re.match(r"(\d+) x \d+", looks[position])
            if native and logical and int(logical[1]):
                text += f", scale {int(native[1]) / int(logical[1]):.2f}"
        display_lines.append(text)
    power = re.search(r"Now drawing from '([^']+)'", outputs.get("power") or "")
    low_power = re.search(r"^[ \t]*lowpowermode[ \t]+(\d+)", outputs.get("power_settings") or "", re.M)
    low_power_text = UNAVAILABLE if not low_power else ("off" if low_power[1] == "0" else "on")
    return [
        f"- Machine: {_first_line(outputs.get('model'))}, {_first_line(outputs.get('cpu'))}, {memory_text}",
        f"- OS: {os_text}",
        f"- GPU: {', '.join(gpus) if gpus else UNAVAILABLE}",
        f"- Displays (resolution, logical size and refresh, scale): "
        f"{'; '.join(display_lines) if display_lines else UNAVAILABLE}",
        f"- Measurement display: {describe_display(monitor)}",
        f"- Power: {power[1] if power else UNAVAILABLE}; Low Power Mode {low_power_text}",
    ]


POWERCFG_ARGV = ("powercfg", "/getactivescheme")
_CPU_KEY = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"
_BIOS_KEY = r"HARDWARE\DESCRIPTION\System\BIOS"
_OS_KEY = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"
# Each Windows host figure's key below HKEY_LOCAL_MACHINE and its value name.
WINDOWS_REGISTRY_VALUES = {
    "cpu": (_CPU_KEY, "ProcessorNameString"),
    "manufacturer": (_BIOS_KEY, "SystemManufacturer"),
    "product": (_BIOS_KEY, "SystemProductName"),
    "os_name": (_OS_KEY, "ProductName"),
    "os_version": (_OS_KEY, "DisplayVersion"),
    "os_build": (_OS_KEY, "CurrentBuild"),
    "os_ubr": (_OS_KEY, "UBR"),
}
# The active scheme's name is the parenthesized text at the end of powercfg's line.
_POWER_PLAN = re.compile(r"\(([^()]+)\)\s*$")


def _registry_value(key: str, name: str) -> object:
    """One HKEY_LOCAL_MACHINE value, read-only, or None when it cannot be read."""
    try:
        import winreg
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, key) as handle:
            return winreg.QueryValueEx(handle, name)[0]
    except (ImportError, OSError):
        return None


def _total_memory_bytes() -> int | None:
    """Physical memory from GlobalMemoryStatusEx, or None off Windows or when the call fails."""
    try:
        import ctypes
        from ctypes import wintypes

        class MemoryStatus(ctypes.Structure):
            """MEMORYSTATUSEX from <sysinfoapi.h>."""

            _fields_ = [("dwLength", wintypes.DWORD), ("dwMemoryLoad", wintypes.DWORD)] + [
                (name, ctypes.c_ulonglong) for name in ("ullTotalPhys", "ullAvailPhys", "ullTotalPageFile",
                                                        "ullAvailPageFile", "ullTotalVirtual", "ullAvailVirtual",
                                                        "ullAvailExtendedVirtual")]

        status = MemoryStatus()
        status.dwLength = ctypes.sizeof(status)
        if not ctypes.WinDLL("kernel32", use_last_error=True).GlobalMemoryStatusEx(ctypes.byref(status)):
            return None
        return int(status.ullTotalPhys)
    except (ImportError, OSError, AttributeError):
        return None


def _file_version(path: str) -> str | None:
    """A file's version resource as `a.b.c.d`, or None when it has none or cannot be read."""
    try:
        import ctypes
        from ctypes import wintypes
        version = ctypes.WinDLL("version", use_last_error=True)
        size = version.GetFileVersionInfoSizeW(path, None)
        if not size:
            return None
        buffer = ctypes.create_string_buffer(size)
        if not version.GetFileVersionInfoW(path, 0, size, buffer):
            return None
        pointer, length = ctypes.c_void_p(), wintypes.UINT()
        if not version.VerQueryValueW(buffer, "\\", ctypes.byref(pointer), ctypes.byref(length)):
            return None
        # VS_FIXEDFILEINFO: signature, structure version, then the file version's high and low halves.
        words = ctypes.cast(pointer, ctypes.POINTER(wintypes.DWORD * 13)).contents
        high, low = words[2], words[3]
        return f"{high >> 16}.{high & 0xFFFF}.{low >> 16}.{low & 0xFFFF}"
    except (ImportError, OSError, AttributeError):
        return None


def windows_host_outputs(host_run: Callable[[Sequence[str], int], CommandRecord], *,
                         registry: Callable[[str, str], object] | None = None,
                         memory: Callable[[], int | None] | None = None,
                         file_version: Callable[[str], str | None] | None = None) -> dict[str, str]:
    """Read the Windows host block's figures, read-only; a figure that could not be read is empty.

    The registry, memory status and file version are injectable, so the tests drive fakes.
    """
    registry = registry or _registry_value
    outputs = {}
    for name, (key, value_name) in WINDOWS_REGISTRY_VALUES.items():
        value = registry(key, value_name)
        outputs[name] = "" if value is None else str(value).strip()
    total = (memory or _total_memory_bytes)()
    outputs["memory_bytes"] = str(total) if _is_int(total) else ""
    record = host_run(POWERCFG_ARGV, HOST_COMMAND_TIMEOUT_S)
    outputs["power"] = "" if _command_failure(record) else record.stdout
    conhost = os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "System32", "conhost.exe")
    outputs["conhost"] = (file_version or _file_version)(conhost) or ""
    return outputs


def host_block_windows(outputs: Mapping[str, str], monitor: Mapping | None,
                       renderer: Mapping | None) -> list[str]:
    """Render the Windows host block; a figure that could not be read reads `unavailable`.

    `monitor` is the display the valid runs shared and `renderer` the adapter they drew through.
    """
    machine = " ".join(part for part in (outputs.get("manufacturer"), outputs.get("product")) if part)
    memory = outputs.get("memory_bytes") or ""
    memory_text = f"{int(memory) / 1024 ** 3:.0f} GiB" if memory.isdigit() else UNAVAILABLE
    os_text = UNAVAILABLE
    if outputs.get("os_name"):
        os_text = outputs["os_name"] + (f" {outputs['os_version']}" if outputs.get("os_version") else "")
        if outputs.get("os_build"):
            update = f".{outputs['os_ubr']}" if outputs.get("os_ubr") else ""
            os_text += f" (build {outputs['os_build']}{update})"
    plan = _POWER_PLAN.search((outputs.get("power") or "").strip())
    return [
        f"- Machine: {machine or UNAVAILABLE}, {outputs.get('cpu') or UNAVAILABLE}, {memory_text}",
        f"- OS: {os_text}",
        f"- GPU: {describe_renderer(renderer) if renderer is not None else UNAVAILABLE}",
        f"- Measurement display: {describe_display(monitor)}",
        f"- Power plan: {plan[1] if plan else UNAVAILABLE}",
        f"- Console host: conhost.exe {outputs.get('conhost') or UNAVAILABLE}",
    ]


RECOVERY_HEADER = "| Frame | Baseline | PR |\n|---|---|---|\n"
RECOVERY_NOTE = ("Each cell sums, over every episode of every accepted counters run, the row-cache misses and hits, "
                 "the shaping requests and the render attempts of that frame: A retries an injected atlas change, B "
                 "is the first recovered presentation, C and D are forced frames of the unchanged scene.")


def atlas_recovery_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """The atlas retry recovery table for S1/atlas-retry: one row per frame A-D and a sequence total, each
    cell summed over every episode of every accepted counters run, with the run and episode counts and each
    side's distinct row keys in the first row. Empty for any other label or when no run recorded episodes."""
    if tuple(label.split("/", 1)) not in COUNTERS_ONLY_VARIANTS:
        return []
    per_side = []
    for side in (base, head):
        recoveries = [outcome.result["atlas_recovery"] for outcome in side.outcomes
                      if isinstance((outcome.result or {}).get("atlas_recovery"), dict)]
        per_side.append(recoveries)
    if not any(per_side):
        return []

    def cell(recoveries: list, frames: Sequence[str]) -> str:
        if not recoveries:
            return "n/a"
        sums = {name: sum(record[name] for recovery in recoveries for record in recovery["records"]
                          if record["frame"] in frames)
                for name in ("misses", "hits", "shapes", "attempts")}
        return (f"misses {sums['misses']}, hits {sums['hits']}, shapes {sums['shapes']}, "
                f"attempts {sums['attempts']}")

    def header(recoveries: list) -> str:
        if not recoveries:
            return "n/a"
        keys = sorted({recovery["distinct_keys"] for recovery in recoveries})
        episodes = sum(recovery["episodes"] for recovery in recoveries)
        return f"{len(recoveries)} runs, {episodes} episodes, distinct keys {', '.join(map(str, keys))}"

    rows = [["runs", header(per_side[0]), header(per_side[1])]]
    for frame in ATLAS_RECOVERY_FRAMES:
        rows.append([frame, cell(per_side[0], (frame,)), cell(per_side[1], (frame,))])
    rows.append(["A-D total", cell(per_side[0], ATLAS_RECOVERY_FRAMES), cell(per_side[1], ATLAS_RECOVERY_FRAMES)])
    return rows


def comparison_document(rows: Sequence[Sequence[str]], lap_rows: Sequence[Sequence[str]],
                        alloc_rows: Sequence[Sequence[str]], host_lines: Sequence[str],
                        detail_lines: Sequence[str], *, counter_rows: Sequence[Sequence[str]] = (),
                        counters_note: str = "", overhead_rows: Sequence[Sequence[str]] = (),
                        capped_note: str = "", recovery_rows: Sequence[Sequence[str]] = (),
                        row_run_rows: Sequence[Sequence[str]] = ()) -> str:
    """Assemble comparison.md: the PR table, the laps, counters, overhead and allocation tables when run,
    the host block and details.

    The counters section appears when the counters set ran or was skipped; `counters_note` says which. The
    Row-run shaping section appears when a counters set measured a row-run decision phase.
    `capped_note` names the variants whose short-mode runs were capped, under the PR table.
    """
    parts = ["## Performance comparison\n\n" + render_table(rows)
             + (f"\n{capped_note}\n" if capped_note else "")]
    if lap_rows:
        parts.append("### Laps (`--laps` runs, never pooled with timed runs)\n\n" + render_table(lap_rows))
    if counter_rows or counters_note:
        section = "### Frame counters (`--counters` runs on the head, never pooled with timed or laps runs)\n\n"
        if counters_note:
            section += counters_note + "\n\n"
        parts.append(section + (render_table(counter_rows, COUNTERS_HEADER) if counter_rows else ""))
    if row_run_rows:
        parts.append(f"### Row-run shaping (partial shard evidence)\n\n{ROW_RUN_NOTE}\n\n"
                     + render_table(row_run_rows, ROW_RUN_HEADER))
    if recovery_rows:
        parts.append(f"### Atlas retry recovery (S1/atlas-retry counters runs)\n\n{RECOVERY_NOTE}\n\n"
                     + render_table(recovery_rows, RECOVERY_HEADER))
    if overhead_rows:
        parts.append(f"### Counters overhead (S2 and S3)\n\n{OVERHEAD_NOTE}.\n\n"
                     + render_table(overhead_rows, OVERHEAD_HEADER))
    if alloc_rows:
        parts.append("### Allocations per frame (`--alloc` runs, never pooled with timed runs)\n\n"
                     + render_table(alloc_rows))
    parts.append("### Host\n\n" + "\n".join(host_lines) + "\n")
    parts.append("<details><summary>SHAs, harness hash, commands and raw logs</summary>\n\n"
                 + "\n".join(detail_lines) + "\n\n</details>\n")
    return "\n".join(parts)



# --- Comparison driver and command line -----------------------------------------------------

EXIT_USAGE = 2
_SHA = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")


# The sys.platform values the harness measures on.
RUN_PLATFORMS = ("darwin", "win32")
# local-gate.py's host names, by sys.platform.
GATE_HOSTS = {"darwin": "macos", "win32": "windows"}


def gate_hosts(platform_name: str) -> tuple[str, ...]:
    """The hosts a synthetic gate step names: the current one, since local-gate selects and labels by host."""
    return (GATE_HOSTS.get(platform_name, "linux"),)


def gate_problem(gate, os_name: str | None = None) -> str | None:
    """Explain why run_step cannot bound and reap children on this interpreter, or return None.

    On Windows the gate's job owns every process, so no process-group id must stay reserved.
    """
    problem = gate.sigchld_problem()
    if problem:
        return problem
    if (os_name or os.name) != "nt" and not gate.leader_watches():
        return ("this Python sees a child's exit only by reaping it (it has neither waitid nor kqueue), "
                "so run_step cannot keep a run's process-group id reserved")
    return None


def _resolve_sha(host_run: Callable, ref: str) -> str:
    """Resolve a ref to its commit SHA with read-only `git rev-parse`."""
    record = host_run(("git", "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"))
    sha = record.stdout.strip()
    if _command_failure(record) or not _SHA.fullmatch(sha):
        raise ValueError(f"cannot resolve {ref!r} to a commit: {record.stderr.strip() or sha or 'no output'}")
    return sha


def _allocation_metric(key: tuple[str, str, str]) -> bool:
    """Whether a metric belongs in the allocation table; the counting allocator's timings do not."""
    return key[0].endswith("allocations per frame")


def release_profile_overrides(environ: Mapping[str, str]) -> str:
    """Name the CARGO_PROFILE_RELEASE_* overrides both builds inherited, or `none`.

    CI relaxes the release profile to fit a pull request's time budget; both refs build with the same
    overrides, so the comparison stays fair, but the binaries differ from the shipped profile.
    """
    overrides = sorted(f"{name}={value}" for name, value in environ.items()
                       if name.startswith("CARGO_PROFILE_RELEASE_"))
    return " ".join(overrides) if overrides else "none"


def comparison_command(args: argparse.Namespace) -> str:
    """Reconstruct the invocation, so the details block records how the table was produced."""
    words = ["python3", "scripts/perf-compare.py", "--base", args.base, "--head", args.head]
    for value in args.scenario or ["all"]:
        words += ["--scenario", value]
    words += ["--runs", str(args.runs or DEFAULT_RUNS)]
    words += [flag for flag, chosen in (("--short", args.short), ("--laps", args.laps), ("--alloc", args.alloc),
                                        ("--counters", args.counters), ("--require-base", args.require_base))
              if chosen]
    if args.counters_runs is not None:
        words += ["--counters-runs", str(args.counters_runs)]
    for value in args.laps_scenario or []:
        words += ["--laps-scenario", value]
    if args.laps_runs is not None:
        words += ["--laps-runs", str(args.laps_runs)]
    if args.keep:
        words.append("--keep")
    if args.prebuilt is not None:
        words += ["--prebuilt", str(args.prebuilt), "--prebuilt-run-id", args.prebuilt_run_id,
                  "--prebuilt-attempt", args.prebuilt_attempt,
                  "--prebuilt-manifest-sha256", args.prebuilt_manifest_sha256]
    return " ".join(words)


def comparison_renderer(results: Iterable[SetResult]) -> dict | None:
    """The adapter the comparison's valid runs drew through, for the host block; None when none reported one."""
    for result in results:
        for side in (result.base, result.head):
            for outcome in side.outcomes:
                if outcome.renderer is not None:
                    return outcome.renderer
    return None


# --- Build once, measure on many runners: the producer's manifest and the consumer's checks -------

PREBUILT_SCHEMA_VERSION = 1
MANIFEST_FILE = "manifest.json"
# Under a comparison's work directory: the consumer's copies, with no `assets` beside them.
PREBUILT_DIRECTORY = "prebuilt"
TIMING_FILE = "timing.json"
# The marks timing.json records, in the order they happen.
TIMING_MARKS = ("start", "listed", "measure_start", "measure_end", "report_written")
_HEX_SHA256 = re.compile(r"[0-9a-f]{64}")
_RELEASE_PROFILE = re.compile(r"\s*\[\s*profile\.release\s*\]\s*(?:#.*)?")
_LTO_KEY = re.compile(r"\s*lto\s*=\s*(.+?)\s*(?:#.*)?")
_FILE_CHUNK_BYTES = 1024 * 1024


def executable_name(example: str, platform_name: str = sys.platform) -> str:
    """The file name Cargo gives an example's executable on a platform."""
    return example + ".exe" if platform_name == "win32" else example


def file_sha256(path: Path) -> str:
    """The sha256 of a file's bytes, read in chunks."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(_FILE_CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def release_lto(root: Path, environ: Mapping[str, str]) -> str:
    """The LTO setting a release build of `root` uses: the environment override, else `[profile.release]` lto.

    Returns `unset` when neither names one. Only the workspace manifest's own table is read.
    """
    override = environ.get("CARGO_PROFILE_RELEASE_LTO")
    if override is not None:
        return override
    try:
        lines = (root / "Cargo.toml").read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError):
        return "unset"
    in_release = False
    for line in lines:
        if line.lstrip().startswith("["):
            in_release = _RELEASE_PROFILE.fullmatch(line) is not None
            continue
        match = _LTO_KEY.fullmatch(line) if in_release else None
        if match:
            return match.group(1).strip('"\'')
    return "unset"


def build_identity(trees: Mapping[str, Path], features: Mapping[str, Sequence[str]], host_run: Callable,
                   environ: Mapping[str, str]) -> dict[str, object]:
    """What a build depends on beyond the SHAs: target, toolchain, profile, features and runner image.

    The producer records it in the manifest; each consumer derives its own from its own job and
    refuses an artifact that differs, so a toolchain or image rollover fails closed.
    """
    toolchain = {}
    for name, argv in (("rustc", ("rustc", "-vV")), ("cargo", ("cargo", "-V"))):
        record = host_run(argv)
        failure = _command_failure(record)
        if failure:
            raise ValueError(f"cannot read the toolchain: {failure}: {record.stderr.strip()}")
        toolchain[name] = record.stdout.strip()
    target = next((line.split(":", 1)[1].strip() for line in toolchain["rustc"].splitlines()
                   if line.startswith("host:")), "")
    return {
        "target": target,
        "toolchain": toolchain,
        "profile": {"lto": {side: release_lto(trees[side], environ) for side in SIDES},
                    "overrides": release_profile_overrides(environ)},
        # Each side's cargo features, a list so a later feature joins without a schema change.
        "features": {side: list(features[side]) for side in SIDES},
        "runner_image": {"os": environ.get("ImageOS", ""), "version": environ.get("ImageVersion", "")},
    }


def publish_binaries(destination: Path, builds: Mapping[str, Mapping[str, object]], shas: Mapping[str, str],
                     digest: str, identity: Mapping[str, object], environ: Mapping[str, str]) -> int:
    """Copy each built binary under `destination/<side>/` and write manifest.json; print its sha256.

    Only executables and the manifest are published: each consumer runs a ref from its own tree, which
    supplies that ref's assets.
    """
    binaries: dict[str, dict[str, dict[str, str]]] = {}
    for side in SIDES:
        binaries[side] = {}
        for example, built in builds[side].items():
            relative = f"{side}/{executable_name(example)}"
            target = destination / relative
            if os.path.lexists(target):
                raise ValueError(f"{target} already exists; a producer never overwrites a published binary")
            target.parent.mkdir(parents=True, exist_ok=True)
            # copy2 keeps the executable bit, which the tarball then carries to the consumer.
            shutil.copy2(built, target)
            binaries[side][example] = {"path": relative, "sha256": file_sha256(target)}
    manifest = {"schema_version": PREBUILT_SCHEMA_VERSION, "run_id": environ.get("GITHUB_RUN_ID", ""),
                "run_attempt": environ.get("GITHUB_RUN_ATTEMPT", ""), "base_sha": shas["base"],
                "head_sha": shas["head"], "harness_hash": digest, "binaries": binaries, **identity}
    data = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8")
    (destination / MANIFEST_FILE).write_bytes(data)
    print(f"manifest_sha256={hashlib.sha256(data).hexdigest()}", flush=True)
    return EXIT_PASS


def _refuse(reason: str) -> ValueError:
    """The error for an artifact this job must not measure."""
    return ValueError(f"refusing the prebuilt binaries: {reason}")


def load_prebuilt(args: argparse.Namespace, shas: Mapping[str, str], digest: str, identity: Mapping[str, object],
                  examples: Sequence[str], copies: Path) -> dict[str, dict[str, Path]]:
    """Check the producer's binaries against this job and copy them under `copies/<side>/`.

    Refused, in order: a missing directory or manifest or another schema; a manifest whose sha256 is not
    the published one; another run or producer attempt; another base or head SHA; another harness hash;
    other features; another target, toolchain or runner image; another profile; a binary that is missing,
    a symlink, not executable or misdigested, or an example a selected set needs that was not built. The
    caller then checks that each copy lists and finds its tree's assets.
    """
    directory = Path(args.prebuilt)
    if directory.is_symlink() or not directory.is_dir():
        raise _refuse(f"no directory {directory}")
    manifest_path = directory / MANIFEST_FILE
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise _refuse(f"no {MANIFEST_FILE} in {directory}")
    data = manifest_path.read_bytes()
    try:
        manifest = json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as error:
        raise _refuse(f"{MANIFEST_FILE} is not JSON: {error}") from error
    if not isinstance(manifest, dict) or manifest.get("schema_version") != PREBUILT_SCHEMA_VERSION:
        found = manifest.get("schema_version") if isinstance(manifest, dict) else None
        raise _refuse(f"manifest schema {found!r} is not {PREBUILT_SCHEMA_VERSION}")
    actual = hashlib.sha256(data).hexdigest()
    if actual != args.prebuilt_manifest_sha256:
        raise _refuse(f"manifest sha256 {actual} is not the producer's {args.prebuilt_manifest_sha256}")
    if str(manifest.get("run_id")) != args.prebuilt_run_id:
        raise _refuse(f"manifest run id {manifest.get('run_id')} is not this run's {args.prebuilt_run_id}")
    if str(manifest.get("run_attempt")) != args.prebuilt_attempt:
        raise _refuse(f"manifest run attempt {manifest.get('run_attempt')} is not the producer's "
                      f"attempt {args.prebuilt_attempt}")
    for side in SIDES:
        if manifest.get(f"{side}_sha") != shas[side]:
            raise _refuse(f"{side} SHA {manifest.get(f'{side}_sha')} is not this job's {shas[side]}")
    if manifest.get("harness_hash") != digest:
        raise _refuse(f"harness hash {manifest.get('harness_hash')} is not this job's {digest}")
    checks = (("features", "features"), ("target", "target"), ("toolchain", "toolchain"),
              ("runner_image", "runner image"), ("profile", "profile"))
    for key, name in checks:
        if manifest.get(key) != identity[key]:
            raise _refuse(f"{name} {manifest.get(key)!r} is not this job's {identity[key]!r}")
    published = manifest.get("binaries") if isinstance(manifest.get("binaries"), dict) else {}
    builds: dict[str, dict[str, Path]] = {side: {} for side in SIDES}
    for side in SIDES:
        entries = published.get(side) if isinstance(published.get(side), dict) else {}
        if (directory / side).is_symlink():
            raise _refuse(f"{side}: {directory / side} is a symlink")
        for example in examples:
            entry = entries.get(example)
            relative = f"{side}/{executable_name(example)}"
            if not isinstance(entry, dict):
                raise _refuse(f"{side} {example}: the manifest has no such binary")
            if entry.get("path") != relative or not _HEX_SHA256.fullmatch(str(entry.get("sha256"))):
                raise _refuse(f"{side} {example}: entry {entry!r} does not name {relative} and its sha256")
            source = directory / relative
            if source.is_symlink():
                raise _refuse(f"{side} {example}: {relative} is a symlink")
            if not source.is_file():
                raise _refuse(f"{side} {example}: {relative} is missing")
            # When: on Windows the executable bit does not exist, so only POSIX hosts check it.
            if os.name != "nt" and not os.access(source, os.X_OK):
                raise _refuse(f"{side} {example}: {relative} is not executable")
            found = file_sha256(source)
            if found != entry["sha256"]:
                raise _refuse(f"{side} {example}: {relative} sha256 digest {found} differs from the manifest's "
                              f"{entry['sha256']}")
            # The copy has no `assets` sibling, so asset_dir() resolves the run's tree, as for a local build.
            target = copies / side / executable_name(example)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            if file_sha256(target) != found:
                raise _refuse(f"{side} {example}: the copy at {target} differs from {relative}")
            builds[side][example] = target
    return builds


def builds_line(args: argparse.Namespace, work: Path) -> str:
    """The details line saying where the binaries came from: this job's builds or the producer's."""
    command_text = " ".join(build_argv(HARNESS_EXAMPLE, release=True))
    if args.prebuilt is not None:
        return (f"- Builds: prebuilt by run {args.prebuilt_run_id} attempt {args.prebuilt_attempt}, manifest "
                f"sha256 `{args.prebuilt_manifest_sha256}`: `{command_text}` in each worktree, copied to "
                f"`{work / PREBUILT_DIRECTORY}`")
    return f"- Builds: `{command_text}` in each worktree, one CARGO_TARGET_DIR per ref"


def write_timing(out: Path, marks: Mapping[str, float], environ: Mapping[str, str]) -> None:
    """Write timing.json: this job's run, attempt, job name, shard and the comparison's marks in unix seconds.

    perf-critical-path.py splits a comparison step's time into prepare, scenarios and report with it.
    """
    _write_json(out / TIMING_FILE, {
        "schema_version": SCHEMA_VERSION, "run_id": environ.get("GITHUB_RUN_ID", ""),
        "run_attempt": environ.get("GITHUB_RUN_ATTEMPT", ""), "job": environ.get("PERF_JOB_NAME", ""),
        "shard": environ.get("PERF_SHARD", ""), "marks": {name: marks[name] for name in TIMING_MARKS}})


def prepare_trees(args: argparse.Namespace, worktrees: Worktrees,
                  host_run: Callable) -> tuple[dict[str, str], dict[str, Path], str]:
    """Resolve both refs, add their worktrees and overlay the head's harness; return SHAs, trees and harness hash."""
    shas = {side: _resolve_sha(host_run, ref) for side, ref in (("base", args.base), ("head", args.head))}
    trees = {"head": worktrees.create("head", shas["head"])}
    trees["base"] = worktrees.create("base", shas["base"])
    overlay_harness(trees["head"], trees["base"])
    hashes = {side: tree_harness_hash(trees[side]) for side in SIDES}
    if hashes["base"] != hashes["head"]:
        raise ValueError(f"harness hashes differ after the overlay: base {hashes['base']}, head {hashes['head']}")
    print(f"[perf-compare] base={shas['base']} head={shas['head']} harness_hash={hashes['head']}", flush=True)
    return shas, trees, hashes["head"]


def build_sides(args: argparse.Namespace, gate, trees: Mapping[str, Path], features: Mapping[str, Sequence[str]],
                examples: Sequence[str], out: Path, work: Path) -> tuple[dict[str, dict[str, object]], int]:
    """Build every example of both refs through the gate's reviewed steps; return the builds and the last log index.

    A build maps an example to its binary, or, for a lenient base, to the reason it cannot run.
    """
    builds: dict[str, dict[str, object]] = {side: {} for side in SIDES}
    index = 0
    # One build at a time, each with its ref's own target directory; the head goes first, so a head
    # that cannot build fails before the base's build time is spent.
    for side in ("head", "base"):
        environ = dict(os.environ, CARGO_TARGET_DIR=str(work / f"target-{side}"))
        for example in examples:
            index += 1
            # A tree builds with exactly the perf features it declares; each build is the gate's own reviewed step.
            catalog = gate.PERF_FEATURE_BUILDS[tuple(features[side])]
            step = catalog[f"build-{side}-{example}"]
            result = gate.run_step(step, index, trees[side], out, environ)
            text = read_log(result.log_path)
            binary = artifact_executable(text, example)
            built = build_passed(result)
            problem = asset_problem(binary, trees[side]) if built and binary else None
            # A strict comparison treats the base like the head: no build or asset problem is tolerated.
            strict = side == "head" or args.require_base
            if built and binary is not None and problem is None:
                builds[side][example] = binary
            elif problem is not None and strict:
                raise ValueError(f"the {side} cannot run {example}: {problem}")
            elif problem is not None:
                builds[side][example] = f"base cannot run {example}: {problem}"
            elif strict:
                raise ValueError(f"the {side} cannot build {example}: {build_error(text)} (log {result.log_path})")
            else:
                builds[side][example] = f"base cannot build {example}: {build_error(text)}"
    return builds, index


def _list_side(gate, binary: Path, out: Path, index: int, side: str, prebuilt: bool) -> list[Scenario]:
    """List one side's scenarios; a prebuilt binary that cannot list refuses the artifact."""
    try:
        return list_scenarios(gate, binary, out, index)
    except ValueError as error:
        # When: the binary came from the producer, its failure to start is the artifact's refusal.
        if prebuilt:
            raise ValueError(f"refusing the prebuilt binaries: the {side} binary failed: {error}") from error
        raise


def _compare(args: argparse.Namespace, gate, out: Path, work: Path, worktrees: Worktrees,
             host_run: Callable) -> int:
    """Build (or load) both refs with the head's harness, run every selected set and write comparison.md.

    With `--build-only` it publishes the binaries and their manifest instead of measuring; with
    `--prebuilt` it measures binaries the producer job built. compare_main owns the worktrees' removal,
    which runs whatever this raises.
    """
    marks = {"start": time.time()}
    runs = args.runs or DEFAULT_RUNS
    shas, trees, digest = prepare_trees(args, worktrees, host_run)
    examples = (HARNESS_EXAMPLE,) + ((ALLOC_EXAMPLE,) if args.alloc else ())
    # A tree builds every harness with the perf features it declares, and only with those.
    features = {side: tree_features(trees[side]) for side in SIDES}
    supports = {side: COUNTERS_FEATURE in features[side] for side in SIDES}
    prebuilt = args.prebuilt is not None
    identity = None
    if prebuilt or args.build_only is not None:
        identity = build_identity(trees, features, host_run, os.environ)
    index = 0
    if prebuilt:
        builds = load_prebuilt(args, shas, digest, identity, examples, work / PREBUILT_DIRECTORY)
        # The copies run from each ref's own tree, which must hold that ref's assets.
        for side in ("head", "base"):
            for example in examples:
                problem = asset_problem(builds[side][example], trees[side])
                if problem is not None:
                    raise ValueError(f"refusing the prebuilt binaries: the {side} cannot run {example}: {problem}")
    else:
        builds, index = build_sides(args, gate, trees, features, examples, out, work)
    index += 1
    scenarios = _list_side(gate, builds["head"][HARNESS_EXAMPLE], out, index, "head", prebuilt)
    if args.require_base or prebuilt or args.build_only is not None:
        # The base must load and list too, so a binary that cannot start fails before anything is measured.
        index += 1
        _list_side(gate, builds["base"][HARNESS_EXAMPLE], out, index, "base", prebuilt)
    if args.build_only is not None:
        return publish_binaries(args.build_only, builds, shas, digest, identity, os.environ)
    marks["listed"] = time.time()
    by_id = {scenario.id: scenario for scenario in scenarios}
    selected = select_scenarios(args.scenario or ["all"], scenarios)
    host = production_host(gate)
    # Only these variants run the laps set; every selected one with --laps, none without a laps selection.
    laps_selection = (select_laps_scenarios(args.laps_scenario, scenarios, selected) if args.laps_scenario
                      else selected if args.laps else [])
    # Each set is (name, example, laps, counters, valid runs per side).
    sets = [("timed", HARNESS_EXAMPLE, False, False, runs)]
    if laps_selection:
        sets.append(("laps", HARNESS_EXAMPLE, True, False, args.laps_runs or runs))
    counters_note = ""
    if args.counters and supports["head"]:
        sets.append(("counters", HARNESS_EXAMPLE, False, True, args.counters_runs or runs))
    elif args.counters:
        counters_note = COUNTERS_UNSUPPORTED
        print(f"[perf-compare] {COUNTERS_UNSUPPORTED}", flush=True)
    if args.alloc:
        sets.append(("alloc", ALLOC_EXAMPLE, False, False, runs))
    # Checked once the sets are known: --counters on a head without perf-counters runs no counters set either.
    refusal = counters_only_problem(selected, any(set_name == "counters" for set_name, *_ in sets))
    if refusal is not None:
        raise ValueError(refusal)
    results = []
    marks["measure_start"] = time.time()
    # One untimed replay per delivered scenario, before any measured run; both sides share the overlaid harness.
    deliveries: dict[str, DeliveryOutcome] = {}
    replay_evidence = out / "delivery"
    for scenario_id, variant in selected:
        if delivery_replayed(scenario_id, sys.platform):
            replay_evidence.mkdir(parents=True, exist_ok=True)
            index += 1
            deliveries[f"{scenario_id}/{variant}"] = run_delivery_replay(
                gate, builds["head"][HARNESS_EXAMPLE], scenario_id, variant, replay_evidence, index + 1,
                short=args.short, timeout_s=run_timeout_s(by_id[scenario_id], False, args.short),
                temp_root=host.temp_root, environ=host.environ)
    # One reference for every set, so all valid runs of the comparison share one refresh rate and scale.
    display = DisplayReference()
    for scenario_id, variant in selected:
        label = f"{scenario_id}/{variant}"
        delivery_problem = deliveries.get(label, (None, None))[1]
        if delivery_problem is not None:
            # When: the replay could not show how ConPTY delivered the scenario, no set of it is measured.
            results.extend(blocked_set_results(label, delivery_problem, [
                set_name for set_name, *_ in variant_sets(scenario_id, variant, sets)
                if set_name != "laps" or (scenario_id, variant) in laps_selection]))
            continue
        for set_name, example, laps, counters, requested_runs in variant_sets(scenario_id, variant, sets):
            if laps and (scenario_id, variant) not in laps_selection:
                # When: --laps-scenario does not name this variant, it runs no laps set.
                continue
            # A capped variant takes min(requested, cap) runs in every set under --short.
            set_runs = capped_runs(by_id[scenario_id], variant, requested_runs, args.short)
            built = {side: builds[side][example] for side in SIDES}
            # A base that did not build is retired before its first slot, so its placeholder path never runs.
            # Both sides run the head's overlaid harness (equal hashes), so the head's list decides the split schema.
            plans = {side: RunPlan(by_id[scenario_id], variant, side,
                                   built[side] if isinstance(built[side], Path) else Path("unbuilt"),
                                   digest, short=args.short, laps=laps, counters=counters, source_root=trees[side],
                                   latency_split_schema=by_id[scenario_id].latency_split_schema)
                     for side in SIDES}
            base_blocked = built["base"] if isinstance(built["base"], str) else None
            if counters and not supports["base"]:
                # A base without the feature has no counters, so the set runs on the head only, base n/a.
                base_blocked = COUNTERS_HEAD_ONLY
            result = run_set(label, plans, base_blocked, set_runs,
                             lambda plan, evidence: execute_run(plan, host, evidence),
                             out / "runs" / f"{scenario_id}-{variant}" / set_name, set_name, display=display)
            result.requested_runs = requested_runs
            results.append(result)
    marks["measure_end"] = time.time()
    timed_rows, lap_rows, alloc_rows, counters_table, overhead = [], [], [], [], []
    recovery_rows: list[list[str]] = []
    row_run_evidence: dict[tuple[str, str, str], RowRunPhase] = {}
    split_details: list[str] = []
    timed_heads: dict[str, SideRuns] = {}
    omitted = 0
    presenter_notes: list[str] = []
    for result in results:
        # Lookups stay keyed by the plain label; only the rows carry the cap.
        shown = capped_label(result)
        if result.set_name == "timed":
            timed_rows.extend(comparison_rows(shown, result.base, result.head))
            timed_heads[result.label] = result.head
            if result.label in deliveries:
                timed_rows.extend(delivery_rows(result.label, *deliveries[result.label]))
        elif result.set_name == "laps":
            lap_rows.extend(laps_rows(shown, result.base, result.head))
            lap_rows.extend(fallback_rows(shown, result.base, result.head))
        elif result.set_name == "counters":
            rows, left_out = counter_rows(shown, result.base, result.head)
            counters_table.extend(rows)
            recovery_rows.extend(atlas_recovery_rows(result.label, result.base, result.head))
            for label, phase_name in ROW_RUN_DECISION_PHASES:
                if label == result.label:
                    row_run_evidence[(row_run_platform(), label, phase_name)] = row_run_phase(
                        result.base, result.head, phase_name)
            counters_table.extend(glyph_atlas_reconciliation_rows(shown, result.base, result.head))
            split_details.extend(attempt_split_details(shown, result.base, result.head))
            counters_table.extend(split_rows(result.label, result.base, result.head,
                                             by_id[result.label.split("/")[0]].latency_split_schema))
            omitted += left_out
            for side_name, side in (("base", result.base), ("head", result.head)):
                presenter_notes.extend(presenter_counter_notes(result.label, side_name, side))
            # The timed set always comes first, so its head runs are the counters-off side.
            if overhead_applies(result.label) and result.label in timed_heads:
                overhead.extend(comparison_rows(result.label, timed_heads[result.label], result.head))
        else:
            alloc_rows.extend(comparison_rows(shown, result.base, result.head, _allocation_metric))
    capped = [f"{result.label} {result.set_name} {result.target_runs} of {result.requested_runs} runs"
              for result in results if 0 < result.target_runs < result.requested_runs]
    capped_note = f"Capped variants (--short): {'; '.join(capped)}." if capped else ""
    if counters_table:
        counters_note = ("Counts are the median of each phase's per-run delta (range in brackets); a histogram's "
                         "p95 and max are bucket bounds over every run's events, its mean is sum_us/count in its "
                         "unit. A change compares a count's medians or a histogram's means. The baseline reads n/a "
                         "when the base does not declare perf-counters, or for a field its contract lacks. "
                         f"{omitted} counter(s) that were 0 on both sides are left out. "
                         "A glyph_atlas_growths, snapshot/counted row compares, per run and live window, the "
                         "growths in the checkpoint's memory snapshot with those the window's counters recorded at "
                         "the same sample; only equal figures are consistent. A run without that per-window "
                         "reading is inconclusive and shows the snapshot's and its phases' sums.")
        # A run whose frame counts contradict its recorded presenter is named, never passed silently.
        counters_note += "".join(f" Presenter mismatch: {note}." for note in presenter_notes)
    if sys.platform == "win32":
        host_lines = host_block_windows(windows_host_outputs(host_run), display.monitor, comparison_renderer(results))
    else:
        outputs = {}
        for name, argv in HOST_COMMANDS.items():
            record = host_run(argv, HOST_COMMAND_TIMEOUT_S)
            outputs[name] = "" if _command_failure(record) else record.stdout
        host_lines = host_block(outputs, display.monitor)
    run_template = harness_argv(Path("<binary>"), "<ID>", "<variant>", digest, Path("<new scratch path>"))
    details = [f"- Base: `{args.base}` = `{shas['base']}`", f"- Head: `{args.head}` = `{shas['head']}`",
               f"- Harness hash (both trees): `{digest}`", f"- Command: `{comparison_command(args)}`",
               f"- Run length: {'short (--short: every hold 5 s, smaller floods)' if args.short else 'full'}",
               f"- Release profile overrides (both refs): {release_profile_overrides(os.environ)}",
               builds_line(args, work), f"- Runs: `{' '.join(run_template)}`",
               f"- Built with `--features {COUNTERS_FEATURE}`: "
               f"{', '.join(side for side in SIDES if supports[side]) or 'neither ref'}",
               f"- Built with `--features {FRAME_TEXTURE_FEATURE}`: "
               f"{', '.join(side for side in SIDES if FRAME_TEXTURE_FEATURE in features[side]) or 'neither ref'}",
               f"- Built with `--features {CHECKPOINT_MEMORY_FEATURE}`: "
               f"{', '.join(side for side in SIDES if CHECKPOINT_MEMORY_FEATURE in features[side]) or 'neither ref'}",
               f"- Built with `--features {ECHO_TRACE_FEATURE}`: "
               f"{', '.join(side for side in SIDES if ECHO_TRACE_FEATURE in features[side]) or 'neither ref'}",
               f"- Built with `--features {TRIM_HOOK_FEATURE}`: "
               f"{', '.join(side for side in SIDES if TRIM_HOOK_FEATURE in features[side]) or 'neither ref'}",
               f"- Evidence: `{out}`", "", "Raw logs:", ""]
    for result in results:
        for side, evidence, kind, _reasons in result.attempts:
            details.append(f"- {result.label} {result.set_name} {side} {kind}: `{evidence}/01-harness.log`")
    if split_details:
        details += ["", "Per-run render-attempt splits (phases with a render attempt):", ""] + split_details
    document = comparison_document(timed_rows, lap_rows, alloc_rows, host_lines, details,
                                   counter_rows=counters_table, counters_note=counters_note, overhead_rows=overhead,
                                   capped_note=capped_note, recovery_rows=recovery_rows,
                                   row_run_rows=row_run_rows(row_run_evidence) if row_run_evidence else ())
    problems = strict_problems(results) if args.require_base else []
    if problems:
        # The first line says the table is partial, so nobody reads a head-only table as a comparison.
        document = f"**Incomplete comparison:** {'; '.join(problems)}\n\n" + document
    (out / "comparison.md").write_text(document, encoding="utf-8")
    profile = {"lto": {side: release_lto(trees[side], os.environ) for side in SIDES},
               "overrides": release_profile_overrides(os.environ)}
    _write_json(out / ROW_RUN_EVIDENCE_FILE, row_run_evidence_document(
        results, out, shas, digest, features, args.short, profile, args.counters, os.environ))
    marks["report_written"] = time.time()
    write_timing(out, marks, os.environ)
    print(document, flush=True)
    return comparison_exit(results, args.require_base)


def compare_main(args: argparse.Namespace) -> int:
    """Compare two refs; remove the worktrees and target directories it created unless --keep."""
    if sys.platform not in RUN_PLATFORMS:
        print("[perf-compare] BLOCKED: the harness runs only on macOS and Windows", flush=True)
        return EXIT_BLOCKED
    gate = load_gate()
    problem = gate_problem(gate)
    if problem:
        print(f"[perf-compare] {problem}", file=sys.stderr)
        return EXIT_FAIL
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}"
    out = (args.out or ROOT / "target" / "perf-compare" / f"out-{stamp}").resolve()
    if args.build_only is not None:
        destination = args.build_only = args.build_only.resolve()
        if destination.exists() and (not destination.is_dir() or any(destination.iterdir())):
            print(f"[perf-compare] --build-only {destination} is not an empty directory", file=sys.stderr)
            return EXIT_USAGE
        # The build logs sit beside the published files; the workflow tars only the manifest and binaries.
        out = destination / "build-logs"
    work = work_directory(stamp)
    if out.exists() and (not out.is_dir() or any(out.iterdir())):
        print(f"[perf-compare] --out {out} is not an empty directory; two comparisons never share evidence",
              file=sys.stderr)
        return EXIT_USAGE
    if os.path.lexists(work):
        print(f"[perf-compare] {work} already exists; a comparison only removes what it created", file=sys.stderr)
        return EXIT_USAGE
    out.mkdir(parents=True, exist_ok=True)
    print(f"[perf-compare] evidence={out}", flush=True)
    print(f"[perf-compare] {focus_rule_line(os.environ)}", flush=True)
    runner = gate.SMOKE_RUNNER.run_command

    def host_run(argv: Sequence[str], timeout_s: int = GIT_TIMEOUT_S) -> CommandRecord:
        return bounded_command(runner, argv, timeout_s)

    worktrees = Worktrees(host_run, work)
    try:
        return _compare(args, gate, out, work, worktrees, host_run)
    except StopComparison as error:
        print(f"[perf-compare] stopped: {error}", file=sys.stderr)
        return EXIT_FAIL
    except (OSError, ValueError) as error:
        print(f"[perf-compare] {error}", file=sys.stderr)
        return EXIT_FAIL
    finally:
        if args.keep:
            print(f"[perf-compare] kept the worktrees and target directories under {work}", flush=True)
        else:
            for failure in worktrees.remove():
                print(f"[perf-compare] worktree removal failed: {failure}", file=sys.stderr)
            # Everything under `work` was created by this run: it did not exist before it started.
            for side in SIDES:
                shutil.rmtree(work / f"target-{side}", ignore_errors=True)
            shutil.rmtree(work / PREBUILT_DIRECTORY, ignore_errors=True)
            try:
                work.rmdir()
            except OSError:
                pass  # A worktree git could not remove stays, and was reported above.


def positive_int(text: str) -> int:
    """Parse a positive integer option."""
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError(f"{text} is not a positive integer")
    return value


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    """Parse the command line: `--smoke` alone, or a comparison of `--base` and `--head`."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--smoke", action="store_true",
                        help="run the macos-perf-smoke gate step: a debug build of this tree, short S1 and S3 "
                             "runs and S1 ended as a step deadline ends it")
    parser.add_argument("--base", help="the baseline ref, usually the merge-base")
    parser.add_argument("--head", help="the ref under test")
    parser.add_argument("--scenario", action="extend", nargs="+", metavar="ID[/VARIANT]",
                        help="`all` (every scenario's default variant), an ID or an ID/variant; several values "
                             "or repeated flags run in the order given, without repeats (default: all)")
    parser.add_argument("--runs", type=positive_int,
                        help=f"valid runs per side and scenario (default: {DEFAULT_RUNS})")
    parser.add_argument("--short", action="store_true",
                        help="run every scenario with the harness's --short holds (5 s) and smaller floods, "
                             "for a quick comparison; the table says so")
    parser.add_argument("--laps", action="store_true",
                        help="also run a --laps set and print its render_timing laps table")
    parser.add_argument("--laps-scenario", action="append", metavar="ID[/VARIANT]",
                        help="run the separate laps set for this selected variant only (a bare ID is its default "
                             "variant); repeatable; an error with --laps")
    parser.add_argument("--laps-runs", type=positive_int, metavar="N",
                        help="valid runs of the laps set (default: --runs); needs --laps or --laps-scenario")
    parser.add_argument("--alloc", action="store_true",
                        help="also build and run perf_scenarios_alloc and print allocations per frame")
    parser.add_argument("--counters", action="store_true",
                        help="when the head declares the perf-counters feature, also run a --counters set on "
                             "it, and on the base too when the base declares it; print the frame-counter "
                             "table and the S2/S3 overhead table")
    parser.add_argument("--counters-runs", type=positive_int, metavar="N",
                        help="valid runs of the counters set (default: --runs)")
    parser.add_argument("--keep", action="store_true", help="keep the worktrees and target directories")
    parser.add_argument("--build-only", type=Path, metavar="DIR",
                        help="build both refs once, list both binaries and publish them with manifest.json under "
                             "an empty DIR, printing manifest_sha256=<hex>; measures nothing (needs "
                             "--require-base)")
    parser.add_argument("--prebuilt", type=Path, metavar="DIR",
                        help="measure the binaries a --build-only job published in DIR instead of building; "
                             "refused unless its manifest matches this job")
    parser.add_argument("--prebuilt-run-id", metavar="ID", help="the workflow run that must have built --prebuilt")
    parser.add_argument("--prebuilt-attempt", metavar="N", help="the producer job's run attempt")
    parser.add_argument("--prebuilt-manifest-sha256", metavar="HEX",
                        help="the manifest digest the producer job published")
    parser.add_argument("--require-base", action="store_true",
                        help="fail unless the base builds, lists and gets every valid run the head does (CI "
                             "passes it); a counters set on a base without perf-counters still reads n/a")
    parser.add_argument("--out", type=Path,
                        help="an empty directory for the evidence and comparison.md "
                             "(default: target/perf-compare/out-<stamp>)")
    parser.add_argument("--row-run-decide", nargs="+", metavar="ARTIFACT_DIR",
                        help="analysis only: decide the row-run diagnostic from one workflow execution's downloaded "
                             "perf-comparison artifacts; builds and runs nothing")
    parser.add_argument("--row-run-execution", nargs=2, action="append", metavar=("RUN_RECORD", "ATTEMPT"),
                        help="a recorded workflow run (perf-critical-path.py's record) and the attempt to decide; "
                             "give it once, or twice for the one same-head replacement")
    parser.add_argument("--row-run-head", help="the reviewed head SHA the executions measured")
    parser.add_argument("--row-run-base", help="the resolved base SHA the executions measured")
    parser.add_argument("--row-run-restarts", type=int, default=0,
                        help="restarts on a changed diagnostic so far, from the recorded head sequence")
    parser.add_argument("--row-run-repo", default=ROW_RUN_DEFAULT_REPOSITORY, help="owner/name of the run")
    args = parser.parse_args(argv)
    if args.row_run_decide is not None:
        if args.smoke or args.base is not None or args.head is not None or args.build_only is not None \
                or args.prebuilt is not None:
            parser.error("--row-run-decide is analysis only: it takes no comparison, build or smoke option")
        if not args.row_run_execution or len(args.row_run_execution) > 2 \
                or any(not attempt.isdigit() for _record, attempt in args.row_run_execution):
            parser.error("--row-run-decide needs one or two --row-run-execution RUN_RECORD ATTEMPT")
        if not all(sha and re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (args.row_run_head, args.row_run_base)):
            parser.error("--row-run-decide needs --row-run-head and --row-run-base as 40-digit SHAs")
        return args
    if args.row_run_execution or args.row_run_head or args.row_run_base or args.row_run_restarts:
        parser.error("--row-run-execution, --row-run-head, --row-run-base and --row-run-restarts need "
                     "--row-run-decide")
    if args.smoke:
        options = (args.base, args.head, args.scenario, args.runs, args.out, args.counters_runs)
        binding = (args.build_only, args.prebuilt, args.prebuilt_run_id, args.prebuilt_attempt,
                   args.prebuilt_manifest_sha256)
        if any(value is not None for value in options + binding) or args.short or args.laps or args.alloc \
                or args.keep or args.counters or args.require_base or args.laps_scenario \
                or args.laps_runs is not None:
            parser.error("--smoke takes no comparison option")
    elif args.base is None or args.head is None:
        parser.error("a comparison needs --base and --head (or run --smoke)")
    elif args.laps and args.laps_scenario:
        parser.error("--laps-scenario restricts the laps set; it is an error with --laps")
    elif args.laps_runs is not None and not (args.laps or args.laps_scenario):
        parser.error("--laps-runs needs --laps or --laps-scenario")
    elif args.counters_runs is not None and not args.counters:
        parser.error("--counters-runs needs --counters")
    elif args.build_only is not None:
        measured = (args.prebuilt, args.prebuilt_run_id, args.prebuilt_attempt, args.prebuilt_manifest_sha256,
                    args.scenario, args.runs, args.counters_runs, args.out, args.laps_scenario, args.laps_runs)
        if any(value is not None for value in measured) or args.short or args.laps or args.counters or args.keep:
            parser.error("--build-only measures nothing: it takes no --prebuilt*, run, counters, --keep or --out "
                         "option (only --alloc)")
        if not args.require_base:
            parser.error("--build-only needs --require-base: a manifest always holds both refs")
    else:
        binding = (args.prebuilt_run_id, args.prebuilt_attempt, args.prebuilt_manifest_sha256)
        if args.prebuilt is not None and any(value is None for value in binding):
            parser.error("--prebuilt needs --prebuilt-run-id, --prebuilt-attempt and --prebuilt-manifest-sha256")
        if args.prebuilt is None and any(value is not None for value in binding):
            parser.error("--prebuilt-run-id, --prebuilt-attempt and --prebuilt-manifest-sha256 need --prebuilt")
        if args.prebuilt is not None and not (args.prebuilt_run_id.isdigit() and args.prebuilt_attempt.isdigit()
                                              and _HEX_SHA256.fullmatch(args.prebuilt_manifest_sha256)):
            parser.error("--prebuilt-run-id and --prebuilt-attempt are numbers; --prebuilt-manifest-sha256 is 64 "
                         "lowercase hex digits")
    return args


def use_utf8_output() -> None:
    """Write stdout and stderr as UTF-8, whatever the console's legacy code page.

    The tables use `≤` and `–`; a Windows runner's cp1252 console cannot encode them, and a failed print
    after comparison.md is written would turn a finished comparison into exit 1.
    """
    for stream in (sys.stdout, sys.stderr):
        # When: a test redirects a stream to StringIO, which has no encoding to change.
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")


def main(argv: Sequence[str] | None = None) -> int:
    """Run the smoke or a comparison and return its exit code."""
    use_utf8_output()
    args = parse_args(argv)
    if args.row_run_decide is not None:
        return row_run_decide_main(args)
    if args.smoke:
        return smoke_main(os.environ)
    return compare_main(args)


if __name__ == "__main__":
    raise SystemExit(main())
