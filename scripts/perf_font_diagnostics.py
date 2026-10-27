"""Perf font diagnostics."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import math
from pathlib import Path
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_memory


@dataclass(frozen=True)
class RenderTimingSample:
    """One `render_timing` line from a `--laps` run: its window and each lap in milliseconds."""

    unix_s: float
    window: str
    laps: dict[str, float]


def parse_render_timing(line: str) -> RenderTimingSample | None:
    """Parse the `line` field of a debug `render_timing` event; a malformed lap drops the line."""
    position = line.find(perf_memory.RENDER_TIMING_MARKER)
    unix_s = perf_memory.parse_utc_stamp(line)
    if position < 0 or unix_s is None:
        return None
    tokens = line[position + len(perf_memory.RENDER_TIMING_MARKER):].split()
    if not tokens or not tokens[0].startswith("window=") or len(tokens[0]) == len("window="):
        return None
    laps = {}
    for token in tokens[1:]:
        match = perf_memory._LAP.fullmatch(token)
        if not match:
            return None
        laps[match[1]] = float(match[2])
    return RenderTimingSample(unix_s, tokens[0][len("window="):], laps) if laps else None


def read_render_timing(log_dir: Path) -> list[RenderTimingSample]:
    """Return a `--laps` run's render_timing samples."""
    return [sample for sample in map(parse_render_timing, perf_memory._log_lines(log_dir)) if sample]


# The font crate's debug timing event: `render_timing: font operation operation="..." phase="..." ...`.
FONT_OPERATION_MARKER = " render_timing: font operation"
# The event-loop wait for a fallback face; only its returns are measured waits.
FALLBACK_RECEIVE = "fallback_receive"
# One `key=value` or `key="value"` field of an event.
_EVENT_FIELD = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)=("(?:[^"\\]|\\.)*"|\S+)')


@dataclass(frozen=True)
class FallbackWait:
    """One fallback_receive return: when it ended (the log stamp), how long it waited, and the stamp resolution."""

    end_unix_s: float
    elapsed_ms: float
    resolution_s: float


@dataclass
class FallbackLog:
    """A laps run's font operations, each classified once.

    `waits` are well-formed fallback_receive returns, `entries` well-formed fallback_receive enters, `other` the
    valid records of every other operation by name, and `unparsed` the malformed fallback_receive records.
    """

    waits: list[FallbackWait] = field(default_factory=list)
    entries: int = 0
    other: dict[str, int] = field(default_factory=dict)
    unparsed: int = 0
    unmatched_enter: int = 0
    unmatched_return: int = 0


def _event_fields(text: str) -> dict[str, str]:
    """An event's fields after its message, with quotes removed."""
    return {match[1]: match[2][1:-1] if match[2].startswith('"') else match[2]
            for match in _EVENT_FIELD.finditer(text)}


def _stamp_resolution_s(line: str) -> float:
    """The resolution of a line's UTC stamp: one unit of its last fraction digit, or one second without one."""
    match = perf_memory._STAMP.match(line)
    digits = len(match[7]) if match and match[7] else 0
    return 10.0 ** -digits


def _elapsed_ms(text: str | None) -> float | None:
    """A return's elapsed_ms when it is a finite number of at least zero, else None."""
    try:
        value = float(text) if text is not None else math.nan
    except ValueError:
        return None
    return value if math.isfinite(value) and value >= 0 else None


