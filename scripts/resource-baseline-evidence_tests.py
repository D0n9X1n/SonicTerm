#!/usr/bin/env python3
"""Unit tests for scripts/resource-baseline-evidence.py."""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

_HERE = Path(__file__).resolve().parent
_COLLECTOR_PATH = _HERE / "resource-baseline-evidence.py"

_spec = importlib.util.spec_from_file_location("resource_baseline_evidence", _COLLECTOR_PATH)
evidence = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(evidence)


def _macos_facts() -> dict:
    return {
        "system": "Darwin",
        "release": "23.6.0",
        "version": "Darwin Kernel Version 23.6.0",
        "machine": "arm64",
        "macos_version": "14.7.5",
        "windows_build": None,
        "image_os": "macos14",
        "image_version": "20260720.1",
    }


def _windows_facts(build: int) -> dict:
    return {
        "system": "Windows",
        "release": "Server",
        "version": "10.0.{}".format(build),
        "machine": "AMD64",
        "macos_version": None,
        "windows_build": build,
        "image_os": "win22" if build < 26100 else "win25",
        "image_version": "20260720.1",
    }


def _live_soak(capabilities: dict) -> dict:
    sample = {
        "tick": 0,
        "elapsed_ms": 0,
        "rss_bytes": 32_000_000,
        "private_bytes": 24_000_000 if capabilities.get("private_bytes") else None,
        "gpu_estimate_bytes": None,
        "thread_count": 1,
        "handle_or_fd_count": 12,
        "process_count": None,
        "ledger_value": None,
    }
    return {
        "schema_version": "soak-harness/1",
        "status": "ok",
        "stop_reason": None,
        "error": None,
        "scenario": "noop",
        "data_source": "live",
        "params": {
            "seed": 0,
            "duration": 2,
            "warmup": 0,
            "cadence": 0,
            "max_samples": 100_000,
            "max_rss_bytes": None,
            "max_seconds": None,
        },
        "environment": {
            "platform": "test",
            "machine": "test",
            "python_version": "3.test",
            "generated_at_unix": 1_700_000_000,
        },
        "capabilities": capabilities,
        "sample_count": 2,
        "samples": [sample, dict(sample, tick=1, elapsed_ms=1)],
        "analysis": {"warmup_ticks": 0, "analyzed_samples": 2, "total_samples": 2, "fields": {}},
    }


def _cargo_success(spec) -> bytes:
    return (
        "\nrunning 1 test\n"
        "test {} ... ok\n\n"
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
        "43 filtered out; finished in 0.01s\n\n"
    ).format(spec.expected_test).encode("utf-8")


def _successful_executor(soak_result: dict):
    soak_bytes = (json.dumps(soak_result, sort_keys=True) + "\n").encode("utf-8")

    def execute(spec, _cwd):
        stdout = soak_bytes if spec.name == "soak-live" else _cargo_success(spec)
        return subprocess.CompletedProcess(spec.argv, 0, stdout=stdout, stderr=b"")

    return execute


def _collect(
    output: Path,
    *,
    runner_label: str = "macos-14",
    platform_facts: dict | None = None,
    soak_result: dict | None = None,
    executor=None,
) -> dict:
    if platform_facts is None:
        platform_facts = _macos_facts()
    if soak_result is None:
        soak_result = _live_soak(
            {
                "rss_bytes": True,
                "private_bytes": False,
                "gpu_estimate_bytes": False,
                "thread_count": True,
                "handle_or_fd_count": True,
                "process_count": False,
                "ledger_value": False,
            }
        )
    if executor is None:
        executor = _successful_executor(soak_result)
    return evidence.collect_evidence(
        repo_root=_HERE.parent,
        output_dir=output,
        runner_label=runner_label,
        source_sha="a" * 40,
        environ={
            "GITHUB_RUN_ID": "12345",
            "GITHUB_RUN_ATTEMPT": "2",
            "GITHUB_JOB": "unit-tests",
            "RUNNER_ARCH": platform_facts["machine"],
        },
        platform_facts=platform_facts,
        executor=executor,
        clock=lambda: 1_700_000_000.0,
    )


class SchemaContractTests(unittest.TestCase):
    def test_schema_version_and_claim_boundary_are_pinned(self):
        self.assertEqual(evidence.SCHEMA_VERSION, "resource-baseline-evidence/1")
        self.assertEqual(evidence.SUBJECT, "harness-process")
        self.assertEqual(evidence.CLAIM_SCOPE, "baseline-capture-capability")

    def test_only_expected_runner_labels_are_supported(self):
        self.assertEqual(
            evidence.RUNNER_LABELS,
            ("macos-14", "windows-latest"),
        )


