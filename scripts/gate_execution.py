"""Gate execution."""

from __future__ import annotations
from dataclasses import dataclass, replace
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
from typing import Iterable, Mapping, Sequence, TextIO

import gate_custody
import gate_files
import gate_harness
import gate_model
import gate_preparations
import gate_process


class _PhaseSink:
    def __init__(self, log):
        self.log, self.written = log, 0

    def write(self, data):
        count = self.log.write(data)
        self.written += count
        return count

    def flush(self):
        self.log.flush()


def _phase_status(raw, policy):
    if raw["interrupted"]:
        return gate_model.INTERRUPTED
    if raw["timed_out"]:
        return gate_model.TIMEOUT
    if raw["launch_failed"]:
        return gate_model.LAUNCH
    if raw["errors"] or raw["exit_code"] != 0 or not gate_process._verified_custody(raw["custody"]):
        return gate_model.FAIL
    if raw["natural"]:
        return gate_model.PASS
    return gate_model.CLEANED_NOT_NATURAL if policy == gate_model.WindowsPolicy.COMPILE_ONLY else gate_model.FAIL


def _run_windows_step(step, root, env, log, log_path, started, output_limit_bytes):
    deadline = started + step.timeout_s
    execution_policy = gate_model.WindowsPolicy.STRICT if step.windows_preparations else step.windows_policy
    execution = gate_process.PhaseResult("execution", gate_harness.launch_argv(step), step.env, execution_policy)
    try:
        if (step.windows_preparations or step.windows_policy != gate_model.WindowsPolicy.STRICT) and not gate_preparations._reviewed_step(step):
            raise ValueError("preparation parity: synthetic step cannot authorize cleanup")
        preparations = gate_preparations.windows_preparations(step, root, env)
    except (OSError, ValueError) as error:
        phases = tuple(gate_process.PhaseResult(f"preparation-{number}", item.argv, item.env, gate_model.WindowsPolicy.COMPILE_ONLY,
                                   detail=f"not run: {error}")
                       for number, item in enumerate(step.windows_preparations, 1))
        execution = gate_process.PhaseResult(execution.name, execution.argv, execution.env, execution.policy,
                                detail=f"not run: {error}")
        return gate_process._finish(log, step, log_path, started, gate_model.FAIL, None, f"{error}; execution NOT_RUN",
                       phases=(*phases, execution))
    phases = [gate_process.PhaseResult(f"preparation-{number}", item.argv, item.env, gate_model.WindowsPolicy.COMPILE_ONLY)
              for number, item in enumerate(preparations, 1)] + [execution]
    remaining_output = output_limit_bytes
    stopping = False
    for index, phase in enumerate(phases):
        if stopping:
            break
        phase_env = dict(env)
        phase_env.update(phase.env)
        if time.monotonic() >= deadline:
            phases[index] = gate_process.PhaseResult(phase.name, phase.argv, phase.env, phase.policy, gate_model.NOT_RUN,
                                        detail="shared step deadline exhausted before admission")
            stopping = True
            break
        program = gate_process.resolve_program(phase.argv[0], root, phase_env)
        if program is None:
            phases[index] = gate_process.PhaseResult(phase.name, phase.argv, phase.env, phase.policy, gate_model.LAUNCH,
                                        detail=f"cannot find {phase.argv[0]}")
            stopping = True
            break
        command = (program, *phase.argv[1:])
        try:
            log.write((f"[local-gate] phase={phase.name} policy={phase.policy.value} argv={json.dumps(command)} "
                       f"env_overrides={json.dumps(phase.env)}\n").encode())
            log.flush()
            sink = _PhaseSink(log)
            raw = gate_model.WINDOWS_JOB.run(command, cwd=root, env=phase_env, sink=sink, deadline=deadline,
                                  output_limit_bytes=remaining_output, grace=gate_model.LEFTOVER_GRACE_S)
            if remaining_output is not None:
                remaining_output = max(0, remaining_output - sink.written)
            status = _phase_status(raw, phase.policy)
            if time.monotonic() >= deadline and status != gate_model.INTERRUPTED:
                status = gate_model.TIMEOUT
            detail = "; ".join(raw["errors"])
            if status == gate_model.TIMEOUT:
                detail = f"deadline of {step.timeout_s}s reached; process tree killed; " + detail
            if not raw["natural"]:
                detail += "; owned job required forced cleanup"
            phases[index] = gate_process.PhaseResult(phase.name, command, phase.env, phase.policy, status,
                                        raw["exit_code"], detail, raw["custody"])
            log.write((f"[local-gate] phase={phase.name} result={status} exit={gate_process._exit_text(raw['exit_code'])} "
                       f"custody={json.dumps(raw['custody'])}\n").encode())
            log.flush()
            stopping = (status in (gate_model.INTERRUPTED, gate_model.TIMEOUT, gate_model.LAUNCH) or bool(raw["errors"])
                        or not gate_process._verified_custody(raw["custody"]))
        except BaseException as error:
            prior = phases[index]
            status = (gate_model.INTERRUPTED if isinstance(error, KeyboardInterrupt) else
                      prior.status if prior.status in (gate_model.INTERRUPTED, gate_model.TIMEOUT, gate_model.LAUNCH) else gate_model.FAIL)
            phases[index] = gate_process.PhaseResult(phase.name, command, phase.env, phase.policy, status,
                                        prior.exit_code, f"phase log or invocation: {type(error).__name__}: {error}", prior.custody)
            stopping = True
    statuses = [phase.status for phase in phases]
    if gate_model.INTERRUPTED in statuses:
        status = gate_model.INTERRUPTED
    elif gate_model.TIMEOUT in statuses or any("deadline exhausted" in phase.detail for phase in phases):
        status = gate_model.TIMEOUT
    elif gate_model.LAUNCH in statuses:
        status = gate_model.LAUNCH
    elif any(value in (gate_model.FAIL, gate_model.NOT_RUN) for value in statuses):
        status = gate_model.FAIL
    else:
        status = gate_model.CLEANED_NOT_NATURAL if gate_model.CLEANED_NOT_NATURAL in statuses else gate_model.PASS
    execution = phases[-1]
    for phase in phases:
        if phase.status == gate_model.NOT_RUN:
            try:
                log.write((f"[local-gate] phase={phase.name} result=NOT_RUN argv={json.dumps(phase.argv)} "
                           f"env_overrides={json.dumps(phase.env)}\n").encode())
            except (OSError, ValueError):
                pass
    custody = execution.custody
    before = custody.get("before_cleanup") if custody else None
    count = before["active_processes"] if before is not None else None
    detail = "; ".join(f"{phase.name}={phase.status}" + (f" ({phase.detail})" if phase.detail else "") for phase in phases)
    return gate_process._finish(log, step, log_path, started, status, execution.exit_code, detail, count,
                   custody, tuple(phases))


