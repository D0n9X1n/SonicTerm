"""Perf driver tests."""

from __future__ import annotations
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import perf_arguments
import perf_build_tests
import perf_counter_rows
import perf_counter_schema_tests
import perf_display_tests
import perf_driver
import perf_execution
import perf_focus_tests
import perf_harness_sources
import perf_home_tests
import perf_host_tests
import perf_metrics
import perf_outcomes_tests
import perf_prebuilt
import perf_scenarios
import perf_sets
import perf_settings
import perf_smoke
import perf_smoke_tests
import perf_watcher_tests
import perf_worktrees


class CompareHarness:
    """Drives `_compare` with fake git, Cargo and runs; shared by the driver, strict-base and prebuilt tests."""

    SHAS = {"main": "1" * 40, "HEAD": "2" * 40}

    def compare(self, base_build="PASS", assets=("base", "head"), options=(), environ=None,
                head_manifest=perf_home_tests.HEAD_MANIFEST, listing=None, scenarios=("S1",), base_manifest=perf_home_tests.BASE_MANIFEST,
                logging_api=("base", "head"), build_status="PASS", list_fail=(), base_run=None,
                head_build="PASS", real_binaries=False, toolchain=None, hook_trees=(), head_run=None,
                trim_trees=(), counters_run=None, api_trees=("base", "head")):
        """Drive the comparison with fake git, Cargo and runs; return the exit code, gate, git calls and paths.

        A counters run answers with the gate on; what the comparison printed is kept in `self.printed`.
        `list_fail` names the sides whose `--list` fails; `base_run`, when given, answers every base run,
        and `head_run` every head run outside the counters set.
        `real_binaries` writes each build's executable under the test's directory, so build-only can copy
        it; `toolchain` replaces the `rustc -vV` text, and a `--build-only` or `--prebuilt` run takes no
        `--scenario` or `--runs`. `trim_trees` names the trees whose app source defines the trim hook method;
        `hook_trees` names the trees whose app source defines the
        checkpoint-memory hook method; `api_trees` the trees that define S1/atlas-retry's App and renderer
        methods (by default both, as two current trees do).
        """
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        work, out = root / "work", root / "out"
        out.mkdir()
        git_calls = []

        def host_run(argv, timeout_s=perf_scenarios.GIT_TIMEOUT_S):
            # Resolve this runtime-only dependency after module initialization.
            import perf_atlas_tests
            import perf_fallback_tests
            git_calls.append(tuple(argv))
            if tuple(argv) == ("rustc", "-vV"):
                return perf_focus_tests.command(argv, toolchain or perf_host_tests.RUSTC_VV)
            if tuple(argv) == ("cargo", "-V"):
                return perf_focus_tests.command(argv, "cargo 1.99.0 (abcdef 2026-09-01)\n")
            if tuple(argv[:2]) == ("git", "rev-parse"):
                return perf_focus_tests.command(argv, self.SHAS[argv[-1].split("^")[0]] + "\n")
            if tuple(argv[:3]) == ("git", "worktree", "add"):
                tree = Path(argv[-2])
                manifest = head_manifest if tree.name == "head" else base_manifest
                (tree / perf_settings.APP_MANIFEST).parent.mkdir(parents=True)
                (tree / perf_settings.APP_MANIFEST).write_text(manifest, encoding="utf-8")
                # Each tree's logging crate, with the filtered init only for the trees in `logging_api`.
                (tree / perf_counter_schema_tests.LOGGING_LIB).parent.mkdir(parents=True)
                (tree / perf_counter_schema_tests.LOGGING_LIB).write_text(perf_counter_schema_tests.LOGGING_WITH_FILTER if tree.name in logging_api
                                                else perf_counter_schema_tests.LOGGING_WITHOUT_FILTER, encoding="utf-8")
                if tree.name in hook_trees:
                    hook = tree / perf_harness_sources.APP_SOURCE_DIRECTORY / "app" / "memory_snapshot.rs"
                    hook.parent.mkdir(parents=True, exist_ok=True)
                    hook.write_text(perf_fallback_tests.HOOK_SOURCE, encoding="utf-8")
                if tree.name in api_trees:
                    perf_watcher_tests.write_atlas_retry_api(tree)
                if tree.name in trim_trees:
                    trim = tree / perf_harness_sources.APP_SOURCE_DIRECTORY / "app" / "retention.rs"
                    trim.parent.mkdir(parents=True, exist_ok=True)
                    trim.write_text(perf_atlas_tests.TRIM_SOURCE, encoding="utf-8")
                if tree.name in assets:
                    font = tree / "assets" / "fonts" / "SymbolsNerdFontMono-Regular.ttf"
                    font.parent.mkdir(parents=True)
                    font.write_bytes(b"font")
                if tree.name == "head":
                    (tree / perf_settings.HARNESS_DIRECTORY).mkdir(parents=True)
                    (tree / perf_settings.HARNESS_DIRECTORY / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
            return perf_focus_tests.command(argv, "")

        def answer(step):
            if step.id.startswith("build-"):
                side, example = step.id.split("-", 2)[1:]
                if (side == "base" and base_build != "PASS") or (side == "head" and head_build != "PASS"):
                    return "FAIL", 101, "error[E0599]: no method named `run_action` found\n"
                executable = f"/{side}/{example}"
                if real_binaries:
                    # A file that exists and is executable, under a directory with no `assets` of its own.
                    built = root / "cargo" / side / perf_prebuilt.executable_name(example)
                    built.parent.mkdir(parents=True, exist_ok=True)
                    built.write_bytes(f"{side} {example} binary".encode("utf-8"))
                    built.chmod(0o755)
                    executable = str(built)
                artifact = {"reason": "compiler-artifact", "target": {"name": example, "kind": ["example"]},
                            "executable": executable}
                return build_status, 0, json.dumps(artifact) + "\n"
            # A listing step's argv[0] is the binary, whose path names its side.
            if any(f"/{side}/" in step.argv[0].replace(os.sep, "/") for side in list_fail):
                return "FAIL", 1, "dyld: Library not loaded: libcairo.2.dylib\n"
            return "PASS", 0, json.dumps(listing or perf_build_tests.LIST_JSON) + "\n"
        gate = perf_watcher_tests.FakeGate(answer)
        selection = () if "--build-only" in options else ("--scenario", *scenarios, "--runs", "1")
        args = perf_arguments.parse_args(["--base", "main", "--head", "HEAD", *selection, *options])
        plans = []
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 60000, "scale_factor": 2.0}

        def fake_run(plan, host, evidence):
            plans.append(plan)
            # When: a test supplies counters runs, they answer every counters plan of either side.
            if counters_run is not None and getattr(plan, "counters", False):
                return counters_run(plan)
            if base_run is not None and plan.side == "base":
                return base_run(plan)
            if head_run is not None and plan.side == "head" and not getattr(plan, "counters", False):
                return head_run(plan)
            if getattr(plan, "counters", False):
                return perf_outcomes_tests.make_outcome(plan=plan, result=perf_counter_schema_tests.counters_result({"window.attempts": 5}, monitor=monitor))
            return perf_display_tests.display_run(60000)(plan)
        printed = io.StringIO()
        # An installed Linux package's assets would win over the worktree's, so this host has none.
        with mock.patch.object(perf_scenarios, "LINUX_SHARED_ASSETS", root / "no-installed-assets"), \
                mock.patch.object(perf_smoke, "production_host", return_value=None), \
                mock.patch.object(perf_execution, "execute_run", side_effect=fake_run), \
                mock.patch.dict(os.environ, environ or {}), \
                contextlib.redirect_stdout(printed):
            code = perf_driver._compare(args, gate, out, work, perf_worktrees.Worktrees(host_run, work), host_run)
        self.printed = printed.getvalue()
        return code, gate, git_calls, plans, work, out