class EnvironmentProvenanceTests(unittest.TestCase):
    def test_image_provenance_lookup_accepts_windows_uppercase_keys(self):
        with mock.patch.object(evidence.platform, "system", return_value="Windows"), \
             mock.patch.object(evidence.platform, "release", return_value="Server"), \
             mock.patch.object(evidence.platform, "version", return_value="10.0.26100"), \
             mock.patch.object(evidence.platform, "machine", return_value="AMD64"), \
             mock.patch.object(evidence, "_windows_build", return_value=26100):
            facts = evidence.platform_facts(
                {"IMAGEOS": "win25", "IMAGEVERSION": "20260720.1"}
            )

        self.assertEqual(facts["image_os"], "win25")
        self.assertEqual(facts["image_version"], "20260720.1")
        self.assertEqual(facts["windows_build"], 26100)

    def test_image_provenance_lookup_accepts_documented_mixed_case_keys(self):
        self.assertEqual(
            evidence._environment_value({"ImageOS": "macos14"}, "ImageOS"),
            "macos14",
        )
        self.assertEqual(
            evidence._environment_value({"ImageVersion": "20260720.1"}, "ImageVersion"),
            "20260720.1",
        )


class PlatformClassificationTests(unittest.TestCase):
    def test_macos_label_requires_a_real_macos_runtime(self):
        profile = evidence.classify_platform("macos-14", _macos_facts())
        self.assertEqual(profile["family"], "macos")
        self.assertEqual(profile["lane"], "macos")
        self.assertIsNone(profile["windows_build"])

        with self.assertRaises(evidence.EvidenceError):
            evidence.classify_platform("macos-14", _windows_facts(20348))

    def test_retired_runner_labels_are_rejected(self):
        with self.assertRaises(evidence.EvidenceError):
            evidence.classify_platform("windows-2022", _windows_facts(20348))

    def test_current_windows_lane_requires_build_26100_or_newer(self):
        profile = evidence.classify_platform("windows-latest", _windows_facts(26100))
        self.assertEqual(profile["family"], "windows")
        self.assertEqual(profile["lane"], "current-windows")
        self.assertEqual(profile["windows_build"], 26100)

        with self.assertRaises(evidence.EvidenceError):
            evidence.classify_platform("windows-latest", _windows_facts(20348))

    def test_unknown_runner_label_is_rejected(self):
        with self.assertRaises(evidence.EvidenceError):
            evidence.classify_platform("ubuntu-latest", _macos_facts())


class CommandContractTests(unittest.TestCase):
    def test_macos_runs_exact_real_pty_cleanup_and_harness_commands(self):
        profile = evidence.classify_platform("macos-14", _macos_facts())
        specs = evidence.command_specs(profile, "python3")
        self.assertEqual(
            [(spec.name, spec.argv) for spec in specs],
            [
                (
                    "pty-child-exit",
                    (
                        "cargo", "test", "-p", "sonicterm-io", "--lib",
                        "pty::pty_tests::child_exit_probe_observes_short_lived_process",
                        "--", "--exact", "--nocapture",
                    ),
                ),
                (
                    "pty-descendant-cleanup",
                    (
                        "cargo", "test", "-p", "sonicterm-io", "--lib",
                        "pty::pty_tests::observed_shell_exit_still_kills_background_process_group",
                        "--", "--exact", "--nocapture",
                    ),
                ),
                (
                    "soak-live",
                    (
                        "python3", "scripts/soak-harness.py", "--scenario", "noop",
                        "--live", "--duration", "64", "--warmup", "8", "--out", "-",
                    ),
                ),
            ],
        )

    def test_both_windows_lanes_run_exact_pty_and_close_drain_commands(self):
        expected = [
            (
                "pty-child-exit",
                (
                    "cargo", "test", "-p", "sonicterm-io", "--lib",
                    "pty::pty_tests::child_exit_probe_observes_short_lived_process",
                    "--", "--exact", "--nocapture",
                ),
            ),
            (
                "pty-thread-cleanup",
                (
                    "cargo", "test", "-p", "sonicterm-io", "--lib",
                    "pty::pty_tests::dropping_live_windows_pty_terminates_native_io_threads",
                    "--", "--exact", "--nocapture",
                ),
            ),
            (
                "conpty-close-drain",
                (
                    "cargo", "test", "-p", "sonicterm-io", "--lib",
                    "pty::pty_tests::conpty_close_runs_while_output_reader_is_draining",
                    "--", "--exact", "--nocapture",
                ),
            ),
            (
                "soak-live",
                (
                    "python", "scripts/soak-harness.py", "--scenario", "noop",
                    "--live", "--duration", "64", "--warmup", "8", "--out", "-",
                ),
            ),
        ]
        for runner_label, build in (("windows-latest", 26100),):
            with self.subTest(runner_label=runner_label):
                profile = evidence.classify_platform(runner_label, _windows_facts(build))
                specs = evidence.command_specs(profile, "python")
                self.assertEqual([(spec.name, spec.argv) for spec in specs], expected)


