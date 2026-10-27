"""Coverage macos tests."""

from __future__ import annotations
from contextlib import contextmanager
import io
import json
from pathlib import Path
import tempfile
import unittest

import coverage_test_fixture


MACOS_FILE = "crates/sonicterm-ui/src/text_edit.rs"


def with_macos(base, files, target=coverage_test_fixture.TARGET, runner=coverage_test_fixture.RUNNER, reason="fixture macOS floor"):
    """Add a macos section of {path: (lines, covered)} floors to a baseline document."""
    base["macos"] = {
        "host": {"target": target, "runner": runner},
        "reason": reason,
        "files": {path: {"lines": lines, "covered": covered, "percent": round(covered * 100 / lines, 2)}
                  for path, (lines, covered) in files.items()},
    }
    return base


def macos_floor(files, base, env, *extra, target=coverage_test_fixture.TARGET):
    """Run the macos-floor subcommand on a fixture report; return the run and the baseline as it is afterwards."""
    with coverage_test_fixture.workspace(coverage_test_fixture.export(files), None, base) as paths:
        stdout, stderr = io.StringIO(), io.StringIO()
        argv = ["macos-floor", "--report", str(paths["report"]), "--baseline", str(paths["baseline"]),
                "--target", target, "--source-root", coverage_test_fixture.ROOT, *extra]
        status = coverage_test_fixture.floor.main(argv, env=env, stdout=stdout, stderr=stderr)
        written = json.loads(paths["baseline"].read_text(encoding="utf-8"))
        return coverage_test_fixture.Run(status, stdout.getvalue(), stderr.getvalue()), written


class MacosFloorTests(unittest.TestCase):
    """The macos section holds macOS-only logic to per-file floors on its own host."""

    def test_the_floor_holds_within_tolerance_and_fails_beyond_it_in_ci_on_its_host(self):
        # 159/160 is the floor; 158/160 is 0.625 pp lower and holds, 150/160 is 5.6 pp lower and fails.
        base = with_macos(coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), {MACOS_FILE: (160, 159)})
        held, _ = macos_floor({**coverage_test_fixture.FILES, MACOS_FILE: (160, 158)}, base, coverage_test_fixture.CI_ENV)
        self.assertEqual(held.status, 0, held.stdout + held.stderr)
        self.assertIn("macOS floor PASS", held.stdout)
        dropped, _ = macos_floor({**coverage_test_fixture.FILES, MACOS_FILE: (160, 150)}, base, coverage_test_fixture.CI_ENV)
        self.assertEqual(dropped.status, 1)
        self.assertIn(f"{MACOS_FILE}: 93.75% is", dropped.stdout)
        self.assertIn("pp below its floor 99.38%", dropped.stdout)

    def test_a_missing_file_fails_and_another_host_is_never_a_pass(self):
        # A floor file the run did not measure fails on the section's host; CI elsewhere fails as not comparable;
        # outside CI the result is informational whatever the numbers.
        base = with_macos(coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), {MACOS_FILE: (160, 159)})
        missing, _ = macos_floor(coverage_test_fixture.FILES, base, coverage_test_fixture.CI_ENV)
        self.assertEqual(missing.status, 1)
        self.assertIn(f"{MACOS_FILE}: not measured", missing.stdout)
        linux_ci = {"GITHUB_ACTIONS": "true", "ImageOS": "ubuntu24", "RUNNER_ARCH": "X64"}
        elsewhere, _ = macos_floor({**coverage_test_fixture.FILES, MACOS_FILE: (160, 159)}, base, linux_ci)
        self.assertEqual(elsewhere.status, 1)
        self.assertIn("not comparable", elsewhere.stdout)
        local, _ = macos_floor({**coverage_test_fixture.FILES, MACOS_FILE: (160, 100)}, base, coverage_test_fixture.LOCAL_ENV)
        self.assertEqual(local.status, 0)
        self.assertIn("informational only", local.stdout)

    def test_update_and_check_refuse_misplaced_or_missing_arguments(self):
        # An update states why and names its verified record; check mode takes no update flags.
        base = with_macos(coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), {MACOS_FILE: (160, 159)})
        report = {**coverage_test_fixture.FILES, MACOS_FILE: (160, 159)}
        for extra, message in (
            (("--update", "--provenance", "record.json"), "--reason"),
            (("--update", "--reason", "why"), "--provenance FILE"),
            (("--reason", "why"), "only valid with --update"),
            (("--provenance", "record.json"), "only valid with --update"),
        ):
            with self.subTest(extra=extra):
                run, _ = macos_floor(report, base, coverage_test_fixture.LOCAL_ENV, *extra)
                self.assertEqual(run.status, 2)
                self.assertIn(message, run.stderr)

    def test_the_section_is_validated_and_kept_by_crate_updates(self):
        # A malformed section fails the whole baseline; crate updates and proposals keep the section verbatim.
        valid = with_macos(coverage_test_fixture.baseline(coverage_test_fixture.FLOORS), {MACOS_FILE: (160, 159)})
        parsed = coverage_test_fixture.floor.parse_baseline(valid, "fixture")
        self.assertEqual(coverage_test_fixture.floor.baseline_document(parsed), valid)
        for case, mutate, message in (
            ("no files", lambda document: document["macos"].update(files={}), "macos.files must name"),
            ("extra key", lambda document: document["macos"].update(extra=1), "macos must hold exactly"),
            ("wrong percent", lambda document: document["macos"]["files"][MACOS_FILE].update(percent=50.0),
             f"macos.files.{MACOS_FILE}.percent"),
            ("no reason", lambda document: document["macos"].update(reason=" "), "macos.reason"),
        ):
            with self.subTest(case=case):
                document = json.loads(json.dumps(valid))
                mutate(document)
                with self.assertRaises(coverage_test_fixture.floor.InputError) as raised:
                    coverage_test_fixture.floor.parse_baseline(document, "fixture")
                self.assertIn(message, str(raised.exception))
        updated = coverage_test_fixture.floor.partial_update(parsed, {}, None, coverage_test_fixture.TARGET, coverage_test_fixture.RUNNER, "no crate changes", [])
        self.assertEqual(updated.macos, parsed.macos)
        self.assertEqual(coverage_test_fixture.floor.rebaseline_proposal(parsed, {}, coverage_test_fixture.TARGET, coverage_test_fixture.RUNNER)["macos"], valid["macos"])


