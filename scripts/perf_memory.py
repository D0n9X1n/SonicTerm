"""Perf memory."""

from __future__ import annotations
import calendar
from collections import Counter
from dataclasses import asdict, dataclass, field
from pathlib import Path
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_attribution_schema



# --- Log lines and result.json --------------------------------------------------------------

# The file layer is tracing_subscriber's default fmt layer without ANSI: an RFC 3339 UTC stamp,
# the level, any span context, the target, the message and the fields.
_STAMP = re.compile(r"(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?Z")
# Other events share the `memory` target, so the snapshot is selected by its message.
MEMORY_MARKER = " memory: memory snapshot "
RENDER_TIMING_MARKER = " render_timing: line=[render_timing] "
_LAP = re.compile(r"([A-Za-z0-9_]+)=([0-9]+(?:\.[0-9]+)?)ms")


def parse_utc_stamp(line: str) -> float | None:
    """Return the Unix time of the fmt layer's leading UTC stamp, or None."""
    match = _STAMP.match(line)
    if not match:
        return None
    whole = calendar.timegm(tuple(int(match[index]) for index in range(1, 7)) + (0, 0, 0))
    fraction = match[7] or "0"
    return whole + int(fraction) / 10 ** len(fraction)


def _field(text: str, name: str) -> str | None:
    match = re.search(r"(?:^|\s)" + re.escape(name) + r"=(\S*)", text)
    return None if match is None else match[1]


@dataclass(frozen=True)
class MemorySample:
    """One `memory snapshot` line; a process figure the host does not report is None."""

    unix_s: float
    process_resident_bytes: int | None
    renderer_total_bytes: int
    session_total_bytes: int
    # A checkpoint sample's identity and completeness; all None on a periodic sample.
    checkpoint_index: int | None = None
    checkpoint_label: str | None = None
    checkpoint_attempt: int | None = None
    checkpoint_complete: bool | None = None
    panes_total: int | None = None
    panes_sampled: int | None = None
    panes_contended: int | None = None
    grid_visible_bytes: int | None = None
    grid_history_bytes: int | None = None
    grid_alternate_bytes: int | None = None
    # Each renderer's glyph atlas facts, in breakdown order; empty on a line from an older build.
    glyph_atlases: tuple = ()
    # Row glyph cache storage summed over every renderer; None on a line from a build that lacks the field.
    renderer_row_glyph_cache_bytes: int | None = None
    # A checkpoint line's trim tags, written only by a build with the trim hook; None when absent.
    trimmed: bool | None = None
    trim_source: str | None = None
    trim_seq: int | None = None
    # Trim tags present on the line whose value did not parse, by name; a malformed tag is never read as absent.
    malformed_trim_tags: tuple = ()

    def totals(self) -> tuple:
        """The figures a checkpoint reading compares: two samples with equal totals are the same reading."""
        return (self.process_resident_bytes, self.renderer_total_bytes, self.session_total_bytes,
                self.panes_total, self.panes_sampled, self.panes_contended, self.grid_visible_bytes,
                self.grid_history_bytes, self.grid_alternate_bytes, self.renderer_row_glyph_cache_bytes)

    def grid_bytes_per_pane(self) -> float | None:
        """Visible, history and alternate grid bytes divided by the panes sampled, or None when a
        field is missing or no pane was sampled."""
        parts = (self.grid_visible_bytes, self.grid_history_bytes, self.grid_alternate_bytes)
        if any(part is None for part in parts) or not self.panes_sampled:
            return None
        return sum(parts) / self.panes_sampled


def _optional_count(fields: str, name: str) -> int | None:
    """An optional integer field: its value when present and all digits, otherwise None."""
    value = _field(fields, name)
    return int(value) if value and value.isdigit() else None


def _optional_text(fields: str, name: str) -> str | None:
    """An optional text field, without the quotes the log layer puts around a string value."""
    value = _field(fields, name)
    if value is None:
        return None
    return value[1:-1] if len(value) >= 2 and value[0] == value[-1] == '"' else value


# One renderer's `role[label]` identity and its six glyph atlas facts inside the quoted renderer breakdown.
# The identity opens each renderer's entry; the lazy gap stops at that renderer's own facts.
_GLYPH_ATLAS_FACTS = re.compile(
    r"([A-Za-z_][\w-]*\[[^\]]*\])[^;]*? "
    r"glyph_atlas_dim=(\d+) glyph_atlas_packed_pixels=(\d+) glyph_atlas_growths=(\d+) "
    r"glyph_atlas_evictions=(\d+) glyph_atlas_fit=(256|512|1024|2048|no_headroom|does_not_fit|evicted) "
    r"glyph_atlas_max_tile=(\d+)x(\d+)")


