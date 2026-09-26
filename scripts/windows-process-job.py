"""Windows local-gate custody with streamed output and an assigned-before-launch bootstrap."""
from __future__ import annotations

import ctypes as C
from ctypes import wintypes as W
import msvcrt
import os
from pathlib import Path
import struct
import subprocess
import sys
import time

BOOTSTRAP = Path(__file__).with_name("windows-process-bootstrap.py")
START_TOKEN = b"SONICTERM-JOB-START-v1\n"
K = C.WinDLL("kernel32", use_last_error=True)


def _api(name, result, *arguments):
    function = getattr(K, name)
    function.restype, function.argtypes = result, list(arguments)
    return function


_create = _api("CreateJobObjectW", W.HANDLE, C.c_void_p, W.LPCWSTR)
_set = _api("SetInformationJobObject", W.BOOL, W.HANDLE, C.c_int, C.c_void_p, W.DWORD)
_query = _api("QueryInformationJobObject", W.BOOL, W.HANDLE, C.c_int, C.c_void_p, W.DWORD, C.c_void_p)
_assign = _api("AssignProcessToJobObject", W.BOOL, W.HANDLE, W.HANDLE)
_terminate = _api("TerminateJobObject", W.BOOL, W.HANDLE, W.UINT)
_close = _api("CloseHandle", W.BOOL, W.HANDLE)
_peek = _api("PeekNamedPipe", W.BOOL, W.HANDLE, C.c_void_p, W.DWORD, C.c_void_p, C.POINTER(W.DWORD), C.c_void_p)


class BasicLimits(C.Structure):
    _fields_ = [("process_time", C.c_int64), ("job_time", C.c_int64), ("flags", W.DWORD),
                ("minimum", C.c_size_t), ("maximum", C.c_size_t), ("active_limit", W.DWORD),
                ("affinity", C.c_size_t), ("priority", W.DWORD), ("scheduling", W.DWORD)]


class ExtendedLimits(C.Structure):
    _fields_ = [("basic", BasicLimits), ("io", C.c_uint64 * 6),
                ("process_memory", C.c_size_t), ("job_memory", C.c_size_t),
                ("peak_process", C.c_size_t), ("peak_job", C.c_size_t)]


class Accounting(C.Structure):
    _fields_ = [("user_time", C.c_int64), ("kernel_time", C.c_int64),
                ("period_user", C.c_int64), ("period_kernel", C.c_int64),
                ("faults", W.DWORD), ("total", W.DWORD), ("active", W.DWORD), ("terminated", W.DWORD)]


def _check(value):
    if not value:
        raise C.WinError(C.get_last_error())
    return value


def _process_handle(process, *, close=False):
    # CPython retains this original handle; it does not retain the primary thread handle.
    if close:
        process._handle.Close()
        return None
    return int(process._handle)


class Job:
    """Retain the only job handle until cleanup is checked, then release it."""
    def __init__(self):
        self.handle = _check(_create(None, None))
        limits = ExtendedLimits()
        limits.basic.flags = 0x2000
        try:
            _check(_set(self.handle, 9, C.byref(limits), C.sizeof(limits)))
        except BaseException:
            self.close()
            raise

    def assign(self, process):
        """Assign the original bootstrap handle before any target is authorized."""
        _check(_assign(self.handle, _process_handle(process)))

    def accounting(self):
        """Read authoritative membership without opening candidate process IDs."""
        value = Accounting()
        _check(_query(self.handle, 1, C.byref(value), C.sizeof(value), None))
        return {"active_processes": value.active, "total_processes": value.total}

    def terminate(self):
        """Terminate only the owned job, without breakaway or PID discovery."""
        _check(_terminate(self.handle, 124))

    def close(self):
        """Release the non-inheritable kill-on-close handle."""
        _check(_close(self.handle))


def _launch(command, cwd, env, startup_handle, status_handle):
    startup = subprocess.STARTUPINFO()
    startup.lpAttributeList = {"handle_list": [startup_handle, status_handle]}
    return subprocess.Popen(
        [sys.executable, "-I", "-S", str(BOOTSTRAP), str(startup_handle), str(status_handle), *command],
        cwd=str(cwd), env=dict(env), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, bufsize=0, close_fds=True, startupinfo=startup,
        creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
    )


class Pipe:
    """Read bounded chunks only when available, independent of inherited writer lifetime."""
    def __init__(self, fd):
        self.fd, self.eof = fd, False

    def chunks(self):
        for _ in range(16):
            available = W.DWORD()
            if not _peek(msvcrt.get_osfhandle(self.fd), None, 0, None, C.byref(available), None):
                error = C.get_last_error()
                if error == 109:
                    self.eof = True
                    return
                raise C.WinError(error)
            if not available.value:
                return
            yield os.read(self.fd, min(4096, available.value))


class Capture:
    """Stream merged target bytes to disk; only private status bytes stay in memory."""
    def __init__(self, output, status, sink, limit):
        self.output, self.status = Pipe(output), Pipe(status)
        self.sink, self.limit, self.written = sink, limit, 0
        self.overflow = self.sink_failed = False
        self.record, self.status_seen = bytearray(), 0

    def pump(self):
        """Drain after overflow or sink failure so a full pipe cannot strand the target."""
        if not self.output.eof:
            for chunk in self.output.chunks():
                kept = chunk if self.limit is None else chunk[:max(0, self.limit - self.written)]
                self.overflow |= len(kept) < len(chunk)
                if not self.sink_failed:
                    try:
                        if kept:
                            if self.sink.write(kept) != len(kept):
                                raise OSError("short output log write")
                            self.sink.flush()
                    except BaseException:
                        self.sink_failed = True
                        raise
                self.written += len(kept)
        if not self.status.eof:
            for chunk in self.status.chunks():
                self.status_seen += len(chunk)
                self.record.extend(chunk[:max(0, 9 - len(self.record))])

    def target(self):
        """Accept one complete fixed-width status, never a truncated or duplicate record."""
        if not self.status.eof or self.status_seen != 8:
            raise ValueError("missing, truncated or duplicate target status")
        value = struct.unpack("<I", self.record[4:])[0]
        if self.record[:4] == b"EXIT":
            return value, False
        if self.record[:4] == b"FAIL":
            return None, True
        raise ValueError("invalid target status")


