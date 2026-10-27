#!/usr/bin/env python3
"""Contract tests for physical line counting, tracked discovery and exact exclusion boundaries."""

from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import source_size


class SourceSizeTests(unittest.TestCase):
    def test_physical_line_endings_and_unicode(self):
        # Only LF boundaries count; a final unterminated line counts even without decodable text.
        for content, expected in [(b"", 0), (b"\n", 1), (b"last", 1), (b"a\r\nb\r\n", 2),
                                  ("中\n文".encode(), 2), (b"\xff\n", 1), (b"a\rb", 1)]:
            with self.subTest(content=content):
                self.assertEqual(source_size.physical_lines(content), expected)

    def test_limit_counts_comments_blanks_and_unterminated_lines(self):
        # Nothing in source syntax is exempt from the physical size ceiling.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for count, suffix in [(1000, b"\n"), (1001, b"\n"), (1000, b""), (1001, b"")]:
                (root / "source.py").write_bytes(b"# comment\n" * (count - 1) + b"# last" + suffix)
                measured, problems = source_size.inspect_sources(root, ["source.py"], (), {})
                self.assertEqual(measured, [("source.py", count)])
                self.assertEqual(bool(problems), count > 1000)

    def test_discovery_reads_working_tree_and_preserves_spaced_names(self):
        # An ignored directory cannot hide tracked sources; staged content is not the measured content.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "space name.rs").write_bytes(b"old\n")
            (root / "untracked.py").write_bytes(b"skip\n")
            subprocess.run(["git", "-C", str(root), "add", "--", "space name.rs"], check=True)
            (root / "space name.rs").write_bytes(b"new\n" * 1001)
            paths = source_size.tracked_paths(root)
            self.assertEqual(paths, ["space name.rs"])
            measured, problems = source_size.inspect_sources(root, paths, (), {})
            self.assertEqual(measured, [("space name.rs", 1001)])
            self.assertTrue(problems)

    def test_unreadable_source_fails_closed(self):
        # A permission failure is asserted directly so privileged hosts cannot bypass the fixture.
        with mock.patch.object(Path, "read_bytes", side_effect=PermissionError("denied")):
            measured, problems = source_size.inspect_sources(Path("."), ["missing.rs"], (), {})
        self.assertEqual(measured, [])
        self.assertTrue(any("cannot read missing.rs" in problem for problem in problems))

    def test_native_roots_are_exact_not_crate_wide(self):
        # Near-prefix first-party files and attributed customglyph code remain measured.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = ["crates/native/upstream/code.c", "crates/native/upstream_extra/code.c",
                     "crates/native/src/lib.rs", "crates/sonicterm-block-glyph/src/customglyph.rs"]
            for path in paths:
                target = root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b"line\n" * 1001)
            measured, problems = source_size.inspect_sources(root, paths, ("crates/native/upstream/",), {})
            self.assertEqual({path for path, _count in measured}, set(paths[1:]))
            self.assertEqual(len(problems), 3)

    def test_exact_exceptions_and_stale_entries(self):
        # An ABI exception exempts only its exact file and must still exist in the index and tree.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "abi.rs").write_bytes(b"line\n" * 1001)
            (root / "abi_extra.rs").write_bytes(b"line\n" * 1001)
            measured, problems = source_size.inspect_sources(root, ["abi.rs", "abi_extra.rs"], (), {"abi.rs": "ABI"})
            self.assertEqual(measured, [("abi_extra.rs", 1001)])
            self.assertEqual(len(problems), 1)
            _measured, problems = source_size.inspect_sources(root, ["abi_extra.rs"], ("missing/",), {"gone.rs": "ABI"})
            self.assertTrue(any("stale exact exception" in problem for problem in problems))
            self.assertTrue(any("stale native root" in problem for problem in problems))

    def test_exact_exceptions_are_only_generated_bindings(self):
        # Hand-written first-party code is always measured; only the regenerated FreeType and HarfBuzz bindings are exempt.
        self.assertEqual(
            set(source_size.EXACT_EXCEPTIONS),
            {
                "crates/sonicterm-freetype/src/lib.rs",
                "crates/sonicterm-freetype/src/types.rs",
                "crates/sonicterm-harfbuzz/src/lib.rs",
            },
        )

    def test_manifest_supplies_only_reviewed_native_roots(self):
        # Native roots come from the pin manifest, and cannot broaden into an entire owned crate.
        root = Path(__file__).resolve().parent.parent
        import json
        manifest = json.loads((root / "scripts/native-dependencies.json").read_text(encoding="utf-8"))
        self.assertEqual(set(source_size.native_roots(manifest)), {item["path"] + "/" for item in manifest["libraries"]})
        for bad in ["crates/sonicterm-font", "../outside", "crates/native/../src", "crates/native//source"]:
            changed = {"libraries": [dict(item) for item in manifest["libraries"]]}
            changed["libraries"][0]["path"] = bad
            with self.assertRaises(ValueError):
                source_size.native_roots(changed)
        with self.assertRaises(ValueError):
            source_size.native_roots({"libraries": []})


    def test_gate_is_required_in_tooling_and_has_one_canonical_identity(self):
        # Every script caller shares the exact catalog objects that authorize process preparation.
        import gate_catalog
        import gate_model
        import local_gate
        import perf_settings
        self.assertIs(local_gate.Step, gate_model.Step)
        self.assertIs(local_gate.STEPS, gate_catalog.STEPS)
        self.assertIs(perf_settings.load_gate(), local_gate)
        step = next(step for step in local_gate.STEPS if step.id == "source-size")
        self.assertEqual(step.hosts, gate_model.HOSTS)
        self.assertEqual(step.ci_jobs, ("tooling",))
        self.assertEqual(step.argv, ("python3", "scripts/check-source-size.py"))

    def test_coverage_subset_keeps_moved_exclusions_and_measured_neighbors(self):
        # Moving an excluded implementation must not silently add it to the deterministic coverage gate.
        import re
        root = Path(__file__).resolve().parent.parent
        source = (root / "scripts/rust-logic-coverage.sh").read_text(encoding="utf-8")
        expression = re.search(r"(?m)^IGNORE_REGEX='(.+)'$", source).group(1)
        for path in ["sonicterm-cfg/src/config_persist.rs", "sonicterm-cfg/src/config_status_save.rs",
                     "sonicterm-cfg/src/config_template.rs", "sonicterm-font/src/font_resolution.rs",
                     "sonicterm-font/src/loaded_font.rs", "sonicterm-font/src/ftwrap_names.rs",
                     "sonicterm-font/src/ftwrap_library.rs", "sonicterm-font/src/ftwrap_metrics.rs",
                     "sonicterm-font/src/hbwrap_paint.rs", "sonicterm-text/src/glyph_atlas_growth.rs",
                     "sonicterm-text/src/glyph_atlas_packing.rs", "sonicterm-ui/src/command_palette_modes.rs",
                     "sonicterm-ui/src/command_palette_actions.rs", "sonicterm-ui/src/command_palette_presentation.rs",
                     "sonicterm-ui/src/search_matcher.rs", "sonicterm-ui/src/selection_words.rs"]:
            self.assertRegex("crates/" + path, expression)
        for path in ["sonicterm-cfg/src/url_scan_paths.rs", "sonicterm-ui/src/status_line_format.rs",
                     "sonicterm-text/src/start_size_inputs_recorded.rs", "sonicterm-types/src/resource_delivery.rs"]:
            self.assertNotRegex("crates/" + path, expression)


if __name__ == "__main__":
    unittest.main(verbosity=2)