@dataclass(frozen=True)
class GlyphAtlasFacts:
    """One renderer's glyph atlas: its `role[label]` identity, dimension, packed area, growths, evictions,
    fit and largest tile."""

    renderer: str
    dim: int
    packed_pixels: int
    growths: int
    evictions: int
    fit: str
    max_tile: tuple


def parse_glyph_atlases(fields: str) -> tuple:
    """Every renderer's glyph atlas facts on a memory line, in order; empty when none are reported."""
    return tuple(
        GlyphAtlasFacts(renderer, int(dim), int(packed), int(growths), int(evictions), fit,
                        (int(width), int(height)))
        for renderer, dim, packed, growths, evictions, fit, width, height in _GLYPH_ATLAS_FACTS.findall(fields))


def parse_memory_line(line: str) -> MemorySample | None:
    """Parse one `memory snapshot` line; any other line, or one with a malformed total, is None."""
    position = line.find(MEMORY_MARKER)
    unix_s = parse_utc_stamp(line)
    if position < 0 or unix_s is None:
        return None
    fields = line[position + len(MEMORY_MARKER):]
    renderer, session = _field(fields, "renderer_total_bytes"), _field(fields, "session_total_bytes")
    if not (renderer and renderer.isdigit() and session and session.isdigit()):
        return None
    resident = _field(fields, "process_resident_bytes")
    if resident is not None and resident != "unsupported" and not resident.isdigit():
        return None
    resident_bytes = int(resident) if resident and resident.isdigit() else None
    complete = _field(fields, "checkpoint_complete")
    trimmed = _field(fields, "trimmed")
    raw_source, raw_seq = _field(fields, "trim_source"), _field(fields, "trim_seq")
    malformed = tuple(name for name, present, valid in (
        ("trimmed", trimmed is not None, trimmed in ("true", "false")),
        ("trim_source", raw_source is not None, bool(_optional_text(fields, "trim_source"))),
        ("trim_seq", raw_seq is not None, bool(raw_seq) and raw_seq.isdigit()),
    ) if present and not valid)
    return MemorySample(
        unix_s, resident_bytes, int(renderer), int(session),
        checkpoint_index=_optional_count(fields, "checkpoint_index"),
        checkpoint_label=_optional_text(fields, "checkpoint_label"),
        checkpoint_attempt=_optional_count(fields, "checkpoint_attempt"),
        checkpoint_complete={"true": True, "false": False}.get(complete) if complete else None,
        panes_total=_optional_count(fields, "panes_total"),
        panes_sampled=_optional_count(fields, "panes_sampled"),
        panes_contended=_optional_count(fields, "panes_contended"),
        grid_visible_bytes=_optional_count(fields, "grid_visible_bytes"),
        grid_history_bytes=_optional_count(fields, "grid_history_bytes"),
        grid_alternate_bytes=_optional_count(fields, "grid_alternate_bytes"),
        glyph_atlases=parse_glyph_atlases(fields),
        renderer_row_glyph_cache_bytes=_optional_count(fields, "renderer_row_glyph_cache_bytes"),
        trimmed={"true": True, "false": False}.get(trimmed) if trimmed else None,
        trim_source=_optional_text(fields, "trim_source"),
        trim_seq=_optional_count(fields, "trim_seq"),
        malformed_trim_tags=malformed)


@dataclass(frozen=True)
class CheckpointReading:
    """A checkpoint's authoritative memory sample, or why it has none it can report."""

    sample: MemorySample | None
    # Whether no attempt measured every pane; the sample is then the last partial attempt.
    partial: bool = False
    # Why the checkpoint has no reading, such as `conflicting samples`; None when it has one.
    problem: str | None = None


def checkpoint_memory(samples: Sequence[MemorySample], index: int) -> CheckpointReading | None:
    """Return checkpoint `index`'s authoritative sample, or None when no sample is tagged with it.

    Every complete attempt is checked first: two complete samples of one attempt with different
    totals make the whole index conflicting, whichever attempt would have been chosen, so a later
    attempt cannot hide an earlier conflict. Then the complete sample with the highest attempt wins;
    with none complete, the partial one with the highest attempt, marked `partial`, and two partial
    samples of that attempt with different totals are conflicting too. A line repeated with the same
    totals counts once. An untagged periodic sample is never returned.
    """
    tagged = [sample for sample in samples if sample.checkpoint_index == index
              and sample.checkpoint_attempt is not None and sample.checkpoint_complete is not None]
    if not tagged:
        return None
    complete = [sample for sample in tagged if sample.checkpoint_complete]
    complete_totals: dict[int, set[tuple]] = {}
    for sample in complete:
        complete_totals.setdefault(sample.checkpoint_attempt, set()).add(sample.totals())
    if any(len(totals) > 1 for totals in complete_totals.values()):
        return CheckpointReading(None, problem="conflicting samples")
    pool = complete or tagged
    attempt = max(sample.checkpoint_attempt for sample in pool)
    chosen = [sample for sample in pool if sample.checkpoint_attempt == attempt]
    if len({sample.totals() for sample in chosen}) > 1:
        return CheckpointReading(None, problem="conflicting samples")
    return CheckpointReading(chosen[0], partial=not complete)


