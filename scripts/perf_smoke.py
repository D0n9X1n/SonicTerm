"""Perf smoke."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import os
from pathlib import Path
import shutil
import sys
import tempfile
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_attribution_report
import perf_build_policy
import perf_delivery
import perf_execution
import perf_focus
import perf_harness_sources
import perf_home
import perf_outcomes
import perf_processes
import perf_report
import perf_scenarios
import perf_sets
import perf_settings


# --- The smoke ------------------------------------------------------------------------------

@dataclass(frozen=True)
class SmokeCase:
    """One smoke case: a scenario variant, whether it is ended at GO, and the kind it must end as."""

    scenario: str
    variant: str = "default"
    kill_at_go: bool = False
    expected: str = "valid"

    @property
    def name(self) -> str:
        """The case's name in the log: `S1`, `S1/wgpu` or `S1-deadline`."""
        return (self.scenario + ("" if self.variant == "default" else f"/{self.variant}")
                + ("-deadline" if self.kill_at_go else ""))


# S1 and S3, then S1 ended exactly as a step deadline ends a run.
SMOKE_CASES = (SmokeCase("S1"), SmokeCase("S3"), SmokeCase("S1", kill_at_go=True))
# Windows adds the wgpu presenter, which must not be degraded, and a role program that exits right
# after GO, which must end the run invalid with a reason naming its pane.
WINDOWS_SMOKE_CASES = SMOKE_CASES + (SmokeCase("S1", "wgpu"), SmokeCase("S1", "role-exit", expected="invalid"))
# The word an invalid role-exit run's reason must hold, naming the pane whose program exited.
PANE_EXIT_WORD = "pane"


# The Windows smoke replays this S10 variant's delivery: the synchronized frames whose brackets it classifies.
SMOKE_REPLAY_VARIANT = "sync"


def smoke_case_list(platform_name: str) -> tuple[SmokeCase, ...]:
    """The smoke's cases on this host."""
    return WINDOWS_SMOKE_CASES if platform_name == "win32" else SMOKE_CASES


def case_verdict(case: SmokeCase, kind: str, reasons: Sequence[str]) -> tuple[str, list[str]]:
    """The smoke's verdict for one attempt of `case`, with the reasons it reports.

    A case that expects `invalid` passes only as `invalid` with a reason naming the pane; a valid end fails.
    """
    if case.expected == "invalid" and kind == "invalid":
        if any(PANE_EXIT_WORD in reason for reason in reasons):
            return "pass", list(reasons)
        return "fail", list(reasons) + ["the run was invalid, but no reason names the pane whose program exited"]
    if case.expected == "invalid" and kind == "valid":
        return "fail", ["the role program exited, yet the run ended valid"]
    return perf_outcomes.smoke_verdict(kind), list(reasons)
EVIDENCE_PREFIX = "sonicterm-perf-evidence-"