class CompareDriverTests(CompareHarness, unittest.TestCase):
    def test_the_counters_set_runs_on_the_head_only_after_the_timed_set(self):
        # Only the head declares perf-counters: it builds with it and runs a head-only counters set after the
        # timed set, and the base and change cells read n/a.
        code, gate, _calls, plans, _work, out = self.compare(options=("--counters", "--counters-runs", "2"),
                                                             head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST)
        self.assertEqual(code, 0)
        self.assertEqual([(plan.side, plan.counters) for plan in plans],
                         [("base", False), ("head", False), ("head", True), ("head", True)])
        builds = {step.id: step.argv for step in gate.steps if step.id.startswith("build-")}
        self.assertIn("perf-counters", builds["build-head-perf_scenarios"])
        self.assertNotIn("--features", builds["build-base-perf_scenarios"])
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S1/default |  | status | n/a | 2 valid runs |  |", document)
        self.assertIn("| S1/default | workload | window.attempts (count) | n/a | 5 (5–5) | n/a |", document)
        self.assertIn("- Built with `--features perf-counters`: head", document)
        self.assertIn("--counters --counters-runs 2", document)

    def test_the_counters_set_runs_on_both_sides_when_the_base_declares_the_feature(self):
        # Both refs build with the feature, the counters set runs base and head runs after the timed set, and the
        # table compares them like the timed table.
        code, gate, _calls, plans, _work, out = self.compare(options=("--counters", "--counters-runs", "2"),
                                                             head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST,
                                                             base_manifest=perf_counter_schema_tests.BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, 0)
        self.assertEqual([(plan.side, plan.counters) for plan in plans[:2]], [("base", False), ("head", False)])
        self.assertEqual(sorted(plan.side for plan in plans[2:] if plan.counters), ["base", "base", "head", "head"])
        self.assertEqual(len(plans), 6)
        builds = {step.id: step.argv for step in gate.steps if step.id.startswith("build-")}
        self.assertIn("perf-counters", builds["build-base-perf_scenarios"])
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S1/default |  | status | 2 valid runs | 2 valid runs |  |", document)
        self.assertIn("| S1/default | workload | window.attempts (count) | 5 (5–5) | 5 (5–5) | +0.0% |", document)
        self.assertIn("- Built with `--features perf-counters`: base, head", document)

    def test_the_counters_run_count_defaults_to_runs(self):
        # Without --counters-runs the counters set takes --runs.
        _code, _gate, _calls, plans, _work, _out = self.compare(options=("--counters",),
                                                                head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST)
        self.assertEqual(sum(1 for plan in plans if plan.counters), 1)

    def test_a_base_with_the_feature_but_no_filtered_logging_init_runs_head_only(self):
        # The 4d19e855 case: the base declares perf-counters but lacks the logging API the head's harness calls,
        # so it builds without the feature and the counters set runs on the head only, base n/a.
        code, gate, _calls, plans, _work, out = self.compare(options=("--counters", "--counters-runs", "2"),
                                                             head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST,
                                                             base_manifest=perf_counter_schema_tests.BASE_COUNTERS_MANIFEST,
                                                             logging_api=("head",))
        self.assertEqual(code, 0)
        builds = {step.id: step.argv for step in gate.steps if step.id.startswith("build-")}
        self.assertNotIn("perf-counters", builds["build-base-perf_scenarios"])
        self.assertIn("perf-counters", builds["build-head-perf_scenarios"])
        self.assertEqual([plan.side for plan in plans if plan.counters], ["head", "head"])
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S1/default | workload | window.attempts (count) | n/a | 5 (5–5) | n/a |", document)
        self.assertIn("- Built with `--features perf-counters`: head\n", document)

    def test_a_head_without_the_feature_skips_the_counters_set_and_says_so(self):
        # No head build gets --features, no counters run is planned, and both the log and the table say why.
        code, gate, _calls, plans, _work, out = self.compare(options=("--counters",))
        self.assertEqual(code, 0)
        self.assertFalse(any(plan.counters for plan in plans))
        self.assertFalse(any("--features" in step.argv for step in gate.steps))
        self.assertIn("counters: head does not support them", self.printed)
        self.assertIn("counters: head does not support them", (out / "comparison.md").read_text(encoding="utf-8"))

    def test_the_overhead_table_covers_s2_and_s3_only(self):
        # The head's counters runs against its timed runs, for S2 here and never for S1.
        listing = {"schema_version": 1, "scenarios": perf_build_tests.LIST_JSON["scenarios"] + [
            {"id": "S2", "variants": ["default"], "title": "Typing", "timeout_s": 120, "short_timeout_s": 30}]}
        _code, _gate, _calls, _plans, _work, out = self.compare(
            options=("--counters",), head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST, listing=listing, scenarios=("S1", "S2"))
        document = (out / "comparison.md").read_text(encoding="utf-8")
        overhead = document.split("### Counters overhead", 1)[1].split("###", 1)[0]
        self.assertIn("counters-on vs counters-off on the head; sequential sets, not interleaved", overhead)
        self.assertIn("| S2/default | status | 1 valid run | 1 valid run |", overhead)
        self.assertNotIn("S1/default", overhead)

    def test_a_build_whose_compiler_helper_was_cleaned_still_builds(self):
        # On Windows a finished build whose linker helper the gate cleaned ends CLEANED_NOT_NATURAL, exit 0;
        # its binary is used, and each build is the gate's reviewed step.
        code, gate, _git_calls, plans, _work, _out = self.compare(build_status="CLEANED_NOT_NATURAL")
        self.assertEqual(code, 0)
        # Both fixture trees define S1/atlas-retry's methods, so both build the plain set with that cfg on.
        plain = gate.PERF_CFG_BUILDS[(("perf_atlas_retry_api",), ())]
        self.assertIs(gate.steps[0], plain["build-head-perf_scenarios"])
        self.assertIs(gate.steps[1], plain["build-base-perf_scenarios"])
        self.assertEqual({plan.binary for plan in plans}, {Path("/base/perf_scenarios"), Path("/head/perf_scenarios")})

    def test_comparison_builds_each_tree_with_its_own_target_and_writes_the_table(self):
        # Head first, one CARGO_TARGET_DIR per ref, the head's harness on both trees, runs alternating.
        code, gate, git_calls, plans, work, out = self.compare()
        self.assertEqual(code, 0)
        adds = [call for call in git_calls if call[1:3] == ("worktree", "add")]
        self.assertEqual(adds, [("git", "worktree", "add", "--detach", str(work / "head"), self.SHAS["HEAD"]),
                                ("git", "worktree", "add", "--detach", str(work / "base"), self.SHAS["main"])])
        self.assertEqual(gate.roots[:2], [work / "head", work / "base"])
        self.assertEqual([environ["CARGO_TARGET_DIR"] for environ in gate.environs[:2]],
                         [str(work / "target-head"), str(work / "target-base")])
        self.assertIn("--release", gate.steps[0].argv)
        self.assertEqual(perf_harness_sources.tree_harness_hash(work / "base"), perf_harness_sources.tree_harness_hash(work / "head"))
        self.assertEqual([plan.side for plan in plans], ["base", "head"])
        self.assertEqual({plan.binary for plan in plans}, {Path("/base/perf_scenarios"), Path("/head/perf_scenarios")})
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S1/default | status | 1 valid run | 1 valid run |", document)
        self.assertIn(self.SHAS["main"], document)
        self.assertIn(perf_harness_sources.tree_harness_hash(work / "head"), document)
        self.assertIn("- Measurement display: Built-in Display, 60 Hz, scale 2", document)

    def test_a_short_comparison_plans_short_runs_and_says_so(self):
        # `--short` reaches every run, and the table's details say the runs were short and how it was invoked.
        _code, _gate, _git_calls, plans, _work, out = self.compare(options=("--short",))
        self.assertTrue(plans and all(plan.short for plan in plans))
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("--runs 1 --short", document)
        self.assertIn("- Run length: short", document)
        _code, _gate, _git_calls, plans, _work, out = self.compare()
        self.assertFalse(any(plan.short for plan in plans))
        self.assertIn("- Run length: full", (out / "comparison.md").read_text(encoding="utf-8"))

    def test_the_details_record_release_profile_overrides(self):
        # A CI run that relaxes the release profile to fit its time budget says so beside the table.
        overrides = {"CARGO_PROFILE_RELEASE_LTO": "off", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16"}
        with mock.patch.dict(os.environ, {}, clear=False):
            for name in list(os.environ):
                if name.startswith("CARGO_PROFILE_RELEASE_"):
                    del os.environ[name]
            _code, _gate, _git_calls, _plans, _work, out = self.compare(environ=overrides)
            document = (out / "comparison.md").read_text(encoding="utf-8")
            self.assertIn("- Release profile overrides (both refs): CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 "
                          "CARGO_PROFILE_RELEASE_LTO=off", document)
            _code, _gate, _git_calls, _plans, _work, out = self.compare()
            self.assertIn("- Release profile overrides (both refs): none",
                          (out / "comparison.md").read_text(encoding="utf-8"))

    def test_each_side_runs_in_its_own_worktree(self):
        # A run's cwd is the worktree that built its binary, so asset_dir() finds that ref's own assets.
        _code, _gate, _git_calls, plans, work, _out = self.compare()
        self.assertEqual({(plan.side, plan.source_root) for plan in plans},
                         {("base", work / "base"), ("head", work / "head")})

    def test_a_tree_without_its_own_assets_cannot_run(self):
        # A base without its assets is blocked with the reason; a head without them fails the comparison.
        code, _gate, _git_calls, plans, _work, out = self.compare(assets=("head",))
        self.assertEqual(code, 0)
        self.assertEqual([plan.side for plan in plans], ["head"])
        self.assertIn("blocked: base cannot run perf_scenarios", (out / "comparison.md").read_text(encoding="utf-8"))
        with self.assertRaises(ValueError):
            self.compare(assets=("base",))

    def test_a_base_that_cannot_build_prints_blocked_with_the_error(self):
        # The head is still measured, and the table names the base's compiler error.
        code, _gate, _git_calls, plans, _work, out = self.compare(base_build="FAIL")
        self.assertEqual(code, 0)
        self.assertEqual([plan.side for plan in plans], ["head"])
        self.assertIn("blocked: base cannot build perf_scenarios: error[E0599]", (out / "comparison.md").read_text(encoding="utf-8"))



class StrictBaseTests(CompareHarness, unittest.TestCase):
    """`--require-base`: a CI comparison whose base cannot build, list or measure fails instead of passing."""

    STRICT = ("--require-base",)

    def test_a_base_build_failure_raises_like_the_heads(self):
        # Under --require-base the base's compiler error ends the comparison before any run.
        with self.assertRaisesRegex(ValueError, "the base cannot build perf_scenarios: error\\[E0599\\]"):
            self.compare(base_build="FAIL", options=self.STRICT)

    def test_a_base_asset_problem_raises_like_the_heads(self):
        # A base whose own assets are missing would measure another font, so the strict comparison stops.
        with self.assertRaisesRegex(ValueError, "the base cannot run perf_scenarios"):
            self.compare(assets=("head",), options=self.STRICT)

    def test_a_base_that_cannot_list_raises(self):
        # Both binaries must load and list their scenarios before anything is measured.
        with self.assertRaisesRegex(ValueError, "--list"):
            self.compare(list_fail=("base",), options=self.STRICT)

    def test_a_base_blocked_at_runtime_fails_and_marks_the_table_incomplete(self):
        # A base retired by an exit-5 run leaves a head-only table: exit 1, and comparison.md says so first.
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=perf_smoke_tests.outcome_of("blocked"), options=self.STRICT)
        self.assertEqual(code, perf_settings.EXIT_FAIL)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertTrue(document.startswith("**Incomplete comparison:**"), document[:200])
        self.assertIn("S1/default timed base", document.split("\n", 1)[0])

    def test_a_base_out_of_retries_fails(self):
        # A base that never produces a valid run is retired by the schedule; strict mode fails on it.
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=perf_smoke_tests.outcome_of("invalid"), options=self.STRICT)
        self.assertEqual(code, perf_settings.EXIT_FAIL)
        self.assertTrue((out / "comparison.md").read_text(encoding="utf-8").startswith("**Incomplete comparison:**"))

    def test_both_sides_blocked_fail_rather_than_report_blocked(self):
        # Without the flag a blocked head exits 3; under it every missing side is a failure.
        blocked = perf_sets.SetResult("S1/default", "timed", perf_metrics.SideRuns(blocked="exit 5"),
                                 perf_metrics.SideRuns(blocked="exit 5"), target_runs=5)
        self.assertEqual(perf_sets.comparison_exit([blocked], require_base=False), perf_settings.EXIT_BLOCKED)
        self.assertEqual(perf_sets.comparison_exit([blocked], require_base=True), perf_settings.EXIT_FAIL)
        self.assertEqual(len(perf_sets.strict_problems([blocked])), 2)

    def test_a_side_short_of_its_runs_is_a_problem(self):
        # Valid runs below the set's target are incomplete even when nothing was marked blocked.
        short = perf_sets.SetResult("S1/default", "timed", perf_metrics.SideRuns(outcomes=[object()] * 5),
                               perf_metrics.SideRuns(outcomes=[object()] * 4), target_runs=5)
        self.assertEqual(perf_sets.strict_problems([short]), ["S1/default timed head: 4 of 5 valid runs"])

    def test_an_empty_result_set_fails(self):
        # A strict comparison that ran no set measured nothing, so it cannot pass; a lenient one is unchanged.
        self.assertEqual(perf_sets.strict_problems([]), ["no scenario set ran"])
        self.assertEqual(perf_sets.comparison_exit([], require_base=True), perf_settings.EXIT_FAIL)
        self.assertEqual(perf_sets.comparison_exit([], require_base=False), perf_settings.EXIT_PASS)

    def test_a_set_without_a_positive_target_fails(self):
        # A set whose target was never set (0) would pass with no runs at all; each side is named.
        unset = perf_sets.SetResult("S1/default", "timed", perf_metrics.SideRuns(), perf_metrics.SideRuns())
        self.assertEqual(perf_sets.strict_problems([unset]), ["S1/default timed base: target 0 is not positive",
                                                         "S1/default timed head: target 0 is not positive"])
        self.assertEqual(perf_sets.comparison_exit([unset], require_base=True), perf_settings.EXIT_FAIL)

    def test_more_valid_runs_than_the_target_fails(self):
        # Counts must be exact: six base runs against a target of five is not the comparison that was planned.
        extra = perf_sets.SetResult("S1/default", "timed", perf_metrics.SideRuns(outcomes=[object()] * 6),
                               perf_metrics.SideRuns(outcomes=[object()] * 5), target_runs=5)
        self.assertEqual(perf_sets.strict_problems([extra]), ["S1/default timed base: 6 of 5 valid runs"])
        self.assertEqual(perf_sets.comparison_exit([extra], require_base=True), perf_settings.EXIT_FAIL)

    def test_the_counters_exception_survives_exact_counts(self):
        # A head-only counters set (base without perf-counters) still passes when the head has exactly its runs.
        head_only = perf_sets.SetResult("S1/default", "counters", perf_metrics.SideRuns(blocked=perf_counter_rows.COUNTERS_HEAD_ONLY),
                                   perf_metrics.SideRuns(outcomes=[object()] * 2), target_runs=2)
        self.assertEqual(perf_sets.strict_problems([head_only]), [])

    def test_a_counters_set_whose_base_has_no_feature_still_passes_with_n_a(self):
        # The one allowed gap: a base without perf-counters runs no counters set, and its cells read n/a.
        code, _gate, _calls, _plans, _work, out = self.compare(
            options=self.STRICT + ("--counters", "--counters-runs", "2"), head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST)
        self.assertEqual(code, perf_settings.EXIT_PASS)
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertNotIn("Incomplete comparison", document)
        self.assertIn("| S1/default | workload | window.attempts (count) | n/a | 5 (5–5) | n/a |", document)
        self.assertIn("--require-base", document)

    def test_without_the_flag_a_blocked_base_still_passes(self):
        # A local comparison stays lenient: the head is measured and the base reads blocked, exit 0.
        code, _gate, _calls, _plans, _work, out = self.compare(base_run=perf_smoke_tests.outcome_of("blocked"))
        self.assertEqual(code, perf_settings.EXIT_PASS)
        self.assertNotIn("Incomplete comparison", (out / "comparison.md").read_text(encoding="utf-8"))
