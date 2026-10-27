"""Perf execution."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import json
import os
from pathlib import Path
import shutil
import stat
import threading

import perf_focus
import perf_font_diagnostics
import perf_guard_schema
import perf_home
import perf_memory
import perf_outcomes
import perf_scenarios
import perf_sessions
import perf_watcher


# The kept scratch's list of special files (the GO FIFOs a POSIX run releases its roles through, or any
# socket or device) left out of the copy, by path relative to the scratch and kind.
SPECIAL_FILES_RECORD = "special-files.json"


def _special_kind(path: Path) -> str | None:
    """The kind of a special file at `path`, which cannot be copied: `fifo`, `socket` or `device`; else None."""
    try:
        mode = os.lstat(path).st_mode
    except OSError:
        return None
    if stat.S_ISFIFO(mode):
        return "fifo"
    if stat.S_ISSOCK(mode):
        return "socket"
    if stat.S_ISCHR(mode) or stat.S_ISBLK(mode):
        return "device"
    return None


def _keep_scratch(scratch: Path, kept: Path) -> None:
    """Copy a run's result, logs and records into its evidence; the scratch's fixtures are left out.

    `progress.json`, which the harness rewrites after each completed phase, is kept so a killed run
    still shows its earlier phases; it is evidence only, never read as a result or for the table.
    A special file is never opened or copied (copytree fails on a named pipe, and reading one blocks); it
    is listed by name in `SPECIAL_FILES_RECORD`, written only when there is one.
    """
    kept.mkdir()
    skipped: list[dict[str, str]] = []

    def leave_special(directory: str, names: list[str]) -> set[str]:
        """copytree's ignore hook: record and leave out each special file among `names`."""
        left = set()
        for name in names:
            kind = _special_kind(Path(directory) / name)
            if kind is not None:
                left.add(name)
                skipped.append({"path": (Path(directory) / name).relative_to(scratch).as_posix(), "kind": kind})
        return left

    for name in ("result.json", "progress.json", "harness.pid", "logs", "sessions", "acks", "checkpoints", "go",
                 perf_guard_schema.GUARD_SIDECAR_DIR):
        source = scratch / name
        if source.is_symlink():
            continue
        kind = _special_kind(source)
        if kind is not None:
            skipped.append({"path": name, "kind": kind})
        elif source.is_dir():
            shutil.copytree(source, kept / name, symlinks=True, ignore=leave_special)
        elif source.is_file():
            shutil.copy2(source, kept / name)
    if skipped:
        record = sorted(skipped, key=lambda entry: entry["path"])
        (kept / SPECIAL_FILES_RECORD).write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")


