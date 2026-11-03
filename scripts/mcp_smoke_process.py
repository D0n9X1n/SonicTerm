"""Process custody for the MCP smoke: every process the smoke starts, and everything they start, ends.

Every launch gets its own random token in `SONICTERM_SMOKE_RUN`, set only in that child's
environment, so all its descendants inherit it, including detached and reparented orphans and
programs after `exec`. A process's identity is its pid with its kernel start time only (Linux
`/proc/<pid>/stat` field 22, macOS `ps -o lstart=`), so `exec` keeps it and a reused pid does not
match. A pid is signalled only while its current identity equals the recorded one.

Unix attribution is layered; cleanup kills the union:
1. Tree walk: a tracker thread walks the descendants of the root and of every still-matching
   tracked identity once at launch and every 100 ms, and once more at cleanup.
2. Environment marker: at cleanup, every same-user process with an environment entry exactly equal
   to `SONICTERM_SMOKE_RUN=<token>`. The environment is read structurally, never argv: Linux splits
   `/proc/<pid>/environ` into whole NUL-terminated entries, so this layer alone is race-free there;
   macOS parses sysctl(KERN_PROCARGS2), which skips argc argv strings before the environment. The
   macOS kernel refuses that read (EPERM/EINVAL) for Apple platform binaries such as `/bin/zsh` and
   `/bin/sleep`, so they are "not marked", never an error.
3. Sessions: the session id of every attributed process is recorded (the root leads its own, and
   each pane shell leads one); every same-user process in a recorded session is attributed. A
   session whose id now names a different process (the pid was reused) is dropped.

Not covered (macOS only): a descendant that runs a system binary, leaves its session with `setsid`,
and is reparented to launchd before a tracker walk sees it under a tracked parent cannot be
attributed without private Apple interfaces, and cleanup does not report it. Example: a root that
starts `/bin/sleep` with `start_new_session=True` and exits at once. Reporting such orphans (new,
same-user, parent launchd, own session leader, unattributed) was measured and rejected: on an idle
developer Mac it fired in 8 of 20 five-second windows, every hit a user daemon detaching the same
way. The smoke's own app does not do this: its pane shells stay children of the live app until
cleanup kills it.

Cleanup runs under one lock and is idempotent: it freezes the tree with SIGSTOP, SIGKILLs the root
and its group with no grace period (no termination handler runs, so nothing spawns during
shutdown), then scans, SIGKILLs every attributed live process and rescans until none is left or 3 s
pass. It fails closed: an unreadable process table, a survivor, or a root that will not be reaped
is a problem the caller reports, never a clean result.

Windows: an owned process starts suspended, joins a kill-on-close Job Object (the gate runner's
`Job` from windows-process-job.py), then resumes, so nothing it starts escapes the job. Cleanup
terminates the job and requires its ActiveProcesses to reach 0 within the bound.

`Owned` holds the one lock every cleanup runs under; an entry leaves its registry only once its
death is confirmed, and the watchdog runs the same cleanup and waits for it before exiting.
"""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import time
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence
import uuid

MARKER_NAME = "SONICTERM_SMOKE_RUN"
TRACK_INTERVAL_S = 0.1
# How long killed processes may take to disappear, or a job to empty, before cleanup fails.
VERIFY_S = 3.0
REAP_S = 10.0
TABLE_TIMEOUT_S = 30
DEADLINE_EXIT_CODE = 124
# The tracker records at most this many read errors; one is already a cleanup failure.
MAX_RECORDED_ERRORS = 3
# macOS columns: pid, ppid, uid, state and the five words of lstart. The environment is read
# structurally with sysctl(KERN_PROCARGS2), never from `ps -E` text, which mixes argv and environment.
PS_COLUMNS = "pid=,ppid=,uid=,stat=,lstart="
CTL_KERN = 1
KERN_PROCARGS2 = 49
CREATE_SUSPENDED = 0x00000004
TH32CS_SNAPTHREAD = 0x00000004
THREAD_SUSPEND_RESUME = 0x0002
TABLE_ERRORS = (RuntimeError, OSError, ValueError, subprocess.SubprocessError)


class Row(NamedTuple):
    """One process: parent, state ("Z" is a zombie), start-time identity, owner, session and marker."""

    ppid: int
    state: str
    start: str
    uid: int
    sid: int
    marked: bool


ProcessTable = Mapping[int, Row]
# A table reader takes the launch token to look for, or None when the marker is not needed.
TableReader = Callable[["str | None"], ProcessTable]


