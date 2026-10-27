"""Perf atlas tests."""

from __future__ import annotations
import dataclasses
import json
from pathlib import Path
import unittest

import perf_atlas_rows
import perf_comparison_rows
import perf_comparison_tests
import perf_counter_rows
import perf_home_tests
import perf_memory
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_result_schema
import perf_result_tests
# An app source file defining the trim hook, and manifests that declare it: alone, and with every other feature.
TRIM_SOURCE = ("impl App {\n    #[doc(hidden)]\n    pub fn __trim_covered_now(&mut self, window_id: WindowId) {}\n}\n")
TRIM_MANIFEST = perf_home_tests.HEAD_MANIFEST + "\n[features]\nperf-hook-trim = []\n"
ALL_FEATURES_TABLE = ("\n[features]\nperf-counters = []\nperf-frame-texture = []\nperf-hook-checkpoint-memory = []\n"
                      "perf-echo-trace = [\"perf-counters\"]\n")
FOUR_FEATURES_MANIFEST = perf_home_tests.HEAD_MANIFEST + ALL_FEATURES_TABLE
FIVE_FEATURES_MANIFEST = FOUR_FEATURES_MANIFEST + "perf-hook-trim = []\n"
COUNTERS_HOOK_MANIFEST = perf_home_tests.HEAD_MANIFEST + "\n[features]\nperf-counters = []\nperf-hook-checkpoint-memory = []\n"


# Golden runs the harness's own test writes: `result.json` from the production serializer and the
# `memory` lines the App's hook logged, keyed by build ("supported" or "unsupported") and case.
CHECKPOINT_FIXTURE = Path(__file__).with_name("perf-compare_checkpoint_fixture.json")
# Two runs' renderer attempt deltas, written by the harness test that computes them.
ATTEMPT_FIXTURE = Path(__file__).with_name("perf-compare_attempt_fixture.json")


def fixture_run(build, case):
    """One golden run's result and its memory samples, parsed as a real run's logs are."""
    run = json.loads(CHECKPOINT_FIXTURE.read_text(encoding="utf-8"))[build][case]
    samples = sorted(filter(None, map(perf_memory.parse_memory_line, run["logs"])), key=lambda sample: sample.unix_s)
    result = run["result"]
    # The golden runs come from the harness's headless probe, which has no renderer and so records no presenter;
    # a real macOS run records one, and these tests read checkpoints, not the presenter.
    if result.get("presenter") is None:
        result["presenter"] = dict(perf_result_tests.MACOS_PRESENTER)
    return result, samples


def fixture_outcome(build, case, plan=None):
    """A valid run outcome carrying a golden run's result and memory samples."""
    result, samples = fixture_run(build, case)
    return perf_outcomes_tests.make_outcome(**({"plan": plan} if plan is not None else {}), result=result, memory=samples)


def tagged_sample(index, attempt, complete, renderer_mib=16, unix_s=60.0):
    """A memory line tagged with checkpoint `index`, attempt `attempt`."""
    return perf_memory.MemorySample(unix_s, 100 * 1048576, renderer_mib * 1048576, 0, checkpoint_index=index,
                             checkpoint_label="end", checkpoint_attempt=attempt, checkpoint_complete=complete)


