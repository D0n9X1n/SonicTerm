"""Perf watcher."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import json
import os
from pathlib import Path
import platform
import re
import threading
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_focus
import perf_latency_schema
import perf_scenarios
import perf_sessions
import perf_settings
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
            if any(perf_latency_schema._is_int(node.get(key)) for key in FOOTPRINT_KEYS):
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
    return next(chosen[0][key] for key in FOOTPRINT_KEYS if perf_latency_schema._is_int(chosen[0].get(key)))


def command_matches(command: str, binary_name: str) -> bool:
    """Whether a kernel command name names a binary; Linux keeps 15 bytes of the name and macOS 16."""
    return bool(command) and len(command) >= min(15, len(binary_name)) and binary_name.startswith(command)


def validate_harness_leader(table, pid: int | None, launch_unix_s: float,
                            expected_command: str = perf_settings.HARNESS_EXAMPLE, expected_start: str | None = None) -> str | None:
    """Check that a pid is this run's harness: alive, leading its session and group, started after
    launch, running the binary this script launched and, given `expected_start`, still the accepted process."""
    if table is None or pid is None or pid <= 1 or pid == perf_sessions.own_pid():
        return f"harness pid {pid} cannot be validated"
    try:
        info = table.read(pid)
    except perf_sessions.ProcessUnreadable as error:
        return f"harness pid {pid} is unreadable: {error}"
    if info is None:
        return f"harness pid {pid} is not alive"
    if expected_start is not None and info.start != expected_start:
        return (f"harness pid {pid} now names another process (start {info.start}, accepted {expected_start}); "
                f"its identity changed, so it was not signalled")
    if not info.sid == info.pgid == pid:
        return f"harness pid {pid} does not lead its session and process group"
    if info.start_unix_s < launch_unix_s - perf_sessions.START_TOLERANCE_S:
        return f"harness pid {pid} started before this run's launch"
    if not command_matches(info.command, expected_command):
        return f"harness pid {pid} runs {info.command!r}, not {expected_command}"
    return None


def validate_windows_harness(table, pid: int | None, launch_unix_s: float, expected_command: str,
                             expected_start: str | None) -> str | None:
    """Check that a pid is still this run's accepted harness on Windows: alive, the same creation time,
    created after launch and running the launched image. Windows has no session or group to check."""
    if table is None or pid is None or pid <= 0 or pid == perf_sessions.own_pid() or expected_start is None:
        return f"harness pid {pid} cannot be validated"
    try:
        info = table.read(pid)
    except perf_sessions.ProcessUnreadable as error:
        return f"harness pid {pid} is unreadable: {error}"
    if info is None:
        return f"harness pid {pid} is not alive"
    if info.start != expected_start:
        return (f"harness pid {pid} now names another process (created {info.start}, accepted {expected_start}); "
                f"its identity changed, so it was not terminated")
    if info.start_unix_s < launch_unix_s - perf_sessions.START_TOLERANCE_S:
        return f"harness pid {pid} was created before this run's launch"
    if info.command.lower() != expected_command.lower():
        return f"harness pid {pid} runs {info.command!r}, not {expected_command}"
    return None


def validate_anchor(record: perf_sessions.SessionRecord, table, launch_unix_s: float,
                    excluded_sids: Iterable[int] = ()) -> perf_sessions.AckedSession | None:
    """Validate only a late record's anchor, so cleanup can still reach a session whose leader is gone.

    The live anchor keeps the leader's process group, whose id is the session id, so the id stays reserved.
    """
    try:
        anchor = table.read(record.anchor_pid)
    except perf_sessions.ProcessUnreadable:
        return None
    if (anchor is None or anchor.sid != record.leader_pid or anchor.pgid != record.leader_pid
            or record.leader_pid in set(excluded_sids) or anchor.start_unix_s < launch_unix_s - perf_sessions.START_TOLERANCE_S):
        return None
    return perf_sessions.AckedSession(record.role, record.leader_pid, record.anchor_pid, "", anchor.start)


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
    harness_command: str = perf_settings.HARNESS_EXAMPLE
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
        except perf_sessions.ProcessUnreadable:
            return None
        if info is None or info.start_unix_s < self.context.launch_unix_s - perf_sessions.START_TOLERANCE_S:
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
                record = perf_sessions.parse_program_record(path.read_text(encoding="utf-8"), role)
            except (OSError, UnicodeDecodeError, ValueError) as error:
                # A record may still be being written; only the final scan treats it as broken.
                if final:
                    self._reject(role, f"program record {path.name} cannot be read: {error}")
                continue
            if final:
                self._reject(role, f"program {role} was not acknowledged before the run ended")
                continue
            acked, problem = perf_sessions.validate_program(record, self.context.table, self.harness_pid,
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
                record = perf_sessions.parse_session_record(path.read_text(encoding="utf-8"), role)
            except (OSError, UnicodeDecodeError, ValueError) as error:
                # A record may still be being written; only the final scan treats it as broken.
                if final:
                    self._reject(role, f"session record {path.name} cannot be read: {error}")
                continue
            acked, problem = perf_sessions.validate_session(record, self.context.table, self.context.launch_unix_s, excluded)
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
        argv = (perf_scenarios.FOOTPRINT_BINARY, "-p", str(self.harness_pid), "-j", str(json_path))
        step = self.context.gate.Step(f"footprint-{stem}", argv, ("macos",), FOOTPRINT_TIMEOUT_S, "local", (), ())
        result = self.context.gate.run_step(step, self._step_index, self.context.evidence,
                                            self.context.evidence, dict(os.environ))
        # An exit status exists only once run_step reaped the process; a launch failure started none.
        reaped = result.exit_code is not None or result.status == "LAUNCH"
        record.update(status=result.status, exit_code=result.exit_code, log=str(result.log_path),
                      output=perf_scenarios.log_tail(perf_scenarios.read_log(result.log_path), 20), reaped=reaped)
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
        reading = self.sample_function() if self.sample_function is not None else perf_focus.sample_front(self.run)
        perf_focus.append_front_samples(self.log_path, reading.records)
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
