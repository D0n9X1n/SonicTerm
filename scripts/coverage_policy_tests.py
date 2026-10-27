"""Coverage policy tests."""

from __future__ import annotations
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

import coverage_test_fixture


class CoverageWorkflowTests(unittest.TestCase):
    """The linux-coverage job preserves evidence upload policy without custom timeout overrides."""

    def test_coverage_job_preserves_upload_contract_without_timeout_overrides(self):
        # Upload is eligible after the gate starts, but a hung gate may exhaust the platform job limit.
        job = coverage_test_fixture.ci_job("linux-coverage")
        gate = coverage_test_fixture.ci_step(job, "Run Rust logic coverage gate")
        upload = coverage_test_fixture.ci_step(job, "Upload coverage evidence")
        self.assertIn("id: coverage", gate)
        self.assertIn("run: scripts/rust-logic-coverage.sh", gate)
        # Run metadata reaches the script through env, never as an expression in the command.
        self.assertIn("COVERAGE_PULL_REQUEST_HEAD: ${{ github.event.pull_request.head.sha }}", gate)
        self.assertNotIn("${{", re.search(r"(?m)^        run: (.*)$", gate).group(1))
        self.assertIn("if: ${{ !cancelled() && steps.coverage.conclusion != 'skipped' }}", upload)
        self.assertIn("uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1", upload)
        self.assertIn(f"name: {coverage_test_fixture.floor.EVIDENCE_ARTIFACT_PREFIX}-${{{{ github.run_id }}}}-${{{{ github.run_attempt }}}}",
                      upload)
        self.assertIn(f"path: {coverage_test_fixture.floor.EVIDENCE_DIRECTORY}\n", upload)
        self.assertIn("retention-days: 90", upload)
        self.assertIn("if-no-files-found: error", upload)
        self.assertLess(job.index("- name: Run Rust logic coverage gate"), job.index("- name: Upload coverage evidence"))

        self.assertNotRegex(job, r"(?m)^\s*(?:-\s*)?timeout-minutes\s*:")

    def test_macos_core_holds_macos_only_logic_to_its_floor_and_keeps_the_evidence(self):
        # The macOS-only floor runs in macOS core gates, records its report's provenance, and uploads both after
        # success and failure under the artifact name the verifier expects.
        job = coverage_test_fixture.ci_job("macos-core")
        gate = coverage_test_fixture.ci_step(job, "Run macOS-only logic coverage")
        upload = coverage_test_fixture.ci_step(job, "Upload macOS-only coverage evidence")
        self.assertIn("id: macos_coverage", gate)
        self.assertIn("run: scripts/macos-logic-coverage.sh", gate)
        self.assertIn("COVERAGE_PULL_REQUEST_HEAD: ${{ github.event.pull_request.head.sha }}", gate)
        self.assertIn("if: ${{ !cancelled() && steps.macos_coverage.conclusion != 'skipped' }}", upload)
        self.assertIn(f"name: {coverage_test_fixture.floor.MACOS_ARTIFACT_PREFIX}-${{{{ github.run_id }}}}-${{{{ github.run_attempt }}}}",
                      upload)
        self.assertIn("path: target/macos-logic-coverage-evidence\n", upload)
        self.assertNotRegex(job, r"(?m)^\s*(?:-\s*)?timeout-minutes\s*:")
        script = (coverage_test_fixture._HERE / "macos-logic-coverage.sh").read_text(encoding="utf-8")
        for command in ("pin-checkout", "record-macos-provenance", "macos-floor"):
            self.assertIn(command, script)

    def test_gate_script_writes_the_evidence_the_workflow_uploads(self):
        # The script's evidence directory and file names are the ones the upload and the floor expect.
        script = (coverage_test_fixture._HERE / "rust-logic-coverage.sh").read_text(encoding="utf-8")
        self.assertEqual(coverage_test_fixture.floor.EVIDENCE_DIRECTORY, "target/rust-logic-coverage-evidence")
        self.assertEqual(coverage_test_fixture.floor.WORK_DIRECTORY, "target/rust-logic-coverage-work")
        for line in ('EVIDENCE_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage-evidence"',
                     'WORK_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage-work"',
                     f'STAGE_DIR="${{WORK_DIR}}/{coverage_test_fixture.floor.STAGING_DIRECTORY}"',
                     f'MEASUREMENT_DIR="${{EVIDENCE_DIR}}/{coverage_test_fixture.floor.MEASUREMENT_DIRECTORY}"',
                     f'REPORT="${{MEASUREMENT_DIR}}/{coverage_test_fixture.floor.REPORT_FILE}"',
                     f'INVENTORY="${{MEASUREMENT_DIR}}/{coverage_test_fixture.floor.INVENTORY_FILE}"',
                     f'RECORD="${{EVIDENCE_DIR}}/{coverage_test_fixture.floor.PROVENANCE_FILE}"', f'PIN="${{WORK_DIR}}/{coverage_test_fixture.floor.PIN_FILE}"'):
            self.assertIn(line, script)
        # The fallback record the exit trap writes uses the schema the checker reads.
        self.assertIn(coverage_test_fixture.floor.PROVENANCE_SCHEMA, script)