def harness_test_executable(text: str) -> tuple[str, Path]:
    """Select one real libtest example artifact from Cargo JSON, never a stale globbed binary."""
    found = set()
    for line in text.splitlines():
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("reason") == "compiler-artifact" and record.get("profile", {}).get("test") \
                and record.get("target", {}).get("name") == "perf_scenarios" \
                and record.get("target", {}).get("kind") == ["example"] and record.get("executable") \
                and record.get("manifest_path"):
            found.add((record["executable"], Path(record["manifest_path"]).parent))
    if len(found) != 1:
        raise ValueError(f"expected one harness test executable, found {sorted(found)}")
    return found.pop()


def harness_test_environment(environ: Mapping[str, str], executable: str, cargo_output: str,
                             *, windows: bool = os.name == "nt") -> dict[str, str]:
    """Restore Cargo's native runtime search paths for directly launched libtest binaries."""
    environment = dict(environ)
    if not windows:
        return environment
    # The setup script supplies this exact triplet root, including when callers choose a dynamic triplet.
    paths = [value for value in [environment.get("SONICTERM_CAIRO_BIN_DIR")] if value]
    # Cargo propagates build-script native paths under the target directory plus its profile/deps dirs.
    profile = Path(executable).parent.parent
    for line in cargo_output.splitlines():
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if not isinstance(record, dict) or record.get("reason") != "build-script-executed":
            continue
        for linked in record.get("linked_paths", []):
            directory = linked.split("=", 1)[-1]
            if Path(directory).is_relative_to(profile):
                paths.append(directory)
    paths.extend([str(profile / "deps"), str(profile)])
    existing = next((name for name in environment if name.upper() == "PATH"), "PATH")
    paths.extend(environment.get(existing, "").split(";"))
    environment[existing] = ";".join(dict.fromkeys(value for value in paths if value))
    return environment