def smoke_cases(scenarios: Mapping[str, perf_scenarios.Scenario], binary: Path, harness_hash: str,
                run_case: Callable[[perf_outcomes.RunPlan, Path], perf_outcomes.RunOutcome], evidence: Path,
                host_platform: str | None = None,
                cases: Sequence[SmokeCase] | None = None,
                replay: Callable[[perf_scenarios.Scenario, str, Path], perf_delivery.DeliveryOutcome] | None = None,
                harness_cfgs: Sequence[str] = (),
                ) -> tuple[int, list[str]]:
    """Run the smoke's cases: exit 1 at once on a failure, 3 when a case has no valid exercised run, else 0.

    Each case's variant must be one the harness lists. Only an occlusion is retried, at most
    RETRY_LIMIT times; no timing is asserted. On Windows each attempt also prints its job's members,
    since a passing smoke deletes the evidence that holds them. With `replay` on Windows, S10/sync's
    delivery is replayed first, and a blocked replay makes the smoke exit 3. `harness_cfgs` are the harness API
    cfgs the smoke binary was built with; each plan carries the listed capabilities and that cfg decision.
    """
    host_platform = host_platform or sys.platform
    cases = smoke_case_list(host_platform) if cases is None else cases
    missing = [case.name for case in cases
               if case.scenario not in scenarios or case.variant not in scenarios[case.scenario].variants]
    replays = replay is not None and host_platform == "win32"
    if replays and ("S10" not in scenarios or SMOKE_REPLAY_VARIANT not in scenarios["S10"].variants):
        missing.append(f"S10/{SMOKE_REPLAY_VARIANT}")
    if missing:
        return perf_settings.EXIT_FAIL, [f"the harness does not list the variant of {', '.join(missing)}"]
    blocked = []
    if replays:
        try:
            _record, problem, note = replay(scenarios["S10"], SMOKE_REPLAY_VARIANT, evidence)
        except perf_sets.StopComparison as error:
            # When: the replay's teardown is unproven, its processes could disturb every case after it.
            return perf_settings.EXIT_FAIL, [str(error)]
        disclosed = f" ({note})" if note else ""
        print(f"[perf-smoke] S10/{SMOKE_REPLAY_VARIANT} delivery: " + (problem or "every check passed") + disclosed,
              flush=True)
        if problem is not None:
            # When: the replay could not show how ConPTY delivered the frames, the smoke did not exercise it.
            blocked.append(f"S10/{SMOKE_REPLAY_VARIANT} delivery: not exercised: {problem}"
                           + (f"; {note}" if note else ""))
    for case in cases:
        name = case.name
        plan = perf_outcomes.RunPlan(scenarios[case.scenario], case.variant, "smoke", binary, harness_hash, short=True,
                       smoke=True, kill_at_go=case.kill_at_go, source_root=perf_settings.ROOT,
                       latency_split_schema=scenarios[case.scenario].latency_split_schema,
                       phase_kinds=scenarios[case.scenario].phase_kinds,
                       echo_timeline_schema=scenarios[case.scenario].echo_timeline_schema,
                       guard_correlation_schema=scenarios[case.scenario].guard_correlation_schema,
                       guard_api=perf_attribution_report.GUARD_SPANS_CFG in harness_cfgs)
        reasons: list[str] = []
        for attempt in range(1, perf_settings.RETRY_LIMIT + 2):
            # A variant's `/` would make a subdirectory, so evidence names use `-`.
            outcome = run_case(plan, evidence / f"{name.replace('/', '-')}-{attempt}")
            kind, reasons = perf_outcomes.classify_outcome(outcome)
            verdict, reasons = case_verdict(case, kind, reasons)
            print(f"[perf-smoke] {name} attempt {attempt}: {kind}" + (f": {'; '.join(reasons)}" if reasons else ""),
                  flush=True)
            if host_platform == "win32":
                print(f"[perf-smoke] {name} attempt {attempt} {perf_outcomes.custody_member_text(outcome.custody)}", flush=True)
            if verdict == "fail":
                return perf_settings.EXIT_FAIL, [f"{name}: {kind}: {reason}" for reason in reasons] or [f"{name}: {kind}"]
            if verdict in ("pass", "blocked"):
                if verdict == "blocked":
                    blocked.append(f"{name}: not exercised: {'; '.join(reasons)}")
                break
        else:
            blocked.append(f"{name}: no valid run after {perf_settings.RETRY_LIMIT} retries: {'; '.join(reasons)}")
    return (perf_settings.EXIT_BLOCKED, blocked) if blocked else (perf_settings.EXIT_PASS, [])


def list_scenarios(gate, binary: Path, log_dir: Path, index: int) -> list[perf_scenarios.Scenario]:
    """Run a harness binary's `--list` through run_step and parse the scenario set."""
    step = gate.Step("list-scenarios", (str(binary), "--list"), perf_build_policy.gate_hosts(sys.platform), perf_scenarios.LIST_TIMEOUT_S, "local",
                     (), ())
    result = gate.run_step(step, index, log_dir, log_dir, perf_scenarios.harness_environment(os.environ))
    text = perf_scenarios.read_log(result.log_path)
    if result.status != "PASS":
        raise ValueError(f"`{binary} --list` {result.status}, exit {result.exit_code}:\n{perf_scenarios.log_tail(text)}")
    return perf_scenarios.parse_scenario_list(text)


def production_host(gate) -> perf_outcomes.Host:
    """The real collaborators: this host's process table, bounded lsappinfo and the user's SonicTerm home."""
    runner = gate.SMOKE_RUNNER.run_command
    excluded = (os.getsid(0),) if hasattr(os, "getsid") else ()
    return perf_outcomes.Host(gate, perf_processes.make_process_table(), lambda argv, timeout_s: perf_focus.bounded_command(runner, argv, timeout_s),
                perf_home.sonicterm_home(os.environ), Path(tempfile.gettempdir()), set(), dict(os.environ),
                excluded_sids=excluded, platform=sys.platform,
                front_sample=perf_focus.sample_foreground if sys.platform == "win32" else None)