def marker_entry(token: str) -> str:
    """The exact environment entry a launch's processes carry."""
    return f"{MARKER_NAME}={token}"


def has_marker(environment: Iterable[bytes], token: str) -> bool:
    """Whether one environment entry is exactly `SONICTERM_SMOKE_RUN=<token>`; argv is never passed here."""
    wanted = marker_entry(token).encode()
    return any(entry == wanted for entry in environment)


def parse_environ(raw: bytes) -> list[bytes]:
    """Split Linux /proc/<pid>/environ into whole NUL-terminated entries; a trailing partial entry is dropped."""
    entries = raw.split(b"\0")
    # The last element is empty when the data ended in NUL; otherwise it was cut short and is not trusted.
    return [entry for entry in entries[:-1] if entry]


def parse_procargs2(raw: bytes) -> list[bytes] | None:
    """The environment entries of a macOS KERN_PROCARGS2 buffer, or None when it cannot be parsed.

    Layout: int32 argc; the exec path, NUL-terminated, then NUL padding; argc NUL-terminated argv
    strings; then NUL-terminated environment strings until an empty string or the end. Only the
    environment is returned, so a token in argv is never a match; an unterminated entry is dropped.
    """
    if len(raw) < 4:
        return None
    argc = int.from_bytes(raw[:4], sys.byteorder, signed=True)
    if argc < 0:
        return None
    position = raw.find(b"\0", 4)
    if position < 0:
        return None
    # Skip the NUL padding after the exec path.
    while position < len(raw) and raw[position] == 0:
        position += 1
    for _argument in range(argc):
        end = raw.find(b"\0", position)
        if end < 0:
            # When: argv itself is cut short, the environment never started.
            return []
        position = end + 1
    environment: list[bytes] = []
    while position < len(raw):
        end = raw.find(b"\0", position)
        if end < 0 or end == position:
            # When: an entry is unterminated (truncated) or empty (the end marker), the environment stops.
            break
        environment.append(raw[position:end])
        position = end + 1
    return environment


def read_procargs2(pid: int) -> bytes | None:
    """Read one macOS process's KERN_PROCARGS2 buffer; None when it is unreadable (system binary, other user)."""
    import ctypes

    libc = ctypes.CDLL(None, use_errno=True)
    mib = (ctypes.c_int * 3)(CTL_KERN, KERN_PROCARGS2, pid)
    size = ctypes.c_size_t(0)
    if libc.sysctl(mib, 3, None, ctypes.byref(size), None, ctypes.c_size_t(0)) != 0 or size.value == 0:
        # When: EPERM, EINVAL or ESRCH, the environment is unreadable for this pid; that is not an error.
        return None
    buffer = ctypes.create_string_buffer(size.value)
    if libc.sysctl(mib, 3, buffer, ctypes.byref(size), None, ctypes.c_size_t(0)) != 0:
        return None
    return buffer.raw[:size.value]


def parse_ps_table(text: str) -> dict[int, Row]:
    """Parse macOS `ps` rows; the session id and the marker are filled in afterwards."""
    table: dict[int, Row] = {}
    for line in text.splitlines():
        fields = line.split()
        if len(fields) < 9 or not (fields[0].isdigit() and fields[1].isdigit() and fields[2].isdigit()):
            continue
        table[int(fields[0])] = Row(int(fields[1]), fields[3], " ".join(fields[4:9]), int(fields[2]), -1, False)
    return table


def parse_proc_stat(text: str) -> tuple[int, int, str, int, str] | None:
    """Parse one Linux /proc/<pid>/stat: (pid, ppid, state, session, starttime); the name may hold ')'."""
    opening, closing = text.find(" ("), text.rfind(")")
    if opening < 0 or closing < opening:
        return None
    head, rest = text[:opening], text[closing + 1:].split()
    # After the name: state, ppid, pgrp, session, ..., field 22 (index 19) the start time in clock ticks.
    if not head.isdigit() or len(rest) < 20 or not (rest[1].isdigit() and rest[3].isdigit() and rest[19].isdigit()):
        return None
    return int(head), int(rest[1]), rest[0], int(rest[3]), rest[19]


