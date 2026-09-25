#!/usr/bin/env python3
"""Contract tests for scripts/coverage-floor.py.

The floor's dangerous failure mode is a quiet pass: a crate that vanishes from
the report, a baseline measured on another host, or a crate with no eligible
line printed as 0% or 100%. Every rule is asserted against fixture JSON built
in code and written to temporary files, so the flat scripts/ directory needs no
fixture folder. The committed baseline and the gate script are checked against
the repository itself.

Run directly:  python3 scripts/coverage-floor_tests.py
Or discovered:  python3 -m unittest coverage-floor_tests   (from scripts/)
"""

from __future__ import annotations

from contextlib import contextmanager
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent
_FLOOR_PATH = _HERE / "coverage-floor.py"

_spec = importlib.util.spec_from_file_location("coverage_floor", _FLOOR_PATH)
floor = importlib.util.module_from_spec(_spec)
# Dataclasses resolve string annotations through sys.modules, so register first.
sys.modules[_spec.name] = floor
_spec.loader.exec_module(floor)

TARGET = "aarch64-apple-darwin"
RUNNER = "macos14/ARM64"
CI_ENV = {"GITHUB_ACTIONS": "true", "ImageOS": "macos14", "RUNNER_ARCH": "ARM64"}
LOCAL_ENV: dict = {}
# A path that exists on no host, so symlink resolution cannot rewrite it.
ROOT = "/nonexistent-sonicterm-fixture/checkout"

MEMBERS = ["sonicterm-grid", "sonicterm-vt"]
FILES = {
    "crates/sonicterm-grid/src/grid.rs": (400, 360),
    "crates/sonicterm-vt/src/vt.rs": (190, 165),
    "crates/sonicterm-vt/src/vt/staging.rs": (10, 9),
}
# grid 360/400 = 90.00%; vt (165 + 9)/(190 + 10) = 174/200 = 87.00%.
FLOORS = {"sonicterm-grid": (400, 360), "sonicterm-vt": (200, 174)}


def export(files, root=ROOT):
    """Build an llvm-cov JSON summary export from {path: (lines, covered)}."""
    return {
        "type": floor.LLVM_EXPORT_TYPE,
        "version": "3.1.0",
        "cargo_llvm_cov": {"version": "0.9.0", "manifest_path": f"{root}/Cargo.toml"},
        "data": [{
            "files": [
                {
                    "filename": path if path.startswith("/") else f"{root}/{path}",
                    "summary": {"lines": {"count": lines, "covered": covered,
                                          "percent": 100.0 * covered / lines if lines else 0.0}},
                }
                for path, (lines, covered) in files.items()
            ],
            "totals": {},
        }],
    }


def metadata(names, root=ROOT, build_scripts=()):
    """Build `cargo metadata --no-deps` output for members under crates/<name>."""
    packages = []
    for name in names:
        targets = [{"kind": ["lib"], "name": name, "src_path": f"{root}/crates/{name}/src/lib.rs"}]
        if name in build_scripts:
            targets.append({"kind": ["custom-build"], "name": "build-script-build",
                            "src_path": f"{root}/crates/{name}/build.rs"})
        packages.append({"id": f"path+file://{root}/crates/{name}#{name}@1.0.0", "name": name,
                         "manifest_path": f"{root}/crates/{name}/Cargo.toml", "targets": targets})
    return {
        "packages": packages,
        "workspace_members": [package["id"] for package in packages],
        "workspace_root": root,
        "target_directory": f"{root}/target/rust-logic-coverage",
        "version": 1,
    }


def baseline(crates=None, not_measured=None, target=TARGET, runner=RUNNER, tolerance=1.0, reason="fixture floor"):
    """Build a baseline document from {crate: (lines, covered)}."""
    return {
        "schema": floor.SCHEMA,
        "host": {"target": target, "runner": runner},
        "tolerance_pp": tolerance,
        "reason": reason,
        "crates": {name: {"lines": lines, "covered": covered, "percent": round(covered * 100 / lines, 2)}
                   for name, (lines, covered) in (crates or {}).items()},
        "not_measured": dict(not_measured or {}),
    }


class Run:
    """Captured result of one coverage-floor invocation."""

    def __init__(self, status, stdout, stderr):
        self.status, self.stdout, self.stderr = status, stdout, stderr


@contextmanager
def workspace(report, meta, base):
    """Write the input documents to a temporary directory and yield their paths; a str is written verbatim."""
    with tempfile.TemporaryDirectory() as directory:
        paths = {}
        for key, document in (("report", report), ("metadata", meta), ("baseline", base)):
            path = Path(directory) / f"{key}.json"
            if document is not None:
                text = document if isinstance(document, str) else json.dumps(document, indent=2) + "\n"
                path.write_text(text, encoding="utf-8")
            paths[key] = path
        yield paths


