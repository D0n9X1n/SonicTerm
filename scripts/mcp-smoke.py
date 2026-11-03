#!/usr/bin/env python3
"""Drive a release SonicTerm's MCP control server end to end through the sonicterm-mcp bridge.

The smoke starts the release app with a scratch config directory whose sonicterm.toml enables
`[mcp]` (control "agent"), scratch logs and, on Unix, a scratch XDG_RUNTIME_DIR, so it never
reaches a SonicTerm the user is running. It then speaks JSON-RPC to the release bridge over
stdio: initialize, list_panes, a helper that exits 3, a sleeper stopped with Ctrl+C, and a fake
OSC 133 shell that run_command drives to exit code 5, then closes every helper. A second app run
with `[mcp] enabled = false` proves that the bridge finds no endpoint and exits 1.

The smoke owns the app's process tree and kills it on every exit path; a watchdog ends the whole
run after a global deadline, and every JSON-RPC read has its own timeout.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import posixpath
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from typing import Callable, Mapping, Sequence

ROOT = Path(__file__).resolve().parent.parent
PROGRAM = "mcp-smoke"
DEADLINE_EXIT_CODE = 124
# The whole protocol run, from the first app launch to the second app's exit, excluding builds.
DEFAULT_DEADLINE_S = 120.0
DEFAULT_READ_TIMEOUT_S = 15.0
# Added to a tool call's own timeout_ms, so the bridge read outlives the server's wait.
TOOL_READ_SLACK_S = 10.0
APP_READY_TIMEOUT_S = 45.0
PANES_READY_TIMEOUT_S = 20.0
INTERRUPT_EXIT_LIMIT_S = 10.0
# After the disabled app runs its first shell, how long its endpoint directory must stay empty.
DISABLED_QUIET_S = 1.0
# Packaged binaries CI passes without changing the gate step's command.
APP_ENV = "SONICTERM_SMOKE_APP"
BRIDGE_ENV = "SONICTERM_SMOKE_BRIDGE"
BRIDGE_EXIT_TIMEOUT_S = 10.0
APP_BUILD_TIMEOUT_S = 1800
BRIDGE_BUILD_TIMEOUT_S = 300
LOG_TAIL_LINES = 40
RESPONSE_PREVIEW_CHARS = 2000
PROTOCOL_VERSION = "2025-06-18"
EXIT_HELPER_CODE = 5
EXITING_HELPER_CODE = 3
NO_ENDPOINT_TEXT = "no running SonicTerm control server was found"
# Refusals that mean the fake shell's prompt is not marked or idle yet; run_command retries them.
PROMPT_NOT_READY = ("no_shell_integration", "busy", "draft_present", "input_busy")

# The cargo package, and the binary it builds, for each host's app.
APP_PACKAGES = {"macos": ("sonicterm-mac", "sonicterm-mac"),
                "linux": ("sonicterm-linux", "sonicterm"),
                "windows": ("sonicterm-windows", "sonicterm-windows")}
BRIDGE_PACKAGE = "sonicterm-mcp"

OSC_PROMPT_START = "\x1b]133;A\x07"
OSC_INPUT_START = "\x1b]133;B\x07"
OSC_OUTPUT_START = "\x1b]133;C\x07"

# A line-reading shell that marks every region with OSC 133, so run_command can drive it on any
# host. It is written to the scratch directory and run with the smoke's own interpreter.
FAKE_SHELL_SOURCE = '''\
"""A minimal OSC 133 shell for the SonicTerm MCP smoke: prompt, read a line, run it, report."""
import subprocess
import sys

MARK = "\\x1b]133;{}\\x07"


def emit(text):
    sys.stdout.write(text)
    sys.stdout.flush()


while True:
    emit(MARK.format("A"))
    emit("$ ")
    emit(MARK.format("B"))
    line = sys.stdin.readline()
    if not line:
        break
    emit(MARK.format("C"))
    status = subprocess.run(line.strip(), shell=True).returncode
    emit(MARK.format("D;{}".format(status)))
'''


class SmokeFailure(Exception):
    """One step failed: the step name, why, and the response that showed it, if any."""

    def __init__(self, step: str, message: str, response: object = None) -> None:
        super().__init__(f"{step}: {message}")
        self.step = step
        self.message = message
        self.response = response


def host_name(platform: str = sys.platform) -> str:
    """Return the gate's host name for a `sys.platform` value."""
    if platform == "darwin":
        return "macos"
    if platform.startswith("win"):
        return "windows"
    return "linux"


def executable_name(name: str, host: str) -> str:
    """Add Windows' `.exe` suffix to a binary name."""
    return f"{name}.exe" if host == "windows" else name


def target_dir(environment: Mapping[str, str], root: Path = ROOT) -> Path:
    """Cargo's target directory: a non-empty CARGO_TARGET_DIR, else the repository's `target`."""
    configured = environment.get("CARGO_TARGET_DIR", "")
    if not configured:
        return root / "target"
    path = Path(configured)
    return path if path.is_absolute() else root / path


