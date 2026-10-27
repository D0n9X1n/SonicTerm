"""Perf watcher tests."""

from __future__ import annotations
import contextlib
import io
import json
import re
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

import perf_build_tests
import perf_focus
import perf_focus_tests
import perf_processes_tests
import perf_scenarios
import perf_sessions
import perf_settings
import perf_watcher


class FakeGate:
    """Stands in for local-gate.py: records each step and answers it from a handler."""

    PASS, FAIL, TIMEOUT = "PASS", "FAIL", "TIMEOUT"

    @property
    def PERF_BUILDS(self):
        """The real gate's reviewed build steps, which perf-compare runs as they are."""
        return perf_build_tests.REAL_GATE.PERF_BUILDS

    @property
    def PERF_COUNTER_BUILDS(self):
        """The real gate's reviewed builds for a tree that declares perf-counters."""
        return perf_build_tests.REAL_GATE.PERF_COUNTER_BUILDS

    @property
    def PERF_FEATURE_BUILDS(self):
        """The real gate's reviewed comparison builds, keyed by the perf features a tree declares."""
        return perf_build_tests.REAL_GATE.PERF_FEATURE_BUILDS

    def __init__(self, handler):
        self.handler = handler
        self.steps = []
        self.roots = []
        self.environs = []

    def __getattr__(self, name):
        """Every other gate fact (the harness cfg table, its catalogs and flag composition) is the real gate's."""
        return getattr(perf_build_tests.REAL_GATE, name)

    def Step(self, step_id, argv, hosts, timeout_s, evidence, prerequisites, ci_jobs):
        return SimpleNamespace(id=step_id, argv=tuple(argv), hosts=tuple(hosts), timeout_s=timeout_s)

    def run_step(self, step, index, root, log_dir, environ, output_limit_bytes=None):
        self.steps.append(step)
        self.roots.append(Path(root))
        self.environs.append(dict(environ))
        status, exit_code, output = self.handler(step)
        log_path = Path(log_dir) / f"{index:02d}-{step.id}.log"
        log_path.write_text(output, encoding="utf-8")
        return SimpleNamespace(id=step.id, status=status, exit_code=exit_code, log_path=log_path,
                               detail="", leftover_processes=getattr(self, "leftover", 0), elapsed_s=0.0,
                               custody=getattr(self, "custody", None))


HARNESS_PID = 900


def write_atlas_retry_api(tree: Path, skip: str | None = None) -> None:
    """Write the four methods `perf_atlas_retry_api` needs into their owning crates of `tree`, except `skip`."""
    for entry in perf_build_tests.REAL_GATE.HARNESS_API_CFGS:
        if entry.name != "perf_atlas_retry_api":
            # When: another entry's methods are not S1/atlas-retry's, they stay out of these fixture trees.
            continue
        for crate, owner, name in entry.methods:
            if name == skip:
                continue
            source = tree / crate / "atlas_retry_api.rs"
            source.parent.mkdir(parents=True, exist_ok=True)
            with source.open("a", encoding="utf-8") as stream:
                stream.write(f"impl {owner} {{\n    pub fn {name}(&self) {{}}\n}}\n")


def harness_process(**overrides):
    """The harness: run_step starts it in a new session, so it leads its session and group."""
    return perf_processes_tests.FakeProcess(**{"pid": HARNESS_PID, "pgid": HARNESS_PID, "sid": HARNESS_PID, "start": "9",
                          "command": "perf_scenarios", "start_unix_s": 1000.5, **overrides})


class RunWatcherTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.scratch, self.evidence = root / "scratch", root / "evidence"
        (self.scratch / "sessions").mkdir(parents=True)
        (self.scratch / "checkpoints").mkdir()
        self.evidence.mkdir()
        self.table = perf_processes_tests.FakeTable(perf_processes_tests.leader(), perf_processes_tests.anchor(), harness_process())
        self.footprint_answer = ("PASS", 0)
        self.gate = FakeGate(self.answer_footprint)

    def tearDown(self):
        self.temporary.cleanup()

    def answer_footprint(self, step):
        status, exit_code = self.footprint_answer
        if status == "PASS":
            Path(step.argv[-1]).write_text(json.dumps({"processes": [{"pid": HARNESS_PID, "footprint": 1234}]}),
                                           encoding="utf-8")
        return status, exit_code, "footprint: cannot attach\n" if status != "PASS" else ""

    def watcher(self, kill_at_go=False, platform=None, harness_command=None):
        # Only a test of another host names one, so the rest keep RunContext's defaults.
        extra = {key: value for key, value in (("platform", platform), ("harness_command", harness_command))
                 if value is not None}
        context = perf_watcher.RunContext(self.scratch, self.evidence, perf_processes_tests.LAUNCH_UNIX_S, self.table, self.gate, (), kill_at_go,
                                  **extra)
        return perf_watcher.RunWatcher(context)

    def write(self, relative, content=""):
        (self.scratch / relative).parent.mkdir(parents=True, exist_ok=True)
        (self.scratch / relative).write_text(content, encoding="utf-8")

    def test_sessions_wait_for_harness_pid_then_are_acknowledged(self):
        # Records are validated once harness.pid exists, then acknowledged with an empty file.
        watcher = self.watcher()
        self.write("sessions/0.json", perf_processes_tests.RECORD_TEXT)
        watcher.poll()
        self.assertFalse((self.scratch / "acks" / "0").exists())
        self.write("harness.pid", f"{HARNESS_PID}\n")
        watcher.poll()
        self.assertEqual((self.scratch / "acks" / "0").read_bytes(), b"")
        self.assertEqual(watcher.acked, {"0": perf_processes_tests.ACKED})
        self.assertEqual(watcher.problems, [])

    def test_invalid_record_is_a_problem_and_never_acknowledged(self):
        # A record whose anchor is outside the leader's session would let cleanup signal a stranger.
        self.table.processes[501].sid = 777
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", perf_processes_tests.RECORD_TEXT)
        watcher.poll()
        self.assertFalse((self.scratch / "acks" / "0").exists())
        self.assertTrue(watcher.problems)

    def test_partial_record_waits_then_fails_the_final_scan(self):
        # A record still being written is retried; one that never parses fails the run.
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", perf_processes_tests.RECORD_TEXT[:20])
        watcher.poll()
        self.assertEqual(watcher.problems, [])
        watcher.final_scan()
        self.assertTrue(watcher.problems)

    def test_a_rejected_record_with_a_valid_anchor_is_cleaned_after_the_run(self):
        # The final scan revalidates a record rejected during the run; its live anchor lets cleanup end the session.
        self.table = perf_processes_tests.FakeTable(perf_processes_tests.anchor(), harness_process(), perf_processes_tests.FakeProcess(510, 500, 500, start="11"))
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", perf_processes_tests.RECORD_TEXT)
        watcher.poll()
        self.assertIn("0", watcher.rejected)
        watcher.final_scan()
        sessions = watcher.sessions_for_cleanup()
        self.assertEqual([session.anchor_pid for session in sessions], [501])
        clock = perf_processes_tests.FakeClock()
        result = perf_sessions.cleanup_sessions(self.table, sessions, unanchored=list(watcher.unanchored.values()),
                                       clock=clock, sleep=clock.sleep)
        self.assertTrue(result.settled, result.problems)
        self.assertEqual(self.table.kills, [510, 501])

    def test_members_without_a_valid_anchor_are_listed_and_never_signalled(self):
        # With the anchor gone the session id may name another session, so its members are only listed.
        self.table = perf_processes_tests.FakeTable(harness_process(), perf_processes_tests.FakeProcess(510, 500, 500, start="11"))
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", perf_processes_tests.RECORD_TEXT)
        watcher.poll()
        watcher.final_scan()
        self.assertEqual(watcher.sessions_for_cleanup(), [])
        clock = perf_processes_tests.FakeClock()
        result = perf_sessions.cleanup_sessions(self.table, [], unanchored=list(watcher.unanchored.values()),
                                       clock=clock, sleep=clock.sleep)
        self.assertFalse(result.settled)
        self.assertEqual([member.pid for member in result.survivors], [510])
        self.assertEqual(self.table.kills, [])

    def test_checkpoint_footprint_path_comes_from_the_request_name(self):
        # The JSON path is derived from `<index>-<label>.request`, never from the harness's field.
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("checkpoints/2-end.request")
        watcher.poll()
        json_path = self.scratch / "checkpoints" / "2-end.json"
        self.assertEqual(self.gate.steps[0].argv,
                         ("/usr/bin/footprint", "-p", str(HARNESS_PID), "-j", str(json_path)))
        self.assertTrue((self.scratch / "checkpoints" / "2-end.done").exists())
        self.assertEqual(watcher.footprints["2-end"]["bytes"], 1234)
        watcher.poll()
        self.assertEqual(len(self.gate.steps), 1)

    def test_failed_or_timed_out_footprint_still_answers_the_request(self):
        # A footprint that failed, or was killed at its bound and reaped, still answers; the run stays valid without it.
        for answer in (("TIMEOUT", -9), ("FAIL", 1)):
            with self.subTest(answer=answer):
                for leftover in (self.scratch / "checkpoints").iterdir():
                    leftover.unlink()
                self.footprint_answer = answer
                watcher = self.watcher()
                self.write("harness.pid", str(HARNESS_PID))
                self.write("checkpoints/1-idle.request")
                watcher.poll()
                self.assertTrue((self.scratch / "checkpoints" / "1-idle.done").exists())
                record = watcher.footprints["1-idle"]
                self.assertIsNone(record["bytes"])
                self.assertEqual((record["status"], record["exit_code"]), answer)
                self.assertIn("cannot attach", record["output"])
                self.assertEqual(watcher.problems, [])

    def test_done_waits_until_the_footprint_process_is_reaped(self):
        # `.done` tells the harness footprint has finished; one whose exit was never collected withholds it.
        self.footprint_answer = ("TIMEOUT", None)
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("checkpoints/1-idle.request")
        watcher.poll()
        self.assertFalse((self.scratch / "checkpoints" / "1-idle.done").exists())
        self.assertFalse(watcher.footprints["1-idle"]["reaped"])
        watcher.poll()
        self.assertFalse((self.scratch / "checkpoints" / "1-idle.done").exists())
        self.assertEqual(len(self.gate.steps), 1)

    def test_footprint_ends_well_inside_the_harness_checkpoint_wait(self):
        # The harness waits CHECKPOINT_WAIT for `.done` and then measures again, so footprint and its reap end first.
        source = (perf_settings.ROOT / "crates/sonicterm-app/examples/perf_scenarios/probe.rs").read_text(encoding="utf-8")
        match = re.search(r"const CHECKPOINT_WAIT: Duration = Duration::from_secs\((\d+)\);", source)
        self.assertIsNotNone(match, "probe.rs no longer declares CHECKPOINT_WAIT in whole seconds")
        self.assertLessEqual(perf_watcher.FOOTPRINT_TIMEOUT_S, 40)
        self.assertLess(perf_watcher.FOOTPRINT_TIMEOUT_S + perf_watcher.RUN_STEP_REAP_BOUND_S + perf_watcher.WATCH_INTERVAL_S, int(match[1]))

    def test_unmatched_request_name_is_still_answered(self):
        # A request this script cannot name gets its done file and no footprint run.
        watcher = self.watcher()
        self.write("harness.pid", str(HARNESS_PID))
        self.write("checkpoints/odd name.request")
        watcher.poll()
        self.assertTrue((self.scratch / "checkpoints" / "odd name.done").exists())
        self.assertEqual(self.gate.steps, [])
        self.assertIsNone(watcher.footprints["odd name"]["bytes"])

    def test_deadline_case_group_kills_only_a_validated_harness_after_go(self):
        # The kill matches a run_step deadline: one group SIGKILL of this run's harness leader.
        watcher = self.watcher(kill_at_go=True)
        self.write("harness.pid", str(HARNESS_PID))
        watcher.poll()
        self.assertEqual(self.table.group_kills, [])
        self.write("go/0")
        watcher.poll()
        self.assertEqual(self.table.group_kills, [HARNESS_PID])
        self.assertTrue(watcher.deadline["sent"])

    def test_harness_name_may_be_cut_by_the_kernel(self):
        # The kernel keeps 15 (Linux) or 16 (macOS) bytes of a command name; it is checked against the launched binary.
        table = perf_processes_tests.FakeTable(harness_process(command="perf_scenarios_a"))
        self.assertIsNone(perf_watcher.validate_harness_leader(table, HARNESS_PID, perf_processes_tests.LAUNCH_UNIX_S, "perf_scenarios_alloc"))
        self.assertTrue(perf_watcher.validate_harness_leader(table, HARNESS_PID, perf_processes_tests.LAUNCH_UNIX_S, "perf_scenarios_other"))
        self.assertTrue(perf_watcher.validate_harness_leader(perf_processes_tests.FakeTable(harness_process(command="perf")), HARNESS_PID,
                                                     perf_processes_tests.LAUNCH_UNIX_S, "perf_scenarios"))

    def test_deadline_kill_rechecks_the_accepted_harness_identity(self):
        # The accepted pid and start token are rechecked just before the signal; a pid that now names another
        # perf_scenarios leader, started after the launch, is never signalled.
        watcher = self.watcher(kill_at_go=True)
        self.write("harness.pid", str(HARNESS_PID))
        watcher.poll()
        self.assertEqual(watcher.harness_pid, HARNESS_PID)
        self.table.processes[HARNESS_PID] = harness_process(start="10", start_unix_s=1002.0)
        self.write("go/0")
        watcher.poll()
        self.assertEqual(self.table.group_kills, [])
        self.assertFalse(watcher.deadline["sent"])
        self.assertIn("identity", watcher.deadline["problem"])

    def test_deadline_case_never_signals_an_unvalidated_harness(self):
        # A pid that is not this run's live harness leader is never signalled.
        cases = {"another program": {"command": "zsh"}, "not a session leader": {"sid": 1},
                 "started before the launch": {"start_unix_s": 900.0}}
        for name, overrides in cases.items():
            with self.subTest(name):
                self.table = perf_processes_tests.FakeTable(harness_process(**overrides))
                watcher = self.watcher(kill_at_go=True)
                self.write("harness.pid", str(HARNESS_PID))
                self.write("go/0")
                watcher.poll()
                self.assertEqual(self.table.group_kills, [])
                self.assertFalse(watcher.deadline["sent"])
                self.assertTrue(watcher.deadline["problem"])
        self.table = perf_processes_tests.FakeTable()
        watcher = self.watcher(kill_at_go=True)
        watcher.poll()
        self.assertEqual(self.table.group_kills, [])

    def test_windows_role_program_is_acknowledged_once_validated(self):
        # A role program's record is acknowledged only when its process is the harness's child, running its image.
        # Resolve this runtime-only dependency after module initialization.
        import perf_windows_tests
        self.table = perf_windows_tests.program_table()
        watcher = self.watcher(platform="win32", harness_command="perf_scenarios.exe")
        self.write("harness.pid", str(HARNESS_PID))
        self.write("sessions/0.json", perf_windows_tests.PROGRAM_TEXT)
        watcher.poll()
        self.assertEqual((self.scratch / "acks" / "0").read_bytes(), b"")
        self.assertEqual(watcher.programs, {"0": perf_sessions.AckedProgram("0", perf_windows_tests.PROGRAM_PID, "21")})
        self.assertEqual(watcher.problems, [])
        # A record naming a process the harness did not start is refused and never acknowledged.
        self.table.processes[perf_windows_tests.PROGRAM_PID + 1] = perf_processes_tests.FakeProcess(perf_windows_tests.PROGRAM_PID + 1, 0, 0, start="22",
                                                             command="perf_scenarios.exe", ppid=4)
        self.write("sessions/1.json", perf_windows_tests.PROGRAM_TEXT.replace('"role": 0', '"role": 1').replace(
            str(perf_windows_tests.PROGRAM_PID), str(perf_windows_tests.PROGRAM_PID + 1)))
        watcher.poll()
        self.assertFalse((self.scratch / "acks" / "1").exists())
        self.assertTrue(any("parent" in problem for problem in watcher.problems), watcher.problems)

    def test_windows_deadline_kill_terminates_only_a_validated_harness(self):
        # On Windows the accepted harness is ended by TerminateProcess after its image and creation time
        # are rechecked; the job ends the rest, and no session or group check applies.
        # Resolve this runtime-only dependency after module initialization.
        import perf_windows_tests
        self.table = perf_windows_tests.program_table()
        watcher = self.watcher(kill_at_go=True, platform="win32", harness_command="perf_scenarios.exe")
        self.write("harness.pid", str(HARNESS_PID))
        watcher.poll()
        self.write("go/0")
        watcher.poll()
        self.assertEqual(self.table.terminations, [(HARNESS_PID, "9")])
        self.assertEqual(self.table.group_kills, [])
        self.assertTrue(watcher.deadline["sent"])
        cases = {"another image": {"command": "cmd.exe"}, "a reused pid": {"start": "10"},
                 "started before the launch": {"start_unix_s": 900.0}}
        for name, overrides in cases.items():
            with self.subTest(name):
                self.table = perf_windows_tests.program_table()
                (self.scratch / "go" / "0").unlink(missing_ok=True)
                watcher = self.watcher(kill_at_go=True, platform="win32", harness_command="perf_scenarios.exe")
                self.write("harness.pid", str(HARNESS_PID))
                watcher.poll()
                self.table.processes[HARNESS_PID] = harness_process(
                    **{"pgid": 0, "sid": 0, "command": "perf_scenarios.exe", **overrides})
                self.write("go/0")
                watcher.poll()
                self.assertEqual(self.table.terminations, [])
                self.assertFalse(watcher.deadline["sent"])
                self.assertTrue(watcher.deadline["problem"])

    def test_off_macos_a_checkpoint_is_done_without_footprint(self):
        # footprint exists only on macOS, so elsewhere the harness's checkpoint wait ends at once.
        # Resolve this runtime-only dependency after module initialization.
        import perf_windows_tests
        self.table = perf_windows_tests.program_table()
        watcher = self.watcher(platform="win32", harness_command="perf_scenarios.exe")
        self.write("harness.pid", str(HARNESS_PID))
        self.write("checkpoints/1-end.request")
        watcher.poll()
        self.assertTrue((self.scratch / "checkpoints" / "1-end.done").exists())
        self.assertEqual(self.gate.steps, [])
        self.assertIn("no footprint on this host", watcher.footprints["1-end"]["detail"])


