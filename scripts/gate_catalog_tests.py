"""Gate catalog tests."""

from __future__ import annotations
import unittest

import gate_catalog
import gate_cli
import gate_harness
import gate_model
import gate_test_fixture
import gate_workflow


class TableTests(unittest.TestCase):
    """The step table is well formed and selects the right steps per host."""

    def test_every_step_is_well_formed(self):
        # Protect the invariants the runner, the renderer, and the parity checks rely on.
        ids = [step.id for step in gate_catalog.STEPS]
        self.assertEqual(len(ids), len(set(ids)))
        commands = [gate_harness.command_text(step) for step in gate_catalog.STEPS]
        self.assertEqual(len(commands), len(set(commands)))
        for step in gate_catalog.STEPS:
            with self.subTest(step=step.id):
                self.assertRegex(step.id, r"^[a-z][a-z0-9-]*$")
                self.assertTrue(step.argv)
                self.assertTrue(step.hosts)
                self.assertLessEqual(set(step.hosts), set(gate_model.HOSTS))
                self.assertEqual(len(step.hosts), len(set(step.hosts)))
                self.assertIn(step.evidence, gate_model.EVIDENCE_CLASSES)
                self.assertGreater(step.timeout_s, 0)
                self.assertTrue(step.prerequisites)
                self.assertLessEqual(set(step.prerequisites), set(gate_model.PREREQUISITES))
                self.assertIn(step.shell, (None, "pwsh"))
                if step.evidence == "optional":
                    self.assertEqual(step.ci_jobs, ())
                for job in step.ci_jobs:
                    self.assertIn(gate_workflow.job_host(job), step.hosts)

    def test_host_selection_keeps_table_order_and_opt_in_classes(self):
        # Protect the default gate: each host runs its local steps in table order, release
        # steps only on request, and the doctests after the build they reuse.
        macos = [step.id for step in gate_cli.select_steps("macos")]
        self.assertEqual(
            macos,
            [step.id for step in gate_catalog.STEPS if "macos" in step.hosts and step.evidence == "local"],
        )
        self.assertNotIn("release-macos", macos)
        self.assertIn("release-macos", [step.id for step in gate_cli.select_steps("macos", release=True)])
        self.assertEqual(macos.index("doctests"), macos.index("workspace-crates") + 1)
        windows = [step.id for step in gate_cli.select_steps("windows")]
        self.assertIn("msi-validator-tests", windows)
        self.assertIn("windows-warp-allocator", windows)
        self.assertNotIn("logic-coverage", windows)
        self.assertEqual(
            [step.id for step in gate_cli.select_steps("linux", ids=["doctests", "fmt"])],
            ["fmt", "doctests"],
        )
        with self.assertRaises(ValueError):
            gate_cli.select_steps("macos", ids=["no-such-step"])
        with self.assertRaises(ValueError):
            gate_cli.select_steps("macos", ids=["msi-validator-tests"])

    def test_doctest_step_compiles_workspace_doctests_without_an_exemption(self):
        # Protect the first-party doctest from going uncompiled: the step builds every workspace
        # doctest in each platform's workspace-test job, and no manifest opts a library out.
        step = next(step for step in gate_catalog.STEPS if step.id == "doctests")
        self.assertEqual(gate_harness.command_text(step), "cargo test --workspace --doc --no-fail-fast")
        self.assertEqual(step.evidence, "local")
        self.assertEqual(set(step.ci_jobs), {"macos-core", "windows-tests-workspace", "linux-core"})
        logging = (gate_test_fixture.ROOT / "crates" / "sonicterm-logging" / "src" / "lib.rs").read_text(encoding="utf-8")
        self.assertIn("//! ```no_run", logging)
        for manifest in sorted((gate_test_fixture.ROOT / "crates").glob("*/Cargo.toml")):
            if manifest.parent.name == "sonicterm-winit":
                continue
            with self.subTest(manifest=manifest.parent.name):
                self.assertNotRegex(manifest.read_text(encoding="utf-8"), r"(?m)^doctest\s*=\s*false")

    def test_pty_close_baseline_is_local_on_every_desktop(self):
        # The ignored native baseline is selected explicitly on every host, never hidden in CI-only evidence.
        matches = [step for step in gate_catalog.STEPS if step.id == "pty-close-baseline"]
        self.assertEqual(len(matches), 1)
        step = matches[0]
        self.assertEqual(
            gate_harness.command_text(step),
            "cargo test -p sonicterm-app --lib pty_close_baseline -- --ignored --nocapture",
        )
        self.assertEqual(step.hosts, gate_model.HOSTS)
        self.assertEqual(step.evidence, "local")
        self.assertEqual(step.prerequisites, ("rust", "native"))
        self.assertEqual(step.ci_jobs, ("macos-core", "windows-tests-runtime", "linux-core"))
        self.assertEqual(step.timeout_s, 1200)
        for host in gate_model.HOSTS:
            self.assertIn(step, gate_cli.select_steps(host))

    def test_perf_scenarios_unit_tests_run_on_every_host(self):
        # workspace-crates runs `cargo test --workspace --lib --bins --tests`, which skips examples, so the
        # scenario harness's unit tests need their own step on every host, right after the doctests.
        step = next((step for step in gate_catalog.STEPS if step.id == "perf-scenarios-tests"), None)
        self.assertIsNotNone(step, "no perf-scenarios-tests step")
        self.assertEqual(gate_harness.command_text(step), "cargo test --locked -p sonicterm-app --example perf_scenarios")
        self.assertEqual(step.hosts, gate_model.HOSTS)
        self.assertEqual(step.evidence, "local")
        self.assertEqual(step.prerequisites, ("rust", "native"))
        self.assertEqual(step.timeout_s, 900)
        self.assertEqual(step.ci_jobs, ("macos-core", "windows-tests-harness", "linux-core"))
        for host in gate_model.HOSTS:
            chosen = [selected.id for selected in gate_cli.select_steps(host)]
            self.assertEqual(chosen.index("perf-scenarios-tests"), chosen.index("doctests") + 1, host)
        jobs = gate_workflow.ci_job_commands(gate_test_fixture.WORKFLOW)
        for job in step.ci_jobs:
            commands = [command for _label, command in jobs[job]]
            # When: the job also runs the doctests, the harness tests follow them and reuse their build;
            # the Windows harness shard runs no doctests, so the harness tests are its first cargo command.
            if "cargo test --workspace --doc --no-fail-fast" in commands:
                doctests = commands.index("cargo test --workspace --doc --no-fail-fast")
                self.assertEqual(commands[doctests + 1], gate_harness.command_text(step), job)
            else:
                self.assertEqual(job, "windows-tests-harness")
                cargo = [command for command in commands if command.startswith("cargo ")]
                self.assertEqual(cargo[0], gate_harness.command_text(step), job)

    def test_native_selection_is_required_locally_on_macos(self):
        # The opt-in example must actually run; compilation and Windows execution are insufficient.
        by_id = {step.id: step for step in gate_catalog.STEPS}
        for name, command, timeout in (
            ("macos-selection-build", "cargo build --locked -p sonicterm-app --example native_split_selection", 1500),
            ("macos-selection-smoke", "python3 scripts/native-selection-smoke.py", 300),
        ):
            step = by_id[name]
            self.assertEqual(gate_harness.command_text(step), command)
            self.assertEqual(step.hosts, ("macos",))
            self.assertEqual(step.evidence, "local")
            self.assertEqual(step.ci_jobs, ("macos-core", "macos-smoke"))
            self.assertEqual(step.timeout_s, timeout)
            self.assertIn(step, gate_cli.select_steps("macos"))
        selected = [step.id for step in gate_cli.select_steps("macos")]
        self.assertLess(selected.index("macos-selection-build"), selected.index("macos-selection-smoke"))

    def test_command_text_matches_ci_spelling(self):
        # Protect verbatim parity: env prefixes and PowerShell paths render exactly as ci.yml
        # spells them, while the runner still launches the PowerShell script through pwsh.
        by_id = {step.id: step for step in gate_catalog.STEPS}
        self.assertEqual(
            gate_harness.command_text(by_id["doc"]),
            'RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps',
        )
        self.assertEqual(
            gate_harness.command_text(by_id["msi-validator-tests"]),
            ".\\scripts\\validate-windows-msi_tests.ps1",
        )
        self.assertEqual(
            gate_harness.launch_argv(by_id["msi-validator-tests"]),
            ("pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-File",
             ".\\scripts\\validate-windows-msi_tests.ps1"),
        )

    def test_host_detection(self):
        # Protect step selection on each desktop host, including Git Bash's MSYS Python.
        for system, host in (
            ("Darwin", "macos"),
            ("Linux", "linux"),
            ("Windows", "windows"),
            ("MSYS_NT-10.0-26100", "windows"),
            ("FreeBSD", None),
        ):
            with self.subTest(system=system):
                self.assertEqual(gate_cli.detect_host(system), host)

    def test_windows_target_is_an_optional_macos_aid_and_never_a_ci_gate(self):
        # Protect the Windows-target check from becoming a CI gate or a default local step.
        step = next(step for step in gate_catalog.STEPS if step.id == "windows-target")
        self.assertEqual(step.evidence, "optional")
        self.assertEqual(step.ci_jobs, ())
        self.assertEqual(step.hosts, ("macos",))
        self.assertEqual(gate_harness.command_text(step), "bash scripts/check-windows-target.sh")
        self.assertIn("win-target", step.prerequisites)
        self.assertNotIn(
            "windows-target", [step.id for step in gate_cli.select_steps("macos", release=True)]
        )
        self.assertIn("windows-target", [step.id for step in gate_cli.select_steps("macos", optional=True)])
        self.assertNotIn("check-windows-target.sh", gate_test_fixture.WORKFLOW)
