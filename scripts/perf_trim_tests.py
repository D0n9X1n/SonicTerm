"""Perf trim tests."""

from __future__ import annotations
import copy
import json
from pathlib import Path
import tempfile
import unittest

import perf_atlas_tests
import perf_build_tests
import perf_foreground_tests
import perf_harness_sources
import perf_home_tests
import perf_memory
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_prebuilt_tests
import perf_processes_tests
import perf_result_schema
import perf_result_tests
import perf_settings



class TrimHookFeatureTests(perf_prebuilt_tests.PrebuiltHarness, unittest.TestCase):
    """perf-hook-trim reaches only a tree that defines the trim hook, through the gate's reviewed steps."""

    def build_step(self, gate, side):
        return next(step for step in gate.steps if step.id == f"build-{side}-perf_scenarios")

    def test_a_tree_needs_the_declaration_and_the_trim_method(self):
        # The feature alone, or the method only in a comment, does not make a tree trim-capable.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / perf_settings.APP_MANIFEST).parent.mkdir(parents=True)
            (root / perf_settings.APP_MANIFEST).write_text(perf_atlas_tests.TRIM_MANIFEST, encoding="utf-8")
            source = root / perf_harness_sources.APP_SOURCE_DIRECTORY / "app" / "retention.rs"
            source.parent.mkdir(parents=True)
            method = perf_harness_sources.HOOK_METHODS[perf_harness_sources.TRIM_HOOK_FEATURE]
            self.assertFalse(perf_harness_sources.tree_supports_hook(root, perf_harness_sources.TRIM_HOOK_FEATURE, method))
            source.write_text("// pub fn __trim_covered_now is coming\n", encoding="utf-8")
            self.assertFalse(perf_harness_sources.tree_supports_hook(root, perf_harness_sources.TRIM_HOOK_FEATURE, method))
            source.write_text(perf_atlas_tests.TRIM_SOURCE, encoding="utf-8")
            self.assertTrue(perf_harness_sources.tree_supports_hook(root, perf_harness_sources.TRIM_HOOK_FEATURE, method))
            (root / perf_settings.APP_MANIFEST).write_text(perf_home_tests.HEAD_MANIFEST, encoding="utf-8")
            self.assertFalse(perf_harness_sources.tree_supports_hook(root, perf_harness_sources.TRIM_HOOK_FEATURE, method))

    def test_a_five_feature_head_and_a_four_feature_base_each_build_their_own_catalog_step(self):
        # The head declares every feature and defines both hooks; the base predates the trim hook. Each side's
        # step is the gate's catalog entry for exactly its tuple, and both run one harness hash.
        code, gate, _calls, plans, _work, out = self.compare(
            head_manifest=perf_atlas_tests.FIVE_FEATURES_MANIFEST, base_manifest=perf_atlas_tests.FOUR_FEATURES_MANIFEST,
            hook_trees=("head", "base"), trim_trees=("head",))
        self.assertEqual(code, perf_settings.EXIT_PASS)
        # Both fixture trees define S1/atlas-retry's methods, so each side builds its features with that cfg on.
        catalog = {features: steps for (cfgs, features), steps in perf_build_tests.REAL_GATE.PERF_CFG_BUILDS.items()
                   if cfgs == ("perf_atlas_retry_api",)}
        five = perf_harness_sources.PERF_FEATURES
        four = tuple(feature for feature in five if feature != perf_harness_sources.TRIM_HOOK_FEATURE)
        self.assertIs(self.build_step(gate, "head"), catalog[five]["build-head-perf_scenarios"])
        self.assertIs(self.build_step(gate, "base"), catalog[four]["build-base-perf_scenarios"])
        self.assertEqual(self.build_step(gate, "head").argv[-1], ",".join(five))
        self.assertEqual(len({plan.harness_hash for plan in plans}), 1, "both sides run one overlaid harness")
        document = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn("- Built with `--features perf-hook-trim`: head", document)

    def test_a_trim_declaration_without_the_method_builds_without_the_feature(self):
        _code, gate, *_rest = self.compare(head_manifest=perf_atlas_tests.TRIM_MANIFEST)
        self.assertNotIn("--features", self.build_step(gate, "head").argv)

    def test_the_manifest_records_the_trim_feature_and_a_mismatch_fails(self):
        binaries, digest = self.produce(head_manifest=perf_atlas_tests.TRIM_MANIFEST, trim_trees=("head",))
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["features"], {"base": [], "head": [perf_harness_sources.TRIM_HOOK_FEATURE]})
        with self.assertRaisesRegex(ValueError, "refusing the prebuilt binaries: features"):
            self.consume(binaries, digest, head_manifest=perf_atlas_tests.TRIM_MANIFEST)
        code, *_rest = self.consume(binaries, digest, head_manifest=perf_atlas_tests.TRIM_MANIFEST, trim_trees=("head",))
        self.assertEqual(code, perf_settings.EXIT_PASS)

    def test_the_gate_rejects_a_lookalike_trim_build_step(self):
        # A copy of a reviewed step with the same id and argv is not the gate's own step.
        step = perf_build_tests.REAL_GATE.PERF_FEATURE_BUILDS[perf_harness_sources.PERF_FEATURES]["build-head-perf_scenarios"]
        self.assertTrue(perf_build_tests.REAL_GATE._reviewed_step(step))
        self.assertFalse(perf_build_tests.REAL_GATE._reviewed_step(copy.copy(step)))

    def test_both_scripts_list_the_same_five_perf_features(self):
        self.assertEqual(perf_harness_sources.PERF_FEATURES, perf_build_tests.REAL_GATE.PERF_FEATURES)
        self.assertEqual(perf_harness_sources.PERF_FEATURES[-1], perf_harness_sources.TRIM_HOOK_FEATURE)