def _run_harness_step(step, index, root, log_dir, environ, output_limit_bytes):
    """Supervise compilation and libtest directly under one deadline, with no nested wrapper process."""
    started = time.monotonic()
    log_path = gate_process._step_log_path(log_dir, index, step)
    environ = dict(environ)
    environ.update(step.env)
    with os.fdopen(gate_files._create_output(log_path), "wb") as log:
        try:
            environ = gate_harness.harness_dependency_environment(environ, step.harness_cfgs)
            plans = list(gate_harness.target_compile_steps(step))
            if step.argv[1] == "clippy" and environ.get("RUSTC_WORKSPACE_WRAPPER"):
                raise ValueError("target lint cannot replace an inherited workspace compiler wrapper")
            execution_root = root
            with tempfile.TemporaryDirectory(prefix="harness-phases-", dir=log_dir) as directory:
                phase_logs = Path(directory)
                remaining_output = output_limit_bytes
                phases = []
                for number, plan in enumerate(plans):
                    remaining_s = step.timeout_s - (time.monotonic() - started)
                    if remaining_s <= 0:
                        return gate_process._finish(log, step, log_path, started, gate_model.TIMEOUT, None, "harness deadline exhausted")
                    bounded = replace(plan, timeout_s=remaining_s)
                    if plan.windows_policy == gate_model.WindowsPolicy.COMPILE_ONLY:
                        gate_harness._TARGET_COMPILE_STEPS.append(bounded)
                    phase_root = execution_root if plan.id == "harness-tests" else root
                    result = run_step(bounded, number, phase_root, phase_logs, environ,
                                      output_limit_bytes=remaining_output)
                    raw = result.log_path.read_bytes()
                    log.write(raw)
                    if remaining_output is not None:
                        remaining_output = max(0, remaining_output - len(raw))
                    if not result.accepted:
                        return gate_process._finish(log, step, log_path, started, result.status, result.exit_code, result.detail)
                    if result.phases:
                        phases.extend(replace(phase, name=f"target-{number}-{phase.name}") for phase in result.phases)
                    if step.argv[1] == "test" and number == 0:
                        executable, execution_root = harness_test_executable(raw.decode("utf-8", errors="replace"))
                        environ = harness_test_environment(environ, executable, raw.decode("utf-8", errors="replace"))
                        plans.append(gate_model.Step("harness-tests", (executable,), step.hosts, step.timeout_s,
                                          step.evidence, (), ()))
                if phases:
                    phases[-1] = replace(phases[-1], name="execution")
                verdict = gate_model.CLEANED_NOT_NATURAL if any(phase.status == gate_model.CLEANED_NOT_NATURAL for phase in phases) else gate_model.PASS
                return gate_process._finish(log, step, log_path, started, verdict, 0, "", phases=tuple(phases))
        except (ValueError, OSError) as error:
            return gate_process._finish(log, step, log_path, started, gate_model.LAUNCH, None, f"harness target: {error}")


