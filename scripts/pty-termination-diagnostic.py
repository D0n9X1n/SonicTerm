#!/usr/bin/env python3
"""Run a non-shipping, fixed-size macOS PTY termination experiment."""

from __future__ import annotations

import argparse
import ctypes
import dataclasses
from datetime import datetime, timezone
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
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parent.parent
BASELINE = "af41d8624ea4b65d459147a611cbea9c03fbd41f"
TEST = "reported_bytes_track_the_ring_the_queue_pins"
PROBE_ENV = "SONICTERM_PTY_TERMINATION_PROBE_DIR"
ATTEMPTS = 8
TOTAL_SECONDS = 1200
OUTPUT_LIMIT = 8 << 20
MAX_IDENTITIES = 8192
LIMITS = [
    "A passing experiment means OBSERVED_NO_REPRO, not a repaired or disproven CI failure.",
    "Baseline and instrumented binaries have separate source roots and Cargo targets on one host.",
    "Each phase stops at its first failure; the second predeclared phase is not a baseline retry.",
    "Observed PID/birth custody cannot establish absence of unobserved escaped descendants.",
    "The passive Rust probe may perturb scheduling; eventual settlement does not prove signal delivery.",
]


def load_gate():
    spec = importlib.util.spec_from_file_location("pty_diagnostic_gate", ROOT / "scripts/local-gate.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


GATE = load_gate()


def utc():
    return datetime.now(timezone.utc).isoformat()


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            value.update(chunk)
    return value.hexdigest()


def save(path, value):
    with Path(path).open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=False)
        stream.write("\n")


def git(root, *args):
    return subprocess.check_output(
        ["git", "--no-optional-locks", "-C", str(root), *args], timeout=15
    )


def source_pin(root):
    paths = git(root, "ls-files", "-z").split(b"\0")
    manifest = []
    for name in paths:
        if not name:
            continue
        path = root / os.fsdecode(name)
        info = path.lstat()
        data = os.fsencode(os.readlink(path)) if path.is_symlink() else path.read_bytes()
        manifest.append([os.fsdecode(name), info.st_mode, hashlib.sha256(data).hexdigest()])
    return {
        "head": git(root, "rev-parse", "HEAD").decode().strip(),
        "tree": git(root, "rev-parse", "HEAD^{tree}").decode().strip(),
        "index": hashlib.sha256(git(root, "ls-files", "--stage", "-z")).hexdigest(),
        "status": git(root, "status", "--porcelain=v1", "-z").decode(),
        "manifest_sha256": hashlib.sha256(json.dumps(manifest).encode()).hexdigest(),
        "file_count": len(manifest),
    }


class ProcInfo(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint32) for name in (
        "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid", "rgid",
        "svuid", "svgid", "reserved",
    )]
    _fields_ += [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
    _fields_ += [(name, ctypes.c_uint32) for name in ("nfiles", "pgid", "pjobc", "tdev", "tpgid")]
    _fields_ += [("nice", ctypes.c_int32), ("seconds", ctypes.c_uint64), ("micros", ctypes.c_uint64)]


class MacProcesses:
    def __init__(self):
        self.library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        self.library.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                             ctypes.c_void_p, ctypes.c_int]
        self.library.proc_pidinfo.restype = ctypes.c_int

    def identity(self, pid):
        info = ProcInfo()
        ctypes.set_errno(0)
        size = self.library.proc_pidinfo(pid, 3, 1, ctypes.byref(info), ctypes.sizeof(info))
        error = ctypes.get_errno()
        if size == 0 and error == errno.ESRCH:
            return None
        if size != ctypes.sizeof(info) or info.pid != pid or not info.seconds:
            raise RuntimeError(f"unknown process identity pid={pid} size={size} errno={error}")
        return {name: int(getattr(info, name)) for name in (
            "pid", "ppid", "pgid", "uid", "status", "seconds", "micros",
        )}

    def census(self):
        result = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,pgid=,uid="],
                                capture_output=True, check=True, timeout=2)
        return parse_census(result.stdout, os.getuid())


def parse_census(output, own_uid):
    rows = {}
    for line in output.splitlines():
        fields = line.split()
        if (len(fields) != 4 or not all(value.isdigit() for value in fields[:3])
                or not re.fullmatch(rb"-?\d+", fields[3])):
            raise RuntimeError("process census row is incomplete")
        pid, parent, group, uid = map(int, fields)
        if uid == own_uid:
            rows[pid] = {"pid": pid, "ppid": parent, "pgid": group, "uid": uid}
    return rows


def identity_key(value):
    return value["pid"], value["seconds"], value["micros"]


