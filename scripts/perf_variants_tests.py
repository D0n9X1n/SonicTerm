"""Perf variants tests."""

from __future__ import annotations
import dataclasses
import json
import unittest

import perf_build_tests
import perf_cell_layout
import perf_comparison_rows
import perf_comparison_tests
import perf_counter_schema_tests
import perf_driver_tests
import perf_harness_sources
import perf_home_tests
import perf_memory
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_prebuilt_tests
import perf_result_schema
import perf_result_tests
import perf_scenarios
import perf_settings


# A tree that declares only perf-frame-texture, and one that declares both perf features.
FRAME_TEXTURE_TABLE = "\n[features]\nperf-frame-texture = []\n"
BOTH_FEATURES_TABLE = "\n[features]\nperf-counters = []\nperf-frame-texture = []\n"
FRAME_MANIFEST = perf_home_tests.HEAD_MANIFEST + FRAME_TEXTURE_TABLE
BOTH_MANIFEST = perf_home_tests.HEAD_MANIFEST + BOTH_FEATURES_TABLE
# A listing whose S4 caps its default variant at one short run; S4 has no Windows delivery replay.
CAPPED_LISTING = {"schema_version": 1, "scenarios": [
    {"id": "S4", "variants": ["default"], "title": "Stream", "timeout_s": 120, "short_timeout_s": 30,
     "run_caps": {"default": 1}}]}


class RunCapListTests(unittest.TestCase):
    """`--list` may cap a variant's short-mode runs; anything but a positive count per listed variant is refused."""

    def entry(self, **extra):
        return {"schema_version": 1, "scenarios": [
            {"id": "S11", "variants": ["default", "release"], "title": "Image", "timeout_s": 420,
             "short_timeout_s": 300, **extra}]}

    def test_absent_caps_leave_the_scenario_uncapped(self):
        # Without `run_caps` no variant is capped, as before.
        scenario = perf_scenarios.parse_scenario_list(json.dumps(self.entry()))[0]
        self.assertEqual(scenario.run_caps, ())
        self.assertIsNone(scenario.cap("release"))

    def test_caps_are_read_per_variant(self):
        # Each listed variant's cap is kept; an unlisted variant stays uncapped.
        scenario = perf_scenarios.parse_scenario_list(json.dumps(self.entry(run_caps={"release": 1})))[0]
        self.assertEqual((scenario.cap("release"), scenario.cap("default")), (1, None))

    def test_malformed_caps_are_refused(self):
        # Zero, a negative or non-integer count, an unknown variant and a non-object all fail the listing.
        for caps in ({"release": 0}, {"release": -1}, {"release": "1"}, {"release": True}, {"release": 1.5},
                     {"gdi": 1}, [["release", 1]], "release"):
            with self.subTest(caps=caps), self.assertRaisesRegex(ValueError, "malformed scenario entry"):
                perf_scenarios.parse_scenario_list(json.dumps(self.entry(run_caps=caps)))

    def test_capped_runs_apply_in_short_mode_only(self):
        # min(requested, cap) under --short; a cap above the request keeps the request; release mode is uncapped.
        scenario = perf_scenarios.parse_scenario_list(json.dumps(self.entry(run_caps={"release": 2})))[0]
        self.assertEqual(perf_scenarios.capped_runs(scenario, "release", 5, short=True), 2)
        self.assertEqual(perf_scenarios.capped_runs(scenario, "release", 1, short=True), 1)
        self.assertEqual(perf_scenarios.capped_runs(scenario, "release", 5, short=False), 5)
        self.assertEqual(perf_scenarios.capped_runs(scenario, "default", 5, short=True), 5)


class RunCapCompareTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    """A capped variant runs min(requested, cap) valid runs per side in every set, and the table says so."""

    def capped(self, *options, **kwargs):
        return self.compare(listing=CAPPED_LISTING, scenarios=("S4",), options=("--runs", "5", *options), **kwargs)

    def per_side(self, plans, counters=False):
        return [sum(1 for plan in plans if plan.side == side and plan.counters == counters) for side in perf_settings.SIDES]

    def test_no_metadata_leaves_the_runs_unchanged(self):
        # The plain listing has no caps: five valid runs per side under --short, as before.
        _code, _gate, _calls, plans, _work, _out = self.compare(options=("--runs", "5", "--short"))
        self.assertEqual(self.per_side(plans), [5, 5])

    def test_the_cap_applies_to_runs_and_counters_runs(self):
        # --runs 5 and --counters-runs 2 both become one run per side.
        code, _gate, _calls, plans, _work, _out = self.capped(
            "--short", "--counters", "--counters-runs", "2", head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST,
            base_manifest=perf_counter_schema_tests.BASE_COUNTERS_MANIFEST)
        self.assertEqual(code, perf_settings.EXIT_PASS)
        self.assertEqual(self.per_side(plans), [1, 1])
        self.assertEqual(self.per_side(plans, counters=True), [1, 1])

    def test_the_laps_and_alloc_sets_are_capped(self):
        # Every set of a capped variant takes the cap, laps and alloc included.
        code, _gate, _calls, plans, _work, _out = self.capped("--short", "--laps", "--alloc")
        self.assertEqual(code, perf_settings.EXIT_PASS)
        for side in perf_settings.SIDES:
            self.assertEqual(sum(1 for plan in plans if plan.side == side and plan.laps), 1, side)
            self.assertEqual(sum(1 for plan in plans if plan.side == side
                                 and plan.binary.name.startswith(perf_settings.ALLOC_EXAMPLE)), 1, side)

    def test_release_mode_ignores_the_cap(self):
        # A release comparison runs every requested run.
        _code, _gate, _calls, plans, _work, _out = self.compare(
            listing=CAPPED_LISTING, scenarios=("S4",), options=("--runs", "3"))
        self.assertEqual(self.per_side(plans), [3, 3])

    def test_capped_rows_and_the_document_disclose_the_cap(self):
        # Each capped row is labelled `(runs N of M)` and comparison.md lists the capped variants.
        _code, _gate, _calls, _plans, _work, out = self.capped("--short")
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("| S4/default (runs 1 of 5) | status | 1 valid run | 1 valid run |", document)
        self.assertIn("Capped variants (--short): S4/default timed 1 of 5 runs.", document)
        _code, _gate, _calls, _plans, _work, out = self.compare(options=("--runs", "5", "--short"))
        self.assertNotIn("Capped variants", (out / "comparison.md").read_text(encoding="utf-8"))


def memory_sample(unix_s, renderer_bytes=16 * 1024 * 1024):
    return perf_memory.MemorySample(unix_s, 100 * 1024 * 1024, renderer_bytes, 0)


