"""Gate custody."""

from __future__ import annotations
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time

import gate_model
import gate_process


def leader_watches(os_module=os, select_module=select) -> tuple[str, ...]:
    """List the ways this host sees a POSIX leader exit without reaping it, preferred first.

    waitid with WNOWAIT is preferred where Python provides it; older macOS builds lack
    os.waitid and use a kqueue exit event. Windows has neither and uses Popen.wait.
    """
    if os_module.name == "nt":
        return ()
    found = []
    if all(hasattr(os_module, name) for name in gate_process._WAITID_NAMES):
        found.append("waitid")
    if all(hasattr(select_module, name) for name in gate_process._KQUEUE_NAMES):
        found.append("kqueue")
    return tuple(found)


# None falls back to Popen.wait, which reaps the leader before its group is settled.
_LEADER_WATCH = (leader_watches() or (None,))[0]


class _LeaderLost(Exception):
    """Another reaper collected a step's leader, so its exit status and group id are gone."""


def _await_leader(process: subprocess.Popen[bytes], timeout: float, watch: str) -> bool:
    """Wait up to timeout for the leader to exit, leaving it unreaped; report whether it exited.

    watch is "waitid" or "kqueue", from leader_watches(). An unreaped leader keeps its pid,
    which is the group id, so no unrelated group can take that number while the runner
    still polls or kills the group. Nothing here reaps the leader. _LeaderLost means
    another reaper collected it first, as when SIGCHLD is ignored.
    """
    if watch == "waitid":
        end = time.monotonic() + timeout
        delay = 0.0005
        while True:
            try:
                if os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None:
                    return True
            except ChildProcessError:
                raise _LeaderLost from None
            remaining = end - time.monotonic()
            if remaining <= 0:
                return False
            delay = min(delay * 2, remaining, 0.05)
            time.sleep(delay)
    queue = select.kqueue()
    try:
        event = select.kevent(process.pid, select.KQ_FILTER_PROC,
                              select.KQ_EV_ADD | select.KQ_EV_ONESHOT, select.KQ_NOTE_EXIT)
        try:
            queue.control([event], 0, 0)
        except ProcessLookupError:
            pass  # Already exited; signal 0 below tells an unreaped zombie from a lost leader.
        else:
            if not queue.control(None, 1, max(0.0, timeout)):
                return False
    finally:
        queue.close()
    try:
        # Signal 0 reaches an unreaped zombie and fails once another reaper has collected it.
        os.kill(process.pid, 0)
    except OSError:
        raise _LeaderLost from None
    return True


def _group_members(pgid: int) -> list[int] | None:
    """List the live processes in a POSIX process group, or None when they cannot be read.

    Linux reads /proc, which a minimal container has even without `ps`; other hosts ask
    `ps`. Zombies are left out: they run nothing, and a container's PID 1 may never reap them.
    """
    proc = Path("/proc")
    if sys.platform.startswith("linux") and proc.is_dir():
        try:
            entries = [entry for entry in proc.iterdir() if entry.name.isdigit()]
        except OSError:
            return None
        members = []
        for entry in entries:
            try:
                data = (entry / "stat").read_bytes()
            except OSError:
                continue  # The process exited while the list was read.
            # After the parenthesized command name come the state, the parent, and the group.
            fields = data[data.rfind(b")") + 2:].split()
            if len(fields) >= 3 and fields[2] == str(pgid).encode() and fields[0] not in (b"Z", b"X"):
                members.append(int(entry.name))
        return members
    try:
        completed = subprocess.run(
            ["ps", "-A", "-o", "pid=", "-o", "pgid=", "-o", "stat="],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=gate_model.PS_TIMEOUT_S,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if completed.returncode != 0:
        return None
    members = []
    for line in completed.stdout.decode("ascii", errors="replace").splitlines():
        fields = line.split()
        if (len(fields) >= 3 and fields[0].isdigit() and fields[1] == str(pgid)
                and not fields[2].startswith("Z")):
            members.append(int(fields[0]))
    return members


def _settle_group(process: subprocess.Popen[bytes], deadline: float) -> tuple[bool, int | None]:
    """Wait up to LEFTOVER_GRACE_S for an exited leader's process group to empty, then kill it.

    Returns whether live members outlived the grace period and were killed, and how many,
    or None when they could not be counted. Where the host can see an exit without reaping
    it, the caller keeps the leader unreaped until after this returns, so the group id cannot
    be reused; emptiness therefore comes from the member list, which leaves the zombie leader
    out, not from signal 0, which counts it.
    The Windows run_step path uses its owned Job Object instead of this POSIX helper.
    """
    if os.name == "nt":
        return False, 0
    # The leader was started with start_new_session, so its pid is the group id.
    pgid = process.pid
    grace_end = min(time.monotonic() + gate_model.LEFTOVER_GRACE_S, deadline)
    members = _group_members(pgid)
    # An unreadable list is retried until the grace period ends; it is never proof of empty.
    while members != [] and time.monotonic() < grace_end:
        time.sleep(0.05)
        members = _group_members(pgid)
    if members == []:
        return False, 0
    # Members are still live, or still cannot be listed after the grace period.
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        return False, 0  # The group emptied between the last check and the kill.
    except PermissionError:
        pass
    kill_end = time.monotonic() + 5
    while time.monotonic() < kill_end and _group_members(pgid) != []:
        time.sleep(0.1)
    return True, (None if members is None else len(members))