def _log_lines(log_dir: Path) -> Iterable[str]:
    """Yield every line of the regular files directly under a run's `logs/` directory."""
    if not log_dir.is_dir():
        return
    for path in sorted(log_dir.iterdir()):
        if path.is_file() and not path.is_symlink():
            yield from path.read_text(encoding="utf-8", errors="replace").splitlines()


def read_memory_samples(log_dir: Path) -> list[MemorySample]:
    """Return a run's memory samples in time order; a short run may have none."""
    samples = [sample for sample in map(parse_memory_line, _log_lines(log_dir)) if sample]
    return sorted(samples, key=lambda sample: sample.unix_s)


# The App's per-present attribution line: WARN on `sonic::perf_present`, fields after the message.
PRESENT_MARKER = " sonic::perf_present: perf_present "


@dataclass(frozen=True)
class PresentLine:
    """One `sonic::perf_present` line; a line with the marker whose fields do not parse keeps its `problem`."""

    kind: str
    arming: int = 0
    seq: int = 0
    pane: int = 0
    resets: int = 0
    epoch: int = 0
    set: bool = False
    sentinel: bool = False
    prompt: bool = False
    admission: str = ""
    emitted: int = 0
    problem: str | None = None


# Each kind's integer, boolean and text fields, in the order the App writes them; every line names the main window.
_PRESENT_FIELDS = {
    "present": (("arming", "seq", "pane", "resets", "epoch"), ("set", "sentinel", "prompt"), ("admission",)),
    "overflow": (("arming", "seq", "pane", "emitted"), (), ()),
}
PRESENT_WINDOW = "main"
_FIELD_NAME = re.compile(r"(?:^|\s)([A-Za-z_][A-Za-z0-9_]*)=")
PRESENT_ADMISSIONS = ("closed", "forced-first", "forced-visibility", "forced-device", "forced-resize",
                      "forced-surface", "timeout", "credit")


def _unquoted(fields: str, name: str) -> str | None:
    """`name`'s value with the formatter's quotes removed; None when absent."""
    value = _field(fields, name)
    return value[1:-1] if value and len(value) >= 2 and value[0] == value[-1] == '"' else value


def parse_present_line(line: str) -> PresentLine | None:
    """Parse one `sonic::perf_present` line; None for any other line. A line with the marker but a missing or
    malformed field is kept with its problem, so the run reads incomplete rather than short."""
    position = line.find(PRESENT_MARKER)
    if position < 0:
        return None
    fields = line[position + len(PRESENT_MARKER):]
    # A field written twice, equal or not, makes the line ambiguous, since `_field` would read only the first.
    repeated = sorted(name for name, count in Counter(_FIELD_NAME.findall(fields)).items() if count > 1)
    kind = _unquoted(fields, "kind")
    if repeated:
        return PresentLine(kind=str(kind), problem=f"field(s) {', '.join(repeated)} written more than once")
    if kind not in _PRESENT_FIELDS:
        return PresentLine(kind=str(kind), problem=f"unknown kind {kind!r}")
    window = _unquoted(fields, "window")
    if window != PRESENT_WINDOW:
        return PresentLine(kind=kind, problem=f"window is {window!r}, not {PRESENT_WINDOW!r}")
    counts, flags, texts = _PRESENT_FIELDS[kind]
    values: dict[str, object] = {}
    for name in counts:
        raw = _field(fields, name)
        if raw is None or not raw.isdigit() or int(raw) > perf_attribution_schema.U64_MAX:
            return PresentLine(kind=kind, problem=f"{name} is {raw!r}")
        values[name] = int(raw)
    for name in flags:
        raw = _field(fields, name)
        if raw not in ("true", "false"):
            return PresentLine(kind=kind, problem=f"{name} is {raw!r}")
        values[name] = raw == "true"
    for name in texts:
        raw = _unquoted(fields, name)
        if raw not in PRESENT_ADMISSIONS:
            return PresentLine(kind=kind, problem=f"{name} is {raw!r}")
        values[name] = raw
    return PresentLine(kind=kind, **values)


def read_present_lines(log_dir: Path) -> list[PresentLine]:
    """Every attribution line of a run's logs, in the order written."""
    return [line for line in map(parse_present_line, _log_lines(log_dir)) if line is not None]
