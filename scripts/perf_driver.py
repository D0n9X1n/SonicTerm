"""Perf driver."""

from __future__ import annotations
import argparse
import os
from pathlib import Path
import sys
import time
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_atlas_rows
import perf_attribution_report
import perf_build_policy
import perf_comparison_rows
import perf_counter_rows
import perf_delivery
import perf_execution
import perf_fallback_rows
import perf_focus
import perf_harness_sources
import perf_host_info
import perf_inventory
import perf_metrics
import perf_outcomes
import perf_prebuilt
import perf_report
import perf_result_schema
import perf_scenarios
import perf_sets
import perf_settings
import perf_smoke
import perf_worktrees


def builds_line(args: argparse.Namespace, work: Path) -> str:
    """The details line saying where the binaries came from: this job's builds or the producer's."""
    command_text = " ".join(perf_scenarios.build_argv(perf_settings.HARNESS_EXAMPLE, release=True))
    if args.prebuilt is not None:
        return (f"- Builds: prebuilt by run {args.prebuilt_run_id} attempt {args.prebuilt_attempt}, manifest "
                f"sha256 `{args.prebuilt_manifest_sha256}`: `{command_text}` in each worktree, copied to "
                f"`{work / perf_build_policy.PREBUILT_DIRECTORY}`")
    return f"- Builds: `{command_text}` in each worktree, one CARGO_TARGET_DIR per ref"


def write_timing(out: Path, marks: Mapping[str, float], environ: Mapping[str, str]) -> None:
    """Write timing.json: this job's run, attempt, job name, shard and the comparison's marks in unix seconds.

    perf-critical-path.py splits a comparison step's time into prepare, scenarios and report with it.
    """
    perf_outcomes._write_json(out / perf_build_policy.TIMING_FILE, {
        "schema_version": perf_settings.SCHEMA_VERSION, "run_id": environ.get("GITHUB_RUN_ID", ""),
        "run_attempt": environ.get("GITHUB_RUN_ATTEMPT", ""), "job": environ.get("PERF_JOB_NAME", ""),
        "shard": environ.get("PERF_SHARD", ""), "marks": {name: marks[name] for name in perf_build_policy.TIMING_MARKS}})


def prepare_trees(args: argparse.Namespace, worktrees: perf_worktrees.Worktrees,
                  host_run: Callable) -> tuple[dict[str, str], dict[str, Path], str]:
    """Resolve both refs, add their worktrees and overlay the head's harness; return SHAs, trees and harness hash."""
    shas = {side: perf_build_policy._resolve_sha(host_run, ref) for side, ref in (("base", args.base), ("head", args.head))}
    trees = {"head": worktrees.create("head", shas["head"])}
    trees["base"] = worktrees.create("base", shas["base"])
    perf_harness_sources.overlay_harness(trees["head"], trees["base"])
    hashes = {side: perf_harness_sources.tree_harness_hash(trees[side]) for side in perf_settings.SIDES}
    if hashes["base"] != hashes["head"]:
        raise ValueError(f"harness hashes differ after the overlay: base {hashes['base']}, head {hashes['head']}")
    print(f"[perf-compare] base={shas['base']} head={shas['head']} harness_hash={hashes['head']}", flush=True)
    return shas, trees, hashes["head"]