class TrimHookResultTests(unittest.TestCase):
    """result.json's `hooks.trim`: optional for an older harness, one known outcome when present."""

    def test_a_result_with_or_without_hooks_validates(self):
        # An older harness writes no `hooks`; this harness writes one of four outcomes.
        self.assertEqual(perf_result_schema.validate_result(perf_result_tests.valid_result(), perf_memory_tests.HARNESS_HASH, 0), [])
        for outcome in ("not-reached", "unsupported", "skipped", "trimmed"):
            with self.subTest(outcome=outcome):
                # A trimmed hook names its trim number; every other outcome carries none.
                after_hook = 1 if outcome == "trimmed" else None
                result = perf_result_tests.valid_result(hooks={"trim": outcome}, trim_seq_after_hook=after_hook)
                self.assertEqual(perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0), [])

    def test_an_unknown_trim_outcome_or_shape_is_refused(self):
        for hooks in ({"trim": "maybe"}, {"trim": None}, {}, {"trim": 0}, [], "unsupported", None):
            with self.subTest(hooks=hooks):
                self.assertTrue(perf_result_schema.validate_result(perf_result_tests.valid_result(hooks=hooks), perf_memory_tests.HARNESS_HASH, 0))

    def test_an_unsupported_trim_keeps_checkpoint_memory_numeric(self):
        # An unsupported trim is an untrimmed baseline: the supported golden run's checkpoint memory stays a
        # figure, exactly as it reads without `hooks`; nothing reads n/a and nothing reads zero.
        result, samples = perf_atlas_tests.fixture_run("supported", "deadline-610")
        baseline = perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=result, memory=samples))
        hooked = dict(result, hooks={"trim": "unsupported"})
        metrics = perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=hooked, memory=samples))
        self.assertEqual(metrics, baseline)
        memory = {key: value for key, value in metrics.items()
                  if key[0].endswith(("renderer_total_bytes", "process_resident_bytes"))}
        self.assertTrue(memory, "the golden run has checkpoint memory")
        for key, value in memory.items():
            with self.subTest(key=key):
                self.assertIsInstance(value, float)
                self.assertNotIsInstance(value, perf_metrics.NotAvailable)
        # The golden run has no renderer, so its renderer reading is a real 0; resident memory is never 0.
        resident = [value for key, value in memory.items() if key[0].endswith("process_resident_bytes")]
        self.assertTrue(resident and all(value > 0 for value in resident), resident)