class MemoryFreshnessTests(unittest.TestCase):
    """A checkpoint with fresh_after_unix_s reads memory only from a sample taken at or after that time."""

    def test_absent_metadata_keeps_the_latest_earlier_sample(self):
        samples = [memory_sample(10.0), memory_sample(40.0)]
        self.assertEqual(perf_cell_layout.memory_at(samples, 50.0).unix_s, 40.0)

    def test_a_sample_before_the_threshold_reads_unavailable(self):
        # The latest sample at or before the checkpoint predates freshness, so the reading is unavailable.
        samples = [memory_sample(10.0), memory_sample(40.0)]
        self.assertIsNone(perf_cell_layout.memory_at(samples, 80.0, fresh_after_unix_s=45.0))

    def test_samples_exactly_at_and_after_the_threshold_are_used(self):
        for sample_s in (45.0, 70.0):
            with self.subTest(sample_s=sample_s):
                samples = [memory_sample(10.0), memory_sample(sample_s)]
                self.assertEqual(perf_cell_layout.memory_at(samples, 80.0, fresh_after_unix_s=45.0).unix_s, sample_s)

    def test_non_finite_or_negative_values_are_schema_problems(self):
        # fresh_after_unix_s and frame_texture_bytes must be finite and non-negative; anything else fails the schema.
        base = {"index": 0, "label": "released", "unix_s": 80.0, "footprint_file": None}
        for key, value in (("fresh_after_unix_s", float("nan")), ("fresh_after_unix_s", float("inf")),
                           ("fresh_after_unix_s", -1.0), ("fresh_after_unix_s", "45"),
                           ("fresh_after_unix_s", True), ("frame_texture_bytes", -4),
                           ("frame_texture_bytes", 4.5), ("frame_texture_bytes", True)):
            with self.subTest(key=key, value=value):
                checkpoint = dict(base, **{key: value})
                self.assertFalse(perf_result_schema._checkpoint_ok(checkpoint))
                problems = perf_result_schema.validate_result(perf_result_tests.valid_result(checkpoints=[checkpoint]), perf_memory_tests.HARNESS_HASH, 0)
                self.assertTrue(any("checkpoints" in problem for problem in problems), problems)
        self.assertTrue(perf_result_schema._checkpoint_ok(dict(base, fresh_after_unix_s=45.0, frame_texture_bytes=4)))

    def test_run_metrics_report_a_released_reading_only_when_fresh(self):
        # The released row appears only when its checkpoint's own sample is fresh; a tagged sample taken
        # before fresh_after_unix_s reads unavailable.
        point = {"index": 1, "label": "released", "unix_s": 80.0, "footprint_file": None,
                 "fresh_after_unix_s": 45.0}
        tag = {"checkpoint_index": 1, "checkpoint_label": "released", "checkpoint_attempt": 1,
               "checkpoint_complete": True}
        result = perf_result_tests.valid_result(checkpoints=[point], checkpoint_memory="supported")
        stale = perf_outcomes_tests.make_outcome(result=result, memory=[dataclasses.replace(memory_sample(40.0), **tag)])
        fresh = perf_outcomes_tests.make_outcome(result=result, memory=[dataclasses.replace(memory_sample(46.0), **tag)])
        self.assertNotIn(("released renderer_total_bytes", "MiB", "run"), perf_metrics.run_metrics(stale))
        self.assertIn(("released renderer_total_bytes", "MiB", "run"), perf_metrics.run_metrics(fresh))


class GridBytesPerPaneRowTests(unittest.TestCase):
    """A checkpoint whose own tagged sample carries grid fields gets a grid-bytes-per-pane row, read through the
    authoritative-sample rule; one without the fields gets none, and a harness without the hook reads n/a."""

    def test_the_row_is_the_per_pane_grid_figure_in_mebibytes(self):
        # Resolve this runtime-only dependency after module initialization.
        import perf_atlas_tests
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}
        supported = perf_result_tests.valid_result(checkpoints=[point], checkpoint_memory="supported")
        with_grid = perf_memory.MemorySample(60.0, None, 1, 0, checkpoint_index=0, checkpoint_label="end",
                                      checkpoint_attempt=1, checkpoint_complete=True,
                                      grid_visible_bytes=1048576, grid_history_bytes=2 * 1048576,
                                      grid_alternate_bytes=1048576, panes_sampled=2)
        metrics = perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=supported, memory=[with_grid]))
        self.assertEqual(metrics[("end grid bytes per pane", "MiB", "run")], 2.0)
        without = perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=supported, memory=[perf_atlas_tests.tagged_sample(0, 1, True)]))
        self.assertNotIn(("end grid bytes per pane", "MiB", "run"), without)
        # An untagged periodic sample with grid fields is never substituted for a checkpoint's own reading.
        periodic = perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[point]),
                                                 memory=[perf_memory.MemorySample(60.0, None, 1, 0,
                                                                           grid_visible_bytes=1048576,
                                                                           grid_history_bytes=0,
                                                                           grid_alternate_bytes=0,
                                                                           panes_sampled=1)]))
        self.assertIsInstance(periodic[("end grid bytes per pane", "MiB", "run")], perf_metrics.NotAvailable)