def build_sides(args: argparse.Namespace, gate, trees: Mapping[str, Path], features: Mapping[str, Sequence[str]],
                examples: Sequence[str], out: Path, work: Path,
                harness_cfgs: Sequence[str] = ()) -> tuple[dict[str, dict[str, object]], int]:
    """Build every example of both refs through the gate's reviewed steps; return the builds and the last log index.

    A build maps an example to its binary, or, for a lenient base, to the reason it cannot run.
    """
    builds: dict[str, dict[str, object]] = {side: {} for side in perf_settings.SIDES}
    index = 0
    # One build at a time, each with its ref's own target directory; the head goes first, so a head
    # that cannot build fails before the base's build time is spent.
    for side in ("head", "base"):
        environ = dict(os.environ, CARGO_TARGET_DIR=str(work / f"target-{side}"))
        for example in examples:
            index += 1
            # A tree builds with exactly the perf features it declares and the comparison's harness cfgs; each
            # build is the gate's own reviewed step, whose flags the gate composes at launch.
            catalog = gate.PERF_CFG_BUILDS[(tuple(harness_cfgs), tuple(features[side]))]
            step = catalog[f"build-{side}-{example}"]
            result = gate.run_step(step, index, trees[side], out, environ)
            text = perf_scenarios.read_log(result.log_path)
            binary = perf_scenarios.artifact_executable(text, example)
            built = perf_scenarios.build_passed(result)
            problem = perf_scenarios.asset_problem(binary, trees[side]) if built and binary else None
            # A strict comparison treats the base like the head: no build or asset problem is tolerated.
            strict = side == "head" or args.require_base
            if built and binary is not None and problem is None:
                builds[side][example] = binary
            elif problem is not None and strict:
                raise ValueError(f"the {side} cannot run {example}: {problem}")
            elif problem is not None:
                builds[side][example] = f"base cannot run {example}: {problem}"
            elif strict:
                raise ValueError(f"the {side} cannot build {example}: {perf_worktrees.build_error(text)} (log {result.log_path})")
            else:
                builds[side][example] = f"base cannot build {example}: {perf_worktrees.build_error(text)}"
    return builds, index


def _list_side(gate, binary: Path, out: Path, index: int, side: str, prebuilt: bool) -> list[perf_scenarios.Scenario]:
    """List one side's scenarios; a prebuilt binary that cannot list refuses the artifact."""
    try:
        return perf_smoke.list_scenarios(gate, binary, out, index)
    except ValueError as error:
        # When: the binary came from the producer, its failure to start is the artifact's refusal.
        if prebuilt:
            raise ValueError(f"refusing the prebuilt binaries: the {side} binary failed: {error}") from error
        raise