class FocusedTestValidationTests(unittest.TestCase):
    def setUp(self):
        profile = evidence.classify_platform("macos-14", _macos_facts())
        self.spec = evidence.command_specs(profile, "python3")[0]

    def test_exact_expected_test_and_single_pass_summary_are_required(self):
        self.assertEqual(evidence.validate_test_result(self.spec, _cargo_success(self.spec)), [])

        cases = {
            "zero matched": (
                b"\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; "
                b"0 ignored; 0 measured; 44 filtered out; finished in 0.00s\n",
                "did not pass exactly once",
            ),
            "ignored": (
                (
                    "\nrunning 1 test\n"
                    "test {} ... ignored\n\n"
                    "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; "
                    "43 filtered out; finished in 0.00s\n"
                ).format(self.spec.expected_test).encode("utf-8"),
                "0 passed, 0 failed, 1 ignored",
            ),
            "wrong name": (
                b"\nrunning 1 test\ntest another::test ... ok\n\n"
                b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                b"43 filtered out; finished in 0.00s\n",
                "did not pass exactly once",
            ),
        }
        for label, (stdout, expected_error) in cases.items():
            with self.subTest(label=label):
                errors = evidence.validate_test_result(self.spec, stdout)
                self.assertTrue(
                    any(expected_error in error for error in errors),
                    errors,
                )

    def test_exit_zero_zero_test_bundle_fails_closed(self):
        soak = _live_soak(
            {
                "rss_bytes": True,
                "private_bytes": False,
                "gpu_estimate_bytes": False,
                "thread_count": True,
                "handle_or_fd_count": True,
                "process_count": False,
                "ledger_value": False,
            }
        )
        successful = _successful_executor(soak)

        def execute(spec, cwd):
            if spec.name == "pty-child-exit":
                return subprocess.CompletedProcess(
                    spec.argv,
                    0,
                    stdout=(
                        b"\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; "
                        b"0 ignored; 0 measured; 44 filtered out; finished in 0.00s\n"
                    ),
                    stderr=b"",
                )
            return successful(spec, cwd)

        with tempfile.TemporaryDirectory() as directory:
            document = _collect(Path(directory), executor=execute)

        self.assertEqual(document["status"], "fail")
        self.assertTrue(
            any("did not pass exactly once" in error for error in document["errors"]),
            document["errors"],
        )