def same_identity(first, second):
    return second is not None and identity_key(first) == identity_key(second)


class Custody:
    def __init__(self, api, leader):
        self.api = api
        self.leader = leader
        self.entries = {identity_key(leader): leader}
        self.problems = []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.watch, daemon=True)

    def update(self):
        verified = {}
        for key in list(self.entries):
            row = self.api.identity(key[0])
            if row is not None and identity_key(row) == key:
                self.entries[key] = row
                verified[row["pid"]] = row
        candidates = {pid: row for pid, row in self.api.census().items() if pid not in verified}
        while True:
            added = False
            for pid, row in list(candidates.items()):
                parent = verified.get(row["ppid"])
                root = verified.get(self.leader["pid"])
                group = root if root and row["pgid"] == root["pid"] else None
                anchor = parent or group
                if not anchor or not same_identity(anchor, self.api.identity(anchor["pid"])):
                    continue
                current = self.api.identity(pid)
                if current is None or current["uid"] != os.getuid():
                    continue
                if parent and current["ppid"] != parent["pid"]:
                    continue
                if not parent and current["pgid"] != anchor["pid"]:
                    continue
                if not same_identity(anchor, self.api.identity(anchor["pid"])):
                    continue
                if len(self.entries) == MAX_IDENTITIES:
                    raise RuntimeError("observed identity capacity exceeded")
                self.entries[identity_key(current)] = current
                verified[pid] = current
                del candidates[pid]
                added = True
            if not added:
                return

    def watch(self):
        try:
            while not self.stop.is_set():
                self.update()
                self.stop.wait(0.1)
        except Exception as error:
            self.problems.append(str(error))

    def remaining(self):
        result = []
        for key in self.entries:
            row = self.api.identity(key[0])
            if row is not None and identity_key(row) == key:
                result.append(row)
        return result

    def finish(self):
        self.stop.set()
        self.thread.join(3)
        if self.thread.is_alive():
            raise RuntimeError("custody observer did not stop")
        def inspect():
            try:
                self.update()
            except Exception as error:
                self.problems.append(str(error))
            remaining = []
            for key in self.entries:
                try:
                    row = self.api.identity(key[0])
                    if row is not None and identity_key(row) == key:
                        remaining.append(row)
                except Exception as error:
                    self.problems.append(str(error))
            return remaining
        remaining = inspect()
        grace = time.monotonic() + 2
        while any(row["status"] != 5 for row in remaining) and time.monotonic() < grace:
            time.sleep(0.1)
            remaining = inspect()
        leftovers = [row for row in remaining if row["status"] != 5]
        signals = []
        deadline = time.monotonic() + 8
        while leftovers and time.monotonic() < deadline:
            for row in leftovers:
                try:
                    current = self.api.identity(row["pid"])
                    if same_identity(row, current) and current["status"] != 5:
                        os.kill(current["pid"], signal.SIGKILL)
                        signals.append(current)
                except ProcessLookupError:
                    pass
                except Exception as error:
                    self.problems.append(str(error))
            time.sleep(0.1)
            remaining = inspect()
            leftovers = [row for row in remaining if row["status"] != 5]
        return {"observed": list(self.entries.values()), "problems": self.problems,
                "signals_after_command": signals, "remaining": remaining,
                "live_survivors": leftovers, "scope": LIMITS[3]}


class Launches:
    def __init__(self, original, api):
        self.original, self.api = original, api
        self.process = self.custody = None
        self.problem = None

    def __getattr__(self, name):
        return getattr(self.original, name)

    def Popen(self, *args, **kwargs):
        self.process = self.original.Popen(*args, **kwargs)
        try:
            leader = self.api.identity(self.process.pid)
            if leader is None:
                raise RuntimeError("direct child birth unavailable")
            self.custody = Custody(self.api, leader)
            self.custody.thread.start()
        except Exception as error:
            self.problem = str(error)
        return self.process


def command_payload(result):
    data = Path(result["log_path"]).read_bytes()
    start = data.find(b"\n\n")
    end = data.rfind(b"\n[local-gate] result=")
    if start < 0 or end < start:
        raise RuntimeError("supervisor log framing is incomplete")
    return data[start + 2:end]


