"""Coverage execution tests."""

from __future__ import annotations
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

import coverage_test_fixture


class GateScriptEvidenceTests(unittest.TestCase):
    """The gate script pins its checkout and publishes its report and inventory as one unit, before either check."""

    def run_gate(self, mode, env_extra=None, fault=None, broken_tool=False):
        """Run a copy of the gate script in a scratch checkout with stand-in cargo and rustc.

        `fault` is an (action, match) pair for the python3 stand-in. Returns the completed process, the
        checkout, and the cargo calls.
        """
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, str(directory), True)
        root, bin_dir, log = directory / "checkout", directory / "bin", directory / "cargo.log"
        (root / "scripts").mkdir(parents=True)
        bin_dir.mkdir()
        for name in ("rust-logic-coverage.sh", "coverage-floor.py"):
            shutil.copyfile(coverage_test_fixture._HERE / name, root / "scripts" / name)
        # The CLI delegates to sibling modules; the isolated checkout must contain that same implementation.
        for helper in coverage_test_fixture._HERE.glob("coverage_*.py"):
            if not helper.name.endswith("_tests.py") and helper.name != "coverage_test_fixture.py":
                shutil.copyfile(helper, root / "scripts" / helper.name)
        if broken_tool:
            (root / "scripts" / "coverage-floor.py").write_text("raise SystemExit(5)\n", encoding="utf-8")
        # A stand-in self-test, so the copied gate does not run this suite again.
        (root / "scripts" / "coverage-floor_tests.py").write_text("", encoding="utf-8")
        (root / "scripts" / "coverage-baseline.json").write_text(json.dumps(coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), indent=2) + "\n",
                                                                 encoding="utf-8")
        (root / ".gitignore").write_text("/target\n__pycache__/\n", encoding="utf-8")
        coverage_test_fixture.git(root, "init", "-q")
        coverage_test_fixture.commit_files(root, {"crates/sonicterm-vt/src/vt.rs": "pub fn vt() {}\n"})
        tools = {"cargo": coverage_test_fixture.FAKE_CARGO, "rustc": coverage_test_fixture.FAKE_RUSTC, **({"python3": coverage_test_fixture.FAULT_SHIM} if fault else {})}
        for name, body in tools.items():
            (bin_dir / name).write_text(body, encoding="utf-8")
            (bin_dir / name).chmod(0o755)
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 150)} if mode == "floor-fails" else coverage_test_fixture.FILES
        env = {key: value for key, value in coverage_test_fixture.clean_env().items()
               if not key.startswith(("CARGO", "RUSTUP", "GIT_", "COVERAGE_", "FAKE_")) and key != "ImageVersion"}
        env.update(env_extra or {})
        env.update({"PATH": f"{bin_dir}{os.pathsep}{env.get('PATH', '')}", "FAKE_COVERAGE_MODE": mode,
                    "FAKE_CARGO_LOG": str(log), "FAKE_REPORT": json.dumps(coverage_test_fixture.export(files, root=str(root))),
                    "FAKE_METADATA": json.dumps(coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS, root=str(root))), "FAKE_REAL_PYTHON": sys.executable})
        if fault:
            env.update({"FAKE_FAULT_ACTION": fault[0], "FAKE_FAULT_MATCH": fault[1]})
        completed = coverage_test_fixture.run_in_session(["bash", str(root / "scripts" / "rust-logic-coverage.sh")], str(root), env,
                                   coverage_test_fixture.GATE_TIMEOUT_S)
        calls = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
        return completed, root, calls

    def visible(self, root):
        """Return the evidence directory and its visible entries, which upload-artifact collects by default."""
        evidence = root / coverage_test_fixture.floor.EVIDENCE_DIRECTORY
        return evidence, sorted(path.name for path in evidence.iterdir() if not path.name.startswith("."))

    def update_from(self, root, measurement):
        """Run --update-baseline in the scratch checkout from its record and a directory holding a report and inventory."""
        stdout, stderr = io.StringIO(), io.StringIO()
        status = coverage_test_fixture.floor.main(["--report", str(measurement / coverage_test_fixture.floor.REPORT_FILE), "--metadata",
                             str(measurement / coverage_test_fixture.floor.INVENTORY_FILE), "--baseline",
                             str(root / "scripts" / "coverage-baseline.json"), "--target", coverage_test_fixture.TARGET, "--update-baseline",
                             "--reason", "rebaseline", "--provenance",
                             str(root / coverage_test_fixture.floor.EVIDENCE_DIRECTORY / coverage_test_fixture.floor.PROVENANCE_FILE)],
                            env=coverage_test_fixture.LOCAL_ENV, stdout=stdout, stderr=stderr)
        return coverage_test_fixture.Run(status, stdout.getvalue(), stderr.getvalue())

    def test_failed_subset_gate_keeps_the_published_measurement(self):
        # The measurement is published before the 80% gate runs, and the run still fails with the gate's status.
        completed, root, calls = self.run_gate("gate-fails")
        self.assertEqual(completed.returncode, 1, completed.stdout + completed.stderr)
        evidence, names = self.visible(root)
        self.assertEqual(names, [coverage_test_fixture.floor.PROVENANCE_FILE, coverage_test_fixture.floor.MEASUREMENT_DIRECTORY])
        measurement = evidence / coverage_test_fixture.floor.MEASUREMENT_DIRECTORY
        self.assertEqual(sorted(path.name for path in measurement.iterdir()), [coverage_test_fixture.floor.REPORT_FILE, coverage_test_fixture.floor.INVENTORY_FILE])
        # The staged directory was renamed into place, not copied.
        self.assertFalse((root / coverage_test_fixture.floor.WORK_DIRECTORY / coverage_test_fixture.floor.STAGING_DIRECTORY).exists())
        document = json.loads((evidence / coverage_test_fixture.floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        self.assertEqual(document["measurement"], "complete")
        self.assertEqual(document["checks"], {"subset-gate": "exit status 1", "floor": "not run"})
        self.assertEqual(document["report_sha256"],
                         hashlib.sha256((measurement / coverage_test_fixture.floor.REPORT_FILE).read_bytes()).hexdigest())
        self.assertEqual(document["inventory_sha256"],
                         hashlib.sha256((measurement / coverage_test_fixture.floor.INVENTORY_FILE).read_bytes()).hexdigest())
        # Bytecode and target output are ignored, so the pinned checkout is clean.
        self.assertEqual(document["worktree_changes"], [])
        self.assertEqual(document["commit"], coverage_test_fixture.git(root, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual([call.split(" ")[:2] for call in calls],
                         [["llvm-cov", "--version"], ["llvm-cov", "--workspace"], ["llvm-cov", "report"],
                          ["metadata", "--no-deps"], ["llvm-cov", "report"]])
        self.assertIn("--no-report", calls[1])
        self.assertIn("--fail-under-lines 80", calls[4])

    def test_failed_floor_keeps_the_evidence_and_names_the_artifact(self):
        # A DROP fails the run after the measurement is published, and its message names this run's artifact.
        completed, root, _calls = self.run_gate("floor-fails", coverage_test_fixture.RUN_ENV)
        self.assertEqual(completed.returncode, 1, completed.stdout + completed.stderr)
        document = json.loads((root / coverage_test_fixture.floor.EVIDENCE_DIRECTORY / coverage_test_fixture.floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        self.assertEqual(document["measurement"], "complete")
        self.assertEqual(document["checks"], {"subset-gate": "exit status 0", "floor": "exit status 1"})
        self.assertEqual(document["artifact"], coverage_test_fixture.ARTIFACT)
        self.assertIn("coverage floor FAIL", completed.stdout)
        self.assertIn(f"the artifact {coverage_test_fixture.ARTIFACT}", completed.stdout)
        self.assertNotIn(coverage_test_fixture.floor.PROPOSAL_BEGIN, completed.stdout)

    def test_failed_test_run_uploads_only_its_incomplete_record(self):
        # A run whose instrumented tests fail publishes nothing, only a record marked incomplete.
        completed, root, calls = self.run_gate("tests-fail")
        self.assertEqual(completed.returncode, 101, completed.stdout + completed.stderr)
        evidence = root / coverage_test_fixture.floor.EVIDENCE_DIRECTORY
        self.assertEqual([path.name for path in evidence.iterdir()], [coverage_test_fixture.floor.PROVENANCE_FILE])
        document = json.loads((evidence / coverage_test_fixture.floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        self.assertEqual(document["measurement"], "incomplete")
        self.assertEqual((document["failed_phase"], document["failure"]), ("instrumented-tests", "exit status 101"))
        self.assertIsNone(document["report_sha256"])
        self.assertEqual(document["cargo_llvm_cov"], "cargo-llvm-cov 0.9.0")
        self.assertEqual(len(calls), 2)

    def test_checkout_drift_during_the_measurement_is_named_and_publishes_nothing(self):
        # A clean commit during the instrumented tests fails the run at publish: nothing is published, the record
        # keeps the pinned commit and names the drift, and the checker refuses it.
        completed, root, _calls = self.run_gate("drift")
        self.assertEqual(completed.returncode, coverage_test_fixture.floor.DRIFT_EXIT_STATUS, completed.stdout + completed.stderr)
        evidence = root / coverage_test_fixture.floor.EVIDENCE_DIRECTORY
        self.assertEqual([path.name for path in evidence.iterdir()], [coverage_test_fixture.floor.PROVENANCE_FILE])
        document = json.loads((evidence / coverage_test_fixture.floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        pinned, head = (coverage_test_fixture.git(root, "rev-parse", rev).stdout.strip() for rev in ("HEAD~1", "HEAD"))
        self.assertEqual((document["measurement"], document["failed_phase"]), ("incomplete", "publish"))
        self.assertIn(f"checkout drifted from its pin: HEAD moved from {pinned} to {head}", document["failure"])
        self.assertEqual(document["commit"], pinned)
        self.assertTrue(document["checkout_drift"])
        run = self.update_from(root, root / coverage_test_fixture.floor.WORK_DIRECTORY / coverage_test_fixture.floor.STAGING_DIRECTORY)
        self.assertEqual(run.status, 2, run.stdout + run.stderr)
        self.assertIn("is incomplete: the run stopped in phase 'publish' (checkout drifted from its pin", run.stderr)

    def test_publication_leaves_both_files_or_neither_at_every_boundary(self):
        # Killed or failed around publication, the evidence holds the record alone or the record beside both
        # files, and no record short of complete vouches for them.
        final = "--phase subset-gate --state running"
        cases = {
            "killed while staging the inventory": ("kill-at-inventory", None, -9, "inventory", False),
            "killed before the rename": ("passes", ("kill", "os.rename"), -9, "publish", False),
            "the rename fails": ("passes", ("fail", "os.rename"), 1, "publish", False),
            "killed after the rename, before the complete record": ("passes", ("kill", final), -9, "publish", True),
            "killed inside the complete record's atomic write": ("passes", ("kill-in-write", final), 137, "publish",
                                                                 True),
        }
        for label, (mode, fault, status, phase, published) in cases.items():
            with self.subTest(boundary=label):
                completed, root, _calls = self.run_gate(mode, fault=fault)
                self.assertEqual(completed.returncode, status, completed.stdout + completed.stderr)
                evidence, names = self.visible(root)
                self.assertEqual(names, [coverage_test_fixture.floor.PROVENANCE_FILE] + ([coverage_test_fixture.floor.MEASUREMENT_DIRECTORY] if published else []))
                if published:
                    self.assertEqual(sorted(path.name for path in (evidence / coverage_test_fixture.floor.MEASUREMENT_DIRECTORY).iterdir()),
                                     [coverage_test_fixture.floor.REPORT_FILE, coverage_test_fixture.floor.INVENTORY_FILE])
                document = json.loads((evidence / coverage_test_fixture.floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
                self.assertEqual((document["measurement"], document["failed_phase"]), ("incomplete", phase))
                self.assertIsNone(document["report_sha256"])
                # Only a write killed inside the atomic replace leaves a temporary file, and it is hidden.
                hidden = [path.name for path in evidence.iterdir() if path.name.startswith(".")]
                self.assertEqual(len(hidden), 1 if fault and fault[0] == "kill-in-write" else 0, hidden)
                if published:
                    run = self.update_from(root, evidence / coverage_test_fixture.floor.MEASUREMENT_DIRECTORY)
                    self.assertEqual(run.status, 2, run.stdout + run.stderr)
                    self.assertIn("is incomplete: the run stopped in phase 'publish'", run.stderr)

    def test_run_whose_provenance_writer_cannot_run_still_leaves_an_incomplete_record(self):
        # Once the coverage step starts, even a broken floor tool leaves a record that names where the run stopped.
        completed, root, _calls = self.run_gate("passes", broken_tool=True)
        self.assertEqual(completed.returncode, 5, completed.stdout + completed.stderr)
        evidence = root / coverage_test_fixture.floor.EVIDENCE_DIRECTORY
        self.assertEqual([path.name for path in evidence.iterdir()], [coverage_test_fixture.floor.PROVENANCE_FILE])
        document = json.loads((evidence / coverage_test_fixture.floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        self.assertEqual((document["schema"], document["measurement"], document["failed_phase"]),
                         (coverage_test_fixture.floor.PROVENANCE_SCHEMA, "incomplete", "self-test"))
        self.assertEqual(document["failure"], "exit status 5; no provenance record could be written")


class SessionRunnerTests(unittest.TestCase):
    """The gate harness bounds and reaps the whole process tree it starts, not only its direct child."""

    def test_timeout_kills_the_grandchild_of_a_hung_stand_in(self):
        # A hung stand-in for cargo leaves a grandchild holding the output pipes. subprocess.run's timeout would kill
        # only the gate script and leave both running; the harness kills the group and returns promptly.
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, str(directory), True)
        pid_file, cargo, gate = directory / "grandchild.pid", directory / "cargo", directory / "gate.sh"
        cargo.write_text('#!/bin/sh\nsleep 300 &\necho $! > "$1"\nwait\n', encoding="utf-8")
        cargo.chmod(0o755)
        # A second command keeps bash from replacing itself with the stand-in, so the sleep is a grandchild.
        gate.write_text(f'"{cargo}" "{pid_file}"\necho unreachable\n', encoding="utf-8")
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            coverage_test_fixture.run_in_session(["bash", str(gate)], str(directory), coverage_test_fixture.clean_env(), 2)
        self.assertLess(time.monotonic() - started, 30)
        grandchild = int(pid_file.read_text(encoding="utf-8"))

        def state():
            """The grandchild's process state: None once it is gone, else its /proc state letter, or ? without /proc."""
            try:
                os.kill(grandchild, 0)
            except ProcessLookupError:
                return None
            try:
                stat = Path(f"/proc/{grandchild}/stat").read_text(encoding="utf-8")
            except OSError:
                return "?"
            # The state follows the parenthesized command name, which may itself hold spaces or parentheses.
            return stat.rsplit(")", 1)[1].split()[0]

        def alive():
            # A killed grandchild reparented to a container's PID 1 that never reaps it stays a zombie (Z): it has
            # exited and holds no pipe, so it counts as killed.
            return state() not in (None, "Z")

        # The killed grandchild is reparented and reaped, or left a zombie, by its new parent; allow a moment for that.
        deadline = time.monotonic() + 5
        while alive() and time.monotonic() < deadline:
            time.sleep(0.05)
        if alive():
            survived = state()
            os.kill(grandchild, signal.SIGKILL)
            self.fail(f"grandchild {grandchild} survived the harness timeout (state {survived})")
