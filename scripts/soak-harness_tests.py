#!/usr/bin/env python3
"""Unit tests for scripts/soak-harness.py.

Flat sibling test module per the repository convention. The harness filename
contains a hyphen, so it cannot be imported by name; it is loaded from its path
with importlib and exercised through its public helpers (produce, main) and the
deterministic building blocks (analysis, rendering).

Run directly:  python3 scripts/soak-harness_tests.py
Or discovered:  python3 -m unittest soak-harness_tests   (from scripts/)
"""

import ctypes
import hashlib
import importlib.util
import io
import os
import tempfile
import unittest
from fractions import Fraction
from unittest import mock

_HERE = os.path.dirname(os.path.abspath(__file__))
_HARNESS_PATH = os.path.join(_HERE, "soak-harness.py")
_GOLDEN_PATH = os.path.join(_HERE, "soak-harness.golden.json")

_spec = importlib.util.spec_from_file_location("soak_harness", _HARNESS_PATH)
soak = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(soak)


def _config(argv):
    """Build a harness config from an argv list via the real parser."""
    args = soak._parser().parse_args(argv)
    return soak._Config(args)


class SchemaContractTests(unittest.TestCase):
    def test_schema_version_is_pinned(self):
        self.assertEqual(soak.SCHEMA_VERSION, "soak-harness/1")
        self.assertEqual(soak.schema()["schema_version"], "soak-harness/1")

    def test_schema_renders_deterministically(self):
        self.assertEqual(soak._render_bytes(soak.schema()),
                         soak._render_bytes(soak.schema()))

    def test_metric_fields_present_in_sample_keys(self):
        keys = soak.schema()["sample_keys"]
        for field in soak.METRIC_FIELDS:
            self.assertIn(field, keys)


class RenderDeterminismTests(unittest.TestCase):
    def test_render_is_lf_only_and_ascii(self):
        data = soak._render_bytes({"b": 1, "a": "x", "z": [3, 2, 1]})
        self.assertNotIn(b"\r", data)
        self.assertTrue(data.endswith(b"\n"))
        data.decode("ascii")  # Raises if any non-ASCII byte slipped through.

    def test_render_escapes_non_ascii(self):
        data = soak._render_bytes({"k": "é中"})
        data.decode("ascii")
        self.assertIn(b"\\u00e9", data)

    def test_render_sorts_keys(self):
        data = soak._render_bytes({"z": 1, "a": 2})
        self.assertLess(data.index(b'"a"'), data.index(b'"z"'))


class FixedPointTests(unittest.TestCase):
    def test_half_rounds_to_even(self):
        # round-half-to-even: 0.0000005 -> 0.000000, 0.0000015 -> 0.000002.
        self.assertEqual(soak._fixed_point(Fraction(5, 10_000_000)), "0.000000")
        self.assertEqual(soak._fixed_point(Fraction(15, 10_000_000)), "0.000002")

    def test_third_truncates_at_six_places(self):
        self.assertEqual(soak._fixed_point(Fraction(1, 3)), "0.333333")

    def test_negative_value(self):
        self.assertEqual(soak._fixed_point(Fraction(-3, 2)), "-1.500000")

    def test_integer_value(self):
        self.assertEqual(soak._fixed_point(Fraction(42)), "42.000000")


class SlopeTests(unittest.TestCase):
    def test_exact_linear_slope(self):
        # y = 2x + 1 -> slope exactly 2.
        points = [(x, 2 * x + 1) for x in range(10)]
        self.assertEqual(soak._linreg_slope(points), Fraction(2))

    def test_flat_slope_is_zero(self):
        points = [(x, 100) for x in range(10)]
        self.assertEqual(soak._linreg_slope(points), Fraction(0))

    def test_single_point_slope_is_zero(self):
        self.assertEqual(soak._linreg_slope([(3, 9)]), Fraction(0))


