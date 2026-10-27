"""Gate harness tests."""

from __future__ import annotations
from pathlib import Path
import re
import textwrap
import unittest
from unittest import mock

import gate_catalog
import gate_cli
import gate_harness
import gate_model
import gate_preparations
import gate_test_fixture


# The `--check-cfg` tokens every composed build carries: one per entry of the harness API cfg table.
DECLARED_HARNESS_CFGS = [token for name in gate_harness.HARNESS_API_CFG_NAMES for token in ("--check-cfg", f"cfg({name})")]


def workspace_check_cfgs(manifest):
    """The `check-cfg` entries of `[workspace.lints.rust]`'s `unexpected_cfgs`, ignoring comments and other tables."""
    section = None
    for line in manifest.splitlines():
        # Every quoted value in this array is single-quoted, so a `#` outside single quotes starts a comment.
        code = re.sub(r"#(?=(?:[^']*'[^']*')*[^']*$).*", "", line).strip()
        header = re.fullmatch(r"\[([^\]]+)\]", code)
        if header:
            section = header[1].strip()
        elif section == "workspace.lints.rust" and code.startswith("unexpected_cfgs"):
            array = re.search(r"check-cfg\s*=\s*\[(.*)\]", code)
            return re.findall(r"'([^']*)'", array[1]) if array else []
    return []