def default_app_path(host: str, target: Path) -> Path:
    """The release app binary for `host` under `target`."""
    return target / "release" / executable_name(APP_PACKAGES[host][1], host)


def default_bridge_path(host: str, target: Path) -> Path:
    """The release bridge binary for `host` under `target`."""
    return target / "release" / executable_name(BRIDGE_PACKAGE, host)


def build_commands(host: str, build_app: bool, build_bridge: bool) -> list[list[str]]:
    """The release builds the defaults need, one package per command, as CI spells them."""
    commands = []
    if build_app:
        commands.append(["cargo", "build", "--release", "-p", APP_PACKAGES[host][0]])
    if build_bridge:
        commands.append(["cargo", "build", "--release", "-p", BRIDGE_PACKAGE])
    return commands


def exiting_argv(host: str) -> list[str]:
    """A helper that prints `ready` and exits with EXITING_HELPER_CODE."""
    if host == "windows":
        return ["cmd", "/c", f"echo ready & exit {EXITING_HELPER_CODE}"]
    return ["sh", "-c", f"echo ready; exit {EXITING_HELPER_CODE}"]


def sleeper_argv(host: str) -> list[str]:
    """A long-running helper that prints a line once it is running, so Ctrl+C reaches it."""
    if host == "windows":
        return ["ping", "-t", "127.0.0.1"]
    return ["sh", "-c", "echo sleeping; exec sleep 600"]


def sleeper_ready_regex(host: str) -> str:
    """The pattern the sleeper prints once its long-running process owns the pane."""
    return r"127\.0\.0\.1" if host == "windows" else r"sleeping"


def config_text(enabled: bool, control: str = "agent") -> str:
    """The scratch sonicterm.toml: only the [mcp] section, every other key at its default."""
    return f'[mcp]\nenabled = {"true" if enabled else "false"}\ncontrol = "{control}"\n'


def encode_request(request_id: int, method: str, params: Mapping[str, object] | None = None) -> bytes:
    """One JSON-RPC request line."""
    message: dict[str, object] = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        message["params"] = dict(params)
    return (json.dumps(message, separators=(",", ":")) + "\n").encode("utf-8")


def encode_notification(method: str) -> bytes:
    """One JSON-RPC notification line, which gets no response."""
    return (json.dumps({"jsonrpc": "2.0", "method": method}, separators=(",", ":")) + "\n").encode("utf-8")


def decode_line(line: bytes) -> dict[str, object]:
    """Parse one line the bridge relayed; anything but a JSON object is a protocol failure."""
    text = line.decode("utf-8", errors="replace").strip()
    try:
        value = json.loads(text)
    except ValueError as error:
        raise ValueError(f"not JSON: {text[:200]!r} ({error})") from None
    if not isinstance(value, dict):
        raise ValueError(f"not a JSON object: {text[:200]!r}")
    return value


def tool_outcome(response: Mapping[str, object]) -> tuple[bool, dict[str, object]]:
    """Return (is_error, structuredContent) of a tools/call response, raising on a JSON-RPC error."""
    if "error" in response:
        raise ValueError(f"JSON-RPC error {response['error']!r}")
    result = response.get("result")
    if not isinstance(result, dict):
        raise ValueError("response has no result object")
    structured = result.get("structuredContent")
    if not isinstance(structured, dict):
        raise ValueError("tools/call result has no structuredContent object")
    return bool(result.get("isError", False)), structured


def tool_read_timeout(wait_ms: int | None) -> float:
    """How long to wait for a tool's response: its own server-side wait plus slack, else the default."""
    if wait_ms:
        return wait_ms / 1000.0 + TOOL_READ_SLACK_S
    return DEFAULT_READ_TIMEOUT_S


class Deadline:
    """A monotonic deadline; the clock is injectable so tests need no sleeps."""

    def __init__(self, seconds: float, clock: Callable[[], float] = time.monotonic) -> None:
        self.clock = clock
        self.end = clock() + seconds

    def remaining(self) -> float:
        """Seconds left, never negative."""
        return max(0.0, self.end - self.clock())

    def expired(self) -> bool:
        """Whether no time is left."""
        return self.remaining() <= 0.0

    def bound(self, seconds: float) -> float:
        """`seconds` capped by the time left."""
        return min(seconds, self.remaining())


def read_line(lines: "queue.Queue[bytes | None]", timeout_s: float) -> bytes:
    """Take the next line a reader thread queued, within `timeout_s`; None marks the stream's end."""
    try:
        line = lines.get(timeout=max(timeout_s, 0.0))
    except queue.Empty:
        raise TimeoutError(f"no line within {timeout_s:.1f} s") from None
    if line is None:
        raise EOFError("the bridge closed its stdout")
    return line


