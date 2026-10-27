"""Perf hook tests."""

from __future__ import annotations
import copy
import json
from pathlib import Path
import tempfile
import unittest

import perf_atlas_tests
import perf_build_tests
import perf_fallback_tests
import perf_harness_sources
import perf_home_tests
import perf_prebuilt_tests
import perf_settings


class HookSourceScanTests(unittest.TestCase):
    """Hook detection matches only code: comments and string literals of every kind are blanked."""

    METHOD = "pub fn __perf_checkpoint_memory(&mut self) {}"

    def detects(self, source):
        return bool(perf_harness_sources.HOOK_METHODS[perf_harness_sources.CHECKPOINT_MEMORY_FEATURE].search(perf_harness_sources.rust_code_only(source)))

    def test_the_method_only_in_comments_or_literals_is_not_detected(self):
        cases = {
            "block comment": f"/*\n{self.METHOD}\n*/\nfn other() {{}}\n",
            "nested block comment": f"/* outer /* inner */\n{self.METHOD}\n*/\n",
            "multi-line string": f'const TEXT: &str = "first line\n{self.METHOD}\n";\n',
            "raw string": f'const TEXT: &str = r#"\n{self.METHOD}\n"#;\n',
            "byte string": f'const TEXT: &[u8] = b"\n{self.METHOD}\n";\n',
            "doc line": f"/// {self.METHOD}\nfn other() {{}}\n",
            "line comment": f"// {self.METHOD}\nfn other() {{}}\n",
        }
        for name, source in cases.items():
            with self.subTest(name=name):
                self.assertFalse(self.detects(source), source)

    def test_the_real_method_is_detected(self):
        source = f"impl App {{\n    #[doc(hidden)]\n    {self.METHOD}\n}}\n"
        self.assertTrue(self.detects(source))
        # Text after a closed comment or string on an earlier line is code again.
        self.assertTrue(self.detects(f'/* note */\nconst TEXT: &str = "\\"done";\n{self.METHOD}\n'))

    def test_the_blanker_keeps_code_lifetimes_and_line_breaks(self):
        # Every position is kept, a quote inside a character literal does not open a string, and a
        # lifetime is code.
        source = 'let quote = \'"\'; fn keep<\'a>(text: &\'a str) {} // tail\nlet done = 1;'
        blanked = perf_harness_sources.rust_code_only(source)
        self.assertEqual(len(blanked), len(source))
        self.assertEqual(blanked.count("\n"), source.count("\n"))
        self.assertIn("fn keep<'a>(text: &'a str) {}", blanked)
        self.assertIn("let done = 1;", blanked)
        self.assertNotIn("tail", blanked)


