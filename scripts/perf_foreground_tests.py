"""Perf foreground tests."""

from __future__ import annotations
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

import perf_comparison_rows
import perf_execution
import perf_focus
import perf_memory_tests
import perf_metrics
import perf_outcomes
import perf_processes_tests
import perf_result_schema
import perf_result_tests
import perf_settings
import perf_watcher
import perf_watcher_tests
# A hardware adapter as parse_adapter_line reads it, and a wgpu presenter that is not degraded.
HARDWARE_RENDERER = {"event": "selected", "backend": "Dx12", "name": "NVIDIA GeForce RTX 4070",
                     "driver": "32.0.15.6094", "device_type": "DiscreteGpu", "software_rendering": False}
WGPU_PRESENTER = {"software_render_mode": "auto", "software_rendering": False, "software_render_degraded": False,
                  "windows_gdi": False}


def macos_run(**presenter):
    """A factory for a valid macOS run, which logs no adapter; overrides change the presenter's fields."""
    def build(plan):
        # Resolve this runtime-only dependency after module initialization.
        import perf_outcomes_tests
        result = perf_result_tests.valid_result(presenter=dict(WGPU_PRESENTER, **presenter))
        return perf_outcomes_tests.make_outcome(plan=plan, platform="darwin", result=result)
    return build


def windows_run(renderer=None, grid=None, **presenter):
    """A factory for a valid Windows run on one adapter and presenter; overrides change the presenter's fields."""
    def build(plan):
        # Resolve this runtime-only dependency after module initialization.
        import perf_outcomes_tests
        result = perf_result_tests.valid_result(grid=grid or {"cols": 250, "rows": 70}, presenter=dict(WGPU_PRESENTER, **presenter))
        return perf_outcomes_tests.make_outcome(plan=plan, platform="win32", renderer=renderer or HARDWARE_RENDERER, result=result)
    return build


def foreground(hwnd, pid, error=None):
    """One classified foreground sample, as sample_foreground records it."""
    return perf_focus.classify_foreground(hwnd, pid, error, unix_s=perf_processes_tests.LAUNCH_UNIX_S)


class ForegroundTests(unittest.TestCase):
    def test_null_window_pid_zero_and_api_errors_classify_apart(self):
        # No foreground window is `none`; a window without a pid, or a failed call, is a failed sample.
        self.assertEqual(foreground(0, 0).kind, "none")
        self.assertEqual(foreground(0x1234, 0).kind, "failed")
        self.assertEqual(foreground(0x1234, 77, error="GetWindowThreadProcessId failed: 5").kind, "failed")
        reading = foreground(0x1234, 77)
        self.assertEqual((reading.kind, reading.pid), ("app", 77))

    def test_each_sample_keeps_the_front_samples_schema(self):
        # front-samples.log holds one record schema on every host, so a Windows sample is a CommandRecord too.
        record = foreground(0x1234, 77).records[0]
        self.assertEqual(record.argv, ("GetForegroundWindow",))
        self.assertEqual(set(record.as_json()), {"unix_s", "argv", "exit_code", "timed_out", "stdout", "stderr"})
        self.assertEqual((record.unix_s, record.exit_code), (perf_processes_tests.LAUNCH_UNIX_S, 0))
        self.assertEqual(foreground(0x1234, 0, error="denied").records[0].exit_code, 1)
        with tempfile.TemporaryDirectory() as temporary:
            log_path = Path(temporary) / "front-samples.log"
            sampler = perf_watcher.FrontSampler(log_path, None, set(), sample=lambda: foreground(0x1234, 77))
            with contextlib.redirect_stdout(io.StringIO()):
                sampler.sample()
            self.assertEqual(json.loads(log_path.read_text(encoding="utf-8"))["argv"], ["GetForegroundWindow"])

    def test_another_app_taking_the_foreground_invalidates_a_desk_run(self):
        # At a desk any foreground change during the run may have moved the user's focus, so the run is invalid.
        verdict = perf_focus.judge_foreground([foreground(1, 100), foreground(2, 200)], user_session=True)
        self.assertFalse(verdict.passed)
        self.assertEqual([(change["from_pid"], change["to_pid"]) for change in verdict.changes], [(100, 200)])

    def test_a_null_reading_between_the_same_app_is_bridged(self):
        # A moment with no foreground window is no change when the same application comes back.
        verdict = perf_focus.judge_foreground([foreground(1, 100), foreground(0, 0), foreground(1, 100)],
                                        user_session=True)
        self.assertTrue(verdict.passed)
        self.assertEqual(verdict.changes, [])

    def test_the_harness_becoming_foreground_is_a_change(self):
        # Launch must not take the foreground, so the harness taking it counts like any other application.
        verdict = perf_focus.judge_foreground([foreground(0, 0), foreground(1, 100), foreground(0, 0),
                                         foreground(3, perf_watcher_tests.HARNESS_PID)], user_session=True)
        self.assertFalse(verdict.passed)
        self.assertEqual([change["to_pid"] for change in verdict.changes], [perf_watcher_tests.HARNESS_PID])

    def test_without_a_user_session_a_change_is_only_noted(self):
        # A GitHub-hosted runner has no user focus to take, so a change is recorded, not failed.
        verdict = perf_focus.judge_foreground([foreground(1, 100), foreground(2, 200)], user_session=False)
        self.assertTrue(verdict.passed)
        self.assertEqual(len(verdict.changes), 1)
        self.assertTrue(verdict.notes)
        failed = perf_focus.judge_foreground([foreground(1, 100), foreground(2, 0)], user_session=False)
        self.assertFalse(failed.passed)


