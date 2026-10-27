"""Coverage comparison tests."""

from __future__ import annotations
import json
import unittest

import coverage_test_fixture


class FloorComparisonTests(unittest.TestCase):
    """A measured crate is held to its floor within the tolerance, never beyond it."""

    def test_drop_beyond_tolerance_fails(self):
        # vt falls from 87.00% to 85.50%: 1.5 pp, more than the 1.0 pp tolerance.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 162)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("DROP", coverage_test_fixture.row(run.stdout, "sonicterm-vt"))
        self.assertIn("-1.50", coverage_test_fixture.row(run.stdout, "sonicterm-vt"))
        self.assertIn("1.50 pp below its floor 87.00% (174/200)", run.stdout)
        self.assertIn("coverage floor FAIL", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        # Lowering a floor is a reviewer's decision, so a drop prints no proposal.
        self.assertNotIn(coverage_test_fixture.floor.PROPOSAL_BEGIN, run.stdout)

    def test_drop_within_tolerance_passes(self):
        # vt falls from 87.00% to 86.50%, inside the tolerance.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 164)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(coverage_test_fixture.row(run.stdout, "sonicterm-vt"), r"\sok\s")
        self.assertIn("-0.50", coverage_test_fixture.row(run.stdout, "sonicterm-vt"))
        self.assertIn("coverage floor PASS: 2 measured crates within 1.0 pp", run.stdout)

    def test_drop_of_exactly_the_tolerance_holds(self):
        # 55% -> 54% is exactly 1.0 pp, but float arithmetic puts this pair past the
        # boundary, so the comparison must use exact fractions to hold here.
        self.assertLess(27 / 50 * 100 - 11 / 20 * 100, -1.0)
        files = {"crates/sonicterm-grid/src/grid.rs": (400, 360), "crates/sonicterm-vt/src/vt.rs": (50, 27)}
        floors = {"sonicterm-grid": (400, 360), "sonicterm-vt": (20, 11)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(floors), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("-1.00", coverage_test_fixture.row(run.stdout, "sonicterm-vt"))

    def test_improvement_never_raises_the_floor(self):
        # A rise is reported, but only a reviewed update moves the floor.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 181)}
        with coverage_test_fixture.workspace(coverage_test_fixture.export(files), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("+8.00", coverage_test_fixture.row(run.stdout, "sonicterm-vt"))


class MissingBaselineTests(unittest.TestCase):
    """A measured crate without a floor fails visibly and is proposed for review."""

    def test_crate_without_baseline_fails_visibly_with_a_proposal(self):
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline({"sonicterm-grid": (400, 360)}), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NO BASELINE", coverage_test_fixture.row(run.stdout, "sonicterm-vt"))
        self.assertIn("sonicterm-vt: measures 87.00% (174/200 lines) but has no baseline entry", run.stdout)
        proposed = coverage_test_fixture.proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-vt"], {"lines": 200, "covered": 174, "percent": 87.0})
        self.assertEqual(proposed["crates"]["sonicterm-grid"], {"lines": 400, "covered": 360, "percent": 90.0})
        # The reason is the reviewer's to write; an unfilled proposal cannot load.
        self.assertEqual(proposed["reason"], "")
        with self.assertRaises(coverage_test_fixture.floor.InputError):
            coverage_test_fixture.floor.parse_baseline(proposed, "proposal")

    def test_initial_baseline_without_floors_fails_and_proposes_every_crate(self):
        # The committed initial baseline carries only host and declarations.
        declared = {"sonicterm-harfbuzz": "declarations only"}
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS + ["sonicterm-harfbuzz"], coverage_test_fixture.baseline({}, declared), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        proposed = coverage_test_fixture.proposal(run.stdout)
        self.assertEqual(sorted(proposed["crates"]), ["sonicterm-grid", "sonicterm-vt"])
        self.assertEqual(proposed["not_measured"], declared)
        self.assertEqual(proposed["host"], {"target": coverage_test_fixture.TARGET, "runner": coverage_test_fixture.RUNNER})
        filled = dict(proposed, reason="initial floors from the first CI run")
        self.assertEqual(coverage_test_fixture.floor.parse_baseline(filled, "filled").crates["sonicterm-vt"], coverage_test_fixture.floor.Entry(200, 174))

    def test_new_measured_crate_fails_while_existing_floors_hold(self):
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-new/src/lib.rs": (50, 40)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS + ["sonicterm-new"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NO BASELINE", coverage_test_fixture.row(run.stdout, "sonicterm-new"))
        self.assertRegex(coverage_test_fixture.row(run.stdout, "sonicterm-grid"), r"\sok\s")
        self.assertRegex(coverage_test_fixture.row(run.stdout, "sonicterm-vt"), r"\sok\s")
        proposed = coverage_test_fixture.proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-new"]["percent"], 80.0)
        # Existing floors are copied verbatim, so the adopted diff only adds the new crate.
        self.assertEqual(proposed["crates"]["sonicterm-vt"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)["crates"]["sonicterm-vt"])


class NotMeasuredTests(unittest.TestCase):
    """A crate without eligible lines is `not measured` with a reason, never 0% or 100%."""

    def test_target_inapplicable_crate_is_not_measured_with_its_reason(self):
        # A crate that cannot build for this host is declared with a reason and passes without a number.
        why = "target-inapplicable here: linked only for non-macOS Unix"
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS + ["sonicterm-fontconfig"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, {"sonicterm-fontconfig": why}), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        line = coverage_test_fixture.row(run.stdout, "sonicterm-fontconfig")
        self.assertIn("declared", line)
        self.assertIn("not measured", line)
        self.assertIn(why, line)
        self.assertNotIn("%", line)
        self.assertIn("1 declared not measured", run.stdout)

    def test_zero_eligible_line_crate_is_not_measured_never_zero_percent(self):
        # Only test files are reported for harfbuzz, so no line is eligible.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-harfbuzz/src/lib_tests.rs": (30, 30),
                 "crates/sonicterm-harfbuzz/tests/shape.rs": (12, 12)}
        members = coverage_test_fixture.MEMBERS + ["sonicterm-harfbuzz"]
        declared = coverage_test_fixture.check(files, members, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, {"sonicterm-harfbuzz": "declarations only"}), coverage_test_fixture.CI_ENV)
        self.assertEqual(declared.status, 0, declared.stdout)
        self.assertIn("not measured", coverage_test_fixture.row(declared.stdout, "sonicterm-harfbuzz"))
        self.assertNotIn("%", coverage_test_fixture.row(declared.stdout, "sonicterm-harfbuzz"))
        undeclared = coverage_test_fixture.check(files, members, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(undeclared.status, 1, undeclared.stdout)
        line = coverage_test_fixture.row(undeclared.stdout, "sonicterm-harfbuzz")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("every reported file is excluded (test: 2)", line)
        self.assertNotIn("%", line)

    def test_files_without_instrumented_lines_are_not_measured(self):
        # A reported file with zero instrumented lines is eligible but yields no percentage.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-fontconfig/src/lib.rs": (0, 0)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS + ["sonicterm-fontconfig"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = coverage_test_fixture.row(run.stdout, "sonicterm-fontconfig")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("1 reported file(s) contain no instrumented line", line)
        self.assertNotIn("%", line)

    def test_unexplained_inventory_member_fails(self):
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS + ["sonicterm-mystery"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = coverage_test_fixture.row(run.stdout, "sonicterm-mystery")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("no reported file", line)
        self.assertNotIn("%", line)
        self.assertIn("no floor, and no not-measured declaration", run.stdout)
        # The proposal names the member with a blank reason a reviewer must write.
        self.assertEqual(coverage_test_fixture.proposal(run.stdout)["not_measured"], {"sonicterm-mystery": ""})

    def test_declared_crate_that_starts_reporting_lines_fails(self):
        # A declaration must not hide a crate that now has lines; it needs a floor instead.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-fontconfig/src/lib.rs": (20, 10)}
        base = coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, {"sonicterm-fontconfig": "declarations only"})
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS + ["sonicterm-fontconfig"], base, coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NOW MEASURED", coverage_test_fixture.row(run.stdout, "sonicterm-fontconfig"))
        proposed = coverage_test_fixture.proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-fontconfig"]["percent"], 50.0)
        self.assertNotIn("sonicterm-fontconfig", proposed["not_measured"])

    def test_declaration_for_a_non_member_fails(self):
        # Declarations reconcile against the inventory, so one naming a removed member fails.
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, {"sonicterm-removed": "declarations only"}), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("sonicterm-removed: declared not measured but is not a workspace member", run.stdout)