def run_smoke(evidence: Path) -> tuple[int, list[str]]:
    """Build the current tree's debug harness (no worktree), list its scenarios and run the smoke's cases."""
    gate = perf_settings.load_gate()
    problem = perf_build_policy.gate_problem(gate)
    if problem:
        return perf_settings.EXIT_FAIL, [problem]
    # The gate's own compile-only step, so a compiler helper it cleaned does not fail the build.
    step = gate.PERF_BUILDS["build-perf_scenarios"]
    build = gate.run_step(step, 1, perf_settings.ROOT, evidence, dict(os.environ))
    build_log = perf_scenarios.read_log(build.log_path)
    binary = perf_scenarios.artifact_executable(build_log, perf_settings.HARNESS_EXAMPLE)
    if not perf_scenarios.build_passed(build) or binary is None:
        return perf_settings.EXIT_FAIL, [f"debug build {build.status}, exit {build.exit_code}:\n{perf_scenarios.log_tail(build_log)}"]
    problem = perf_scenarios.asset_problem(binary, perf_settings.ROOT)
    if problem:
        return perf_settings.EXIT_FAIL, [f"the smoke runs from {perf_settings.ROOT}, but {problem}"]
    try:
        digest = perf_harness_sources.tree_harness_hash(perf_settings.ROOT)
        scenarios = list_scenarios(gate, binary, evidence, 2)
    except (OSError, ValueError) as error:
        return perf_settings.EXIT_FAIL, [str(error)]
    print(f"[perf-smoke] harness_hash={digest} binary={binary}", flush=True)
    host = production_host(gate)
    print(f"[perf-smoke] {perf_focus.focus_rule_line(host.environ)}", flush=True)
    def replay(scenario: Scenario, variant: str, replay_evidence: Path) -> DeliveryOutcome:
        return perf_delivery.run_delivery_replay(gate, binary, scenario.id, variant, replay_evidence, 3, short=True,
                                   timeout_s=perf_scenarios.run_timeout_s(scenario, True, True), temp_root=host.temp_root,
                                   environ=host.environ)
    return smoke_cases({scenario.id: scenario for scenario in scenarios}, binary, digest,
                       lambda plan, case_evidence: perf_execution.execute_run(plan, host, case_evidence), evidence,
                       replay=replay if sys.platform == "win32" else None,
                       # The smoke's debug build composes no harness API cfg, so its App is never taken.
                       harness_cfgs=tuple(step.harness_cfgs or ()))


def replay_retried(evidence: Path) -> bool:
    """Whether `evidence` holds a delivery replay attempt after the first, so a retry was made."""
    return any((matched := perf_delivery.REPLAY_ATTEMPT_FILE.search(path.name)) and int(matched.group(1)) > 1
               for path in evidence.rglob("*") if path.is_file())


def smoke_main(environ: Mapping[str, str], runner: Callable[[Path], tuple[int, list[str]]] | None = None) -> int:
    """The perf smoke step: export the evidence directory, run the smoke, and keep the evidence on failure.

    A pass also keeps it when a delivery replay was retried, and exports REPLAY_RETRIED_ENV so CI uploads every
    attempt's record, text and log; only an unretried pass removes the evidence.
    """
    if sys.platform not in perf_report.RUN_PLATFORMS:
        print("[perf-smoke] BLOCKED: the harness runs only on macOS and Windows", flush=True)
        return perf_settings.EXIT_BLOCKED
    evidence = Path(tempfile.mkdtemp(prefix=EVIDENCE_PREFIX))
    github_env = environ.get("GITHUB_ENV")
    if github_env:
        # CI uploads this directory when the job fails.
        with Path(github_env).open("a", encoding="utf-8") as stream:
            stream.write(f"SONICTERM_PERF_EVIDENCE_DIR={evidence}\n")
    print(f"[perf-smoke] evidence={evidence}", flush=True)
    code, reasons = (runner or run_smoke)(evidence)
    for reason in reasons:
        print(f"[perf-smoke] {reason}", file=sys.stderr, flush=True)
    if code == perf_settings.EXIT_PASS and replay_retried(evidence):
        if github_env:
            # A green step uploads nothing by default; this tells the upload step a retry is worth keeping.
            with Path(github_env).open("a", encoding="utf-8") as stream:
                stream.write(f"{perf_delivery.REPLAY_RETRIED_ENV}=1\n")
        print(f"[perf-smoke] verdict=PASS; a delivery replay was retried, so evidence is kept at {evidence}",
              flush=True)
    elif code == perf_settings.EXIT_PASS:
        shutil.rmtree(evidence, ignore_errors=True)
        print(f"[perf-smoke] verdict=PASS; evidence {evidence} removed", flush=True)
    else:
        verdict = "BLOCKED" if code == perf_settings.EXIT_BLOCKED else "FAIL"
        print(f"[perf-smoke] verdict={verdict}; evidence kept at {evidence}", flush=True)
    return code