def trim_run(platform_trim="trimmed", after_hook=2, **overrides):
    """A short S12 trim-experiment result whose `covered` checkpoint is index 0."""
    covered = {"name": "covered", "start_unix_s": 10.0, "end_unix_s": 15.0, "cpu_user_s": 0.2,
               "cpu_system_s": 0.1, "presented_frames": 1, "redraw_requested": 2,
               "dispatch_ms": [1.0], "present_interval_ms": [16.6], "allocations_per_frame": None}
    fields = dict(scenario="S12", short=True, trim_experiment="s12-short-trim", hooks={"trim": platform_trim},
                  trim_seq_after_hook=after_hook, phases=[covered], checkpoint_memory="supported",
                  checkpoints=[{"index": 0, "label": "covered", "unix_s": 15.0, "footprint_file": None}])
    fields.update(overrides)
    return perf_result_tests.valid_result(**fields)


def covered_sample(trim_seq=None, trimmed=None, trim_source=None, renderer_mib=16):
    """A complete `covered` checkpoint sample with the given trim tags."""
    return perf_memory.MemorySample(15.0, 100 * 1048576, renderer_mib * 1048576, 0, checkpoint_index=0,
                             checkpoint_label="covered", checkpoint_attempt=1, checkpoint_complete=True,
                             trimmed=trimmed, trim_source=trim_source, trim_seq=trim_seq)


# One trimmed renderer entry exactly as the App's breakdown writes it. The App's own test asserts this text
# byte for byte, so a rename on either side fails one of the two suites.
TRIMMED_RENDERER_ENTRY = ("visible[WindowId(1)] glyph=512/5 image=256/2 row_glyph=64/4 row_quad=32/3 software=1024/1 vertex=272/1 row_ink=48/40 frame_scratch=96/2 chrome_cache=24/3 total=2328/61 glyph_atlas_dim=512 glyph_atlas_packed_pixels=4000 glyph_atlas_growths=1 glyph_atlas_evictions=0 glyph_atlas_fit=512 glyph_atlas_max_tile=25x16 renderer_trimmed=true renderer_gpu_released_requested_bytes=8388608")


def renderer_entries(*trimmed_states):
    """Renderer entries in the App's format, one per state, each with its own window id."""
    return "; ".join(
        TRIMMED_RENDERER_ENTRY.replace("WindowId(1)", f"WindowId({index + 1})")
        .replace("renderer_trimmed=true", f"renderer_trimmed={str(state).lower()}")
        for index, state in enumerate(trimmed_states))


def formatted_covered_line(entries, trim_tags=""):
    """A covered checkpoint `memory snapshot` line in the production field order, with `entries` inside the
    renderer breakdown and `trim_tags` appended after the checkpoint tags (empty for a build without the hook)."""
    return (f"{perf_processes_tests.STAMP}  INFO memory: memory snapshot process_private_committed_bytes=unsupported "
            f"process_resident_bytes=104857600 process_virtual_bytes=unsupported session_total_bytes=0 "
            f"panes_total=1 panes_sampled=1 panes_contended=0 renderer_total_bytes=16777216 live_renderers=2 "
            f"renderers={entries} allocator_state=unsupported checkpoint_index=0 checkpoint_label=\"covered\" "
            f"checkpoint_attempt=1 checkpoint_complete=true{trim_tags}")


