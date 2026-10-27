"""Perf report."""

from __future__ import annotations
import platform
import re
import sys
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_comparison_rows
import perf_sets
import perf_settings


# The perf-end completeness rows: each scenario variant's source name, as `InputSource` names it in
# `start_size_inputs.rs`, and its fixture. Helper and real-renderer rows come from test 17, never from here.
COMPLETENESS_SOURCES = {"S9/default": ("perf_s9_end", "S9"), "S12/default": ("perf_s12_end", "S12")}
COMPLETENESS_FILE = "completeness.json"
_HOST_PLATFORMS = {"darwin": "macos", "win32": "windows"}


def completeness_rows(results: Sequence[perf_sets.SetResult], shas: Mapping[str, str], environ: Mapping[str, str],
                      platform_name: str = sys.platform) -> list[dict]:
    """One row per S9/S12 run and side: its source, run id and attempt, the run's attempt directory, measured SHA,
    side, platform, fixture, set and scale, with the `end` checkpoint's reading. A run whose `end` has no reading (a harness that predates it) reads
    `unavailable` with the reason `not recorded`, never certified."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_result_schema
    platform = _HOST_PLATFORMS.get(platform_name, "linux" if platform_name.startswith("linux") else platform_name)
    rows = []
    for result in results:
        if result.label not in COMPLETENESS_SOURCES:
            # When: the set is not S9/default or S12/default, its `end` is not a perf-end census.
            continue
        source, fixture = COMPLETENESS_SOURCES[result.label]
        for side in perf_settings.SIDES:
            for outcome in getattr(result, side).outcomes:
                points = [point for point in (outcome.result or {}).get("checkpoints") or []
                          if isinstance(point, dict) and point.get("label") == "end"]
                reading = points[-1].get("completeness") if points else None
                reading = reading if isinstance(reading, dict) else {"state": "unavailable",
                                                                     "reason": "not recorded", "scale": None}
                rows.append({"source": source, "run_id": environ.get("GITHUB_RUN_ID") or "local",
                             "run_attempt": environ.get("GITHUB_RUN_ATTEMPT") or "local",
                             "attempt_dir": str(outcome.evidence), "measured_sha": shas[side], "side": side, "platform": platform,
                             "fixture": fixture, "set": result.set_name, "scale": reading.get("scale"),
                             "state": reading.get("state"), "reason": reading.get("reason"),
                             **{key: reading.get(key) for key in (*perf_result_schema._COMPLETENESS_COUNTS, *perf_result_schema._COMPLETENESS_DIMENSIONS)}})
    return rows


def completeness_lines(rows: Sequence[Mapping]) -> list[str]:
    """comparison.md's lines for the completeness rows: each row's state with its counts or its reason."""
    if not rows:
        return []
    lines = ["", "Glyph completeness at S9/S12 `end` (distinct missing characters, Full-frame certificate):", ""]
    for row in rows:
        where = f"- {row['fixture']} {row['set']} {row['side']} ({row['platform']}, scale {row['scale']})"
        if row["state"] == "certified":
            lines.append(f"{where}: certified, {row['missing_terminal']} terminal and {row['missing_chrome']} chrome "
                         f"missing, atlas {row['atlas_width']}x{row['atlas_height']}")
        else:
            lines.append(f"{where}: unavailable ({row['reason']})")
    return lines


def comparison_document(rows: Sequence[Sequence[str]], lap_rows: Sequence[Sequence[str]],
                        alloc_rows: Sequence[Sequence[str]], host_lines: Sequence[str],
                        detail_lines: Sequence[str], *, counter_rows: Sequence[Sequence[str]] = (),
                        counters_note: str = "", overhead_rows: Sequence[Sequence[str]] = (),
                        capped_note: str = "", recovery_rows: Sequence[Sequence[str]] = (),
                        flag_checks: Sequence[perf_comparison_rows.FlagCheck] | None = None,
                        unclassified_rows: Sequence[Sequence[str]] = (),
                        counters_only_rows: Sequence[Sequence[str]] = (),
                        attribution_rows: Sequence[Sequence[str]] = ()) -> str:
    """Assemble comparison.md: the PR table, the laps, counters, overhead and allocation tables when run,
    the host block and details.

    The counters section appears when the counters set ran or was skipped; `counters_note` says which.
    `capped_note` names the variants whose short-mode runs were capped, under the PR table.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_attribution_report
    import perf_comparison_rows
    import perf_counter_rows
    import perf_fallback_rows
    import perf_host_info
    import perf_inventory
    parts = ["## Performance comparison\n\n" + perf_inventory.render_table(rows)
             + (f"\n{capped_note}\n" if capped_note else "")]
    if counters_only_rows:
        parts.append("### Counters-only workloads (`--counters` runs, never pooled with timed runs)\n\n"
                     f"{perf_comparison_rows.COUNTERS_ONLY_NOTE}\n\n" + perf_inventory.render_table(counters_only_rows))
    if unclassified_rows:
        parts.append(f"### Unclassified (older harness)\n\n{perf_comparison_rows.UNCLASSIFIED_NOTE}\n\n" + perf_inventory.render_table(unclassified_rows))
    if flag_checks:
        # When: a phase-kinds harness was checked, the section lists its flags or says none was raised.
        flagged = perf_comparison_rows.flag_rows(flag_checks)
        parts.append(f"### Candidate flags\n\n{perf_comparison_rows.FLAG_NOTE}\n\n"
                     + (perf_inventory.render_table(flagged, perf_comparison_rows.FLAG_HEADER) if flagged
                        else f"None of the {len(flag_checks)} checks is flagged.\n"))
    if lap_rows:
        parts.append("### Laps (`--laps` runs, never pooled with timed runs)\n\n" + perf_inventory.render_table(lap_rows))
    if counter_rows or counters_note:
        section = "### Frame counters (`--counters` runs on the head, never pooled with timed or laps runs)\n\n"
        if counters_note:
            section += counters_note + "\n\n"
        parts.append(section + (perf_inventory.render_table(counter_rows, perf_fallback_rows.COUNTERS_HEADER) if counter_rows else ""))
    if recovery_rows:
        parts.append(f"### Atlas retry recovery (S1/atlas-retry counters runs)\n\n{perf_host_info.RECOVERY_NOTE}\n\n"
                     + perf_inventory.render_table(recovery_rows, perf_host_info.RECOVERY_HEADER))
    if attribution_rows:
        parts.append(f"### S10 attribution (counters runs)\n\n{perf_attribution_report.ATTRIBUTION_NOTE}\n\n"
                     + perf_inventory.render_table(attribution_rows, perf_attribution_report.ATTRIBUTION_HEADER))
    if overhead_rows:
        parts.append(f"### Counters overhead (S2 and S3)\n\n{perf_counter_rows.OVERHEAD_NOTE}.\n\n"
                     + perf_inventory.render_table(overhead_rows, perf_counter_rows.OVERHEAD_HEADER))
    if alloc_rows:
        parts.append("### Allocations per frame (`--alloc` runs, never pooled with timed runs)\n\n"
                     + perf_inventory.render_table(alloc_rows))
    parts.append("### Host\n\n" + "\n".join(host_lines) + "\n")
    parts.append("<details><summary>SHAs, harness hash, commands and raw logs</summary>\n\n"
                 + "\n".join(detail_lines) + "\n\n</details>\n")
    return "\n".join(parts)



# --- Comparison driver and command line -----------------------------------------------------

EXIT_USAGE = 2
_SHA = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")


# The sys.platform values the harness measures on.
RUN_PLATFORMS = ("darwin", "win32")
