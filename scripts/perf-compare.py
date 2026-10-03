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

The module imports on every host. POSIX-only calls (`os.getsid`,
`os.killpg`, `signal.SIGKILL`) and the macOS libproc binding are resolved
inside the code paths that need them, so the tests drive fakes everywhere.
"""

from __future__ import annotations

import argparse
import calendar
from dataclasses import asdict, dataclass, field
import errno
import hashlib
import importlib.util
import json
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
from typing import Callable, Iterable, Mapping, Sequence

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


def make_process_table():
    """Return this host's process table, or None where runs are not supported."""
    if sys.platform == "darwin":
        return MacProcessTable()
    if sys.platform.startswith("linux"):
        return LinuxProcessTable()
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
    return MemorySample(unix_s, resident_bytes, int(renderer), int(session))


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


def memory_at(samples: Sequence[MemorySample], unix_s: float) -> MemorySample | None:
    """Return the latest sample at or before a checkpoint, or None when memory is unavailable."""
    earlier = [sample for sample in samples if sample.unix_s <= unix_s]
    return earlier[-1] if earlier else None


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


def _monitor_ok(monitor: object) -> bool:
    """A measurement display gives its name or null, its refresh in millihertz or null, and its scale factor."""
    return (isinstance(monitor, dict)
            and all(key in monitor for key in ("name", "refresh_rate_millihertz", "scale_factor"))
            and (monitor["name"] is None or isinstance(monitor["name"], str))
            and (monitor["refresh_rate_millihertz"] is None or _is_int(monitor["refresh_rate_millihertz"]))
            and _is_number(monitor["scale_factor"]))


def _checkpoint_ok(point: object) -> bool:
    """A checkpoint names its index, label and time; its footprint file is a path or null."""
    return (isinstance(point, dict) and _is_int(point.get("index")) and isinstance(point.get("label"), str)
            and _is_number(point.get("unix_s"))
            and (point.get("footprint_file") is None or isinstance(point.get("footprint_file"), str)))


# Each phase field's check; a field the result leaves out is a metric the harness does not have.
_PHASE_FIELDS = {
    "cpu_user_s": _is_number, "cpu_system_s": _is_number, "presented_frames": _is_int,
    "redraw_requested": _is_int, "dispatch_ms": _numbers, "present_interval_ms": _numbers,
    "allocations_per_frame": lambda value: value is None or _numbers(value),
}


def validate_result(data: object, harness_hash: str, process_exit_code: int | None) -> list[str]:
    """Check result.json against schema version 1; an empty list means it can be read.

    A result whose `managed` is not true, or whose harness hash differs from the one this
    script passed, is a schema failure: it is a standalone run or another harness's run.
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
    latency = data.get("latency")
    if latency is not None and not (isinstance(latency, dict) and latency_values(latency) is not None
                                    and _is_int(latency.get("attributed")) and _is_int(latency.get("total"))
                                    and "coverage" in latency
                                    and (latency["coverage"] is None or _is_number(latency["coverage"]))):
        problems.append("latency needs samples, attributed, total and coverage")
    throughput = data.get("throughput")
    if throughput is not None and not (isinstance(throughput, dict) and _is_int(throughput.get("bytes"))
                                       and _is_number(throughput.get("seconds"))):
        problems.append("throughput needs bytes and seconds")
    if data.get("uncover_ms") is not None and not _is_number(data.get("uncover_ms")):
        problems.append("uncover_ms is not a number")
    if data.get("scrollback_rows_retained") is not None and not _is_int(data.get("scrollback_rows_retained")):
        problems.append("scrollback_rows_retained is not an integer")
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
OTHER_INSTANCE_COMMANDS = frozenset(("sonicterm-mac", "sonicterm-linux"))
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
BUILD_TIMEOUT_S = 3600
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


def build_argv(example: str, release: bool) -> tuple[str, ...]:
    """Return the locked build of one harness example; Cargo's JSON messages name the built binary."""
    profile = ("--release",) if release else ()
    return ("cargo", "build", "--locked", *profile, "-p", "sonicterm-app", "--example", example,
            "--message-format=json-render-diagnostics")


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
        scenarios.append(Scenario(entry["id"], tuple(variants), entry["title"], entry["timeout_s"],
                                  entry["short_timeout_s"]))
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


