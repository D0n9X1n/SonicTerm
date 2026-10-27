"""Perf build tests."""

from __future__ import annotations
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import perf_harness_sources
import perf_home_tests
import perf_memory_tests
import perf_scenarios
import perf_settings


class CargoOverlayTests(unittest.TestCase):
    def test_entries_are_the_two_harness_tables_in_fixed_order(self):
        # The hash and the overlay read exactly the two harness examples, never another example.
        entries = perf_harness_sources.harness_entries(perf_home_tests.HEAD_MANIFEST)
        self.assertLess(entries.index('name = "perf_scenarios"'), entries.index('name = "perf_scenarios_alloc"'))
        self.assertNotIn("native_split_selection", entries)
        self.assertNotIn("dev-dependencies", entries)

    def test_missing_entries_are_inserted_and_other_tables_kept(self):
        # A base without the harness gains the head's two tables; everything else is untouched.
        overlaid = perf_harness_sources.overlay_manifest(perf_home_tests.BASE_MANIFEST, perf_harness_sources.harness_entries(perf_home_tests.HEAD_MANIFEST))
        self.assertEqual(perf_harness_sources.harness_entries(overlaid), perf_harness_sources.harness_entries(perf_home_tests.HEAD_MANIFEST))
        self.assertTrue(overlaid.startswith(perf_home_tests.BASE_MANIFEST.rstrip("\n")))
        if perf_home_tests.tomllib is not None:
            parsed = perf_home_tests.tomllib.loads(overlaid)
            self.assertEqual([entry["name"] for entry in parsed["example"]],
                             ["native_split_selection", "perf_scenarios", "perf_scenarios_alloc"])
            self.assertEqual(parsed["dev-dependencies"], {"tempfile": "3"})

    def test_identical_entries_leave_the_manifest_unchanged(self):
        # The head's own manifest, and a base that already matches, are not rewritten.
        self.assertEqual(perf_harness_sources.overlay_manifest(perf_home_tests.HEAD_MANIFEST, perf_harness_sources.harness_entries(perf_home_tests.HEAD_MANIFEST)), perf_home_tests.HEAD_MANIFEST)

    def test_different_or_duplicated_entries_are_replaced(self):
        # Every stale harness table is removed, so each example is defined once, as at head.
        stale = perf_home_tests.HARNESS_TABLES.replace("test = true", "test = false")
        for base in (perf_home_tests.PACKAGE + stale + perf_home_tests.DEPENDENCIES, perf_home_tests.PACKAGE + stale + "\n" + stale + perf_home_tests.DEPENDENCIES):
            with self.subTest(base=base):
                overlaid = perf_harness_sources.overlay_manifest(base, perf_harness_sources.harness_entries(perf_home_tests.HEAD_MANIFEST))
                self.assertEqual(perf_harness_sources.harness_entries(overlaid), perf_harness_sources.harness_entries(perf_home_tests.HEAD_MANIFEST))
                self.assertEqual(overlaid.count('name = "perf_scenarios"\n'), 1)
                if perf_home_tests.tomllib is not None:
                    self.assertEqual(len(perf_home_tests.tomllib.loads(overlaid)["example"]), 2)

    def test_head_without_both_entries_is_refused(self):
        # A head whose harness is not declared cannot define the overlay.
        for manifest in (perf_home_tests.BASE_MANIFEST, perf_home_tests.HEAD_MANIFEST.replace('name = "perf_scenarios_alloc"', 'name = "other"')):
            with self.subTest(manifest=manifest), self.assertRaises(ValueError):
                perf_harness_sources.harness_entries(manifest)


