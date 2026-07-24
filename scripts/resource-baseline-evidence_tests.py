#!/usr/bin/env python3
"""Unit tests for scripts/resource-baseline-evidence.py."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

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
            ("macos-14", "windows-2022", "windows-latest"),
        )


class PlatformClassificationTests(unittest.TestCase):
    def test_macos_label_requires_a_real_macos_runtime(self):
        profile = evidence.classify_platform("macos-14", _macos_facts())
        self.assertEqual(profile["family"], "macos")
        self.assertEqual(profile["lane"], "macos")
        self.assertIsNone(profile["windows_build"])

        with self.assertRaises(evidence.EvidenceError):
            evidence.classify_platform("macos-14", _windows_facts(20348))

    def test_old_windows_lane_requires_a_pre_26100_runtime(self):
        profile = evidence.classify_platform("windows-2022", _windows_facts(20348))
        self.assertEqual(profile["family"], "windows")
        self.assertEqual(profile["lane"], "old-windows")
        self.assertEqual(profile["windows_build"], 20348)

        with self.assertRaises(evidence.EvidenceError):
            evidence.classify_platform("windows-2022", _windows_facts(26100))

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
        for runner_label, build in (("windows-2022", 20348), ("windows-latest", 26100)):
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