class PlateauTests(unittest.TestCase):
    def test_flat_series_plateaus_at_start(self):
        points = [(x, 500) for x in range(10)]
        reached, start = soak._plateau(points)
        self.assertTrue(reached)
        self.assertEqual(start, 0)

    def test_monotonic_growth_does_not_plateau(self):
        # Large steps keep every pair outside the 1% band.
        points = [(x, 100 * (x + 1)) for x in range(10)]
        reached, start = soak._plateau(points)
        self.assertFalse(reached)
        self.assertIsNone(start)

    def test_saturating_series_plateaus_late(self):
        values = [10, 20, 40, 80, 160, 300, 590, 599, 600, 600]
        points = list(enumerate(values))
        reached, start = soak._plateau(points)
        self.assertTrue(reached)
        self.assertGreater(start, 0)


class DeterminismTests(unittest.TestCase):
    def test_same_seed_same_bytes(self):
        argv = ["--scenario", "control", "--seed", "7", "--duration", "40"]
        a = soak.canonical_sha256(soak.produce(_config(argv)))
        b = soak.canonical_sha256(soak.produce(_config(argv)))
        self.assertEqual(a, b)

    def test_different_seed_different_bytes(self):
        a = soak.canonical_sha256(soak.produce(_config(["--seed", "1"])))
        b = soak.canonical_sha256(soak.produce(_config(["--seed", "2"])))
        self.assertNotEqual(a, b)

    def test_default_control_matches_pinned_golden_hash(self):
        # The shared cross-platform reference: every OS must reproduce these
        # exact bytes, so matching the pinned constant here on any host proves
        # cross-OS byte-identity by construction, not just self-consistency.
        result = soak.produce(_config(["--scenario", "control"]))
        self.assertEqual(soak.canonical_sha256(result),
                         soak.GOLDEN_CONTROL_SHA256)

    def test_default_control_matches_committed_golden_file(self):
        result = soak.produce(_config(["--scenario", "control"]))
        rendered = soak._render_bytes(soak._canonical_subset(result))
        with open(_GOLDEN_PATH, "rb") as handle:
            golden = handle.read()
        self.assertEqual(rendered, golden)
        self.assertEqual(hashlib.sha256(golden).hexdigest(),
                         soak.GOLDEN_CONTROL_SHA256)

    def test_canonical_subset_excludes_host_specific_keys(self):
        result = soak.produce(_config(["--scenario", "noop"]))
        subset = soak._canonical_subset(result)
        self.assertNotIn("environment", subset)
        self.assertNotIn("capabilities", subset)
        self.assertIn("capabilities", result)  # Present in the full result.

    def test_canonical_subset_has_no_float_values(self):
        # Floats format differently across platforms; the deterministic subset
        # must contain only ints, strings, bools, None, and containers.
        result = soak.produce(_config(["--scenario", "control"]))
        subset = soak._canonical_subset(result)

        def assert_no_float(node):
            if isinstance(node, float):
                self.fail("float found in canonical subset: {!r}".format(node))
            if isinstance(node, dict):
                for value in node.values():
                    assert_no_float(value)
            elif isinstance(node, (list, tuple)):
                for value in node:
                    assert_no_float(value)

        assert_no_float(subset)


class ScenarioShapeTests(unittest.TestCase):
    def test_noop_is_flat_with_zero_slope(self):
        result = soak.produce(_config(["--scenario", "noop", "--duration", "32"]))
        rss = result["analysis"]["fields"]["rss_bytes"]
        self.assertEqual(rss["delta"], 0)
        self.assertEqual(rss["slope_per_tick_fixed6"], "0.000000")
        self.assertTrue(rss["plateau_reached"])

    def test_control_rss_grows(self):
        result = soak.produce(_config(["--scenario", "control", "--duration", "64"]))
        rss = result["analysis"]["fields"]["rss_bytes"]
        self.assertGreater(rss["delta"], 0)
        self.assertGreater(Fraction(rss["slope_per_tick_fixed6"]), 0)


