#!/usr/bin/env python3
"""Keep diagnostic orchestration bounded and failures distinguishable from evidence."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("pty_diagnostic", Path(__file__).with_name("pty-termination-diagnostic.py"))
tool = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tool)



class OrchestrationTests(unittest.TestCase):
    def test_phase_has_fixed_count_and_stops_at_first_failure(self):
        # An observed failure ends that phase instead of retrying it until a pass.
        for failure, count in ((None, 8), (0, 1), (3, 4)):
            visited = []
            def execute(index):
                return {"accepted": index != failure}
            results = tool.run_phase(execute, visited.append)
            self.assertEqual(len(results), count)
            self.assertEqual(visited, list(range(count)))

    def test_exact_result_rejects_zero_tests_wrong_filter_and_duplicate_pass(self):
        # A green process without the exact integration test is not a measured baseline.
        line = f"test {tool.TEST} ... ok\n"
        summary = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 1s\n"
        good = (line + summary).encode()
        self.assertTrue(tool.exact_test_pass(good, 6))
        self.assertFalse(tool.exact_test_pass(good, 19))
        self.assertFalse(tool.exact_test_pass((line * 2 + summary).encode(), 6))
        self.assertFalse(tool.exact_test_pass(summary.encode(), 6))
        self.assertFalse(tool.exact_test_pass(good.replace(b"1 passed", b"0 passed"), 6))

    def test_compiler_artifact_requires_unique_exact_source_and_target(self):
        # Cargo's resolved executable must come from this phase, not another clone's cache.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            target = root / "target"
            target.mkdir()
            binary = target / "test-executable"
            binary.write_bytes(b"binary")
            artifact = {"reason": "compiler-artifact", "executable": str(binary),
                        "target": {"name": "pty_queue_heap_truth", "kind": ["test"]},
                        "manifest_path": str(root / "crates/sonicterm-io/Cargo.toml")}
            payload = json.dumps(artifact).encode() + b"\n"
            self.assertEqual(tool.compiler_artifact(payload, root, target), artifact)
            for invalid in (payload + payload, payload.replace(b"sonicterm-io/Cargo", b"elsewhere/Cargo"), b""):
                with self.assertRaises(RuntimeError):
                    tool.compiler_artifact(invalid, root, target)
            other = root / "other"
            other.mkdir()
            with self.assertRaises(RuntimeError):
                tool.compiler_artifact(payload, root, other)

    def test_census_accepts_signed_foreign_uid_without_owning_it(self):
        # macOS nobody may print as -2; only the exact current uid can become a custody candidate.
        rows = tool.parse_census(b"1 0 1 0\n773 1 773 -2\n456 123 456 501\n", 501)
        self.assertEqual(rows, {456: {"pid": 456, "ppid": 123, "pgid": 456, "uid": 501}})
        for invalid in (b"bad row\n", b"456 123 456 uid\n", b"-1 123 456 501\n"):
            with self.assertRaises(RuntimeError):
                tool.parse_census(invalid, 501)

    def test_native_birth_identity_never_matches_reused_pid(self):
        # PID equality cannot authorize later cleanup of a new process.
        original = {"pid": 123, "seconds": 20, "micros": 5}
        self.assertTrue(tool.same_identity(original, dict(original)))
        self.assertFalse(tool.same_identity(original, None))
        self.assertFalse(tool.same_identity(original, dict(original, seconds=21)))
        self.assertFalse(tool.same_identity(original, dict(original, micros=6)))

    def test_supervisor_exception_retains_command_failure_and_cleanup_evidence(self):
        # An observer failure still needs an immutable per-command receipt, not an unbound local error.
        with tempfile.TemporaryDirectory() as directory:
            experiment = tool.Experiment.__new__(tool.Experiment)
            experiment.output = Path(directory)
            experiment.deadline = tool.time.monotonic() + 60
            experiment.environment = {}
            experiment.api = object()
            experiment.steps = []
            with patch.object(tool.GATE, "run_step", side_effect=RuntimeError("capture failed")):
                result = experiment.run("broken", ["never-run"], Path(directory), 10)
            self.assertFalse(result["accepted"])
            self.assertEqual(result["status"], "BLOCKED")
            self.assertIn("capture failed", result["supervision_error"])
            saved = json.loads((Path(directory) / "broken-result.json").read_text())
            self.assertEqual(saved, result)
            self.assertEqual(experiment.steps, [result])


class AdmissionTests(unittest.TestCase):
    def experiment(self, directory):
        experiment = tool.Experiment.__new__(tool.Experiment)
        experiment.output = Path(directory)
        experiment.deadline = tool.time.monotonic() + 120
        experiment.environment = {}
        experiment.api = object()
        experiment.steps = []
        experiment.report = {}
        experiment.cancelled = False
        return experiment

    def test_short_aggregate_budget_never_shortens_a_native_case(self):
        # The passive 20-second window cannot be interrupted by a clipped diagnostic deadline.
        with tempfile.TemporaryDirectory() as directory:
            experiment = self.experiment(directory)
            with patch.object(tool.GATE, "run_step") as run:
                with self.assertRaisesRegex(RuntimeError, "full command budget"):
                    experiment.run("native-case", ["never-run"], Path(directory), 150)
            run.assert_not_called()
            self.assertEqual(experiment.steps, [])

    def test_cancelled_experiment_never_launches_a_paired_command(self):
        # Cancellation is terminal even if the previous command's cleanup settled.
        with tempfile.TemporaryDirectory() as directory:
            experiment = self.experiment(directory)
            experiment.cancelled = True
            with patch.object(tool.GATE, "run_step") as run:
                with self.assertRaisesRegex(RuntimeError, "cancelled"):
                    experiment.run("later", ["never-run"], Path(directory), 10)
            run.assert_not_called()

    def test_signal_latches_cancellation_without_interrupting_cleanup_twice(self):
        # Actions may send SIGINT then SIGTERM; only the first starts unwinding into cleanup.
        with tempfile.TemporaryDirectory() as directory:
            experiment = self.experiment(directory)
            with self.assertRaises(KeyboardInterrupt):
                experiment.cancel(tool.signal.SIGTERM, None)
            self.assertTrue(experiment.cancelled)
            self.assertEqual(experiment.report["cancelled_signal"], tool.signal.SIGTERM)
            experiment.cancel(tool.signal.SIGINT, None)
            self.assertEqual(experiment.report["cancelled_signal"], tool.signal.SIGTERM)


class PhaseEvidenceTests(unittest.TestCase):
    def fixture(self, directory, custody):
        root = Path(directory)
        executable = root / "fixture-binary"
        executable.write_bytes(b"fixture-only")
        experiment = tool.Experiment.__new__(tool.Experiment)
        experiment.output = root
        experiment.deadline = tool.time.monotonic() + 60
        experiment.environment = {}
        experiment.report = {"phases": {}}
        result = {"accepted": False, "status": "FAIL", "exit_code": 101,
                  "leader_reaped": True, "leader_pid": 123, "custody": custody}
        return experiment, root, executable, result

    def test_cleaned_baseline_remains_failed_but_admits_the_paired_phase(self):
        # Owned cleanup preserves the failed baseline yet must not prevent the planned instrumented observation.
        with tempfile.TemporaryDirectory() as directory:
            custody = {"live_survivors": [], "problems": [], "signals_after_command": [{"pid": 456}]}
            experiment, root, executable, result = self.fixture(directory, custody)
            with patch.object(tool, "source_pin", return_value={"head": "fixed"}), \
                    patch.object(experiment, "compile", return_value=(executable, {}, tool.digest(executable))), \
                    patch.object(experiment, "run", return_value=result), \
                    patch.object(tool, "command_payload", return_value=b"test fixture ... FAILED\n"):
                self.assertFalse(experiment.phase(root, "baseline", root / "target", False))
            cases = experiment.report["phases"]["baseline"]["cases"]
            self.assertEqual(len(cases), 1)
            self.assertFalse(cases[0]["accepted"])
            self.assertEqual(cases[0]["command"]["custody"]["signals_after_command"], [{"pid": 456}])

    def test_unknown_custody_is_explicit_and_keeps_the_failed_case(self):
        # Missing identity evidence must block admission without replacing the original record with a TypeError.
        with tempfile.TemporaryDirectory() as directory:
            experiment, root, executable, result = self.fixture(directory, None)
            with patch.object(tool, "source_pin", return_value={"head": "fixed"}), \
                    patch.object(experiment, "compile", return_value=(executable, {}, tool.digest(executable))), \
                    patch.object(experiment, "run", return_value=result), \
                    patch.object(tool, "command_payload", return_value=b"failed original command\n"):
                with self.assertRaisesRegex(RuntimeError, "phase custody unconfirmed"):
                    experiment.phase(root, "baseline", root / "target", False)
            case = json.loads((root / "baseline-01-case.json").read_text())
            self.assertIsNone(case["command"]["custody"])
            self.assertEqual(len(experiment.report["phases"]["baseline"]["cases"]), 1)

    def test_truncated_command_log_retains_case_and_phase_before_blocking(self):
        # A missing supervisor footer cannot erase the command from the final experiment summary.
        with tempfile.TemporaryDirectory() as directory:
            custody = {"live_survivors": [], "problems": [], "signals_after_command": []}
            experiment, root, executable, result = self.fixture(directory, custody)
            with patch.object(tool, "source_pin", return_value={"head": "fixed"}), \
                    patch.object(experiment, "compile", return_value=(executable, {}, tool.digest(executable))), \
                    patch.object(experiment, "run", return_value=result), \
                    patch.object(tool, "command_payload", side_effect=RuntimeError("missing footer")):
                with self.assertRaisesRegex(RuntimeError, "phase evidence failed"):
                    experiment.phase(root, "baseline", root / "target", False)
            case = json.loads((root / "baseline-01-case.json").read_text())
            self.assertFalse(case["accepted"])
            self.assertIn("missing footer", case["evidence_error"])
            retained = json.loads(json.dumps(experiment.report["phases"]["baseline"]["cases"]))
            self.assertEqual(retained, [case])

    def test_live_survivor_and_unknown_observation_block_the_paired_phase(self):
        # Continued native work requires settled observed custody, even when the test itself has returned.
        for custody in ({"live_survivors": [{"pid": 456}], "problems": [], "signals_after_command": []},
                        {"live_survivors": [], "problems": ["unknown birth"], "signals_after_command": []}):
            with self.subTest(custody=custody), tempfile.TemporaryDirectory() as directory:
                experiment, root, executable, result = self.fixture(directory, custody)
                with patch.object(tool, "source_pin", return_value={"head": "fixed"}), \
                        patch.object(experiment, "compile", return_value=(executable, {}, tool.digest(executable))), \
                        patch.object(experiment, "run", return_value=result), \
                        patch.object(tool, "command_payload", return_value=b"failed original command\n"):
                    with self.assertRaisesRegex(RuntimeError, "phase custody unconfirmed"):
                        experiment.phase(root, "baseline", root / "target", False)


class FailureOnlyEvidenceTests(unittest.TestCase):
    def test_success_has_no_failure_probe_evidence(self):
        # An unchanged successful path performs no diagnostic native queries or trace transport.
        self.assertEqual(tool.failure_observation(b"test success ... ok\n", True)["verdict"], "UNEXERCISED")
        with self.assertRaises(RuntimeError):
            tool.failure_observation(b"PTY_PASSIVE verdict=Unknown heap_sample=false\n", True)

    def test_wouldblock_retains_unknown_and_cleanup_separately(self):
        # UNKNOWN is retained evidence, never a successful heap sample or a claimed signal-delivery explanation.
        output = (b"PTY_PASSIVE_STATE at_ns=1 observation=unknown\n"
                  b"PTY_PASSIVE verdict=Unknown identities=0 heap_sample=false\n"
                  b"PTY_ORIGINAL_FAILURE error=WouldBlock heap_sample=false\n"
                  b"PTY_DIAGNOSTIC_CLEANUP drop_returned=true settlement=NOT_PROVEN\n")
        result = tool.failure_observation(output, False)
        self.assertIn("Unknown", result["passive"][0])
        self.assertEqual(len(result["states"]), 1)
        self.assertIn("NOT_PROVEN", result["cleanup"])
        for invalid in (output.replace(b"PTY_DIAGNOSTIC_CLEANUP", b"missing"),
                        output + b"PTY_PASSIVE verdict=SettledLate heap_sample=false\n",
                        output.replace(b"verdict=Unknown", b"verdict=Success")):
            with self.assertRaises(RuntimeError):
                tool.failure_observation(invalid, False)

    def test_other_failure_never_implies_the_kill_probe_ran(self):
        # Setup/accounting failures and non-WouldBlock errors are not attributed to passive termination observation.
        self.assertEqual(tool.failure_observation(b"setup panicked\n", False)["verdict"], "UNAVAILABLE")
        output = (b"PTY_ORIGINAL_FAILURE error=TimedOut heap_sample=false\n"
                  b"PTY_DIAGNOSTIC_CLEANUP drop_returned=true settlement=NOT_PROVEN\n")
        self.assertEqual(tool.failure_observation(output, False)["passive"], [])
        with self.assertRaises(RuntimeError):
            tool.failure_observation(b"PTY_PASSIVE verdict=Unknown heap_sample=false\n" + output, False)

    def test_incomplete_failure_observations_cannot_be_called_an_unrelated_failure(self):
        # Interrupted passive output locates the boundary but does not prove its final verdict or cleanup.
        for output in (b"PTY_PASSIVE_STATE at_ns=1 observation=live\n",
                       b"PTY_PASSIVE verdict=Unknown heap_sample=false\n",
                       b"PTY_DIAGNOSTIC_CLEANUP drop_returned=true settlement=NOT_PROVEN\n",
                       b"PTY_ORIGINAL_FAILURE error=WouldBlock heap_sample=false\n" * 2):
            with self.subTest(output=output), self.assertRaisesRegex(RuntimeError, "incomplete failure-only"):
                tool.failure_observation(output, False)

    def test_production_termination_and_successful_population_are_unchanged(self):
        # The lighter probe must not recreate the recorder that perturbed the first paired experiment.
        root = tool.ROOT
        production = (root / "crates/sonicterm-io/src/pty.rs").read_bytes()
        self.assertEqual(tool.hashlib.sha256(production).hexdigest(),
                         "3cb37224099b5327be419776c451f4dda184cffebc0cff9ec03f0684719a9f21")
        fixture = (root / "crates/sonicterm-io/tests/pty_queue_heap_truth.rs").read_text()
        before_kill = fixture.split("fn measure_full_queue(script: &str)", 1)[1].split("let killed = pty.kill();", 1)[0]
        for forbidden in ("Ticket", "Probe", "pid()", "observe(", "var_os", "Instant::now();"):
            self.assertNotIn(forbidden, before_kill)
        self.assertIn("if let Err(error) = &killed", fixture)
        self.assertNotIn("child_exit_probe", fixture)
        self.assertNotIn("SONICTERM_PTY_TERMINATION_PROBE_DIR", fixture)
        self.assertEqual(tool.PURE_TESTS, 10)


@unittest.skipUnless(sys.platform == "darwin", "native custody uses macOS libproc")
class NativeCustodyTests(unittest.TestCase):
    def test_real_command_success_and_output_overflow_are_distinct(self):
        # A real child is reaped, while zero-exit oversized output still fails diagnostic acceptance.
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            experiment = tool.Experiment(out)
            good = experiment.run("native-good", [sys.executable, "-c", "print('bounded-ok')"], tool.ROOT, 10)
            self.assertTrue(good["accepted"], good)
            self.assertTrue(good["leader_reaped"])
            self.assertFalse(good["custody"]["remaining"])
            with patch.object(tool, "OUTPUT_LIMIT", 128):
                overflow = experiment.run("native-overflow", [sys.executable, "-c", "print('x'*129)"], tool.ROOT, 10)
            self.assertFalse(overflow["accepted"])
            self.assertEqual(overflow["exit_code"], 0)
            self.assertEqual(overflow["status"], "FAIL")
            self.assertIn("output limit", overflow["detail"])

    def test_observed_session_escaped_child_fails_and_is_cleaned_after_command(self):
        # A setsid child is owned through its live parent's birth, never through a process-name match.
        with tempfile.TemporaryDirectory() as directory:
            experiment = tool.Experiment(Path(directory))
            source = ("import subprocess,sys,time; child=subprocess.Popen([sys.executable,'-c',"
                      "'import time; time.sleep(12)'],start_new_session=True,"
                      "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);"
                      "print(child.pid,flush=True); time.sleep(0.7)")
            result = experiment.run("escaped", [sys.executable, "-c", source], tool.ROOT, 10)
            self.assertEqual(result["status"], "PASS", result)
            self.assertFalse(result["accepted"])
            self.assertTrue(result["custody"]["signals_after_command"])
            self.assertFalse(result["custody"]["live_survivors"])
            self.assertTrue(result["leader_reaped"])
            self.assertGreaterEqual(len(result["custody"]["observed"]), 2)

    def test_timeout_does_not_hide_a_live_leader_or_create_a_success_receipt(self):
        # The canonical deadline kills and reaps the exact owned command rather than waiting forever.
        with tempfile.TemporaryDirectory() as directory:
            experiment = tool.Experiment(Path(directory))
            result = experiment.run("deadline", [sys.executable, "-c", "import time; time.sleep(10)"], tool.ROOT, 1)
            self.assertFalse(result["accepted"])
            self.assertEqual(result["status"], "TIMEOUT")
            self.assertTrue(result["leader_reaped"])
            self.assertFalse(result["custody"]["live_survivors"])


class WorkflowTests(unittest.TestCase):
    def test_diagnostic_job_is_isolated_read_only_bounded_and_retains_failures(self):
        # This workflow cannot become an implicit shipping or retry-until-green path.
        text = (tool.ROOT / ".github/workflows/pty-diagnostic.yml").read_text()
        for required in ("contents: read", "cancel-in-progress: false", "runs-on: macos-14",
                         "timeout-minutes: 35", "diagnostic/macos-pty-termination-1488",
                         "persist-credentials: false", "fetch-depth: 0", "always()",
                         "retention-days: 14", "if-no-files-found: error"):
            self.assertIn(required, text)
        self.assertNotIn("pull_request_target", text)
        self.assertNotIn("workflow_dispatch", text)
        self.assertNotIn("continue-on-error", text)
        self.assertEqual(text.count("timeout-minutes:"), 6)
        self.assertIn("--expected-head", text)
        self.assertEqual(tool.ATTEMPTS, 8)
        self.assertEqual(tool.TOTAL_SECONDS, 1200)


if __name__ == "__main__":
    unittest.main(verbosity=2)