class CheckpointMemoryTests(unittest.TestCase):
    """A checkpoint reads its own tagged sample: the complete one with the highest attempt, else the last partial."""

    LINE = ("2026-10-03T00:00:01Z  INFO memory: memory snapshot process_resident_bytes=10 "
            "session_total_bytes=3 panes_total=2 panes_sampled=1 panes_contended=1 renderer_total_bytes=7 "
            "checkpoint_index=2 checkpoint_label=\"end\" checkpoint_attempt=3 checkpoint_complete=false")

    def test_a_tagged_line_parses_its_four_tags_and_pane_counts(self):
        # The label comes back without the log layer's quotes; a periodic line carries no tags.
        sample = perf_memory.parse_memory_line(self.LINE)
        self.assertEqual((sample.checkpoint_index, sample.checkpoint_label, sample.checkpoint_attempt,
                          sample.checkpoint_complete), (2, "end", 3, False))
        self.assertEqual((sample.panes_total, sample.panes_sampled, sample.panes_contended), (2, 1, 1))
        periodic = perf_memory.parse_memory_line(perf_memory_tests.memory_line())
        self.assertEqual((periodic.checkpoint_index, periodic.checkpoint_complete), (None, None))

    def test_a_complete_attempt_wins_over_an_earlier_partial_one(self):
        reading = perf_memory.checkpoint_memory([tagged_sample(0, 1, False, 9), tagged_sample(0, 2, True, 16)], 0)
        self.assertEqual((reading.sample.checkpoint_attempt, reading.partial), (2, False))

    def test_all_partial_reads_the_highest_attempt_marked_partial(self):
        samples = [tagged_sample(0, attempt, False, attempt) for attempt in (1, 3, 2)]
        reading = perf_memory.checkpoint_memory(samples, 0)
        self.assertEqual((reading.sample.checkpoint_attempt, reading.partial), (3, True))

    def test_a_repeated_line_counts_once(self):
        # The same reading twice, even at another timestamp, is one reading, not a conflict.
        reading = perf_memory.checkpoint_memory([tagged_sample(0, 1, True), tagged_sample(0, 1, True, unix_s=61.0)], 0)
        self.assertIsNone(reading.problem)
        self.assertEqual(reading.sample.checkpoint_attempt, 1)

    def test_two_complete_samples_of_one_attempt_with_different_totals_conflict(self):
        reading = perf_memory.checkpoint_memory([tagged_sample(0, 1, True, 16), tagged_sample(0, 1, True, 17)], 0)
        self.assertEqual((reading.sample, reading.problem), (None, "conflicting samples"))
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}
        result = perf_result_tests.valid_result(checkpoints=[point], checkpoint_memory="supported")
        side = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=result, memory=[tagged_sample(0, 1, True, 16),
                                                                  tagged_sample(0, 1, True, 17)])])
        rows = perf_comparison_rows.comparison_rows("S1/default", side, side)
        self.assertEqual(perf_comparison_tests.row_for(rows, "end renderer_total_bytes (MiB)")[2], "n/a: conflicting samples")

    def test_a_periodic_sample_is_never_a_checkpoint_reading(self):
        # An older untagged sample before the checkpoint is not substituted for the missing tagged one.
        # Resolve this runtime-only dependency after module initialization.
        import perf_variants_tests
        self.assertIsNone(perf_memory.checkpoint_memory([perf_variants_tests.memory_sample(50.0)], 0))
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}
        outcome = perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[point], checkpoint_memory="supported"),
                               memory=[perf_variants_tests.memory_sample(50.0)])
        self.assertNotIn(("end renderer_total_bytes", "MiB", "run"), perf_metrics.run_metrics(outcome))

    def test_each_index_reads_its_own_samples(self):
        samples = [tagged_sample(1, 1, True, 11), tagged_sample(2, 1, True, 22)]
        self.assertEqual(perf_memory.checkpoint_memory(samples, 1).sample.renderer_total_bytes, 11 * 1048576)
        self.assertEqual(perf_memory.checkpoint_memory(samples, 2).sample.renderer_total_bytes, 22 * 1048576)
        self.assertIsNone(perf_memory.checkpoint_memory(samples, 0))

    def test_a_partial_reading_counts_and_its_cell_says_so(self):
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None, "sampling": "exhausted",
                 "attempts": 5, "last_attempt_complete": False}
        result = perf_result_tests.valid_result(checkpoints=[point], checkpoint_memory="supported")
        self.assertEqual(perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0), [])
        side = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=result, memory=[tagged_sample(0, 5, False, 16)])])
        rows = perf_comparison_rows.comparison_rows("S1/default", side, side)
        self.assertEqual(perf_comparison_tests.row_for(rows, "end renderer_total_bytes (MiB)")[2], "16.00 (16.00–16.00), 1 partial")

    def test_a_side_without_the_hook_reads_n_a_unsupported(self):
        # The base's harness has no hook, so its checkpoint rows say so; the head's tagged sample reads.
        # Resolve this runtime-only dependency after module initialization.
        import perf_variants_tests
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}
        base = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[point],
                                                               checkpoint_memory="unsupported"),
                                           memory=[perf_variants_tests.memory_sample(60.0)])])
        old_base = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[point]))])
        head = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[point], checkpoint_memory="supported"),
                                           memory=[tagged_sample(0, 1, True, 16)])])
        for side in (base, old_base):
            row = perf_comparison_tests.row_for(perf_comparison_rows.comparison_rows("S1/default", side, head), "end renderer_total_bytes (MiB)")
            self.assertEqual(row[2:5], ["n/a: unsupported", "16.00 (16.00–16.00)", "n/a"])

    def test_an_unknown_checkpoint_memory_or_sampling_state_is_a_schema_problem(self):
        self.assertTrue(perf_result_schema.validate_result(perf_result_tests.valid_result(checkpoint_memory="maybe"), perf_memory_tests.HARNESS_HASH, 0))
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None, "sampling": "late"}
        self.assertTrue(perf_result_schema.validate_result(perf_result_tests.valid_result(checkpoints=[point]), perf_memory_tests.HARNESS_HASH, 0))