class GitTreeIdTests(unittest.TestCase):
    """The offline checker rebuilds the Git tree a record's path map names, exactly as Git hashes it."""

    def test_head_tree_is_recomputed_from_ls_tree(self):
        # Executables, symlinks, gitlinks, nested directories, names whose order depends on Git's directory
        # rule, and non-UTF-8 names all rehash to HEAD's tree, in SHA-1 and, where Git supports it, SHA-256.
        for form in ("sha1", "sha256"):
            with self.subTest(object_format=form):
                directory = Path(tempfile.mkdtemp())
                self.addCleanup(shutil.rmtree, str(directory), True)
                made = subprocess.run(["git", "init", "-q", f"--object-format={form}", str(directory)],
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=60)
                if made.returncode != 0:
                    self.assertEqual(form, "sha256", made.stderr)
                    self.skipTest("this Git cannot create a SHA-256 repository")

                def blob(content):
                    return subprocess.run(["git", "-C", str(directory), "hash-object", "-w", "--stdin"],
                                          input=content, stdout=subprocess.PIPE, check=True,
                                          timeout=60).stdout.decode().strip()

                gitlink = "1" * coverage_test_fixture.floor.OBJECT_ID_LENGTHS[form]
                entries = [("100644", blob(b"plain\n"), b"a.txt"), ("100755", blob(b"#!/bin/sh\n"), b"bin/run.sh"),
                           ("120000", blob(b"a.txt"), b"link"), ("160000", gitlink, b"vendor/sub"),
                           ("100644", blob(b"x"), b"a-b"), ("100644", blob(b"y"), b"a/inner.rs"),
                           ("100644", blob(b"z"), b"a0"), ("100644", blob(b"deep"), b"a/b/c/deep.rs"),
                           ("100644", blob(b"u"), "café.md".encode()), ("100644", blob(b"r"), b"raw\xff.bin")]
                for mode, object_id, name in entries:
                    subprocess.run([b"git", b"-C", str(directory).encode(), b"update-index", b"--add", b"--cacheinfo",
                                    f"{mode},{object_id},".encode() + name], check=True, timeout=60)
                coverage_test_fixture.git(directory, "commit", "-q", "-m", "tree fixture")
                head_tree = coverage_test_fixture.git(directory, "rev-parse", "HEAD^{tree}").stdout.strip()
                listed = coverage_test_fixture.floor.tree_entries(str(directory))
                self.assertEqual(len(listed), len(entries))
                self.assertEqual(coverage_test_fixture.floor.git_tree_id(listed, form), head_tree)

    def test_a_path_map_git_could_not_write_is_refused(self):
        # An unknown mode, a short object id, or a file with a path beneath it cannot hash to any Git tree.
        good = "100644 " + "a" * 40
        for label, entries in {"unknown mode": {"a.txt": "100664 " + "a" * 40},
                               "short object id": {"a.txt": "100644 abc"},
                               "path beneath a file": {"a": good, "a/b": good}}.items():
            with self.subTest(case=label):
                with self.assertRaises(coverage_test_fixture.floor.InputError):
                    coverage_test_fixture.floor.git_tree_id(entries, "sha1")