def smoke_environment(base: Mapping[str, str], scratch: Path, run_name: str, host: str) -> dict[str, str]:
    """The app and bridge environment for one run: scratch config, logs and endpoint directory.

    HOME stays as it is, since a pane's shell inherits it; NO_COLOR and every variable that names
    another SonicTerm's endpoint or pane are dropped.
    """
    environment = dict(base)
    for name in ("NO_COLOR", "SONICTERM_CONTROL", "SONICTERM_PANE_ID", "SONICTERM_RUNTIME_SMOKE_DIR",
                 "SONICTERM_RUNTIME_SMOKE_SCENARIO"):
        environment.pop(name, None)
    run_dir = scratch / run_name
    environment["SONICTERM_CONFIG_DIR"] = str(run_dir / "config")
    environment["SONICTERM_LOG_DIR"] = str(run_dir / "logs")
    if host == "windows":
        # The control pipe's discovery records live under %LOCALAPPDATA%\SonicTerm\control.
        environment["LOCALAPPDATA"] = str(run_dir / "localappdata")
    else:
        original_runtime = base.get("XDG_RUNTIME_DIR", "")
        wayland = base.get("WAYLAND_DISPLAY", "")
        # When: a relative Wayland socket name resolves under the runtime dir being replaced, keep it absolute.
        # POSIX joins on any host: these values are only ever used on macOS and Linux.
        if wayland and not posixpath.isabs(wayland) and original_runtime:
            environment["WAYLAND_DISPLAY"] = posixpath.join(original_runtime, wayland)
        environment["XDG_RUNTIME_DIR"] = str(run_dir / "runtime")
    return environment


def endpoint_dir(environment: Mapping[str, str], host: str) -> Path:
    """Where the app publishes its endpoint under `environment`."""
    if host == "windows":
        return Path(environment["LOCALAPPDATA"]) / "SonicTerm" / "control"
    return Path(environment["XDG_RUNTIME_DIR"]) / "sonicterm"


def endpoint_entry(directory: Path, pid: int, host: str) -> Path:
    """The endpoint entry process `pid` publishes: `<pid>.sock` on Unix, a `<pid>` record on Windows."""
    return directory / (str(pid) if host == "windows" else f"{pid}.sock")


def prepare_run(scratch: Path, run_name: str, environment: Mapping[str, str], enabled: bool, host: str) -> None:
    """Create one run's config, log and endpoint parent directories, owner-only on Unix."""
    config_dir = Path(environment["SONICTERM_CONFIG_DIR"])
    config_dir.mkdir(parents=True)
    (config_dir / "sonicterm.toml").write_text(config_text(enabled), encoding="utf-8")
    Path(environment["SONICTERM_LOG_DIR"]).mkdir(parents=True)
    if host == "windows":
        Path(environment["LOCALAPPDATA"]).mkdir(parents=True)
    else:
        runtime = Path(environment["XDG_RUNTIME_DIR"])
        runtime.mkdir(parents=True)
        os.chmod(runtime, 0o700)
        os.chmod(scratch / run_name, 0o700)