class LiveResultValidationTests(unittest.TestCase):
    def test_macos_requires_each_real_signal_and_rejects_private_bytes_claim(self):
        profile = evidence.classify_platform("macos-14", _macos_facts())
        capabilities = {
            "rss_bytes": True,
            "private_bytes": False,
            "gpu_estimate_bytes": False,
            "thread_count": True,
            "handle_or_fd_count": True,
            "process_count": False,
            "ledger_value": False,
        }
        self.assertEqual(evidence.validate_live_result(profile, _live_soak(capabilities)), [])

        for field in ("rss_bytes", "thread_count", "handle_or_fd_count"):
            with self.subTest(field=field):
                unavailable = dict(capabilities, **{field: False})
                self.assertIn(
                    "required live capability is unavailable: {}".format(field),
                    evidence.validate_live_result(profile, _live_soak(unavailable)),
                )

        false_claim = dict(capabilities, private_bytes=True)
        self.assertIn(
            "macOS private_bytes capability must remain unavailable",
            evidence.validate_live_result(profile, _live_soak(false_claim)),
        )

    def test_windows_requires_each_real_signal(self):
        profile = evidence.classify_platform("windows-latest", _windows_facts(26100))
        capabilities = {
            "rss_bytes": True,
            "private_bytes": True,
            "gpu_estimate_bytes": False,
            "thread_count": True,
            "handle_or_fd_count": True,
            "process_count": False,
            "ledger_value": False,
        }
        self.assertEqual(evidence.validate_live_result(profile, _live_soak(capabilities)), [])

        for field in ("rss_bytes", "private_bytes", "thread_count", "handle_or_fd_count"):
            with self.subTest(field=field):
                unavailable = dict(capabilities, **{field: False})
                self.assertIn(
                    "required live capability is unavailable: {}".format(field),
                    evidence.validate_live_result(profile, _live_soak(unavailable)),
                )

    def test_synthetic_or_failed_result_is_rejected(self):
        profile = evidence.classify_platform("macos-14", _macos_facts())
        result = _live_soak(
            {
                "rss_bytes": True,
                "private_bytes": False,
                "gpu_estimate_bytes": False,
                "thread_count": True,
                "handle_or_fd_count": True,
                "process_count": False,
                "ledger_value": False,
            }
        )
        result["data_source"] = "synthetic"
        result["status"] = "error"
        errors = evidence.validate_live_result(profile, result)
        self.assertIn("soak result data_source must be live", errors)
        self.assertIn("soak result status must be ok", errors)

    def test_malformed_result_returns_validation_errors(self):
        profile = evidence.classify_platform("macos-14", _macos_facts())
        errors = evidence.validate_live_result(profile, {"schema_version": "unknown"})
        self.assertIn("soak schema must be soak-harness/1", errors)
        self.assertTrue(any("capabilities" in error for error in errors))