def read_proc_table(token: str | None = None, proc: Path = Path("/proc")) -> dict[int, Row]:
    """The Linux process table from /proc; the marker is read from each same-user environ when asked."""
    own_uid = os.getuid()
    table: dict[int, Row] = {}
    for entry in proc.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            parsed = parse_proc_stat((entry / "stat").read_text(encoding="utf-8", errors="replace"))
            uid = entry.stat().st_uid
        except OSError:
            # When: the process exited while the table was read, it is simply not in the table.
            continue
        if parsed is None:
            continue
        marked = False
        if token and uid == own_uid:
            try:
                marked = has_marker(parse_environ((entry / "environ").read_bytes()), token)
            except OSError:
                # When: one pid vanished or refuses its environ, it carries no readable marker.
                marked = False
        table[parsed[0]] = Row(parsed[1], parsed[2], parsed[4], uid, parsed[3], marked)
    return table


def read_process_table(token: str | None = None) -> dict[int, Row]:
    """The host's process table; raises RuntimeError when it cannot be read."""
    if Path("/proc/self/stat").is_file():
        return read_proc_table(token)
    argv = ["ps", "-A", "-o", PS_COLUMNS]
    try:
        completed = subprocess.run(argv, capture_output=True, timeout=TABLE_TIMEOUT_S,
                                   env={**os.environ, "LC_ALL": "C"})
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError(f"cannot run ps: {error}") from None
    if completed.returncode != 0:
        raise RuntimeError(f"ps exited {completed.returncode}: {completed.stderr[:400]!r}")
    table = parse_ps_table(completed.stdout.decode("utf-8", errors="replace"))
    if not table:
        raise RuntimeError("ps listed no processes")
    own_uid = os.getuid()
    # macOS `ps` has no session-id column; getsid works for any pid.
    for pid, row in list(table.items()):
        try:
            row = row._replace(sid=os.getsid(pid))
        except ProcessLookupError:
            # When: the process exited after ps listed it, it is no longer in the table.
            del table[pid]
            continue
        except OSError:
            pass
        if token and row.uid == own_uid:
            raw = read_procargs2(pid)
            environment = parse_procargs2(raw) if raw is not None else None
            row = row._replace(marked=bool(environment) and has_marker(environment, token))
        table[pid] = row
    return table


def descendants(table: ProcessTable, roots: Iterable[int]) -> set[int]:
    """Every process below any of `roots` in `table`, following parent links; the roots excluded."""
    children: dict[int, list[int]] = {}
    for pid, row in table.items():
        children.setdefault(row.ppid, []).append(pid)
    start = set(roots)
    found: set[int] = set()
    pending = list(start)
    while pending:
        for child in children.get(pending.pop(), ()):
            # When: a child is a root or already found (a cycle in a racy table), it is not walked twice.
            if child not in start and child not in found:
                found.add(child)
                pending.append(child)
    return found


def matches(table: ProcessTable, pid: int, start: str) -> bool:
    """Whether `pid` in `table` is still the process recorded with start time `start`."""
    row = table.get(pid)
    return row is not None and row.start == start


def alive(table: ProcessTable, pid: int, start: str) -> bool:
    """Whether the recorded identity is present and not a zombie."""
    return matches(table, pid, start) and not table[pid].state.startswith("Z")


def _signal(send: Callable[[int, int], None], pid: int, number: int) -> None:
    """Send one signal, ignoring a pid that is already gone or not ours."""
    try:
        send(pid, number)
    except (ProcessLookupError, PermissionError):
        pass