def run(command, *, cwd, env, sink, deadline, output_limit_bytes=None, grace=2.0):
    """Return target exit and custody facts; the local gate chooses the step policy."""
    process = job = capture = None
    fds = set()
    assigned = authorized = natural = interrupted = timed_out = launch_failed = False
    errors = []
    custody = {"before_cleanup": None, "after_cleanup": None, "cleanup": "none",
               "empty": False, "bootstrap_reaped": False, "protocol_complete": False,
               "capture_complete": False, "errors": errors}
    exit_code = None

    def record(stage, error):
        nonlocal interrupted
        interrupted |= isinstance(error, KeyboardInterrupt)
        errors.append(f"{stage}: {type(error).__name__}: {error}")

    try:
        job = Job()
        start_read, start_write = os.pipe()
        fds.update((start_read, start_write))
        status_read, status_write = os.pipe()
        fds.update((status_read, status_write))
        os.set_inheritable(start_read, True)
        os.set_inheritable(status_write, True)
        try:
            process = _launch(command, cwd, env, msvcrt.get_osfhandle(start_read), msvcrt.get_osfhandle(status_write))
        except OSError:
            launch_failed = True
            raise
        for fd in (start_read, status_write):
            os.close(fd)
            fds.remove(fd)
        capture = Capture(process.stdout.fileno(), status_read, sink, output_limit_bytes)
        job.assign(process)
        assigned = True
        if time.monotonic() >= deadline:
            raise TimeoutError("execution deadline reached before authorization")
        os.write(start_write, START_TOKEN)
        authorized = True
        os.close(start_write)
        fds.remove(start_write)
        while True:
            capture.pump()
            if time.monotonic() >= deadline:
                raise TimeoutError("execution deadline reached")
            if process.poll() is not None:
                break
            time.sleep(0.005)
        grace_end = min(deadline, time.monotonic() + grace)
        while True:
            counts = job.accounting()
            if counts["active_processes"] == 0:
                natural = True
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("execution deadline reached during grace")
            if time.monotonic() >= grace_end:
                break
            capture.pump()
            time.sleep(0.005)
    except BaseException as error:
        timed_out |= isinstance(error, TimeoutError)
        record("execution", error)
    finally:
        cleanup_end = time.monotonic() + 2.0
        try:
            if "start_write" in locals() and start_write in fds:
                os.close(start_write)
                fds.remove(start_write)
            if job is not None:
                try:
                    custody["before_cleanup"] = job.accounting()
                except BaseException as error:
                    record("pre-cleanup accounting", error)
                if assigned and (not natural or errors):
                    custody["cleanup"] = "terminated"
                    try:
                        job.terminate()
                    except BaseException as error:
                        record("job termination", error)
                        raise
                elif process is not None and not assigned:
                    custody["cleanup"] = "unassigned-bootstrap"
                    process.kill()
                while time.monotonic() < cleanup_end:
                    try:
                        counts = job.accounting()
                        if counts["active_processes"] == 0:
                            custody["after_cleanup"], custody["empty"] = counts, True
                            break
                    except BaseException as error:
                        record("cleanup accounting", error)
                        break
                    if capture:
                        capture.pump()
                    time.sleep(0.005)
                if not custody["empty"]:
                    errors.append("owned job emptiness was not verified")
            if process is not None:
                process.wait(timeout=max(0, cleanup_end - time.monotonic()))
                custody["bootstrap_reaped"] = True
                if process.returncode != 0:
                    errors.append(f"bootstrap exited with {process.returncode}")
            if capture:
                while time.monotonic() < cleanup_end and not (capture.output.eof and capture.status.eof):
                    capture.pump()
                    time.sleep(0.005)
                if authorized:
                    try:
                        exit_code, target_launch_failed = capture.target()
                        custody["protocol_complete"] = True
                        launch_failed |= target_launch_failed
                    except ValueError as error:
                        record("protocol", error)
                custody["capture_complete"] = capture.output.eof and not capture.overflow and not capture.sink_failed
                if not capture.output.eof:
                    errors.append("output EOF was not observed")
                if capture.overflow:
                    errors.append(f"output limit of {output_limit_bytes} bytes exceeded")
                if capture.sink_failed:
                    errors.append("output log capture failed")
        except BaseException as error:
            record("cleanup", error)
            custody["empty"] = False
        finally:
            if job is not None:
                try:
                    job.close()
                except BaseException as error:
                    record("job close", error)
            if process is not None and not custody["bootstrap_reaped"]:
                try:
                    if not assigned:
                        process.kill()
                    process.wait(timeout=max(0, cleanup_end - time.monotonic()))
                    custody["bootstrap_reaped"] = True
                except BaseException as error:
                    record("final reap", error)
            for fd in fds:
                try:
                    os.close(fd)
                except BaseException as error:
                    record("protocol close", error)
            if process is not None:
                for close in (process.stdout.close, lambda: _process_handle(process, close=True)):
                    try:
                        close()
                    except BaseException as error:
                        record("process close", error)
    return {"exit_code": exit_code, "interrupted": interrupted, "timed_out": timed_out,
            "launch_failed": launch_failed, "natural": natural, "errors": errors, "custody": custody}