class AlwaysEmitTests(unittest.TestCase):
    def test_hard_stop_on_max_samples_still_produces_artifact(self):
        result = soak.produce(_config(
            ["--scenario", "control", "--duration", "64", "--max-samples", "4"]))
        self.assertEqual(result["status"], "hard_stop")
        self.assertEqual(result["stop_reason"], "max_samples")
        self.assertEqual(result["sample_count"], 4)
        self.assertEqual(result["schema_version"], soak.SCHEMA_VERSION)

    def test_hard_stop_on_max_rss_still_produces_artifact(self):
        result = soak.produce(_config(
            ["--scenario", "control", "--max-rss-bytes", "1"]))
        self.assertEqual(result["status"], "hard_stop")
        self.assertEqual(result["stop_reason"], "max_rss_bytes")
        self.assertGreaterEqual(result["sample_count"], 1)

    def test_internal_error_still_produces_artifact(self):
        original = soak._collect
        try:
            def boom(_config, _samples):
                raise RuntimeError("synthetic failure")
            soak._collect = boom
            result = soak.produce(_config(["--scenario", "control"]))
        finally:
            soak._collect = original
        self.assertEqual(result["status"], "error")
        self.assertIn("RuntimeError", result["error"])
        self.assertEqual(result["schema_version"], soak.SCHEMA_VERSION)

    def test_analysis_error_still_returns_partial_artifact(self):
        original = soak._analyze
        try:
            def boom(_config, _samples):
                raise RuntimeError("analysis failure")
            soak._analyze = boom
            result = soak.produce(_config([
                "--scenario", "control", "--duration", "4", "--warmup", "0",
            ]))
        finally:
            soak._analyze = original
        self.assertEqual(result["status"], "error")
        self.assertIn("assembly: RuntimeError: analysis failure", result["error"])
        self.assertEqual(result["sample_count"], 4)
        self.assertEqual(len(result["samples"]), 4)
        self.assertIsNone(result["analysis"])

    def test_capability_error_still_returns_partial_artifact(self):
        original = soak._capabilities
        try:
            def boom():
                raise RuntimeError("capability failure")
            soak._capabilities = boom
            result = soak.produce(_config([
                "--scenario", "control", "--duration", "4", "--warmup", "0",
            ]))
        finally:
            soak._capabilities = original
        self.assertEqual(result["status"], "error")
        self.assertIn("assembly: RuntimeError: capability failure", result["error"])
        self.assertEqual(result["sample_count"], 4)
        self.assertEqual(len(result["samples"]), 4)
        self.assertIsNone(result["capabilities"])

    def test_partial_samples_survive_hard_stop(self):
        # The always-emit guarantee promises partial progress, not just a
        # status: a mid-run stop must still carry the samples collected so far.
        result = soak.produce(_config(
            ["--scenario", "control", "--duration", "64", "--max-samples", "5"]))
        self.assertEqual(result["status"], "hard_stop")
        self.assertEqual(len(result["samples"]), 5)


class _FakeFunction:
    def __init__(self, callback):
        self.callback = callback
        self.argtypes = None
        self.restype = None

    def __call__(self, *args):
        return self.callback(*args)


class _FakeKernel32:
    def __init__(self, *, memory_ok=True, handles_ok=True):
        self.handle = ctypes.c_void_p(-1).value
        self.seen_memory_handle = None
        self.seen_handle_handle = None
        self.GetCurrentProcess = _FakeFunction(lambda: self.handle)
        self.K32GetProcessMemoryInfo = _FakeFunction(
            lambda handle, counters, size: self._memory(
                handle, counters, size, memory_ok
            )
        )
        self.GetProcessHandleCount = _FakeFunction(
            lambda handle, count: self._handles(handle, count, handles_ok)
        )

    def _memory(self, handle, counters, size, ok):
        self.seen_memory_handle = handle
        if not ok:
            return 0
        struct = counters._obj
        self.assert_equal(size, ctypes.sizeof(type(struct)))
        struct.WorkingSetSize = 123_456
        struct.PrivateUsage = 98_765
        return 1

    def _handles(self, handle, count, ok):
        self.seen_handle_handle = handle
        if not ok:
            return 0
        count._obj.value = 42
        return 1

    @staticmethod
    def assert_equal(actual, expected):
        if actual != expected:
            raise AssertionError("{} != {}".format(actual, expected))


