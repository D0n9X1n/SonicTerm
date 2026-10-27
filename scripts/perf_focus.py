"""Perf focus."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import json
import os
from pathlib import Path
import re
import time
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_settings


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
                    clock: Callable[[], float] = time.time, cwd: Path = perf_settings.ROOT) -> CommandRecord:
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
