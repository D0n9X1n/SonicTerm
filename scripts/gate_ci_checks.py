"""Gate ci checks."""

from __future__ import annotations
from pathlib import Path
import re
from typing import Iterable, Mapping, Sequence, TextIO

import gate_catalog
import gate_ci_catalog
import gate_commands
import gate_harness
import gate_model
import gate_workflow


def _test_section_disables(manifest: str, target: str) -> bool:
    """Report whether a [[test]] section turns the target off or gates it on features."""
    for section in re.split(r"(?m)^\[\[test\]\]\s*$", manifest)[1:]:
        body = re.split(r"(?m)^\[", section, maxsplit=1)[0]
        if re.search(rf'(?m)^name\s*=\s*"{re.escape(target)}"', body):
            return bool(re.search(r"(?m)^(?:test\s*=\s*false|required-features\s*=)", body))
    return False


# A shard whose CI-only evidence reruns are backed by another shard's workspace step. Both must be mandatory
# Windows jobs that the fail-closed Windows aggregate checks, so moving the reruns never leaves them unbacked.
RERUN_BACKING_JOBS = {gate_catalog._WINDOWS_RUNTIME: gate_catalog._WINDOWS_WORKSPACE}
_WINDOWS_AGGREGATE = "windows"


# The only verification script an aggregate may run, line for line: every listed result must be `success`.
_AGGREGATE_HEADER = re.compile(r'for result in ((?:"\$[A-Z_]+")(?: "\$[A-Z_]+")*); do')


def aggregate_loop_operands(step: str) -> list[str] | None:
    """Return the variables an aggregate step's whole `run:` script tests, or None if it is not exactly the loop."""
    keys = re.findall(r"(?m)^        run:(.*)$", step)
    # When: the step has no or several run keys, or an inline one, its script is not the one block checked here.
    if keys != [" |"]:
        return None
    block = step.split("\n        run: |\n", 1)[1]
    lines = []
    for line in block.split("\n"):
        if line.strip() and not line.startswith("          "):
            break
        lines.append(line)
    script = [line[10:] for line in lines if line.strip()]
    if len(script) != 3 or script[1:] != ['  test "$result" = "success"', "done"]:
        return None
    header = _AGGREGATE_HEADER.fullmatch(script[0])
    return None if header is None else [operand[2:-1] for operand in header[1].split(" ")]


def aggregate_checks(workflow: str, aggregate: str, job: str) -> bool:
    """Whether an aggregate job always runs one mandatory step whose fail-closed loop tests `job`'s result."""
    body = gate_workflow._job_body(workflow, aggregate)
    if body is None:
        return False
    # When: the aggregate itself could be skipped or advisory, a failed shard would not fail the required check.
    if "\n    if: always()\n" not in "\n" + body or re.search(r"(?m)^    continue-on-error:", body):
        return False
    needs = re.search(r"(?m)^    needs: \[(.*)\]$", body)
    if needs is None or job not in [name.strip() for name in needs[1].split(",")]:
        return False
    steps = re.split(r"(?m)^      - ", body.split("    steps:\n", 1)[-1])[1:]
    if len(steps) != 1:
        return False
    step = "      - " + steps[0]
    # When: the verification step is skipped, advisory, or not Bash, its loop cannot fail the job.
    if re.search(r"(?m)^        (?:if|continue-on-error):", step) or "\n        shell: bash\n" not in step:
        return False
    operands = aggregate_loop_operands(step)
    if operands is None:
        return False
    # The loop operands are only quoted variables, each bound to one needed job's result, and none other.
    bindings = dict(re.findall(r"(?m)^          ([A-Z_]+): \$\{\{ needs\.([A-Za-z0-9_-]+)\.result \}\}$", step))
    if sorted(operands) != sorted(bindings) or len(operands) != len(set(operands)):
        return False
    return job in bindings.values()


def rerun_backing_problems(
    workflow: str, jobs: Mapping[str, list[tuple[str, str]]], workspace: str, targets: Iterable[str]
) -> list[str]:
    """Check that each reviewed rerun-backing pair in use keeps a mandatory, aggregated Windows workspace gate."""
    problems: list[str] = []
    for target in sorted(set(targets) & set(RERUN_BACKING_JOBS)):
        source = RERUN_BACKING_JOBS[target]
        label = f"evidence reruns in {target}, backed by {source}"
        if workspace not in [command for _label, command in jobs.get(source, [])]:
            problems.append(f"{label}: {source} does not run `{workspace}`")
        for job in (target, source):
            body = gate_workflow._job_body(workflow, job)
            if body is None:
                problems.append(f"{label}: ci.yml has no {job} job")
                continue
            if not re.search(r"(?m)^    runs-on: windows-latest$", body):
                problems.append(f"{label}: {job} must run on windows-latest")
            if re.search(r"(?m)^    (?:if|continue-on-error):", body):
                problems.append(f"{label}: {job} must not be conditional or advisory")
            if not aggregate_checks(workflow, _WINDOWS_AGGREGATE, job):
                problems.append(f"{label}: the {_WINDOWS_AGGREGATE} aggregate must need and check {job}")
            if job == source:
                gate_workflow._mandatory_step_index(body, f"{label}: {job}", workspace, problems)
    return problems