class TrimExperimentTests(unittest.TestCase):
    """The short S12 trim experiment: covered-row rules, the Windows validator and the covered activity rows."""

    COVERED = ("covered renderer_total_bytes", "MiB", "run")

    def covered_metric(self, result, sample):
        return perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=result, memory=[sample]))[self.COVERED]

    def test_a_tagged_line_parses_its_three_trim_tags(self):
        # The trim tags come back typed; a line without them reads None for each.
        line = (perf_atlas_tests.CheckpointMemoryTests.LINE + ' trimmed=true trim_source="hook" trim_seq=3')
        sample = perf_memory.parse_memory_line(line)
        self.assertEqual((sample.trimmed, sample.trim_source, sample.trim_seq), (True, "hook", 3))
        plain = perf_memory.parse_memory_line(perf_atlas_tests.CheckpointMemoryTests.LINE)
        self.assertEqual((plain.trimmed, plain.trim_source, plain.trim_seq), (None, None, None))

    def test_top_level_trim_tags_never_read_a_renderer_entry(self):
        # Two renderer entries disagree with the line's own tag in both directions; the parser reads only the
        # top-level tag, and the renderer entries still parse as glyph atlas facts.
        for top, entries in ((True, (False, True)), (False, (True, True))):
            with self.subTest(top=top):
                tags = f' trimmed={str(top).lower()} trim_source="hook" trim_seq=2'
                sample = perf_memory.parse_memory_line(formatted_covered_line(renderer_entries(*entries), tags))
                self.assertEqual((sample.trimmed, sample.trim_source, sample.trim_seq), (top, "hook", 2))
                self.assertEqual(len(sample.glyph_atlases), 2)

    def test_a_feature_off_line_with_renderer_entries_is_an_untrimmed_baseline(self):
        # A build without the hook writes renderer entries but no checkpoint trim tags: every tag parses as None,
        # and an unsupported side's covered reading stays a figure, never n/a: schema.
        sample = perf_memory.parse_memory_line(formatted_covered_line(renderer_entries(True, False)))
        self.assertEqual((sample.trimmed, sample.trim_source, sample.trim_seq), (None, None, None))
        base = trim_run("unsupported", None)
        self.assertEqual(self.covered_metric(base, sample), 16.0)

    def test_a_malformed_trim_tag_is_a_schema_problem_never_an_absent_one(self):
        # A present but unreadable trim tag is recorded as malformed rather than read as absent, so an
        # unsupported side carrying one reads n/a: schema, and a trimmed side does too. A line with no trim
        # tags at all keeps the numeric unsupported baseline.
        base = trim_run("unsupported", None)
        head = trim_run("trimmed", 2)
        for tags, malformed in ((" trimmed=bogus", ("trimmed",)), (" trim_seq=-1", ("trim_seq",)),
                                (' trimmed=true trim_source="hook" trim_seq=x2', ("trim_seq",))):
            with self.subTest(tags=tags):
                sample = perf_memory.parse_memory_line(formatted_covered_line(renderer_entries(True), tags))
                self.assertEqual(sample.malformed_trim_tags, malformed)
                self.assertEqual(self.covered_metric(base, sample), perf_metrics.NotAvailable("schema"))
                self.assertEqual(self.covered_metric(head, sample), perf_metrics.NotAvailable("schema"))
        absent = perf_memory.parse_memory_line(formatted_covered_line(renderer_entries(True)))
        self.assertEqual(absent.malformed_trim_tags, ())
        self.assertEqual(self.covered_metric(base, absent), 16.0)

    def test_a_supported_experiment_that_did_not_trim_is_never_credited(self):
        # Skipped, unreached or unrecorded hooks read n/a with their reason, never as an ordinary trimmed
        # reading; the raw covered figure stays on its own row, never replaced by zero.
        raw_key = ("covered renderer_total_bytes, uncredited trim", "MiB", "run")
        cases = (("skipped", "trim skipped"), ("not-reached", "trim not reached"), (None, "trim not recorded"))
        for outcome, reason in cases:
            with self.subTest(outcome=outcome):
                hooks = {"trim": outcome} if outcome else None
                result = trim_run(after_hook=None, hooks=hooks)
                metrics = perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=result, memory=[covered_sample(renderer_mib=12)]))
                self.assertEqual(metrics[self.COVERED], perf_metrics.NotAvailable(reason))
                self.assertIn(raw_key, metrics, "the raw covered figure keeps its own row")
                self.assertEqual(metrics[raw_key], 12.0)

    def test_a_credited_trim_needs_its_state_source_and_number(self):
        # A trimmed hook counts only a sample that is trimmed, names hook or scheduler, and is at least the
        # hook's number; a sample after the trim whose window reads untrimmed is stale, and a missing state or a
        # source that names no trim is a schema problem.
        head = trim_run("trimmed", 2)
        for source in ("hook", "scheduler"):
            with self.subTest(source=source):
                self.assertEqual(self.covered_metric(head, covered_sample(2, True, source, 4)), 4.0)
        self.assertEqual(self.covered_metric(head, covered_sample(2, False, "hook")), perf_metrics.NotAvailable("stale"))
        for trimmed, source in ((None, "hook"), (True, "none"), (True, None), (True, "bogus")):
            with self.subTest(trimmed=trimmed, source=source):
                value = self.covered_metric(head, covered_sample(2, trimmed, source))
                self.assertEqual(value, perf_metrics.NotAvailable("schema"))

    def test_the_hook_outcome_and_its_number_must_agree(self):
        # A trimmed hook needs a positive number; any other outcome carries none; an experiment records its hook.
        # The results carry an `end` checkpoint, so each one's only problem is the one asserted.
        points = [{"index": 0, "label": "covered", "unix_s": 15.0, "footprint_file": None},
                  {"index": 1, "label": "end", "unix_s": 20.0, "footprint_file": None}]
        self.assertEqual(perf_result_schema.validate_result(trim_run("trimmed", 2, checkpoints=points), perf_memory_tests.HARNESS_HASH, 0), [])
        self.assertEqual(perf_result_schema.validate_result(trim_run("unsupported", None, checkpoints=points), perf_memory_tests.HARNESS_HASH, 0), [])
        expected = {("trimmed", None): "hooks.trim is trimmed but trim_seq_after_hook is not a positive integer",
                    ("unsupported", 2): "hooks.trim is 'unsupported' but trim_seq_after_hook is 2, not null",
                    ("skipped", 1): "hooks.trim is 'skipped' but trim_seq_after_hook is 1, not null",
                    ("not-reached", 3): "hooks.trim is 'not-reached' but trim_seq_after_hook is 3, not null"}
        for (outcome, after_hook), problem in expected.items():
            with self.subTest(outcome=outcome, after_hook=after_hook):
                result = trim_run(outcome, after_hook, checkpoints=points)
                self.assertEqual(perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0), [problem])
        unrecorded = trim_run(after_hook=None, checkpoints=points)
        del unrecorded["hooks"]
        self.assertEqual(perf_result_schema.validate_result(unrecorded, perf_memory_tests.HARNESS_HASH, 0),
                         ["a trim experiment result records no hooks"])

    def test_covered_rows_follow_the_trim_rules(self):
        # An unsupported base without tags is an untrimmed baseline; a hooked sample at or after the hook's trim
        # number compares; one before it, or without a number, is stale; tags on an unsupported side are a schema
        # problem.
        base = trim_run("unsupported", None)
        self.assertEqual(self.covered_metric(base, covered_sample()), 16.0)
        head = trim_run("trimmed", 2)
        self.assertEqual(self.covered_metric(head, covered_sample(2, True, "hook", 4)), 4.0)
        self.assertEqual(self.covered_metric(head, covered_sample(3, True, "hook", 4)), 4.0)
        for stale in (covered_sample(1, True, "hook"), covered_sample(0, False, "none"), covered_sample()):
            with self.subTest(trim_seq=stale.trim_seq):
                self.assertEqual(self.covered_metric(head, stale), perf_metrics.NotAvailable("stale"))
        tagged = covered_sample(0, False, "none")
        self.assertEqual(self.covered_metric(base, tagged), perf_metrics.NotAvailable("schema"))

    def test_the_trim_rules_leave_other_checkpoints_and_runs_alone(self):
        # Pins: a partial authoritative sample still reads partial; an unsupported checkpoint hook still reads
        # n/a: unsupported; a result outside the experiment ignores its tags.
        point = {"index": 0, "label": "covered", "unix_s": 15.0, "footprint_file": None,
                 "sampling": "exhausted", "attempts": 1, "last_attempt_complete": False}
        partial = perf_memory.MemorySample(15.0, 100 * 1048576, 8 * 1048576, 0, checkpoint_index=0,
                                    checkpoint_label="covered", checkpoint_attempt=1, checkpoint_complete=False,
                                    trimmed=True, trim_source="hook", trim_seq=2)
        value = self.covered_metric(trim_run(checkpoints=[point]), partial)
        self.assertIsInstance(value, perf_metrics.PartialValue)
        unsupported = self.covered_metric(trim_run(checkpoint_memory="unsupported"), covered_sample())
        self.assertEqual(unsupported, perf_metrics.NotAvailable("unsupported"))
        outside = trim_run("unsupported", None, trim_experiment=None)
        self.assertEqual(self.covered_metric(outside, covered_sample(0, False, "none")), 16.0)

    def test_windows_accepts_synthetic_occlusion_only_for_the_trim_experiment(self):
        # The one valid combination: win32, synthetic occlusion, S12, short and the experiment name.
        points = [{"index": 0, "label": "covered", "unix_s": 15.0, "footprint_file": None},
                  {"index": 1, "label": "end", "unix_s": 20.0, "footprint_file": None}]
        good = trim_run(synthetic_occlusion=True, presenter=dict(perf_foreground_tests.WGPU_PRESENTER), checkpoints=points)
        self.assertEqual(perf_result_schema.validate_result(good, perf_memory_tests.HARNESS_HASH, 0, platform_name="win32"), [])
        for change in ({"scenario": "S1"}, {"short": False}, {"trim_experiment": None}):
            with self.subTest(change=change):
                broken = dict(good, **change)
                self.assertTrue(perf_result_schema.validate_result(broken, perf_memory_tests.HARNESS_HASH, 0, platform_name="win32"))
        self.assertEqual(perf_result_schema.validate_result(good, perf_memory_tests.HARNESS_HASH, 0, platform_name="darwin"), [])
        for bad in ({"trim_experiment": "other"}, {"trim_seq_after_hook": 0}, {"trim_seq_after_hook": "2"}):
            with self.subTest(bad=bad):
                self.assertTrue(perf_result_schema.validate_result(dict(good, **bad), perf_memory_tests.HARNESS_HASH, 0))

    def test_the_covered_phase_reports_activity_not_rates(self):
        # On both hosts the experiment's covered phase prints wall, presented frames and redraws as counts and
        # CPU, never a frame rate or a present interval; the stored interval samples are untouched.
        for platform_name in ("darwin", "win32"):
            with self.subTest(platform=platform_name):
                result = trim_run()
                metrics = perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=result))
                self.assertNotIn(("covered presented frames", "fps", "run"), metrics)
                self.assertNotIn(("covered present interval", "ms", "frame"), metrics)
                self.assertEqual(metrics[("covered wall", "s", "run")], 5.0)
                self.assertEqual(metrics[("covered presented frames", "count", "run")], 1)
                self.assertEqual(metrics[("covered redraws requested", "count", "run")], 2)
                self.assertAlmostEqual(metrics[("covered CPU", "s", "run")], 0.3)
                self.assertEqual(result["phases"][0]["present_interval_ms"], [16.6])

    def test_without_the_experiment_the_covered_phase_keeps_its_rates(self):
        # Pin: the same result without `trim_experiment` keeps today's rate and interval rows.
        metrics = perf_metrics.run_metrics(perf_outcomes_tests.make_outcome(result=trim_run(trim_experiment=None)))
        self.assertIn(("covered presented frames", "fps", "run"), metrics)
        self.assertIn(("covered present interval", "ms", "frame"), metrics)


# A listing whose S1 also offers the counters-only atlas-retry variant.
ATLAS_RETRY_LIST = {"schema_version": 1, "scenarios": [
    {"id": "S1", "variants": ["default", "atlas-retry"], "title": "Idle", "timeout_s": 120, "short_timeout_s": 30,
     "run_caps": {"atlas-retry": 2}}]}