class ClassificationTests(unittest.TestCase):
    """Report files are attributed consistently and every exclusion is counted."""

    def test_paths_are_normalized_before_classification(self):
        # llvm-cov reports `src/../build_config.rs` for a `#[path]` module.
        files = {
            "crates/sonicterm-freetype/src/../build_config.rs": (58, 56),
            "crates/sonicterm-freetype/build.rs": (40, 0),
            f"{coverage_test_fixture.ROOT}/crates\\sonicterm-vt\\src\\vt.rs": (200, 174),
        }
        base = coverage_test_fixture.baseline({"sonicterm-freetype": (58, 56), "sonicterm-vt": (200, 174)})
        run = coverage_test_fixture.check(files, ["sonicterm-freetype", "sonicterm-vt"], base, coverage_test_fixture.CI_ENV, build_scripts=("sonicterm-freetype",))
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(coverage_test_fixture.row(run.stdout, "sonicterm-freetype"), r"\s58\s+56\s")
        self.assertRegex(run.stdout, r"build-script\s+files=1\s+lines=40\s")

    def test_exclusions_are_reported_with_counts_and_reasons(self):
        files = {
            **coverage_test_fixture.FILES,
            "crates/sonicterm-vt/src/vt_tests.rs": (500, 500),
            "crates/sonicterm-vt/tests/autowrap/main.rs": (40, 40),
            "crates/sonicterm-vt/benches/parse.rs": (7, 7),
            "crates/sonicterm-vt/examples/demo.rs": (5, 5),
            "crates/sonicterm-winit/src/platform_impl/macos/mod.rs": (100, 50),
            "crates/sonicterm-harfbuzz/harfbuzz/src/rust/lib.rs": (20, 0),
            "target/rust-logic-coverage/llvm-cov-target/debug/build/x-1/out/bindings.rs": (9, 9),
            "/Users/runner/.cargo/registry/src/index/foo-1.0.0/src/lib.rs": (11, 1),
            "crates/sonicterm-vt/build.rs": (3, 0),
            "scripts/freetype-config.rs": (4, 4),
        }
        base = coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, {"sonicterm-harfbuzz": "declarations only"})
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS + ["sonicterm-harfbuzz"], base, coverage_test_fixture.CI_ENV, build_scripts=("sonicterm-vt",))
        self.assertEqual(run.status, 0, run.stdout)
        # Only vt's two src files count toward its row.
        self.assertRegex(coverage_test_fixture.row(run.stdout, "sonicterm-vt"), r"\s200\s+174\s+87\.00%")
        expected = {"test": (4, 552), "vendored": (2, 120), "generated": (1, 9),
                    "outside-repository": (1, 11), "build-script": (1, 3), "outside-members": (1, 4)}
        for category, (count, lines) in expected.items():
            with self.subTest(category=category):
                self.assertRegex(run.stdout, rf"(?m)^  {re.escape(category)}\s+files={count}\s+lines={lines}\s+\S")
        self.assertIn("      scripts/freetype-config.rs", run.stdout)
        self.assertIn("      crates/sonicterm-vt/build.rs", run.stdout)

    def test_committed_generated_bindings_count_under_generated(self):
        # Tracked rust-bindgen output is excluded like build output; FreeType keeps only authored code.
        files = {
            "crates/sonicterm-freetype/src/lib.rs": (22, 0),
            "crates/sonicterm-freetype/src/types.rs": (5, 0),
            "crates/sonicterm-freetype/src/fixed_point.rs": (18, 18),
            "crates/sonicterm-freetype/src/../build_config.rs": (58, 56),
            "crates/sonicterm-harfbuzz/src/lib.rs": (9, 0),
        }
        members = ["sonicterm-freetype", "sonicterm-harfbuzz"]
        base = coverage_test_fixture.baseline({"sonicterm-freetype": (76, 74)}, {"sonicterm-harfbuzz": "generated only"})
        run = coverage_test_fixture.check(files, members, base, coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(coverage_test_fixture.row(run.stdout, "sonicterm-freetype"), r"\s76\s+74\s+97\.37%")
        self.assertRegex(run.stdout, r"(?m)^  generated\s+files=3\s+lines=36\s")
        for path in ("crates/sonicterm-freetype/src/lib.rs", "crates/sonicterm-freetype/src/types.rs",
                     "crates/sonicterm-harfbuzz/src/lib.rs"):
            self.assertIn("      " + path, run.stdout)
        self.assertNotIn("%", coverage_test_fixture.row(run.stdout, "sonicterm-harfbuzz"))
        undeclared = coverage_test_fixture.check(files, members, coverage_test_fixture.baseline({"sonicterm-freetype": (76, 74)}), coverage_test_fixture.CI_ENV)
        self.assertIn("every reported file is excluded (generated: 1)",
                      coverage_test_fixture.row(undeclared.stdout, "sonicterm-harfbuzz"))

    def test_change_confined_to_generated_bindings_does_not_move_the_crate(self):
        # Counting these generated lines would read two added uncovered lines as a 1.51 pp drop (74/100 against 74/98).
        authored = {"crates/sonicterm-freetype/src/fixed_point.rs": (18, 18),
                    "crates/sonicterm-freetype/src/../build_config.rs": (58, 56)}
        base = coverage_test_fixture.baseline({"sonicterm-freetype": (76, 74)})
        for generated_lines in (22, 24):
            with self.subTest(generated_lines=generated_lines):
                files = {**authored, "crates/sonicterm-freetype/src/lib.rs": (generated_lines, 0)}
                run = coverage_test_fixture.check(files, ["sonicterm-freetype"], base, coverage_test_fixture.CI_ENV)
                self.assertEqual(run.status, 0, run.stdout)
                self.assertRegex(coverage_test_fixture.row(run.stdout, "sonicterm-freetype"),
                                 r"\s76\s+74\s+97\.37%\s+97\.37%\s+\+0\.00")
                self.assertNotIn("DROP", run.stdout)
        # Counted as authored code, the same probe would cross the tolerance: 74/98 against 74/100.
        self.assertGreater(74 * 100 / 98 - 74 * 100 / 100, 1.0)

    def test_source_root_maps_a_report_from_another_checkout(self):
        other = "/private/tmp/sonicterm-fixture-other-checkout"
        with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES, root=other), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)) as paths:
            unmapped = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
            mapped = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV, "--source-root", other)
        # Unmapped, every file is outside the repository and both floors go missing.
        self.assertEqual(unmapped.status, 1, unmapped.stdout)
        self.assertRegex(unmapped.stdout, r"outside-repository\s+files=3\s")
        self.assertIn("MISSING", coverage_test_fixture.row(unmapped.stdout, "sonicterm-vt"))
        self.assertEqual(mapped.status, 0, mapped.stdout)

    def test_platform_crates_are_labelled_as_compiled_on_the_host(self):
        # Single-platform crates measured on another OS are labelled, never shown as native coverage.
        files = {"crates/sonicterm-mac/src/main.rs": (100, 10), "crates/sonicterm-windows/src/main.rs": (100, 20),
                 "crates/sonicterm-linux/src/main.rs": (70, 66)}
        members = ["sonicterm-linux", "sonicterm-mac", "sonicterm-windows"]
        base = coverage_test_fixture.baseline({"sonicterm-mac": (100, 10), "sonicterm-windows": (100, 20), "sonicterm-linux": (70, 66)})
        on_mac = coverage_test_fixture.check(files, members, base, coverage_test_fixture.CI_ENV)
        self.assertEqual(on_mac.status, 0, on_mac.stdout)
        self.assertIn("compiled on macOS; not Windows execution coverage", coverage_test_fixture.row(on_mac.stdout, "sonicterm-windows"))
        self.assertIn("compiled on macOS; not Linux execution coverage", coverage_test_fixture.row(on_mac.stdout, "sonicterm-linux"))
        self.assertNotIn("compiled on", coverage_test_fixture.row(on_mac.stdout, "sonicterm-mac"))
        on_linux = coverage_test_fixture.check(files, members, base, coverage_test_fixture.LOCAL_ENV, target="x86_64-unknown-linux-gnu")
        self.assertIn("compiled on Linux; not macOS execution coverage", coverage_test_fixture.row(on_linux.stdout, "sonicterm-mac"))
        self.assertNotIn("compiled on", coverage_test_fixture.row(on_linux.stdout, "sonicterm-linux"))

    def test_one_file_reported_twice_is_refused(self):
        # Two spellings of one file would double its lines.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt/../vt.rs": (190, 165)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("lists crates/sonicterm-vt/src/vt.rs twice", run.stderr)