def run_step(step: gate_model.Step, index: int, root: Path, log_dir: Path,
             environ: Mapping[str, str], *, output_limit_bytes: int | None = None) -> gate_process.StepResult:
    """Run one step in its own process group under a deadline that kills the group, logging its output.

    On POSIX the step ends once its leader has exited and its process group is empty;
    members that outlive the leader by LEFTOVER_GRACE_S are killed, counted, and fail the
    step. The leader is reaped only after that, so its pid keeps the group id reserved.
    Windows checks owned job accounting before output EOF and records forced compilation
    cleanup separately. An optional limit retains a prefix, drains excess bytes, and fails on overflow.
    """
    if step.harness_cfgs is not None:
        return _run_harness_step(step, index, root, log_dir, environ, output_limit_bytes)
    log_path = gate_process._step_log_path(log_dir, index, step)
    env = dict(environ)
    env.update(step.env)
    argv = gate_harness.launch_argv(step)
    started = time.monotonic()
    # A new file renamed onto log_path, so a link an earlier step left there is replaced.
    with os.fdopen(gate_files._create_output(log_path), "wb") as log:
        gate_process._write_header(log, step, argv, root)
        if gate_model.WINDOWS_JOB is not None:
            return _run_windows_step(step, root, env, log, log_path, started, output_limit_bytes)
        program = gate_process.resolve_program(argv[0], root, env)
        if program is None:
            return gate_process._finish(log, step, log_path, started, gate_model.LAUNCH, None,
                           f"cannot find {argv[0]} under the repository root or on PATH")
        try:
            process = subprocess.Popen(
                [program, *argv[1:]],
                cwd=str(root),
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                **gate_model.SMOKE_RUNNER.process_group_options(),
            )
        except OSError as error:
            return gate_process._finish(log, step, log_path, started, gate_model.LAUNCH, None, f"launch failed: {error}")
        overflow = threading.Event()
        if output_limit_bytes is None:
            reader = threading.Thread(target=gate_process._copy_output, args=(process.stdout, log), daemon=True)
        else:
            reader = threading.Thread(target=gate_process._copy_limited_output,
                                      args=(process.stdout, log, output_limit_bytes, overflow), daemon=True)
        reader.start()
        deadline = started + step.timeout_s
        timed_out = lost = False
        leftover, count = False, 0
        try:
            try:
                remaining = max(0.0, deadline - time.monotonic())
                if gate_custody._LEADER_WATCH is None:
                    # Reaping first leaves a window where the group id can be reused; only
                    # Windows and POSIX hosts with neither waitid nor kqueue take this path.
                    process.wait(timeout=remaining)
                elif not gate_custody._await_leader(process, remaining, gate_custody._LEADER_WATCH):
                    raise subprocess.TimeoutExpired(argv, step.timeout_s)
            except subprocess.TimeoutExpired:
                timed_out = True
            except gate_custody._LeaderLost:
                # The group id is no longer reserved, so no group is scanned or signalled.
                lost = True
                reader.join(max(0.0, deadline - time.monotonic()))
                timed_out = reader.is_alive()
            else:
                leftover, count = gate_custody._settle_group(process, deadline)
                if not leftover:
                    # With the group empty, a descendant that left it can still hold the output
                    # pipe, so the step stays under its deadline until the pipe closes.
                    reader.join(max(0.0, deadline - time.monotonic()))
                    timed_out = reader.is_alive()
        except KeyboardInterrupt:
            note = gate_process._kill_tree(process, owned=not lost)
            detail = gate_process._reap(process, reader, owned=not lost)
            return gate_process._finish(log, step, log_path, started, gate_model.INTERRUPTED, process.returncode,
                           f"interrupted; {note}{detail}")
        if timed_out:
            note = gate_process._kill_tree(process, owned=not lost)
            detail = gate_process._reap(process, reader, owned=not lost)
            return gate_process._finish(log, step, log_path, started, gate_model.TIMEOUT, process.returncode,
                           f"deadline of {step.timeout_s}s reached; {note}{detail}")
        # The leader has exited, so this reaps it without blocking. Until now its pid kept the
        # group id reserved for every group kill above, including the pipe-timeout one.
        if lost or not gate_process._reap_leader(process, 10):
            gate_process._close(process.stdout)
            return gate_process._finish(log, step, log_path, started, gate_model.FAIL, None,
                           "exit status unavailable: another reaper collected the leader, so no "
                           "signal was sent to its pid or group")
        if leftover:
            counted = "an unknown number of" if count is None else str(count)
            detail = (f"{counted} leftover process(es) outlived the leader by "
                      f"{gate_model.LEFTOVER_GRACE_S:g}s; process group killed{gate_process._reap(process, reader)}")
            return gate_process._finish(log, step, log_path, started, gate_model.FAIL, process.returncode, detail, count)
        gate_process._close(process.stdout)
        if overflow.is_set():
            return gate_process._finish(log, step, log_path, started, gate_model.FAIL, process.returncode,
                           f"output limit of {output_limit_bytes} bytes exceeded or capture failed")
        status = gate_model.PASS if process.returncode == 0 else gate_model.FAIL
        return gate_process._finish(log, step, log_path, started, status, process.returncode, "")