class UnixContainment:
    """One owned Unix process and everything attributed to it: tree, marker and sessions."""

    def __init__(self, process, reader: TableReader = read_process_table, token: str | None = None,
                 kill: Callable[[int, int], None] = os.kill,
                 killpg: Callable[[int, int], None] | None = None, verify_s: float = VERIFY_S,
                 own_pid: int | None = None, own_sid: int | None = None, own_uid: int | None = None) -> None:
        self.process = process
        self.reader = reader
        self.token = token
        self.kill = kill
        self.killpg = killpg if killpg is not None else getattr(os, "killpg", kill)
        self.verify_s = verify_s
        self.own_pid = os.getpid() if own_pid is None else own_pid
        self.own_sid = os.getsid(0) if own_sid is None else own_sid
        self.own_uid = os.getuid() if own_uid is None else own_uid
        self.state_lock = threading.Lock()
        self.cleanup_lock = threading.Lock()
        self.identities: set[tuple[int, str]] = set()
        # Session id -> its leader's start time when recorded, or None if the leader was already gone.
        # The root was launched into a new session, so its pid is its session id.
        self.sessions: dict[int, str | None] = {process.pid: None}
        self.root_identity: tuple[int, str] | None = None
        self.table: ProcessTable = {}
        self.read_errors: list[str] = []
        self.read_error_count = 0
        self.stopping = threading.Event()
        self.thread: threading.Thread | None = None
        self.result: list[str] | None = None

    def start(self) -> None:
        """Walk once now, then every TRACK_INTERVAL_S on a daemon thread until cleanup."""
        self.walk()
        self.thread = threading.Thread(target=self._track, daemon=True)
        self.thread.start()

    def _track(self) -> None:
        while not self.stopping.wait(TRACK_INTERVAL_S):
            self.walk()

    def _read(self, token: str | None) -> ProcessTable | None:
        """Read the table, recording a failure; None means this read's outcome is unknown."""
        try:
            return self.reader(token)
        except TABLE_ERRORS as error:
            with self.state_lock:
                self.read_error_count += 1
                if len(self.read_errors) < MAX_RECORDED_ERRORS:
                    self.read_errors.append(f"{type(error).__name__}: {error}")
            return None

    def _session_valid(self, table: ProcessTable, sid: int) -> bool:
        """Whether a recorded session id still names the session we recorded, not a reuse of the pid."""
        if sid <= 1 or sid == self.own_sid or sid not in self.sessions:
            return False
        # When: the root is unreaped its pid, and so its session id, cannot have been reused.
        if sid == self.process.pid and self.process.returncode is None:
            return True
        leader = table.get(sid)
        recorded = self.sessions[sid]
        # A session id is never reused while any member lives; a different leader means it was.
        return leader is None or (recorded is not None and leader.start == recorded)

    def _attribute(self, table: ProcessTable) -> set[int]:
        """Extend the tracked identities and sessions from `table`; return the live attributed pids."""
        with self.state_lock:
            self.table = table
            pid = self.process.pid
            # When: the root is not reaped yet, its pid still names it, so its identity can be recorded.
            if self.root_identity is None and self.process.returncode is None and pid in table:
                self.root_identity = (pid, table[pid].start)
                self.identities.add(self.root_identity)
                self.sessions[pid] = table[pid].start if table[pid].sid == pid else self.sessions.get(pid)
            owned = {known for known, start in self.identities if matches(table, known, start)}
            for candidate, row in table.items():
                if candidate == self.own_pid or row.uid != self.own_uid:
                    continue
                if row.marked or (row.sid in self.sessions and self._session_valid(table, row.sid)):
                    owned.add(candidate)
            owned |= descendants(table, owned)
            owned.discard(self.own_pid)
            for member in owned:
                row = table[member]
                self.identities.add((member, row.start))
                if row.sid > 1 and row.sid != self.own_sid and row.sid not in self.sessions:
                    leader = table.get(row.sid)
                    self.sessions[row.sid] = leader.start if leader is not None else None
            return {member for member in owned if not table[member].state.startswith("Z")}

    def walk(self) -> bool:
        """One tracker walk (tree and sessions); False when the table could not be read."""
        table = self._read(None)
        if table is None:
            return False
        self._attribute(table)
        return True

    def known_pids(self) -> set[int]:
        """Every tracked pid, root included."""
        with self.state_lock:
            return {pid for pid, _start in self.identities}

    def descendant_count(self) -> int:
        """Live tracked processes other than the root in the latest walk; raises if a read failed."""
        with self.state_lock:
            if self.read_errors:
                raise RuntimeError(f"process table unreadable: {self.read_errors[0]}")
            return sum(1 for pid, start in self.identities
                       if (pid, start) != self.root_identity and alive(self.table, pid, start))

    def _signal_root(self, number: int) -> None:
        # When: the root is reaped, its pid may be reused, so neither it nor its group id is signalled.
        if self.process.returncode is None:
            _signal(self.killpg, self.process.pid, number)
            _signal(self.kill, self.process.pid, number)

    def cleanup(self) -> list[str]:
        """Kill the root and everything attributed to it, then verify; return the problems, empty when clean.

        Idempotent: a second call returns the first call's problems without signalling anything.
        """
        with self.cleanup_lock:
            if self.result is not None:
                return list(self.result)
            self.stopping.set()
            if self.thread is not None:
                self.thread.join(timeout=TABLE_TIMEOUT_S + 5)
            problems: list[str] = []
            # Freeze the tree so nothing forks between the final scan and the kill.
            stop = getattr(signal, "SIGSTOP", signal.SIGTERM)
            self._signal_root(stop)
            table = self._read(self.token)
            if table is not None:
                for pid in self._attribute(table):
                    _signal(self.kill, pid, stop)
            self._signal_root(signal.SIGKILL)
            problems.extend(self._kill_and_verify())
            # The root is reaped last: until then its pid, and so its session id, cannot be reused.
            try:
                self.process.wait(timeout=REAP_S)
            except subprocess.TimeoutExpired:
                problems.append(f"pid {self.process.pid} was not reaped within {REAP_S:.0f} s of SIGKILL")
            with self.state_lock:
                if self.read_error_count:
                    problems.insert(0, f"process table unreadable {self.read_error_count} time(s) for "
                                       f"pid {self.process.pid}: {'; '.join(self.read_errors)}")
            self.result = problems
            return list(problems)

    def _kill_and_verify(self) -> list[str]:
        """Scan, SIGKILL every attributed live process, and rescan until none is left or verify_s passes."""
        end = time.monotonic() + self.verify_s
        while True:
            table = self._read(self.token)
            living: set[int] = set()
            if table is not None:
                living = self._attribute(table)
                for pid in living:
                    _signal(self.kill, pid, signal.SIGKILL)
                if not living:
                    return []
            if time.monotonic() >= end:
                if table is None:
                    return [f"cannot verify that the processes of pid {self.process.pid} died: "
                            f"the process table is unreadable"]
                return [f"attributed processes still alive after SIGKILL: {sorted(living)}"]
            time.sleep(0.05)


