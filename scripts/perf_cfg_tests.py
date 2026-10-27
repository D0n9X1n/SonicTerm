"""Perf cfg tests."""

from __future__ import annotations
import contextlib
import io
import json
import shutil
from pathlib import Path
import tempfile
import unittest

import perf_arguments
import perf_build_tests
import perf_cli
import perf_counter_schema_tests
import perf_driver_tests
import perf_focus_tests
import perf_harness_sources
import perf_home_tests
import perf_metrics
import perf_outcomes_tests
import perf_prebuilt_tests
import perf_scenarios
import perf_sets
import perf_settings
import perf_trim_tests
import perf_watcher_tests


class HarnessCfgDetectionTests(unittest.TestCase):
    """One harness cfg decision for both sides, from each tree's owning crates."""

    def test_a_cfg_is_on_only_when_both_trees_define_every_method_in_code(self):
        # Every method in both trees turns the cfg on; one missing from either tree, or present only in a comment,
        # a string, a unit-test file, test-only code (a file, module, impl or the method's own attributes), or in
        # another crate than its owner, turns it off for both. Other attributes on the method leave it counted.
        table = perf_build_tests.REAL_GATE.HARNESS_API_CFGS
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)

            def tree(name, skip=None, extra=None):
                built = root / name
                perf_watcher_tests.write_atlas_retry_api(built, skip=skip)
                if extra is not None:
                    relative, body = extra
                    (built / relative).parent.mkdir(parents=True, exist_ok=True)
                    (built / relative).write_text(body, encoding="utf-8")
                return built
            full = tree("full")
            cases = {
                "both": (full, tree("both"), ("perf_atlas_retry_api",)),
                "base lacks one": (full, tree("lacks", skip="last_missing_chrome"), ()),
                "head lacks one": (tree("head-lacks", skip="font_fallback_notice_id"), full, ()),
                "a comment": (full, tree("comment", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/notes.rs", "// pub fn last_missing_chrome(\n")), ()),
                "a string": (full, tree("string", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/notes.rs", 'const NAME: &str = "\npub fn last_missing_chrome(";\n')),
                    ()),
                "a unit-test file": (full, tree("tests", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/core_tests.rs", "pub fn last_missing_chrome() {}\n")), ()),
                "another crate": (full, tree("crate", "last_missing_chrome", (
                    "crates/sonicterm-app/src/other.rs", "pub fn last_missing_chrome() {}\n")), ()),
                "another owner": (full, tree("owner", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs", "impl Api {\n    pub fn last_missing_chrome(&self) {}\n}\n")),
                    ()),
                "a free function": (full, tree("free", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs", "pub fn last_missing_chrome() {}\n")), ()),
                "a test-only impl": (full, tree("test-impl", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "#[cfg(test)]\nimpl GpuRenderer {\n    pub fn last_missing_chrome(&self) {}\n}\n")), ()),
                "a test-only module": (full, tree("test-mod", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "#[cfg(test)]\nmod tests {\n    impl GpuRenderer {\n        pub fn last_missing_chrome(&self) {}\n"
                    "    }\n}\n")), ()),
                "a test-only file": (full, tree("test-file", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "#![cfg(test)]\nimpl GpuRenderer {\n    pub fn last_missing_chrome(&self) {}\n}\n")), ()),
                "a test-only method": (full, tree("test-method", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "impl GpuRenderer {\n    #[cfg(test)]\n    pub fn last_missing_chrome(&self) {}\n}\n")), ()),
                "a test-only method under other attributes": (full, tree("test-method-attrs", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "impl GpuRenderer {\n    #[doc(hidden)]\n    #[cfg(test)]\n    #[inline]\n"
                    "    pub fn last_missing_chrome(&self) {}\n}\n")), ()),
                "a method with other attributes": (full, tree("method-attrs", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "impl GpuRenderer {\n    #[doc(hidden)]\n    #[inline]\n"
                    "    pub fn last_missing_chrome(&self) {}\n}\n")), ("perf_atlas_retry_api",)),
                "a path-qualified owner": (full, tree("path", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "impl crate::core::GpuRenderer {\n    pub fn last_missing_chrome(&self) {}\n}\n")),
                    ("perf_atlas_retry_api",)),
                "a trait impl": (full, tree("trait", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "impl Chrome for GpuRenderer {\n    fn last_missing_chrome(&self) {}\n}\n")), ()),
                # A trait impl defines no inherent method even when its trait shares the owner's name.
                "a trait named like the owner": (full, tree("trait-named", "last_missing_chrome", (
                    "crates/sonicterm-gpu/src/other.rs",
                    "impl GpuRenderer<Frame> for Painter {\n    pub fn last_missing_chrome(&self) {}\n}\n")), ()),
            }
            for name, (head, base, expected) in cases.items():
                with self.subTest(name):
                    decided, _per_tree = perf_harness_sources.harness_cfg_decision({"head": head, "base": base}, table)
                    self.assertEqual(decided, expected)


class TaskbarProgressCfgTests(unittest.TestCase):
    """S12/status-line's taskbar checks need the App's counting surface on both sides."""

    def test_a_base_without_the_counting_surface_turns_the_taskbar_cfg_off_for_both_sides(self):
        # The head defines both methods; a base that predates them (as 08874f7a) or has only the install
        # leaves the cfg off for both, so the head's harness never names an App method the base lacks.
        table = perf_build_tests.REAL_GATE.HARNESS_API_CFGS
        entry = next(entry for entry in table if entry.name == "perf_taskbar_progress_api")
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)

            def tree(name, methods):
                built = root / name
                source = built / "crates/sonicterm-app/src/program_state.rs"
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text("".join(f"impl App {{\n    #[doc(hidden)]\n    pub fn {method}(&mut self) {{}}\n}}\n"
                                          for method in methods), encoding="utf-8")
                return built
            every = [method for _crate, _owner, method in entry.methods]
            head = tree("head", every)
            cases = {"both": (tree("both", every), True), "base predates both": (tree("old", []), False),
                     "base has only the install": (tree("install", every[:1]), False)}
            for name, (base, expected) in cases.items():
                with self.subTest(name):
                    decided, _ = perf_harness_sources.harness_cfg_decision({"head": head, "base": base}, table)
                    self.assertEqual("perf_taskbar_progress_api" in decided, expected)


class AtlasRetryAvailabilityTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    """S1/atlas-retry is a declared diagnostic: unavailable on both sides when its harness cfg is off."""

    def test_a_previous_release_base_leaves_the_ordinary_sets_and_marks_atlas_retry_unavailable(self):
        # A base without the methods (as v1.3.8) turns the cfg off for both sides: S1/default still runs both its
        # sets strictly, and S1/atlas-retry runs nothing, with its reason in the table and the details.
        code, gate, _calls, plans, _work, out = self.compare(
            listing=perf_trim_tests.ATLAS_RETRY_LIST, scenarios=("S1", "S1/atlas-retry"), api_trees=("head",),
            options=("--counters", "--counters-runs", "1", "--require-base"),
            head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST, base_manifest=perf_counter_schema_tests.BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, perf_settings.EXIT_PASS, self.printed)
        self.assertTrue(plans)
        self.assertEqual({plan.variant for plan in plans}, {"default"}, "atlas-retry is never planned")
        self.assertTrue(all(step.harness_cfgs == () for step in gate.steps if step.id.startswith("build-")))
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("unavailable on both sides: perf_atlas_retry_api is off because the base tree lacks", document)
        self.assertIn("| S1/atlas-retry | counters set | unavailable | unavailable |", document)

    def test_two_current_trees_schedule_atlas_retry_and_need_its_runs(self):
        # Both trees have the methods, so the cfg is on and S1/atlas-retry runs its counters plans, needing every
        # valid run under --require-base.
        code, gate, _calls, plans, _work, _out = self.compare(
            listing=perf_trim_tests.ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",),
            options=("--counters", "--counters-runs", "2", "--require-base"),
            head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST, base_manifest=perf_counter_schema_tests.BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, perf_settings.EXIT_PASS, self.printed)
        self.assertEqual(sorted(plan.side for plan in plans), ["base", "base", "head", "head"])
        self.assertTrue(all(step.harness_cfgs == ("perf_atlas_retry_api",)
                            for step in gate.steps if step.id.startswith("build-")))
        short = [perf_sets.SetResult("S1/atlas-retry", "counters", perf_metrics.SideRuns([perf_outcomes_tests.make_outcome()]),
                                perf_metrics.SideRuns([]), target_runs=1)]
        self.assertTrue(perf_sets.strict_problems(short), "a missing atlas-retry run is still a strict problem")

    def test_a_selection_of_only_unavailable_diagnostics_does_not_pass(self):
        # With the cfg off, selecting only S1/atlas-retry leaves nothing runnable: blocked without --require-base
        # and failed with it, never passed.
        for options, expected in ((("--counters",), perf_settings.EXIT_BLOCKED),
                                  (("--counters", "--require-base"), perf_settings.EXIT_FAIL)):
            with self.subTest(options=options):
                code, *_rest = self.compare(listing=perf_trim_tests.ATLAS_RETRY_LIST, scenarios=("S1/atlas-retry",),
                                            api_trees=("head",), options=options,
                                            head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST, base_manifest=perf_counter_schema_tests.BASE_COUNTERS_MANIFEST)
                self.assertEqual(code, expected, self.printed)
        unavailable = [perf_sets.SetResult("S1/atlas-retry", "counters", perf_metrics.SideRuns(blocked="off"),
                                      perf_metrics.SideRuns(blocked="off"), unavailable="off")]
        self.assertEqual(perf_sets.strict_problems(unavailable), ["no runnable scenario set: S1/atlas-retry off"])

    def test_unexpected_blocking_stays_a_strict_problem(self):
        # Only a set declared unavailable is excused; any other blocked side still fails --require-base.
        blocked = [perf_sets.SetResult("S1/default", "timed", perf_metrics.SideRuns(blocked="the base cannot build"),
                                  perf_metrics.SideRuns([perf_outcomes_tests.make_outcome()]), target_runs=1)]
        self.assertEqual(perf_sets.comparison_exit(blocked, require_base=True), perf_settings.EXIT_FAIL)


