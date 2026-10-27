"""Gate process."""

from __future__ import annotations
from dataclasses import dataclass, replace
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import time
from typing import Iterable, Mapping, Sequence, TextIO

import gate_harness
import gate_model
import gate_preparations


def resolve_program(program: str, root: Path, env: Mapping[str, str]) -> str | None:
    """Find a step's executable: a path relative to the repository root, or a name on PATH."""
    if "/" in program or "\\" in program:
        candidate = Path(program)
        if not candidate.is_absolute():
            candidate = root / candidate
        if candidate.is_file() and (os.name == "nt" or os.access(candidate, os.X_OK)):
            return str(candidate)
        return None
    return shutil.which(program, path=env.get("PATH"))


def _verified_custody(custody: dict[str, object] | None) -> bool:
    return bool(custody and custody.get("empty") is True and custody.get("bootstrap_reaped") is True
                and custody.get("protocol_complete") is True and custody.get("capture_complete") is True
                and custody.get("errors") == [])


@dataclass(frozen=True)
class PhaseResult:
    """Keep preparation outcomes separate from the original command's exit and custody."""

    name: str
    argv: tuple[str, ...]
    env: tuple[tuple[str, str], ...]
    policy: WindowsPolicy
    status: str = gate_model.NOT_RUN
    exit_code: int | None = None
    detail: str = ""
    custody: dict[str, object] | None = None

    @property
    def accepted(self) -> bool:
        """Require complete evidence even when compilation needed forced cleanup."""
        return (self.exit_code == 0 and _verified_custody(self.custody)
                and (self.status == gate_model.PASS or (self.status == gate_model.CLEANED_NOT_NATURAL
                     and self.policy == gate_model.WindowsPolicy.COMPILE_ONLY
                     and self.custody.get("cleanup") == "terminated")))


@dataclass(frozen=True)
class StepResult:
    """The verdict, exit status, duration, and log of one executed step."""

    id: str
    status: str
    exit_code: int | None
    elapsed_s: float
    log_path: Path
    detail: str = ""
    # Process-group members killed after the leader exited; None when they could not be counted.
    leftover_processes: int | None = 0
    windows_policy: gate_model.WindowsPolicy = gate_model.WindowsPolicy.STRICT
    custody: dict[str, object] | None = None
    phases: tuple[PhaseResult, ...] = ()

    @property
    def accepted(self) -> bool:
        """Accept cleaned compilation without concealing its non-natural lifetime."""
        if self.phases:
            return (self.status in (gate_model.PASS, gate_model.CLEANED_NOT_NATURAL) and all(phase.accepted for phase in self.phases)
                    and self.phases[-1].name == "execution"
                    and (self.phases[-1].policy == gate_model.WindowsPolicy.COMPILE_ONLY
                         or self.phases[-1].status == gate_model.PASS))
        return self.status == gate_model.PASS or (
            self.status == gate_model.CLEANED_NOT_NATURAL and self.exit_code == 0
            and self.windows_policy == gate_model.WindowsPolicy.COMPILE_ONLY
            and (self.id in gate_model._COMPILE_ONLY_STEPS or self.id in gate_preparations.PERF_BUILDS
                 or any(self.id == step.id for step in gate_harness._TARGET_COMPILE_STEPS))
            and self.custody is not None and self.custody.get("empty") is True
            and self.custody.get("cleanup") == "terminated"
            and self.custody.get("bootstrap_reaped") is True
            and self.custody.get("protocol_complete") is True
            and self.custody.get("capture_complete") is True and self.custody.get("errors") == []
        )


def _step_log_path(log_dir: Path, index: int, step: gate_model.Step) -> Path:
    return log_dir / f"{index:02d}-{step.id}.log"


def _copy_output(pipe, log) -> None:
    """Stream a step's combined output into its log as it arrives, so the log can be tailed."""
    try:
        while True:
            chunk = pipe.read1(65536)
            if not chunk:
                return
            log.write(chunk)
            log.flush()
    except (OSError, ValueError):
        # The pipe or log closed under a killed or detached step; the
        # result line already records why the output ends here.
        return


def _copy_limited_output(pipe, log, limit: int, overflow: threading.Event) -> None:
    """Keep a bounded prefix but drain the pipe so overflow cannot block the child."""
    remaining = limit
    try:
        while True:
            chunk = pipe.read1(65536)
            if not chunk:
                return
            if len(chunk) > remaining:
                overflow.set()
            kept = chunk[:remaining]
            remaining -= len(kept)
            log.write(kept)
            log.flush()
    except (OSError, ValueError):
        overflow.set()