# A macos-core run's environment: the crate job's fixture runner, under the job that runs the macOS-only floor.
MACOS_RUN_ENV = {**coverage_test_fixture.RUN_ENV, "GITHUB_JOB": "macos-core"}
MACOS_ARTIFACT = "macos-logic-coverage-evidence-36087392875-1"


@contextmanager
def macos_retained_run(files, env=MACOS_RUN_ENV, base=None, version="0.9.0"):
    """Yield a checkout holding the baseline, plus a retained macos-core run's report and provenance record."""
    with tempfile.TemporaryDirectory() as directory:
        checkout, evidence = Path(directory) / "checkout", Path(directory) / "evidence"
        checkout.mkdir()
        evidence.mkdir()
        coverage_test_fixture.git(checkout, "init", "-q")
        document = coverage_test_fixture.baseline(coverage_test_fixture.FLOORS) if base is None else base
        coverage_test_fixture.commit_files(checkout, {**coverage_test_fixture.CHECKOUT, "scripts/coverage-baseline.json": json.dumps(document, indent=2) + "\n"})
        paths = {"root": checkout, "baseline": checkout / "scripts" / "coverage-baseline.json",
                 "report": evidence / coverage_test_fixture.floor.REPORT_FILE, "provenance": evidence / coverage_test_fixture.floor.PROVENANCE_FILE}
        paths["report"].write_text(json.dumps(coverage_test_fixture.export(files, root=str(checkout), version=version), indent=2) + "\n",
                                   encoding="utf-8")
        pin = evidence / "checkout-pin.json"
        coverage_test_fixture.pin_checkout(checkout, pin)
        stderr = io.StringIO()
        status = coverage_test_fixture.floor.main(["record-macos-provenance", "--output", str(paths["provenance"]), "--root", str(checkout),
                             "--pin", str(pin), "--target", coverage_test_fixture.TARGET, "--rustc-verbose", coverage_test_fixture.RUSTC_VERBOSE,
                             "--report", str(paths["report"])], env=env, stdout=io.StringIO(), stderr=stderr)
        if status != 0:
            raise AssertionError(f"record-macos-provenance exited {status}: {stderr.getvalue()}")
        yield paths


