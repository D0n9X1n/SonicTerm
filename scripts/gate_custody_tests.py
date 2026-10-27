"""Gate custody tests."""

from __future__ import annotations
import dataclasses
import io
import os
from pathlib import Path
import re
import sys
import tempfile
import time
import unittest
from unittest import mock

import gate_catalog
import gate_execution
import gate_files
import gate_model
import gate_process
import gate_report
import gate_test_fixture


@unittest.skipUnless(os.name == "nt", "Windows Job Object contract")
class WindowsCustodyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.deadline = time.monotonic() + 60

    @classmethod
    def tearDownClass(cls):
        if time.monotonic() > cls.deadline:
            raise AssertionError("Windows custody test group exceeded its 60s cleanup-inclusive budget")

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.remove_scratch)
        self.root = Path(self.scratch.name)
        self.job = gate_model.WINDOWS_JOB
        if time.monotonic() + 3 >= self.deadline:
            self.fail("Windows custody test group exhausted its cleanup-inclusive budget")

    def remove_scratch(self):
        # The scratch folder can stay open for a moment after a job's processes are gone, so retry a sharing
        # violation for up to 2 s. A survivor that escaped custody sleeps 8 s, so it still fails the removal.
        retry_deadline_s = time.monotonic() + 2
        while True:
            try:
                self.scratch.cleanup()
                return
            except PermissionError as error:
                if getattr(error, "winerror", None) != gate_test_fixture.ERROR_SHARING_VIOLATION or time.monotonic() >= retry_deadline_s:
                    raise
            time.sleep(0.05)

    def execute(self, code="pass", *, step_id="probe", limit=None, timeout=4, **fields):
        step = gate_test_fixture.python_step(step_id, code, timeout_s=min(timeout, self.deadline - time.monotonic() - 2), **fields)
        result = gate_execution.run_step(step, 1, self.root, self.root, os.environ, output_limit_bytes=limit)
        return result, result.log_path.read_bytes()

    def settled(self, result):
        self.assertTrue(result.custody["empty"], result)
        self.assertTrue(result.custody["bootstrap_reaped"], result)

    def test_original_exit_and_invocation_bytes_survive_bootstrap(self):
        # The private protocol carries DWORD exits while merged output preserves exact write order.
        code = ("import os,sys; os.write(1,b'out\\x00\\xff'); os.write(2,b'err'); "
                "assert sys.stdin.buffer.read()==b''; "
                f"assert os.getcwd()=={str(self.root)!r}; "
                "assert os.environ['WINDOWS_JOB_PROBE']=='value'; "
                "assert os.environ['NO_COLOR']=='kept'; "
                "assert sys.argv[1:]==['space value','quote\\\"value','','trailing\\\\']; "
                "import ctypes; ctypes.windll.kernel32.ExitProcess(0xFEDCBA98)")
        step = gate_test_fixture.python_step("args", code, argv=(sys.executable, "-I", "-S", "-c", code,
                                              "space value", 'quote"value', "", "trailing\\"),
                           env=(("WINDOWS_JOB_PROBE", "value"), ("NO_COLOR", "kept")))
        result = gate_execution.run_step(step, 1, self.root, self.root, os.environ)
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertEqual(result.exit_code, 0xFEDCBA98)
        self.assertIn(b"out\x00\xfferr", result.log_path.read_bytes())
        self.settled(result)

    def test_short_child_settles_inside_grace(self):
        # A brief descendant is allowed to finish before the fixed grace expires.
        code = ("import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time;time.sleep(0.15)'],"
                "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)")
        result, _ = self.execute(code)
        self.assertEqual(result.status, gate_model.PASS)
        self.assertEqual(result.custody["cleanup"], "none")
        self.settled(result)

    def test_compile_only_cleanup_is_explicit_while_mixed_remains_strict(self):
        # The caller policy, not command or image spelling, controls acceptance of forced cleanup.
        code = ("import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time;time.sleep(8)'],"
                "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)")
        with mock.patch.object(gate_model, "LEFTOVER_GRACE_S", 0.03):
            for name, policy, expected in (("clippy", gate_model.WindowsPolicy.COMPILE_ONLY, gate_model.CLEANED_NOT_NATURAL),
                                           ("doctests", gate_model.WindowsPolicy.STRICT, gate_model.FAIL)):
                canonical = next(step for step in gate_catalog.STEPS if step.id == name)
                original_launch = self.job._launch
                def launch(command, cwd, env, startup_handle, status_handle):
                    return original_launch([sys.executable, "-c", code], cwd, env, startup_handle, status_handle)
                with (mock.patch.object(self.job, "_launch", side_effect=launch),
                      mock.patch.object(gate_process, "resolve_program", return_value=sys.executable)):
                    result = gate_execution.run_step(canonical, 1, self.root, self.root, os.environ)
                log = result.log_path.read_bytes()
                self.assertEqual(result.status, expected)
                self.assertEqual(result.exit_code, 0)
                self.assertIn(expected.encode(), log)
                self.assertEqual(result.custody["cleanup"], "terminated")
                self.settled(result)

    def test_real_preparation_job_is_settled_before_separate_strict_execution(self):
        # Substitute only the target program: real owned jobs must settle independently around original execution.
        step = next(step for step in gate_catalog.STEPS if step.id == "pty-close-baseline")
        survivor = ("import subprocess,sys;subprocess.Popen([sys.executable,'-c','import time;time.sleep(8)'],"
                    "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)")
        original_launch, original_run = self.job._launch, self.job.run
        observed = []
        def run(command, **kwargs):
            result = original_run(command, **kwargs)
            observed.append(result)
            return result
        for execution_survivor in (False, True):
            observed.clear()
            def launch(command, cwd, env, startup_handle, status_handle):
                preparation = "--no-run" in command
                if not preparation:
                    self.assertEqual(len(observed), 1)
                    self.assertTrue(observed[0]["custody"]["empty"])
                    self.assertTrue(observed[0]["custody"]["bootstrap_reaped"])
                code = survivor if preparation or execution_survivor else "print('execution-only')"
                return original_launch([sys.executable, "-c", code], cwd, env, startup_handle, status_handle)
            with (mock.patch.object(self.job, "_launch", side_effect=launch),
                  mock.patch.object(self.job, "run", side_effect=run),
                  mock.patch.object(gate_model, "LEFTOVER_GRACE_S", 0.03),
                  mock.patch.object(gate_process, "resolve_program", return_value=sys.executable)):
                result = gate_execution.run_step(step, 1, self.root, self.root, os.environ)
            self.assertEqual(len(observed), 2)
            self.assertEqual(result.phases[0].status, gate_model.CLEANED_NOT_NATURAL)
            self.assertEqual(result.phases[1].policy, gate_model.WindowsPolicy.STRICT)
            self.assertEqual(result.status, gate_model.FAIL if execution_survivor else gate_model.CLEANED_NOT_NATURAL)
            self.assertEqual(result.accepted, not execution_survivor)
            self.assertEqual(result.exit_code, 0)
            self.assertTrue(all(item["custody"]["empty"] and item["custody"]["bootstrap_reaped"] for item in observed))

    def test_assignment_and_token_refusals_never_launch_target(self):
        # Assignment refusal and failed authorization may kill only this retained bootstrap, never launch its target.
        marker = self.root / "launched"
        code = f"from pathlib import Path; Path({str(marker)!r}).touch()"
        with mock.patch.object(self.job.Job, "assign", side_effect=OSError(5, "assignment refused")):
            result, _ = self.execute(code)
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertIsNone(result.exit_code)
        self.assertFalse(marker.exists())
        self.settled(result)
        for token in (b"", b"invalid", self.job.START_TOKEN + b"extra"):
            with self.subTest(token=token), mock.patch.object(self.job, "START_TOKEN", token):
                result, _ = self.execute(code)
            self.assertEqual(result.status, gate_model.FAIL)
            self.assertFalse(marker.exists())
            self.settled(result)

    def test_malformed_status_never_supplies_target_success(self):
        # Missing, truncated, duplicate, and unknown records must not become successful command exits.
        for payload in (b"", b"EXIT\x00", b"EXIT\x00\x00\x00\x00" * 2, b"JUNK\x00\x00\x00\x00"):
            fake = self.root / "bad-bootstrap.py"
            fake.write_text("import os,msvcrt,sys\n"
                            "r=msvcrt.open_osfhandle(int(sys.argv[1]),os.O_RDONLY|os.O_BINARY)\n"
                            "w=msvcrt.open_osfhandle(int(sys.argv[2]),os.O_WRONLY|os.O_BINARY)\n"
                            "while os.read(r,100): pass\n"
                            f"os.write(w,{payload!r});os.close(w)\n", encoding="utf-8")
            with self.subTest(payload=payload), mock.patch.object(self.job, "BOOTSTRAP", fake):
                result, _ = self.execute()
            self.assertEqual(result.status, gate_model.FAIL)
            self.assertIsNone(result.exit_code)
            self.settled(result)

    def test_target_launch_failure_preserves_none_exit(self):
        # Target CreateProcess refusal is LAUNCH, rather than the bootstrap's successful protocol exit.
        invalid = self.root / "invalid.exe"
        invalid.write_bytes(b"not an executable")
        result, _ = self.execute(argv=(str(invalid),))
        self.assertEqual(result.status, gate_model.LAUNCH)
        self.assertIsNone(result.exit_code)
        self.settled(result)

    def test_output_streams_before_exit_and_optional_cap_keeps_prefix(self):
        # A child waits for a sink-created acknowledgement, so buffering until exit would hit the deadline.
        ack = self.root / "ack"
        code = ("import os,time;from pathlib import Path;os.write(1,b'first');"
                f"p=Path({str(ack)!r});end=time.monotonic()+2\n"
                "while not p.exists() and time.monotonic()<end:time.sleep(.005)\n"
                "assert p.exists();os.write(2,b'second')")
        class Sink(io.BytesIO):
            def write(inner, data):
                if b"first" in data:
                    ack.touch()
                return super().write(data)
        sink = Sink()
        raw = self.job.run([sys.executable, "-c", code], cwd=self.root, env=os.environ, sink=sink,
                           deadline=time.monotonic()+4)
        self.assertEqual(raw["exit_code"], 0)
        self.assertEqual(sink.getvalue(), b"firstsecond")
        self.assertEqual(raw["errors"], [])
        for limit in (None, 17):
            result, log = self.execute("import os;os.write(1,b'Z'*300000)", limit=limit)
            self.assertEqual(result.exit_code, 0)
            self.assertEqual(result.status, gate_model.PASS if limit is None else gate_model.FAIL)
            self.assertIn(b"Z" * (300000 if limit is None else 17), log)
            if limit is not None:
                self.assertNotIn(b"Z"*18, log)
            self.settled(result)

    def test_pipe_holding_survivor_does_not_wait_for_eof(self):
        # The job count is examined after leader exit even while its descendant still holds the pipe.
        code = "import subprocess,sys;subprocess.Popen([sys.executable,'-c','import time;time.sleep(8)'])"
        with mock.patch.object(gate_model, "LEFTOVER_GRACE_S", 0.03):
            result, _ = self.execute(code)
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertEqual(result.exit_code, 0)
        self.assertGreater(result.leftover_processes, 0)
        self.settled(result)

    def test_io_faults_fail_and_owned_job_is_closed(self):
        # Sink/read failures cannot hide success or bypass exact-job cleanup and handle closure.
        original = self.job.Job.close
        closed = []
        def close(job):
            original(job)
            closed.append(True)
        class BrokenSink:
            def write(self, data):
                raise OSError("sink refused")
        with mock.patch.object(self.job.Job, "close", new=close):
            raw = self.job.run([sys.executable, "-c", "print('bytes',flush=True);import time;time.sleep(5)"],
                               cwd=self.root, env=os.environ, sink=BrokenSink(), deadline=time.monotonic()+3)
        self.assertTrue(raw["errors"])
        self.assertTrue(raw["custody"]["empty"])
        self.assertTrue(raw["custody"]["bootstrap_reaped"])
        self.assertTrue(closed)
        with mock.patch.object(self.job.Pipe, "chunks", side_effect=OSError("pipe failed")):
            result, _ = self.execute("import time;time.sleep(5)")
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertTrue(result.custody["bootstrap_reaped"])

    def test_timeout_interrupt_and_query_faults_fail_closed(self):
        # Interrupt wins over deadline, and unverified accounting never invents an empty job.
        result, _ = self.execute("import time;time.sleep(5)", timeout=0.2)
        self.assertEqual(result.status, gate_model.TIMEOUT)
        self.settled(result)
        original = self.job.Capture.pump
        calls = 0
        def interrupted(capture):
            nonlocal calls
            calls += 1
            if calls == 1:
                raise KeyboardInterrupt()
            return original(capture)
        with mock.patch.object(self.job.Capture, "pump", new=interrupted):
            result, _ = self.execute("import time;time.sleep(5)")
        self.assertEqual(result.status, gate_model.INTERRUPTED)
        self.settled(result)
        with mock.patch.object(self.job.Job, "accounting", side_effect=OSError("query refused")):
            result, _ = self.execute("import time;time.sleep(5)", timeout=0.15)
        self.assertEqual(result.status, gate_model.TIMEOUT)
        self.assertFalse(result.custody["empty"])

    def test_cleanup_interrupt_closes_job_and_reaps_bootstrap(self):
        # Interruption during exact-job termination still closes the kill-on-close handle in the final guard.
        original = self.job.Job.close
        closed = []
        def close(job):
            original(job)
            closed.append(True)
        with (mock.patch.object(self.job.Job, "terminate", side_effect=KeyboardInterrupt()),
              mock.patch.object(self.job.Job, "close", new=close)):
            result, _ = self.execute("import time;time.sleep(5)", timeout=0.1)
        self.assertEqual(result.status, gate_model.INTERRUPTED)
        self.assertTrue(closed)
        self.assertFalse(result.custody["empty"])
        self.assertTrue(result.custody["bootstrap_reaped"])

    def test_job_members_lists_a_member_left_running(self):
        # A step whose leader leaves a child running gets that child named, not just counted, before cleanup ends it.
        code = ("import subprocess;child=subprocess.Popen(['ping','-n','30','127.0.0.1'],"
                "stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print('PING_PID',child.pid,flush=True)")
        result, log = self.execute(code)
        self.assertEqual(result.status, gate_model.FAIL)
        self.settled(result)
        self.assertEqual(result.custody["cleanup"], "terminated")
        match = re.search(rb"PING_PID (\d+)", log)
        self.assertIsNotNone(match, log)
        members = result.custody["before_cleanup"]["members"]
        self.assertGreaterEqual(members["count"], 1)
        listed = {member["pid"]: member for member in members["processes"]}
        ping = listed.get(int(match.group(1)))
        self.assertIsNotNone(ping, members)
        self.assertEqual(ping["image"].casefold(), "ping.exe")
        self.assertIsInstance(ping["created"], int)
        self.assertGreater(ping["created"], 0)

    def test_failed_member_listing_never_changes_the_step_status(self):
        # The member list is evidence only: a failed query is recorded beside the counts and the step still passes.
        with mock.patch.object(self.job.Job, "members", side_effect=OSError("list refused")):
            result, _ = self.execute()
        self.assertEqual(result.status, gate_model.PASS)
        self.settled(result)
        self.assertIn("list refused", result.custody["before_cleanup"]["members_error"])
        self.assertNotIn("members", result.custody["before_cleanup"])

    def test_nested_job_is_supported_and_protocol_handles_are_private(self):
        # A nested assignment must work or fail the test; it is not a host-capability skip.
        module_path = str(gate_test_fixture.ROOT / "scripts/windows-process-job.py")
        code = ("import importlib.util,io,os,sys,time;from pathlib import Path;"
                f"s=importlib.util.spec_from_file_location('inner',{module_path!r});"
                "m=importlib.util.module_from_spec(s);s.loader.exec_module(m);"
                "r=m.run([sys.executable,'-c','pass'],cwd=Path.cwd(),env=os.environ,sink=io.BytesIO(),"
                "deadline=time.monotonic()+2);assert r['exit_code']==0 and r['natural'] and not r['errors']")
        result, _ = self.execute(code)
        self.assertEqual(result.status, gate_model.PASS)
        original = self.job._launch
        def launch(command, cwd, env, startup_handle, status_handle):
            probe = ("import ctypes;from ctypes import wintypes as W;k=ctypes.WinDLL('kernel32',use_last_error=True);"
                     "k.WriteFile.argtypes=[W.HANDLE,ctypes.c_void_p,W.DWORD,ctypes.c_void_p,ctypes.c_void_p];"
                     "k.ReadFile.argtypes=k.WriteFile.argtypes;b=ctypes.create_string_buffer(1);n=W.DWORD();"
                     f"assert not k.WriteFile({status_handle},b,1,ctypes.byref(n),None);"
                     f"assert not k.ReadFile({startup_handle},b,1,ctypes.byref(n),None)")
            return original([sys.executable,"-c",probe],cwd,env,startup_handle,status_handle)
        with mock.patch.object(self.job, "_launch", side_effect=launch):
            result, _ = self.execute()
        self.assertEqual(result.status, gate_model.PASS)

    def test_expired_budget_cannot_become_natural_success(self):
        # A slow capture callback must not let a completed target bypass the original execution deadline.
        original = self.job.Capture.pump
        def slow_pump(capture):
            original(capture)
            time.sleep(0.2)
        with mock.patch.object(self.job.Capture, "pump", new=slow_pump):
            result, _ = self.execute(timeout=0.15)
        self.assertEqual(result.status, gate_model.TIMEOUT)
        self.settled(result)

    def test_final_log_failure_cannot_report_success(self):
        # Failure to persist the verdict is a capture failure even after the command has settled.
        class FailedLog:
            def write(self, data):
                raise OSError("final log write refused")
        result = gate_process._finish(FailedLog(), gate_test_fixture.python_step("footer", "pass"), self.root / "log",
                              time.monotonic(), gate_model.PASS, 0, "", custody={"empty": True})
        self.assertEqual(result.status, gate_model.FAIL)
        self.assertIn("final log", result.detail)

    def test_pipe_free_survivor_fails_step(self):
        # A successful leader cannot hide a live descendant by closing its output pipes.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            code = ("import subprocess,sys; subprocess.Popen([sys.executable,'-c',"
                    "'import time; time.sleep(4)'],stdin=subprocess.DEVNULL,"
                    "stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,close_fds=True)")
            started = time.monotonic()
            try:
                result = gate_execution.run_step(gate_test_fixture.python_step("survivor", code, timeout_s=8), 1, root, root, os.environ)
                self.assertEqual(result.exit_code, 0)
                self.assertEqual(result.status, gate_model.FAIL)
                self.assertGreater(result.leftover_processes, 0)
            finally:
                # Before custody exists, the bounded RED child exits on its own.
                time.sleep(max(0, 4.5 - (time.monotonic() - started)))