class FrontSamplerTests(unittest.TestCase):
    def test_each_sample_form_is_printed_once_with_its_raw_text(self):
        # Even a passing CI log shows what the runner's lsappinfo printed for each form.
        answers = {"front": ["ASN:0x0-0x1:\n", "ASN:0x0-0x1:\n", "[ NULL ]\n", ""]}

        def run(argv, timeout_s):
            if argv == perf_focus.FRONT_ARGV:
                return perf_focus_tests.command(argv, answers["front"].pop(0))
            return perf_focus_tests.command(argv, '"pid"=55\n')
        printed = set()
        with tempfile.TemporaryDirectory() as temporary:
            log_path = Path(temporary) / "front-samples.log"
            sampler = perf_watcher.FrontSampler(log_path, run, printed)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                for _sample in range(4):
                    sampler.sample()
            logged = log_path.read_text(encoding="utf-8").splitlines()
        text = output.getvalue()
        self.assertEqual(text.count("form=front application"), 1)
        self.assertEqual(text.count("form=no front application"), 1)
        self.assertEqual(text.count("form=failed"), 1)
        self.assertIn("[ NULL ]", text)
        self.assertEqual(len(logged), 6)
        self.assertEqual([reading.kind for reading in sampler.readings], ["app", "app", "none", "failed"])
        self.assertEqual(printed, {"app", "none", "failed"})


IDLE_SCENARIO = perf_scenarios.Scenario("S1", ("default",), "Idle", 120, 30)
# The reason the harness writes (waits.rs first_present_missing_reason) when a window that opened on a hidden
# Space presented no frame within 10 s: no redraw and no native occlusion event.
STARTUP_OCCLUSION_REASON = (
    "no frame presented within 10 s of the window opening (0 RedrawRequested; no native occlusion event "
    "arrived), so the run is treated as a suspected occlusion; the likely cause is a full-screen app on its "
    "display, which keeps the window on a hidden Space")
# What every run_step deadline or interruption reason says: its count of the harness's group proves nothing.
UNCOUNTED_STOP = ("so either the harness's process group was not counted or a process outside it held the output "
                  "open; either way the run's cleanup is unresolved")