def macos_update(paths, *extra):
    """Run macos-floor --update against a retained macos-core run's report, outside CI."""
    stdout, stderr = io.StringIO(), io.StringIO()
    argv = ["macos-floor", "--update", "--report", str(paths["report"]), "--baseline", str(paths["baseline"]),
            "--target", coverage_test_fixture.TARGET, "--source-root", str(paths["root"]), *extra]
    status = coverage_test_fixture.floor.main(argv, env=coverage_test_fixture.LOCAL_ENV, stdout=stdout, stderr=stderr)
    return coverage_test_fixture.Run(status, stdout.getvalue(), stderr.getvalue())


class MacosProvenanceTests(unittest.TestCase):
    """The macOS floors change only from a verified macos-core record, as the crate floors do."""

    def test_update_without_a_verified_record_is_refused(self):
        # The probe that once passed: arbitrary low numbers labelled as the CI host never replace the floors.
        with macos_retained_run({**coverage_test_fixture.FILES, MACOS_FILE: (160, 2)}) as paths:
            before = paths["baseline"].read_text(encoding="utf-8")
            run = macos_update(paths, "--reason", "probe", "--runner", coverage_test_fixture.RUNNER, "--file", MACOS_FILE)
            self.assertEqual(run.status, 2, run.stdout)
            self.assertIn("--provenance FILE", run.stderr)
            self.assertEqual(paths["baseline"].read_text(encoding="utf-8"), before)

    def test_records_that_do_not_verify_are_refused(self):
        # A local record, another job's record, a report changed after its record, and a report from a
        # cargo-llvm-cov other than the pinned one are each refused, and the baseline is left as it was.
        for case, options, tamper, message in (
            ("local", {"env": coverage_test_fixture.LOCAL_ENV}, False, "measured outside CI"),
            ("other job", {"env": coverage_test_fixture.RUN_ENV}, False, "not macos-core"),
            ("changed report", {}, True, "SHA-256"),
            ("unpinned tool", {"version": "0.8.7"}, False, "the version scripts/pinned-tools.json pins"),
        ):
            with self.subTest(case=case), macos_retained_run({**coverage_test_fixture.FILES, MACOS_FILE: (160, 159)}, **options) as paths:
                if tamper:
                    changed = coverage_test_fixture.export({**coverage_test_fixture.FILES, MACOS_FILE: (160, 2)}, root=str(paths["root"]))
                    paths["report"].write_text(json.dumps(changed, indent=2) + "\n", encoding="utf-8")
                before = paths["baseline"].read_text(encoding="utf-8")
                run = macos_update(paths, "--reason", "probe", "--file", MACOS_FILE,
                                   "--provenance", str(paths["provenance"]))
                self.assertEqual(run.status, 2, run.stdout)
                self.assertIn(message, run.stderr)
                self.assertEqual(paths["baseline"].read_text(encoding="utf-8"), before)

    def test_a_verified_macos_core_record_rewrites_only_the_section(self):
        # The verified record supplies the host and appends its run to the reason; every crate floor stays verbatim,
        # and a section the baseline does not hold yet must name its files.
        with macos_retained_run({**coverage_test_fixture.FILES, MACOS_FILE: (160, 159)}) as paths:
            unnamed = macos_update(paths, "--reason", "seed", "--provenance", str(paths["provenance"]))
            self.assertEqual(unnamed.status, 2)
            self.assertIn("--file PATH", unnamed.stderr)
            run = macos_update(paths, "--reason", "seed", "--file", MACOS_FILE, "--provenance", str(paths["provenance"]))
            self.assertEqual(run.status, 0, run.stderr)
            written = json.loads(paths["baseline"].read_text(encoding="utf-8"))
            self.assertEqual(written["crates"], coverage_test_fixture.baseline(coverage_test_fixture.FLOORS)["crates"])
            self.assertEqual(written["macos"]["host"], {"target": coverage_test_fixture.TARGET, "runner": coverage_test_fixture.RUNNER})
            self.assertEqual(written["macos"]["files"], {MACOS_FILE: {"lines": 160, "covered": 159, "percent": 99.38}})
            self.assertIn(f"job macos-core, evidence artifact {MACOS_ARTIFACT}", written["macos"]["reason"])

    def test_a_crate_record_from_an_unpinned_cargo_llvm_cov_is_refused(self):
        # The crate floors need the cargo-llvm-cov release the checkout pins, as the macOS floors do.
        with coverage_test_fixture.retained_run(version="0.8.7") as paths:
            run = coverage_test_fixture.update(paths, "--provenance", str(paths["provenance"]), "--reason", "probe")
            self.assertEqual(run.status, 2, run.stdout)
            self.assertIn("the version scripts/pinned-tools.json pins", run.stderr)