def harness_argv(binary: Path, scenario_id: str, variant: str, harness_hash: str, scratch: Path, *,
                 short: bool = False, laps: bool = False) -> tuple[str, ...]:
    """Return one managed harness run's command line."""
    flags = (("--short",) if short else ()) + (("--laps",) if laps else ())
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


def run_timeout_s(scenario: Scenario, smoke: bool) -> int:
    """Return a run's run_step bound: the scenario timeout plus a margin, capped in the smoke."""
    if smoke:
        return min(scenario.short_timeout_s + RUN_MARGIN_S, SMOKE_RUN_CAP_S)
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
            self._sessions(final=False)
            self._checkpoints()
        if (self.context.kill_at_go and not self.deadline["sent"] and self.deadline["problem"] is None
                and (self.context.scratch / "go" / "0").exists()):
            self._deadline_kill()

    def final_scan(self) -> None:
        """After the run: read the records once more; one that never parsed, or was never acknowledged, fails the run."""
        if self.harness_pid is None:
            self.harness_pid = read_pid_file(self.context.scratch / "harness.pid")
        self._sessions(final=True)
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
        """End the run as a step deadline does: one group SIGKILL of the accepted harness leader.

        The pid and start token accepted from harness.pid are rechecked immediately before the
        signal, as each PTY member is, so a pid the launcher reaped and the kernel reused is never signalled.
        """
        pid = self.harness_pid
        if pid is None or self.harness_start is None:
            problem = "the harness identity was never accepted, so nothing was signalled"
        else:
            problem = validate_harness_leader(self.context.table, pid, self.context.launch_unix_s,
                                              self.context.harness_command, self.harness_start)
        self.deadline.update(pid=pid, problem=problem)
        if problem is None:
            outcome = self.context.table.kill_group(pid)
            self.deadline["sent"] = outcome == "sent"
            if outcome != "sent":
                self.deadline["problem"] = f"group SIGKILL of {pid} was {outcome}"


class FrontSampler:
    """Take front-application samples, append each raw command to the evidence log and print each form once."""

    def __init__(self, log_path: Path, run: Callable[[Sequence[str], int], CommandRecord],
                 printed_forms: set[str]) -> None:
        self.log_path = log_path
        self.run = run
        self.printed_forms = printed_forms
        self.readings: list[FrontReading] = []

    def sample(self) -> FrontReading:
        """Take one sample; the first of each form prints its raw text, so even a passing log shows it."""
        reading = sample_front(self.run)
        append_front_samples(self.log_path, reading.records)
        self.readings.append(reading)
        if reading.kind not in self.printed_forms:
            self.printed_forms.add(reading.kind)
            raw = " | ".join(json.dumps(record.as_json()) for record in reading.records)
            print(f"[perf-compare] lsappinfo sample form={FORM_LABELS[reading.kind]} raw={raw}", flush=True)
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
    smoke: bool = False
    # The smoke's deadline case: ended by a group SIGKILL as soon as `go/0` exists.
    kill_at_go: bool = False
    # The tree that built the binary; the run's cwd, so asset_dir() finds that tree's assets.
    source_root: Path = ROOT


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


# SIGKILL is signal 9 on every host that runs the harness; signal.SIGKILL does not exist on Windows.
SIGKILL_EXIT_CODE = -9


