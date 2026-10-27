"""Perf memory tests."""

from __future__ import annotations
from pathlib import Path
import tempfile
import unittest

import perf_cell_layout
import perf_font_diagnostics
import perf_memory
import perf_processes_tests


def memory_line(stamp=perf_processes_tests.STAMP, resident="123", renderer="456", session="789", prefix="memory: ",
                grid=""):
    """One `memory snapshot` line as the file layer writes it; `grid` adds the grid fields."""
    return (f"{stamp}  INFO {prefix}memory snapshot process_private_committed_bytes=unsupported "
            f"process_resident_bytes={resident} process_virtual_bytes=unsupported "
            f"session_total_bytes={session} {grid}renderer_total_bytes={renderer} renderers=[main warm] "
            f"allocator_state=unsupported")


GRID_FIELDS = ("grid_visible_bytes=3000000 grid_history_bytes=6000000 grid_alternate_bytes=0 "
               "panes_total=3 panes_sampled=3 panes_contended=0 ")


class MemoryLineTests(unittest.TestCase):
    def test_snapshot_line_parses_byte_counts_and_unsupported_values(self):
        # Process fields may read `unsupported`; extra fields are tolerated.
        sample = perf_memory.parse_memory_line(memory_line(resident="unsupported"))
        self.assertIsNone(sample.process_resident_bytes)
        self.assertEqual((sample.renderer_total_bytes, sample.session_total_bytes), (456, 789))
        self.assertAlmostEqual(sample.unix_s, perf_processes_tests.STAMP_UNIX_S, places=5)
        self.assertEqual(perf_memory.parse_memory_line(memory_line()).process_resident_bytes, 123)

    def test_a_span_may_precede_the_target(self):
        # The line is found by its message, wherever a span puts the target.
        line = memory_line(prefix="run{scenario=S1}: memory: ")
        self.assertEqual(perf_memory.parse_memory_line(line).renderer_total_bytes, 456)

    def test_other_memory_events_and_malformed_lines_are_ignored(self):
        # Other events share the `memory` target; required totals must be integers.
        for line in (f"{perf_processes_tests.STAMP}  INFO memory: reclaimed renderer_total_bytes=5 session_total_bytes=1",
                     f"{perf_processes_tests.STAMP} DEBUG memory: pane snapshot renderer_total_bytes=5",
                     memory_line(renderer="unsupported"), memory_line(session=""),
                     memory_line(stamp="yesterday"), ""):
            with self.subTest(line=line):
                self.assertIsNone(perf_memory.parse_memory_line(line))

    def test_grid_fields_parse_and_give_bytes_per_pane(self):
        # Visible + history + alternate over the panes sampled is the per-pane grid figure.
        sample = perf_memory.parse_memory_line(memory_line(grid=GRID_FIELDS))
        self.assertEqual((sample.grid_visible_bytes, sample.grid_history_bytes,
                          sample.grid_alternate_bytes, sample.panes_sampled), (3000000, 6000000, 0, 3))
        self.assertEqual(sample.grid_bytes_per_pane(), 3000000)

    def test_missing_grid_fields_or_no_sampled_pane_give_no_per_pane_figure(self):
        # An older line without the fields, a malformed field, or zero sampled panes reads unavailable.
        for grid in ("", GRID_FIELDS.replace("grid_history_bytes=6000000", "grid_history_bytes=x"),
                     GRID_FIELDS.replace("panes_sampled=3", "panes_sampled=0")):
            with self.subTest(grid=grid):
                sample = perf_memory.parse_memory_line(memory_line(grid=grid))
                self.assertIsNotNone(sample, "the required totals still parse")
                self.assertIsNone(sample.grid_bytes_per_pane())

    def test_each_renderers_glyph_atlas_facts_parse_in_order_with_every_fit_label(self):
        # The renderer breakdown carries six glyph atlas facts per renderer under its identity; every fit
        # label parses, an older line without them gives no facts, and an unknown fit label is not a fact.
        def facts(dim, fit):
            return (f"glyph_atlas_dim={dim} glyph_atlas_packed_pixels=4000 glyph_atlas_growths=1 "
                    f"glyph_atlas_evictions=0 glyph_atlas_fit={fit} glyph_atlas_max_tile=25x16")
        for fit in ("256", "512", "1024", "2048", "no_headroom", "does_not_fit", "evicted"):
            with self.subTest(fit=fit):
                line = memory_line().replace(
                    "renderers=[main warm]",
                    f'renderers="main[1] total=9/1 {facts(512, fit)}; warm[0] total=1/1 {facts(256, "256")}"')
                atlases = perf_memory.parse_memory_line(line).glyph_atlases
                # Each renderer's facts carry its `role[label]` identity from the breakdown.
                self.assertEqual(atlases, (perf_memory.GlyphAtlasFacts("main[1]", 512, 4000, 1, 0, fit, (25, 16)),
                                           perf_memory.GlyphAtlasFacts("warm[0]", 256, 4000, 1, 0, "256", (25, 16))))
        self.assertEqual(perf_memory.parse_memory_line(memory_line()).glyph_atlases, ())
        unknown = memory_line().replace("renderers=[main warm]", f"renderers={facts(512, 'tiny')}")
        self.assertEqual(perf_memory.parse_memory_line(unknown).glyph_atlases, ())

    def test_checkpoint_takes_the_latest_line_at_or_before_it(self):
        # A line written after the checkpoint never describes it.
        samples = [perf_memory.MemorySample(unix_s, None, int(unix_s), 0) for unix_s in (10.0, 20.0, 30.0)]
        self.assertEqual(perf_cell_layout.memory_at(samples, 25.0).unix_s, 20.0)
        self.assertEqual(perf_cell_layout.memory_at(samples, 20.0).unix_s, 20.0)
        self.assertIsNone(perf_cell_layout.memory_at(samples, 5.0))

    def test_log_directory_reads_every_rolled_file_but_not_breadcrumbs(self):
        # Rolled daily files are all read; subdirectories such as breadcrumbs are not.
        with tempfile.TemporaryDirectory() as temporary:
            logs = Path(temporary)
            (logs / "sonicterm.log.2026-10-03").write_text(
                memory_line(stamp="2026-10-03T00:00:01Z", renderer="2") + "\n", encoding="utf-8")
            (logs / "sonicterm.log.2026-10-02").write_text(memory_line() + "\nnoise\n", encoding="utf-8")
            (logs / "breadcrumbs").mkdir()
            (logs / "breadcrumbs" / "breadcrumbs-1-2-3.log").write_text(memory_line(renderer="9"), encoding="utf-8")
            samples = perf_memory.read_memory_samples(logs)
        self.assertEqual([sample.renderer_total_bytes for sample in samples], [456, 2])
        self.assertEqual(perf_memory.read_memory_samples(Path(temporary) / "missing"), [])


class RenderTimingTests(unittest.TestCase):
    LINE = ("2026-10-02T11:24:16.5Z DEBUG render_timing: line=[render_timing] window=main "
            "total=4.20ms prepare=1.00ms present=3.00ms tail=0.20ms")

    def test_render_timing_field_parses_every_lap(self):
        # The fmt layer writes the text as the `line` field; every lap and the total are kept.
        sample = perf_font_diagnostics.parse_render_timing(self.LINE)
        self.assertEqual(sample.window, "main")
        self.assertEqual(sample.laps, {"total": 4.2, "prepare": 1.0, "present": 3.0, "tail": 0.2})

    def test_malformed_or_unrelated_lines_are_ignored(self):
        # A lap without a millisecond value would shift every later lap, so the line is dropped.
        for line in (self.LINE.replace("prepare=1.00ms", "prepare=fast"),
                     self.LINE.replace("window=main ", ""), memory_line(), ""):
            with self.subTest(line=line):
                self.assertIsNone(perf_font_diagnostics.parse_render_timing(line))


HARNESS_HASH = "ab" * 32