def _rerun_problems(
    root: Path, entry: gate_ci_catalog.CiOnly, invocation: gate_commands.Invocation,
    jobs: Mapping[str, list[tuple[str, str]]], workspace: str,
) -> list[str]:
    """Prove that one CI-only cargo test only reruns a target the local workspace step runs."""
    label = f"CI-only {entry.kind} `{entry.command}`"
    args = invocation.args
    if (
        invocation.env or len(args) != 6 or args[0] != "-p" or args[2] != "--test"
        or args[4:] != ("--", "--nocapture")
        or not gate_ci_catalog._CARGO_NAME.fullmatch(args[1]) or not gate_ci_catalog._CARGO_NAME.fullmatch(args[3])
    ):
        return [f"{label}: each cargo test must be exactly "
                f"`cargo test -p PACKAGE --test TARGET -- --nocapture`"]
    package, target = args[1], args[3]
    manifest_path = root / "crates" / package / "Cargo.toml"
    test_path = root / "crates" / package / "tests" / f"{target}.rs"
    if not manifest_path.is_file():
        return [f"{label}: crates/{package}/Cargo.toml does not exist"]
    manifest = manifest_path.read_text(encoding="utf-8")
    if not test_path.is_file():
        return [f"{label}: crates/{package}/tests/{target}.rs does not exist"]
    source = test_path.read_text(encoding="utf-8")
    problems: list[str] = []
    if f'name = "{package}"' not in manifest:
        problems.append(f"{label}: crates/{package} is not the package {package}")
    if "#[ignore" in source:
        problems.append(f"{label}: the target has ignored tests that the workspace pass skips")
    if re.search(r"(?m)^autotests\s*=\s*false", manifest) or _test_section_disables(manifest, target):
        problems.append(f"{label}: the manifest keeps the target out of the workspace pass")
    for job in entry.jobs:
        # When: a reviewed shard backs this job's reruns, that shard must run the workspace step instead.
        source = RERUN_BACKING_JOBS.get(job, job)
        if workspace not in [command for _label, command in jobs.get(source, [])]:
            problems.append(f"{label}: {source} does not run `{workspace}`, so nothing runs it locally")
    return problems


def ci_only_problems(
    root: Path, workflow: str, steps: Sequence[gate_model.Step] = gate_catalog.STEPS, entries: Sequence[gate_ci_catalog.CiOnly] = gate_ci_catalog.CI_ONLY
) -> list[str]:
    """Check that each CI-only entry is reasoned and hides no missing local test or gate."""
    jobs = gate_workflow.ci_job_commands(workflow)
    table = {gate_harness.command_text(step) for step in steps}
    workspace_step = next((step for step in steps if step.id == "workspace-crates"), None)
    problems: list[str] = []
    if workspace_step is None:
        problems.append("the table has no workspace-crates step to back CI test reruns")
        workspace = ""
    else:
        workspace = gate_harness.command_text(workspace_step)
    script = root / "scripts" / "check-workspace-crates.sh"
    if not script.is_file() or not gate_ci_catalog.WORKSPACE_TEST_COMMAND.search(script.read_text(encoding="utf-8")):
        problems.append("check-workspace-crates.sh no longer runs every workspace test target once")
    # When: a CI-only cargo test runs in a backed shard, that pair's workspace gate must be mandatory and aggregated.
    if workspace:
        rerun_jobs = [job for entry in entries if entry.kind in ("evidence-rerun", "runtime-evidence")
                      for job in entry.jobs]
        problems.extend(rerun_backing_problems(workflow, jobs, workspace, rerun_jobs))
    for entry in entries:
        label = f"CI-only `{entry.command}`"
        if entry.kind not in gate_ci_catalog.CI_ONLY_KINDS:
            problems.append(f"{label}: unknown kind {entry.kind!r}")
        if not entry.reason.strip():
            problems.append(f"{label}: has no reason")
        if not entry.jobs:
            problems.append(f"{label}: names no CI job")
        if entry.command in table:
            problems.append(f"{label}: is already a table step")
        invocations, unsupported = gate_commands.classify_command(entry.command)
        problems.extend(
            f"{label}: uses {reason}, a spelling the gate classifier does not model"
            for reason in unsupported
        )
        if not invocations and not unsupported:
            problems.append(f"{label}: is not a gate invocation, so it needs no entry")
        if any(gate_commands.is_self_test(invocation) for invocation in invocations):
            problems.append(f"{label}: is a first-party self-test; make it a table step")
        other_cargo = sorted({
            invocation.name for invocation in invocations
            if invocation.kind == "cargo" and invocation.name != "test"
        })
        if other_cargo:
            problems.append(f"{label}: cargo {', '.join(other_cargo)} in CI only is a missing local gate")
        cargo_tests = [
            invocation for invocation in invocations
            if invocation.kind == "cargo" and invocation.name == "test"
        ]
        if cargo_tests:
            if entry.kind not in ("evidence-rerun", "runtime-evidence"):
                problems.append(f"{label}: a cargo test run in CI only is a missing local test")
            for invocation in cargo_tests:
                problems.extend(_rerun_problems(root, entry, invocation, jobs, workspace))
        elif entry.kind == "evidence-rerun":
            problems.append(f"{label}: an evidence rerun must rerun a cargo test target")
    return problems