def _compare(args: argparse.Namespace, gate, out: Path, work: Path, worktrees: perf_worktrees.Worktrees,
             host_run: Callable) -> int:
    """Build (or load) both refs with the head's harness, run every selected set and write comparison.md.

    With `--build-only` it publishes the binaries and their manifest instead of measuring; with
    `--prebuilt` it measures binaries the producer job built. compare_main owns the worktrees' removal,
    which runs whatever this raises.
    """
    marks = {"start": time.time()}
    runs = args.runs or perf_settings.DEFAULT_RUNS
    shas, trees, digest = prepare_trees(args, worktrees, host_run)
    examples = (perf_settings.HARNESS_EXAMPLE,) + ((perf_settings.ALLOC_EXAMPLE,) if args.alloc else ())
    # A tree builds every harness with the perf features it declares, and only with those.
    features = {side: perf_harness_sources.tree_features(trees[side]) for side in perf_settings.SIDES}
    supports = {side: perf_harness_sources.COUNTERS_FEATURE in features[side] for side in perf_settings.SIDES}
    # One harness cfg decision for both sides, and the flags it composes; an inherited flag against it fails here.
    harness_cfgs, tree_cfgs = perf_harness_sources.harness_cfg_decision(trees, gate.HARNESS_API_CFGS)
    rustflags = gate.compose_harness_rustflags(os.environ, harness_cfgs)
    print(f"[perf-compare] harness cfgs (both sides): {', '.join(harness_cfgs) or 'none'}", flush=True)
    prebuilt = args.prebuilt is not None
    identity = None
    if prebuilt or args.build_only is not None:
        identity = perf_prebuilt.build_identity(trees, features, host_run, os.environ, harness_cfgs, rustflags)
    index = 0
    if prebuilt:
        builds = perf_prebuilt.load_prebuilt(args, shas, digest, identity, examples, work / perf_build_policy.PREBUILT_DIRECTORY)
        # The copies run from each ref's own tree, which must hold that ref's assets.
        for side in ("head", "base"):
            for example in examples:
                problem = perf_scenarios.asset_problem(builds[side][example], trees[side])
                if problem is not None:
                    raise ValueError(f"refusing the prebuilt binaries: the {side} cannot run {example}: {problem}")
    else:
        builds, index = build_sides(args, gate, trees, features, examples, out, work, harness_cfgs)
    index += 1
    scenarios = _list_side(gate, builds["head"][perf_settings.HARNESS_EXAMPLE], out, index, "head", prebuilt)
    if args.require_base or prebuilt or args.build_only is not None:
        # The base must load and list too, so a binary that cannot start fails before anything is measured.
        index += 1
        _list_side(gate, builds["base"][perf_settings.HARNESS_EXAMPLE], out, index, "base", prebuilt)
    if args.build_only is not None:
        return perf_prebuilt.publish_binaries(args.build_only, builds, shas, digest, identity, os.environ)
    marks["listed"] = time.time()
    by_id = {scenario.id: scenario for scenario in scenarios}
    selected = perf_scenarios.select_scenarios(args.scenario or ["all"], scenarios)
    host = perf_smoke.production_host(gate)
    # Only these variants run the laps set; every selected one with --laps, none without a laps selection.
    laps_selection = (perf_scenarios.select_laps_scenarios(args.laps_scenario, scenarios, selected) if args.laps_scenario
                      else selected if args.laps else [])
    # Each set is (name, example, laps, counters, valid runs per side).
    sets = [("timed", perf_settings.HARNESS_EXAMPLE, False, False, runs)]
    if laps_selection:
        sets.append(("laps", perf_settings.HARNESS_EXAMPLE, True, False, args.laps_runs or runs))
    counters_note = ""
    if args.counters and supports["head"]:
        sets.append(("counters", perf_settings.HARNESS_EXAMPLE, False, True, args.counters_runs or runs))
    elif args.counters:
        counters_note = perf_counter_rows.COUNTERS_UNSUPPORTED
        print(f"[perf-compare] {perf_counter_rows.COUNTERS_UNSUPPORTED}", flush=True)
    if args.alloc:
        sets.append(("alloc", perf_settings.ALLOC_EXAMPLE, False, False, runs))
    # Checked once the sets are known: --counters on a head without perf-counters runs no counters set either.
    refusal = perf_result_schema.counters_only_problem(selected, any(set_name == "counters" for set_name, *_ in sets))
    if refusal is not None:
        raise ValueError(refusal)
    results = []
    # A diagnostic whose harness cfg is off is unavailable on both sides; it is excluded from the measured sets.
    unavailable = {}
    for (scenario_id, variant), cfg_name in perf_result_schema.HARNESS_CFG_VARIANTS.items():
        if (scenario_id, variant) in selected and cfg_name not in harness_cfgs:
            lacking = [side for side in perf_settings.SIDES if cfg_name not in tree_cfgs[side]]
            unavailable[f"{scenario_id}/{variant}"] = (
                f"unavailable on both sides: {cfg_name} is off because the {' and '.join(lacking)} "
                f"tree{'s lack' if len(lacking) > 1 else ' lacks'} its App and renderer methods")
    marks["measure_start"] = time.time()
    # One untimed replay per delivered scenario, before any measured run; both sides share the overlaid harness.
    deliveries: dict[str, DeliveryOutcome] = {}
    replay_evidence = out / "delivery"
    for scenario_id, variant in selected:
        if f"{scenario_id}/{variant}" in unavailable:
            # When: the diagnostic is unavailable, it is not measured, so its delivery is not replayed either.
            continue
        if perf_delivery.delivery_replayed(scenario_id, sys.platform):
            replay_evidence.mkdir(parents=True, exist_ok=True)
            index += 1
            deliveries[f"{scenario_id}/{variant}"] = perf_delivery.run_delivery_replay(
                gate, builds["head"][perf_settings.HARNESS_EXAMPLE], scenario_id, variant, replay_evidence, index + 1,
                short=args.short, timeout_s=perf_scenarios.run_timeout_s(by_id[scenario_id], False, args.short),
                temp_root=host.temp_root, environ=host.environ)
    # One reference for every set, so all valid runs of the comparison share one refresh rate and scale.
    display = perf_sets.DisplayReference()
    for scenario_id, variant in selected:
        label = f"{scenario_id}/{variant}"
        if label in unavailable:
            # When: the label's harness cfg is off, each of its sets is recorded unavailable and none runs.
            print(f"[perf-compare] {label} {unavailable[label]}", flush=True)
            results.extend(perf_sets.SetResult(label, set_name, perf_metrics.SideRuns(blocked=unavailable[label]),
                                     perf_metrics.SideRuns(blocked=unavailable[label]), unavailable=unavailable[label])
                           for set_name, *_ in perf_result_schema.variant_sets(scenario_id, variant, sets))
            continue
        delivery_problem = deliveries.get(label, (None, None))[1]
        if delivery_problem is not None:
            # When: the replay could not show how ConPTY delivered the scenario, no set of it is measured.
            results.extend(perf_comparison_rows.blocked_set_results(label, delivery_problem, [
                set_name for set_name, *_ in perf_result_schema.variant_sets(scenario_id, variant, sets)
                if set_name != "laps" or (scenario_id, variant) in laps_selection]))
            continue
        for set_name, example, laps, counters, requested_runs in perf_result_schema.variant_sets(scenario_id, variant, sets):
            if laps and (scenario_id, variant) not in laps_selection:
                # When: --laps-scenario does not name this variant, it runs no laps set.
                continue
            # A capped variant takes min(requested, cap) runs in every set under --short.
            set_runs = perf_scenarios.capped_runs(by_id[scenario_id], variant, requested_runs, args.short)
            built = {side: builds[side][example] for side in perf_settings.SIDES}
            # A base that did not build is retired before its first slot, so its placeholder path never runs.
            # Both sides run the head's overlaid harness (equal hashes), so the head's list decides the split schema.
            plans = {side: perf_outcomes.RunPlan(by_id[scenario_id], variant, side,
                                   built[side] if isinstance(built[side], Path) else Path("unbuilt"),
                                   digest, short=args.short, laps=laps, counters=counters, source_root=trees[side],
                                   latency_split_schema=by_id[scenario_id].latency_split_schema,
                                   phase_kinds=by_id[scenario_id].phase_kinds,
                                   attribution_api=perf_attribution_report.ATTRIBUTION_CFG in harness_cfgs,
                                   attribution_schema=by_id[scenario_id].attribution_schema,
                                   echo_timeline_schema=by_id[scenario_id].echo_timeline_schema,
                                   guard_correlation_schema=by_id[scenario_id].guard_correlation_schema,
                                   guard_api=perf_attribution_report.GUARD_SPANS_CFG in harness_cfgs)
                     for side in perf_settings.SIDES}
            base_blocked = built["base"] if isinstance(built["base"], str) else None
            if counters and not supports["base"]:
                # A base without the feature has no counters, so the set runs on the head only, base n/a.
                base_blocked = perf_counter_rows.COUNTERS_HEAD_ONLY
            result = perf_sets.run_set(label, plans, base_blocked, set_runs,
                             lambda plan, evidence: perf_execution.execute_run(plan, host, evidence),
                             out / "runs" / f"{scenario_id}-{variant}" / set_name, set_name, display=display)
            result.requested_runs = requested_runs
            results.append(result)
    marks["measure_end"] = time.time()
    timed_rows, lap_rows, alloc_rows, counters_table, overhead = [], [], [], [], []
    unclassified_rows: list[list[str]] = []
    counters_only_rows: list[list[str]] = []
    recovery_rows: list[list[str]] = []
    split_details: list[str] = []
    timed_heads: dict[str, SideRuns] = {}
    timed_results: dict[str, SetResult] = {}
    attribution_rows: list[list[str]] = []
    attribution_evidence: dict[str, dict] = {}
    omitted = 0
    presenter_notes: list[str] = []
    for result in results:
        # Lookups stay keyed by the plain label; only the rows carry the cap.
        shown = perf_sets.capped_label(result)
        if result.unavailable is not None:
            timed_rows.append([shown, f"{result.set_name} set", "unavailable", "unavailable", result.unavailable])
            continue
        if result.set_name == "timed":
            # An older harness's rows cannot be read by kind, so they leave the main table for their own block.
            destination = unclassified_rows if perf_comparison_rows.unclassified_result(result) else timed_rows
            destination.extend(perf_comparison_rows.comparison_rows(shown, result.base, result.head))
            timed_heads[result.label] = result.head
            timed_results[result.label] = result
            if result.label in deliveries:
                destination.extend(perf_delivery.delivery_rows(result.label, *deliveries[result.label]))
        elif result.set_name == "laps":
            lap_rows.extend(perf_atlas_rows.laps_rows(shown, result.base, result.head))
            lap_rows.extend(perf_fallback_rows.fallback_rows(shown, result.base, result.head))
        elif result.set_name == "counters":
            if tuple(result.label.split("/", 1)) in perf_result_schema.COUNTERS_ONLY_VARIANTS:
                # When: the variant runs only in the counters set, its kind-aware rows come from these runs, in
                # their own section; an older harness's go to the unclassified block, labelled with the dataset.
                if perf_comparison_rows.unclassified_result(result):
                    unclassified_rows.extend(perf_comparison_rows.comparison_rows(f"{shown} [counters]", result.base, result.head))
                else:
                    counters_only_rows.extend(perf_comparison_rows.comparison_rows(shown, result.base, result.head))
            rows, left_out = perf_counter_rows.counter_rows(shown, result.base, result.head)
            counters_table.extend(rows)
            recovery_rows.extend(perf_attribution_report.atlas_recovery_rows(result.label, result.base, result.head))
            counters_table.extend(perf_atlas_rows.glyph_atlas_reconciliation_rows(shown, result.base, result.head))
            split_details.extend(perf_counter_rows.attempt_split_details(shown, result.base, result.head))
            if result.label.startswith("S10/"):
                rows, evidence = perf_attribution_report.attribution_report(result.label, result, timed_results.get(result.label),
                                                    tuple(features["base"]) == tuple(features["head"]))
                attribution_rows.extend(rows)
                attribution_evidence[result.label] = evidence
            counters_table.extend(perf_counter_rows.split_rows(result.label, result.base, result.head,
                                             by_id[result.label.split("/")[0]].latency_split_schema))
            counters_table.extend(perf_counter_rows.timeline_rows(result.label, result.base, result.head))
            omitted += left_out
            for side_name, side in (("base", result.base), ("head", result.head)):
                presenter_notes.extend(perf_counter_rows.presenter_counter_notes(result.label, side_name, side))
            # The timed set always comes first, so its head runs are the counters-off side.
            if perf_counter_rows.overhead_applies(result.label) and result.label in timed_heads:
                overhead.extend(perf_comparison_rows.comparison_rows(result.label, timed_heads[result.label], result.head))
        else:
            alloc_rows.extend(perf_comparison_rows.comparison_rows(shown, result.base, result.head, perf_build_policy._allocation_metric))
    capped = [f"{result.label} {result.set_name} {result.target_runs} of {result.requested_runs} runs"
              for result in results if 0 < result.target_runs < result.requested_runs]
    capped_note = f"Capped variants (--short): {'; '.join(capped)}." if capped else ""
    if counters_table:
        counters_note = ("Counts are the median of each phase's per-run delta (range in brackets); a histogram's "
                         "p95 and max are bucket bounds over every run's events, its mean is sum_us/count in its "
                         "unit. A change compares a count's medians or a histogram's means. The baseline reads n/a "
                         "when the base does not declare perf-counters, or for a field its contract lacks. "
                         f"{omitted} counter(s) that were 0 on both sides are left out. "
                         "A glyph_atlas_growths, snapshot/counted row compares, per run and live window, the "
                         "growths in the checkpoint's memory snapshot with those the window's counters recorded at "
                         "the same sample; only equal figures are consistent. A run without that per-window "
                         "reading is inconclusive and shows the snapshot's and its phases' sums.")
        # A run whose frame counts contradict its recorded presenter is named, never passed silently.
        counters_note += "".join(f" Presenter mismatch: {note}." for note in presenter_notes)
    if sys.platform == "win32":
        host_lines = perf_host_info.host_block_windows(perf_host_info.windows_host_outputs(host_run), display.monitor, perf_build_policy.comparison_renderer(results))
    else:
        outputs = {}
        for name, argv in perf_worktrees.HOST_COMMANDS.items():
            record = host_run(argv, perf_host_info.HOST_COMMAND_TIMEOUT_S)
            outputs[name] = "" if perf_focus._command_failure(record) else record.stdout
        host_lines = perf_host_info.host_block(outputs, display.monitor)
    run_template = perf_scenarios.harness_argv(Path("<binary>"), "<ID>", "<variant>", digest, Path("<new scratch path>"))
    details = [f"- Base: `{args.base}` = `{shas['base']}`", f"- Head: `{args.head}` = `{shas['head']}`",
               f"- Harness hash (both trees): `{digest}`", f"- Command: `{perf_build_policy.comparison_command(args)}`",
               f"- Run length: {'short (--short: every hold 5 s, smaller floods)' if args.short else 'full'}",
               f"- Release profile overrides (both refs): {perf_build_policy.release_profile_overrides(os.environ)}",
               builds_line(args, work), f"- Runs: `{' '.join(run_template)}`",
               f"- Built with `--features {perf_harness_sources.COUNTERS_FEATURE}`: "
               f"{', '.join(side for side in perf_settings.SIDES if supports[side]) or 'neither ref'}",
               f"- Built with `--features {perf_harness_sources.FRAME_TEXTURE_FEATURE}`: "
               f"{', '.join(side for side in perf_settings.SIDES if perf_harness_sources.FRAME_TEXTURE_FEATURE in features[side]) or 'neither ref'}",
               f"- Built with `--features {perf_harness_sources.CHECKPOINT_MEMORY_FEATURE}`: "
               f"{', '.join(side for side in perf_settings.SIDES if perf_harness_sources.CHECKPOINT_MEMORY_FEATURE in features[side]) or 'neither ref'}",
               f"- Built with `--features {perf_harness_sources.ECHO_TRACE_FEATURE}`: "
               f"{', '.join(side for side in perf_settings.SIDES if perf_harness_sources.ECHO_TRACE_FEATURE in features[side]) or 'neither ref'}",
               f"- Built with `--features {perf_harness_sources.TRIM_HOOK_FEATURE}`: "
               f"{', '.join(side for side in perf_settings.SIDES if perf_harness_sources.TRIM_HOOK_FEATURE in features[side]) or 'neither ref'}",
               f"- Harness API cfgs (both refs): {', '.join(harness_cfgs) or 'none'}; compiler flags "
               f"`{' '.join(rustflags)}`",
               *(f"- {label}: {reason}" for label, reason in unavailable.items()),
               f"- Evidence: `{out}`", "", "Raw logs:", ""]
    for result in results:
        for side, evidence, kind, _reasons in result.attempts:
            details.append(f"- {result.label} {result.set_name} {side} {kind}: `{evidence}/01-harness.log`")
    if split_details:
        details += ["", "Per-run render-attempt splits (phases with a render attempt):", ""] + split_details
    completeness = perf_report.completeness_rows(results, shas, os.environ)
    details += perf_report.completeness_lines(completeness)
    document = perf_report.comparison_document(timed_rows, lap_rows, alloc_rows, host_lines, details,
                                   counter_rows=counters_table, counters_note=counters_note, overhead_rows=overhead,
                                   capped_note=capped_note, recovery_rows=recovery_rows,
                                   flag_checks=perf_comparison_rows.flag_checks(results), unclassified_rows=unclassified_rows,
                                   counters_only_rows=counters_only_rows, attribution_rows=attribution_rows)
    if attribution_evidence:
        perf_outcomes._write_json(out / perf_attribution_report.ATTRIBUTION_FILE, {
            "schema_version": perf_settings.SCHEMA_VERSION, "harness_cfgs": list(harness_cfgs),
            "features": {side: list(features[side]) for side in perf_settings.SIDES}, "sets": attribution_evidence})
    problems = perf_sets.strict_problems(results) if args.require_base else []
    if problems:
        # The first line says the table is partial, so nobody reads a head-only table as a comparison.
        document = f"**Incomplete comparison:** {'; '.join(problems)}\n\n" + document
    (out / "comparison.md").write_text(document, encoding="utf-8")
    if completeness:
        # When: S9 or S12 ran, their perf-end rows are kept for the start-size table.
        perf_outcomes._write_json(out / perf_report.COMPLETENESS_FILE, {"schema_version": perf_settings.SCHEMA_VERSION, "rows": completeness})
    profile = {"lto": {side: perf_prebuilt.release_lto(trees[side], os.environ) for side in perf_settings.SIDES},
               "overrides": perf_build_policy.release_profile_overrides(os.environ)}
    perf_outcomes._write_json(out / perf_inventory.RUN_IDENTITY_FILE, perf_inventory.run_identity_document(shas, digest, features, args.short, profile,
                                                               args.counters, os.environ, results,
                                                               perf_inventory.listed_capabilities(scenarios),
                                                               perf_attribution_report.GUARD_SPANS_CFG in harness_cfgs))
    marks["report_written"] = time.time()
    write_timing(out, marks, os.environ)
    print(document, flush=True)
    return perf_sets.comparison_exit(results, args.require_base)


# --- The previous release tag must still build the head's harness --------------------------------------------

PREVIOUS_RELEASE_TIMEOUT_S = perf_scenarios.GIT_TIMEOUT_S