class HarnessCfgPrebuiltTests(perf_prebuilt_tests.PrebuiltHarness, unittest.TestCase):
    """The harness cfg decision and the composed flags are part of the build identity a consumer checks."""

    def test_load_prebuilt_refuses_another_cfg_decision_or_other_flags(self):
        # The producer records the decision and the flags; a manifest naming other ones is refused.
        binaries, digest = self.produce()
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["harness_cfgs"], ["perf_atlas_retry_api"])
        declared = [token for name in perf_build_tests.REAL_GATE.HARNESS_API_CFG_NAMES for token in ("--check-cfg", f"cfg({name})")]
        self.assertEqual(manifest["rustflags"], [*declared, "--cfg", "perf_atlas_retry_api"])
        for key, value, message in (("harness_cfgs", [], "harness cfgs"),
                                    ("rustflags", declared, "compiler flags")):
            with self.subTest(key=key):
                changed = self.rewrite(binaries, lambda data, key=key, value=value: data.update({key: value}))
                with self.assertRaisesRegex(ValueError, f"refusing the prebuilt binaries: {message}"):
                    self.consume(binaries, changed)
                self.rewrite(binaries, lambda data, key=key: data.update({key: manifest[key]}))


class PreviousReleaseCheckTests(unittest.TestCase):
    """The head's harness overlaid on the previous release tag must still build."""

    HEAD, PARENT, BASE = "a" * 40, "b" * 40, "c" * 40

    def check(self, *, shallow="false", tags="v1.3.8\n", status="PASS", base_api=False):
        """Run the check against a fake repository; return its exit code, the gate and the git calls."""
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        root = Path(scratch.name)
        head = root / "head"
        (head / perf_settings.APP_MANIFEST).parent.mkdir(parents=True)
        (head / perf_settings.APP_MANIFEST).write_text(perf_home_tests.HEAD_MANIFEST, encoding="utf-8")
        (head / perf_settings.HARNESS_DIRECTORY).mkdir(parents=True)
        (head / perf_settings.HARNESS_DIRECTORY / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
        perf_watcher_tests.write_atlas_retry_api(head)
        calls = []

        def host_run(argv, timeout_s=perf_scenarios.GIT_TIMEOUT_S):
            calls.append(tuple(argv))
            words = tuple(argv[3:]) if tuple(argv[:2]) == ("git", "-C") else tuple(argv[1:])
            answers = {("rev-parse", "--is-shallow-repository"): shallow,
                       ("rev-parse", "--verify", "HEAD^{commit}"): self.HEAD,
                       ("rev-list", "--parents", "-n", "1", self.HEAD): f"{self.HEAD} {self.PARENT}",
                       ("tag", "--list", "v[0-9]*", "--merged", self.PARENT): tags,
                       ("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*", self.PARENT): "v1.3.8",
                       ("rev-parse", "--verify", "v1.3.8^{commit}"): self.BASE}
            if words[:2] == ("worktree", "add"):
                tree = Path(words[-2])
                (tree / perf_settings.APP_MANIFEST).parent.mkdir(parents=True)
                (tree / perf_settings.APP_MANIFEST).write_text(perf_home_tests.BASE_MANIFEST, encoding="utf-8")
                if base_api:
                    perf_watcher_tests.write_atlas_retry_api(tree)
                return perf_focus_tests.command(argv, "")
            if words[:2] == ("worktree", "remove"):
                shutil.rmtree(words[-1], ignore_errors=True)
                return perf_focus_tests.command(argv, "")
            return perf_focus_tests.command(argv, answers.get(words, "") + "\n")
        gate = perf_watcher_tests.FakeGate(lambda step: (status, 0 if status == "PASS" else 101,
                                      "" if status == "PASS" else "error[E0599]: no method named `x`\n"))
        work = root / "work"
        code = perf_cli.check_previous_release(gate, host_run, root / "out", work, {}, head_root=head)
        self.assertFalse(work.exists(), "the check removes only, and all, the scratch state it created")
        return code, gate, calls

    def test_the_previous_tag_is_checked_with_the_derived_cfgs_in_an_isolated_target(self):
        # The previous tag of HEAD's parent is checked with the base's features and the decision (off for a base
        # without the methods, on for one with them), in the check's own target directory.
        code, gate, calls = self.check()
        self.assertEqual(code, perf_settings.EXIT_PASS)
        self.assertIs(gate.steps[0], perf_build_tests.REAL_GATE.PREVIOUS_RELEASE_CHECKS[((), ())])
        # Compared by components: a Windows host writes the path with backslashes.
        self.assertEqual(Path(gate.environs[0]["CARGO_TARGET_DIR"]).parts[-2:], ("work", "target"))
        self.assertIn(("git", "worktree", "add", "--detach", str(gate.roots[0]), self.BASE), calls)
        code, gate, _calls = self.check(base_api=True)
        self.assertIs(gate.steps[0], perf_build_tests.REAL_GATE.PREVIOUS_RELEASE_CHECKS[(("perf_atlas_retry_api",), ())])

    def test_a_failed_check_fails_and_a_shallow_clone_is_refused(self):
        # A harness that does not build on the tag fails the step; a shallow checkout cannot select the tag.
        self.assertEqual(self.check(status="FAIL")[0], perf_settings.EXIT_FAIL)
        with self.assertRaisesRegex(ValueError, "shallow"):
            self.check(shallow="true")

    def test_no_earlier_tag_has_nothing_to_check(self):
        # Before the first release there is no tag to build against, so nothing is built.
        code, gate, _calls = self.check(tags="")
        self.assertEqual((code, gate.steps), (perf_settings.EXIT_PASS, []))

    def test_the_mode_takes_only_out(self):
        # The check is its own gate step; a comparison option with it is a usage error.
        self.assertTrue(perf_arguments.parse_args(["--check-previous-release"]).check_previous_release)
        for extra in (["--base", "main"], ["--smoke"], ["--counters"], ["--keep"]):
            with self.subTest(extra=extra), self.assertRaises(SystemExit), \
                    contextlib.redirect_stderr(io.StringIO()):
                perf_arguments.parse_args(["--check-previous-release", *extra])
