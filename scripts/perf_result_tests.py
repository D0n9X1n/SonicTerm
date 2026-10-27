"""Perf result tests."""

from __future__ import annotations
import unittest

import perf_attribution_schema
import perf_memory_tests
import perf_result_schema
import perf_watcher_tests


def without_presenter(result):
    """`result` with its presenter record removed, for a test of a run that recorded none."""
    result.pop("presenter", None)
    return result


# The presenter a macOS run on the hardware path records: wgpu, not degraded, never Windows GDI.
MACOS_PRESENTER = {"software_render_mode": "auto", "software_rendering": False, "software_render_degraded": False,
                   "windows_gdi": False}


def valid_result(**overrides):
    """A result.json body that satisfies the schema; overrides replace top-level keys. It carries the presenter a
    valid macOS result must record; a test of a run that recorded none removes it with `without_presenter`."""
    phase = {"name": "workload", "start_unix_s": 10.0, "end_unix_s": 70.0, "cpu_user_s": 1.5,
             "cpu_system_s": 0.5, "presented_frames": 120, "redraw_requested": 130,
             "dispatch_ms": [1.0, 2.0], "present_interval_ms": [16.6, 16.7], "allocations_per_frame": None}
    result = {"schema_version": 1, "managed": True, "harness_hash": perf_memory_tests.HARNESS_HASH, "status": "valid",
              "exit_code": 0, "grid": {"columns": 250, "rows": 70}, "phases": [phase],
              "latency": None, "throughput": {"bytes": 1000, "seconds": 2.0}, "uncover_ms": None,
              "scrollback_rows_retained": None,
              "checkpoints": [{"index": 0, "label": "end", "unix_s": 70.0, "footprint_file": None}],
              "finish_session_settled": True, "notes": [], "presenter": dict(MACOS_PRESENTER)}
    result.update(overrides)
    return result


class ResultSchemaTests(unittest.TestCase):
    def test_valid_result_passes(self):
        # The documented schema, with extra keys tolerated, has no problem.
        self.assertEqual(perf_result_schema.validate_result({**valid_result(), "scenario": "S1"}, perf_memory_tests.HARNESS_HASH, 0), [])

    def test_unmanaged_result_or_another_harness_fails(self):
        # A standalone run or a result from another harness can never enter a comparison.
        for overrides in ({"managed": False}, {"managed": None}, {"harness_hash": "cd" * 32}):
            with self.subTest(overrides=overrides):
                self.assertTrue(perf_result_schema.validate_result(valid_result(**overrides), perf_memory_tests.HARNESS_HASH, 0))
        missing = valid_result()
        del missing["managed"]
        self.assertTrue(perf_result_schema.validate_result(missing, perf_memory_tests.HARNESS_HASH, 0))

    def test_schema_version_must_be_one(self):
        # Another version or a string version means the parser cannot trust any field.
        for version in (2, "1", None, True):
            with self.subTest(version=version):
                self.assertTrue(perf_result_schema.validate_result(valid_result(schema_version=version), perf_memory_tests.HARNESS_HASH, 0))
        self.assertTrue(perf_result_schema.validate_result([], perf_memory_tests.HARNESS_HASH, 0))

    def test_field_types_are_checked(self):
        # Wrong types fail instead of reading as zero or as a missing metric.
        broken_phase = dict(valid_result()["phases"][0], dispatch_ms=[1.0, "slow"])
        for overrides in ({"phases": {}}, {"phases": [broken_phase]},
                          {"phases": [dict(broken_phase, dispatch_ms=[], presented_frames=None)]},
                          {"checkpoints": [{"index": 0, "unix_s": 1.0}]},
                          {"finish_session_settled": "yes"}, {"exit_code": True},
                          {"latency": {"samples": [], "attributed": 0, "total": 0}},
                          {"throughput": {"bytes": 1}}, {"notes": "none"}, {"grid": None}):
            with self.subTest(overrides=overrides):
                self.assertTrue(perf_result_schema.validate_result(valid_result(**overrides), perf_memory_tests.HARNESS_HASH, 0))

    def test_partial_result_with_null_fields_passes(self):
        # A timed-out or early result writes null for what it never measured; that is not a schema failure.
        latency = {"samples": [{"inject_unix_s": 2.0, "latency_ms": None, "reason": "no-candidate"},
                               {"inject_unix_s": 3.0, "latency_ms": 7.5, "reason": None}],
                   "attributed": 1, "total": 2, "coverage": 0.5}
        early = valid_result(status="timeout", exit_code=4, grid=None, finish_session_settled=None,
                             latency=latency, invalid_reason=None, harness_pid=perf_watcher_tests.HARNESS_PID)
        self.assertEqual(perf_result_schema.validate_result(early, perf_memory_tests.HARNESS_HASH, 4), [])
        self.assertTrue(perf_result_schema.validate_result(valid_result(finish_session_settled="yes"), perf_memory_tests.HARNESS_HASH, 0))
        broken = dict(latency, samples=[{"latency_ms": "slow"}])
        self.assertTrue(perf_result_schema.validate_result(valid_result(latency=broken), perf_memory_tests.HARNESS_HASH, 0))

    def test_valid_result_ends_with_an_end_checkpoint(self):
        # Memory is reported for every checkpoint label, and a valid run always ends with `end`.
        other = [{"index": 0, "label": "idle", "unix_s": 50.0, "footprint_file": None}]
        self.assertTrue(perf_result_schema.validate_result(valid_result(checkpoints=other), perf_memory_tests.HARNESS_HASH, 0))
        invalid = valid_result(checkpoints=other, status="invalid", exit_code=3)
        self.assertEqual(perf_result_schema.validate_result(invalid, perf_memory_tests.HARNESS_HASH, 3), [])

    def test_recorded_exit_code_matches_the_process(self):
        # A result whose exit code disagrees with the process describes another run.
        self.assertTrue(perf_result_schema.validate_result(valid_result(), perf_memory_tests.HARNESS_HASH, 3))
        self.assertEqual(perf_result_schema.validate_result(valid_result(), perf_memory_tests.HARNESS_HASH, None), [])

    def test_occlusion_reason_marks_an_environmental_invalidation(self):
        # Only an occlusion is retried in the smoke; other invalidations are not environmental.
        self.assertTrue(perf_attribution_schema.occlusion_invalidated(
            valid_result(status="invalid", notes=["unrequested native occlusion change"])))
        self.assertFalse(perf_attribution_schema.occlusion_invalidated(
            valid_result(status="invalid", notes=["unexpected keyboard input"])))
        self.assertFalse(perf_attribution_schema.occlusion_invalidated(valid_result(notes=["occlusion synthetic"])))