class HarnessHashTests(unittest.TestCase):
    def tree(self, root, files):
        for relative, data in files.items():
            target = Path(root) / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        return Path(root)

    def test_hash_covers_sorted_paths_bytes_and_entries(self):
        # Creation order does not matter; any byte, name or entry change does.
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            files = {"main.rs": b"fn main() {}\n", "nested/record.rs": b"pub struct Record;\n"}
            one = self.tree(first, files)
            two = self.tree(second, dict(reversed(list(files.items()))))
            digest = perf_harness_sources.harness_hash(one, perf_home_tests.HARNESS_TABLES)
            self.assertEqual(digest, perf_harness_sources.harness_hash(two, perf_home_tests.HARNESS_TABLES))
            self.assertRegex(digest, r"^[0-9a-f]{64}$")
            self.assertNotEqual(digest, perf_harness_sources.harness_hash(one, perf_home_tests.HARNESS_TABLES + "# changed\n"))
            (two / "main.rs").write_bytes(b"fn main() { }\n")
            self.assertNotEqual(digest, perf_harness_sources.harness_hash(two, perf_home_tests.HARNESS_TABLES))
            (two / "main.rs").write_bytes(files["main.rs"])
            (two / "main.rs").rename(two / "main2.rs")
            self.assertNotEqual(digest, perf_harness_sources.harness_hash(two, perf_home_tests.HARNESS_TABLES))

    def test_path_and_content_boundaries_cannot_collide(self):
        # Length prefixes keep `ab` + `c` apart from `a` + `bc`.
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            self.assertNotEqual(perf_harness_sources.harness_hash(self.tree(first, {"ab": b"c"}), ""),
                                perf_harness_sources.harness_hash(self.tree(second, {"a": b"bc"}), ""))

    def test_empty_or_linked_harness_is_refused(self):
        # An empty directory or a symlink would make the two trees' hashes meaningless.
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(ValueError):
                perf_harness_sources.harness_hash(Path(temporary), perf_home_tests.HARNESS_TABLES)
            root = self.tree(temporary, {"main.rs": b"x"})
            try:
                (root / "link.rs").symlink_to(root / "main.rs")
            except (OSError, NotImplementedError):
                self.skipTest("this host cannot create a symlink")
            with self.assertRaises(ValueError):
                perf_harness_sources.harness_hash(root, perf_home_tests.HARNESS_TABLES)

    def test_overlay_refuses_to_target_its_own_source(self):
        # The overlay deletes its target's harness first, so a target that is the source would lose it.
        with tempfile.TemporaryDirectory() as head_root:
            head = self.tree(head_root, {f"{perf_settings.HARNESS_DIRECTORY}/main.rs": b"head",
                                         perf_settings.APP_MANIFEST: perf_home_tests.HEAD_MANIFEST.encode()})
            spellings = [head, head / "crates" / ".."]
            alias = Path(head_root + "-alias")
            try:
                alias.symlink_to(head, target_is_directory=True)
                spellings.append(alias)
            except (OSError, NotImplementedError):
                pass  # This host cannot create a symlink; the other spellings still run.
            try:
                for target in spellings:
                    with self.subTest(target=target), self.assertRaises(ValueError):
                        perf_harness_sources.overlay_harness(head, target)
            finally:
                if alias.is_symlink():
                    alias.unlink()
            self.assertEqual((head / perf_settings.HARNESS_DIRECTORY / "main.rs").read_bytes(), b"head")

    def test_overlay_refuses_to_target_the_main_checkout(self):
        # The main checkout holds uncommitted work; ROOT is replaced so a broken guard deletes only a fixture.
        with tempfile.TemporaryDirectory() as head_root, tempfile.TemporaryDirectory() as main_root:
            head = self.tree(head_root, {f"{perf_settings.HARNESS_DIRECTORY}/main.rs": b"head",
                                         perf_settings.APP_MANIFEST: perf_home_tests.HEAD_MANIFEST.encode()})
            main = self.tree(main_root, {f"{perf_settings.HARNESS_DIRECTORY}/main.rs": b"uncommitted",
                                         perf_settings.APP_MANIFEST: perf_home_tests.HEAD_MANIFEST.encode()})
            with mock.patch.object(perf_settings, "ROOT", main), self.assertRaises(ValueError):
                perf_harness_sources.overlay_harness(head, main / "crates" / "..")
            self.assertEqual((main / perf_settings.HARNESS_DIRECTORY / "main.rs").read_bytes(), b"uncommitted")

    def test_overlay_makes_both_trees_hash_alike_and_leaves_src_alone(self):
        # The base gets the head's directory and entries; files under src/ are never touched.
        with tempfile.TemporaryDirectory() as head_root, tempfile.TemporaryDirectory() as base_root:
            harness = perf_settings.HARNESS_DIRECTORY
            head = self.tree(head_root, {f"{harness}/main.rs": b"head", f"{harness}/alloc_main.rs": b"alloc",
                                         perf_settings.APP_MANIFEST: perf_home_tests.HEAD_MANIFEST.encode()})
            base = self.tree(base_root, {f"{harness}/main.rs": b"old", f"{harness}/stale.rs": b"stale",
                                         perf_settings.APP_MANIFEST: perf_home_tests.BASE_MANIFEST.encode(),
                                         "crates/sonicterm-app/src/lib.rs": b"base source"})
            perf_harness_sources.overlay_harness(head, base)
            self.assertEqual(perf_harness_sources.tree_harness_hash(base), perf_harness_sources.tree_harness_hash(head))
            self.assertFalse((base / harness / "stale.rs").exists())
            self.assertEqual((base / "crates/sonicterm-app/src/lib.rs").read_bytes(), b"base source")


