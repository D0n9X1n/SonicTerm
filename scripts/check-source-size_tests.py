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


class RustTestOwnershipTests(unittest.TestCase):
    def problems(self, files):
        """Check real Rust declarations without depending on this host's active cfgs."""
        from rust_test_ownership import ownership_problems
        return ownership_problems(files)

    def test_exact_and_topic_suites_preserve_module_aliases(self):
        # Filenames express physical ownership; module aliases may remain stable for subprocess filters.
        files = {
            "src/foo.rs": '#[cfg(test)] #[path = "foo_tests.rs"] mod old_name;\n'
                          '#[cfg(all(test, windows))] #[path = "foo_native_tests.rs"] pub(crate) mod native;\n',
            "src/foo_tests.rs": '#[cfg(test)] #[path = "foo_tests_fixture_tests.rs"] mod fixture;\n',
            "src/foo_native_tests.rs": "",
            "src/foo_tests_fixture_tests.rs": "",
        }
        self.assertEqual(self.problems(files), [])

    def test_source_scan_is_crlf_safe_and_accepts_raw_path_strings(self):
        # The same token stream declares the same file under either checkout newline convention.
        source = '#[cfg(test)]\n#[path = r#"foo_topic_tests.rs"#]\nmod topic;\n'
        for content in [source, source.replace("\n", "\r\n")]:
            self.assertEqual(self.problems({"foo.rs": content, "foo_topic_tests.rs": ""}), [])

    def test_wrong_owner_and_ambiguous_prefix_fail(self):
        # A broader foo owner cannot steal foo_bar's exact or topical suite.
        for name in ["bar_tests.rs", "foo_bar_tests.rs", "foo_bar_topic_tests.rs"]:
            with self.subTest(name=name):
                files = {"foo.rs": f'#[cfg(test)] #[path = "{name}"] mod cases;\n',
                         "foo_bar.rs": "", name: ""}
                self.assertTrue(self.problems(files))

    def test_missing_duplicate_untracked_and_nonflat_declarations_fail(self):
        # A name alone is not enough: a suite must be present and have one real sibling declaration.
        cases = [
            {"foo.rs": "", "foo_tests.rs": ""},
            {"foo.rs": '#[cfg(test)] #[path = "foo_tests.rs"] mod one;\n'
                       '#[cfg(test)] #[path = "foo_tests.rs"] mod two;', "foo_tests.rs": ""},
            {"foo.rs": '#[cfg(test)] #[path = "foo_tests.rs"] mod cases;'},
            {"foo.rs": '#[cfg(test)] mod foo_tests;', "foo_tests.rs": ""},
            {"foo.rs": '#[cfg(test)] #[path = "sub/foo_tests.rs"] mod cases;', "sub/foo_tests.rs": ""},
            {"foo.rs": '#[cfg(test)] #[path = "../foo_tests.rs"] mod cases;', "foo_tests.rs": ""},
        ]
        for files in cases:
            with self.subTest(files=files):
                self.assertTrue(self.problems(files))

    def test_implicit_generic_test_file_is_rejected(self):
        # A cfg(test) declaration must not bypass owner naming by relying on Rust's default filename.
        files = {"lib.rs": "#[cfg(test)] mod tests;", "tests.rs": ""}
        self.assertTrue(self.problems(files))

    def test_explicit_generic_test_file_is_rejected(self):
        # An explicit path to generic tests.rs is still not an owner-named sibling suite.
        files = {"foo.rs": '#[cfg(test)] #[path = "tests.rs"] mod cases;', "tests.rs": ""}
        self.assertTrue(self.problems(files))

    def test_explicit_singular_test_file_is_rejected(self):
        # A singular suffix cannot escape the owner-named plural suite convention.
        files = {"foo.rs": '#[cfg(test)] #[path = "foo_test.rs"] mod cases;', "foo_test.rs": ""}
        self.assertTrue(self.problems(files))

    def test_test_gated_suites_cannot_hide_behind_arbitrary_names(self):
        # Resolve both Rust default layouts and explicit paths, including platform gates and CRLF.
        for declaration, target in [
            ('#[cfg(test)] mod cases;', 'cases.rs'),
            ('#[cfg(test)] mod cases;', 'cases/mod.rs'),
            ('#[cfg(all(test, windows))] #[path = "cases.rs"] mod cases;', 'cases.rs'),
        ]:
            for newline in ["\n", "\r\n"]:
                files = {"lib.rs": declaration + newline,
                         target: "#[test]" + newline + "fn contract() {}" + newline}
                with self.subTest(declaration=declaration, target=target, newline=newline):
                    self.assertTrue(self.problems(files))
        files = {"foo.rs": "#[cfg(test)] mod cases;",
                 "foo/cases.rs": "#[test] fn contract() {}"}
        self.assertTrue(self.problems(files))

    def test_loaded_suites_require_test_gate_regardless_of_filename(self):
        # Real test functions identify a suite even under a production-admitting cfg or no cfg.
        for gate in ['', '#[cfg(any(test, feature = "fixtures"))]']:
            for declaration in ['mod helper;', '#[path = "helper.rs"] mod helper;']:
                files = {"lib.rs": gate + declaration, "helper.rs": "#[test] fn contract() {}"}
                with self.subTest(gate=gate, declaration=declaration):
                    problems = self.problems(files)
                    self.assertTrue(any("test declaration must require cfg(test)" in item for item in problems), problems)

    def test_ungated_suite_loaded_from_test_file_requires_its_own_gate(self):
        # Inherited test context does not replace the explicit gate required on every suite declaration.
        files = {"foo.rs": '#[cfg(test)] #[path = "foo_tests.rs"] mod suite;',
                 "foo_tests.rs": '#[path = "helper.rs"] mod helper;',
                 "helper.rs": "#[test] fn contract() {}"}
        problems = self.problems(files)
        self.assertTrue(any("foo_tests.rs:1: test declaration must require cfg(test)" in item
                            for item in problems), problems)

    def test_inline_test_gated_suites_are_rejected(self):
        # Renaming the inline module or adding platform conditions cannot evade the flat-sibling rule.
        for declaration in ['#[cfg(test)] mod tests', '#[cfg(test)] mod cases',
                            '#[cfg(all(test, windows))] pub(crate) mod cases']:
            for newline in ["\n", "\r\n"]:
                source = declaration + " {" + newline + "#[test] fn contract() {}" + newline + "}"
                with self.subTest(declaration=declaration, newline=newline):
                    problems = self.problems({"foo.rs": source})
                    self.assertTrue(any("inline test suite" in item for item in problems), problems)

    def test_crate_roots_cannot_hold_inline_or_bare_tests(self):
        # Nothing declares a crate root, so its own text is the only place an ungated suite could hide.
        for root in ["crates/foo/src/lib.rs", "crates/foo/src/main.rs", "crates/foo/examples/demo.rs"]:
            for body in ["mod tests { #[test] fn contract() {} }",
                         "#[cfg(test)] mod tests { #[test] fn contract() {} }",
                         "#[test] fn contract() {}"]:
                with self.subTest(root=root, body=body):
                    problems = self.problems({root: body})
                    self.assertTrue(any("test" in item and root in item for item in problems), problems)

    def test_integration_and_bench_roots_may_hold_tests(self):
        # tests/ and benches/ targets are their own crates; their #[test] functions are the suite.
        for root in ["crates/foo/tests/contract.rs", "crates/foo/tests/suite/main.rs", "crates/foo/benches/speed.rs"]:
            with self.subTest(root=root):
                self.assertEqual(self.problems({root: "#[test] fn contract() {}"}), [])

    def test_inline_helpers_and_literal_test_text_do_not_hide_following_suites(self):
        # Inspecting an inline body must ignore fake test attributes and resume at the next file-scope item.
        for body in [
            "pub fn helper() {}",
            "/* #[test] fn hidden() {} */",
            'const TEXT: &str = r#"#[test] fn hidden() {}"#;',
            "macro_rules! example { () => { #[test] fn hidden() {} }; }",
            "opaque! { #[test] fn hidden() {} }",
        ]:
            source = '#[cfg(test)] mod helper { ' + body + ' }\n' \
                     '#[cfg(test)] #[path = "foo_tests.rs"] mod suite;'
            with self.subTest(body=body):
                self.assertEqual(self.problems({"foo.rs": source, "foo_tests.rs": ""}), [])

    def test_undeclared_files_reached_by_expansion_or_nesting_are_caught(self):
        # cfg_attr paths, include! and nested declarations leave the file holding #[test] undeclared,
        # so the crate-root rule rejects it even though the scanner does not expand those forms.
        cases = [
            {"lib.rs": '#[cfg(test)] #[cfg_attr(test, path = "alternate.rs")] mod helper;',
             "alternate.rs": "#[test] fn contract() {}"},
            {"lib.rs": '#[cfg(test)] mod helper;', "helper.rs": 'include!("alternate.rs");',
             "alternate.rs": "#[test] fn contract() {}"},
            {"lib.rs": '#[cfg(test)] mod outer { #[path = "helper.rs"] mod helper; }',
             "helper.rs": "#[test] fn contract() {}"},
        ]
        for files in cases:
            with self.subTest(files=files):
                problems = self.problems(files)
                self.assertTrue(any("crate root holds tests" in item for item in problems), problems)

    def test_documented_macro_generated_test_limit(self):
        # This source scanner is not rustc: tests produced by macro_rules! expansion need review.
        files = {"lib.rs": '#[cfg(test)] mod helper;',
                 "helper.rs": 'macro_rules! suite { () => { #[test] fn contract() {} }; } suite!();'}
        self.assertEqual(self.problems(files), [])

    def test_helper_imports_are_not_mistaken_for_test_suites(self):
        # Build-code test imports and source-scanner fixtures have no test functions of their own.
        for fixture in [
            "pub fn helper() {}",
            "// #[test] fn hidden() {}",
            'const TEXT: &str = "#[test] fn hidden() {}";',
            'const TEXT: &str = r#"#[test] fn hidden() {}"#;',
            "macro_rules! example { () => { #[test] fn hidden() {} }; }",
            "opaque! { #[test] fn hidden() {} }",
        ]:
            files = {"lib.rs": '#[cfg(test)] #[path = "helper.rs"] mod helper;', "helper.rs": fixture}
            with self.subTest(fixture=fixture):
                self.assertEqual(self.problems(files), [])
        files = {"src/lib.rs": '#[cfg(test)] #[path = "../build_config.rs"] mod build_config;',
                 "build_config.rs": "pub fn configure() {}"}
        self.assertEqual(self.problems(files), [])

    def test_invalid_suite_names_fail_without_owning_declarations(self):
        # Renaming an orphan to a generic or singular test filename must not remove it from the inventory.
        for name in ["tests.rs", "test.rs", "foo_test.rs"]:
            with self.subTest(name=name):
                self.assertTrue(self.problems({"foo.rs": "", name: ""}))

    def test_comments_strings_macros_and_unrelated_attributes_cannot_declare_tests(self):
        # Fake declarations must not turn a missing suite owner into a green scan.
        declaration = '#[cfg(test)] #[path = "foo_tests.rs"] mod cases;'
        for source in [
            "// " + declaration,
            "/* " + declaration + " */",
            'const TEXT: &str = r#"' + declaration + '"#;',
            "opaque! { " + declaration + " }",
            "macro_rules! example { () => { " + declaration + " }; }",
            '#[cfg(test)] #[path = "foo_tests.rs"] fn unrelated() {} mod cases;',
            '#[cfg(test)] mod nested { #[path = "foo_tests.rs"] mod cases; }',
        ]:
            with self.subTest(source=source):
                self.assertTrue(self.problems({"foo.rs": source, "foo_tests.rs": ""}))

    def test_cfg_requires_tests_in_every_alternative(self):
        # cfg(any(test, windows)) admits production Windows builds, unlike all(test, windows).
        for expression, valid in [("test", True), ("all(test, windows)", True),
                                  ("any(all(test, windows), all(test, unix))", True),
                                  ("any(test, windows)", False), ("not(test)", False),
                                  ("windows", False), ('feature = "test"', False)]:
            source = f'#[cfg({expression})] #[path = "foo_tests.rs"] mod cases;'
            with self.subTest(expression=expression):
                self.assertEqual(not self.problems({"foo.rs": source, "foo_tests.rs": ""}), valid)
        self.assertTrue(self.problems({"foo.rs": '#[path = "foo_tests.rs"] mod cases;', "foo_tests.rs": ""}))

    def test_generated_owners_resolve_and_native_roots_stay_excluded(self):
        # Generated lib.rs still owns authored tests; upstream test layouts are outside this first-party policy.
        from rust_test_ownership import inspect_test_ownership
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {"src/lib.rs": '#[cfg(test)] #[path = "lib_tests.rs"] mod suite;',
                     "src/lib_tests.rs": "", "native/upstream/orphan_tests.rs": ""}
            for name, source in files.items():
                target = root / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(source, encoding="utf-8")
            self.assertEqual(inspect_test_ownership(root, list(files), ("native/upstream/",)), [])
            self.assertTrue(inspect_test_ownership(root, [*files, "missing.rs"], ("native/upstream/",)))

    def test_ownership_violation_fails_the_source_size_cli(self):
        # The naming check is part of the enforced CLI, not an unused helper with green unit tests.
        import contextlib
        import io
        import json
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "scripts/native-dependencies.json").write_text(json.dumps({"libraries": []}), encoding="utf-8")
            cases = [
                {"foo_tests.rs": "// orphan\n"},
                {"lib.rs": "#[cfg(test)] mod tests;", "tests.rs": ""},
                {"foo.rs": '#[cfg(test)] #[path = "tests.rs"] mod cases;', "tests.rs": ""},
                {"foo.rs": '#[cfg(test)] #[path = "foo_test.rs"] mod cases;', "foo_test.rs": ""},
                {"lib.rs": 'mod helper;', "helper.rs": "#[test] fn contract() {}"},
                {"lib.rs": '#[cfg(any(test, feature = "fixtures"))] mod helper;',
                 "helper.rs": "#[test] fn contract() {}"},
                {"foo.rs": '#[cfg(test)] mod cases { #[test] fn contract() {} }'},
            ]
            for files in cases:
                for name, source in files.items():
                    (root / name).write_text(source, encoding="utf-8")
                with self.subTest(files=files), \
                        mock.patch.object(source_size, "native_roots", return_value=()), \
                        mock.patch.object(source_size, "tracked_paths", return_value=list(files)), \
                        mock.patch.object(source_size, "EXACT_EXCEPTIONS", {}), \
                        contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(source_size.main(["--root", str(root)]), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
