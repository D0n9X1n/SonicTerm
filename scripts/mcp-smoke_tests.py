#!/usr/bin/env python3
"""Unit tests for the MCP end-to-end smoke's pure helpers: framing, the fake shell, defaults, timeouts."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import tempfile
import unittest

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location("mcp_smoke", _HERE / "mcp-smoke.py")
assert _SPEC is not None and _SPEC.loader is not None
smoke = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(smoke)


class FramingTests(unittest.TestCase):
    def test_request_is_one_compact_json_rpc_line(self):
        # The bridge relays newline-delimited JSON, so a request must be exactly one line ending in \n.
        line = smoke.encode_request(7, "tools/call", {"name": "list_panes", "arguments": {}})
        self.assertTrue(line.endswith(b"\n"))
        self.assertEqual(line.count(b"\n"), 1)
        self.assertEqual(json.loads(line), {"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                                            "params": {"name": "list_panes", "arguments": {}}})

    def test_request_without_params_omits_the_key(self):
        # A method with no parameters sends no `params` member rather than a null one.
        self.assertNotIn("params", json.loads(smoke.encode_request(1, "ping")))

    def test_notification_has_no_id(self):
        # A notification carries no id, so the server sends no response the smoke would wait for.
        message = json.loads(smoke.encode_notification("notifications/initialized"))
        self.assertEqual(message, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    def test_decode_accepts_objects_and_rejects_anything_else(self):
        # A relayed line that is not a JSON object is a protocol failure, never silently skipped.
        self.assertEqual(smoke.decode_line(b'{"id":1,"result":{}}\r\n'), {"id": 1, "result": {}})
        for bad in (b"not json\n", b"[1, 2]\n", b"\n"):
            with self.subTest(line=bad), self.assertRaises(ValueError):
                smoke.decode_line(bad)

    def test_tool_outcome_reads_structured_content_and_the_error_flag(self):
        # Tool refusals are results with isError; JSON-RPC errors and shapeless results raise.
        ok_response = {"id": 1, "result": {"structuredContent": {"reason": "exited"}, "isError": False}}
        self.assertEqual(smoke.tool_outcome(ok_response), (False, {"reason": "exited"}))
        refused = {"id": 1, "result": {"structuredContent": {"error": "busy"}, "isError": True}}
        self.assertEqual(smoke.tool_outcome(refused), (True, {"error": "busy"}))
        for bad in ({"id": 1, "error": {"code": -32601}}, {"id": 1}, {"id": 1, "result": {"content": []}}):
            with self.subTest(response=bad), self.assertRaises(ValueError):
                smoke.tool_outcome(bad)


class FakeShellTests(unittest.TestCase):
    def test_fake_shell_marks_each_region_and_reports_the_status(self):
        # run_command needs A, B before input, C after it and D;<status>; EOF must end the shell.
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "shell.py"
            script.write_text(smoke.FAKE_SHELL_SOURCE, encoding="utf-8")
            completed = subprocess.run([sys.executable, "-u", str(script)], input=b"exit 5\n",
                                       capture_output=True, timeout=30)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        output = completed.stdout.decode("utf-8")
        prompt = smoke.OSC_PROMPT_START + "$ " + smoke.OSC_INPUT_START
        self.assertEqual(output, prompt + smoke.OSC_OUTPUT_START + "\x1b]133;D;5\x07" + prompt)

    def test_fake_shell_source_compiles(self):
        # The source is written verbatim to the scratch dir, so a syntax error would only show in the app.
        compile(smoke.FAKE_SHELL_SOURCE, "fake-osc133-shell.py", "exec")


class DefaultTests(unittest.TestCase):
    def test_host_names_follow_sys_platform(self):
        # Each host selects its app package, binary name and helper commands from this name.
        self.assertEqual(smoke.host_name("darwin"), "macos")
        self.assertEqual(smoke.host_name("win32"), "windows")
        self.assertEqual(smoke.host_name("linux"), "linux")

    def test_default_binaries_are_the_release_builds_in_cargo_target(self):
        # The issue's defaults: target/release/sonicterm-mac, sonicterm, sonicterm-windows.exe and the bridge.
        target = Path("/repo/target")
        self.assertEqual(smoke.default_app_path("macos", target), target / "release" / "sonicterm-mac")
        self.assertEqual(smoke.default_app_path("linux", target), target / "release" / "sonicterm")
        self.assertEqual(smoke.default_app_path("windows", target), target / "release" / "sonicterm-windows.exe")
        self.assertEqual(smoke.default_bridge_path("linux", target), target / "release" / "sonicterm-mcp")
        self.assertEqual(smoke.default_bridge_path("windows", target), target / "release" / "sonicterm-mcp.exe")

    def test_target_dir_honors_cargo_target_dir(self):
        # An isolated run's CARGO_TARGET_DIR is where its builds land; empty means the repository's target.
        root = Path("/repo")
        self.assertEqual(smoke.target_dir({}, root), root / "target")
        self.assertEqual(smoke.target_dir({"CARGO_TARGET_DIR": ""}, root), root / "target")
        self.assertEqual(smoke.target_dir({"CARGO_TARGET_DIR": "/scratch/out"}, root), Path("/scratch/out"))
        self.assertEqual(smoke.target_dir({"CARGO_TARGET_DIR": "out"}, root), root / "out")

    def test_builds_run_only_for_defaulted_binaries_one_package_each(self):
        # CI builds the app and bridge in separate commands, so the smoke's no-op rebuild matches them.
        self.assertEqual(smoke.build_commands("macos", True, True), [
            ["cargo", "build", "--release", "-p", "sonicterm-mac"],
            ["cargo", "build", "--release", "-p", "sonicterm-mcp"]])
        self.assertEqual(smoke.build_commands("linux", False, True),
                         [["cargo", "build", "--release", "-p", "sonicterm-mcp"]])
        self.assertEqual(smoke.build_commands("windows", False, False), [])

    def test_explicit_paths_and_no_build_skip_cargo(self):
        # --app and --bridge name prebuilt binaries; --no-build never runs cargo at all.
        args = smoke.parse_args(["--app", "/bin/app", "--bridge", "/bin/bridge"], "macos", {})
        self.assertEqual(args.build, [])
        self.assertEqual(args.app, Path("/bin/app").resolve())
        args = smoke.parse_args(["--no-build"], "linux", {"CARGO_TARGET_DIR": "/out"})
        self.assertEqual(args.build, [])
        self.assertEqual(args.bridge, Path("/out/release/sonicterm-mcp").resolve())
        self.assertEqual(args.deadline_seconds, smoke.DEFAULT_DEADLINE_S)

    def test_helper_commands_match_the_issue(self):
        # The exiting helper prints `ready` and exits 3; the sleeper outlives the Ctrl+C bound unless interrupted.
        self.assertEqual(smoke.exiting_argv("macos"), ["sh", "-c", "echo ready; exit 3"])
        self.assertEqual(smoke.exiting_argv("windows"), ["cmd", "/c", "echo ready & exit 3"])
        self.assertIn("sleep 600", smoke.sleeper_argv("linux")[-1])
        self.assertEqual(smoke.sleeper_argv("windows"), ["ping", "-t", "127.0.0.1"])

    def test_config_enables_or_disables_only_the_mcp_section(self):
        # The scratch config turns the server on with agent scope, or off for the no-endpoint run.
        self.assertEqual(smoke.config_text(True), '[mcp]\nenabled = true\ncontrol = "agent"\n')
        self.assertIn("enabled = false", smoke.config_text(False))


class EnvironmentTests(unittest.TestCase):
    def test_unix_run_isolates_config_logs_and_endpoint_but_keeps_home(self):
        # A scratch endpoint dir keeps the bridge from finding the user's SonicTerm; HOME reaches the pane shell.
        base = {"HOME": "/home/me", "NO_COLOR": "1", "SONICTERM_CONTROL": "/x.sock", "SONICTERM_PANE_ID": "4",
                "XDG_RUNTIME_DIR": "/run/user/1", "WAYLAND_DISPLAY": "wayland-0"}
        environment = smoke.smoke_environment(base, Path("/tmp/stmcp"), "enabled", "linux")
        self.assertEqual(environment["HOME"], "/home/me")
        for name in ("NO_COLOR", "SONICTERM_CONTROL", "SONICTERM_PANE_ID"):
            self.assertNotIn(name, environment)
        self.assertEqual(environment["SONICTERM_CONFIG_DIR"], str(Path("/tmp/stmcp/enabled/config")))
        self.assertEqual(environment["SONICTERM_LOG_DIR"], str(Path("/tmp/stmcp/enabled/logs")))
        self.assertEqual(environment["XDG_RUNTIME_DIR"], str(Path("/tmp/stmcp/enabled/runtime")))
        self.assertEqual(environment["WAYLAND_DISPLAY"], "/run/user/1/wayland-0")
        self.assertEqual(smoke.endpoint_dir(environment, "linux"), Path("/tmp/stmcp/enabled/runtime/sonicterm"))
        self.assertEqual(smoke.endpoint_entry(Path("/d"), 42, "macos"), Path("/d/42.sock"))

    def test_windows_run_isolates_the_discovery_records(self):
        # The Windows pipe is found through %LOCALAPPDATA%\SonicTerm\control\<pid>, so that root is scratch.
        environment = smoke.smoke_environment({"LOCALAPPDATA": "C:/Users/me"}, Path("/s"), "disabled", "windows")
        self.assertEqual(environment["LOCALAPPDATA"], str(Path("/s/disabled/localappdata")))
        self.assertNotIn("XDG_RUNTIME_DIR", environment)
        self.assertEqual(smoke.endpoint_dir(environment, "windows"),
                         Path("/s/disabled/localappdata") / "SonicTerm" / "control")
        self.assertEqual(smoke.endpoint_entry(Path("/d"), 42, "windows"), Path("/d/42"))


class TimeoutTests(unittest.TestCase):
    def test_deadline_counts_down_and_caps_waits(self):
        # Every bounded wait is capped by the global deadline, and the remainder never goes negative.
        now = [100.0]
        deadline = smoke.Deadline(10.0, clock=lambda: now[0])
        self.assertEqual(deadline.bound(15.0), 10.0)
        now[0] = 107.0
        self.assertAlmostEqual(deadline.remaining(), 3.0)
        self.assertEqual(deadline.bound(1.0), 1.0)
        self.assertFalse(deadline.expired())
        now[0] = 200.0
        self.assertEqual(deadline.remaining(), 0.0)
        self.assertTrue(deadline.expired())

    def test_read_line_times_out_and_reports_the_stream_end(self):
        # A silent bridge must fail the read within its timeout; a closed stdout is an EOF, not a hang.
        lines: queue.Queue = queue.Queue()
        with self.assertRaises(TimeoutError):
            smoke.read_line(lines, 0.01)
        lines.put(b"{}\n")
        self.assertEqual(smoke.read_line(lines, 0.01), b"{}\n")
        lines.put(None)
        with self.assertRaises(EOFError):
            smoke.read_line(lines, 0.01)

    def test_tool_reads_outlive_the_server_side_wait(self):
        # A wait_for of N ms gets N ms plus slack before the read times out; other calls get the default.
        self.assertEqual(smoke.tool_read_timeout(None), smoke.DEFAULT_READ_TIMEOUT_S)
        self.assertEqual(smoke.tool_read_timeout(20000), 20.0 + smoke.TOOL_READ_SLACK_S)

    def test_negative_deadline_is_refused(self):
        # A zero or negative global deadline would make every step fail at once, so it is a usage error.
        with self.assertRaises(SystemExit):
            smoke.parse_args(["--deadline-seconds", "0"], "macos", {})


class ListingTests(unittest.TestCase):
    def test_anchor_is_the_active_pane_else_the_first(self):
        # The helpers anchor on the pane the user is looking at when list_panes marks one active.
        listing = {"panes": [{"pane_id": "1", "active": False}, {"pane_id": "2", "active": True}]}
        self.assertEqual(smoke.first_pane(listing), "2")
        self.assertEqual(smoke.first_pane({"panes": [{"pane_id": "5"}]}), "5")
        self.assertIsNone(smoke.first_pane({"panes": []}))
        self.assertEqual(smoke.pane_ids(listing), {"1", "2"})


POSIX_ONLY = unittest.skipIf(sys.platform.startswith("win"), "POSIX shell fixtures")

# A stand-in bridge: answers close_pane and list_panes, then exits on EOF with the code its mode names.
FIXTURE_BRIDGE = """\
import json, sys
mode = sys.argv[1]
if mode == "exit":
    sys.exit(0)
