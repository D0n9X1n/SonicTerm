#!/usr/bin/env python3
"""Contracts of perf-flags.py: loading two runs' downloaded comparison artifacts and splitting their candidate
flags. Every artifact tree is built here; nothing calls GitHub."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("perf_flags", HERE / "perf-flags.py")
try:
    flags = importlib.util.module_from_spec(SPEC)
    sys.modules[SPEC.name] = flags
    SPEC.loader.exec_module(flags)
except (FileNotFoundError, AttributeError) as error:
    # When: the script does not exist yet, every test fails by assertion rather than erroring at import.
    flags = None
    IMPORT_PROBLEM = str(error)

HARNESS = "ab" * 32
HEAD, OTHER_HEAD, BASE, OTHER_BASE = "1" * 40, "2" * 40, "3" * 40, "4" * 40
SETTINGS = {"short": True, "counters": False, "features": {"base": [], "head": []}, "profile": {}}
CAPABILITIES = {"latency_split_schema": 1, "phase_kinds": 1, "s10_attribution": None,
                "echo_timeline_schema": None}
MACOS_PRESENTER = {"software_render_mode": "auto", "software_rendering": False, "software_render_degraded": False,
                   "windows_gdi": False}


def transition(completion_ms):
    """A complete transition phase of a phase-kinds harness, presenting one frame."""
    return {"name": "image", "kind": "transition", "endpoint": "image-registered-then-presented",
            "completion_ms": completion_ms, "start_unix_s": 10.0, "end_unix_s": 11.0, "presented_frames": 1,
            "redraw_requested": 1, "first_present_ms": 5.0, "last_present_ms": 5.0, "first_present_seq": 1,
            "last_present_seq": 1, "nonpresenting_redraws": 0}


def sustained(frames):
    """A complete sustained phase presenting `frames` frames over 60 s."""
    return {"name": "workload", "kind": "sustained", "presented_frames": frames, "redraw_requested": frames,
            "start_unix_s": 10.0, "end_unix_s": 70.0, "present_interval_ms": [16.6], "first_present_ms": 1.0,
            "last_present_ms": 59_000.0, "first_present_seq": 1, "last_present_seq": frames,
            "nonpresenting_redraws": 0}


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


class ArtifactTree:
    """One downloaded run: artifacts named under `root`, each with its identity, set inventory and attempts."""

    def __init__(self, root):
        self.root = root
        self.identities = {}

    def artifact(self, name, *, head=HEAD, base=BASE, version=None, attempt=1, platform="macos", harness=HARNESS,
                 settings=None, capabilities=None):
        directory = self.root / name
        self.identities[directory] = {
            "schema_version": 1, "run_id": "500", "run_attempt": attempt, "platform": platform, "base_sha": base,
            "head_sha": head, "harness_hash": harness, "settings": SETTINGS if settings is None else settings,
            "flag_metrics_version": flags.FLAG_METRICS_VERSION if version is None else version, "sets": [],
            "capabilities": CAPABILITIES if capabilities is None else capabilities}
        self.save(directory)
        return directory

    def save(self, artifact):
        write_json(artifact / "run-identity.json", self.identities[artifact])

    def entry(self, artifact, label, dataset):
        """The artifact's inventory entry for one set, created empty when it has none yet."""
        for entry in self.identities[artifact]["sets"]:
            if (entry["label"], entry["dataset"]) == (label, dataset):
                return entry
        entry = {"label": label, "dataset": dataset, "base": {"status": "", "accepted": []},
                 "head": {"status": "", "accepted": []}}
        self.identities[artifact]["sets"].append(entry)
        return entry

    def run(self, artifact, scenario, variant, index, side, phases, *, kind="valid", dataset="timed",
            classification="same", accepted=None, result_fields=None, outcome_fields=None, classification_side=None):
        """One attempt. `classification` is its final kind (`same` as the outcome's) or None for no file;
        `accepted` lists it in the inventory (by default exactly when the final kind is valid)."""
        directory = artifact / "runs" / f"{scenario}-{variant}" / dataset / f"{index:02d}-{side}"
        write_json(directory / "outcome.json", {"kind": kind, "side": side, "scenario": scenario, "variant": variant,
                                                "exit_code": 0, **(outcome_fields or {})})
        final = kind if classification == "same" else classification
        if classification is not None:
            write_json(directory / "classification.json",
                       {"side": classification_side or side, "kind": final, "reasons": []})
        # A result that passes perf-compare's own validator, so each test breaks exactly one thing.
        result = {"schema_version": 1, "harness_hash": HARNESS, "status": "valid", "exit_code": 0,
                  "scenario": scenario, "variant": variant, "managed": True, "short": True,
                  "frame_counters": "on" if dataset == "counters" else "unsupported",
                  "grid": {"columns": 250, "rows": 70}, "phases": phases, "latency": None, "throughput": None,
                  "uncover_ms": None, "scrollback_rows_retained": None,
                  "checkpoints": [{"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}],
                  "finish_session_settled": True, "notes": [], "presenter": dict(MACOS_PRESENTER)}
        result.update(result_fields or {})
        write_json(directory / "scratch" / "result.json", result)
        entry = self.entry(artifact, f"{scenario}/{variant}", dataset)
        if final == "valid" if accepted is None else accepted:
            entry[side]["accepted"].append(directory.name)
        self.save(artifact)
        return directory

    def side_status(self, artifact, label, dataset, side, status):
        """Mark one side of a set blocked or failed, as the comparison's final inventory records it."""
        self.entry(artifact, label, dataset)[side]["status"] = status
        self.save(artifact)


class PerfFlagsTests(unittest.TestCase):
    def setUp(self):
        if flags is None:
            self.fail(f"perf-flags.py cannot be loaded: {IMPORT_PROBLEM}")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.first = ArtifactTree(Path(self.temporary.name) / "first")
        self.second = ArtifactTree(Path(self.temporary.name) / "second")
        self.cases = 0

    def fresh_tree(self):
        """A new empty run directory, for a subtest that must not see another's artifacts."""
        self.cases += 1
        return ArtifactTree(Path(self.temporary.name) / f"case-{self.cases}")

    def late_image(self, tree, artifact, head_ms=90.0):
        tree.run(artifact, "S11", "release", 1, "base", [transition(40.0)])
        return tree.run(artifact, "S11", "release", 2, "head", [transition(head_ms)])

    def compare(self, allow_different_heads=False):
        return flags.compare_runs(flags.load_run(self.first.root), flags.load_run(self.second.root),
                                  allow_different_heads=allow_different_heads)

    def completion_checks(self, tree):
        checks = flags.load_run(tree.root).checks
        return [(check.base_samples, check.head_samples, check.head_value)
                for key, check in checks.items() if key[4] == "completion"]

    def test_flags_split_into_both_runs_one_run_and_missing_evidence(self):
        # A flag in both runs, a flag in one run whose other run checked it unflagged, and a flag whose other run
        # has no such check: the last is missing evidence, never a valid unflagged result.
        first = self.first.artifact("perf-macos-a")
        self.late_image(self.first, first)
        self.first.run(first, "S2", "default", 1, "base", [sustained(120)])
        self.first.run(first, "S2", "default", 2, "head", [sustained(60)])
        self.first.run(first, "S10", "default", 1, "base", [sustained(120)])
        self.first.run(first, "S10", "default", 2, "head", [sustained(60)])
        second = self.second.artifact("perf-macos-b")
        self.late_image(self.second, second)
        self.second.run(second, "S2", "default", 1, "base", [sustained(120)])
        self.second.run(second, "S2", "default", 2, "head", [sustained(120)])
        split = self.compare()
        keys = {name: {(key[1], key[4]) for key in split[name]} for name in ("both", "one", "missing")}
        self.assertEqual(keys["both"], {("S11/release", "completion")})
        self.assertEqual(keys["one"], {("S2/default", "fps")})
        self.assertEqual(keys["missing"], {("S10/default", "fps")})
        self.assertEqual(split["one"][next(iter(split["one"]))], ("flagged", "not flagged"))
        self.assertEqual(split["missing"][next(iter(split["missing"]))], ("flagged", "not checked"))

    def test_different_heads_are_refused_unless_allowed_and_then_listed_side_by_side(self):
        # Flags of different heads are not the same claim, so the split is refused by default.
        self.late_image(self.first, self.first.artifact("perf-macos-a"))
        self.late_image(self.second, self.second.artifact("perf-macos-b", head=OTHER_HEAD, base=OTHER_BASE))
        with self.assertRaises(flags.NotComparable):
            self.compare()
        listed = self.compare(allow_different_heads=True)
        self.assertEqual(set(listed), {"side_by_side"})
        self.assertEqual([len(run_flags) for run_flags in listed["side_by_side"]], [1, 1])
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed), contextlib.redirect_stderr(io.StringIO()):
            code = flags.main([str(self.first.root), str(self.second.root)])
        self.assertEqual(code, flags.EXIT_NOT_COMPARABLE)

    def test_incompatible_metric_versions_and_inconsistent_runs_are_refused(self):
        # A run whose flag metrics differ, or whose artifacts name different heads, cannot be compared.
        self.late_image(self.first, self.first.artifact("perf-macos-a"))
        self.late_image(self.second, self.second.artifact("perf-macos-b", version=flags.FLAG_METRICS_VERSION + 1))
        with self.assertRaises(flags.NotComparable):
            self.compare()
        self.late_image(self.first, self.first.artifact("perf-windows-a", head=OTHER_HEAD, platform="windows"))
        with self.assertRaises(flags.NotComparable):
            flags.load_run(self.first.root)

    def test_an_unsupported_metric_version_is_refused_even_when_both_runs_declare_it(self):
        # Two runs that agree on a version this script does not implement still cannot be read by its rules.
        for tree in (self.first, self.second):
            self.late_image(tree, tree.artifact("perf-macos", version=999))
        with self.assertRaises(flags.NotComparable):
            self.compare()

    def test_a_same_head_comparison_needs_the_same_base_harness_and_settings(self):
        # Same-head runs measured against different bases, by different harnesses or with different dataset
        # settings are not repeats of one comparison, so their flags are not split.
        variants = {"base": {"base": OTHER_BASE}, "harness": {"harness": "cd" * 32},
                    "settings": {"settings": dict(SETTINGS, short=False)}}
        for name, changes in variants.items():
            with self.subTest(differs=name):
                first, second = self.fresh_tree(), self.fresh_tree()
                self.late_image(first, first.artifact("perf-macos"))
                artifact = second.artifact("perf-macos", **changes)
                result_fields = {"harness_hash": changes["harness"]} if "harness" in changes else {}
                result_fields.update({"short": False} if "settings" in changes else {})
                second.run(artifact, "S11", "release", 1, "base", [transition(40.0)], result_fields=result_fields)
                second.run(artifact, "S11", "release", 2, "head", [transition(90.0)], result_fields=result_fields)
                with self.assertRaises(flags.NotComparable):
                    flags.compare_runs(flags.load_run(first.root), flags.load_run(second.root))

    def test_allowing_different_heads_never_waives_the_metric_contract(self):
        # Different heads may be listed side by side, but only under the same flag metrics and dataset settings.
        self.late_image(self.first, self.first.artifact("perf-macos-a"))
        artifact = self.second.artifact("perf-macos-b", head=OTHER_HEAD, settings=dict(SETTINGS, short=False))
        self.second.run(artifact, "S11", "release", 1, "base", [transition(40.0)], result_fields={"short": False})
        self.second.run(artifact, "S11", "release", 2, "head", [transition(90.0)], result_fields={"short": False})
        with self.assertRaises(flags.NotComparable):
            self.compare(allow_different_heads=True)

    def test_identities_and_settings_are_typed_and_agree_within_a_platform(self):
        # An untyped setting or head, or two artifacts of one platform with different settings, is refused before
        # any flag is computed.
        broken = {"untyped counters": {"settings": dict(SETTINGS, counters="yes")},
                  "settings without features": {"settings": {"short": True, "counters": False}},
                  "short head": {"head": "123"}}
        for name, changes in broken.items():
            with self.subTest(identity=name):
                tree = self.fresh_tree()
                self.late_image(tree, tree.artifact("perf-macos", **changes))
                with self.assertRaises(flags.NotComparable):
                    flags.load_run(tree.root)
        tree = self.fresh_tree()
        self.late_image(tree, tree.artifact("perf-macos-a"))
        tree.artifact("perf-macos-b", settings=dict(SETTINGS, counters=True))
        with self.assertRaises(flags.NotComparable):
            flags.load_run(tree.root)

    def test_duplicate_artifacts_and_superseded_attempts_count_once(self):
        # The same artifact downloaded twice contributes its runs once; a rerun attempt replaces the earlier one.
        first = self.first.artifact("perf-macos-a")
        self.late_image(self.first, first)
        shutil.copytree(first, self.first.root / "perf-macos-a-copy")
        self.assertEqual(self.completion_checks(self.first), [(1, 1, 90.0)])
        rerun = self.first.artifact("perf-macos-a-attempt2", attempt=2)
        self.late_image(self.first, rerun, head_ms=30.0)
        self.assertEqual(self.completion_checks(self.first), [(1, 1, 30.0)])
        # Two copies of one attempt whose inventories disagree are not a duplicate, and are refused.
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos-a")
        self.late_image(tree, artifact)
        copy = tree.root / "perf-macos-a-copy"
        shutil.copytree(artifact, copy)
        identity = json.loads((copy / "run-identity.json").read_text(encoding="utf-8"))
        identity["sets"][0]["head"]["accepted"] = []
        write_json(copy / "run-identity.json", identity)
        with self.assertRaises(flags.NotComparable):
            flags.load_run(tree.root)

    def test_a_rerun_that_accepted_nothing_leaves_the_evidence_missing(self):
        # The rerun's inventory replaces the earlier attempt's even when it accepted no run: its values are
        # missing, never the superseded measurements.
        self.late_image(self.first, self.first.artifact("perf-macos-a"))
        rerun = self.first.artifact("perf-macos-a-attempt2", attempt=2)
        self.first.run(rerun, "S11", "release", 1, "base", [transition(40.0)], kind="focus")
        self.first.run(rerun, "S11", "release", 2, "head", [transition(90.0)], kind="focus")
        self.assertEqual(self.completion_checks(self.first), [])

    def test_every_set_on_disk_needs_its_inventory_entry(self):
        # A rerun whose identity leaves out a set it ran cannot fall back to the superseded attempt's values: an
        # attempt directory of a set the inventory does not list is refused. A set listed blocked on both sides
        # that never created a directory still loads.
        tree = self.fresh_tree()
        self.late_image(tree, tree.artifact("perf-macos-a"))
        rerun = tree.artifact("perf-macos-a-attempt2", attempt=2)
        tree.run(rerun, "S11", "release", 1, "base", [transition(40.0)], kind="focus")
        tree.run(rerun, "S11", "release", 2, "head", [transition(90.0)], kind="focus")
        tree.identities[rerun]["sets"].clear()
        tree.save(rerun)
        self.assert_refused(tree, "a physical set missing from its inventory")
        # A set is its label and its dataset: a listed timed set does not explain a counters directory beside it.
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos")
        self.late_image(tree, artifact)
        tree.run(artifact, "S11", "release", 3, "head", [transition(90.0)], dataset="counters", kind="focus")
        tree.identities[artifact]["sets"] = [entry for entry in tree.identities[artifact]["sets"]
                                             if entry["dataset"] == "timed"]
        tree.save(artifact)
        self.assert_refused(tree, "a dataset directory its inventory does not list")
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos")
        self.late_image(tree, artifact)
        tree.entry(artifact, "S2/default", "timed")
        for side_name in ("base", "head"):
            tree.side_status(artifact, "S2/default", "timed", side_name, "the harness did not build")
        self.assertEqual(self.completion_checks(tree), [(1, 1, 90.0)])

    def test_a_side_that_failed_contributes_none_of_its_valid_attempts(self):
        # A side the comparison ended failed or blocked discards its valid attempts, even with their files present;
        # an inventory that both fails a side and accepts its runs is refused.
        artifact = self.first.artifact("perf-macos-a")
        self.late_image(self.first, artifact)
        self.first.entry(artifact, "S11/release", "timed")["head"]["accepted"].clear()
        self.first.side_status(artifact, "S11/release", "timed", "head", "no 1 valid runs after 3 retries")
        self.assertEqual(self.completion_checks(self.first), [])
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos")
        self.late_image(tree, artifact)
        tree.side_status(artifact, "S11/release", "timed", "head", "no 1 valid runs after 3 retries")
        with self.assertRaises(flags.NotComparable):
            flags.load_run(tree.root)

    def test_an_accepted_execution_must_carry_matching_accepted_evidence(self):
        # Each accepted attempt is bound to its final classification, outcome and result: any of them naming
        # another run, side, status, mode, dataset or a broken phase schema is refused, never counted or skipped.
        cases = {
            "result status": {"result_fields": {"status": "invalid"}},
            "result scenario": {"result_fields": {"scenario": "S2"}},
            "result variant": {"result_fields": {"variant": "default"}},
            "unmanaged result": {"result_fields": {"managed": False}},
            "managed as a number": {"result_fields": {"managed": 1}},
            "result length": {"result_fields": {"short": False}},
            "counter gate on a timed run": {"result_fields": {"frame_counters": "on"}},
            "another harness": {"result_fields": {"harness_hash": "cd" * 32}},
            "phase schema": {"result_fields": {"phases": [dict(transition(90.0), completion_ms=-1.0)]}},
            "classification side": {"classification_side": "base"},
            "no classification": {"classification": None, "accepted": True},
            "classified focus": {"classification": "focus", "accepted": True},
            "outcome focus": {"kind": "focus", "classification": "valid", "accepted": True},
            "outcome scenario": {"outcome_fields": {"scenario": "S2"}},
        }
        for name, changes in cases.items():
            with self.subTest(evidence=name):
                tree = self.fresh_tree()
                artifact = tree.artifact("perf-macos")
                tree.run(artifact, "S11", "release", 1, "base", [transition(40.0)])
                tree.run(artifact, "S11", "release", 2, "head", [transition(90.0)], **changes)
                with self.assertRaises(flags.NotComparable):
                    flags.load_run(tree.root)
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos")
        self.late_image(tree, artifact)
        tree.entry(artifact, "S11/release", "timed")["head"]["accepted"].append("09-head")
        tree.save(artifact)
        with self.subTest(evidence="accepted attempt without files"), self.assertRaises(flags.NotComparable):
            flags.load_run(tree.root)

    def test_attempts_the_comparison_rejected_contribute_nothing(self):
        # An attempt whose final classification is not valid is not evidence, whatever its result says.
        artifact = self.first.artifact("perf-macos-a")
        self.late_image(self.first, artifact)
        self.first.run(artifact, "S11", "release", 4, "head", [transition(5000.0)], kind="focus")
        self.assertEqual(self.completion_checks(self.first), [(1, 1, 90.0)])

    def assert_refused(self, tree, why):
        """`tree` loads as not comparable, and main exits 2 for it rather than raising."""
        try:
            with self.assertRaises(flags.NotComparable, msg=why):
                flags.load_run(tree.root)
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                code = flags.main([str(tree.root), str(tree.root)])
        except Exception as error:  # noqa: BLE001 - any other exception is the failure under test
            # When: the loader raised something other than NotComparable, malformed evidence escaped as a crash.
            self.fail(f"{why}: {type(error).__name__}: {error}")
        self.assertEqual(code, flags.EXIT_NOT_COMPARABLE, why)

    def test_the_inventory_must_equal_the_valid_attempts_on_disk(self):
        # A healthy side's accepted list is exactly the attempts whose final classification is valid: an omitted
        # valid attempt, an alias of another attempt, or one that escapes the artifact is refused.
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos")
        self.late_image(tree, artifact)
        tree.run(artifact, "S11", "release", 3, "head", [transition(100.0)], accepted=False)
        self.assert_refused(tree, "a valid attempt left out of the inventory")
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos")
        real = self.late_image(tree, artifact, head_ms=10.0)
        tree.run(artifact, "S11", "release", 3, "head", [transition(100.0)])
        (real.parent / "04-head").symlink_to(real, target_is_directory=True)
        tree.entry(artifact, "S11/release", "timed")["head"]["accepted"].append("04-head")
        tree.save(artifact)
        self.assert_refused(tree, "an alias of another attempt")
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos")
        self.late_image(tree, artifact)
        outside = self.fresh_tree()
        elsewhere = outside.run(outside.artifact("elsewhere"), "S11", "release", 9, "head", [transition(1.0)])
        (artifact / "runs" / "S11-release" / "timed" / "05-head").symlink_to(elsewhere, target_is_directory=True)
        tree.entry(artifact, "S11/release", "timed")["head"]["accepted"].append("05-head")
        tree.save(artifact)
        self.assert_refused(tree, "an attempt outside the artifact")
        tree = self.fresh_tree()
        artifact = tree.artifact("perf-macos")
        head = self.late_image(tree, artifact)
        (head / "scratch" / "result.json").unlink()
        (head / "scratch" / "result.json").symlink_to(head.parent / "01-base" / "scratch" / "result.json")
        self.assert_refused(tree, "a result linked from another attempt")

    def test_conflicting_duplicates_are_refused_whatever_their_order(self):
        # Two copies of one attempt whose evidence differs are not a duplicate, whichever name sorts first.
        for copy_name in ("000-copy", "z-copy"):
            with self.subTest(copy=copy_name):
                tree = self.fresh_tree()
                artifact = tree.artifact("perf-macos-a")
                self.late_image(tree, artifact)
                copy = tree.root / copy_name
                shutil.copytree(artifact, copy)
                result_path = copy / "runs" / "S11-release" / "timed" / "02-head" / "scratch" / "result.json"
                result = json.loads(result_path.read_text(encoding="utf-8"))
                result["phases"][0]["completion_ms"] = 10.0
                write_json(result_path, result)
                self.assert_refused(tree, f"a conflicting copy named {copy_name}")

    def test_an_accepted_result_must_pass_the_full_result_schema(self):
        # The loader validates each accepted result as perf-compare does, under the harness's capabilities;
        # malformed evidence is refused with exit 2, never read and never a traceback.
        cases = {
            "unknown counter state": {"result_fields": {"frame_counters": "banana"}},
            "another schema version": {"result_fields": {"schema_version": 999}},
            "a failing exit": {"result_fields": {"exit_code": 4}},
            "untyped dispatches": {"result_fields": {"phases": [dict(transition(90.0), dispatch_ms="bad")]}},
            "untyped start": {"result_fields": {"phases": [dict(transition(90.0), start_unix_s="bad")]}},
            "a null phase": {"result_fields": {"phases": [transition(90.0), None]}},
            "a process that did not exit 0": {"outcome_fields": {"exit_code": 4}},
        }
        for name, changes in cases.items():
            with self.subTest(evidence=name):
                tree = self.fresh_tree()
                artifact = tree.artifact("perf-macos")
                tree.run(artifact, "S11", "release", 1, "base", [transition(40.0)])
                tree.run(artifact, "S11", "release", 2, "head", [transition(90.0)], **changes)
                self.assert_refused(tree, name)
        with self.subTest(evidence="kinds without the capability"):
            tree = self.fresh_tree()
            self.late_image(tree, tree.artifact("perf-macos",
                                                capabilities=dict(CAPABILITIES, phase_kinds=None)))
            self.assert_refused(tree, "kinded phases from a harness that declares none")
        with self.subTest(evidence="attribution without the capability"):
            tree = self.fresh_tree()
            artifact = tree.artifact("perf-macos")
            tree.run(artifact, "S11", "release", 1, "base", [transition(40.0)])
            unavailable = {"state": "unavailable", "reason": "api-disabled"}
            tree.run(artifact, "S11", "release", 2, "head", [dict(transition(90.0), s10_attribution=unavailable)])
            self.assert_refused(tree, "an attribution record from a harness that declares none")
        with self.subTest(evidence="attribution capability without the recorded build"):
            # Under a declared schema the validator runs as perf-compare does: the result must say whether its
            # build calls the attribution API.
            tree = self.fresh_tree()
            artifact = tree.artifact("perf-macos", capabilities=dict(CAPABILITIES, s10_attribution=1))
            tree.run(artifact, "S11", "release", 1, "base", [transition(40.0)])
            tree.run(artifact, "S11", "release", 2, "head", [transition(90.0)])
            self.assert_refused(tree, "a result with no s10_attribution_api under the declared schema")
        # An S2/default result under the declared echo-timeline schema: a credited sample whose timeline is an
        # unavailable object is accepted; the same sample with a null timeline, or a timeline from a harness that
        # declares none, is refused.
        unavailable = {key: None for key in flags.compare.TIMELINE_KEYS}
        unavailable.update(schema=1, availability="unavailable", unavailable_reason="cfg-off")

        def timeline_latency(echo_timeline, coverage):
            sample = {"inject_unix_s": 1.0, "latency_ms": 8.0, "attributed": True, "reason": "credited",
                      "split": None, "split_reason": "unsupported", "echo_timeline": echo_timeline}
            return {"samples": [sample], "attributed": 1, "total": 1, "coverage": 1.0, "split_schema": 1,
                    "split_count": 0, "split_reasons": {"unsupported": 1}, "split_coverage": 0.0,
                    "echo_timeline_coverage": coverage}

        declared = dict(CAPABILITIES, echo_timeline_schema=1)
        honest = timeline_latency(unavailable, flags.compare.timeline_coverage([unavailable]))
        for name, capabilities, latency, refused in (
                ("an unavailable timeline under the declared schema", declared, honest, False),
                ("a credited S2 sample with no timeline object", declared, timeline_latency(None, None), True),
                ("a timeline from a harness that declares none", CAPABILITIES, honest, True)):
            with self.subTest(evidence=name):
                tree = self.fresh_tree()
                artifact = tree.artifact("perf-macos", capabilities=capabilities)
                for index, side in ((1, "base"), (2, "head")):
                    tree.run(artifact, "S2", "default", index, side, [sustained(120)],
                             result_fields={"latency": latency})
                if refused:
                    self.assert_refused(tree, name)
                else:
                    flags.load_run(tree.root)
        # Without the split schema the latency is the legacy shape; a timeline then needs its own declaration, and
        # that declaration needs the split schema it depends on.
        legacy_sample = {"inject_unix_s": 1.0, "latency_ms": 8.0, "attributed": True, "reason": "credited",
                         "echo_timeline": unavailable}
        legacy_latency = {"samples": [legacy_sample], "attributed": 1, "total": 1, "coverage": 1.0,
                          "echo_timeline_coverage": flags.compare.timeline_coverage([unavailable])}
        undeclared = dict(CAPABILITIES, latency_split_schema=None, phase_kinds=None)
        # The identity alone is refused: the timeline schema extends the split schema it is declared without.
        self.assertEqual(flags._capabilities_problem(dict(undeclared, echo_timeline_schema=1)),
                         "capability echo_timeline_schema is declared without latency_split_schema")
        self.assertIsNone(flags._capabilities_problem(dict(CAPABILITIES, echo_timeline_schema=1)))
        for name, capabilities in (
                ("a timeline declared without the split schema", dict(undeclared, echo_timeline_schema=1)),
                ("a timeline from a harness that declares neither", undeclared)):
            with self.subTest(evidence=name):
                tree = self.fresh_tree()
                artifact = tree.artifact("perf-macos", capabilities=capabilities)
                for index, side in ((1, "base"), (2, "head")):
                    phase = {key: value for key, value in sustained(120).items()
                             if key not in ("kind", "first_present_ms", "last_present_ms", "first_present_seq",
                                            "last_present_seq", "nonpresenting_redraws")}
                    tree.run(artifact, "S2", "default", index, side, [phase], result_fields={"latency": legacy_latency})
                self.assert_refused(tree, name)
        # An unrepresentable instant through the artifact reader: the chain form with a valid split is accepted;
        # readiness without the split, with or without a flood interval, and a publication flag are refused.
        readiness = {"outcome": "native_request", "loop_seq": 4, "site": "output_service"}
        late = {key: None for key in flags.compare.TIMELINE_KEYS}
        late.update(schema=1, availability="recorded", overflow=False, read_stamp="stamped", ordering="clock-order",
                    ordering_reason="unrepresentable-instant", readiness=readiness, credited_dispatch_seq=3,
                    admission="fallback", permit_identity="none", tick_qualified=False, token=7, window=1, pane=2)
        split = {"input_to_parse_ms": 2.0, "parse_to_publication_ms": 3.0, "publication_to_present_ms": 5.0,
                 "delivery_lag_us": 40.0, "delivery": "sent", "coalesced": False, "sync_open": False,
                 "echo_generation": 3}

        def split_timeline_latency(entry, split_reason):
            sample = {"inject_unix_s": 1.0, "latency_ms": 10.0, "attributed": True, "reason": "credited",
                      "split": split if split_reason == "split" else None, "split_reason": split_reason,
                      "echo_timeline": entry}
            return {"samples": [sample], "attributed": 1, "total": 1, "coverage": 1.0, "split_schema": 1,
                    "split_count": int(split_reason == "split"), "split_reasons": {split_reason: 1},
                    "split_coverage": float(split_reason == "split"),
                    "echo_timeline_coverage": flags.compare.timeline_coverage([entry])}

        for name, entry, split_reason, refused in (
                ("a late unrepresentable instant with its split", late, "split", False),
                ("readiness without the split", late, "pane-not-shown", True),
                ("readiness and a flood interval without the split", dict(late, flood_services=4, m3_complete=True),
                 "pane-not-shown", True),
                ("a publication flag on an unrepresentable instant", dict(late, ready_before_publication=True),
                 "split", True)):
            with self.subTest(evidence=name):
                tree = self.fresh_tree()
                artifact = tree.artifact("perf-macos", capabilities=declared)
                for index, side in ((1, "base"), (2, "head")):
                    tree.run(artifact, "S2", "default", index, side, [sustained(120)],
                             result_fields={"latency": split_timeline_latency(entry, split_reason)})
                if refused:
                    self.assert_refused(tree, name)
                else:
                    flags.load_run(tree.root)
        with self.subTest(evidence="a capability map missing a known key"):
            tree = self.fresh_tree()
            self.late_image(tree, tree.artifact("perf-macos",
                                                capabilities={"latency_split_schema": 1, "phase_kinds": 1}))
            self.assert_refused(tree, "capabilities without the attribution key")
        with self.subTest(evidence="untyped capabilities"):
            tree = self.fresh_tree()
            self.late_image(tree, tree.artifact("perf-macos", capabilities={"phase_kinds": "yes"}))
            self.assert_refused(tree, "capabilities that are not the known map")
        with self.subTest(evidence="an unsupported capability value"):
            tree = self.fresh_tree()
            self.late_image(tree, tree.artifact("perf-macos",
                                                capabilities=dict(CAPABILITIES, phase_kinds=7)))
            self.assert_refused(tree, "a phase-kinds schema this script cannot validate")

    def test_a_result_the_validator_cannot_read_is_refused_not_raised(self):
        # A result shape the validator itself fails on is malformed evidence: refused with exit 2, never a
        # traceback. The validator is made to fail directly, since no known shape makes it raise today.
        tree = self.fresh_tree()
        self.late_image(tree, tree.artifact("perf-macos"))

        def unreadable(*_args, **_kwargs):
            raise TypeError("unhashable type: 'list'")
        original = flags.compare.validate_result
        flags.compare.validate_result = unreadable
        try:
            self.assert_refused(tree, "a result the validator cannot read")
        finally:
            flags.compare.validate_result = original

    def test_the_report_prints_each_runs_identity_and_the_split(self):
        # The text names both runs' heads, bases and harness hashes, and each section, with exit 0.
        self.late_image(self.first, self.first.artifact("perf-macos-a"))
        self.late_image(self.second, self.second.artifact("perf-macos-b"))
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            code = flags.main([str(self.first.root), str(self.second.root)])
        self.assertEqual(code, 0)
        text = printed.getvalue()
        for expected in (f"head {HEAD}", f"base {BASE}", f"harness {HARNESS}", "Flagged in both runs",
                         "Flagged in one run", "Evidence missing", "macos S11/release timed image completion median"):
            self.assertIn(expected, text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
