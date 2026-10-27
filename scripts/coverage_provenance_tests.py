"""Coverage provenance tests."""

from __future__ import annotations
import hashlib
import json
import unittest

import coverage_test_fixture


class RebaselineProvenanceTests(unittest.TestCase):
    """A floor on the CI host changes only from a verified, complete record of the run that measured it."""

    def refuse(self, paths, *needles, extra=(), target=coverage_test_fixture.TARGET):
        """Assert that an update from the retained run is refused, names each needle, and writes nothing."""
        before = paths["baseline"].read_bytes()
        run = coverage_test_fixture.update(paths, "--reason", "toolchain moved coverage", "--provenance", str(paths["provenance"]),
                     *extra, target=target)
        self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2, run.stdout + run.stderr)
        for needle in needles:
            self.assertIn(needle, run.stderr)

    def test_ci_host_baseline_change_without_provenance_is_refused(self):
        # A stated runner is no longer enough to change a floor CI enforces.
        with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", coverage_test_fixture.RUNNER,
                         "--crate", "sonicterm-vt")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("requires --provenance FILE whenever the baseline names a CI host "
                      "(aarch64-apple-darwin on macos14/ARM64)", run.stderr)
        # A first baseline for the CI host needs a record too.
        with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), None) as paths:
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", coverage_test_fixture.RUNNER)
            self.assertFalse(paths["baseline"].exists())
        self.assertEqual(run.status, 2)
        self.assertIn("requires --provenance FILE", run.stderr)

    def test_incomplete_record_is_refused(self):
        # A run that stopped before its measurement completed can never change a floor.
        with coverage_test_fixture.retained_run() as paths:
            coverage_test_fixture.record(paths["root"], paths["provenance"], "instrumented-tests", "failed", coverage_test_fixture.RUN_ENV, 101)
            self.refuse(paths, "is incomplete: the run stopped in phase 'instrumented-tests' (exit status 101)")

    def test_record_missing_a_field_is_refused(self):
        # Every listed field must be present and non-empty; the refusal names the missing one.
        for field_name in ("image_version", "run_id", "rustc_verbose", "tree_entries"):
            with self.subTest(field=field_name):
                with coverage_test_fixture.retained_run() as paths:
                    coverage_test_fixture.rewrite(paths["provenance"], **{field_name: None})
                    self.refuse(paths, f"is missing required field(s): {field_name}")
        with coverage_test_fixture.retained_run(env=dict(coverage_test_fixture.RUN_ENV, GITHUB_EVENT_NAME="pull_request")) as paths:
            self.refuse(paths, "is missing required field(s): pull_request_head")

    def test_record_from_outside_ci_is_refused(self):
        # A local run has no run to verify, so its record cannot change a floor.
        with coverage_test_fixture.retained_run(env=coverage_test_fixture.LOCAL_ENV) as paths:
            self.refuse(paths, "was measured outside CI")

    def test_digest_mismatch_is_refused(self):
        # A report or inventory that differs from the record fails, naming both digests.
        with coverage_test_fixture.retained_run() as paths:
            recorded = json.loads(paths["provenance"].read_text(encoding="utf-8"))["report_sha256"]
            paths["report"].write_text(json.dumps(coverage_test_fixture.export({**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 166)})),
                                       encoding="utf-8")
            actual = hashlib.sha256(paths["report"].read_bytes()).hexdigest()
            self.refuse(paths, f"has SHA-256 {actual}, but the record's report_sha256 is {recorded}")
        with coverage_test_fixture.retained_run() as paths:
            paths["metadata"].write_text(json.dumps(coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS)), encoding="utf-8")
            self.refuse(paths, "but the record's inventory_sha256 is")

    def test_conflicting_target_or_runner_is_refused(self):
        # The stated host must agree with the record; each conflict is named.
        with coverage_test_fixture.retained_run() as paths:
            self.refuse(paths, "--target x86_64-apple-darwin conflicts with the record's target aarch64-apple-darwin",
                        target="x86_64-apple-darwin")
            self.refuse(paths, "--runner macos15/ARM64 conflicts with the record's runner macos14/ARM64",
                        extra=("--runner", "macos15/ARM64"))

    def test_measured_tree_with_other_coverage_relevant_content_is_refused(self):
        # Source, policy, and a new test target each change what is measured, committed or not.
        cases = {"source": ("crates/sonicterm-vt/src/vt.rs", "pub fn vt() { changed(); }\n"),
                 "policy": ("scripts/coverage-floor.py", "# policy changed\n"),
                 "new test target": ("crates/sonicterm-vt/tests/new.rs", "#[test]\nfn new() {}\n")}
        for label, (relative, content) in cases.items():
            with self.subTest(change=label):
                with coverage_test_fixture.retained_run() as paths:
                    changed = paths["root"] / relative
                    changed.parent.mkdir(parents=True, exist_ok=True)
                    changed.write_text(content, encoding="utf-8")
                    self.refuse(paths, f"uncommitted coverage-relevant changes in {relative}")
                    coverage_test_fixture.commit_files(paths["root"], {})
                    self.refuse(paths, f"differs from this checkout's HEAD in {relative}")

    def test_baseline_or_documentation_only_changes_are_accepted(self):
        # A tree that changes only the baseline or documentation still matches the measured tree.
        with coverage_test_fixture.retained_run() as paths:
            coverage_test_fixture.commit_files(paths["root"], {
                "README.md": "# SonicTerm, reworded\n", "wiki/Home.md": "# Home, reworded\n", "docs/notes.md": "notes\n",
                "crates/sonicterm-vt/CLAUDE.md": "# vt\n",
                "scripts/coverage-baseline.json": json.dumps(coverage_test_fixture.baseline(coverage_test_fixture.FLOORS, reason="edited"), indent=2) + "\n"})
            (paths["root"] / "wiki" / "Draft.md").write_text("draft\n", encoding="utf-8")
            run = coverage_test_fixture.update(paths, "--reason", "vt parser paths moved", "--provenance", str(paths["provenance"]),
                         "--crate", "sonicterm-vt")
        self.assertEqual(run.status, 0, run.stderr)

    def test_coverage_relevant_paths(self):
        # Only the baseline file and documentation are exempt from the measured-tree comparison.
        relevant = ["crates/sonicterm-vt/src/vt.rs", "crates/sonicterm-vt/Cargo.toml", "Cargo.lock",
                    "rust-toolchain.toml", "scripts/rust-logic-coverage.sh", "scripts/coverage-floor.py",
                    ".github/workflows/ci.yml", "crates/sonicterm-vt/tests/autowrap.rs", "wikipedia.rs"]
        exempt = ["scripts/coverage-baseline.json", "README.md", "CLAUDE.md", "crates/sonicterm-vt/CLAUDE.md",
                  "wiki/Development-and-Release.md", "wiki/logo.png", "docs/specs/plan.txt", "CHANGELOG.MD"]
        for path in relevant:
            with self.subTest(path=path):
                self.assertTrue(coverage_test_fixture.floor.coverage_relevant(path))
        for path in exempt:
            with self.subTest(path=path):
                self.assertFalse(coverage_test_fixture.floor.coverage_relevant(path))

    def test_crate_update_reproduces_the_retained_floor_exactly(self):
        # With verified provenance, --crate writes the retained run's counts for that crate and nothing else.
        files = {"crates/sonicterm-grid/src/grid.rs": (400, 300), "crates/sonicterm-vt/src/vt.rs": (190, 141),
                 "crates/sonicterm-vt/src/vt/staging.rs": (10, 9)}
        with coverage_test_fixture.retained_run(files=files) as paths:
            run = coverage_test_fixture.update(paths, "--reason", "vt lost dead parser paths", "--provenance", str(paths["provenance"]),
                         "--crate", "sonicterm-vt")
            stored = coverage_test_fixture.floor.load_baseline(str(paths["baseline"]))
            checked = coverage_test_fixture.invoke(paths, coverage_test_fixture.CI_ENV)
        self.assertEqual(run.status, 0, run.stderr)
        self.assertEqual(stored.crates["sonicterm-vt"], coverage_test_fixture.floor.Entry(200, 150))
        self.assertEqual(stored.crates["sonicterm-grid"], coverage_test_fixture.floor.Entry(400, 360))
        self.assertIn(coverage_test_fixture.ARTIFACT, stored.reason)
        self.assertRegex(coverage_test_fixture.row(checked.stdout, "sonicterm-vt"), r"\sok\s")
        self.assertIn("DROP", coverage_test_fixture.row(checked.stdout, "sonicterm-grid"))

    def test_full_update_migrates_the_host_and_names_both(self):
        # A new runner image needs every floor re-measured: a full update records the move and both hosts.
        with coverage_test_fixture.retained_run(env=dict(coverage_test_fixture.RUN_ENV, ImageOS="macos15")) as paths:
            run = coverage_test_fixture.update(paths, "--reason", "The runner image moved to macOS 15", "--provenance",
                         str(paths["provenance"]))
            stored = coverage_test_fixture.floor.load_baseline(str(paths["baseline"]))
        self.assertEqual(run.status, 0, run.stderr)
        self.assertEqual((stored.target, stored.runner), (coverage_test_fixture.TARGET, "macos15/ARM64"))
        self.assertTrue(stored.reason.startswith(
            "Host migration from aarch64-apple-darwin on macos14/ARM64 to aarch64-apple-darwin on macos15/ARM64. "
            "The runner image moved to macOS 15. Measured by"), stored.reason)
        self.assertIn("host: aarch64-apple-darwin on macos14/ARM64 -> aarch64-apple-darwin on macos15/ARM64",
                      run.stdout)
        self.assertEqual(stored.crates, {"sonicterm-grid": coverage_test_fixture.floor.Entry(400, 360),
                                         "sonicterm-vt": coverage_test_fixture.floor.Entry(200, 174)})

    def test_forged_identity_and_empty_checks_are_refused(self):
        # A path map alone cannot vouch for a tree or commit it never hashed to, and a complete measurement
        # carries both checks.
        with coverage_test_fixture.retained_run() as paths:
            coverage_test_fixture.rewrite(paths["provenance"], tree="0" * 40, commit="not-a-commit", checks={})
            self.refuse(paths, "the record's commit 'not-a-commit' is not a full lowercase sha1 object id",
                        "not the recorded tree " + "0" * 40, "the record's checks lacks floor, subset-gate")

    def test_path_map_that_does_not_hash_to_the_recorded_tree_is_refused(self):
        # One changed or one dropped entry no longer hashes to the tree, even where the tree comparison is exempt.
        for label in ("changed", "dropped"):
            with self.subTest(mutation=label):
                with coverage_test_fixture.retained_run() as paths:
                    document = json.loads(paths["provenance"].read_text(encoding="utf-8"))
                    entries = dict(document["tree_entries"])
                    if label == "changed":
                        mode, object_id = entries["README.md"].split(" ")
                        entries["README.md"] = f"{mode} {'f' * len(object_id)}"
                    else:
                        del entries["README.md"]
                    coverage_test_fixture.rewrite(paths["provenance"], tree_entries=entries)
                    self.refuse(paths, "the record's tree_entries hash to tree",
                                f"not the recorded tree {document['tree']}")

    def test_object_ids_must_be_full_lowercase_ids_in_the_checkout_format(self):
        # Abbreviated, uppercase, or wrong-length IDs are refused by name; this fixture checkout uses SHA-1.
        mutations = {"abbreviated commit": ("commit", lambda value: value[:12]),
                     "uppercase commit": ("commit", str.upper),
                     "sha256-length tree": ("tree", lambda value: (value * 2)[:64])}
        for label, (field_name, mutate) in mutations.items():
            with self.subTest(case=label):
                with coverage_test_fixture.retained_run() as paths:
                    value = mutate(json.loads(paths["provenance"].read_text(encoding="utf-8"))[field_name])
                    coverage_test_fixture.rewrite(paths["provenance"], **{field_name: value})
                    self.refuse(paths, f"the record's {field_name} {value!r} is not a full lowercase sha1 object id")
        with coverage_test_fixture.retained_run(env=dict(coverage_test_fixture.RUN_ENV, GITHUB_EVENT_NAME="pull_request",
                                   COVERAGE_PULL_REQUEST_HEAD="c209d05")) as paths:
            self.refuse(paths, "the record's pull_request_head 'c209d05' is not a full lowercase sha1 object id")

    def test_incomplete_or_impossible_checks_are_refused(self):
        # A complete measurement carries exactly both checks, each with a status in a pair the gate script writes.
        cases = {
            "missing floor": ({"subset-gate": "exit status 0"}, "the record's checks lacks floor"),
            "unknown status": ({"subset-gate": "passed", "floor": "not run"}, "which the gate script never writes"),
            "passed gate, floor never run": ({"subset-gate": "exit status 0", "floor": "not run"},
                                             "which the gate script never writes"),
            "failed gate, floor ran": ({"subset-gate": "exit status 1", "floor": "exit status 0"},
                                       "which the gate script never writes"),
            "negative status": ({"subset-gate": "exit status -1", "floor": "not run"},
                                "which the gate script never writes"),
            "extra entry": ({"subset-gate": "exit status 0", "floor": "exit status 0", "doctests": "exit status 0"},
                            "has unknown entries doctests"),
        }
        for label, (checks, needle) in cases.items():
            with self.subTest(case=label):
                with coverage_test_fixture.retained_run() as paths:
                    coverage_test_fixture.rewrite(paths["provenance"], checks=checks)
                    self.refuse(paths, needle)

    def test_record_naming_checkout_drift_is_refused(self):
        # A complete record never names drift, so one that does is refused even if everything else verifies.
        with coverage_test_fixture.retained_run() as paths:
            coverage_test_fixture.rewrite(paths["provenance"], checkout_drift=["HEAD moved from a to b"])
            self.refuse(paths, "the record names checkout drift: HEAD moved from a to b")

    def test_ci_host_baseline_cannot_move_to_local_without_provenance(self):
        # Declaring the host `local` does not sidestep provenance: the existing baseline names the CI host.
        with coverage_test_fixture.workspace(coverage_test_fixture.export(coverage_test_fixture.FILES), coverage_test_fixture.metadata(coverage_test_fixture.MEMBERS), coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = coverage_test_fixture.invoke(paths, coverage_test_fixture.LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", "local")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("requires --provenance FILE whenever the baseline names a CI host "
                      "(aarch64-apple-darwin on macos14/ARM64)", run.stderr)


class DropGuidanceTests(unittest.TestCase):
    """A DROP still fails without a proposal of its own and points at its evidence and the procedure."""

    def test_drop_names_this_runs_artifact_and_the_procedure(self):
        # The message names the run's artifact and the documented procedure, and prints no proposal.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 162)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS, coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.RUN_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn(f"The evidence for this run is the artifact {coverage_test_fixture.ARTIFACT} of "
                      "https://github.com/D0n9X1n/SonicTerm/actions/runs/36087392875.", run.stdout)
        self.assertIn(coverage_test_fixture.floor.REBASELINE_PROCEDURE, run.stdout)
        self.assertIn("--update-baseline --provenance", run.stdout)
        self.assertNotIn(coverage_test_fixture.floor.PROPOSAL_BEGIN, run.stdout)

    def test_drop_beside_a_missing_floor_keeps_the_dropped_floor_in_the_preview(self):
        # The additions-only preview never lowers the dropped floor, and the DROP guidance still prints.
        files = {**coverage_test_fixture.FILES, "crates/sonicterm-vt/src/vt.rs": (190, 162), "crates/sonicterm-new/src/lib.rs": (10, 8)}
        run = coverage_test_fixture.check(files, coverage_test_fixture.MEMBERS + ["sonicterm-new"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), coverage_test_fixture.RUN_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn(f"the artifact {coverage_test_fixture.ARTIFACT}", run.stdout)
        self.assertEqual(coverage_test_fixture.proposal(run.stdout)["crates"]["sonicterm-vt"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)["crates"]["sonicterm-vt"])
        self.assertIn("a preview only", run.stdout)