def _write_header(log, step: gate_model.Step, argv: Sequence[str], root: Path) -> None:
    lines = [
        f"[local-gate] step={step.id} evidence={step.evidence} timeout={step.timeout_s}s",
        f"[local-gate] command={gate_harness.command_text(step)}",
        f"[local-gate] argv={json.dumps(list(argv))}",
        f"[local-gate] cwd={root}",
    ]
    log.write(("\n".join(lines) + "\n\n").encode("utf-8", errors="replace"))
    log.flush()


def _finish(log, step: gate_model.Step, log_path: Path, started: float, status: str,
            exit_code: int | None, detail: str, leftover: int | None = 0,
            custody: dict[str, object] | None = None, phases: tuple[PhaseResult, ...] = ()) -> StepResult:
    elapsed = time.monotonic() - started
    footer = f"\n[local-gate] result={status} exit={_exit_text(exit_code)} elapsed={elapsed:.1f}s"
    if leftover != 0:
        footer += f" leftover_processes={'unknown' if leftover is None else leftover}"
    if detail:
        footer += f" detail={detail}"
    try:
        log.write((footer + "\n").encode("utf-8", errors="replace"))
        log.flush()
    except (OSError, ValueError) as error:
        if custody is not None:
            if status not in (gate_model.INTERRUPTED, gate_model.TIMEOUT, gate_model.LAUNCH):
                status = gate_model.FAIL
            detail += f"; final log write failed: {error}"
    return StepResult(step.id, status, exit_code, elapsed, log_path, detail, leftover,
                      step.windows_policy, custody, phases)


def _reap_leader(process: subprocess.Popen[bytes], timeout: float) -> bool:
    """Reap an exited or killed leader within timeout; return False if another reaper had it.

    Popen.wait turns a lost child into exit status 0, so POSIX reaps with os.waitpid, which
    reports ECHILD instead. A leader still running at the timeout is left to the caller.
    """
    if process.returncode is not None:
        return True
    if os.name == "nt":
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass
        return True
    end = time.monotonic() + timeout
    while True:
        try:
            pid, status = os.waitpid(process.pid, os.WNOHANG)
        except ChildProcessError:
            return False
        if pid:
            process.returncode = os.waitstatus_to_exitcode(status)
            return True
        if time.monotonic() >= end:
            return True
        time.sleep(0.01)


def _kill_tree(process: subprocess.Popen[bytes], owned: bool) -> str:
    """Kill a timed-out or interrupted step's tree and describe how the kill went.

    Without ownership nothing is signalled: another reaper collected the leader, so its pid and
    group id may already name other processes. macOS refuses a group kill with EPERM when the
    unreaped leader is the only member left, where Linux reports success; that refusal is
    recorded, and the leader is still killed or reaped.
    """
    if not owned:
        return "no signal sent: another reaper collected the leader, so its pid and group id are not reserved"
    try:
        gate_model.SMOKE_RUNNER.terminate_process_tree(process)
    except PermissionError:
        try:
            process.kill()
        except OSError:
            pass  # The leader cannot be signalled either; the bounded reap still returns.
        return "the group kill was refused with EPERM, so only the leader was killed or reaped"
    return "process tree killed"


def _reap(process: subprocess.Popen[bytes], reader: threading.Thread, owned: bool = True) -> str:
    """Collect a killed step's status and let its output drain, without blocking forever.

    A leader another reaper collected is not waited for, so no synthetic status is recorded.
    """
    if owned:
        _reap_leader(process, 10)
    reader.join(5)
    if reader.is_alive():
        # A descendant that left the process group still holds the pipe;
        # the reader is a daemon thread, so it cannot keep the gate alive.
        return "; a descendant outside the process group still holds the output pipe"
    _close(process.stdout)
    return ""


def _close(pipe) -> None:
    if pipe is not None:
        try:
            pipe.close()
        except OSError:
            pass


def _exit_text(exit_code: int | None) -> str:
    return "-" if exit_code is None else str(exit_code)


_WAITID_NAMES = ("waitid", "P_PID", "WEXITED", "WNOHANG", "WNOWAIT")
_KQUEUE_NAMES = ("kqueue", "kevent", "KQ_FILTER_PROC", "KQ_EV_ADD", "KQ_EV_ONESHOT", "KQ_NOTE_EXIT")