def compiler_artifact(output, root, target):
    artifacts = []
    for line in output.splitlines():
        if not line.startswith(b"{"):
            continue
        value = json.loads(line)
        if value.get("reason") != "compiler-artifact" or not value.get("executable"):
            continue
        if value["target"]["name"] == "pty_queue_heap_truth" and "test" in value["target"]["kind"]:
            artifacts.append(value)
    if len(artifacts) != 1:
        raise RuntimeError("expected exactly one compiler-artifact test executable")
    artifact = artifacts[0]
    executable = Path(artifact["executable"])
    if (Path(artifact["manifest_path"]).resolve() != root / "crates/sonicterm-io/Cargo.toml"
            or not executable.resolve().is_relative_to(target.resolve())
            or not executable.is_file() or executable.is_symlink()):
        raise RuntimeError("compiler artifact source or output root differs")
    return artifact


def exact_test_pass(output, filtered):
    text = output.decode(errors="replace")
    lines = text.splitlines()
    return (lines.count(f"test {TEST} ... ok") == 1 and
            len(re.findall(rf"^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; {filtered} filtered out;", text, re.M)) == 1)


def trace_inventory(directory, process):
    paths = sorted(directory.iterdir())
    if not 1 <= len(paths) <= 16:
        raise RuntimeError("probe trace count is missing or excessive")
    entries = []
    for path in paths:
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 1 << 20:
            raise RuntimeError("probe trace type, links or length is invalid")
        data = path.read_bytes()
        lines = data.decode("utf-8").splitlines()
        if not data.endswith(b"\n") or b"\r" in data or len(lines) < 4:
            raise RuntimeError("probe trace framing is incomplete")
        header, end = lines[0].split("\t"), lines[-1].split("\t")
        if (len(header) != 10 or header[:2] != ["PTYTERM", "2"] or int(header[2]) != process
                or len(end) != 4 or end[0] != "END" or int(end[1]) != len(lines) - 2
                or end[2] != "0" or end[3] not in ("0", "1", "2")
                or any(len(line.split("\t")) != 25 for line in lines[1:-1])):
            raise RuntimeError("probe trace identity/schema/overflow is invalid")
        if path.name != f"ptyterm-{header[2]}-{header[3]}-{header[4]}.tsv":
            raise RuntimeError("probe trace filename identity differs")
        entries.append({"file": path.name, "sha256": hashlib.sha256(data).hexdigest(),
                        "session": int(header[3]), "result": int(end[3]), "records": int(end[1])})
    return entries


def run_phase(execute, before_case, attempts=ATTEMPTS):
    results = []
    for index in range(attempts):
        before_case(index)
        result = execute(index)
        results.append(result)
        if not result["accepted"]:
            break
    return results


