"""Perf sessions."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import json
import os
import time
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_latency_schema
import perf_settings


# --- Process table and session cleanup ------------------------------------------------------

# How long cleanup keeps signalling a session's members before it reports them as survivors.
CLEANUP_BOUND_S = 5.0
CLEANUP_PAUSE_S = 0.05
# How long the anchor may take to disappear after its SIGKILL.
ANCHOR_EXIT_BOUND_S = 5.0
# Linux reports boot time in whole seconds, so a computed start time can read up to a second early.
START_TOLERANCE_S = 1.0


def own_pid() -> int:
    """This script's own process id, which no kill or validation may target.

    Every guard reads it here, so tests can pin it away from the made-up process ids they use.
    """
    return os.getpid()


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
    if not ((isinstance(named, str) and named == role) or (perf_latency_schema._is_int(named) and str(named) == role)):
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
    if not ((isinstance(named, str) and named == role) or (perf_latency_schema._is_int(named) and str(named) == role)):
        raise ValueError(f"program record does not name role {role!r}")
    program_pid, tty = data.get("program_pid"), data.get("tty")
    if not _positive_pid(program_pid):
        raise ValueError("program record needs a positive program_pid")
    if not isinstance(tty, str):
        raise ValueError("program record tty is not a string")
    return ProgramRecord(role, program_pid, tty)


def validate_program(record: ProgramRecord, table, harness_pid: int | None, launch_unix_s: float, *,
                     harness_command: str = perf_settings.HARNESS_EXAMPLE + ".exe") -> tuple[AckedProgram | None, str | None]:
    """Validate a role program before it is acknowledged; return its identity or the reason it fails.

    The process is alive, runs the harness image (Windows names compare without case), its parent
    is the harness, and it was created after this run's launch.
    """
    label = f"program {record.role}"
    pid = record.program_pid
    if table is None or harness_pid is None or pid in (harness_pid, own_pid()):
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