class CustodyPolicyTests(unittest.TestCase):
    def test_compile_cleanup_policy_is_explicit_and_narrow(self):
        # Only these reviewed standalone compilation steps may accept forced owned cleanup.
        self.assertEqual({step.id for step in gate_catalog.STEPS if step.windows_policy == gate_model.WindowsPolicy.COMPILE_ONLY},
                         {"clippy", "perf-scenarios-counters-clippy", "perf-scenarios-frame-texture-clippy",
                          "perf-scenarios-echo-trace-clippy", "perf-scenarios-harness-api-clippy",
                          "perf-scenarios-harness-api-echo-trace-clippy", "doc",
                          "doc-resource-features", "release-windows", "windows-perf-build"})
        self.assertEqual(gate_test_fixture.python_step("mixed", "pass").windows_policy, gate_model.WindowsPolicy.STRICT)

    def test_every_compile_only_step_may_pass_after_a_forced_cleanup(self):
        # A step declared compile-only is also on the allow list a cleaned run is judged by, so a forced Windows
        # cleanup of its compiler tree after exit 0 reads as passed, never as failed.
        compile_only = {step.id for step in gate_catalog.STEPS if step.windows_policy == gate_model.WindowsPolicy.COMPILE_ONLY}
        self.assertLessEqual(compile_only, set(gate_model._COMPILE_ONLY_STEPS))

    def test_cleaned_status_stays_distinct_in_all_summaries(self):
        # A successful cleanup does not turn the original non-natural lifetime into PASS.
        step = next(step for step in gate_catalog.STEPS if step.id == "clippy")
        state = gate_files.GitSnapshot(False)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = gate_process.StepResult("clippy", gate_model.CLEANED_NOT_NATURAL, 0, 1, root / "step.log",
                                     windows_policy=step.windows_policy,
                                     custody={"cleanup": "terminated", "empty": True, "bootstrap_reaped": True,
                                              "protocol_complete": True, "capture_complete": True, "errors": []})
            report = gate_report.GateReport("windows", (result,), state, state, (), root)
            self.assertEqual(report.exit_code, 0)
            self.assertIn("cleaned=1", "\n".join(gate_report.summary_lines(report)))
            self.assertIn("verdict=CLEANED_NOT_NATURAL exit=0", "\n".join(gate_report.summary_lines(report)))
            document = gate_report.summary_json(report, [step])
            self.assertEqual(document["verdict"], gate_model.CLEANED_NOT_NATURAL)
            self.assertEqual(document["steps"][0]["windows_policy"], "compile-only")
            self.assertEqual(document["steps"][0]["custody"], result.custody)
            for bad in (dataclasses.replace(result, id="doctests"),
                        dataclasses.replace(result, windows_policy=gate_model.WindowsPolicy.STRICT),
                        dataclasses.replace(result, exit_code=3),
                        dataclasses.replace(result, custody={**result.custody, "bootstrap_reaped": False}),
                        dataclasses.replace(result, custody={**result.custody, "capture_complete": False}),
                        dataclasses.replace(result, custody={**result.custody, "protocol_complete": False}),
                        dataclasses.replace(result, custody={**result.custody, "errors": ["query failed"]})):
                self.assertEqual(dataclasses.replace(report, results=(bad,)).exit_code, 1)
