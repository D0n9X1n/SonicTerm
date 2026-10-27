"""Coverage repository tests."""

from __future__ import annotations
import json
import re
import subprocess
import sys
import unittest

import coverage_test_fixture


class RepositoryContractTests(unittest.TestCase):
    """The committed baseline and gate script agree with the repository."""

    def test_committed_baseline_is_valid_and_keyed_to_the_linux_coverage_container(self):
        # The committed floor loads and names the host CI enforces it on: linux-coverage's ubuntu:22.04 container.
        committed = coverage_test_fixture.floor.load_baseline(str(coverage_test_fixture._HERE / "coverage-baseline.json"))
        self.assertEqual((committed.target, committed.runner), ("x86_64-unknown-linux-gnu", "ubuntu22-container/X64"))
        self.assertEqual(committed.tolerance_pp, 1.0)

    def test_committed_macos_section_names_the_macos_only_logic_files(self):
        # The macOS-only floor covers the subset files whose AppKit and macOS branches compile only on macOS.
        committed = coverage_test_fixture.floor.load_baseline(str(coverage_test_fixture._HERE / "coverage-baseline.json"))
        self.assertIsNotNone(committed.macos)
        self.assertEqual((committed.macos.target, committed.macos.runner), ("aarch64-apple-darwin", "macos14/ARM64"))
        self.assertEqual(set(committed.macos.files), {"crates/sonicterm-ui/src/text_edit.rs",
                                                      "crates/sonicterm-ui/src/command_label.rs"})
        for file_path in committed.macos.files:
            self.assertTrue((coverage_test_fixture._REPO / file_path).is_file(), file_path)

    def test_baseline_names_are_workspace_packages(self):
        # A typo fails here in seconds; the full inventory check needs the instrumented run.
        manifest = (coverage_test_fixture._REPO / "Cargo.toml").read_text(encoding="utf-8")
        table = re.search(r"(?ms)^\[workspace\]\n(.*?)^\[", manifest).group(1)
        members = re.findall(r'"([^"]+)"', re.search(r"(?ms)^members\s*=\s*\[(.*?)\]", table).group(1))
        names = set()
        for member in members:
            text = (coverage_test_fixture._REPO / member / "Cargo.toml").read_text(encoding="utf-8")
            package = re.search(r"(?ms)^\[package\]\n(.*?)(?=^\[|\Z)", text).group(1)
            names.add(re.search(r'(?m)^name\s*=\s*"([^"]+)"', package).group(1))
        self.assertIn("sonicterm-vt", names)
        committed = coverage_test_fixture.floor.load_baseline(str(coverage_test_fixture._HERE / "coverage-baseline.json"))
        self.assertLessEqual(set(committed.crates) | set(committed.not_measured), names)

    def test_generated_sources_equal_the_marked_first_party_files(self):
        # A newly committed generated file cannot be counted as authored code without failing here.
        sources = coverage_test_fixture.first_party_rust_sources()
        self.assertGreater(len(sources), 100)
        marked = {path for path in sources if coverage_test_fixture.has_generator_marker(path)}
        for path in coverage_test_fixture.floor.GENERATED_SOURCES:
            with self.subTest(path=path):
                self.assertTrue((coverage_test_fixture._REPO / path).is_file())
        # A listed file without a marker must be pinned here with the reason it is generated.
        unmarked_generated = {}
        self.assertLessEqual(set(unmarked_generated), set(coverage_test_fixture.floor.GENERATED_SOURCES))
        self.assertEqual(set(coverage_test_fixture.floor.GENERATED_SOURCES) - set(unmarked_generated), marked)
        # Pinned authored: hand-written FFI declarations stay counted even though they resemble bindings.
        hand_written = {
            "crates/sonicterm-fontconfig/src/lib.rs":
                "Servo's hand-written Fontconfig declarations; the file opens with the Servo copyright header",
        }
        for path, reason in hand_written.items():
            with self.subTest(path=path, reason=reason):
                self.assertNotIn(path, coverage_test_fixture.floor.GENERATED_SOURCES)
                self.assertNotIn(path, marked)
                first = (coverage_test_fixture._REPO / path).read_text(encoding="utf-8").splitlines()[0]
                self.assertEqual(first, "// Copyright 2013 The Servo Project Developers. See the COPYRIGHT")

    def test_vendored_roots_match_the_native_dependency_manifest(self):
        # Vendored exclusions follow the pinned import list instead of a second copy.
        manifest = json.loads((coverage_test_fixture._HERE / "native-dependencies.json").read_text(encoding="utf-8"))
        self.assertEqual(sorted(coverage_test_fixture.floor.VENDORED_ROOTS), sorted(library["path"] for library in manifest["libraries"]))

    def test_gate_measures_vt_keeps_the_subset_floor_and_runs_the_crate_floor(self):
        script = (coverage_test_fixture._HERE / "rust-logic-coverage.sh").read_text(encoding="utf-8")
        regex = re.search(r"(?m)^IGNORE_REGEX='(.*)'$", script).group(1)
        prefix = "(sonicterm-("
        crates = regex[regex.index(prefix) + len(prefix):].split(")", 1)[0].split("|")
        self.assertNotIn("vt", crates)
        self.assertNotIn("block-glyph", crates)
        # CLAUDE.md and both CI-and-Coverage pages state this count.
        self.assertEqual(len(crates), 9)
        self.assertEqual(script.count("--fail-under-lines 80"), 1)
        # The report and inventory are published before the 80% subset gate or the floor can fail the run.
        markers = ["pin-checkout", "record self-test running", "coverage-floor_tests.py", "begin instrumented-tests",
                   "cargo llvm-cov --workspace --lib --bins --tests --no-report", "begin report",
                   "cargo llvm-cov report --json --summary-only", "begin inventory", "cargo metadata --no-deps",
                   "begin publish", "os.rename", "begin subset-gate", "--fail-under-lines 80", "begin floor",
                   '--baseline "$ROOT/scripts/coverage-baseline.json"', 'record floor "done"']
        positions = [script.index(marker) for marker in markers]
        self.assertEqual(positions, sorted(positions))


