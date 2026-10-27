#!/usr/bin/env python3
"""Target-only API cfg tests, including real Cargo dependency reuse on a tiny fixture."""
import importlib.util
import gate_catalog
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("harness_target", Path(__file__).with_name("harness-target.py"))
launcher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(launcher)
gate = launcher.gate


class TargetTests(unittest.TestCase):
    def test_artifact_selection_refuses_ordinary_or_ambiguous_binaries(self):
        # A successful compile cannot accidentally execute an old binary or the ordinary non-test example.
        record = {"reason": "compiler-artifact", "profile": {"test": True},
                  "target": {"name": "perf_scenarios", "kind": ["example"]}, "executable": "compiled-test", "manifest_path": "/crate/Cargo.toml"}
        self.assertEqual(gate.harness_test_executable(json.dumps(record)), ("compiled-test", Path("/crate")))
        with self.assertRaises(ValueError):
            gate.harness_test_executable(json.dumps(dict(record, profile={"test": False})))
        with self.assertRaises(ValueError):
            gate.harness_test_executable(json.dumps(record) + "\n" + json.dumps(dict(record, executable="other")))

    def test_cfg_tests_run_and_dependencies_remain_fresh(self):
        # Real Cargo proves --profile test runs libtest, cfg flags do not leak into dependencies,
        # and cfg-on/off target switches reuse the same dependency codegen artifacts.
        with tempfile.TemporaryDirectory(prefix="sonicterm-cfg-test-") as scratch:
            root = Path(scratch)
            (root / "examples").mkdir()
            (root / "dependency").mkdir()
            (root / "Cargo.toml").write_text('[package]\nname="sonicterm-app"\nversion="0.1.0"\nedition="2021"\n'
                '[dependencies]\ndependency={path="dependency"}\n', encoding="utf-8")
            (root / "dependency/Cargo.toml").write_text('[package]\nname="dependency"\nversion="0.1.0"\nedition="2021"\n'
                '[lib]\npath="lib.rs"\n', encoding="utf-8")
            (root / "dependency/lib.rs").write_text('#[cfg(perf_atlas_retry_api)] compile_error!("cfg leaked");\n'
                'pub fn value() -> bool { true }\n', encoding="utf-8")
            (root / "examples/perf_scenarios.rs").write_text('fn main() {}\n'
                '#[test] fn target_cfg_is_active() { assert!(dependency::value()); '
                'assert_eq!(cfg!(perf_atlas_retry_api), std::env::var("EXPECT_CFG").unwrap() == "1"); }\n', encoding="utf-8")
            for enabled in (True, False):
                step = gate.Step("fixture-cfg-test", ("cargo", "test", "-p", "sonicterm-app", "--example", "perf_scenarios"),
                                 gate.HOSTS, 60, "local", (), (),
                                 harness_cfgs=("perf_atlas_retry_api",) if enabled else ())
                environment = dict(os.environ, EXPECT_CFG="1" if enabled else "0", CARGO_TARGET_DIR=str(root / "target"))
                environment.pop("RUSTFLAGS", None)
                environment.pop("CARGO_ENCODED_RUSTFLAGS", None)
                if enabled:
                    environment["RUSTFLAGS"] = "--cfg perf_atlas_retry_api"
                with patch.object(gate_catalog, "STEPS", (step,)):
                    result = gate.run_step(step, int(enabled), root, root, environment)
                text = result.log_path.read_text(encoding="utf-8")
                self.assertTrue(result.accepted, text)
                self.assertIn("1 passed", text)
                if not enabled:
                    records = [json.loads(line) for line in text.splitlines() if line.startswith('{"reason"')]
                    dependency = [record for record in records if record.get("reason") == "compiler-artifact"
                                  and record.get("target", {}).get("name") == "dependency"]
                    self.assertEqual(len(dependency), 1, text)
                    self.assertTrue(dependency[0]["fresh"], text)

    def test_cfg_lints_are_target_only_and_include_test_bodies(self):
        # The package library must never inherit an API cfg. A lint existing only in cfg-on test code
        # must still fail, proving the metadata-only Clippy-driver pass actually visits test bodies.
        with tempfile.TemporaryDirectory(prefix="sonicterm-cfg-lint-") as scratch:
            root = Path(scratch)
            (root / "examples").mkdir()
            (root / "src").mkdir()
            (root / "Cargo.toml").write_text('[package]\nname="sonicterm-app"\nversion="0.1.0"\nedition="2021"\n'
                '[lints.rust]\nunexpected_cfgs={level="allow",check-cfg=["cfg(perf_atlas_retry_api)"]}\n', encoding="utf-8")
            (root / "src/lib.rs").write_text('#[cfg(perf_atlas_retry_api)] compile_error!("cfg leaked");\n'
                'pub fn value() -> bool { true }\n', encoding="utf-8")
            (root / "examples/perf_scenarios_alloc.rs").write_text("fn main() {}\n", encoding="utf-8")
            example = root / "examples/perf_scenarios.rs"
            example.write_text('fn main() { assert!(sonicterm_app::value()); }\n'
                '#[cfg(all(test, perf_atlas_retry_api))] #[test] fn linted() { '
                'let value = true; if value == true {} }\n', encoding="utf-8")
            step = gate.Step("fixture-cfg-lint", ("cargo", "clippy", "-p", "sonicterm-app", "--example", "perf_scenarios",
                             "--all-targets", "--", "-D", "clippy::bool_comparison"),
                             gate.HOSTS, 60, "local", (), (), harness_cfgs=("perf_atlas_retry_api",))
            environment = dict(os.environ, CARGO_TARGET_DIR=str(root / "target"))
            for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WORKSPACE_WRAPPER"):
                environment.pop(name, None)
            with patch.object(gate_catalog, "STEPS", (step,)):
                result = gate.run_step(step, 1, root, root, environment)
            text = result.log_path.read_text(encoding="utf-8")
            self.assertFalse(result.accepted, text)
            self.assertIn("equality checks against true are unnecessary", text)
            self.assertNotIn("error: cfg leaked", text)
            example.write_text(example.read_text(encoding="utf-8").replace("value == true", "value"), encoding="utf-8")
            with patch.object(gate_catalog, "STEPS", (step,)):
                result = gate.run_step(step, 2, root, root, environment)
            self.assertTrue(result.accepted, result.log_path.read_text(encoding="utf-8"))

    def test_allocation_root_is_linted_with_the_api_cfg_enabled(self):
        # An alloc-only cfg-on Clippy error escaped the former narrowed plan. Keep a real Cargo
        # regression rather than merely asserting that its name appears in an argv list.
        with tempfile.TemporaryDirectory(prefix="sonicterm-alloc-lint-") as scratch:
            root = Path(scratch)
            (root / "examples").mkdir()
            (root / "Cargo.toml").write_text('[package]\nname="sonicterm-app"\nversion="0.1.0"\nedition="2021"\n'
                '[[example]]\nname="perf_scenarios_alloc"\ntest=false\n', encoding="utf-8")
            (root / "examples/perf_scenarios.rs").write_text('fn main() {}\n', encoding="utf-8")
            (root / "examples/perf_scenarios_alloc.rs").write_text('fn main() { '
                '#[cfg(perf_atlas_retry_api)] { let value = true; if value == true {} } }\n', encoding="utf-8")
            step = gate.Step("fixture-alloc-lint", ("cargo", "clippy", "-p", "sonicterm-app", "--example", "perf_scenarios",
                             "--all-targets", "--", "-D", "clippy::bool_comparison"),
                             gate.HOSTS, 60, "local", (), (), harness_cfgs=("perf_atlas_retry_api",))
            environment = dict(os.environ, CARGO_TARGET_DIR=str(root / "target"))
            for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WORKSPACE_WRAPPER"):
                environment.pop(name, None)
            with patch.object(gate_catalog, "STEPS", (step,)):
                result = gate.run_step(step, 1, root, root, environment)
            self.assertFalse(result.accepted, result.log_path.read_text(encoding="utf-8"))
            self.assertIn("equality checks against true are unnecessary", result.log_path.read_text(encoding="utf-8"))

    def test_windows_direct_test_launch_has_cairo_and_cargo_native_paths(self):
        # PATH is case-insensitive on Windows; preserve the inherited spelling/content and restore
        # Cairo plus build-script native/deps directories without leaking host-specific paths elsewhere.
        with tempfile.TemporaryDirectory() as scratch:
            profile = Path(scratch) / "debug"
            executable = str(profile / "examples/perf_scenarios.exe")
            native = str(profile / "build/native/out")
            record = json.dumps({"reason": "build-script-executed", "linked_paths": ["native=" + native]})
            inherited = {"Path": "existing;tools", "SONICTERM_CAIRO_BIN_DIR": "cairo/bin"}
            actual = gate.harness_test_environment(inherited, executable, record, windows=True)
            self.assertEqual(actual["Path"].split(";"), ["cairo/bin", native, str(profile / "deps"), str(profile),
                                                        "existing", "tools"])
            self.assertNotIn("PATH", actual)
            self.assertEqual(inherited["Path"], "existing;tools")
            self.assertEqual(gate.harness_test_environment(inherited, executable, record, windows=False), inherited)
            source = (Path(__file__).parent / "setup-windows-cairo.ps1").read_text(encoding="utf-8")
            self.assertIn('SONICTERM_CAIRO_BIN_DIR = (Join-Path -Path $targetRoot -ChildPath "bin")', source)
            self.assertIn('Add-Content -LiteralPath $env:GITHUB_PATH -Value $cairoBin', source)


if __name__ == "__main__":
    unittest.main(verbosity=2)