def atlas_facts(renderer="main[1]", dim=512, packed=4000, growths=1, evictions=0, fit="512", tile=(25, 16)):
    """One renderer's glyph atlas facts as a memory line reports them."""
    return perf_memory.GlyphAtlasFacts(renderer, dim, packed, growths, evictions, fit, tile)


def atlas_outcome(facts, phase_growths=None, checkpoint_unix_s=70.0, readings=None):
    """A valid run whose `end` checkpoint sample reports `facts`; `phase_growths` gives each phase's counted
    `renderer.glyph_atlas_growths` delta, one phase per entry ending at 20 s steps, or no counters when None;
    `readings` is the checkpoint's `atlas_readings` list, absent when None (a harness that predates it)."""
    point = {"index": 0, "label": "end", "unix_s": checkpoint_unix_s, "footprint_file": None}
    if readings is not None:
        point["atlas_readings"] = readings
    phases = perf_result_tests.valid_result()["phases"]
    if phase_growths is not None:
        phases = [{**phases[0], "name": f"phase{number}", "start_unix_s": 20.0 * number,
                   "end_unix_s": 20.0 * number + 10.0,
                   "frame_counters": {"renderer": {"glyph_atlas_growths": growths}}}
                  for number, growths in enumerate(phase_growths)]
    sample = dataclasses.replace(tagged_sample(0, 1, True), glyph_atlases=tuple(facts))
    return perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[point], checkpoint_memory="supported", phases=phases),
                        memory=[sample])