def parse_fallback_log(lines: Iterable[str]) -> FallbackLog:
    """Classify every `font operation` record of one run, in log order, and pair fallback_receive records.

    A fallback_receive enter closes at the next return; an enter followed by another enter, or by the log's end,
    is an unmatched enter, and a return with no open enter an unmatched return. Pairing never drops a wait.
    """
    log = FallbackLog()
    open_enter = False
    for line in lines:
        position = line.find(FONT_OPERATION_MARKER)
        if position < 0:
            continue
        fields = _event_fields(line[position + len(FONT_OPERATION_MARKER):])
        operation, phase = fields.get("operation"), fields.get("phase")
        if operation != FALLBACK_RECEIVE:
            if operation is not None and phase in ("enter", "return"):
                # When: another operation's record is well formed, it is counted by name, never unparsed.
                log.other[operation] = log.other.get(operation, 0) + 1
            continue
        stamp = perf_memory.parse_utc_stamp(line)
        elapsed = _elapsed_ms(fields.get("elapsed_ms"))
        if stamp is None or phase not in ("enter", "return") or (phase == "return" and elapsed is None):
            # When: the stamp, phase or a return's elapsed_ms is missing or malformed, the record is unparsed.
            log.unparsed += 1
            continue
        if phase == "enter":
            log.entries += 1
            log.unmatched_enter += open_enter
            open_enter = True
            continue
        log.waits.append(FallbackWait(stamp, elapsed, _stamp_resolution_s(line)))
        if open_enter:
            open_enter = False
        else:
            # When: no enter is open, this return pairs with nothing, but it is still a measured wait.
            log.unmatched_return += 1
    log.unmatched_enter += open_enter
    return log


def read_fallback_log(log_dir: Path) -> FallbackLog | None:
    """A laps run's font operations, its log files read in name order; None when the run left no logs directory."""
    if not log_dir.is_dir():
        return None
    return parse_fallback_log(perf_memory._log_lines(log_dir))


# The App's adapter messages and the event each names.
ADAPTER_EVENTS = {"wgpu adapter selected": "selected", "wgpu adapter reused": "reused"}
# The adapter fields in the order the App logs them; values hold spaces, so a line splits at these keys.
ADAPTER_KEYS = ("backend", "name", "driver", "device_type", "software_rendering")
# Logged after the identity fields; it ends the last value and is not kept.
ADAPTER_TRAILING_KEYS = ("device_memory_policy",)


def parse_adapter_line(line: str) -> dict[str, object] | None:
    """Read the App's `wgpu adapter selected` or `wgpu adapter reused` line, or None for any other line.

    Each value runs from its key to the next known key, so a name or driver with spaces stays whole.
    A line missing a key, or whose software_rendering is not true or false, is None.
    """
    for message, event in ADAPTER_EVENTS.items():
        index = line.find(message + " ")
        if index >= 0:
            break
    else:
        return None
    rest = line[index + len(message):]
    starts: list[tuple[str, int]] = []
    position = 0
    for key in ADAPTER_KEYS + ADAPTER_TRAILING_KEYS:
        found = rest.find(f" {key}=", position)
        if found < 0:
            if key in ADAPTER_TRAILING_KEYS:
                continue
            return None
        starts.append((key, found))
        position = found + len(key) + 2
    fields = {key: rest[start + len(key) + 2:end].strip()
              for (key, start), (_next, end) in zip(starts, starts[1:] + [("", len(rest))])}
    if fields["software_rendering"] not in ("true", "false"):
        return None
    return {"event": event, "backend": fields["backend"], "name": fields["name"], "driver": fields["driver"],
            "device_type": fields["device_type"], "software_rendering": fields["software_rendering"] == "true"}


def read_renderer(log_dir: Path) -> dict[str, object] | None:
    """A run's wgpu adapter: its first `selected` line, else its first `reused` one; None when none was logged."""
    adapters = [adapter for adapter in map(parse_adapter_line, perf_memory._log_lines(log_dir)) if adapter is not None]
    selected = [adapter for adapter in adapters if adapter["event"] == "selected"]
    return (selected or adapters or [None])[0]


def describe_renderer(renderer: Mapping | None) -> str:
    """Name an adapter as `<name> (<backend> <device type>, driver <driver>)`; none reads `unknown`."""
    if renderer is None:
        return "unknown"
    software = ", software rendering" if renderer.get("software_rendering") else ""
    return (f"{renderer.get('name')} ({renderer.get('backend')} {renderer.get('device_type')}, "
            f"driver {renderer.get('driver')}{software})")
