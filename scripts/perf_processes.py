"""Perf processes."""

from __future__ import annotations
import errno
import os
from pathlib import Path
import signal
import struct
import sys

import perf_sessions


# `proc_bsdinfo` from <sys/proc_info.h>: flags, status, xstatus, pid, ppid, uid, gid, ruid, rgid,
# svuid, svgid, rfu_1, comm[16], name[32], nfiles, pgid, pjobc, e_tdev, e_tpgid, nice,
# start seconds and microseconds; 136 bytes with natural alignment.
BSDINFO_FORMAT = "=IIIIIIIIIII I16s32sIIIIIiQQ".replace(" ", "")
BSDINFO_SIZE = struct.calcsize(BSDINFO_FORMAT)
PROC_PIDTBSDINFO = 3
# `SZOMB` from <sys/proc.h>.
MACOS_ZOMBIE_STATUS = 5


def decode_bsdinfo(raw: bytes, sid: int) -> perf_sessions.ProcessInfo | None:
    """Decode one `proc_bsdinfo` record; a zombie reads as gone because it runs nothing."""
    fields = struct.unpack(BSDINFO_FORMAT, raw[:BSDINFO_SIZE])
    if fields[1] == MACOS_ZOMBIE_STATUS:
        return None
    start_s, start_us = fields[20], fields[21]
    command = fields[12].split(b"\0", 1)[0].decode("utf-8", errors="replace")
    return perf_sessions.ProcessInfo(fields[3], fields[15], sid, f"{start_s}.{start_us:06d}",
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
            raise perf_sessions.ProcessUnreadable(f"getsid({pid}) failed: {error}") from None

    def read(self, pid: int) -> ProcessInfo | None:
        """Read a same-user process, None when it is gone; another user's process is unreadable."""
        ctypes = self._ctypes
        buffer = ctypes.create_string_buffer(BSDINFO_SIZE)
        written = self._libproc.proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, buffer, BSDINFO_SIZE)
        if written != BSDINFO_SIZE:
            code = ctypes.get_errno()
            if written <= 0 and code == errno.ESRCH:
                return None
            raise perf_sessions.ProcessUnreadable(f"proc_pidinfo({pid}) wrote {written} bytes, errno {code}")
        sid = self.session_of(pid)
        if sid is None:
            return None
        info = decode_bsdinfo(buffer.raw, sid)
        if info is not None and info.pid != pid:
            raise perf_sessions.ProcessUnreadable(f"proc_pidinfo({pid}) returned pid {info.pid}")
        return info

    def kill(self, pid: int) -> str:
        """SIGKILL one validated pid."""
        return send_kill(pid)

    def kill_group(self, pgid: int) -> str:
        """SIGKILL one validated process group."""
        return send_kill(pgid, group=True)


def parse_proc_stat(text: str, pid: int, boot_unix_s: float, ticks_per_s: int) -> perf_sessions.ProcessInfo | None:
    """Parse `/proc/<pid>/stat`; the command name ends at the last `)`, and a zombie reads as gone."""
    open_index, close_index = text.find("("), text.rfind(")")
    if open_index < 0 or close_index < open_index:
        raise perf_sessions.ProcessUnreadable(f"/proc/{pid}/stat cannot be parsed")
    fields = text[close_index + 2:].split()
    if len(fields) < 20:
        raise perf_sessions.ProcessUnreadable(f"/proc/{pid}/stat has too few fields")
    if fields[0] in ("Z", "X", "x"):
        return None
    try:
        ppid, pgid, sid, start_ticks = int(fields[1]), int(fields[2]), int(fields[3]), int(fields[19])
    except ValueError:
        raise perf_sessions.ProcessUnreadable(f"/proc/{pid}/stat holds a non-numeric field") from None
    return perf_sessions.ProcessInfo(pid, pgid, sid, str(start_ticks), boot_unix_s + start_ticks / ticks_per_s,
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
            raise perf_sessions.ProcessUnreadable(f"/proc/{pid}/stat: {error}") from None
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
            raise perf_sessions.ProcessUnreadable(f"the process list cannot be read: {error}") from error
        row = next((row for row in rows if row[0] == pid), None)
        if row is None:
            return None
        _pid, parent, image = row
        try:
            handle = self.api.open_process(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, pid)
        except OSError as error:
            raise perf_sessions.ProcessUnreadable(f"pid {pid} cannot be opened: {error}") from error
        if handle is None:
            return None
        try:
            if not self.api.is_alive(handle):
                return None
            created = self.api.creation_time(handle)
        except OSError as error:
            raise perf_sessions.ProcessUnreadable(f"pid {pid} cannot be read: {error}") from error
        finally:
            self.api.close(handle)
        return perf_sessions.ProcessInfo(pid, 0, 0, str(created), filetime_to_unix_s(created), image, parent)

    def terminate(self, pid: int, start: str) -> str:
        """TerminateProcess(124) on `pid` only if it still has creation time `start`.

        Returns `sent`, `gone`, `stale` (the pid now names another process) or `refused`.
        """
        if pid <= 0 or pid == perf_sessions.own_pid():
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