class GlyphAtlasReportTests(unittest.TestCase):
    """The memory line's per-renderer glyph atlas facts become per-checkpoint report rows."""

    def test_each_checkpoint_reports_every_renderers_atlas_facts_and_a_base_without_them_reads_n_a(self):
        # A base built before the facts reads n/a; the head's two runs show each fact per renderer, a
        # categorical fit that differs between runs lists each outcome with its run count, and numeric
        # facts compare medians only when both sides have them.
        base = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoint_memory="supported"),
                                           memory=[tagged_sample(0, 1, True)])])
        head = perf_metrics.SideRuns([
            atlas_outcome([atlas_facts(fit="no_headroom", growths=2, evictions=0),
                           atlas_facts("warm[0]", dim=256, packed=0, growths=0, fit="256", tile=(0, 0))]),
            atlas_outcome([atlas_facts(fit="evicted", growths=2, evictions=7),
                           atlas_facts("warm[0]", dim=256, packed=0, growths=0, fit="256", tile=(0, 0))])])
        rows = perf_comparison_rows.comparison_rows("S9/default", base, head)
        expected = {
            "end main[1] glyph_atlas_dim (px)": "512",
            "end main[1] glyph_atlas_packed_pixels (px)": "4000",
            "end main[1] glyph_atlas_fit (outcome)": "evicted ×1; no_headroom ×1",
            "end main[1] glyph_atlas_growths (count)": "2",
            "end main[1] glyph_atlas_evictions (count)": "0 ×1; 7 ×1",
            "end main[1] glyph_atlas_max_tile (px)": "25x16",
            "end warm[0] glyph_atlas_fit (outcome)": "256",
        }
        for metric, head_cell in expected.items():
            with self.subTest(metric=metric):
                # A numeric fact's change is n/a without a base figure; a categorical fact has no change.
                change = "" if "_fit " in metric or "_max_tile " in metric else "n/a"
                self.assertEqual(perf_comparison_tests.row_for(rows, metric)[2:5], ["n/a", head_cell, change])
        both = perf_comparison_rows.comparison_rows("S9/default", perf_metrics.SideRuns([atlas_outcome([atlas_facts(dim=256)])]),
                                    perf_metrics.SideRuns([atlas_outcome([atlas_facts(dim=512, fit="does_not_fit")])]))
        self.assertEqual(perf_comparison_tests.row_for(both, "end main[1] glyph_atlas_dim (px)")[2:5], ["256", "512", "+100.0%"])
        self.assertEqual(perf_comparison_tests.row_for(both, "end main[1] glyph_atlas_fit (outcome)")[2:5], ["512", "does_not_fit", ""])

    def test_a_run_without_atlas_facts_adds_no_atlas_rows(self):
        # Neither side reports facts, so the table keeps its existing rows only.
        side = perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoint_memory="supported"),
                                           memory=[tagged_sample(0, 1, True)])])
        rows = perf_comparison_rows.comparison_rows("S1/default", side, side)
        self.assertFalse([row for row in rows if "glyph_atlas" in row[1]], rows)

    def test_rows_aggregate_each_window_by_its_logical_identity_across_runs_and_sides(self):
        # A native window id (an object address on macOS, an HWND on Windows) differs in every run, so rows
        # group the run's main window as `main` through the checkpoint reading's `main_window`, keep the warm
        # pool slot, and list each run's native id as evidence; a run without a reading never guesses `main`.
        def run(native, dim):
            reading = {"attempt": 1, "main_window": native, "counted_glyph_atlas_growths": None,
                       "closed_glyph_atlas_growths": None}
            return atlas_outcome([atlas_facts(f"visible[{native}]", dim=dim),
                                  atlas_facts("warm[0]", dim=256, fit="256")], readings=[reading])
        base = perf_metrics.SideRuns([run("WindowId(0x1a)", 256), run("WindowId(0x2b)", 256)])
        head = perf_metrics.SideRuns([run("WindowId(0x3c)", 512), run("WindowId(0x4d)", 512)])
        rows = perf_comparison_rows.comparison_rows("S9/default", base, head)
        self.assertEqual(perf_comparison_tests.row_for(rows, "end main glyph_atlas_dim (px)")[2:5], ["256", "512", "+100.0%"])
        self.assertEqual(perf_comparison_tests.row_for(rows, "end main renderer_native_id (id)")[2:5],
                         ["WindowId(0x1a) ×1; WindowId(0x2b) ×1", "WindowId(0x3c) ×1; WindowId(0x4d) ×1", ""])
        self.assertEqual(perf_comparison_tests.row_for(rows, "end warm[0] glyph_atlas_dim (px)")[2:4], ["256", "256"])
        self.assertFalse([row for row in rows if "WindowId(" in row[1]], "a native id never keys a row")
        unread = perf_metrics.SideRuns([atlas_outcome([atlas_facts("visible[WindowId(0x5e)]", dim=512)])])
        unread_rows = perf_comparison_rows.comparison_rows("S9/default", unread, unread)
        self.assertEqual(perf_comparison_tests.row_for(unread_rows, "end visible#1 glyph_atlas_dim (px)")[2:4], ["512", "512"])

    def test_reconciliation_compares_each_windows_snapshot_and_counted_growths_at_the_same_sample(self):
        # Each live window's snapshot growths must equal the growths its own counters recorded at the same
        # sample. Dropped counts and duplicates that startup growth would mask under an inequality both read
        # as mismatches; a closed window's counted growths are listed apart, so retiring one is consistent.
        main = "WindowId(0x1a)"

        def run(snapshot, counted, phase_growths, closed=0):
            reading = {"attempt": 1, "main_window": main, "counted_glyph_atlas_growths": counted,
                       "closed_glyph_atlas_growths": closed}
            return atlas_outcome([atlas_facts(f"visible[{main}]", growths=snapshot),
                                  atlas_facts("warm[0]", growths=0)], phase_growths, readings=[reading])
        # Every growth went uncounted: the phases count 0, which an inequality would call consistent.
        missing = run(2, {main: 0}, [0])
        # Two startup growths outside any phase, then one phase growth counted twice: the phases count 2,
        # below the snapshot's 3, while the window's own lifetime count is 4.
        duplicated = run(3, {main: 4}, [2])
        # A closed window grew 3 times; the phases count 4 across both windows, the snapshot holds only main.
        retired = run(1, {main: 1}, [4], closed=3)
        rows = perf_atlas_rows.glyph_atlas_reconciliation_rows(
            "S9/default", perf_metrics.SideRuns([missing, duplicated]), perf_metrics.SideRuns([retired]))
        self.assertEqual(rows, [
            ["S9/default", "end", "glyph_atlas_growths, snapshot/counted",
             "mismatch in 2 of 2 runs: main 2/0; main 3/4", "consistent: main 1/1, closed 3", ""]])

    def test_reconciliation_without_a_per_window_reading_is_inconclusive_with_both_figures(self):
        # Without a same-sample per-window count only the summed figures exist, and an inequality between them
        # proves nothing either way, so the run is inconclusive and shows both; so is a visible window the
        # reading did not count. Without counters, facts or a base there is nothing to reconcile.
        no_reading = atlas_outcome([atlas_facts(growths=3)], [1, 2])
        uncounted = atlas_outcome(
            [atlas_facts("visible[WindowId(0x1a)]", growths=2)], [2],
            readings=[{"attempt": 1, "main_window": "WindowId(0x1a)", "counted_glyph_atlas_growths": {},
                       "closed_glyph_atlas_growths": 0}])
        rows = perf_atlas_rows.glyph_atlas_reconciliation_rows(
            "S9/default", perf_metrics.SideRuns([no_reading]), perf_metrics.SideRuns([uncounted]))
        self.assertEqual(rows, [
            ["S9/default", "end", "glyph_atlas_growths, snapshot/counted",
             "inconclusive in 1 of 1 runs: snapshot 3, counted 3", "inconclusive in 1 of 1 runs: main 2/none", ""]])
        no_counters = atlas_outcome([atlas_facts(growths=2)])
        no_facts = atlas_outcome([], [1])
        self.assertEqual(perf_atlas_rows.glyph_atlas_reconciliation_rows(
            "S9/default", perf_metrics.SideRuns([no_counters]), perf_metrics.SideRuns([no_facts])), [])
        head_only = perf_metrics.SideRuns(blocked=perf_counter_rows.COUNTERS_HEAD_ONLY)
        self.assertEqual(perf_atlas_rows.glyph_atlas_reconciliation_rows("S9/default", head_only, perf_metrics.SideRuns([no_reading])),
                         [["S9/default", "end", "glyph_atlas_growths, snapshot/counted", "n/a",
                           "inconclusive in 1 of 1 runs: snapshot 3, counted 3", ""]])

    def test_a_malformed_atlas_reading_is_a_schema_problem(self):
        # A reading names its attempt, the main window's label or null, and per-window and closed counts or
        # null; anything else would let the report compare a figure the harness never recorded.
        good = {"attempt": 1, "main_window": "WindowId(1)", "counted_glyph_atlas_growths": {"WindowId(1)": 2},
                "closed_glyph_atlas_growths": 0}
        point = {"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None, "atlas_readings": [good]}
        self.assertEqual(perf_result_schema.validate_result(perf_result_tests.valid_result(checkpoints=[point]), perf_memory_tests.HARNESS_HASH, 0), [])
        for bad in ({**good, "attempt": "1"}, {**good, "main_window": 3},
                    {**good, "counted_glyph_atlas_growths": {"WindowId(1)": -1}},
                    {**good, "closed_glyph_atlas_growths": 1.5}, "reading"):
            with self.subTest(bad=bad):
                self.assertTrue(perf_result_schema.validate_result(
                    perf_result_tests.valid_result(checkpoints=[{**point, "atlas_readings": [bad]}]), perf_memory_tests.HARNESS_HASH, 0))


class CheckpointFixtureTests(unittest.TestCase):
    """The harness's golden runs read as the harness meant them: exhausted, partial, or unsupported."""

    # Each late-exhaustion case and the attempts its record and authoritative sample must carry.
    LATE_CASES = {"eight-attempts": 8, "stale-clock": 1, "single-at-600": 1, "deadline-500": 5,
                  "deadline-610": 5}

    def test_every_fixture_line_parses_as_a_memory_sample(self):
        # The golden lines are what the harness's App wrote; each must survive `parse_memory_line`, so a
        # fixture whose lines the parser drops cannot pass by reading fewer samples.
        fixture = json.loads(CHECKPOINT_FIXTURE.read_text(encoding="utf-8"))
        for build, cases in fixture.items():
            for case, run in cases.items():
                for line in run["logs"]:
                    with self.subTest(build=build, case=case, line=line[:60]):
                        self.assertIn(perf_memory.MEMORY_MARKER, line)
                        self.assertIsNotNone(perf_memory.parse_memory_line(line))

    def test_late_exhaustion_reads_the_last_partial_attempt(self):
        # Each case's result passes the schema and records exhausted sampling with its attempt count
        # and a partial last attempt; the authoritative sample is that attempt, marked partial, never
        # the untagged periodic line logged before it, and its cells count as partial.
        for case, attempts in self.LATE_CASES.items():
            with self.subTest(case=case):
                result, samples = fixture_run("supported", case)
                self.assertEqual(perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0), [])
                self.assertEqual(result["checkpoint_memory"], "supported")
                point = result["checkpoints"][0]
                self.assertEqual((point["sampling"], point["attempts"], point["last_attempt_complete"]),
                                 ("exhausted", attempts, False))
                self.assertTrue(any(sample.checkpoint_index is None for sample in samples))
                reading = perf_memory.checkpoint_memory(samples, 0)
                self.assertEqual((reading.sample.checkpoint_attempt, reading.partial, reading.problem),
                                 (attempts, True, None))
                metrics = perf_metrics.run_metrics(fixture_outcome("supported", case))
                self.assertIsInstance(metrics[("end renderer_total_bytes", "MiB", "run")], perf_metrics.PartialValue)
                side = perf_metrics.SideRuns([fixture_outcome("supported", case)])
                row = perf_comparison_tests.row_for(perf_comparison_rows.comparison_rows("S1/default", side, side), "end renderer_total_bytes (MiB)")
                self.assertTrue(row[3].endswith(", 1 partial"), row)

    def test_a_build_without_the_hook_reads_n_a_unsupported_beside_a_hooked_head(self):
        # The unsupported run's result comes from the no-feature build's serializer: its checkpoint has
        # no sampling record, and its rows read `n/a: unsupported` though a periodic sample was logged.
        result, samples = fixture_run("unsupported", "unsupported")
        self.assertEqual(perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0), [])
        self.assertEqual(result["checkpoint_memory"], "unsupported")
        self.assertNotIn("sampling", result["checkpoints"][0])
        self.assertTrue(samples)
        base = perf_metrics.SideRuns([fixture_outcome("unsupported", "unsupported")])
        head = perf_metrics.SideRuns([fixture_outcome("supported", "deadline-610")])
        row = perf_comparison_tests.row_for(perf_comparison_rows.comparison_rows("S1/default", base, head), "end renderer_total_bytes (MiB)")
        self.assertEqual(row[2], "n/a: unsupported")
        self.assertTrue(row[3].endswith(", 1 partial"), row)


class CheckpointConflictTests(unittest.TestCase):
    """Every complete attempt is checked for conflicts before the authoritative sample is chosen."""

    def test_a_later_attempt_does_not_hide_an_earlier_conflict(self):
        # Attempt 1 has two complete samples with different totals (16 and 17 MiB); attempt 2 has one
        # complete sample (18 MiB). Choosing attempt 2 would hide the conflict, so the index conflicts.
        samples = [tagged_sample(0, 1, True, 16), tagged_sample(0, 1, True, 17), tagged_sample(0, 2, True, 18)]
        reading = perf_memory.checkpoint_memory(samples, 0)
        self.assertEqual((reading.sample, reading.problem), (None, "conflicting samples"))

    def test_identical_complete_duplicates_still_count_once(self):
        # The same reading twice in attempt 1, then attempt 2: no conflict, attempt 2 wins.
        samples = [tagged_sample(0, 1, True, 16), tagged_sample(0, 1, True, 16, unix_s=61.0),
                   tagged_sample(0, 2, True, 18)]
        reading = perf_memory.checkpoint_memory(samples, 0)
        self.assertIsNone(reading.problem)
        self.assertEqual(reading.sample.checkpoint_attempt, 2)