class ExecutorDeadlineTests(unittest.TestCase):
    def test_every_collector_command_has_a_bounded_deadline(self):
        profile = evidence.classify_platform("windows-latest", _windows_facts(26100))
        specs = evidence.command_specs(profile, "python")

        self.assertEqual(
            [(spec.name, spec.timeout_seconds) for spec in specs],
            [
                ("pty-child-exit", 30),
                ("pty-thread-cleanup", 30),
                ("conpty-close-drain", 30),
                ("soak-live", 90),
            ],
        )

    def test_collector_reports_each_active_command(self):
        output = io.StringIO()
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(output):
            document = _collect(Path(directory))

        self.assertEqual(document["status"], "pass")
        text = output.getvalue()
        for name in ("pty-child-exit", "pty-descendant-cleanup", "soak-live"):
            self.assertIn(
                "resource-baseline: start {} (timeout=".format(name), text
            )
            self.assertIn(
                "resource-baseline: finish {} (exit=0, duration=".format(name), text
            )

    def test_timeout_kills_descendants_and_preserves_partial_output(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "descendant-survived"
            child = (
                "import pathlib,time; time.sleep(3); "
                "pathlib.Path({!r}).write_text('alive')"
            ).format(str(marker))
            code = (
                "import subprocess,sys,time; "
                "subprocess.Popen([sys.executable, '-c', {!r}]); "
                "print('partial', flush=True); time.sleep(60)"
            ).format(child)
            spec = evidence.CommandSpec(
                "timeout-fixture",
                (sys.executable, "-c", code),
                timeout_seconds=1,
            )

            started = time.monotonic()
            completed = evidence._default_executor(spec, _HERE.parent)
            elapsed = time.monotonic() - started
            time.sleep(3)

            self.assertLess(elapsed, 10)
            self.assertEqual(completed.returncode, evidence.TIMEOUT_RETURN_CODE)
            self.assertIn(b"partial", completed.stdout)
            self.assertIn(b"timed out after 1 seconds", completed.stderr)
            self.assertFalse(marker.exists(), "timed-out command left its descendant running")

    def test_every_workflow_step_has_one_positive_timeout(self):
        invalid = []
        workflows = sorted((_HERE.parent / ".github" / "workflows").glob("*.yml"))
        for path in workflows:
            text = path.read_text(encoding="utf-8")
            steps = re.split(r"(?m)^      - ", text)[1:]
            for step in steps:
                identity = re.match(r"(?:name: ([^\n]+)|uses: ([^\n]+))", step)
                if identity is None:
                    continue
                label = next(value for value in identity.groups() if value is not None)
                values = re.findall(r"(?m)^        timeout-minutes: (\S+)$", step)
                if len(values) != 1 or not values[0].isdigit() or int(values[0]) <= 0:
                    invalid.append((path.name, label, values))

        self.assertEqual(invalid, [])

    def test_every_workflow_job_has_one_positive_timeout(self):
        invalid = []
        workflows = sorted((_HERE.parent / ".github" / "workflows").glob("*.yml"))
        for path in workflows:
            text = path.read_text(encoding="utf-8").split("\njobs:\n", 1)[1]
            parts = re.split(r"(?m)^  ([A-Za-z0-9_-]+):\n", text)[1:]
            for index in range(0, len(parts), 2):
                name, job = parts[index : index + 2]
                values = re.findall(r"(?m)^    timeout-minutes: (\S+)$", job)
                if len(values) != 1 or not values[0].isdigit() or int(values[0]) <= 0:
                    invalid.append((path.name, name, values))

        self.assertEqual(invalid, [])

    def test_every_step_timeout_is_reachable_inside_its_job(self):
        invalid = []
        workflows = sorted((_HERE.parent / ".github" / "workflows").glob("*.yml"))
        for path in workflows:
            text = path.read_text(encoding="utf-8").split("\njobs:\n", 1)[1]
            parts = re.split(r"(?m)^  ([A-Za-z0-9_-]+):\n", text)[1:]
            for index in range(0, len(parts), 2):
                name, job = parts[index : index + 2]
                job_timeout = int(re.search(r"(?m)^    timeout-minutes: (\d+)$", job).group(1))
                for step_timeout in re.findall(r"(?m)^        timeout-minutes: (\d+)$", job):
                    if int(step_timeout) >= job_timeout:
                        invalid.append((path.name, name, job_timeout, int(step_timeout)))

        self.assertEqual(invalid, [])

    def test_ci_jobs_keep_outer_timeouts(self):
        workflow = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        expected = {
            "macos-core": 45,
            "macos-coverage": 35,
            "macos": 5,
            "windows-native": 50,
            "windows-checks": 45,
            "windows-tests": 45,
            "windows": 5,
            "linux-core": 45,
            "linux-packages": 45,
            "linux": 5,
        }
        for name, minutes in expected.items():
            with self.subTest(job=name):
                job = workflow.split("  {}:\n".format(name), 1)[1]
                job = re.split(r"(?m)^  [A-Za-z0-9_-]+:\n", job, maxsplit=1)[0]
                self.assertIn("timeout-minutes: {}".format(minutes), job.split("    steps:\n", 1)[0])

    def test_slow_ci_stages_keep_cold_cache_headroom(self):
        workflow = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        expected = {
            ("macos-core", "Run workspace unit and integration tests"): 35,
            ("macos-core", "Capture real resource baseline evidence"): 10,
            ("macos-coverage", "Install cargo-llvm-cov"): 10,
            ("macos-coverage", "Run Rust logic coverage gate"): 25,
            ("windows-native", "Install Cairo for Windows"): 30,
            ("windows-tests", "Run workspace unit and integration tests"): 35,
            ("windows-tests", "Capture real resource baseline evidence"): 10,
            ("windows-tests", "Test MSI validator"): 5,
            ("linux-core", "Run workspace unit and integration tests"): 35,
            ("linux-packages", "Build Linux release binary"): 30,
            ("linux-packages", "Build and validate Linux packages"): 15,
        }
        for (job_name, step_name), minutes in expected.items():
            with self.subTest(job=job_name, step=step_name):
                job = workflow.split("  {}:\n".format(job_name), 1)[1]
                job = re.split(r"(?m)^  [A-Za-z0-9_-]+:\n", job, maxsplit=1)[0]
                step = job.split("- name: {}".format(step_name), 1)[1]
                step = re.split(r"(?m)^      - ", step, maxsplit=1)[0]
                self.assertIn("timeout-minutes: {}".format(minutes), step)

        native = workflow.split("  windows-native:\n", 1)[1]
        native = re.split(r"(?m)^  [A-Za-z0-9_-]+:\n", native, maxsplit=1)[0]
        job_timeout = int(re.search(r"(?m)^    timeout-minutes: (\d+)$", native).group(1))
        step_timeouts = [
            int(value)
            for value in re.findall(r"(?m)^        timeout-minutes: (\d+)$", native)
        ]
        self.assertGreaterEqual(job_timeout, sum(step_timeouts))

    def test_slow_release_stages_keep_cold_cache_headroom(self):
        workflow = (_HERE.parent / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        validation = workflow.split("  validate-release-tag:\n", 1)[1]
        validation = re.split(r"(?m)^  [A-Za-z0-9_-]+:\n", validation, maxsplit=1)[0]
        self.assertIn("timeout-minutes: 35", validation.split("    steps:\n", 1)[0])

        expected = {
            ("build-mac-x86_64", "Build x86_64"): 30,
            ("build-mac-aarch64", "Build aarch64"): 30,
            ("build-windows", "Install cargo-wix"): 10,
            ("build-windows", "Install WiX Toolset"): 10,
            ("build-windows", "Build release binary"): 30,
            ("build-windows", "Validate MSI metadata"): 5,
            ("package-linux", "Build Linux release binary"): 30,
        }
        for (job_name, step_name), minutes in expected.items():
            with self.subTest(job=job_name, step=step_name):
                job = workflow.split("  {}:\n".format(job_name), 1)[1]
                job = re.split(r"(?m)^  [A-Za-z0-9_-]+:\n", job, maxsplit=1)[0]
                step = job.split("- name: {}".format(step_name), 1)[1]
                step = re.split(r"(?m)^      - ", step, maxsplit=1)[0]
                self.assertIn("timeout-minutes: {}".format(minutes), step)


class EvidenceBundleTests(unittest.TestCase):
    def test_success_bundle_records_truthful_scope_and_integrity_hashes(self):
        soak_result = _live_soak(
            {
                "rss_bytes": True,
                "private_bytes": False,
                "gpu_estimate_bytes": False,
                "thread_count": True,
                "handle_or_fd_count": True,
                "process_count": False,
                "ledger_value": False,
            }
        )
        soak_bytes = (json.dumps(soak_result, sort_keys=True) + "\n").encode("utf-8")
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            document = _collect(output, soak_result=soak_result)

            self.assertEqual(document["status"], "pass")
            self.assertEqual(document["subject"], "harness-process")
            self.assertEqual(document["claim_scope"], "baseline-capture-capability")
            provenance = document["provenance"]
            self.assertEqual(provenance["source_sha"], "a" * 40)
            self.assertEqual(provenance["runner_label"], "macos-14")
            self.assertEqual(provenance["github_run_id"], "12345")
            self.assertEqual(provenance["github_run_attempt"], "2")
            self.assertEqual(provenance["github_job"], "unit-tests")
            self.assertEqual(provenance["runner_arch"], "arm64")
            self.assertEqual(provenance["image_os"], "macos14")
            self.assertEqual(provenance["image_version"], "20260720.1")
            self.assertEqual(provenance["adapter"], "not-applicable")
            self.assertEqual(provenance["started_at_unix"], 1_700_000_000)
            self.assertEqual(provenance["finished_at_unix"], 1_700_000_000)
            self.assertEqual(provenance["duration_ms"], 0)
            self.assertEqual(document["platform"]["system"], "Darwin")
            self.assertEqual(document["platform"]["release"], "23.6.0")
            self.assertEqual(document["platform"]["version"], "Darwin Kernel Version 23.6.0")
            self.assertEqual(document["platform"]["machine"], "arm64")
            self.assertEqual(document["platform"]["macos_version"], "14.7.5")
            self.assertEqual(document["platform"]["lane"], "macos")
            expected_specs = evidence.command_specs(document["platform"], "python3")
            expected_commands = [
                {"name": spec.name, "argv": list(spec.argv)}
                for spec in expected_specs
            ]
            self.assertEqual(document["commands"], expected_commands)
            self.assertEqual(
                [
                    {
                        "name": result["name"],
                        "argv": result["argv"],
                        "started_at_unix": result["started_at_unix"],
                        "finished_at_unix": result["finished_at_unix"],
                        "duration_ms": result["duration_ms"],
                        "returncode": result["returncode"],
                    }
                    for result in document["command_results"]
                ],
                [
                    {
                        "name": command["name"],
                        "argv": command["argv"],
                        "started_at_unix": 1_700_000_000,
                        "finished_at_unix": 1_700_000_000,
                        "duration_ms": 0,
                        "returncode": 0,
                    }
                    for command in expected_commands
                ],
            )
            self.assertEqual(
                (output / "pty-child-exit.stdout.log").read_bytes(),
                _cargo_success(expected_specs[0]),
            )
            self.assertEqual((output / "pty-child-exit.stderr.log").read_bytes(), b"")
            self.assertEqual(
                (output / "pty-descendant-cleanup.stdout.log").read_bytes(),
                _cargo_success(expected_specs[1]),
            )
            self.assertEqual((output / "pty-descendant-cleanup.stderr.log").read_bytes(), b"")
            self.assertEqual((output / "soak-live.stdout.log").read_bytes(), soak_bytes)
            self.assertEqual((output / "soak-live.stderr.log").read_bytes(), b"")
            self.assertTrue((output / "evidence.json").is_file())
            self.assertEqual((output / "soak-live.json").read_bytes(), soak_bytes)
            self.assertTrue((output / "SHA256SUMS").is_file())
            self._assert_checksums_match(output)

    def test_command_failure_still_emits_logs_evidence_and_checksums(self):
        soak = _live_soak(
            {
                "rss_bytes": True,
                "private_bytes": False,
                "gpu_estimate_bytes": False,
                "thread_count": True,
                "handle_or_fd_count": True,
                "process_count": False,
                "ledger_value": False,
            }
        )
        successful = _successful_executor(soak)

        def execute(spec, cwd):
            if spec.name == "pty-child-exit":
                return subprocess.CompletedProcess(spec.argv, 1, stdout=b"partial\n", stderr=b"failed\n")
            return successful(spec, cwd)

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            document = _collect(output, executor=execute)

            self.assertEqual(document["status"], "fail")
            self.assertTrue(any("pty-child-exit exited with 1" in error for error in document["errors"]))
            self.assertEqual((output / "pty-child-exit.stdout.log").read_bytes(), b"partial\n")
            self.assertEqual((output / "pty-child-exit.stderr.log").read_bytes(), b"failed\n")
            macos_specs = evidence.command_specs(
                evidence.classify_platform("macos-14", _macos_facts()),
                "python3",
            )
            self.assertEqual(
                (output / "pty-descendant-cleanup.stdout.log").read_bytes(),
                _cargo_success(macos_specs[1]),
            )
            self.assertEqual((output / "pty-descendant-cleanup.stderr.log").read_bytes(), b"")
            expected_soak = (json.dumps(soak, sort_keys=True) + "\n").encode("utf-8")
            self.assertEqual((output / "soak-live.stdout.log").read_bytes(), expected_soak)
            self.assertEqual((output / "soak-live.stderr.log").read_bytes(), b"")
            self.assertEqual((output / "soak-live.json").read_bytes(), expected_soak)
            self.assertTrue((output / "evidence.json").is_file())
            self._assert_checksums_match(output)

    def test_timeout_is_recorded_and_later_evidence_still_emits(self):
        soak = _live_soak(
            {
                "rss_bytes": True,
                "private_bytes": False,
                "gpu_estimate_bytes": False,
                "thread_count": True,
                "handle_or_fd_count": True,
                "process_count": False,
                "ledger_value": False,
            }
        )
        successful = _successful_executor(soak)
        calls = []

        def execute(spec, cwd):
            calls.append(spec.name)
            if spec.name == "pty-child-exit":
                return subprocess.CompletedProcess(
                    spec.argv,
                    evidence.TIMEOUT_RETURN_CODE,
                    stdout=b"partial\n",
                    stderr=b"pty-child-exit timed out after 30 seconds\n",
                )
            return successful(spec, cwd)

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            document = _collect(output, executor=execute)

            self.assertEqual(document["status"], "fail")
            self.assertEqual(calls, ["pty-child-exit", "pty-descendant-cleanup", "soak-live"])
            self.assertTrue(
                any("pty-child-exit exited with 124" in error for error in document["errors"])
            )
            self.assertEqual((output / "pty-child-exit.stdout.log").read_bytes(), b"partial\n")
            self.assertIn(b"timed out after 30 seconds", (output / "pty-child-exit.stderr.log").read_bytes())
            self.assertTrue((output / "evidence.json").is_file())
            self.assertTrue((output / "SHA256SUMS").is_file())
            self._assert_checksums_match(output)

    def test_malformed_soak_output_is_preserved_and_fails_closed(self):
        def execute(spec, _cwd):
            stdout = b"not json\n" if spec.name == "soak-live" else _cargo_success(spec)
            return subprocess.CompletedProcess(spec.argv, 0, stdout=stdout, stderr=b"")

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            document = _collect(output, executor=execute)

            self.assertEqual(document["status"], "fail")
            self.assertTrue(any("soak-live output is not valid JSON" in error for error in document["errors"]))
            self.assertEqual((output / "soak-live.json").read_bytes(), b"not json\n")
            self._assert_checksums_match(output)

    def test_platform_validation_failure_emits_without_running_commands(self):
        calls = []

        def execute(spec, _cwd):
            calls.append(spec.name)
            raise AssertionError("commands must not run for an invalid platform")

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            document = _collect(
                output,
                runner_label="windows-latest",
                platform_facts=_windows_facts(20348),
                executor=execute,
            )

            self.assertEqual(document["status"], "fail")
            self.assertEqual(calls, [])
            self.assertTrue(any("windows-latest requires Windows build 26100 or newer" in error for error in document["errors"]))
            self.assertTrue((output / "evidence.json").is_file())
            self._assert_checksums_match(output)

    def _assert_checksums_match(self, output: Path) -> None:
        lines = (output / "SHA256SUMS").read_text(encoding="utf-8").splitlines()
        self.assertEqual(lines, sorted(lines, key=lambda line: line.split("  ", 1)[1]))
        names = []
        for line in lines:
            digest, name = line.split("  ", 1)
            names.append(name)
            self.assertNotEqual(name, "SHA256SUMS")
            self.assertEqual(hashlib.sha256((output / name).read_bytes()).hexdigest(), digest)
        emitted = {path.name for path in output.iterdir()} - {"SHA256SUMS"}
        self.assertEqual(set(names), emitted)
        self.assertIn("evidence.json", names)


class CliContractTests(unittest.TestCase):
    def test_cli_accepts_required_arguments_and_exits_zero_on_pass(self):
        capabilities = {
            "rss_bytes": True,
            "private_bytes": False,
            "gpu_estimate_bytes": False,
            "thread_count": True,
            "handle_or_fd_count": True,
            "process_count": False,
            "ledger_value": False,
        }
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "evidence"
            code = evidence.main(
                ["--runner-label", "macos-14", "--output-dir", str(output)],
                repo_root=_HERE.parent,
                source_sha="a" * 40,
                environ={
                    "GITHUB_RUN_ID": "12345",
                    "GITHUB_RUN_ATTEMPT": "2",
                    "GITHUB_JOB": "unit-tests",
                    "RUNNER_ARCH": "arm64",
                },
                platform_facts=_macos_facts(),
                executor=_successful_executor(_live_soak(capabilities)),
                clock=lambda: 1_700_000_000.0,
            )

            self.assertEqual(code, 0)
            document = json.loads((output / "evidence.json").read_text(encoding="utf-8"))
            self.assertEqual(document["status"], "pass")
            self.assertTrue((output / "SHA256SUMS").is_file())

    def test_cli_exits_nonzero_on_failed_evidence_and_keeps_artifacts(self):
        capabilities = {
            "rss_bytes": True,
            "private_bytes": False,
            "gpu_estimate_bytes": False,
            "thread_count": True,
            "handle_or_fd_count": True,
            "process_count": False,
            "ledger_value": False,
        }
        successful = _successful_executor(_live_soak(capabilities))

        def execute(spec, cwd):
            if spec.name == "pty-child-exit":
                return subprocess.CompletedProcess(spec.argv, 9, stdout=b"partial\n", stderr=b"failed\n")
            return successful(spec, cwd)

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "evidence"
            code = evidence.main(
                ["--runner-label", "macos-14", "--output-dir", str(output)],
                repo_root=_HERE.parent,
                source_sha="a" * 40,
                environ={
                    "GITHUB_RUN_ID": "12345",
                    "GITHUB_RUN_ATTEMPT": "2",
                    "GITHUB_JOB": "unit-tests",
                    "RUNNER_ARCH": "arm64",
                },
                platform_facts=_macos_facts(),
                executor=execute,
                clock=lambda: 1_700_000_000.0,
            )

            self.assertNotEqual(code, 0)
            document = json.loads((output / "evidence.json").read_text(encoding="utf-8"))
            self.assertEqual(document["status"], "fail")
            self.assertEqual((output / "pty-child-exit.stdout.log").read_bytes(), b"partial\n")
            self.assertEqual((output / "pty-child-exit.stderr.log").read_bytes(), b"failed\n")
            self.assertTrue((output / "SHA256SUMS").is_file())


if __name__ == "__main__":
    unittest.main(verbosity=2)