class CheckpointMemoryFeatureTests(perf_prebuilt_tests.PrebuiltHarness, unittest.TestCase):
    """perf-hook-checkpoint-memory reaches only a tree that defines the hook, through the gate's reviewed steps."""

    def build_step(self, gate, side):
        return next(step for step in gate.steps if step.id == f"build-{side}-perf_scenarios")

    def test_a_tree_needs_the_declaration_and_the_method(self):
        # The feature alone, or the method only in a comment, does not make a tree hook-capable.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / perf_settings.APP_MANIFEST).parent.mkdir(parents=True)
            (root / perf_settings.APP_MANIFEST).write_text(perf_fallback_tests.HOOK_MANIFEST, encoding="utf-8")
            hook = root / perf_harness_sources.APP_SOURCE_DIRECTORY / "app" / "memory_snapshot.rs"
            hook.parent.mkdir(parents=True)
            method = perf_harness_sources.HOOK_METHODS[perf_harness_sources.CHECKPOINT_MEMORY_FEATURE]
            self.assertFalse(perf_harness_sources.tree_supports_hook(root, perf_harness_sources.CHECKPOINT_MEMORY_FEATURE, method))
            hook.write_text("// pub fn __perf_checkpoint_memory is coming\n", encoding="utf-8")
            self.assertFalse(perf_harness_sources.tree_supports_hook(root, perf_harness_sources.CHECKPOINT_MEMORY_FEATURE, method))
            hook.write_text(perf_fallback_tests.HOOK_SOURCE, encoding="utf-8")
            self.assertTrue(perf_harness_sources.tree_supports_hook(root, perf_harness_sources.CHECKPOINT_MEMORY_FEATURE, method))
            (root / perf_settings.APP_MANIFEST).write_text(perf_home_tests.HEAD_MANIFEST, encoding="utf-8")
            self.assertFalse(perf_harness_sources.tree_supports_hook(root, perf_harness_sources.CHECKPOINT_MEMORY_FEATURE, method))

    def test_the_head_builds_with_the_hook_and_the_base_without_it(self):
        # Each side's build step is the gate's own catalog entry for exactly the features it supports.
        # Each side's run is the golden run its build writes, so the base's `unsupported` comes from
        # the no-feature serializer and the head's `supported` from the hooked one.
        code, gate, _calls, _plans, _work, out = self.compare(
            head_manifest=perf_atlas_tests.COUNTERS_HOOK_MANIFEST, hook_trees=("head",),
            base_run=lambda plan: perf_atlas_tests.fixture_outcome("unsupported", "unsupported", plan),
            head_run=lambda plan: perf_atlas_tests.fixture_outcome("supported", "deadline-610", plan))
        self.assertEqual(code, perf_settings.EXIT_PASS)
        # Both fixture trees define S1/atlas-retry's methods, so each side builds its features with that cfg on.
        catalog = {features: steps for (cfgs, features), steps in perf_build_tests.REAL_GATE.PERF_CFG_BUILDS.items()
                   if cfgs == ("perf_atlas_retry_api",)}
        hooked = ("perf-counters", perf_harness_sources.CHECKPOINT_MEMORY_FEATURE)
        self.assertIs(self.build_step(gate, "head"), catalog[hooked]["build-head-perf_scenarios"])
        self.assertIs(self.build_step(gate, "base"), catalog[()]["build-base-perf_scenarios"])
        self.assertEqual(perf_build_tests.REAL_GATE.PERF_BUILD_CATALOG[("build-head-perf_scenarios", hooked)].argv,
                         self.build_step(gate, "head").argv)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("- Built with `--features perf-hook-checkpoint-memory`: head", document)
        row = next(line for line in document.splitlines() if "end renderer_total_bytes (MiB)" in line)
        cells = [cell.strip() for cell in row.strip("|").split("|")]
        self.assertEqual(cells[2], "n/a: unsupported", row)
        self.assertTrue(cells[3].endswith(", 1 partial"), row)

    def test_a_declaration_without_the_method_builds_without_the_feature(self):
        _code, gate, *_rest = self.compare(head_manifest=perf_fallback_tests.HOOK_MANIFEST)
        self.assertNotIn("--features", self.build_step(gate, "head").argv)

    def test_the_manifest_records_each_sides_features_and_a_mismatch_fails(self):
        binaries, digest = self.produce(head_manifest=perf_fallback_tests.HOOK_MANIFEST, hook_trees=("head",))
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["features"], {"base": [], "head": [perf_harness_sources.CHECKPOINT_MEMORY_FEATURE]})
        with self.assertRaisesRegex(ValueError, "refusing the prebuilt binaries: features"):
            self.consume(binaries, digest, head_manifest=perf_fallback_tests.HOOK_MANIFEST)
        code, *_rest = self.consume(binaries, digest, head_manifest=perf_fallback_tests.HOOK_MANIFEST, hook_trees=("head",))
        self.assertEqual(code, perf_settings.EXIT_PASS)

    def test_the_gate_rejects_a_lookalike_build_step(self):
        # A copy of a reviewed step with the same id and argv is not the gate's own step.
        hooked = ("perf-counters", perf_harness_sources.CHECKPOINT_MEMORY_FEATURE)
        step = perf_build_tests.REAL_GATE.PERF_FEATURE_BUILDS[hooked]["build-head-perf_scenarios"]
        self.assertTrue(perf_build_tests.REAL_GATE._reviewed_step(step))
        self.assertFalse(perf_build_tests.REAL_GATE._reviewed_step(copy.copy(step)))

    def test_both_scripts_list_the_same_perf_features(self):
        self.assertEqual(perf_harness_sources.PERF_FEATURES, perf_build_tests.REAL_GATE.PERF_FEATURES)



CELL_MIB = 1048576
