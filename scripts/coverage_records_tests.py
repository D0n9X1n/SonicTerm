"""Coverage records tests."""

from __future__ import annotations
import hashlib
import io
import json
from pathlib import Path
import shutil
import tempfile
import unittest

import coverage_test_fixture


class ProvenanceRecordTests(unittest.TestCase):
    """Each run's record says how far the measurement got and never claims a report it lacks."""

    def scratch_checkout(self):
        """Return a committed scratch checkout and a record path outside it."""
        directory = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, directory, True)
        checkout = Path(directory) / "checkout"
        checkout.mkdir()
        coverage_test_fixture.git(checkout, "init", "-q")
        coverage_test_fixture.commit_files(checkout, coverage_test_fixture.CHECKOUT)
        return checkout, Path(directory) / "record.json"

    def test_incomplete_run_names_its_failed_phase_and_carries_no_report(self):
        # A build or test failure leaves an incomplete record: the failed phase, its status, and no digest.
        checkout, output = self.scratch_checkout()
        document = coverage_test_fixture.record(checkout, output, "instrumented-tests", "failed", coverage_test_fixture.RUN_ENV, 101)
        self.assertEqual(document["schema"], coverage_test_fixture.floor.PROVENANCE_SCHEMA)
        self.assertEqual(document["measurement"], "incomplete")
        self.assertEqual((document["failed_phase"], document["failure"]), ("instrumented-tests", "exit status 101"))
        self.assertEqual(document["checks"], {"subset-gate": "not run", "floor": "not run"})
        self.assertIsNone(document["report_sha256"])
        self.assertIsNone(document["inventory_sha256"])
        self.assertEqual(document["commit"], coverage_test_fixture.git(checkout, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual((document["runner"], document["image_version"]), ("macos14/ARM64", "20260915.0150"))
        self.assertEqual((document["artifact"], document["workflow"]), (coverage_test_fixture.ARTIFACT, ".github/workflows/ci.yml"))
        self.assertEqual(document["rustc_version"], "rustc 1.98.1 (48a229cea 2026-09-01)")
        self.assertEqual(document["cargo_llvm_cov"], "cargo-llvm-cov 0.9.0")
        self.assertEqual(document["worktree_changes"], [])
        self.assertIn("crates/sonicterm-vt/src/vt.rs", document["tree_entries"])

    def test_interrupted_phase_leaves_its_write_ahead_record(self):
        # A run killed inside a phase keeps the record written when that phase started.
        checkout, output = self.scratch_checkout()
        document = coverage_test_fixture.record(checkout, output, "report", "running", coverage_test_fixture.RUN_ENV)
        self.assertEqual(document["measurement"], "incomplete")
        self.assertEqual((document["failed_phase"], document["failure"]),
                         ("report", "interrupted before the phase finished"))

    def test_complete_run_digests_its_files_even_when_a_check_fails(self):
        # The measurement is complete once the inventory exists; a failing 80% gate does not undo it.
        with coverage_test_fixture.retained_run(phase="subset-gate", state="failed", exit_status=1) as paths:
            document = json.loads(paths["provenance"].read_text(encoding="utf-8"))
            digests = (hashlib.sha256(paths["report"].read_bytes()).hexdigest(),
                       hashlib.sha256(paths["metadata"].read_bytes()).hexdigest())
        self.assertEqual(document["measurement"], "complete")
        self.assertIsNone(document["failed_phase"])
        self.assertEqual(document["checks"], {"subset-gate": "exit status 1", "floor": "not run"})
        self.assertEqual((document["report_sha256"], document["inventory_sha256"]), digests)

    def test_complete_record_without_its_files_is_refused(self):
        # The writer never claims a report it cannot digest.
        checkout, output = self.scratch_checkout()
        pin = output.with_name("pin.json")
        coverage_test_fixture.pin_checkout(checkout, pin)
        stderr = io.StringIO()
        status = coverage_test_fixture.floor.main(["record-provenance", "--output", str(output), "--root", str(checkout), "--pin", str(pin),
                             "--phase", "floor", "--state", "done"], env=coverage_test_fixture.RUN_ENV, stdout=io.StringIO(), stderr=stderr)
        self.assertEqual(status, 2)
        self.assertFalse(output.exists())
        self.assertIn("needs --report and --metadata", stderr.getvalue())


class ProvenanceWriterV2Tests(unittest.TestCase):
    """Records carry the pinned checkout, name drift at publish, and only write check pairs a run can reach."""

    def test_publish_record_names_each_kind_of_checkout_drift(self):
        # The publish record compares HEAD, its tree, and the coverage-relevant uncommitted changes with the pin;
        # documentation changes alone are not drift, and the record keeps the pinned commit.
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, str(directory), True)
        checkout, output = directory / "checkout", directory / "record.json"
        checkout.mkdir()
        coverage_test_fixture.git(checkout, "init", "-q")
        coverage_test_fixture.commit_files(checkout, coverage_test_fixture.CHECKOUT)
        document = coverage_test_fixture.record(checkout, output, "self-test", "running", coverage_test_fixture.RUN_ENV)
        pin = json.loads(output.with_name(output.name + ".pin.json").read_text(encoding="utf-8"))
        self.assertEqual(coverage_test_fixture.floor.checkout_drift(pin, str(checkout)), [])
        (checkout / "README.md").write_text("# reworded\n", encoding="utf-8")
        self.assertEqual(coverage_test_fixture.floor.checkout_drift(pin, str(checkout)), [])
        (checkout / "crates/sonicterm-vt/src/vt.rs").write_text("pub fn vt() { changed(); }\n", encoding="utf-8")
        self.assertEqual(coverage_test_fixture.floor.checkout_drift(pin, str(checkout)),
                         ["the coverage-relevant uncommitted changes moved from [] to [crates/sonicterm-vt/src/vt.rs]"])
        coverage_test_fixture.commit_files(checkout, {})
        head = coverage_test_fixture.git(checkout, "rev-parse", "HEAD").stdout.strip()
        drift = coverage_test_fixture.floor.checkout_drift(pin, str(checkout))
        self.assertEqual(len(drift), 2, drift)
        self.assertEqual(drift[0], f"HEAD moved from {document['commit']} to {head}")
        self.assertTrue(drift[1].startswith("the tree moved from "), drift)
        published = coverage_test_fixture.record(checkout, output, "publish", "running", coverage_test_fixture.RUN_ENV, expect=coverage_test_fixture.floor.DRIFT_EXIT_STATUS)
        self.assertEqual((published["measurement"], published["failed_phase"]), ("incomplete", "publish"))
        self.assertEqual(published["failure"], "checkout drifted from its pin: " + "; ".join(drift))
        self.assertEqual(published["checkout_drift"], drift)
        self.assertEqual((published["commit"], published["tree"]), (document["commit"], document["tree"]))

    def test_every_complete_record_the_writer_produces_passes_the_checks_rule(self):
        # The rule refuses only pairs the gate script cannot write, so no genuine complete record trips it.
        with coverage_test_fixture.retained_run() as paths:
            for phase, state, status in (("subset-gate", "running", 0), ("subset-gate", "failed", 1),
                                         ("subset-gate", "failed", 137), ("floor", "running", 0),
                                         ("floor", "failed", 1), ("floor", "failed", 2), ("floor", "done", 0)):
                with self.subTest(phase=phase, state=state, status=status):
                    document = coverage_test_fixture.record(paths["root"], paths["provenance"], phase, state, coverage_test_fixture.RUN_ENV, status,
                                      paths["report"], paths["metadata"])
                    self.assertEqual(document["measurement"], "complete")
                    self.assertIsNone(document["checkout_drift"])
                    self.assertEqual(coverage_test_fixture.floor.checks_problems(document["checks"]), [])