LIST_JSON = {"schema_version": 1, "scenarios": [
    {"id": "S1", "variants": ["default"], "title": "Idle", "timeout_s": 120, "short_timeout_s": 30},
    {"id": "S10", "variants": ["default", "sync"], "title": "TUI", "timeout_s": 200, "short_timeout_s": 40}]}


class BuildAndListTests(unittest.TestCase):
    def test_build_command_is_locked_and_reports_artifacts(self):
        # Release builds for comparisons, debug for the smoke; the artifact message names the binary.
        self.assertEqual(perf_scenarios.build_argv("perf_scenarios", release=True),
                         ("cargo", "build", "--locked", "--release", "-p", "sonicterm-app", "--example",
                          "perf_scenarios", "--message-format=json-render-diagnostics"))
        self.assertNotIn("--release", perf_scenarios.build_argv("perf_scenarios", release=False))

    def test_the_gate_s_reviewed_builds_run_the_same_commands(self):
        # The gate owns the steps perf-compare runs; their commands are the builds this script describes.
        for example in perf_harness_sources.HARNESS_EXAMPLES:
            for side in ("head", "base"):
                self.assertEqual(REAL_GATE.PERF_BUILDS[f"build-{side}-{example}"].argv,
                                 perf_scenarios.build_argv(example, release=True))
        self.assertEqual(REAL_GATE.PERF_BUILDS["build-perf_scenarios"].argv,
                         perf_scenarios.build_argv(perf_settings.HARNESS_EXAMPLE, release=False))

    def test_executable_comes_from_the_examples_artifact_message(self):
        # Only the named example's artifact counts, whatever target directory Cargo chose.
        log = "\n".join([
            "[local-gate] step=build", "",
            '{"reason":"compiler-artifact","target":{"name":"sonicterm_app","kind":["lib"]},"executable":null}',
            "   Compiling sonicterm-app v1.3.8",
            '{"reason":"compiler-artifact","target":{"name":"perf_scenarios","kind":["example"]},'
            '"executable":"/t/release/examples/perf_scenarios","fresh":true}',
            '{"reason":"build-finished","success":true}'])
        self.assertEqual(perf_scenarios.artifact_executable(log, "perf_scenarios"), Path("/t/release/examples/perf_scenarios"))
        self.assertIsNone(perf_scenarios.artifact_executable(log, "perf_scenarios_alloc"))

    def test_scenario_list_parses_from_the_step_log(self):
        # The list is the JSON line among the launcher's header and footer.
        log = "[local-gate] step=list\n\n" + json.dumps(LIST_JSON) + "\n\n[local-gate] result=PASS exit=0\n"
        scenarios = perf_scenarios.parse_scenario_list(log)
        self.assertEqual([scenario.id for scenario in scenarios], ["S1", "S10"])
        self.assertEqual(scenarios[1].variants, ("default", "sync"))
        self.assertEqual((scenarios[1].timeout_s, scenarios[1].short_timeout_s), (200, 40))

    def test_malformed_scenario_list_is_refused(self):
        # A wrong version, a missing field or no JSON at all cannot define the scenario set.
        broken = json.loads(json.dumps(LIST_JSON))
        del broken["scenarios"][0]["short_timeout_s"]
        for text in ("no json here", json.dumps({**LIST_JSON, "schema_version": 2}), json.dumps(broken),
                     json.dumps({"schema_version": 1, "scenarios": []})):
            with self.subTest(text=text), self.assertRaises(ValueError):
                perf_scenarios.parse_scenario_list(text)

    def test_selection_expands_all_dedupes_and_keeps_the_given_order(self):
        # `all` is every default variant; named variants run only when asked; repeats are dropped.
        scenarios = perf_scenarios.parse_scenario_list(json.dumps(LIST_JSON))
        self.assertEqual(perf_scenarios.select_scenarios(["all", "S10/sync", "S1", "S10/default"], scenarios),
                         [("S1", "default"), ("S10", "default"), ("S10", "sync")])
        self.assertEqual(perf_scenarios.select_scenarios(["S10/sync", "all"], scenarios),
                         [("S10", "sync"), ("S1", "default"), ("S10", "default")])
        for unknown in (["S99"], ["S1/sync"], []):
            with self.subTest(unknown=unknown), self.assertRaises(ValueError):
                perf_scenarios.select_scenarios(unknown, scenarios)

    def test_run_command_environment_and_scratch(self):
        # The child drops NO_COLOR and RUST_LOG, keeps HOME, and gets a new scratch path.
        binary, scratch = Path("/b/perf_scenarios"), Path("/tmp/run")
        argv = perf_scenarios.harness_argv(binary, "S10", "sync", perf_memory_tests.HARNESS_HASH, scratch, short=True, laps=True)
        self.assertEqual(argv, (str(binary), "--run", "S10", "--variant", "sync", "--short", "--laps",
                                "--managed", "--harness-hash", perf_memory_tests.HARNESS_HASH, str(scratch)))
        self.assertEqual(perf_scenarios.harness_environment({"HOME": "/h", "NO_COLOR": "1", "RUST_LOG": "debug"}),
                         {"HOME": "/h"})
        with tempfile.TemporaryDirectory() as temporary:
            scratch = perf_scenarios.new_scratch_path(Path(temporary), "S10", "sync")
            self.assertEqual(scratch.parent, Path(temporary))
            self.assertFalse(scratch.exists())

    def test_run_timeouts_add_a_margin_and_the_smoke_caps_at_100_s(self):
        # The bound is run_step's deadline; a run that reaches it is unresolved cleanup, never a pass or a retry.
        scenario = perf_scenarios.Scenario("S1", ("default",), "Idle", 120, 30)
        self.assertEqual(perf_scenarios.run_timeout_s(scenario, smoke=False), 120 + perf_scenarios.RUN_MARGIN_S)
        self.assertEqual(perf_scenarios.run_timeout_s(scenario, smoke=True), min(30 + perf_scenarios.RUN_MARGIN_S, 100))
        self.assertEqual(perf_scenarios.run_timeout_s(perf_scenarios.Scenario("S11", ("default",), "Image", 300, 90), smoke=True), 100)

    def test_a_short_comparison_run_gets_the_short_bound_without_the_smoke_cap(self):
        # A --short comparison run uses the scenario's short timeout plus the margin; only the smoke is capped.
        scenario = perf_scenarios.Scenario("S11", ("default",), "Image", 300, 90)
        self.assertEqual(perf_scenarios.run_timeout_s(scenario, smoke=False, short=True), 90 + perf_scenarios.RUN_MARGIN_S)
        self.assertEqual(perf_scenarios.run_timeout_s(scenario, smoke=False, short=False), 300 + perf_scenarios.RUN_MARGIN_S)


REAL_GATE = perf_settings.load_gate()