class FrameTextureRowTests(unittest.TestCase):
    """The end checkpoint's frame_texture_bytes becomes its own row, n/a on a base that never reports it."""

    def test_the_row_comes_from_the_checkpoint_not_from_memory(self):
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None, "frame_texture_bytes": 4}
        head = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[point]))])
        base = perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome()])
        self.assertEqual(perf_metrics.run_metrics(head.outcomes[0])[("end frame_texture_bytes", "B", "run")], 4)
        row = perf_comparison_tests.row_for(perf_comparison_rows.comparison_rows("S11/gdi", base, head), "end frame_texture_bytes (B)")
        self.assertEqual(row[2:4], ["n/a", "4.00 (4.00–4.00)"])


class FrameTextureFeatureTests(perf_prebuilt_tests.PrebuiltHarness, unittest.TestCase):
    """perf-frame-texture reaches only a tree that declares it, in every build path, and binds a prebuilt artifact."""

    def build_argv(self, gate, side):
        return next(step.argv for step in gate.steps if step.id == f"build-{side}-perf_scenarios")

    def test_a_building_comparison_passes_the_feature_only_to_a_declaring_tree(self):
        # The head declares it and builds with it; the base does not and builds without it. Windows per-shard
        # builds go through the same build_sides catalog.
        _code, gate, _calls, _plans, _work, out = self.compare(head_manifest=FRAME_MANIFEST)
        self.assertIn("perf-frame-texture", self.build_argv(gate, "head"))
        self.assertNotIn("--features", self.build_argv(gate, "base"))
        self.assertIn("- Built with `--features perf-frame-texture`: head",
                      (out / "comparison.md").read_text(encoding="utf-8"))
        _code, gate, _calls, _plans, _work, _out = self.compare(head_manifest=BOTH_MANIFEST)
        head = self.build_argv(gate, "head")
        self.assertEqual(head[head.index("--features") + 1], "perf-counters,perf-frame-texture")

    def test_build_only_records_the_feature_in_the_manifest(self):
        binaries, _digest = self.produce(head_manifest=FRAME_MANIFEST)
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["features"], {"base": [], "head": [perf_harness_sources.FRAME_TEXTURE_FEATURE]})

    def test_load_prebuilt_refuses_a_feature_mismatch(self):
        # A consumer whose head tree does not declare the feature refuses binaries built with it.
        binaries, digest = self.produce(head_manifest=FRAME_MANIFEST)
        with self.assertRaisesRegex(ValueError, "refusing the prebuilt binaries: features"):
            self.consume(binaries, digest)
        code, *_rest = self.consume(binaries, digest, head_manifest=FRAME_MANIFEST)
        self.assertEqual(code, perf_settings.EXIT_PASS)

    def test_the_gate_reviews_one_build_per_feature_set(self):
        # Every feature combination has the gate's own reviewed steps; the plain and counters sets are the old ones.
        # Five features give 32 ordered subsets, each with its four side/example build steps. The catalog is
        # checked as data: no subset is compiled here.
        catalog = perf_build_tests.REAL_GATE.PERF_FEATURE_BUILDS
        counters, texture, hook, echo, trim = perf_harness_sources.PERF_FEATURES
        self.assertEqual((echo, trim), ("perf-echo-trace", "perf-hook-trim"))
        subsets = [()]
        for feature in perf_harness_sources.PERF_FEATURES:
            subsets += [subset + (feature,) for subset in subsets]
        self.assertEqual(set(catalog), set(subsets))
        self.assertEqual(len(catalog), 32)
        self.assertIn((counters, texture, hook, echo, trim), catalog)
        self.assertIn((counters, hook, echo), catalog, "a base without the trim hook keeps its own step")
        self.assertEqual(sum(len(steps) for steps in catalog.values()), 128)
        for features, steps in catalog.items():
            for step in steps.values():
                self.assertTrue(perf_build_tests.REAL_GATE._reviewed_step(step))
                if features:
                    self.assertEqual(step.argv[step.argv.index("--features") + 1], ",".join(features))
                else:
                    self.assertNotIn("--features", step.argv)
        for step_id, step in catalog[()].items():
            self.assertIs(step, perf_build_tests.REAL_GATE.PERF_BUILDS[step_id])
        for step_id, step in catalog[("perf-counters",)].items():
            self.assertIs(step, perf_build_tests.REAL_GATE.PERF_COUNTER_BUILDS[step_id])