def invoke(paths, env, *extra, target=TARGET):
    """Run the floor in-process with an explicit environment and capture its output."""
    stdout, stderr = io.StringIO(), io.StringIO()
    argv = ["--report", str(paths["report"]), "--metadata", str(paths["metadata"]),
            "--baseline", str(paths["baseline"]), "--target", target,
            "--toolchain", "rustc 1.98.1 (fixture)", *extra]
    status = floor.main(argv, env=env, stdout=stdout, stderr=stderr)
    return Run(status, stdout.getvalue(), stderr.getvalue())


def check(files, members, base, env, *extra, target=TARGET, build_scripts=()):
    """Check one fixture report against one fixture baseline."""
    with workspace(export(files), metadata(members, build_scripts=build_scripts), base) as paths:
        return invoke(paths, env, *extra, target=target)


def row(output, crate):
    """Return the per-crate table row for `crate`."""
    for line in output.splitlines():
        if line.startswith(crate + " "):
            return line
    raise AssertionError(f"no table row for {crate} in:\n{output}")


def proposal(output):
    """Parse the proposed baseline printed between the markers."""
    begin = output.index(floor.PROPOSAL_BEGIN) + len(floor.PROPOSAL_BEGIN)
    return json.loads(output[begin:output.index(floor.PROPOSAL_END)])


def clean_env():
    """Return this process's environment without any CI runner identity."""
    return {key: value for key, value in os.environ.items()
            if not key.startswith("GITHUB_") and key not in ("ImageOS", "RUNNER_ARCH")}


# Markers generators write at the top of their output; scanning only the first
# lines keeps prose that mentions a generator from matching.
GENERATOR_MARKER = re.compile(r"automatically generated by|@generated|do not edit", re.IGNORECASE)
MARKER_LINES = 5


def first_party_rust_sources():
    """Return the repository's first-party .rs paths, relative and POSIX-style."""
    if (_REPO / ".git").exists():
        listed = subprocess.run(["git", "-C", str(_REPO), "ls-files", "--", "*.rs"], stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, encoding="utf-8", check=True, timeout=60)
        paths = listed.stdout.splitlines()
    else:
        # A `git archive` export has no index; every file in it is tracked content.
        paths = [path.relative_to(_REPO).as_posix() for path in _REPO.rglob("*.rs")]
    vendored = tuple(root + "/" for root in floor.VENDORED_ROOTS)
    return sorted(path for path in paths if not path.startswith(vendored) and not path.startswith("target/"))


def has_generator_marker(path):
    """Whether the first lines of the repository file `path` carry a generator marker."""
    with open(_REPO / path, encoding="utf-8", errors="replace") as handle:
        head = "".join(line for _, line in zip(range(MARKER_LINES), handle))
    return bool(GENERATOR_MARKER.search(head))