class Experiment:
    def __init__(self, output):
        self.output = output
        self.deadline = time.monotonic() + TOTAL_SECONDS
        self.api = MacProcesses()
        self.environment = dict(os.environ, CARGO_TERM_COLOR="never", CARGO_INCREMENTAL="0",
                                PYTHONDONTWRITEBYTECODE="1", GIT_OPTIONAL_LOCKS="0")
        for key in list(self.environment):
            if key in ("NO_COLOR", "BASH_ENV", "ENV") or key.startswith(("DYLD", "SONICTERM_")):
                self.environment.pop(key)
        self.steps = []
        self.cancelled = False
        self.report = {"verdict": "BLOCKED", "baseline": BASELINE, "attempts_per_phase": ATTEMPTS,
                       "started_utc": utc(), "phases": {}, "limits": LIMITS, "steps": self.steps}

    def cancel(self, signum, _frame):
        if not self.cancelled:
            self.cancelled = True
            self.report["cancelled_signal"] = signum
            raise KeyboardInterrupt("diagnostic cancelled")

    def run(self, name, argv, root, timeout, environment=None):
        if getattr(self, "cancelled", False):
            raise RuntimeError("diagnostic cancelled; no further command admitted")
        remaining = self.deadline - time.monotonic() - 30
        if remaining < timeout:
            raise RuntimeError("full command budget does not fit the experiment deadline")
        step = GATE.Step(name, tuple(argv), ("macos",), timeout, "local", (), ())
        original = GATE.subprocess
        launches = Launches(original, self.api)
        GATE.subprocess = launches
        custody = None
        result = {"id": name, "status": "BLOCKED", "exit_code": None,
                  "log_path": str(GATE._step_log_path(self.output, len(self.steps) + 1, step)),
                  "leftover_processes": None}
        print(f"[pty-diagnostic] start {name}", flush=True)
        try:
            value = GATE.run_step(step, len(self.steps) + 1, root, self.output,
                                  environment or self.environment, output_limit_bytes=OUTPUT_LIMIT)
            result = dataclasses.asdict(value)
            result["log_path"] = str(result["log_path"])
        except BaseException as error:
            result["supervision_error"] = str(error)
            if isinstance(error, KeyboardInterrupt):
                self.cancelled = True
                result["status"] = "INTERRUPTED"
            if launches.process is not None and launches.process.returncode is None:
                owned = False
                try:
                    GATE._await_leader(launches.process, 0, GATE._LEADER_WATCH)
                    owned = True
                except GATE._LeaderLost:
                    result["leader_lost"] = True
                if owned:
                    result["exception_cleanup"] = GATE._kill_tree(launches.process, owned=True)
                    GATE._reap_leader(launches.process, 10)
        finally:
            GATE.subprocess = original
            if launches.custody is not None:
                try:
                    custody = launches.custody.finish()
                except Exception as error:
                    result["custody_error"] = str(error)
            if launches.problem:
                result["custody_error"] = launches.problem
        if result["status"] == "INTERRUPTED":
            self.cancelled = True
        result["custody"] = custody
        result["leader_reaped"] = launches.process is not None and launches.process.returncode is not None
        result["leader_pid"] = launches.process.pid if launches.process else None
        result["accepted"] = (result["status"] == "PASS" and result["exit_code"] == 0
                              and result["leftover_processes"] == 0 and result["leader_reaped"]
                              and not result.get("custody_error") and custody is not None
                              and not custody["problems"] and not custody["signals_after_command"]
                              and not custody["live_survivors"])
        result["log_sha256"] = digest(result["log_path"]) if Path(result["log_path"]).is_file() else None
        self.steps.append(result)
        save(self.output / (name + "-result.json"), result)
        print(f"[pty-diagnostic] finish {name} status={result['status']} accepted={result['accepted']}", flush=True)
        return result

    def compile(self, root, phase, target):
        environment = dict(self.environment, CARGO_TARGET_DIR=str(target))
        result = self.run(phase + "-compile", ["cargo", "test", "--locked", "-p", "sonicterm-io",
                          "--test", "pty_queue_heap_truth", "--no-run", "--message-format=json"], root, 480, environment)
        if not result["accepted"]:
            raise RuntimeError(phase + " compilation/custody failed")
        artifact = compiler_artifact(command_payload(result), root, target)
        executable = Path(artifact["executable"])
        retained = self.output / (phase + "-test-executable")
        shutil.copyfile(executable, retained)
        artifact["sha256"] = digest(executable)
        artifact["retained_sha256"] = digest(retained)
        save(self.output / (phase + "-artifact.json"), artifact)
        return executable, environment, artifact["sha256"]

    def phase(self, root, phase, target, probe):
        pin = source_pin(root)
        executable, environment, binary_sha = self.compile(root, phase, target)
        if probe:
            pure = self.run("probe-pure-tests", [str(executable), "pty_termination_probe_tests::", "--nocapture"], root, 60, environment)
            if not pure["accepted"] or b"13 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out" not in command_payload(pure):
                raise RuntimeError("probe classifier/transport controls failed")
        phase_report = {"source": pin, "binary_sha256": binary_sha, "cases": []}
        self.report["phases"][phase] = phase_report
        def before_case(index):
            if source_pin(root) != pin or digest(executable) != binary_sha:
                raise RuntimeError("source or executable changed before case")
        def execute(index):
            name = f"{phase}-{index + 1:02d}"
            case = {"accepted": False, "command": None, "exact_test_pass": False,
                    "before": {"time": utc(), "load": os.getloadavg()}}
            try:
                env = dict(environment)
                traces = self.output / (name + "-traces")
                if probe:
                    traces.mkdir(mode=0o700)
                    env[PROBE_ENV] = str(traces)
                result = self.run(name, [str(executable), "--exact", TEST, "--nocapture", "--test-threads=1"], root, 150, env)
                case["command"] = result
                output = command_payload(result)
                passed = exact_test_pass(output, 19 if probe else 6)
                case.update({"accepted": result["accepted"] and passed, "exact_test_pass": passed,
                             "binary_sha256": digest(executable)})
                if probe:
                    case["traces"] = trace_inventory(traces, result["leader_pid"])
                    if passed and (len(case["traces"]) != 3 or any(trace["result"] for trace in case["traces"])):
                        raise RuntimeError("successful three-workload fixture lacks three successful traces")
                    if b"PTY_TERMINATION_PROBE_WRITE_ERROR" in output:
                        raise RuntimeError("native trace write failed")
                if source_pin(root) != pin or digest(executable) != binary_sha:
                    raise RuntimeError("source or executable changed during case")
            except BaseException as error:
                case["evidence_error"] = str(error)
                case["accepted"] = False
            finally:
                case["after"] = {"time": utc(), "load": os.getloadavg()}
                phase_report["cases"].append(case)
                save(self.output / (name + "-case.json"), case)
            return case
        try:
            cases = run_phase(execute, before_case)
            for case in cases:
                result = case["command"]
                custody = result.get("custody") if result else None
                if (not custody or not result.get("leader_reaped") or result.get("custody_error")
                        or custody["live_survivors"] or custody["problems"]):
                    raise RuntimeError("phase custody unconfirmed; no further native experiment is admitted")
                if result["status"] == "INTERRUPTED" or getattr(self, "cancelled", False):
                    raise RuntimeError("diagnostic cancelled; paired phase not admitted")
                if case.get("evidence_error"):
                    raise RuntimeError("phase evidence failed: " + case["evidence_error"])
            return all(case["accepted"] for case in cases) and len(cases) == ATTEMPTS
        except BaseException as error:
            phase_report["problem"] = str(error)
            raise
        finally:
            save(self.output / (phase + "-phase.json"), phase_report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--expected-head", required=True)
    args = parser.parse_args()
    if (sys.platform != "darwin" or platform.mac_ver()[0].split(".")[0] != "14"
            or platform.machine() != "arm64"):
        parser.error("diagnostic execution requires an actual macOS14 arm64 host")
    output, scratch = args.output.resolve(), args.scratch.resolve()
    if any(path == ROOT or ROOT in path.parents or path in ROOT.parents for path in (output, scratch)):
        parser.error("output and scratch must be outside the source checkout")
    output.mkdir(mode=0o700)
    scratch.mkdir(mode=0o700)
    experiment = Experiment(output)
    previous_handlers = {sig: signal.signal(sig, experiment.cancel) for sig in (signal.SIGINT, signal.SIGTERM)}
    result = 2
    try:
        before = source_pin(ROOT)
        if before["head"] != args.expected_head or before["status"]:
            raise RuntimeError("expected clean diagnostic checkout differs")
        if GATE.sigchld_problem() or not GATE.leader_watches():
            raise RuntimeError("native supervisor cannot retain child exit custody")
        toolchain = experiment.run("rust-toolchain", ["rustc", "-vV"], ROOT, 10)
        if not toolchain["accepted"]:
            raise RuntimeError("toolchain identity could not be recorded")
        version = command_payload(toolchain).decode("utf-8")
        if "release: 1.98.1\n" not in version or "commit-hash: 48a229ceaefd4985c50990b14116b6d856af0985\n" not in version:
            raise RuntimeError("compiler differs from the failed CI subject")
        save(output / "host.json", {"system": platform.platform(), "mac_ver": platform.mac_ver(),
             "machine": platform.machine(), "load": os.getloadavg(), "python": sys.version,
             "image_os": os.environ.get("ImageOS"), "image_version": os.environ.get("ImageVersion"),
             "github_run_id": os.environ.get("GITHUB_RUN_ID"), "github_run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
             "rustc_vV": version, "head": before,
             "HOME_preserved": experiment.environment.get("HOME") == os.environ.get("HOME")})
        baseline = scratch / "baseline"
        clone = experiment.run("baseline-checkout", ["git", "clone", "--no-hardlinks", "--no-checkout", str(ROOT), str(baseline)], ROOT, 60)
        if not clone["accepted"]:
            raise RuntimeError("baseline clone failed")
        checkout = experiment.run("baseline-detach", ["git", "switch", "--detach", BASELINE], baseline, 30)
        if not checkout["accepted"] or source_pin(baseline)["head"] != BASELINE:
            raise RuntimeError("baseline identity failed")
        baseline_pass = experiment.phase(baseline.resolve(), "baseline", scratch / "baseline-target", False)
        probe_pass = experiment.phase(ROOT, "probe", scratch / "probe-target", True)
        if source_pin(ROOT) != before:
            raise RuntimeError("diagnostic checkout changed during experiment")
        experiment.report["verdict"] = "OBSERVED_NO_REPRO" if baseline_pass and probe_pass else "OBSERVED_FAILURE"
        result = 0 if baseline_pass and probe_pass else 1
    except BaseException as error:
        experiment.report["problem"] = str(error)
    finally:
        experiment.cancelled = True
        experiment.report["finished_utc"] = utc()
        experiment.report["exit_code"] = result
        save(output / "summary.json", experiment.report)
        files = [{"path": str(path.relative_to(output)), "sha256": digest(path)}
                 for path in sorted(output.rglob("*")) if path.is_file()]
        save(output / "manifest.json", files)
        print("PTY_DIAGNOSTIC", experiment.report["verdict"], "exit", result, flush=True)
        for sig, handler in previous_handlers.items():
            signal.signal(sig, handler)
    return result


if __name__ == "__main__":
    raise SystemExit(main())