def _windows_job_module():
    """Load the gate runner's Windows Job wrapper (Windows only)."""
    spec = importlib.util.spec_from_file_location("sonicterm_windows_process_job",
                                                  Path(__file__).resolve().parent / "windows-process-job.py")
    if spec is None or spec.loader is None:
        raise ImportError("cannot load scripts/windows-process-job.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def resume_process(pid: int) -> None:
    """Resume every thread of a process started with CREATE_SUSPENDED (Windows only)."""
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class ThreadEntry(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("usage", wintypes.DWORD), ("thread_id", wintypes.DWORD),
                    ("owner_pid", wintypes.DWORD), ("base_priority", wintypes.LONG),
                    ("delta_priority", wintypes.LONG), ("flags", wintypes.DWORD)]

    snapshot_api = kernel32.CreateToolhelp32Snapshot
    snapshot_api.restype, snapshot_api.argtypes = wintypes.HANDLE, [wintypes.DWORD, wintypes.DWORD]
    first_api, next_api = kernel32.Thread32First, kernel32.Thread32Next
    for api in (first_api, next_api):
        api.restype, api.argtypes = wintypes.BOOL, [wintypes.HANDLE, ctypes.POINTER(ThreadEntry)]
    open_api = kernel32.OpenThread
    open_api.restype, open_api.argtypes = wintypes.HANDLE, [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    resume_api = kernel32.ResumeThread
    resume_api.restype, resume_api.argtypes = wintypes.DWORD, [wintypes.HANDLE]
    close_api = kernel32.CloseHandle
    close_api.restype, close_api.argtypes = wintypes.BOOL, [wintypes.HANDLE]

    snapshot = snapshot_api(TH32CS_SNAPTHREAD, 0)
    if not snapshot or snapshot == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    resumed = 0
    try:
        entry = ThreadEntry()
        entry.size = ctypes.sizeof(ThreadEntry)
        more = first_api(snapshot, ctypes.byref(entry))
        while more:
            if entry.owner_pid == pid:
                handle = open_api(THREAD_SUSPEND_RESUME, False, entry.thread_id)
                if not handle:
                    raise ctypes.WinError(ctypes.get_last_error())
                try:
                    if resume_api(handle) == 0xFFFFFFFF:
                        raise ctypes.WinError(ctypes.get_last_error())
                    resumed += 1
                finally:
                    close_api(handle)
            more = next_api(snapshot, ctypes.byref(entry))
    finally:
        close_api(snapshot)
    if resumed == 0:
        raise OSError(f"process {pid} has no thread to resume")


class WindowsContainment:
    """One owned Windows process inside its own kill-on-close Job Object."""

    def __init__(self, process, job, verify_s: float = VERIFY_S) -> None:
        self.process = process
        self.job = job
        self.verify_s = verify_s
        self.cleanup_lock = threading.Lock()
        self.result: list[str] | None = None

    def descendant_count(self) -> int:
        """Active job processes other than the root; raises OSError when the job cannot be queried."""
        active = self.job.accounting()["active_processes"]
        return max(0, active - (1 if self.process.poll() is None else 0))

    def cleanup(self) -> list[str]:
        """TerminateJobObject, then require ActiveProcesses == 0 within VERIFY_S; idempotent."""
        with self.cleanup_lock:
            if self.result is not None:
                return list(self.result)
            problems: list[str] = []
            try:
                self.job.terminate()
            except OSError as error:
                problems.append(f"TerminateJobObject failed for pid {self.process.pid}: {error}")
            end = time.monotonic() + self.verify_s
            while True:
                try:
                    active = self.job.accounting()["active_processes"]
                except OSError as error:
                    problems.append(f"job accounting unreadable for pid {self.process.pid}: {error}")
                    break
                if active == 0:
                    break
                if time.monotonic() >= end:
                    problems.append(f"{active} process(es) still active in pid {self.process.pid}'s job")
                    break
                time.sleep(0.05)
            try:
                self.process.wait(timeout=REAP_S)
            except subprocess.TimeoutExpired:
                problems.append(f"pid {self.process.pid} was not reaped after its job was terminated")
            try:
                # Closing the kill-on-close handle also ends anything a failed check left behind.
                self.job.close()
            except OSError as error:
                problems.append(f"closing pid {self.process.pid}'s job failed: {error}")
            self.result = problems
            return list(problems)


def launch(argv: Sequence[str], *, reader: TableReader | None = None, verify_s: float = VERIFY_S, **options):
    """Start `argv` contained, with its own marker token in its environment only.

    Unix: a new session tracked by tree walk, marker and session. Windows: suspended, joined to a
    kill-on-close job, then resumed.
    """
    token = uuid.uuid4().hex
    environment = dict(options.pop("env", None) or os.environ)
    environment[MARKER_NAME] = token
    if os.name == "nt":
        job = _windows_job_module().Job()
        # No CREATE_NEW_PROCESS_GROUP: that flag makes the app and every descendant ignore Ctrl+C,
        # so a pane program could never be interrupted. The job contains the tree instead.
        flags = options.pop("creationflags", 0) | CREATE_SUSPENDED
        try:
            process = subprocess.Popen(list(argv), creationflags=flags, env=environment, **options)
        except BaseException:
            job.close()
            raise
        try:
            # The process cannot run, so cannot start children, until it belongs to the job.
            job.assign(process)
            resume_process(process.pid)
        except BaseException:
            process.kill()
            process.wait(timeout=REAP_S)
            job.close()
            raise
        return WindowsContainment(process, job, verify_s)
    options["start_new_session"] = True
    process = subprocess.Popen(list(argv), env=environment, **options)
    containment = UnixContainment(process, reader or read_process_table, token, verify_s=verify_s)
    containment.start()
    return containment


class Owned:
    """Every contained process the smoke started, under the one lock all cleanup runs under."""

    def __init__(self, reader: TableReader | None = None, verify_s: float = VERIFY_S) -> None:
        self.lock = threading.Lock()
        self.entries: list = []
        self.closing = False
        self.reader = reader
        self.verify_s = verify_s

    def launch(self, argv: Sequence[str], **options):
        """Start and register a contained process; refused once the watchdog's cleanup has begun."""
        with self.lock:
            if self.closing:
                raise RuntimeError("cleanup has started; no process may be launched")
            containment = launch(argv, reader=self.reader, verify_s=self.verify_s, **options)
            self.entries.append(containment)
            return containment

    def cleanup(self, containment=None) -> list[str]:
        """Clean one entry, or every entry newest first; return the problems, empty when all are dead."""
        with self.lock:
            targets = list(reversed(self.entries)) if containment is None else [containment]
            problems: list[str] = []
            for target in targets:
                try:
                    found = target.cleanup()
                except Exception as error:
                    found = [f"cleanup of pid {target.process.pid} raised {type(error).__name__}: {error}"]
                problems.extend(found)
                # Lifecycle: an entry leaves the registry only once its death is confirmed.
                if not found and target in self.entries:
                    self.entries.remove(target)
            return problems

    def expire(self, report: Callable[[list[str]], None], exit_function: Callable[[int], None] = os._exit,
               code: int = DEADLINE_EXIT_CODE) -> None:
        """The watchdog: run the same locked cleanup, waiting for any cleanup in progress, then exit."""
        self.closing = True
        problems = self.cleanup()
        report(problems)
        exit_function(code)