class FloorComparisonTests(unittest.TestCase):
    """A measured crate is held to its floor within the tolerance, never beyond it."""

    def test_drop_beyond_tolerance_fails(self):
        # vt falls from 87.00% to 85.50%: 1.5 pp, more than the 1.0 pp tolerance.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 162)}
        run = check(files, MEMBERS, baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("DROP", row(run.stdout, "sonicterm-vt"))
        self.assertIn("-1.50", row(run.stdout, "sonicterm-vt"))
        self.assertIn("1.50 pp below its floor 87.00% (174/200)", run.stdout)
        self.assertIn("coverage floor FAIL", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        # Lowering a floor is a reviewer's decision, so a drop prints no proposal.
        self.assertNotIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_drop_within_tolerance_passes(self):
        # vt falls from 87.00% to 86.50%, inside the tolerance.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 164)}
        run = check(files, MEMBERS, baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(row(run.stdout, "sonicterm-vt"), r"\sok\s")
        self.assertIn("-0.50", row(run.stdout, "sonicterm-vt"))
        self.assertIn("coverage floor PASS: 2 measured crates within 1.0 pp", run.stdout)

    def test_drop_of_exactly_the_tolerance_holds(self):
        # 55% -> 54% is exactly 1.0 pp, but float arithmetic puts this pair past the
        # boundary, so the comparison must use exact fractions to hold here.
        self.assertLess(27 / 50 * 100 - 11 / 20 * 100, -1.0)
        files = {"crates/sonicterm-grid/src/grid.rs": (400, 360), "crates/sonicterm-vt/src/vt.rs": (50, 27)}
        floors = {"sonicterm-grid": (400, 360), "sonicterm-vt": (20, 11)}
        run = check(files, MEMBERS, baseline(floors), CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("-1.00", row(run.stdout, "sonicterm-vt"))

    def test_improvement_never_raises_the_floor(self):
        # A rise is reported, but only a reviewed update moves the floor.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 181)}
        with workspace(export(files), metadata(MEMBERS), baseline(FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, CI_ENV)
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("+8.00", row(run.stdout, "sonicterm-vt"))


class MissingBaselineTests(unittest.TestCase):
    """A measured crate without a floor fails visibly and is proposed for review."""

    def test_crate_without_baseline_fails_visibly_with_a_proposal(self):
        run = check(FILES, MEMBERS, baseline({"sonicterm-grid": (400, 360)}), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NO BASELINE", row(run.stdout, "sonicterm-vt"))
        self.assertIn("sonicterm-vt: measures 87.00% (174/200 lines) but has no baseline entry", run.stdout)
        proposed = proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-vt"], {"lines": 200, "covered": 174, "percent": 87.0})
        self.assertEqual(proposed["crates"]["sonicterm-grid"], {"lines": 400, "covered": 360, "percent": 90.0})
        # The reason is the reviewer's to write; an unfilled proposal cannot load.
        self.assertEqual(proposed["reason"], "")
        with self.assertRaises(floor.InputError):
            floor.parse_baseline(proposed, "proposal")

    def test_initial_baseline_without_floors_fails_and_proposes_every_crate(self):
        # The committed initial baseline carries only host and declarations.
        declared = {"sonicterm-harfbuzz": "declarations only"}
        run = check(FILES, MEMBERS + ["sonicterm-harfbuzz"], baseline({}, declared), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        proposed = proposal(run.stdout)
        self.assertEqual(sorted(proposed["crates"]), ["sonicterm-grid", "sonicterm-vt"])
        self.assertEqual(proposed["not_measured"], declared)
        self.assertEqual(proposed["host"], {"target": TARGET, "runner": RUNNER})
        filled = dict(proposed, reason="initial floors from the first CI run")
        self.assertEqual(floor.parse_baseline(filled, "filled").crates["sonicterm-vt"], floor.Entry(200, 174))

    def test_new_measured_crate_fails_while_existing_floors_hold(self):
        files = {**FILES, "crates/sonicterm-new/src/lib.rs": (50, 40)}
        run = check(files, MEMBERS + ["sonicterm-new"], baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NO BASELINE", row(run.stdout, "sonicterm-new"))
        self.assertRegex(row(run.stdout, "sonicterm-grid"), r"\sok\s")
        self.assertRegex(row(run.stdout, "sonicterm-vt"), r"\sok\s")
        proposed = proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-new"]["percent"], 80.0)
        # Existing floors are copied verbatim, so the adopted diff only adds the new crate.
        self.assertEqual(proposed["crates"]["sonicterm-vt"], baseline(FLOORS)["crates"]["sonicterm-vt"])


class NotMeasuredTests(unittest.TestCase):
    """A crate without eligible lines is `not measured` with a reason, never 0% or 100%."""

    def test_target_inapplicable_crate_is_not_measured_with_its_reason(self):
        # A crate that cannot build for this host is declared with a reason and passes without a number.
        why = "target-inapplicable here: linked only for non-macOS Unix"
        run = check(FILES, MEMBERS + ["sonicterm-fontconfig"], baseline(FLOORS, {"sonicterm-fontconfig": why}), CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        line = row(run.stdout, "sonicterm-fontconfig")
        self.assertIn("declared", line)
        self.assertIn("not measured", line)
        self.assertIn(why, line)
        self.assertNotIn("%", line)
        self.assertIn("1 declared not measured", run.stdout)

    def test_zero_eligible_line_crate_is_not_measured_never_zero_percent(self):
        # Only test files are reported for harfbuzz, so no line is eligible.
        files = {**FILES, "crates/sonicterm-harfbuzz/src/lib_tests.rs": (30, 30),
                 "crates/sonicterm-harfbuzz/tests/shape.rs": (12, 12)}
        members = MEMBERS + ["sonicterm-harfbuzz"]
        declared = check(files, members, baseline(FLOORS, {"sonicterm-harfbuzz": "declarations only"}), CI_ENV)
        self.assertEqual(declared.status, 0, declared.stdout)
        self.assertIn("not measured", row(declared.stdout, "sonicterm-harfbuzz"))
        self.assertNotIn("%", row(declared.stdout, "sonicterm-harfbuzz"))
        undeclared = check(files, members, baseline(FLOORS), CI_ENV)
        self.assertEqual(undeclared.status, 1, undeclared.stdout)
        line = row(undeclared.stdout, "sonicterm-harfbuzz")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("every reported file is excluded (test: 2)", line)
        self.assertNotIn("%", line)

    def test_files_without_instrumented_lines_are_not_measured(self):
        # A reported file with zero instrumented lines is eligible but yields no percentage.
        files = {**FILES, "crates/sonicterm-fontconfig/src/lib.rs": (0, 0)}
        run = check(files, MEMBERS + ["sonicterm-fontconfig"], baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = row(run.stdout, "sonicterm-fontconfig")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("1 reported file(s) contain no instrumented line", line)
        self.assertNotIn("%", line)

    def test_unexplained_inventory_member_fails(self):
        run = check(FILES, MEMBERS + ["sonicterm-mystery"], baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = row(run.stdout, "sonicterm-mystery")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("no reported file", line)
        self.assertNotIn("%", line)
        self.assertIn("no floor, and no not-measured declaration", run.stdout)
        # The proposal names the member with a blank reason a reviewer must write.
        self.assertEqual(proposal(run.stdout)["not_measured"], {"sonicterm-mystery": ""})

    def test_declared_crate_that_starts_reporting_lines_fails(self):
        # A declaration must not hide a crate that now has lines; it needs a floor instead.
        files = {**FILES, "crates/sonicterm-fontconfig/src/lib.rs": (20, 10)}
        base = baseline(FLOORS, {"sonicterm-fontconfig": "declarations only"})
        run = check(files, MEMBERS + ["sonicterm-fontconfig"], base, CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NOW MEASURED", row(run.stdout, "sonicterm-fontconfig"))
        proposed = proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-fontconfig"]["percent"], 50.0)
        self.assertNotIn("sonicterm-fontconfig", proposed["not_measured"])

    def test_declaration_for_a_non_member_fails(self):
        # Declarations reconcile against the inventory, so one naming a removed member fails.
        run = check(FILES, MEMBERS, baseline(FLOORS, {"sonicterm-removed": "declarations only"}), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("sonicterm-removed: declared not measured but is not a workspace member", run.stdout)


class MissingCrateTests(unittest.TestCase):
    """A crate with a floor never leaves the floor silently."""

    def test_previously_measured_crate_missing_from_report_fails(self):
        # A vanished crate's row carries no number; its old floor appears only in the finding.
        run = check({"crates/sonicterm-grid/src/grid.rs": (400, 360)}, MEMBERS, baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = row(run.stdout, "sonicterm-vt")
        self.assertIn("MISSING", line)
        self.assertIn("not measured", line)
        self.assertIn("no reported file", line)
        self.assertNotIn("%", line)
        self.assertIn("has a floor of 87.00% (174/200)", run.stdout)
        self.assertIn("must not leave the floor silently", run.stdout)
        # Removing a floor is a reviewer's decision, so no proposal is printed.
        self.assertNotIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_previously_measured_crate_removed_from_workspace_fails(self):
        # Deleting a member does not delete its floor silently.
        run = check({"crates/sonicterm-grid/src/grid.rs": (400, 360)}, ["sonicterm-grid"], baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("sonicterm-vt: has a floor of 87.00% but is no longer a workspace member", run.stdout)


class HostIdentityTests(unittest.TestCase):
    """A baseline from another host is never a passing result."""

    def test_runner_mismatch_in_ci_fails_as_not_comparable(self):
        run = check(FILES, MEMBERS, baseline(FLOORS), dict(CI_ENV, ImageOS="macos15"))
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("coverage floor FAIL: not comparable", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        # The replacement proposal is keyed to the host that measured it.
        self.assertEqual(proposal(run.stdout)["host"], {"target": TARGET, "runner": "macos15/ARM64"})

    def test_target_mismatch_in_ci_fails_as_not_comparable(self):
        # Another target triple is another host, even on the same runner image.
        run = check(FILES, MEMBERS, baseline(FLOORS), CI_ENV, target="x86_64-apple-darwin")
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("not comparable", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertEqual(proposal(run.stdout)["host"]["target"], "x86_64-apple-darwin")

    def test_local_run_is_informational_and_never_passes(self):
        # Outside CI the floor decides nothing, even when every number matches.
        run = check(FILES, MEMBERS, baseline(FLOORS), LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("informational only; no verdict outside CI", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertNotIn("FAIL", run.stdout)
        self.assertNotIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_local_run_reports_deltas_without_failing(self):
        # A drop and an unexplained member are shown locally but decide nothing.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 150)}
        run = check(files, MEMBERS + ["sonicterm-mystery"], baseline(FLOORS), LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("delta (info)", run.stdout)
        self.assertIn("DROP", row(run.stdout, "sonicterm-vt"))
        self.assertIn("CI on the baseline host would fail on each", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertNotIn("FAIL", run.stdout)

    def test_baseline_recorded_as_local_still_gives_no_local_verdict(self):
        # Matching a hand-written `local` identity must not turn a local run into a pass.
        run = check(FILES, MEMBERS, baseline(FLOORS, runner="local"), LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertNotIn("PASS", run.stdout)

    def test_runner_identity_comes_from_the_ci_runner_image(self):
        # The identity is ImageOS/RUNNER_ARCH in CI and `local` everywhere else.
        self.assertEqual(floor.runner_identity(CI_ENV), "macos14/ARM64")
        self.assertEqual(floor.runner_identity({}), "local")
        self.assertEqual(floor.runner_identity({"GITHUB_ACTIONS": "false", "ImageOS": "macos14"}), "local")
        self.assertEqual(floor.runner_identity({"GITHUB_ACTIONS": "true"}), "unknown-image/unknown-arch")


class UpdateBaselineTests(unittest.TestCase):
    """The baseline changes only through an explicit, reasoned update."""

    def test_reviewed_update_writes_a_baseline_that_then_passes(self):
        # An update with a reason writes exact counts that the next CI check accepts.
        declared = {"sonicterm-harfbuzz": "declarations only"}
        with workspace(export(FILES), metadata(MEMBERS + ["sonicterm-harfbuzz"]), baseline({}, declared)) as paths:
            written = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "initial floors from CI run 1",
                             "--runner", RUNNER)
            stored = floor.load_baseline(str(paths["baseline"]))
            checked = invoke(paths, CI_ENV)
        self.assertEqual(written.status, 0, written.stderr)
        self.assertEqual(stored.crates, {"sonicterm-grid": floor.Entry(400, 360),
                                         "sonicterm-vt": floor.Entry(200, 174)})
        self.assertEqual(stored.not_measured, declared)
        self.assertEqual(stored.reason, "initial floors from CI run 1")
        self.assertEqual((stored.target, stored.runner), (TARGET, RUNNER))
        self.assertIn("+ sonicterm-vt: 87.00% (174/200)", written.stdout)
        self.assertEqual(checked.status, 0, checked.stdout)
        self.assertIn("coverage floor PASS", checked.stdout)

    def test_update_without_a_reason_is_refused_and_writes_nothing(self):
        # A missing or blank reason leaves the committed floor untouched.
        for extra in ((), ("--reason", ""), ("--reason", "   ")):
            with self.subTest(extra=extra):
                with workspace(export(FILES), metadata(MEMBERS), baseline({})) as paths:
                    before = paths["baseline"].read_bytes()
                    run = invoke(paths, LOCAL_ENV, "--update-baseline", "--runner", RUNNER, *extra)
                    self.assertEqual(paths["baseline"].read_bytes(), before)
                self.assertEqual(run.status, 2)
                self.assertIn("--reason", run.stderr)

    def test_update_without_a_runner_is_refused(self):
        # The host identity is stated explicitly, never inferred from the environment.
        with workspace(export(FILES), metadata(MEMBERS), baseline({})) as paths:
            run = invoke(paths, CI_ENV, "--update-baseline", "--reason", "floors")
        self.assertEqual(run.status, 2)
        self.assertIn("--runner", run.stderr)

    def test_update_refuses_an_unmeasured_undeclared_member(self):
        # The tool cannot invent why a crate is unmeasured.
        with workspace(export(FILES), metadata(MEMBERS + ["sonicterm-mystery"]), baseline({})) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", RUNNER)
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("not measured and not declared: sonicterm-mystery (no reported file)", run.stderr)

    def test_partial_update_changes_only_the_named_crate(self):
        # `--crate` rewrites one floor and keeps every other entry verbatim.
        files = {"crates/sonicterm-grid/src/grid.rs": (400, 300), "crates/sonicterm-vt/src/vt.rs": (200, 150)}
        with workspace(export(files), metadata(MEMBERS), baseline(FLOORS)) as paths:
            run = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "vt drops dead parser paths",
                         "--runner", RUNNER, "--crate", "sonicterm-vt")
            stored = floor.load_baseline(str(paths["baseline"]))
        self.assertEqual(run.status, 0, run.stderr)
        self.assertEqual(stored.crates["sonicterm-vt"], floor.Entry(200, 150))
        self.assertEqual(stored.crates["sonicterm-grid"], floor.Entry(400, 360))

    def test_partial_update_refuses_another_host(self):
        # One file never mixes floors measured on two hosts.
        with workspace(export(FILES), metadata(MEMBERS), baseline(FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", "macos15/ARM64",
                         "--crate", "sonicterm-vt")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("cannot mix hosts", run.stderr)

    def test_check_never_rewrites_the_baseline(self):
        # Even a failing run that prints a proposal leaves the baseline untouched.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 100), "crates/sonicterm-new/src/lib.rs": (10, 1)}
        with workspace(export(files), metadata(MEMBERS + ["sonicterm-new"]), baseline(FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, CI_ENV)
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 1)
        self.assertIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_update_only_options_are_rejected_in_check_mode(self):
        # Check mode reads the runner from CI, so a stated runner cannot fake a match.
        for extra in (("--reason", "x"), ("--runner", RUNNER), ("--crate", "sonicterm-vt")):
            with self.subTest(extra=extra):
                run = check(FILES, MEMBERS, baseline(FLOORS), CI_ENV, *extra)
                self.assertEqual(run.status, 2)
                self.assertIn("only valid with --update-baseline", run.stderr)


class ClassificationTests(unittest.TestCase):
    """Report files are attributed consistently and every exclusion is counted."""

    def test_paths_are_normalized_before_classification(self):
        # llvm-cov reports `src/../build_config.rs` for a `#[path]` module.
        files = {
            "crates/sonicterm-freetype/src/../build_config.rs": (58, 56),
            "crates/sonicterm-freetype/build.rs": (40, 0),
            f"{ROOT}/crates\\sonicterm-vt\\src\\vt.rs": (200, 174),
        }
        base = baseline({"sonicterm-freetype": (58, 56), "sonicterm-vt": (200, 174)})
        run = check(files, ["sonicterm-freetype", "sonicterm-vt"], base, CI_ENV, build_scripts=("sonicterm-freetype",))
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(row(run.stdout, "sonicterm-freetype"), r"\s58\s+56\s")
        self.assertRegex(run.stdout, r"build-script\s+files=1\s+lines=40\s")

    def test_exclusions_are_reported_with_counts_and_reasons(self):
        files = {
            **FILES,
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
        base = baseline(FLOORS, {"sonicterm-harfbuzz": "declarations only"})
        run = check(files, MEMBERS + ["sonicterm-harfbuzz"], base, CI_ENV, build_scripts=("sonicterm-vt",))
        self.assertEqual(run.status, 0, run.stdout)
        # Only vt's two src files count toward its row.
        self.assertRegex(row(run.stdout, "sonicterm-vt"), r"\s200\s+174\s+87\.00%")
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
        base = baseline({"sonicterm-freetype": (76, 74)}, {"sonicterm-harfbuzz": "generated only"})
        run = check(files, members, base, CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(row(run.stdout, "sonicterm-freetype"), r"\s76\s+74\s+97\.37%")
        self.assertRegex(run.stdout, r"(?m)^  generated\s+files=3\s+lines=36\s")
        for path in ("crates/sonicterm-freetype/src/lib.rs", "crates/sonicterm-freetype/src/types.rs",
                     "crates/sonicterm-harfbuzz/src/lib.rs"):
            self.assertIn("      " + path, run.stdout)
        self.assertNotIn("%", row(run.stdout, "sonicterm-harfbuzz"))
        undeclared = check(files, members, baseline({"sonicterm-freetype": (76, 74)}), CI_ENV)
        self.assertIn("every reported file is excluded (generated: 1)",
                      row(undeclared.stdout, "sonicterm-harfbuzz"))

    def test_change_confined_to_generated_bindings_does_not_move_the_crate(self):
        # Counting these generated lines would read two added uncovered lines as a 1.51 pp drop (74/100 against 74/98).
        authored = {"crates/sonicterm-freetype/src/fixed_point.rs": (18, 18),
                    "crates/sonicterm-freetype/src/../build_config.rs": (58, 56)}
        base = baseline({"sonicterm-freetype": (76, 74)})
        for generated_lines in (22, 24):
            with self.subTest(generated_lines=generated_lines):
                files = {**authored, "crates/sonicterm-freetype/src/lib.rs": (generated_lines, 0)}
                run = check(files, ["sonicterm-freetype"], base, CI_ENV)
                self.assertEqual(run.status, 0, run.stdout)
                self.assertRegex(row(run.stdout, "sonicterm-freetype"),
                                 r"\s76\s+74\s+97\.37%\s+97\.37%\s+\+0\.00")
                self.assertNotIn("DROP", run.stdout)
        # Counted as authored code, the same probe would cross the tolerance: 74/98 against 74/100.
        self.assertGreater(74 * 100 / 98 - 74 * 100 / 100, 1.0)

    def test_source_root_maps_a_report_from_another_checkout(self):
        other = "/private/tmp/sonicterm-fixture-other-checkout"
        with workspace(export(FILES, root=other), metadata(MEMBERS), baseline(FLOORS)) as paths:
            unmapped = invoke(paths, CI_ENV)
            mapped = invoke(paths, CI_ENV, "--source-root", other)
        # Unmapped, every file is outside the repository and both floors go missing.
        self.assertEqual(unmapped.status, 1, unmapped.stdout)
        self.assertRegex(unmapped.stdout, r"outside-repository\s+files=3\s")
        self.assertIn("MISSING", row(unmapped.stdout, "sonicterm-vt"))
        self.assertEqual(mapped.status, 0, mapped.stdout)

    def test_platform_crates_are_labelled_as_compiled_on_the_host(self):
        # Single-platform crates measured on another OS are labelled, never shown as native coverage.
        files = {"crates/sonicterm-mac/src/main.rs": (100, 10), "crates/sonicterm-windows/src/main.rs": (100, 20),
                 "crates/sonicterm-linux/src/main.rs": (70, 66)}
        members = ["sonicterm-linux", "sonicterm-mac", "sonicterm-windows"]
        base = baseline({"sonicterm-mac": (100, 10), "sonicterm-windows": (100, 20), "sonicterm-linux": (70, 66)})
        on_mac = check(files, members, base, CI_ENV)
        self.assertEqual(on_mac.status, 0, on_mac.stdout)
        self.assertIn("compiled on macOS; not Windows execution coverage", row(on_mac.stdout, "sonicterm-windows"))
        self.assertIn("compiled on macOS; not Linux execution coverage", row(on_mac.stdout, "sonicterm-linux"))
        self.assertNotIn("compiled on", row(on_mac.stdout, "sonicterm-mac"))
        on_linux = check(files, members, base, LOCAL_ENV, target="x86_64-unknown-linux-gnu")
        self.assertIn("compiled on Linux; not macOS execution coverage", row(on_linux.stdout, "sonicterm-mac"))
        self.assertNotIn("compiled on", row(on_linux.stdout, "sonicterm-linux"))

    def test_one_file_reported_twice_is_refused(self):
        # Two spellings of one file would double its lines.
        files = {**FILES, "crates/sonicterm-vt/src/vt/../vt.rs": (190, 165)}
        run = check(files, MEMBERS, baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("lists crates/sonicterm-vt/src/vt.rs twice", run.stderr)


class InputValidationTests(unittest.TestCase):
    """Broken inputs are errors, never an empty or passing comparison."""

    def test_empty_report_is_refused(self):
        # An empty export is a broken run; comparing it would mark every crate missing.
        with workspace(export({}), metadata(MEMBERS), baseline(FLOORS)) as paths:
            run = invoke(paths, CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("lists no files", run.stderr)

    def test_document_that_is_not_an_llvm_export_is_refused(self):
        # Only an llvm-cov JSON export is accepted as a report.
        with workspace(dict(export(FILES), type="something.else"), metadata(MEMBERS), baseline(FLOORS)) as paths:
            run = invoke(paths, CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("is not an llvm.coverage.json.export document", run.stderr)

    def test_malformed_baselines_are_refused(self):
        # Each schema rule rejects its own violation, so a hand edit cannot weaken the floor.
        good = baseline(FLOORS, {"sonicterm-fontconfig": "linked only on non-macOS Unix"})
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
                run = check(FILES, MEMBERS + ["sonicterm-fontconfig"], document, CI_ENV)
                self.assertEqual(run.status, 2, run.stdout)
                self.assertIn("is invalid", run.stderr)

    def test_non_finite_baseline_numbers_are_refused(self):
        # NaN compares false and 1e309 overflows to inf; each must exit 2 instead of passing or crashing.
        text = json.dumps(baseline(FLOORS), indent=2)
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
                with workspace(export(FILES), metadata(MEMBERS), document) as paths:
                    run = invoke(paths, CI_ENV)
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
                document = baseline(FLOORS)
                mutate(document)
                with self.assertRaisesRegex(floor.InputError, "finite"):
                    floor.parse_baseline(document, "in-memory")

    def test_missing_input_file_is_an_input_error(self):
        # A missing input exits 2 instead of comparing against nothing.
        with workspace(export(FILES), metadata(MEMBERS), None) as paths:
            run = invoke(paths, CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("cannot read baseline", run.stderr)


class RepositoryContractTests(unittest.TestCase):
    """The committed baseline and gate script agree with the repository."""

    def test_committed_baseline_is_valid_and_keyed_to_the_macos_arm64_ci_runner(self):
        # The committed floor loads and names the host CI enforces it on.
        committed = floor.load_baseline(str(_HERE / "coverage-baseline.json"))
        self.assertEqual((committed.target, committed.runner), ("aarch64-apple-darwin", "macos14/ARM64"))
        self.assertEqual(committed.tolerance_pp, 1.0)

    def test_baseline_names_are_workspace_packages(self):
        # A typo fails here in seconds; the full inventory check needs the instrumented run.
        manifest = (_REPO / "Cargo.toml").read_text(encoding="utf-8")
        table = re.search(r"(?ms)^\[workspace\]\n(.*?)^\[", manifest).group(1)
        members = re.findall(r'"([^"]+)"', re.search(r"(?ms)^members\s*=\s*\[(.*?)\]", table).group(1))
        names = set()
        for member in members:
            text = (_REPO / member / "Cargo.toml").read_text(encoding="utf-8")
            package = re.search(r"(?ms)^\[package\]\n(.*?)(?=^\[|\Z)", text).group(1)
            names.add(re.search(r'(?m)^name\s*=\s*"([^"]+)"', package).group(1))
        self.assertIn("sonicterm-vt", names)
        committed = floor.load_baseline(str(_HERE / "coverage-baseline.json"))
        self.assertLessEqual(set(committed.crates) | set(committed.not_measured), names)

    def test_generated_sources_equal_the_marked_first_party_files(self):
        # A newly committed generated file cannot be counted as authored code without failing here.
        sources = first_party_rust_sources()
        self.assertGreater(len(sources), 100)
        marked = {path for path in sources if has_generator_marker(path)}
        for path in floor.GENERATED_SOURCES:
            with self.subTest(path=path):
                self.assertTrue((_REPO / path).is_file())
        # A listed file without a marker must be pinned here with the reason it is generated.
        unmarked_generated = {}
        self.assertLessEqual(set(unmarked_generated), set(floor.GENERATED_SOURCES))
        self.assertEqual(set(floor.GENERATED_SOURCES) - set(unmarked_generated), marked)
        # Pinned authored: hand-written FFI declarations stay counted even though they resemble bindings.
        hand_written = {
            "crates/sonicterm-fontconfig/src/lib.rs":
                "Servo's hand-written Fontconfig declarations; the file opens with the Servo copyright header",
        }
        for path, reason in hand_written.items():
            with self.subTest(path=path, reason=reason):
                self.assertNotIn(path, floor.GENERATED_SOURCES)
                self.assertNotIn(path, marked)
                first = (_REPO / path).read_text(encoding="utf-8").splitlines()[0]
                self.assertEqual(first, "// Copyright 2013 The Servo Project Developers. See the COPYRIGHT")

    def test_vendored_roots_match_the_native_dependency_manifest(self):
        # Vendored exclusions follow the pinned import list instead of a second copy.
        manifest = json.loads((_HERE / "native-dependencies.json").read_text(encoding="utf-8"))
        self.assertEqual(sorted(floor.VENDORED_ROOTS), sorted(library["path"] for library in manifest["libraries"]))

    def test_gate_measures_vt_keeps_the_subset_floor_and_runs_the_crate_floor(self):
        script = (_HERE / "rust-logic-coverage.sh").read_text(encoding="utf-8")
        regex = re.search(r"(?m)^IGNORE_REGEX='(.*)'$", script).group(1)
        prefix = "(sonicterm-("
        crates = regex[regex.index(prefix) + len(prefix):].split(")", 1)[0].split("|")
        self.assertNotIn("vt", crates)
        # CLAUDE.md and both Development-and-Release pages state this count.
        self.assertEqual(len(crates), 10)
        self.assertIn("--fail-under-lines 80", script)
        markers = ["coverage-floor_tests.py", "cargo llvm-cov --workspace",
                   "cargo llvm-cov report --json --summary-only", "cargo metadata --no-deps",
                   '"$PY" "$ROOT/scripts/coverage-floor.py"']
        positions = [script.index(marker) for marker in markers]
        self.assertEqual(positions, sorted(positions))


class CliTests(unittest.TestCase):
    """The script's exit status holds when run as a real process."""

    def test_exit_status_from_a_real_process(self):
        # Exit 1 fails CI, 0 is informational, 2 is misuse; the gate step relies on these codes.
        env = clean_env()
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 100)}
        with workspace(export(files), metadata(MEMBERS), baseline(FLOORS)) as paths:
            argv = [sys.executable, str(_FLOOR_PATH), "--report", str(paths["report"]),
                    "--metadata", str(paths["metadata"]), "--baseline", str(paths["baseline"]), "--target", TARGET]

            def run(arguments, environment):
                return subprocess.run(arguments, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      text=True, encoding="utf-8", check=False, timeout=60)

            failing = run(argv, {**env, **CI_ENV})
            local = run(argv, env)
            incomplete = run(argv[:4], env)
        self.assertEqual(failing.returncode, 1, failing.stdout + failing.stderr)
        self.assertEqual(local.returncode, 0, local.stdout + local.stderr)
        self.assertEqual(incomplete.returncode, 2)
        self.assertIn("required", incomplete.stderr)

    def test_overflowing_baseline_number_exits_2_without_a_traceback(self):
        # 1e309 overflows to inf; the reader must refuse it with exit 2, not raise an uncaught ValueError.
        document = json.dumps(baseline(FLOORS), indent=2).replace(
            '"tolerance_pp": 1.0', '"tolerance_pp": 1e309')
        with workspace(export(FILES), metadata(MEMBERS), document) as paths:
            completed = subprocess.run(
                [sys.executable, str(_FLOOR_PATH), "--report", str(paths["report"]), "--metadata",
                 str(paths["metadata"]), "--baseline", str(paths["baseline"]), "--target", TARGET],
                env={**clean_env(), **CI_ENV}, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True, encoding="utf-8", check=False, timeout=60)
        self.assertEqual(completed.returncode, 2, completed.stdout + completed.stderr)
        self.assertNotIn("Traceback", completed.stderr)
        self.assertIn("non-finite number 1e309 is not allowed", completed.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