def _deliberately_killed(outcome: RunOutcome) -> bool:
    """Whether the deadline case ended as planned: run_step reaped the harness this script killed at GO.

    run_step reports FAIL with exit -SIGKILL only after it saw the leader exit, found its process
    group empty and reaped it. A TIMEOUT, or no exit status, proves no termination.
    """
    return (outcome.plan.kill_at_go and bool(outcome.deadline.get("sent")) and outcome.status == "FAIL"
            and outcome.exit_code == SIGKILL_EXIT_CODE and outcome.leftover_processes == 0)


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
        cleanup.append(f"{counted} member(s) of the harness's process group outlived it: {outcome.step_detail}")
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
    exit -SIGKILL, an empty group): its result is expected to be missing or partial, and run_step
    and the anchor cleanup prove its teardown instead. Then come the retryable reasons (session
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
                                f"{code}, not its own reap of the SIGKILL (exit {SIGKILL_EXIT_CODE})"]
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
    sampler = FrontSampler(evidence / "front-samples.log", host.front_run, host.printed_forms)
    sampler.sample()  # The baseline, so the first sample after launch has a predecessor.
    launch_unix_s = host.clock()
    watcher = RunWatcher(RunContext(scratch, evidence, launch_unix_s, host.table, host.gate,
                                    host.excluded_sids, plan.kill_at_go, plan.binary.name))
    thread_problems: list[str] = []
    stop = threading.Event()
    threads = [run_periodically(sampler.sample, FRONT_SAMPLE_INTERVAL_S, stop, thread_problems, "front sampler"),
               run_periodically(watcher.poll, WATCH_INTERVAL_S, stop, thread_problems, "run watcher")]
    argv = harness_argv(plan.binary, plan.scenario.id, plan.variant, plan.harness_hash, scratch,
                        short=plan.short, laps=plan.laps)
    step = host.gate.Step("harness", argv, ("macos",), run_timeout_s(plan.scenario, plan.smoke), "local", (), ())
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
    if host.table is None:
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
                schema = validate_result(parsed, plan.harness_hash, exit_code)
            data = parsed if isinstance(parsed, dict) else None
    focus = judge_focus(sampler.readings, watcher.harness_pid, user_session=has_user_session(host.environ))
    for failed in focus.failed:
        print(describe_sample(failed), flush=True)
    for note in focus.notes:
        print(f"[perf-compare] focus: {note}", flush=True)
    outcome = RunOutcome(plan, evidence, step_result.status, step_result.exit_code, data, schema, not_exercised,
                         focus, thread_problems + watcher.problems, cleanup, home, dict(watcher.deadline),
                         read_memory_samples(kept / "logs"),
                         read_render_timing(kept / "logs") if plan.laps else [], watcher.footprints,
                         watcher.harness_pid, step_result.leftover_processes, step_result.detail, font_failures)
    kind, reasons = classify_outcome(outcome)
    _write_json(evidence / "cleanup.json", {
        "settled": cleanup.settled, "signalled": cleanup.signalled, "problems": cleanup.problems,
        "survivors": [asdict(member) for member in cleanup.survivors],
        "sessions": [asdict(session) for session in sessions],
        "unanchored": [asdict(record) for record in unanchored]})
    _write_json(evidence / "home-check.json", {
        "home": str(host.home), "unresolved": bool(home_unresolved),
        "absent_before": before is None, "absent_after": after is None,
        "sentinel_mtime_ns": sentinel_ns, "other_instance_alive": other, "violations": home})
    _write_json(evidence / "footprints.json", watcher.footprints)
    _write_json(evidence / "outcome.json", {
        "kind": kind, "reasons": reasons, "side": plan.side, "scenario": plan.scenario.id, "variant": plan.variant,
        "argv": list(argv), "status": step_result.status, "exit_code": step_result.exit_code,
        "launch_unix_s": launch_unix_s, "harness_pid": watcher.harness_pid, "deadline": watcher.deadline,
        "focus_problems": focus.problems, "focus_notes": focus.notes, "watcher_problems": outcome.watcher_problems,
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


def run_metrics(outcome: RunOutcome) -> dict[tuple[str, str, str], object]:
    """Extract one valid run's metrics, keyed (name, unit, kind).

    `frame` metrics hold that run's samples, pooled across runs; `run` and `footprint` metrics hold
    one value per run. A field the result lacks yields no key, so it prints `n/a`, never zero.
    """
    result = outcome.result or {}
    metrics: dict[tuple[str, str, str], object] = {}
    for phase in result.get("phases") or []:
        name = phase.get("name")
        start, end = phase.get("start_unix_s"), phase.get("end_unix_s")
        wall_s = end - start if _is_number(start) and _is_number(end) else None
        if wall_s is not None:
            metrics[(f"{name} wall", "s", "run")] = wall_s
        if wall_s and _is_int(phase.get("presented_frames")):
            metrics[(f"{name} presented frames", "fps", "run")] = phase["presented_frames"] / wall_s
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
    for point in result.get("checkpoints") or []:
        sample = memory_at(outcome.memory, point["unix_s"]) if _checkpoint_ok(point) else None
        if sample is None:
            continue  # No memory line yet: memory is unavailable for this checkpoint, not a failure.
        metrics[(f"{point['label']} renderer_total_bytes", "MiB", "run")] = sample.renderer_total_bytes / MIB
        if sample.process_resident_bytes is not None:
            metrics[(f"{point['label']} process_resident_bytes", "MiB", "run")] = sample.process_resident_bytes / MIB
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
            summary = None if side.blocked else run_summary(side_values)
            if summary is None:
                cells.append(_missing_cell(side))
                figures.append(None)
                continue
            cell = f"{summary.median:.2f} ({summary.minimum:.2f}–{summary.maximum:.2f})"
            # A footprint can fail without failing its run, so its cell says how many runs have it.
            if kind == "footprint" or summary.runs < len(side.outcomes):
                cell += f", {summary.runs}/{len(side.outcomes)} runs"
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
    return rows


def laps_rows(label: str, base: SideRuns, head: SideRuns) -> list[list[str]]:
    """Rows of the separate laps table: each render_timing lap pooled, with its per-run spread."""
    return _rows(label, base, head, lap_metrics, None)


def render_table(rows: Iterable[Sequence[str]]) -> str:
    """Render rows as the PR's Markdown table; a `|` or newline in a cell cannot break it."""
    def cell(text: str) -> str:
        return str(text).replace("|", "\\|").replace("\n", " ")
    return TABLE_HEADER + "".join("| " + " | ".join(cell(text) for text in row) + " |\n" for row in rows)


# --- The smoke ------------------------------------------------------------------------------

# Each case is (scenario, ended at GO); the third ends S1 exactly as a step deadline ends a run.
SMOKE_CASES = (("S1", False), ("S3", False), ("S1", True))
# The gate step allows 2700 s: a cold debug build, then at most 4 bounded runs of each case.
SMOKE_BUILD_TIMEOUT_S = 1500
EVIDENCE_PREFIX = "sonicterm-perf-evidence-"


def smoke_cases(scenarios: Mapping[str, Scenario], binary: Path, harness_hash: str,
                run_case: Callable[[RunPlan, Path], RunOutcome], evidence: Path) -> tuple[int, list[str]]:
    """Run the smoke's cases: exit 1 at once on a failure, 3 when a case has no valid exercised run, else 0.

    Only an occlusion is retried, at most RETRY_LIMIT times; no timing is asserted.
    """
    missing = sorted({scenario_id for scenario_id, _kill in SMOKE_CASES
                      if scenario_id not in scenarios or "default" not in scenarios[scenario_id].variants})
    if missing:
        return EXIT_FAIL, [f"the harness lists no default variant of {', '.join(missing)}"]
    blocked = []
    for scenario_id, kill_at_go in SMOKE_CASES:
        name = scenario_id + ("-deadline" if kill_at_go else "")
        plan = RunPlan(scenarios[scenario_id], "default", "smoke", binary, harness_hash, short=True,
                       smoke=True, kill_at_go=kill_at_go, source_root=ROOT)
        reasons: list[str] = []
        for attempt in range(1, RETRY_LIMIT + 2):
            kind, reasons = classify_outcome(run_case(plan, evidence / f"{name}-{attempt}"))
            verdict = smoke_verdict(kind)
            print(f"[perf-smoke] {name} attempt {attempt}: {kind}" + (f": {'; '.join(reasons)}" if reasons else ""),
                  flush=True)
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
    step = gate.Step("list-scenarios", (str(binary), "--list"), ("macos",), LIST_TIMEOUT_S, "local", (), ())
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
                Path.home() / ".sonicterm", Path(tempfile.gettempdir()), set(), dict(os.environ),
                excluded_sids=excluded)


def run_smoke(evidence: Path) -> tuple[int, list[str]]:
    """Build the current tree's debug harness (no worktree), list its scenarios and run the smoke's cases."""
    gate = load_gate()
    problem = gate_problem(gate)
    if problem:
        return EXIT_FAIL, [problem]
    step = gate.Step("build-perf_scenarios", build_argv(HARNESS_EXAMPLE, release=False), ("macos",),
                     SMOKE_BUILD_TIMEOUT_S, "local", ("rust", "native"), ())
    build = gate.run_step(step, 1, ROOT, evidence, dict(os.environ))
    build_log = read_log(build.log_path)
    binary = artifact_executable(build_log, HARNESS_EXAMPLE)
    if build.status != "PASS" or binary is None:
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
    return smoke_cases({scenario.id: scenario for scenario in scenarios}, binary, digest,
                       lambda plan, case_evidence: execute_run(plan, host, case_evidence), evidence)


def smoke_main(environ: Mapping[str, str], runner: Callable[[Path], tuple[int, list[str]]] | None = None) -> int:
    """The `macos-perf-smoke` step: export the evidence directory, run the smoke, and keep the evidence on failure."""
    if sys.platform != "darwin":
        print("[perf-smoke] BLOCKED: the harness, lsappinfo and footprint run only on macOS", flush=True)
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
    if code == EXIT_PASS:
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


def run_set(label: str, plans: Mapping[str, RunPlan], base_blocked: str | None, runs: int,
            run_case: Callable[[RunPlan, Path], RunOutcome], evidence: Path, set_name: str = "timed",
            display: DisplayReference | None = None) -> SetResult:
    """Run one set A B B A until each side has `runs` valid runs.

    An invalid run is retried, at most RETRY_LIMIT times per side. A grid that differs from
    the first valid run's makes the pair invalid, and so does a display that differs from
    `display`, the comparison's reference, in any field both reported: name, refresh rate or
    scale. Only a field a run did not report goes unchecked. A base that cannot build or run is
    `blocked` and the head still runs; a head that cannot is blocked, and one that exhausts
    its retries fails. A schema failure, a refusal or an unresolved cleanup stops the comparison.
    """
    sides = {side: SideRuns() for side in SIDES}
    result = SetResult(label, set_name, sides["base"], sides["head"])
    reasons: dict[str, list[str]] = {side: [] for side in SIDES}
    schedule = AbbaSchedule(runs)
    if base_blocked:
        sides["base"].blocked = base_blocked
        schedule.retire("base")
    reference_grid = None
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
            if reference_grid is not None and grid != reference_grid:
                kind, why = "grid", [f"grid {grid} differs from the pair's {reference_grid}"]
            elif (measured is not None and display.monitor is not None
                  and display_differences(display.monitor, measured)):
                fields = ", ".join(display_differences(display.monitor, measured))
                kind, why = "display", [f"display {describe_display(measured)} differs from the comparison's "
                                        f"{describe_display(display.monitor)} in {fields}"]
            else:
                # Only a run that passed both checks sets the grid or teaches the display reference.
                if reference_grid is None:
                    reference_grid = grid
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


def comparison_exit(results: Iterable[SetResult]) -> int:
    """Exit 1 when a head set failed, 3 when the head was blocked, else 0; a blocked base is reported, not failed."""
    results = list(results)
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


def comparison_document(rows: Sequence[Sequence[str]], lap_rows: Sequence[Sequence[str]],
                        alloc_rows: Sequence[Sequence[str]], host_lines: Sequence[str],
                        detail_lines: Sequence[str]) -> str:
    """Assemble comparison.md: the PR table, the laps and allocation tables when run, the host block and details."""
    parts = ["## Performance comparison\n\n" + render_table(rows)]
    if lap_rows:
        parts.append("### Laps (`--laps` runs, never pooled with timed runs)\n\n" + render_table(lap_rows))
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


def gate_problem(gate) -> str | None:
    """Explain why run_step cannot bound and reap children on this interpreter, or return None."""
    problem = gate.sigchld_problem()
    if problem:
        return problem
    if not gate.leader_watches():
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


def comparison_command(args: argparse.Namespace) -> str:
    """Reconstruct the invocation, so the details block records how the table was produced."""
    words = ["python3", "scripts/perf-compare.py", "--base", args.base, "--head", args.head]
    for value in args.scenario or ["all"]:
        words += ["--scenario", value]
    words += ["--runs", str(args.runs or DEFAULT_RUNS)]
    words += [flag for flag, chosen in (("--laps", args.laps), ("--alloc", args.alloc), ("--keep", args.keep))
              if chosen]
    return " ".join(words)


def _compare(args: argparse.Namespace, gate, out: Path, work: Path, worktrees: Worktrees,
             host_run: Callable) -> int:
    """Build both refs with the head's harness, run every selected set and write comparison.md.

    compare_main owns the worktrees' removal, which runs whatever this raises.
    """
    runs = args.runs or DEFAULT_RUNS
    shas = {side: _resolve_sha(host_run, ref) for side, ref in (("base", args.base), ("head", args.head))}
    trees = {"head": worktrees.create("head", shas["head"])}
    trees["base"] = worktrees.create("base", shas["base"])
    overlay_harness(trees["head"], trees["base"])
    hashes = {side: tree_harness_hash(trees[side]) for side in SIDES}
    if hashes["base"] != hashes["head"]:
        raise ValueError(f"harness hashes differ after the overlay: base {hashes['base']}, head {hashes['head']}")
    digest = hashes["head"]
    print(f"[perf-compare] base={shas['base']} head={shas['head']} harness_hash={digest}", flush=True)
    examples = (HARNESS_EXAMPLE,) + ((ALLOC_EXAMPLE,) if args.alloc else ())
    builds: dict[str, dict[str, object]] = {side: {} for side in SIDES}
    index = 0
    # One build at a time, each with its ref's own target directory; the head goes first, so a head
    # that cannot build fails before the base's build time is spent.
    for side in ("head", "base"):
        environ = dict(os.environ, CARGO_TARGET_DIR=str(work / f"target-{side}"))
        for example in examples:
            index += 1
            step = gate.Step(f"build-{side}-{example}", build_argv(example, release=True), ("macos",),
                             BUILD_TIMEOUT_S, "local", ("rust", "native"), ())
            result = gate.run_step(step, index, trees[side], out, environ)
            text = read_log(result.log_path)
            binary = artifact_executable(text, example)
            problem = asset_problem(binary, trees[side]) if result.status == "PASS" and binary else None
            if result.status == "PASS" and binary is not None and problem is None:
                builds[side][example] = binary
            elif problem is not None and side == "head":
                raise ValueError(f"the head cannot run {example}: {problem}")
            elif problem is not None:
                builds[side][example] = f"base cannot run {example}: {problem}"
            elif side == "head":
                raise ValueError(f"the head cannot build {example}: {build_error(text)} (log {result.log_path})")
            else:
                builds[side][example] = f"base cannot build {example}: {build_error(text)}"
    scenarios = list_scenarios(gate, builds["head"][HARNESS_EXAMPLE], out, index + 1)
    by_id = {scenario.id: scenario for scenario in scenarios}
    selected = select_scenarios(args.scenario or ["all"], scenarios)
    host = production_host(gate)
    sets = [("timed", HARNESS_EXAMPLE, False)]
    if args.laps:
        sets.append(("laps", HARNESS_EXAMPLE, True))
    if args.alloc:
        sets.append(("alloc", ALLOC_EXAMPLE, False))
    results = []
    # One reference for every set, so all valid runs of the comparison share one refresh rate and scale.
    display = DisplayReference()
    for scenario_id, variant in selected:
        label = f"{scenario_id}/{variant}"
        for set_name, example, laps in sets:
            built = {side: builds[side][example] for side in SIDES}
            # A base that did not build is retired before its first slot, so its placeholder path never runs.
            plans = {side: RunPlan(by_id[scenario_id], variant, side,
                                   built[side] if isinstance(built[side], Path) else Path("unbuilt"),
                                   digest, laps=laps, source_root=trees[side]) for side in SIDES}
            base_blocked = built["base"] if isinstance(built["base"], str) else None
            results.append(run_set(label, plans, base_blocked, runs,
                                   lambda plan, evidence: execute_run(plan, host, evidence),
                                   out / "runs" / f"{scenario_id}-{variant}" / set_name, set_name,
                                   display=display))
    timed_rows, lap_rows, alloc_rows = [], [], []
    for result in results:
        if result.set_name == "timed":
            timed_rows.extend(comparison_rows(result.label, result.base, result.head))
        elif result.set_name == "laps":
            lap_rows.extend(laps_rows(result.label, result.base, result.head))
        else:
            alloc_rows.extend(comparison_rows(result.label, result.base, result.head, _allocation_metric))
    outputs = {}
    for name, argv in HOST_COMMANDS.items():
        record = host_run(argv, HOST_COMMAND_TIMEOUT_S)
        outputs[name] = "" if _command_failure(record) else record.stdout
    run_template = harness_argv(Path("<binary>"), "<ID>", "<variant>", digest, Path("<new scratch path>"))
    details = [f"- Base: `{args.base}` = `{shas['base']}`", f"- Head: `{args.head}` = `{shas['head']}`",
               f"- Harness hash (both trees): `{digest}`", f"- Command: `{comparison_command(args)}`",
               f"- Builds: `{' '.join(build_argv(HARNESS_EXAMPLE, release=True))}` in each worktree, "
               f"one CARGO_TARGET_DIR per ref", f"- Runs: `{' '.join(run_template)}`",
               f"- Evidence: `{out}`", "", "Raw logs:", ""]
    for result in results:
        for side, evidence, kind, _reasons in result.attempts:
            details.append(f"- {result.label} {result.set_name} {side} {kind}: `{evidence}/01-harness.log`")
    document = comparison_document(timed_rows, lap_rows, alloc_rows, host_block(outputs, display.monitor), details)
    (out / "comparison.md").write_text(document, encoding="utf-8")
    print(document, flush=True)
    return comparison_exit(results)


def compare_main(args: argparse.Namespace) -> int:
    """Compare two refs; remove the worktrees and target directories it created unless --keep."""
    if sys.platform != "darwin":
        print("[perf-compare] BLOCKED: the harness, lsappinfo and footprint run only on macOS", flush=True)
        return EXIT_BLOCKED
    gate = load_gate()
    problem = gate_problem(gate)
    if problem:
        print(f"[perf-compare] {problem}", file=sys.stderr)
        return EXIT_FAIL
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}"
    out = (args.out or ROOT / "target" / "perf-compare" / f"out-{stamp}").resolve()
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
    parser.add_argument("--laps", action="store_true",
                        help="also run a --laps set and print its render_timing laps table")
    parser.add_argument("--alloc", action="store_true",
                        help="also build and run perf_scenarios_alloc and print allocations per frame")
    parser.add_argument("--keep", action="store_true", help="keep the worktrees and target directories")
    parser.add_argument("--out", type=Path,
                        help="an empty directory for the evidence and comparison.md "
                             "(default: target/perf-compare/out-<stamp>)")
    args = parser.parse_args(argv)
    if args.smoke:
        options = (args.base, args.head, args.scenario, args.runs, args.out)
        if any(value is not None for value in options) or args.laps or args.alloc or args.keep:
            parser.error("--smoke takes no comparison option")
    elif args.base is None or args.head is None:
        parser.error("a comparison needs --base and --head (or run --smoke)")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    """Run the smoke or a comparison and return its exit code."""
    args = parse_args(argv)
    if args.smoke:
        return smoke_main(os.environ)
    return compare_main(args)


if __name__ == "__main__":
    raise SystemExit(main())
