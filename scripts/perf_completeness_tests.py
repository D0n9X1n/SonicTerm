"""Perf completeness tests."""

from __future__ import annotations
from pathlib import Path
import unittest

import perf_attribution_tests
import perf_metrics
import perf_outcomes_tests
import perf_report
import perf_result_schema
import perf_result_tests
import perf_sets


def end_point(completeness=None):
    """S9's or S12's `end` checkpoint, with a completeness reading when one is given."""
    point = {"index": 0, "label": "end", "unix_s": 10.0, "footprint_file": None}
    if completeness is not None:
        point["completeness"] = completeness
    return point


class CompletenessCheckpointTests(unittest.TestCase):
    """The perf-end glyph completeness reading: its schema and the rows the comparison records."""

    def test_a_reading_is_certified_counts_or_a_known_reason(self):
        # A certified reading has exactly its counts, positive dimensions and a scale; an unavailable one has
        # exactly a known reason and a scale. Anything else is a malformed checkpoint.
        unavailable = {"state": "unavailable", "reason": "scene changed", "scale": 2.0}
        for reading in (perf_attribution_tests.CERTIFIED, unavailable, {**unavailable, "reason": "api-disabled"}):
            with self.subTest(reading=reading):
                self.assertTrue(perf_result_schema._checkpoint_ok(end_point(reading)))
        malformed = {
            "negative count": {**perf_attribution_tests.CERTIFIED, "missing_terminal": -1},
            "boolean count": {**perf_attribution_tests.CERTIFIED, "missing_chrome": True},
            "zero dimension": {**perf_attribution_tests.CERTIFIED, "atlas_width": 0},
            "certified with a reason": {**perf_attribution_tests.CERTIFIED, "reason": "scene changed"},
            "unknown reason": {**unavailable, "reason": "maybe"},
            "unavailable with counts": {**unavailable, "missing_terminal": 0},
            "no scale": {key: value for key, value in perf_attribution_tests.CERTIFIED.items() if key != "scale"},
            "non-finite scale": {**perf_attribution_tests.CERTIFIED, "scale": float("inf")},
            "unknown state": {**unavailable, "state": "partial"},
            "not an object": "certified",
        }
        for name, reading in malformed.items():
            with self.subTest(name):
                self.assertFalse(perf_result_schema._checkpoint_ok(end_point(reading)), name)

    def test_s9_and_s12_end_give_one_row_per_run_and_side(self):
        # Each run of S9/default and S12/default gives one row naming its source, run id and attempt, the run's
        # attempt directory, measured SHA, side, platform, fixture, set and scale; other scenarios give none, and a
        # run whose `end` has no reading (an older base harness) reads `not recorded`, never certified.
        shas = {"base": "b" * 40, "head": "h" * 40}
        certified = perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[end_point(perf_attribution_tests.CERTIFIED)]),
                                 evidence=Path("/out/S9-default-head-timed-1"))
        older = perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[end_point()]))
        results = [
            perf_sets.SetResult("S9/default", "timed", base=perf_metrics.SideRuns([older]), head=perf_metrics.SideRuns([certified])),
            perf_sets.SetResult("S12/default", "counters", base=perf_metrics.SideRuns([certified]), head=perf_metrics.SideRuns([])),
            perf_sets.SetResult("S1/default", "timed", base=perf_metrics.SideRuns([certified]), head=perf_metrics.SideRuns([certified])),
        ]
        rows = perf_report.completeness_rows(results, shas, {"GITHUB_RUN_ID": "77", "GITHUB_RUN_ATTEMPT": "2"}, "darwin")
        self.assertEqual([(row["source"], row["side"], row["state"]) for row in rows],
                         [("perf_s9_end", "base", "unavailable"), ("perf_s9_end", "head", "certified"),
                          ("perf_s12_end", "base", "certified")])
        self.assertEqual(rows[0]["reason"], "not recorded")
        self.assertEqual(rows[1], {"source": "perf_s9_end", "run_id": "77", "run_attempt": "2",
                                   "attempt_dir": str(Path("/out/S9-default-head-timed-1")), "measured_sha": "h" * 40,
                                   "side": "head", "platform": "macos", "fixture": "S9", "set": "timed",
                                   "scale": 1.0, "state": "certified", "reason": None, "missing_terminal": 2,
                                   "missing_chrome": 0, "atlas_width": 512, "atlas_height": 512})
        self.assertEqual(rows[2]["measured_sha"], "b" * 40)
        local = perf_report.completeness_rows(results, shas, {}, "win32")[0]
        self.assertEqual((local["run_id"], local["run_attempt"]), ("local", "local"))
        self.assertEqual(perf_report.completeness_rows(results, shas, {}, "win32")[0]["platform"], "windows")

    def test_perf_rows_never_claim_a_helper_or_real_renderer_source(self):
        # The comparison measures only the renderer at the perf scenarios' end; the helper and real-renderer rows
        # come from test 17, so a perf row never uses their sources and the two kinds stay distinct.
        self.assertEqual(set(perf_report.COMPLETENESS_SOURCES.values()),
                         {("perf_s9_end", "S9"), ("perf_s12_end", "S12")})

    def test_the_summary_names_each_row(self):
        # comparison.md lists each row with its state and counts or reason, so a reviewer sees the census.
        rows = perf_report.completeness_rows(
            [perf_sets.SetResult("S9/default", "timed", base=perf_metrics.SideRuns([]),
                            head=perf_metrics.SideRuns([perf_outcomes_tests.make_outcome(result=perf_result_tests.valid_result(checkpoints=[end_point(perf_attribution_tests.CERTIFIED)]))]))],
            {"base": "b" * 40, "head": "h" * 40}, {}, "darwin")
        self.assertEqual(perf_report.completeness_lines(rows)[-1],
                         "- S9 timed head (macos, scale 1.0): certified, 2 terminal and 0 chrome missing, atlas 512x512")
        self.assertEqual(perf_report.completeness_lines([]), [])