class HarnessFlagTests(unittest.TestCase):
    """Harness API cfg flags, composed once at launch from what a step inherits."""

    def test_the_combined_steps_enable_every_table_entry_and_no_api_has_its_own_step(self):
        # One lint and one test step enable every HARNESS_API_CFGS entry at once, so a new entry is linted and
        # tested with its cfg on without another dependency rebuild; Cargo.toml declares each entry for check-cfg.
        # The expected names come from the table entries themselves, not the derived name tuple the steps use,
        # so a filter on that tuple cannot shrink both sides of the comparison and still pass.
        expected = [entry.name for entry in gate_harness.HARNESS_API_CFGS]
        self.assertTrue(expected, "the harness API cfg table is empty")
        self.assertEqual(len(expected), len(set(expected)), expected)
        steps = {step.id: step for step in gate_catalog.STEPS}
        for lint_id in ("perf-scenarios-harness-api-clippy", "perf-scenarios-harness-api-echo-trace-clippy"):
            lint = steps[lint_id]
            enabled = list(lint.harness_cfgs)
            self.assertEqual(enabled, expected, lint_id)
        for test_id in ("perf-scenarios-harness-api-tests", "perf-scenarios-harness-api-echo-trace-tests"):
            self.assertEqual(list(steps[test_id].harness_cfgs), expected, test_id)
        combined = {"perf-scenarios-harness-api-clippy", "perf-scenarios-harness-api-tests",
                    "perf-scenarios-harness-api-echo-trace-clippy", "perf-scenarios-harness-api-echo-trace-tests"}
        for step in gate_catalog.STEPS:
            if step.id.startswith("perf-scenarios-") and step.id not in combined:
                with self.subTest(step=step.id):
                    self.assertNotIn("--cfg", step.argv)
                    self.assertIn(step.harness_cfgs, (None, ()))
        declared = workspace_check_cfgs((gate_test_fixture.ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        for name in expected:
            with self.subTest(cfg=name):
                self.assertIn(f"cfg({name})", declared)

    def test_every_echo_timeline_combination_is_a_maintained_lint_and_test(self):
        # The echo timeline's adapter needs perf-echo-trace and perf_echo_timeline_api together, and its
        # complement compiles with either missing, so each of the four feature/cfg combinations has a maintained
        # lint and a maintained test step: run locally on every host and in CI.
        def features(step):
            words = list(step.argv)
            return set(words[words.index("--features") + 1].split(",")) if "--features" in words else set()

        def cfgs(step):
            words = list(step.argv)
            return {words[index + 1] for index, word in enumerate(words) if word == "--cfg"} | set(
                step.harness_cfgs or ())

        found = {}
        for step in gate_catalog.STEPS:
            if "perf_scenarios" not in step.argv or step.evidence != "local" or not step.ci_jobs:
                continue
            if step.hosts != gate_model.HOSTS:
                continue
            kind = "clippy" if "clippy" in step.argv else "test" if "test" in step.argv else None
            if kind is None or "--ignored" in step.argv:
                continue
            combination = ("perf-echo-trace" in features(step), "perf_echo_timeline_api" in cfgs(step))
            found.setdefault(combination, {}).setdefault(kind, []).append(step.id)
        for combination in ((False, False), (True, False), (False, True), (True, True)):
            for kind in ("clippy", "test"):
                with self.subTest(feature=combination[0], cfg=combination[1], kind=kind):
                    self.assertTrue(found.get(combination, {}).get(kind), found)
        self.assertEqual(found.get((True, True)), {"clippy": ["perf-scenarios-harness-api-echo-trace-clippy"],
                                               "test": ["perf-scenarios-harness-api-echo-trace-tests"]})

    def test_workspace_check_cfgs_reads_only_the_live_lint_array(self):
        # A declaration surviving only in a comment, or outside [workspace.lints.rust], is not a declaration.
        manifest = textwrap.dedent("""\
            [workspace.lints.rust]
            # unexpected_cfgs = { level = "warn", check-cfg = ['cfg(commented_out)'] }
            unexpected_cfgs = { level = "warn", check-cfg = ['cfg(feature, values("x"))', 'cfg(live_one)'] } # 'cfg(trailing)'
            [workspace.lints.clippy]
            unexpected_cfgs = { check-cfg = ['cfg(other_table)'] }
            """)
        self.assertEqual(workspace_check_cfgs(manifest), ['cfg(feature, values("x"))', "cfg(live_one)"])

    def test_encoded_flags_take_precedence_and_plain_flags_are_tokenized_as_cargo_does(self):
        # CARGO_ENCODED_RUSTFLAGS is kept token for token and RUSTFLAGS is then ignored, as Cargo ignores it; plain
        # RUSTFLAGS splits on spaces with empty tokens dropped; an empty encoded value means no inherited flags.
        declared = DECLARED_HARNESS_CFGS
        encoded = {"CARGO_ENCODED_RUSTFLAGS": "-C\x1fdebuginfo=1 x", "RUSTFLAGS": "-D warnings"}
        self.assertEqual(gate_harness.compose_harness_rustflags(encoded, ()), ["-C", "debuginfo=1 x", *declared])
        plain = {"RUSTFLAGS": "  -C  target-cpu=native "}
        self.assertEqual(gate_harness.compose_harness_rustflags(plain, ("perf_atlas_retry_api",)),
                         ["-C", "target-cpu=native", *declared, "--cfg", "perf_atlas_retry_api"])
        self.assertEqual(gate_harness.compose_harness_rustflags({"CARGO_ENCODED_RUSTFLAGS": "", "RUSTFLAGS": "-D x"}, ()),
                         declared)

    def test_an_inherited_flag_against_the_decision_is_refused(self):
        # Appending cannot unset a cfg, so an inherited flag that sets an owned cfg the decision leaves off, or
        # sets it to a value, is refused; one that agrees with an on decision, or names another cfg, is kept.
        for environ in ({"RUSTFLAGS": "--cfg perf_atlas_retry_api"},
                        {"CARGO_ENCODED_RUSTFLAGS": "--cfg=perf_atlas_retry_api"},
                        {"RUSTFLAGS": '--cfg perf_atlas_retry_api="1"'}):
            with self.subTest(environ=environ), self.assertRaises(gate_harness.HarnessFlagConflict):
                gate_harness.compose_harness_rustflags(environ, ())
        with self.assertRaises(gate_harness.HarnessFlagConflict):
            gate_harness.compose_harness_rustflags({"RUSTFLAGS": '--cfg perf_atlas_retry_api="1"'},
                                           ("perf_atlas_retry_api",))
        with self.assertRaises(gate_harness.HarnessFlagConflict):
            gate_harness.compose_harness_rustflags({}, ("perf_unknown_api",))
        self.assertIn("--cfg", gate_harness.compose_harness_rustflags({"RUSTFLAGS": "--cfg perf_atlas_retry_api"},
                                                              ("perf_atlas_retry_api",)))
        self.assertEqual(gate_harness.compose_harness_rustflags({"RUSTFLAGS": "--cfg other"}, ())[:2], ["--cfg", "other"])

    def test_every_spelling_of_an_owned_cfg_is_read_as_rustc_reads_it(self):
        # rustc sets the cfg for `--cfg X` and `--cfg=X` whatever whitespace surrounds the spec, and for a raw
        # identifier, so each spelling, encoded or plain, is refused when the decision is off; a value form is
        # refused either way, and a spelling that only names the cfg is accepted when the decision is on.
        owned = "perf_atlas_retry_api"
        encoded = ["--cfg\x1f perf_atlas_retry_api", "--cfg\x1fperf_atlas_retry_api ", "--cfg\x1fperf_atlas_retry_api\t",
                   "--cfg= perf_atlas_retry_api", "--cfg=perf_atlas_retry_api\t", "--cfg=\tperf_atlas_retry_api \t",
                   "--cfg\x1fr#perf_atlas_retry_api", '--cfg\x1fperf_atlas_retry_api = "1"',
                   '--cfg=perf_atlas_retry_api="1"']
        plain = ["--cfg=perf_atlas_retry_api\t", "--cfg r#perf_atlas_retry_api", "--cfg\tperf_atlas_retry_api"]
        # A spec rustc cannot parse but that names an owned cfg is read as setting it with a value: refused always.
        unparsed = [{"CARGO_ENCODED_RUSTFLAGS": "--cfg=perf_atlas_retry_api!"}, {"RUSTFLAGS": "--cfg=(perf_atlas_retry_api)"}]
        for environ in unparsed:
            for enabled in ((), (owned,)):
                with self.subTest(unparsed=environ, enabled=enabled), self.assertRaises(gate_harness.HarnessFlagConflict):
                    gate_harness.compose_harness_rustflags(environ, enabled)
        cases = [{"CARGO_ENCODED_RUSTFLAGS": flags} for flags in encoded] + [{"RUSTFLAGS": flags} for flags in plain]
        for environ in cases:
            with self.subTest(environ=environ), self.assertRaises(gate_harness.HarnessFlagConflict):
                gate_harness.compose_harness_rustflags(environ, ())
        for environ in cases[:7]:
            with self.subTest(enabled=environ):
                gate_harness.compose_harness_rustflags(environ, (owned,))
        for environ in cases[7:9]:
            with self.subTest(value=environ), self.assertRaises(gate_harness.HarnessFlagConflict):
                gate_harness.compose_harness_rustflags(environ, (owned,))
        self.assertTrue(gate_harness.compose_harness_rustflags({"CARGO_ENCODED_RUSTFLAGS": "--cfg\x1f perf_other_api "}, ()))

    def test_target_cfg_plans_preserve_dependencies_and_all_lint_modes(self):
        # API flags are final-target arguments, never Cargo-wide environment changes. Tests compile a real
        # libtest binary; lint retains all-targets coverage plus cfg-on normal and test example targets.
        steps = {step.id: step for step in gate_catalog.STEPS}
        test = steps["perf-scenarios-harness-api-tests"]
        planned, = gate_harness.target_compile_steps(test)
        self.assertEqual(planned.argv[:2], ("cargo", "rustc"))
        self.assertIn("--profile", planned.argv)
        self.assertEqual(planned.argv[planned.argv.index("--profile") + 1], "test")
        self.assertLess(planned.argv.index("--"), planned.argv.index("--cfg"))
        self.assertEqual(planned.env, ())
        self.assertTrue(gate_preparations._reviewed_step(planned))
        lint = gate_harness.target_compile_steps(steps["perf-scenarios-harness-api-clippy"])
        self.assertEqual(len(lint), 4)
        self.assertIn("--all-targets", lint[0].argv)
        self.assertNotIn("--cfg", lint[0].argv)
        for step in lint[1:]:
            self.assertNotIn("--all-targets", step.argv)
            self.assertIn("--cfg", step.argv)
        self.assertEqual({(plan.argv[plan.argv.index("--example") + 1], "--test" in plan.argv)
                          for plan in lint[1:]}, {("perf_scenarios", False), ("perf_scenarios", True),
                                                 ("perf_scenarios_alloc", False)})
        self.assertIn("--test", lint[2].argv)
        self.assertEqual(lint[2].env, (("RUSTC_WORKSPACE_WRAPPER", "clippy-driver"),))
        self.assertNotIn("RUSTFLAGS=", gate_harness.command_text(test))
        self.assertIn("harness-target.py", gate_harness.command_text(test))

    def test_macos_only_custody_test_stays_in_a_required_macos_step(self):
        # Relocating the portable suite must not turn its macOS-only zombie-group test into a Linux skip.
        step = next(step for step in gate_catalog.STEPS if step.id == "macos-gate-custody")
        self.assertEqual(step.hosts, ("macos",))
        self.assertEqual(step.ci_jobs, ("macos-core",))
        self.assertEqual(step.argv[-1],
                         "RunnerTests.test_a_zombie_only_group_kill_is_refused_on_macos_and_the_step_is_still_reaped")
        self.assertIn(step, gate_cli.select_steps("macos"))
        self.assertNotIn(step, gate_cli.select_steps("windows"))
        self.assertNotIn(step, gate_cli.select_steps("linux"))

    def test_cfg_on_lint_preserves_every_consuming_target_cfg_pair(self):
        # --all-targets previously covered both roots and all seven API cfgs. Check the consumer
        # source inventory independently of the planner, so a new root cannot silently lose cfg-on lint.
        package = gate_test_fixture.ROOT / "crates/sonicterm-app"
        manifest = (package / "Cargo.toml").read_text(encoding="utf-8")
        cfg_names = set(gate_harness.HARNESS_API_CFG_NAMES)
        baseline = set()
        for section in re.split(r"(?m)^\[\[example\]\]\s*$", manifest)[1:]:
            name = re.search(r'^name\s*=\s*"([^"]+)"', section, re.M)[1]
            source = package / re.search(r'^path\s*=\s*"([^"]+)"', section, re.M)[1]
            body = source.read_text(encoding="utf-8")
            consumes = set(re.findall(r"\bperf_\w+_api\b", body))
            for module in re.findall(r"(?m)^mod (\w+);", body):
                module_path = source.parent / f"{module}.rs"
                if module_path.exists():
                    consumes.update(re.findall(r"\bperf_\w+_api\b", module_path.read_text(encoding="utf-8")))
            baseline.update((name, cfg) for cfg in consumes & cfg_names)
        expected = {(target, cfg) for target in ("perf_scenarios", "perf_scenarios_alloc") for cfg in cfg_names}
        self.assertEqual(baseline, expected)
        for step in gate_catalog.STEPS:
            if step.id not in ("perf-scenarios-harness-api-clippy", "perf-scenarios-harness-api-echo-trace-clippy"):
                continue
            after = set()
            for plan in gate_harness.target_compile_steps(step):
                if "--cfg" in plan.argv:
                    target = plan.argv[plan.argv.index("--example") + 1]
                    after.update((target, plan.argv[index + 1]) for index, token in enumerate(plan.argv) if token == "--cfg")
            self.assertEqual(after, baseline, step.id)

    def test_cfg_inventory_reads_utf8_even_with_a_legacy_windows_default(self):
        # A UTF-8 macOS locale cannot expose Windows cp1252 decoding failures. Force that default
        # only for omitted encodings and run the real non-ASCII source inventory, not an ASCII fixture.
        read_text = Path.read_text

        def legacy_read(path, encoding=None, errors=None, **options):
            return read_text(path, encoding=encoding or "cp1252", errors=errors, **options)

        with mock.patch.object(Path, "read_text", legacy_read):
            self.test_cfg_on_lint_preserves_every_consuming_target_cfg_pair()

    def test_every_comparison_build_composes_its_cfgs_and_no_step_fixes_its_flags(self):
        # Each comparison build and previous-release check carries its cfg subset for launch-time composition, the
        # catalogs cover every cfg and feature subset, and no step replaces the inherited flags with a fixed env.
        subsets = gate_preparations._feature_subsets(gate_harness.HARNESS_API_CFG_NAMES)
        self.assertEqual(len(gate_preparations.PERF_CFG_BUILDS), len(subsets) * 32)
        for (cfgs, _features), steps in gate_preparations.PERF_CFG_BUILDS.items():
            self.assertTrue(all(step.harness_cfgs == cfgs and gate_preparations._reviewed_step(step) for step in steps.values()))
        for (cfgs, _features), step in gate_preparations.PREVIOUS_RELEASE_CHECKS.items():
            self.assertEqual((step.harness_cfgs, step.argv[:3]), (cfgs, ("cargo", "check", "--locked")))
        every = [*gate_catalog.STEPS, *gate_preparations.PREVIOUS_RELEASE_CHECKS.values(),
                 *(step for steps in gate_preparations.PERF_CFG_BUILDS.values() for step in steps.values())]
        for step in every:
            self.assertFalse({"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"} & {name for name, _ in step.env}, step.id)