def _load_smoke_runner():
    """Load native-smoke-runner.py for its process-group launch and tree kill."""
    spec = importlib.util.spec_from_file_location("sonicterm_native_smoke_runner",
                                                  Path(__file__).resolve().parent / "native-smoke-runner.py")
    if spec is None or spec.loader is None:
        raise ImportError("cannot load scripts/native-smoke-runner.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SMOKE_RUNNER = _load_smoke_runner()


def _load_process_module():
    """Load mcp_smoke_process.py beside this script without depending on the caller's sys.path."""
    spec = importlib.util.spec_from_file_location("sonicterm_mcp_smoke_process",
                                                  Path(__file__).resolve().parent / "mcp_smoke_process.py")
    if spec is None or spec.loader is None:
        raise ImportError("cannot load scripts/mcp_smoke_process.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PROCESS = _load_process_module()


# One locked, fail-closed cleanup for every process the smoke starts; see mcp_smoke_process.py.
Owned = PROCESS.Owned


class Bridge:
    """The sonicterm-mcp process, with its stdout lines read on a thread so every read is bounded."""

    def __init__(self, argv: Sequence[str], environment: Mapping[str, str], stderr_path: Path,
                 owned: Owned, deadline: Deadline) -> None:
        self.deadline = deadline
        self.stderr_path = stderr_path
        self.stderr_file = open(stderr_path, "wb")
        self.containment = owned.launch(list(argv), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.stderr_file, env=dict(environment))
        self.process = self.containment.process
        self.lines: "queue.Queue[bytes | None]" = queue.Queue()
        self.next_id = 1
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self) -> None:
        stdout = self.process.stdout
        assert stdout is not None
        for line in iter(stdout.readline, b""):
            self.lines.put(line)
        self.lines.put(None)

    def send(self, payload: bytes) -> None:
        """Write one line to the bridge's stdin."""
        assert self.process.stdin is not None
        self.process.stdin.write(payload)
        self.process.stdin.flush()

    def notify(self, method: str) -> None:
        """Send a notification."""
        self.send(encode_notification(method))

    def request(self, step: str, method: str, params: Mapping[str, object] | None = None,
                timeout_s: float = DEFAULT_READ_TIMEOUT_S) -> dict[str, object]:
        """Send a request and return its response, skipping notifications, within a bounded wait."""
        request_id = self.next_id
        self.next_id += 1
        try:
            self.send(encode_request(request_id, method, params))
        except OSError as error:
            # When: the bridge already exited, the write fails; report the step, not a traceback.
            raise SmokeFailure(step, f"{method}: cannot write to the bridge: {error}") from None
        wait = Deadline(self.deadline.bound(timeout_s))
        while True:
            try:
                message = decode_line(read_line(self.lines, wait.remaining()))
            except (TimeoutError, EOFError, ValueError) as error:
                raise SmokeFailure(step, f"{method}: {error}") from None
            # When: the message carries no matching id it is a notification or a stray reply; keep reading.
            if message.get("id") == request_id:
                return message

    def call_tool(self, step: str, name: str, arguments: Mapping[str, object],
                  wait_ms: int | None = None) -> tuple[bool, dict[str, object], dict[str, object]]:
        """Call one tool: (is_error, structuredContent, raw response)."""
        timeout_s = tool_read_timeout(wait_ms)
        response = self.request(step, "tools/call", {"name": name, "arguments": dict(arguments)}, timeout_s)
        try:
            is_error, structured = tool_outcome(response)
        except ValueError as error:
            raise SmokeFailure(step, f"{name}: {error}", response) from None
        return is_error, structured, response

    def tool(self, step: str, name: str, arguments: Mapping[str, object],
             wait_ms: int | None = None) -> dict[str, object]:
        """Call one tool that must succeed and return its structuredContent."""
        is_error, structured, response = self.call_tool(step, name, arguments, wait_ms)
        if is_error:
            raise SmokeFailure(step, f"{name} refused: {structured.get('error')}: {structured.get('message')}",
                               response)
        return structured

    def close(self) -> int | None:
        """Close stdin, which ends the relay, and return the bridge's exit code, or None if it hung."""
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
        except OSError:
            pass
        try:
            return self.process.wait(timeout=self.deadline.bound(BRIDGE_EXIT_TIMEOUT_S))
        except subprocess.TimeoutExpired:
            return None
        finally:
            self.stderr_file.close()


def report(message: str) -> None:
    """Print one progress line."""
    print(f"{PROGRAM}: {message}", flush=True)


def tail(path: Path, line_count: int = LOG_TAIL_LINES) -> str:
    """The last `line_count` lines of a file, or a note when it cannot be read."""
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as error:
        return f"(cannot read {path}: {error})"
    return "\n".join(lines[-line_count:]) if lines else "(empty)"


def start_app(app: Path, environment: Mapping[str, str], cwd: Path, output_path: Path, owned: Owned):
    """Launch the app normally, contained with every descendant it starts, with output to a file."""
    with open(output_path, "wb") as output:
        return owned.launch([str(app)], cwd=str(cwd), env=dict(environment), stdin=subprocess.DEVNULL,
                            stdout=output, stderr=subprocess.STDOUT)


def wait_until(step: str, condition: Callable[[], bool], timeout_s: float, deadline: Deadline,
               app: subprocess.Popen, what: str, poll_s: float = 0.1) -> None:
    """Poll `condition` until it holds, failing if the app exits or the time runs out."""
    wait = Deadline(deadline.bound(timeout_s))
    while not condition():
        if app.poll() is not None:
            raise SmokeFailure(step, f"the app exited with code {app.returncode} before {what}")
        if wait.expired():
            raise SmokeFailure(step, f"timed out waiting for {what}")
        time.sleep(poll_s)


def start_virtual_display(environment: dict[str, str], scratch: Path, owned: Owned) -> None:
    """On a Linux host with no display, start an owned Xvfb and select software Vulkan, as CI does."""
    if environment.get("DISPLAY") or environment.get("WAYLAND_DISPLAY"):
        return
    xvfb = shutil.which("Xvfb")
    if xvfb is None:
        raise SmokeFailure("display", "no DISPLAY or WAYLAND_DISPLAY, and Xvfb is not installed")
    read_end, write_end = os.pipe()
    with open(scratch / "xvfb.log", "wb") as output:
        process = owned.launch(
            [xvfb, "-displayfd", str(write_end), "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
            stdout=output, stderr=subprocess.STDOUT, pass_fds=(write_end,)).process
    os.close(write_end)
    number = b""
    wait = Deadline(15.0)
    try:
        # Xvfb writes the display number it chose and a newline once it accepts clients.
        while not number.endswith(b"\n") and not wait.expired():
            chunk = os.read(read_end, 16)
            if not chunk:
                break
            number += chunk
    finally:
        os.close(read_end)
    if not number.strip().isdigit():
        raise SmokeFailure("display", f"Xvfb did not report a display (exit {process.poll()}): "
                                      f"{tail(scratch / 'xvfb.log')}")
    environment["DISPLAY"] = f":{number.strip().decode()}"
    lavapipe = "/usr/share/vulkan/icd.d/lvp_icd.x86_64.json"
    if os.path.isfile(lavapipe):
        environment.setdefault("VK_ICD_FILENAMES", lavapipe)
        environment.setdefault("WGPU_BACKEND", "vulkan")
        environment.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
    report(f"started Xvfb on {environment['DISPLAY']}")


def first_pane(listing: Mapping[str, object]) -> str | None:
    """The active pane's id from a list_panes result, else the first pane's, else None."""
    panes = listing.get("panes")
    if not isinstance(panes, list) or not panes:
        return None
    active = [pane for pane in panes if isinstance(pane, dict) and pane.get("active")]
    chosen = active[0] if active else panes[0]
    return str(chosen.get("pane_id")) if isinstance(chosen, dict) else None


def pane_ids(listing: Mapping[str, object]) -> set[str]:
    """Every pane id in a list_panes result."""
    panes = listing.get("panes")
    if not isinstance(panes, list):
        return set()
    return {str(pane.get("pane_id")) for pane in panes if isinstance(pane, dict)}


def expect_equal(step: str, label: str, actual: object, expected: object, response: object) -> None:
    """Fail `step` unless `actual` equals `expected`."""
    if actual != expected:
        raise SmokeFailure(step, f"{label} was {actual!r}, expected {expected!r}", response)


def stop_app(containment, owned: Owned, step: str) -> None:
    """Kill a contained process and every descendant now, failing `step` unless their deaths are confirmed."""
    problems = owned.cleanup(containment)
    if problems:
        raise SmokeFailure(step, "cleanup failed: " + "; ".join(problems))


def interrupt_sleeper(bridge: Bridge, step: str, sleeper: str,
                      clock: Callable[[], float] = time.monotonic) -> tuple[float, dict[str, object]]:
    """Send Ctrl+C and require the pane's process to exit within INTERRUPT_EXIT_LIMIT_S of the keys.

    The clock starts before send_keys, so the bound covers delivery as well as the exit.
    """
    limit_ms = int(INTERRUPT_EXIT_LIMIT_S * 1000)
    started = clock()
    bridge.tool(step, "send_keys", {"pane_id": sleeper, "keys": ["Ctrl+C"]})
    waited = bridge.tool(step, "wait_for", {"pane_id": sleeper, "exit": True, "timeout_ms": limit_ms}, limit_ms)
    elapsed = clock() - started
    expect_equal(step, "reason", waited.get("reason"), "exited", waited)
    if elapsed > INTERRUPT_EXIT_LIMIT_S:
        raise SmokeFailure(step, f"the sleeper exited {elapsed:.1f} s after Ctrl+C, over "
                                 f"{INTERRUPT_EXIT_LIMIT_S:.0f} s", waited)
    return elapsed, waited


def close_helpers(bridge: Bridge, helpers: Sequence[str]) -> None:
    """Close every helper pane through a running bridge, then require the bridge to exit 0 on EOF.

    A pane held after its process exited still needs closing, so each helper is closed by id and
    each close must succeed and name its pane.
    """
    step = "close_pane"
    if bridge.process.poll() is not None:
        raise SmokeFailure(step, f"the bridge exited with code {bridge.process.returncode} before close_pane",
                           tail(bridge.stderr_path))
    for pane in helpers:
        closed = bridge.tool(step, "close_pane", {"pane_id": pane})
        expect_equal(step, f"close_pane {pane} pane_id", closed.get("pane_id"), pane, closed)
    remaining = pane_ids(bridge.tool(step, "list_panes", {})) & set(helpers)
    if remaining:
        raise SmokeFailure(step, f"helper panes still open: {sorted(remaining)}")
    code = bridge.close()
    expect_equal("shutdown", "bridge exit code after stdin closed", code, 0, tail(bridge.stderr_path))


def run_enabled(args: argparse.Namespace, host: str, scratch: Path, base: Mapping[str, str],
                owned: Owned, deadline: Deadline) -> None:
    """The protocol run against an app with [mcp] enabled."""
    environment = smoke_environment(base, scratch, "enabled", host)
    prepare_run(scratch, "enabled", environment, True, host)
    app = start_app(args.app, environment, scratch, scratch / "enabled" / "app-output.log", owned)
    entry = endpoint_entry(endpoint_dir(environment, host), app.process.pid, host)
    wait_until("start", entry.exists, APP_READY_TIMEOUT_S, deadline, app.process, f"the endpoint {entry}")
    report(f"app pid {app.process.pid} published {entry}")

    bridge = Bridge([str(args.bridge)], environment, scratch / "enabled" / "bridge-stderr.log", owned, deadline)
    step = "initialize"
    response = bridge.request(step, "initialize", {
        "protocolVersion": PROTOCOL_VERSION, "capabilities": {},
        "clientInfo": {"name": PROGRAM, "version": "1"}})
    result = response.get("result")
    if not isinstance(result, dict):
        raise SmokeFailure(step, "no result", response)
    expect_equal(step, "protocolVersion", result.get("protocolVersion"), PROTOCOL_VERSION, response)
    server = result.get("serverInfo")
    expect_equal(step, "serverInfo.name", server.get("name") if isinstance(server, dict) else None,
                 "sonicterm", response)
    bridge.notify("notifications/initialized")
    report(f"[1/6] initialize ok (protocol {PROTOCOL_VERSION}, through the discovered endpoint)")

    step = "list_panes"
    anchor: str | None = None
    wait = Deadline(deadline.bound(PANES_READY_TIMEOUT_S))
    listing: dict[str, object] = {}
    while anchor is None:
        listing = bridge.tool(step, "list_panes", {})
        anchor = first_pane(listing)
        if anchor is None:
            if wait.expired():
                raise SmokeFailure(step, "no pane appeared", listing)
            time.sleep(0.2)
    expect_equal(step, "control", listing.get("control"), "agent", listing)
    report(f"[2/6] list_panes ok (anchor pane {anchor}, {len(pane_ids(listing))} pane(s))")

    helpers: list[str] = []
    steps_passed = False
    try:
        step = "exit-code"
        created = bridge.tool(step, "create_pane", {"anchor_pane_id": anchor, "placement": "right",
                                                    "argv": exiting_argv(host), "title": "mcp-smoke exit"})
        expect_equal(step, "started", created.get("started"), True, created)
        exiting = str(created.get("pane_id"))
        helpers.append(exiting)
        waited = bridge.tool(step, "wait_for", {"pane_id": exiting, "exit": True, "timeout_ms": 20000}, 20000)
        expect_equal(step, "reason", waited.get("reason"), "exited", waited)
        expect_equal(step, "exit_code", waited.get("exit_code"), EXITING_HELPER_CODE, waited)
        # The exit can be seen before the parser has read the last output, so the tail is polled briefly.
        wait = Deadline(deadline.bound(5.0))
        while True:
            read = bridge.tool(step, "read_pane", {"pane_id": exiting, "mode": "tail"})
            lines = read.get("lines")
            if isinstance(lines, list) and any("ready" in str(line) for line in lines):
                break
            if wait.expired():
                raise SmokeFailure(step, "read_pane tail has no 'ready' line", read)
            time.sleep(0.2)
        report(f"[3/6] create_pane {exiting} exited with code {EXITING_HELPER_CODE}; its tail holds 'ready'")

        step = "interrupt"
        created = bridge.tool(step, "create_pane", {"anchor_pane_id": anchor, "placement": "down",
                                                    "argv": sleeper_argv(host), "title": "mcp-smoke sleeper"})
        expect_equal(step, "started", created.get("started"), True, created)
        sleeper = str(created.get("pane_id"))
        helpers.append(sleeper)
        ready = bridge.tool(step, "wait_for", {"pane_id": sleeper, "regex": sleeper_ready_regex(host),
                                               "timeout_ms": 20000}, 20000)
        expect_equal(step, "reason", ready.get("reason"), "matched", ready)
        elapsed, waited = interrupt_sleeper(bridge, step, sleeper)
        report(f"[4/6] Ctrl+C ended sleeper pane {sleeper} {elapsed:.1f} s after send_keys "
               f"(exit code {waited.get('exit_code')})")

        step = "run_command"
        shell_path = scratch / "fake-osc133-shell.py"
        shell_path.write_text(FAKE_SHELL_SOURCE, encoding="utf-8")
        created = bridge.tool(step, "create_pane", {"anchor_pane_id": anchor, "placement": "tab",
                                                    "argv": [sys.executable, "-u", str(shell_path)],
                                                    "cwd": str(scratch), "title": "mcp-smoke shell"})
        expect_equal(step, "started", created.get("started"), True, created)
        shell = str(created.get("pane_id"))
        helpers.append(shell)
        prompt = bridge.tool(step, "wait_for", {"pane_id": shell, "regex": r"^\$", "timeout_ms": 20000}, 20000)
        expect_equal(step, "reason", prompt.get("reason"), "matched", prompt)
        # The prompt text precedes its B mark, so a refusal that says the prompt is not marked yet is retried.
        wait = Deadline(deadline.bound(10.0))
        while True:
            is_error, outcome, response = bridge.call_tool(
                step, "run_command", {"pane_id": shell, "command": f"exit {EXIT_HELPER_CODE}",
                                      "timeout_ms": 20000}, 20000)
            if not is_error:
                break
            if outcome.get("error") not in PROMPT_NOT_READY or wait.expired():
                raise SmokeFailure(step, f"run_command refused: {outcome.get('error')}: {outcome.get('message')}",
                                   response)
            time.sleep(0.2)
        expect_equal(step, "state", outcome.get("state"), "completed", outcome)
        expect_equal(step, "exit_code", outcome.get("exit_code"), EXIT_HELPER_CODE, outcome)
        report(f"[5/6] run_command in OSC 133 pane {shell} completed with exit code {EXIT_HELPER_CODE}")
        steps_passed = True
    finally:
        if not steps_passed and bridge.process.poll() is None and not deadline.expired():
            # When: an earlier step failed, close what can be closed; that failure is the one reported.
            for pane in helpers:
                try:
                    bridge.call_tool("cleanup", "close_pane", {"pane_id": pane})
                except SmokeFailure:
                    break
    close_helpers(bridge, helpers)
    # The bridge has exited; its cleanup confirms nothing it started is still running.
    stop_app(bridge.containment, owned, "shutdown")
    report(f"[6/6] close_pane closed helper panes {', '.join(helpers)}; the bridge exited 0 on EOF")
    stop_app(app, owned, "shutdown")


def wait_for_child(step: str, app, timeout_s: float, deadline: Deadline) -> int:
    """Wait, bounded, until the contained app has a child process (its first pane's shell); return the count."""
    wait = Deadline(deadline.bound(timeout_s))
    while True:
        if app.process.poll() is not None:
            raise SmokeFailure(step, f"the app exited with code {app.process.returncode} before spawning a shell")
        try:
            count = app.descendant_count()
        except (RuntimeError, OSError) as error:
            # When: the process table or job cannot be read, readiness is unknown, which is a failure.
            raise SmokeFailure(step, f"cannot read the app's processes: {error}") from None
        if count:
            return count
        if wait.expired():
            raise SmokeFailure(step, f"the app spawned no child process within {timeout_s:.0f} s")
        time.sleep(0.2)


def run_disabled(args: argparse.Namespace, host: str, scratch: Path, base: Mapping[str, str],
                 owned: Owned, deadline: Deadline, ready_timeout_s: float | None = None) -> None:
    """A second app with [mcp] disabled: once it runs a shell, no endpoint appears and the bridge exits 1."""
    step = "disabled"
    environment = smoke_environment(base, scratch, "disabled", host)
    prepare_run(scratch, "disabled", environment, False, host)
    app = start_app(args.app, environment, scratch, scratch / "disabled" / "app-output.log", owned)
    timeout_s = APP_READY_TIMEOUT_S if ready_timeout_s is None else ready_timeout_s
    children = wait_for_child(step, app, timeout_s, deadline)
    # The app is past startup once its first pane runs a shell; an enabled server would publish by now.
    directory = endpoint_dir(environment, host)
    quiet = Deadline(deadline.bound(DISABLED_QUIET_S))
    while not quiet.expired():
        published = sorted(path.name for path in directory.iterdir()) if directory.is_dir() else []
        if published:
            raise SmokeFailure(step, f"an endpoint appeared with [mcp] enabled = false: {published}")
        if app.process.poll() is not None:
            raise SmokeFailure(step, f"the app exited with code {app.process.returncode}")
        time.sleep(0.1)
    try:
        completed = subprocess.run([str(args.bridge)], env=environment, stdin=subprocess.DEVNULL,
                                   capture_output=True, timeout=deadline.bound(BRIDGE_EXIT_TIMEOUT_S))
    except subprocess.TimeoutExpired:
        raise SmokeFailure(step, "the bridge did not exit with no endpoint") from None
    stderr = completed.stderr.decode("utf-8", errors="replace")
    expect_equal(step, "bridge exit code", completed.returncode, 1, stderr)
    if NO_ENDPOINT_TEXT not in stderr:
        raise SmokeFailure(step, f"bridge stderr does not say {NO_ENDPOINT_TEXT!r}", stderr)
    stop_app(app, owned, step)
    report(f"with [mcp] enabled = false ({children} child process(es) running, no endpoint for "
           f"{DISABLED_QUIET_S:.0f} s) the bridge exits 1: {stderr.strip().splitlines()[0]}")


def parse_args(argv: Sequence[str], host: str, environment: Mapping[str, str]) -> argparse.Namespace:
    """Parse the command line; app and bridge come from the flags, then SONICTERM_SMOKE_APP and
    SONICTERM_SMOKE_BRIDGE (packaged binaries in CI), then the release builds in Cargo's target dir."""
    parser = argparse.ArgumentParser(prog=PROGRAM, description=__doc__.splitlines()[0])
    parser.add_argument("--app", type=Path, help=f"the app binary (else ${APP_ENV}, else the release build)")
    parser.add_argument("--bridge", type=Path,
                        help=f"the sonicterm-mcp binary (else ${BRIDGE_ENV}, else the release build)")
    parser.add_argument("--no-build", action="store_true", help="never run cargo; use existing binaries")
    parser.add_argument("--deadline-seconds", type=float, default=DEFAULT_DEADLINE_S,
                        help="the whole protocol run's bound, excluding builds")
    parser.add_argument("--keep-scratch", action="store_true", help="keep the scratch directory")
    args = parser.parse_args(list(argv))
    if args.deadline_seconds <= 0:
        parser.error("--deadline-seconds must be positive")
    if args.app is None and environment.get(APP_ENV):
        args.app = Path(environment[APP_ENV])
    if args.bridge is None and environment.get(BRIDGE_ENV):
        args.bridge = Path(environment[BRIDGE_ENV])
    target = target_dir(environment)
    args.build = [] if args.no_build else build_commands(host, args.app is None, args.bridge is None)
    args.app = (args.app or default_app_path(host, target)).resolve()
    args.bridge = (args.bridge or default_bridge_path(host, target)).resolve()
    return args


def run_builds(commands: Sequence[Sequence[str]], environment: Mapping[str, str]) -> None:
    """Run each release build, bounded, with its output streamed through."""
    timeouts = {BRIDGE_PACKAGE: BRIDGE_BUILD_TIMEOUT_S}
    for command in commands:
        report("build: " + " ".join(command))
        timeout_s = timeouts.get(command[-1], APP_BUILD_TIMEOUT_S)
        process = subprocess.Popen(list(command), cwd=str(ROOT), env=dict(environment),
                                   **SMOKE_RUNNER.process_group_options())
        try:
            code = process.wait(timeout=timeout_s)
        except subprocess.TimeoutExpired:
            SMOKE_RUNNER.terminate_process_tree(process)
            process.wait(timeout=10)
            raise SmokeFailure("build", f"{' '.join(command)} timed out after {timeout_s} s") from None
        if code != 0:
            raise SmokeFailure("build", f"{' '.join(command)} exited with code {code}")


def print_failure(failure: SmokeFailure, scratch: Path | None) -> None:
    """Print the failed step, its response, and the app, bridge and log tails."""
    print(f"{PROGRAM}: FAIL step={failure.step}: {failure.message}", file=sys.stderr, flush=True)
    if failure.response is not None:
        text = failure.response if isinstance(failure.response, str) else json.dumps(failure.response)
        print(f"{PROGRAM}: response: {text[:RESPONSE_PREVIEW_CHARS]}", file=sys.stderr, flush=True)
    if scratch is None:
        return
    for run_name in ("enabled", "disabled"):
        for relative in ("app-output.log", "bridge-stderr.log", "logs/sonicterm.log"):
            path = scratch / run_name / relative
            if path.exists():
                print(f"{PROGRAM}: --- tail of {path} ---\n{tail(path)}", file=sys.stderr, flush=True)


def print_cleanup_problems(problems: Sequence[str]) -> None:
    """Report each cleanup problem; an unknown or failed cleanup is never silent."""
    for problem in problems:
        print(f"{PROGRAM}: FAIL cleanup: {problem}", file=sys.stderr, flush=True)


def make_scratch(host: str) -> Path:
    """A fresh scratch directory with a short path, so a Unix socket path inside stays under 104 bytes."""
    base = None if host == "windows" else "/tmp"
    return Path(tempfile.mkdtemp(prefix="stmcp-", dir=base)).resolve()


def main(argv: Sequence[str] | None = None) -> int:
    host = host_name()
    base = dict(os.environ)
    base.pop("NO_COLOR", None)
    args = parse_args(sys.argv[1:] if argv is None else argv, host, base)
    owned = Owned()
    scratch: Path | None = None
    try:
        run_builds(args.build, base)
        for label, path in (("app", args.app), ("bridge", args.bridge)):
            if not path.is_file():
                raise SmokeFailure("setup", f"the {label} binary {path} does not exist")
        deadline = Deadline(args.deadline_seconds)

        def expire() -> None:
            # The watchdog: it runs the same locked cleanup, waits for any in progress, then exits.
            print(f"{PROGRAM}: FAIL deadline: the run passed {args.deadline_seconds:.0f} s",
                  file=sys.stderr, flush=True)
            owned.expire(print_cleanup_problems, os._exit, DEADLINE_EXIT_CODE)

        watchdog = threading.Timer(args.deadline_seconds + 5.0, expire)
        watchdog.daemon = True
        watchdog.start()
        scratch = make_scratch(host)
        report(f"host={host} app={args.app} bridge={args.bridge} scratch={scratch}")
        try:
            if host == "linux":
                start_virtual_display(base, scratch, owned)
            run_enabled(args, host, scratch, base, owned, deadline)
            run_disabled(args, host, scratch, base, owned, deadline)
        finally:
            problems = owned.cleanup()
            watchdog.cancel()
        if problems:
            raise SmokeFailure("cleanup", "; ".join(problems))
    except SmokeFailure as failure:
        problems = owned.cleanup()
        print_failure(failure, scratch)
        if failure.step != "cleanup":
            print_cleanup_problems(problems)
        if scratch is not None:
            print(f"{PROGRAM}: scratch kept at {scratch}", file=sys.stderr, flush=True)
        return 1
    except BaseException:
        print_cleanup_problems(owned.cleanup())
        raise
    if scratch is not None and not args.keep_scratch:
        shutil.rmtree(scratch, ignore_errors=True)
    report("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