def execute_run(plan: perf_outcomes.RunPlan, host: perf_outcomes.Host, evidence: Path) -> perf_outcomes.RunOutcome:
    """Run the harness once through run_step, serve it while it runs, clean up and record the evidence."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_build_policy
    import perf_result_schema
    evidence.mkdir(parents=True)
    scratch = perf_scenarios.new_scratch_path(host.temp_root, plan.scenario.id, plan.variant)
    # The snapshot comes before the sentinel, so a write between them reads as a change, never as an old file.
    home_unresolved: list[str] = []
    try:
        before = perf_home.snapshot_home(host.home)
    except perf_home.HomeSnapshotUnresolved as error:
        before = None
        home_unresolved.append(f"before the run: {error}")
    sentinel = evidence / "sentinel"
    sentinel.write_bytes(b"")
    sentinel_ns = sentinel.stat().st_mtime_ns
    sampler = perf_watcher.FrontSampler(evidence / "front-samples.log", host.front_run, host.printed_forms,
                           sample=host.front_sample)
    sampler.sample()  # The baseline, so the first sample after launch has a predecessor.
    launch_unix_s = host.clock()
    watcher = perf_watcher.RunWatcher(perf_watcher.RunContext(scratch, evidence, launch_unix_s, host.table, host.gate,
                                    host.excluded_sids, plan.kill_at_go, plan.binary.name, platform=host.platform))
    thread_problems: list[str] = []
    stop = threading.Event()
    threads = [perf_watcher.run_periodically(sampler.sample, perf_focus.FRONT_SAMPLE_INTERVAL_S, stop, thread_problems, "front sampler"),
               perf_watcher.run_periodically(watcher.poll, perf_watcher.WATCH_INTERVAL_S, stop, thread_problems, "run watcher")]
    argv = perf_scenarios.harness_argv(plan.binary, plan.scenario.id, plan.variant, plan.harness_hash, scratch,
                        short=plan.short, laps=plan.laps, counters=plan.counters)
    step = host.gate.Step("harness", argv, perf_build_policy.gate_hosts(host.platform),
                          perf_scenarios.run_timeout_s(plan.scenario, plan.smoke, plan.short), "local", (), ())
    try:
        # The cwd is the tree that built the binary, so the App loads that tree's fonts; logs stay in the evidence.
        step_result = host.gate.run_step(step, 1, plan.source_root, evidence, perf_scenarios.harness_environment(host.environ))
    finally:
        stop.set()
        for thread in threads:
            thread.join(perf_watcher.FOOTPRINT_TIMEOUT_S + 30)
    if any(thread.is_alive() for thread in threads):
        thread_problems.append("a watcher thread did not stop, so the session records were not rescanned")
    else:
        watcher.final_scan()
    sessions = watcher.sessions_for_cleanup()
    unanchored = list(watcher.unanchored.values())
    custody = getattr(step_result, "custody", None)
    leftover = step_result.leftover_processes
    if host.platform == "win32":
        # The gate's job owns every process the harness started, so its custody, not anchors, proves teardown.
        cleanup = perf_outcomes.custody_cleanup(custody)
        leftover = perf_outcomes.windows_leftover_processes(custody, deadline_case=plan.kill_at_go and bool(watcher.deadline["sent"]))
    elif host.table is None:
        cleanup = perf_sessions.CleanupResult()
        if sessions or unanchored:
            cleanup.unresolved("this host has no process table, so the sessions were not cleaned")
    else:
        cleanup = perf_sessions.cleanup_sessions(host.table, sessions, unanchored=unanchored)
    try:
        after = perf_home.snapshot_home(host.home)
    except perf_home.HomeSnapshotUnresolved as error:
        after = None
        home_unresolved.append(f"after the run: {error}")
    if home_unresolved:
        # A check that could not read the home whole never clears the run, whatever the readable part shows.
        other = False
        home = [f"cannot be ruled out: the home check is unresolved {problem}" for problem in home_unresolved]
    else:
        other = after is not None and perf_home.other_instance_alive(host.table, watcher.harness_pid)
        home = perf_home.home_violations(host.home, before, after, sentinel_ns, watcher.harness_pid, other,
                               [str(scratch), os.path.realpath(scratch)])
    kept = evidence / "scratch"
    _keep_scratch(scratch, kept)
    if scratch.is_dir() and not scratch.is_symlink():
        shutil.rmtree(scratch, ignore_errors=True)
    log_text = perf_scenarios.read_log(step_result.log_path)
    # The font crate logs on its `config` target, which only the stderr layer keeps, so the run_step log
    # shows the failure; the run's own logs are read as well.
    font_failures = perf_outcomes.primary_font_errors(log_text.splitlines() + list(perf_memory._log_lines(kept / "logs")))
    not_exercised = any(perf_outcomes._NOT_EXERCISED.search(line) for line in log_text.splitlines()
                        if not line.startswith("[local-gate]"))
    data, schema = None, []
    if (kept / "result.json").is_file():
        try:
            parsed = json.loads((kept / "result.json").read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            schema = [f"result.json cannot be parsed: {error}"]
        else:
            # A run ended at GO is expected to leave no complete result, so its result is not judged.
            if not (plan.kill_at_go and watcher.deadline["sent"]):
                exit_code = step_result.exit_code if step_result.status != "TIMEOUT" else None
                # A base may predate a contract field; the head is the contract under test.
                schema = perf_result_schema.validate_result(parsed, plan.harness_hash, exit_code, counters=plan.counters,
                                         partial_counters=plan.side == "base", platform_name=host.platform,
                                         latency_split_schema=plan.latency_split_schema,
                                         phase_kinds=plan.phase_kinds,
                                         attribution_api=plan.attribution_api,
                                         attribution_schema=plan.attribution_schema,
                                         echo_timeline_schema=plan.echo_timeline_schema,
                                         guard_correlation_schema=plan.guard_correlation_schema,
                                         scope=(plan.scenario.id, plan.variant))
            data = parsed if isinstance(parsed, dict) else None
    phases = data.get("phases") if data is not None else None
    if isinstance(phases, list) and (plan.guard_correlation_schema is not None or any(
            isinstance(phase, dict) and "guard_correlation" in phase for phase in phases)):
        # When: the harness declares the contract or wrote a field, every finalized phase gets a verdict.
        availability = perf_guard_schema.guard_availability(plan.guard_api, perf_result_schema.frame_counter_state(data))
        verdicts = perf_guard_schema.guard_correlation_verdicts(phases, kept, plan.harness_hash, availability)
        perf_outcomes._write_json(evidence / "guard-correlation-join.json", verdicts)
        for verdict in verdicts:
            print(perf_guard_schema.format_guard_verdict(verdict), flush=True)
    if host.platform == "win32":
        focus = perf_focus.judge_foreground(sampler.readings, user_session=perf_focus.has_user_session(host.environ))
    else:
        focus = perf_focus.judge_focus(sampler.readings, watcher.harness_pid, user_session=perf_focus.has_user_session(host.environ))
    # Windows runs record the adapter they drew through, so a pair on two adapters is never compared.
    renderer = perf_font_diagnostics.read_renderer(kept / "logs") if host.platform == "win32" else None
    for failed in focus.failed:
        print(perf_focus.describe_sample(failed), flush=True)
    for note in focus.notes:
        print(f"[perf-compare] focus: {note}", flush=True)
    outcome = perf_outcomes.RunOutcome(plan, evidence, step_result.status, step_result.exit_code, data, schema, not_exercised,
                         focus, thread_problems + watcher.problems, cleanup, home, dict(watcher.deadline),
                         perf_memory.read_memory_samples(kept / "logs"),
                         perf_font_diagnostics.read_render_timing(kept / "logs") if plan.laps else [], watcher.footprints,
                         watcher.harness_pid, leftover, step_result.detail, font_failures,
                         custody=custody if host.platform == "win32" else None,
                         deadline_exit_code=perf_outcomes.deadline_exit_code(host.platform), platform=host.platform,
                         renderer=renderer,
                         fallback_log=perf_font_diagnostics.read_fallback_log(kept / "logs") if plan.laps else None,
                         present_lines=perf_memory.read_present_lines(kept / "logs") if plan.counters else [])
    kind, reasons = perf_outcomes.classify_outcome(outcome)
    cleanup_record = {
        "settled": cleanup.settled, "signalled": cleanup.signalled, "problems": cleanup.problems,
        "survivors": [asdict(member) for member in cleanup.survivors],
        "sessions": [asdict(session) for session in sessions],
        "unanchored": [asdict(record) for record in unanchored]}
    if host.platform == "win32":
        # The job's counts and members before the gate ended it, and whether it had to.
        cleanup_record.update(cleanup=(custody or {}).get("cleanup"), before_cleanup=(custody or {}).get("before_cleanup"),
                              programs=[asdict(program) for program in watcher.programs.values()])
    perf_outcomes._write_json(evidence / "cleanup.json", cleanup_record)
    perf_outcomes._write_json(evidence / "home-check.json", {
        "home": str(host.home), "unresolved": bool(home_unresolved),
        "absent_before": before is None, "absent_after": after is None,
        "sentinel_mtime_ns": sentinel_ns, "other_instance_alive": other, "violations": home})
    perf_outcomes._write_json(evidence / "footprints.json", watcher.footprints)
    perf_outcomes._write_json(evidence / "outcome.json", {
        "kind": kind, "reasons": reasons, "side": plan.side, "scenario": plan.scenario.id, "variant": plan.variant,
        "argv": list(argv), "status": step_result.status, "exit_code": step_result.exit_code,
        "launch_unix_s": launch_unix_s, "harness_pid": watcher.harness_pid, "deadline": watcher.deadline,
        "focus_problems": focus.problems, "focus_notes": focus.notes, "foreground_changes": focus.changes,
        "renderer": renderer, "watcher_problems": outcome.watcher_problems,
        "schema_problems": schema, "log": str(step_result.log_path)})
    return outcome


# --- Statistics per scenario and the tables -------------------------------------------------

MIB = 1024 * 1024
TABLE_HEADER = "| Scenario | Metric (unit) | Baseline | PR | Change |\n| --- | --- | --- | --- | --- |\n"
# Latency acceptance: each side attributes at least this share of its samples, and the sides differ by at most the gap.
LATENCY_MIN_PERCENT = 80
LATENCY_MAX_GAP_POINTS = 10