class CliTests(unittest.TestCase):
    """The script's exit status holds when run as a real process."""

    def test_exit_status_from_a_real_process(self):
        # Exit 1 fails CI, 0 is informational, 2 is misuse; the gate step relies on these codes.
        env = coverage_test_fixture.clean_env()
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 100)}
        with coverage_test_fixture.workspace(coverage_test_fixture.export(files), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)) as paths:
            argv = [sys.executable, str(coverage_test_fixture._FLOOR_PATH), "--report", str(paths["report"]),
                    "--metadata", str(paths["metadata"]), "--baseline", str(paths["baseline"]), "--target", coverage_test_fixture.TARGET]

            def run(arguments, environment):
                return subprocess.run(arguments, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      text=True, encoding="utf-8", check=False, timeout=60)

            failing = run(argv, {**env, **coverage_test_fixture.CI_ENV})
            local = run(argv, env)
            incomplete = run(argv[:4], env)
        self.assertEqual(failing.returncode, 1, failing.stdout + failing.stderr)
        self.assertEqual(local.returncode, 0, local.stdout + local.stderr)
        self.assertEqual(incomplete.returncode, 2)
        self.assertIn("required", incomplete.stderr)

    def test_overflowing_baseline_number_exits_2_without_a_traceback(self):
        # 1e309 overflows to inf; the reader must refuse it with exit 2, not raise an uncaught ValueError.
        document = json.dumps(coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), indent=2).replace(
            '"tolerance_pp": 1.0', '"tolerance_pp": 1e309')
        with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), document) as paths:
            completed = subprocess.run(
                [sys.executable, str(coverage_test_fixture._FLOOR_PATH), "--report", str(paths["report"]), "--metadata",
                 str(paths["metadata"]), "--baseline", str(paths["baseline"]), "--target", coverage_test_fixture.TARGET],
                env={**coverage_test_fixture.clean_env(), **coverage_test_fixture.CI_ENV}, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True, encoding="utf-8", check=False, timeout=60)
        self.assertEqual(completed.returncode, 2, completed.stdout + completed.stderr)
        self.assertNotIn("Traceback", completed.stderr)
        self.assertIn("non-finite number 1e309 is not allowed", completed.stderr)