class MissingCrateTests(unittest.TestCase):
    """A crate with a floor never leaves the floor silently."""

    def test_previously_measured_crate_missing_from_report_fails(self):
        # A vanished crate's row carries no number; its old floor appears only in the finding.
        run = coverage_test_fixture.check({"crates/sonicterm-grid/src/grid.rs": (400, 360)}, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = coverage_test_fixture.row(run.stdout, "sonicterm-vt")
        self.assertIn("MISSING", line)
        self.assertIn("not measured", line)
        self.assertIn("no reported file", line)
        self.assertNotIn("%", line)
        self.assertIn("has a floor of 87.00% (174/200)", run.stdout)
        self.assertIn("must not leave the floor silently", run.stdout)
        # Removing a floor is a reviewer's decision, so no proposal is printed.
        self.assertNotIn(coverage_test_fixture.floor.PROPOSAL_BEGIN, run.stdout)

    def test_previously_measured_crate_removed_from_workspace_fails(self):
        # Deleting a member does not delete its floor silently.
        run = coverage_test_fixture.check({"crates/sonicterm-grid/src/grid.rs": (400, 360)}, ["sonicterm-grid"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("sonicterm-vt: has a floor of 87.00% but is no longer a workspace member", run.stdout)


class HostIdentityTests(unittest.TestCase):
    """A baseline from another host is never a passing result."""

    def test_runner_mismatch_in_ci_fails_as_not_comparable(self):
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), dict(coverage_test_fixture.CI_ENV, ImageOS="macos15"))
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("coverage floor FAIL: not comparable", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        # The replacement proposal is keyed to the host that measured it.
        self.assertEqual(coverage_test_fixture.proposal(run.stdout)["host"], {"target": coverage_test_fixture.TARGET, "runner": "macos15/ARM64"})

    def test_target_mismatch_in_ci_fails_as_not_comparable(self):
        # Another target triple is another host, even on the same runner image.
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV, target="x86_64-apple-darwin")
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("not comparable", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertEqual(coverage_test_fixture.proposal(run.stdout)["host"]["target"], "x86_64-apple-darwin")

    def test_local_run_is_informational_and_never_passes(self):
        # Outside CI the floor decides nothing, even when every number matches.
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("informational only; no verdict outside CI", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertNotIn("FAIL", run.stdout)
        self.assertNotIn(coverage_test_fixture.floor.PROPOSAL_BEGIN, run.stdout)

    def test_local_run_reports_deltas_without_failing(self):
        # A drop and an unexplained member are shown locally but decide nothing.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 150)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS + ["sonicterm-mystery"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("delta (info)", run.stdout)
        self.assertIn("DROP", coverage_test_fixture.row(run.stdout, "sonicterm-vt"))
        self.assertIn("CI on the baseline host would fail on each", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertNotIn("FAIL", run.stdout)
        # Outside CI there is no artifact to name, so the DROP guidance is not printed.
        self.assertNotIn("A DROP is never proposed", run.stdout)

    def test_baseline_recorded_as_local_still_gives_no_local_verdict(self):
        # Matching a hand-written `local` identity must not turn a local run into a pass.
        run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, runner="local"), coverage_test_fixture.LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertNotIn("PASS", run.stdout)

    def test_runner_identity_comes_from_the_ci_runner_image(self):
        # The identity is ImageOS/RUNNER_ARCH in CI and `local` everywhere else.
        self.assertEqual(coverage_test_fixture.floor.runner_identity(coverage_test_fixture.CI_ENV), "macos14/ARM64")
        self.assertEqual(coverage_test_fixture.floor.runner_identity({}), "local")
        self.assertEqual(coverage_test_fixture.floor.runner_identity({"GITHUB_ACTIONS": "false", "ImageOS": "macos14"}), "local")
        self.assertEqual(coverage_test_fixture.floor.runner_identity({"GITHUB_ACTIONS": "true"}), "unknown-image/unknown-arch")


class UpdateBaselineTests(unittest.TestCase):
    """The baseline changes only through an explicit, reasoned update."""

    def test_reviewed_update_writes_a_baseline_that_then_passes(self):
        # A verified update writes exact counts that the next CI check accepts; its reason names the run.
        declared = {"sonicterm-harfbuzz": "declarations only"}
        members = coverage_test_fixture.MEMBERS + ["sonicterm-harfbuzz"]
        with coverage_test_fixture.retained_run(members=members, base=coverage_test_fixture.baseline({}, declared), state="done", exit_status=0) as paths:
            checks = json.loads(paths["provenance"].read_text(encoding="utf-8"))["checks"]
            written = coverage_test_fixture.update(paths, "--reason", "initial floors from CI run 1", "--provenance",
                             str(paths["provenance"]))
            stored = coverage_test_fixture.floor.load_baseline(str(paths["baseline"]))
            checked = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
        self.assertEqual(checks, {"subset-gate": "exit status 0", "floor": "exit status 0"})
        self.assertEqual(written.status, 0, written.stderr)
        self.assertEqual(stored.crates, {"sonicterm-grid": coverage_test_fixture.floor.Entry(400, 360),
                                         "sonicterm-vt": coverage_test_fixture.floor.Entry(200, 174)})
        self.assertEqual(stored.not_measured, declared)
        self.assertTrue(stored.reason.startswith(
            "initial floors from CI run 1. Measured by https://github.com/D0n9X1n/SonicTerm/actions/runs/"
            "36087392875 attempt 1, job linux-coverage"), stored.reason)
        for fact in (coverage_test_fixture.ARTIFACT, "macos14/ARM64 (image 20260915.0150)", "rustc 1.98.1 (48a229cea 2026-09-01)",
                     "cargo-llvm-cov 0.9.0"):
            self.assertIn(fact, stored.reason)
        self.assertIn("the verified provenance record", written.stdout)
        self.assertEqual((stored.target, stored.runner), (coverage_test_fixture.TARGET, coverage_test_fixture.RUNNER))
        self.assertIn("+ sonicterm-vt: 87.00% (174/200)", written.stdout)
        self.assertEqual(checked.status, 0, checked.stdout)
        self.assertIn("coverage floor PASS", checked.stdout)

    def test_update_without_a_reason_is_refused_and_writes_nothing(self):
        # A missing or blank reason leaves the committed floor untouched.
        for extra in ((), ("--reason", ""), ("--reason", "   ")):
            with self.subTest(extra=extra):
                with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline({})) as paths:
                    before = paths["baseline"].read_bytes()
                    run = coverage_test_fixture.invoke(paths, coverage_test_fixture.LOCAL_ENV, "--update-baseline", "--runner", coverage_test_fixture.RUNNER, *extra)
                    self.assertEqual(paths["baseline"].read_bytes(), before)
                self.assertEqual(run.status, 2)
                self.assertIn("--reason", run.stderr)

    def test_update_without_a_runner_is_refused(self):
        # The host comes from a verified record or an explicit local runner, never from the environment.
        with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline({})) as paths:
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV, "--update-baseline", "--reason", "floors")
        self.assertEqual(run.status, 2)
        self.assertIn("--runner", run.stderr)
        self.assertIn("--provenance", run.stderr)

    def test_update_refuses_an_unmeasured_undeclared_member(self):
        # The tool cannot invent why a crate is unmeasured; a local baseline needs no provenance to show it.
        local = coverage_test_fixture.baseline({}, runner="local")
        with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS + ["sonicterm-mystery"]), local) as paths:
            before = paths["baseline"].read_bytes()
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", "local")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("not measured and not declared: sonicterm-mystery (no reported file)", run.stderr)

    def test_partial_update_changes_only_the_named_crate(self):
        # `--crate` rewrites one floor and keeps every other entry verbatim; a local baseline needs no record.
        files = {"crates/sonicterm-grid/src/grid.rs": (400, 300), "crates/sonicterm-vt/src/vt.rs": (200, 150)}
        with coverage_test_fixture.workspace(coverage_test_fixture.export(files), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, runner="local")) as paths:
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.LOCAL_ENV, "--update-baseline", "--reason", "vt drops dead parser paths",
                         "--runner", "local", "--crate", "sonicterm-vt")
            stored = coverage_test_fixture.floor.load_baseline(str(paths["baseline"]))
        self.assertEqual(run.status, 0, run.stderr)
        self.assertEqual(stored.crates["sonicterm-vt"], coverage_test_fixture.floor.Entry(200, 150))
        self.assertEqual(stored.crates["sonicterm-grid"], coverage_test_fixture.floor.Entry(400, 360))

    def test_partial_update_refuses_another_host(self):
        # `--crate` keeps the same-host rule: a verified record from another runner image cannot change one floor.
        with coverage_test_fixture.retained_run(env=dict(coverage_test_fixture.RUN_ENV, ImageOS="macos15")) as paths:
            before = paths["baseline"].read_bytes()
            run = coverage_test_fixture.update(paths, "--reason", "floors", "--provenance", str(paths["provenance"]),
                         "--crate", "sonicterm-vt")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("cannot mix hosts: the baseline is for aarch64-apple-darwin on macos14/ARM64, "
                      "this report is aarch64-apple-darwin on macos15/ARM64", run.stderr)

    def test_check_never_rewrites_the_baseline(self):
        # Even a failing run that prints a proposal leaves the baseline untouched.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 100), "crates/sonicterm-new/src/lib.rs": (10, 1)}
        with coverage_test_fixture.workspace(coverage_test_fixture.export(files), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS + ["sonicterm-new"]), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 1)
        self.assertIn(coverage_test_fixture.floor.PROPOSAL_BEGIN, run.stdout)

    def test_update_only_options_are_rejected_in_check_mode(self):
        # Check mode reads the runner from CI, so a stated runner cannot fake a match.
        for extra in (("--reason", "x"), ("--runner", coverage_test_fixture.RUNNER), ("--crate", "sonicterm-vt"),
                      ("--provenance", "record.json")):
            with self.subTest(extra=extra):
                run = coverage_test_fixture.check(coverage_test_fixture.FILES, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.CI_ENV, *extra)
                self.assertEqual(run.status, 2)
                self.assertIn("only valid with --update-baseline", run.stderr)