class InputValidationTests(unittest.TestCase):
    """Broken inputs are errors, never an empty or passing comparison."""

    def test_empty_report_is_refused(self):
        # An empty export is a broken run; comparing it would mark every crate missing.
        with coverage_test_fixture.workspace(coverage_test_fixture.export({}), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)) as paths:
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("lists no files", run.stderr)

    def test_document_that_is_not_an_llvm_export_is_refused(self):
        # Only an llvm-cov JSON export is accepted as a report.
        with coverage_test_fixture.workspace(dict(coverage_test_fixture.export(coverage_test_fixture.FILES), type="something.else"), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)) as paths:
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("is not an llvm.coverage.json.export document", run.stderr)

    def test_malformed_baselines_are_refused(self):
        # Each schema rule rejects its own violation, so a hand edit cannot weaken the floor.
        good = coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, {"sonicterm-fontconfig": "linked only on non-macOS Unix"})
        mutations = {
            "schema": lambda doc: doc.update(schema="other/1"),
            "percent": lambda doc: doc["crates"]["sonicterm-vt"].update(percent=50.0),
            "overlap": lambda doc: doc["not_measured"].update({"sonicterm-vt": "both"}),
            "empty declaration": lambda doc: doc["not_measured"].update({"sonicterm-fontconfig": " "}),
            "unknown key": lambda doc: doc.update(floor_pp=1.0),
            "zero tolerance": lambda doc: doc.update(tolerance_pp=0),
            "empty reason": lambda doc: doc.update(reason=""),
            "empty runner": lambda doc: doc["host"].update(runner=""),
            "zero lines": lambda doc: doc["crates"]["sonicterm-vt"].update(lines=0),
        }
        for label, mutate in mutations.items():
            with self.subTest(mutation=label):
                document = json.loads(json.dumps(good))
                mutate(document)
                run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS + ["sonicterm-fontconfig"], document, coverage_test_fixture.CI_ENV)
                self.assertEqual(run.status, 2, run.stdout)
                self.assertIn("is invalid", run.stderr)

    def test_non_finite_baseline_numbers_are_refused(self):
        # NaN compares false and 1e309 overflows to inf; each must exit 2 instead of passing or crashing.
        text = json.dumps(coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), indent=2)
        self.assertEqual(text.count('"tolerance_pp": 1.0'), 1)
        self.assertEqual(text.count('"percent": 87.0'), 1)
        cases = {
            "tolerance NaN": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": NaN'),
            "tolerance Infinity": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": Infinity'),
            "tolerance -Infinity": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": -Infinity'),
            "tolerance 1e309": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": 1e309'),
            "tolerance 1 followed by 400 zeros": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": 1' + "0" * 400),
            "percent NaN": text.replace('"percent": 87.0', '"percent": NaN'),
            "percent 1e309": text.replace('"percent": 87.0', '"percent": 1e309'),
        }
        for label, document in cases.items():
            with self.subTest(case=label):
                with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), document) as paths:
                    run = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
                self.assertEqual(run.status, 2, run.stdout)
                self.assertIn("finite", run.stderr)
                self.assertNotIn("PASS", run.stdout)
        # Documents that bypass the JSON reader, such as proposals, are held to the same rule.
        mutations = {
            "tolerance nan": lambda doc: doc.update(tolerance_pp=float("nan")),
            "tolerance inf": lambda doc: doc.update(tolerance_pp=float("inf")),
            "percent nan": lambda doc: doc["crates"]["sonicterm-vt"].update(percent=float("nan")),
        }
        for label, mutate in mutations.items():
            with self.subTest(case=label):
                document = coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)
                mutate(document)
                with self.assertRaisesRegex(coverage_test_fixture.floor.InputError, "finite"):
                    coverage_test_fixture.floor.parse_baseline(document, "in-memory")

    def test_missing_input_file_is_an_input_error(self):
        # A missing input exits 2 instead of comparing against nothing.
        with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), None) as paths:
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("cannot read baseline", run.stderr)