class LiveCaptureTests(unittest.TestCase):
    def test_rss_probe_error_degrades_to_unavailable(self):
        class _FailingResource:
            RUSAGE_SELF = 0

            @staticmethod
            def getrusage(_who):
                raise OSError("probe failed")

        original = soak.sys.modules.get("resource")
        try:
            soak.sys.modules["resource"] = _FailingResource
            self.assertIsNone(soak._live_rss_bytes())
        finally:
            if original is None:
                del soak.sys.modules["resource"]
            else:
                soak.sys.modules["resource"] = original

    def test_windows_typed_process_and_handle_probes(self):
        kernel32 = _FakeKernel32()
        with mock.patch.object(ctypes, "WinDLL", return_value=kernel32, create=True):
            memory = soak._windows_process_memory()
            handles = soak._windows_handle_count()

        self.assertEqual(
            memory,
            {"rss_bytes": 123_456, "private_bytes": 98_765},
        )
        self.assertEqual(handles, 42)
        self.assertEqual(kernel32.seen_memory_handle, kernel32.handle)
        self.assertEqual(kernel32.seen_handle_handle, kernel32.handle)
        self.assertEqual(kernel32.GetCurrentProcess.argtypes, [])
        self.assertIsNotNone(kernel32.GetCurrentProcess.restype)
        self.assertEqual(len(kernel32.K32GetProcessMemoryInfo.argtypes), 3)
        self.assertEqual(len(kernel32.GetProcessHandleCount.argtypes), 2)

    def test_windows_probe_failures_degrade_to_unavailable(self):
        kernel32 = _FakeKernel32(memory_ok=False, handles_ok=False)
        with mock.patch.object(ctypes, "WinDLL", return_value=kernel32, create=True):
            self.assertIsNone(soak._windows_process_memory())
            self.assertIsNone(soak._windows_handle_count())


class ValidationAndCliTests(unittest.TestCase):
    def test_output_path_error_falls_back_to_stdout_artifact(self):
        import json as _json

        class _FakeStdout:
            def __init__(self):
                self.buffer = io.BytesIO()

        fake = _FakeStdout()
        original = soak.sys.stdout
        try:
            soak.sys.stdout = fake
            with tempfile.TemporaryDirectory() as directory:
                code = soak.main([
                    "--scenario", "noop", "--duration", "2", "--warmup", "0",
                    "--out", directory,
                ])
        finally:
            soak.sys.stdout = original
        result = _json.loads(fake.buffer.getvalue().decode("utf-8"))
        self.assertEqual(code, 1)
        self.assertEqual(result["schema_version"], soak.SCHEMA_VERSION)
        self.assertEqual(result["status"], "error")
        self.assertTrue(result["error"].startswith("emit: "), result["error"])
        self.assertIn("Error:", result["error"])
        self.assertEqual(result["sample_count"], 2)

    def test_zero_duration_is_usage_error(self):
        self.assertEqual(soak.main(["--duration", "0", "--out", "-"]), 2)

    def test_negative_warmup_is_usage_error(self):
        self.assertEqual(soak.main(["--warmup", "-1", "--out", "-"]), 2)

    def test_fail_on_hard_stop_exit_code(self):
        code = soak.main([
            "--scenario", "control", "--max-samples", "2",
            "--fail-on-hard-stop", "--out", os.devnull,
        ])
        self.assertEqual(code, 3)

    def test_main_writes_canonical_golden_to_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, "result.json")
            code = soak.main(["--scenario", "control", "--canonical", "--out", out])
            self.assertEqual(code, 0)
            with open(out, "rb") as handle:
                data = handle.read()
            self.assertNotIn(b"\r", data)
            self.assertEqual(hashlib.sha256(data).hexdigest(),
                             soak.GOLDEN_CONTROL_SHA256)

    def test_print_schema_is_valid_and_deterministic(self):
        import json as _json

        class _FakeStdout:
            def __init__(self):
                self.buffer = io.BytesIO()

        fake = _FakeStdout()
        original = soak.sys.stdout
        try:
            soak.sys.stdout = fake
            self.assertEqual(soak.main(["--print-schema"]), 0)
        finally:
            soak.sys.stdout = original
        payload = _json.loads(fake.buffer.getvalue().decode("utf-8"))
        self.assertEqual(payload["schema_version"], soak.SCHEMA_VERSION)


if __name__ == "__main__":
    unittest.main(verbosity=2)