for line in sys.stdin:
    request = json.loads(line)
    arguments = request.get("params", {}).get("arguments", {})
    name = request.get("params", {}).get("name")
    is_error = False
    if name == "close_pane":
        structured = {"pane_id": "999" if mode == "wrong" else arguments["pane_id"]}
        if mode == "refuse":
            is_error, structured = True, {"error": "pane_gone", "message": "no pane"}
    else:
        structured = {"panes": [{"pane_id": "1"}]}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"],
                      "result": {"structuredContent": structured, "isError": is_error}}), flush=True)
sys.exit(3 if mode == "badexit" else 0)
"""


class FakeToolBridge:
    """Answers tools from a table and records each call, for helpers that only use Bridge.tool."""

    def __init__(self, answers):
        self.answers = answers
        self.calls = []

    def tool(self, step, name, arguments, wait_ms=None):
        self.calls.append((name, dict(arguments), wait_ms))
        return self.answers[name]


class InterruptTests(unittest.TestCase):
    def test_clock_starts_before_send_keys_and_wait_uses_the_ten_second_bound(self):
        # The 10 s bound covers key delivery too, and the server-side wait itself is capped at 10000 ms.
        bridge = FakeToolBridge({"send_keys": {"bytes": 1}, "wait_for": {"reason": "exited", "exit_code": None}})
        ticks = iter([100.0, 104.0])
        elapsed, _waited = smoke.interrupt_sleeper(bridge, "interrupt", "3", clock=lambda: next(ticks))
        self.assertEqual(elapsed, 4.0)
        self.assertEqual([call[0] for call in bridge.calls], ["send_keys", "wait_for"])
        self.assertEqual(bridge.calls[1][1]["timeout_ms"], 10000)
        self.assertEqual(bridge.calls[1][2], 10000)

    def test_a_late_or_missing_exit_fails(self):
        # An exit reported 11 s after Ctrl+C, or a wait that timed out, fails the step.
        bridge = FakeToolBridge({"send_keys": {"bytes": 1}, "wait_for": {"reason": "exited"}})
        ticks = iter([0.0, 11.0])
        with self.assertRaisesRegex(smoke.SmokeFailure, "over 10 s"):
            smoke.interrupt_sleeper(bridge, "interrupt", "3", clock=lambda: next(ticks))
        bridge = FakeToolBridge({"send_keys": {"bytes": 1}, "wait_for": {"reason": "timeout"}})
        with self.assertRaisesRegex(smoke.SmokeFailure, "reason"):
            smoke.interrupt_sleeper(bridge, "interrupt", "3", clock=iter([0.0, 1.0]).__next__)


class FixtureTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix="stmcpt-", dir=None if os.name == "nt" else "/tmp"))
        self.addCleanup(shutil.rmtree, self.directory, True)
        self.owned = smoke.Owned()
        self.addCleanup(self.owned.cleanup)

    def bridge(self, mode):
        """Start the fixture bridge in `mode` under the smoke's own Bridge client."""
        script = self.directory / "bridge.py"
        script.write_text(FIXTURE_BRIDGE, encoding="utf-8")
        return smoke.Bridge([sys.executable, str(script), mode], dict(os.environ), self.directory / f"{mode}.err",
                            self.owned, smoke.Deadline(30.0))

    def test_close_helpers_requires_each_close_named_and_a_clean_exit(self):
        # Step six passes only when every helper closes by id and the bridge exits 0 after EOF.
        smoke.close_helpers(self.bridge("good"), ["2", "3", "4"])

    def test_close_helpers_fails_when_the_bridge_already_exited(self):
        # A bridge that died after step five cannot pass step six by skipping close_pane.
        bridge = self.bridge("exit")
        bridge.process.wait(timeout=10)
        with self.assertRaisesRegex(smoke.SmokeFailure, "exited with code 0 before close_pane"):
            smoke.close_helpers(bridge, ["2", "3"])

    def test_close_helpers_fails_on_a_wrong_pane_a_refusal_or_a_bad_exit(self):
        # Each close must name its own pane and succeed, and the bridge must exit 0 on EOF.
        for mode, pattern in (("wrong", "pane_id"), ("refuse", "refused"), ("badexit", "exit code")):
            with self.subTest(mode=mode), self.assertRaisesRegex(smoke.SmokeFailure, pattern):
                smoke.close_helpers(self.bridge(mode), ["2"])

    def disabled_run(self, app_body, bridge_body, ready_timeout_s):
        """Run the disabled phase against fixture scripts standing in for the app and the bridge."""
        app = self.directory / "app.sh"
        app.write_text("#!/bin/sh\n" + app_body, encoding="utf-8")
        bridge = self.directory / "bridge.sh"
        bridge.write_text("#!/bin/sh\n" + bridge_body, encoding="utf-8")
        for path in (app, bridge):
            path.chmod(0o755)
        args = smoke.parse_args(["--app", str(app), "--bridge", str(bridge)], "macos", {})
        smoke.run_disabled(args, smoke.host_name(), self.directory, dict(os.environ), self.owned,
                           smoke.Deadline(30.0), ready_timeout_s=ready_timeout_s)

    @POSIX_ONLY
    def test_disabled_run_fails_when_the_app_never_spawns_a_shell(self):
        # A log file is not readiness: an app that never starts its first pane's shell must fail.
        with self.assertRaisesRegex(smoke.SmokeFailure, "spawned no child process"):
            self.disabled_run("exec sleep 60\n", f"echo '{smoke.NO_ENDPOINT_TEXT}' >&2; exit 1\n", 1.0)

    @POSIX_ONLY
    def test_disabled_run_passes_once_a_child_runs_and_the_bridge_finds_nothing(self):
        # With a running child, an empty endpoint directory for 1 s and the bridge's exit 1, the phase passes.
        self.disabled_run("sleep 60 &\nwait\n", f"echo 'sonicterm-mcp: {smoke.NO_ENDPOINT_TEXT}' >&2; exit 1\n", 10.0)

    @POSIX_ONLY
    def test_disabled_run_fails_when_the_bridge_does_not_exit_1(self):
        # A bridge that connects or reports success with the server off is a failure.
        with self.assertRaisesRegex(smoke.SmokeFailure, "bridge exit code"):
            self.disabled_run("sleep 60 &\nwait\n", "exit 0\n", 10.0)

    def test_wait_for_child_needs_a_child_and_a_readable_table(self):
        # Readiness is a contained child: none within the bound fails, and an unreadable table fails too.
        class Running:
            pid = 50
            returncode = None

            def poll(self):
                return None

        class Contained:
            def __init__(self, counts):
                self.process = Running()
                self.counts = counts

            def descendant_count(self):
                value = self.counts.pop(0) if len(self.counts) > 1 else self.counts[0]
                if isinstance(value, Exception):
                    raise value
                return value
        with self.assertRaisesRegex(smoke.SmokeFailure, "no child"):
            smoke.wait_for_child("disabled", Contained([0]), 0.3, smoke.Deadline(5.0))
        self.assertEqual(smoke.wait_for_child("disabled", Contained([0, 1]), 5.0, smoke.Deadline(5.0)), 1)
        with self.assertRaisesRegex(smoke.SmokeFailure, "cannot read"):
            smoke.wait_for_child("disabled", Contained([RuntimeError("ps failed")]), 5.0, smoke.Deadline(5.0))


class PackagedPathTests(unittest.TestCase):
    def test_packaged_binaries_come_from_the_environment_without_a_build(self):
        # CI passes the packaged app and bridge through the environment, so the gate command stays the catalog's.
        environment = {"SONICTERM_SMOKE_APP": "/pkg/SonicTerm.app/Contents/MacOS/sonicterm-mac",
                       "SONICTERM_SMOKE_BRIDGE": "/pkg/SonicTerm.app/Contents/MacOS/sonicterm-mcp"}
        args = smoke.parse_args([], "macos", environment)
        self.assertEqual(args.build, [])
        self.assertEqual(args.app, Path(environment["SONICTERM_SMOKE_APP"]).resolve())
        self.assertEqual(args.bridge, Path(environment["SONICTERM_SMOKE_BRIDGE"]).resolve())
        args = smoke.parse_args(["--bridge", "/other"], "macos", {"SONICTERM_SMOKE_APP": "/pkg/app"})
        self.assertEqual((args.app, args.bridge, args.build), (Path("/pkg/app").resolve(), Path("/other").resolve(), []))


if __name__ == "__main__":
    unittest.main(verbosity=2)