def checkpoint_outcome(platform_name, labels, **overrides):
    """A valid run on `platform_name` whose result has one checkpoint per label."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_outcomes_tests
    checkpoints = [{"index": index, "label": label, "unix_s": 70.0, "footprint_file": None}
                   for index, label in enumerate(labels)]
    result = perf_result_tests.valid_result(checkpoints=checkpoints, uncover_ms=overrides.pop("uncover_ms", None))
    return perf_outcomes_tests.make_outcome(platform=platform_name, result=result, **overrides)


class WindowsTableTests(unittest.TestCase):
    def rows(self, label, outcome):
        """The table rows of one scenario whose sides each hold `outcome`, keyed by metric."""
        sides = [perf_metrics.SideRuns(outcomes=[outcome]) for _ in perf_settings.SIDES]
        return {row[1]: row for row in perf_comparison_rows.comparison_rows(label, *sides)}

    def test_s12_on_windows_has_n_a_occlusion_rows(self):
        # Windows reports no occlusion, so S12's uncover and covered-memory rows say n/a and why.
        rows = self.rows("S12/default", checkpoint_outcome("win32", ["settled", "covered", "end"]))
        for metric in ("uncover (ms)", "memory released while covered (MiB)"):
            with self.subTest(metric=metric):
                self.assertEqual(rows[metric][2:], ["n/a", "n/a", "Windows reports no occlusion"])
        self.assertNotIn("uncover (ms)", self.rows("S1/default", checkpoint_outcome("win32", ["end"])))

    def test_every_windows_checkpoint_has_an_n_a_footprint_row(self):
        # Windows has no footprint tool, so each checkpoint's footprint row says n/a and why.
        rows = self.rows("S12/default", checkpoint_outcome("win32", ["settled", "end"]))
        for label in ("settled", "end"):
            with self.subTest(label=label):
                self.assertEqual(rows[f"{label} footprint (MiB)"][2:], ["n/a", "n/a", "Windows has no `footprint`"])

    def test_a_macos_comparison_keeps_its_measured_rows(self):
        # macOS measures uncover and footprint, so its rows keep their figures and no Windows note appears; its
        # presenter row names wgpu, so a reader sees the run stayed on the hardware path.
        outcome = checkpoint_outcome("darwin", ["end"], uncover_ms=120.0,
                                     footprints={"1-end": {"bytes": 64 * perf_execution.MIB}})
        rows = self.rows("S12/default", outcome)
        self.assertTrue(rows["uncover (ms)"][2].startswith("120.00"), rows["uncover (ms)"])
        self.assertTrue(rows["end footprint (MiB)"][2].startswith("64.00"), rows["end footprint (MiB)"])
        self.assertFalse(any("Windows" in cell for row in rows.values() for cell in row))
        self.assertEqual(rows["presenter"][2:4], ["wgpu", "wgpu"])

    def test_synthetic_occlusion_on_windows_is_a_schema_failure(self):
        # Windows reports no occlusion, so a synthetic one there means the harness did what it must not.
        result = perf_result_tests.valid_result(synthetic_occlusion=True)
        self.assertTrue(perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0, platform_name="win32"))
        self.assertEqual(perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0), [])

    def test_presenter_types_are_checked_when_present(self):
        # The presenter object is optional; when present each field has its type, and the schema stays 1.
        self.assertEqual(perf_result_schema.validate_result(perf_result_tests.valid_result(presenter=WGPU_PRESENTER), perf_memory_tests.HARNESS_HASH, 0), [])
        self.assertEqual(perf_result_schema.validate_result(perf_result_tests.valid_result(presenter=dict(WGPU_PRESENTER, software_render_mode=None)),
                                              perf_memory_tests.HARNESS_HASH, 0), [])
        for broken in ("gdi", dict(WGPU_PRESENTER, windows_gdi="no"), {"software_rendering": False}):
            with self.subTest(broken=broken):
                self.assertTrue(perf_result_schema.validate_result(perf_result_tests.valid_result(presenter=broken), perf_memory_tests.HARNESS_HASH, 0))

    def test_a_valid_windows_or_macos_result_must_carry_its_presenter(self):
        # Every Windows and macOS run records how it presented, so a valid win32 or darwin result without one is a
        # schema problem; a run that ended early need not have one.
        for platform in ("win32", "darwin"):
            with self.subTest(platform=platform):
                missing = perf_result_tests.without_presenter(perf_result_tests.valid_result())
                problems = perf_result_schema.validate_result(missing, perf_memory_tests.HARNESS_HASH, 0, platform_name=platform)
                self.assertTrue(any("presenter" in problem for problem in problems), problems)
                self.assertEqual(perf_result_schema.validate_result(perf_result_tests.valid_result(presenter=WGPU_PRESENTER), perf_memory_tests.HARNESS_HASH, 0,
                                                      platform_name=platform), [])
                invalid = perf_result_tests.without_presenter(perf_result_tests.valid_result(status="invalid", exit_code=3))
                self.assertEqual(perf_result_schema.validate_result(invalid, perf_memory_tests.HARNESS_HASH, 3, platform_name=platform), [])

    def test_a_macos_presenter_reads_wgpu_or_wgpu_degraded(self):
        # macOS never presents through GDI, so its row names wgpu and says when the degrade path was taken.
        # Resolve this runtime-only dependency after module initialization.
        import perf_outcomes_tests
        for degraded, expected in ((False, "wgpu"), (True, "wgpu, degraded")):
            with self.subTest(degraded=degraded):
                result = perf_result_tests.valid_result(presenter=dict(WGPU_PRESENTER, software_render_degraded=degraded))
                self.assertEqual(perf_metrics.presenter_text(perf_outcomes_tests.make_outcome(platform="darwin", result=result)), expected)

    def test_a_gdi_or_wgpu_run_without_its_presenter_record_is_blocked(self):
        # gdi and wgpu exist to measure one presenter, so a run that recorded none proves neither; the
        # default variant names no presenter and needs none.
        # Resolve this runtime-only dependency after module initialization.
        import perf_outcomes_tests
        for variant in ("gdi", "wgpu"):
            with self.subTest(variant=variant):
                plan = perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, variant, "head", Path("/b"), perf_memory_tests.HARNESS_HASH)
                reason = perf_outcomes.presenter_blocked(perf_outcomes_tests.make_outcome(plan=plan, result=perf_result_tests.without_presenter(perf_result_tests.valid_result())))
                self.assertIsNotNone(reason)
                self.assertIn("presenter", reason)
        self.assertIsNone(perf_outcomes.presenter_blocked(perf_outcomes_tests.make_outcome(result=perf_result_tests.without_presenter(perf_result_tests.valid_result()))))
        self.assertIsNone(perf_outcomes.presenter_blocked(perf_outcomes_tests.make_outcome()))
