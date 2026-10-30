"""Gate workflow."""

from __future__ import annotations
import re
from typing import Iterable, Mapping, Sequence, TextIO

import gate_catalog
import gate_ci_catalog
import gate_commands
import gate_harness
import gate_model
_NESTED_RUN = re.compile(r"""["']?run["']?[ \t]*:""")
# Characters that cannot start a plain YAML scalar the parser reads verbatim.
_SCALAR_INDICATORS = frozenset(">|'\"&*!%@`{}[],?:-#")
_STEP_INDENT = 6
_KEY_INDENT = 8
# The step shells the classifier models. Any other value, including a custom template,
# can move or rewrap a step's commands, so it raises like a working-directory.
_STEP_SHELLS = ("bash", "pwsh")


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _unfamiliar(number: int, what: str) -> ValueError:
    """Describe a workflow form the parser refuses to guess at, so parity fails loudly."""
    return ValueError(f"ci.yml line {number}: {what}; the parity check reads only the forms it models")


def job_host(job: str) -> str | None:
    """Return the host a platform shard runs on, from its job-name prefix."""
    if job == "tooling":
        return "linux"
    for host in gate_model.HOSTS:
        if job.startswith(host):
            return host
    return None


def _logical_lines(script: str) -> list[str]:
    """Join shell and PowerShell continuation lines, dropping blanks and comments."""
    lines: list[str] = []
    pending = ""
    for raw in script.splitlines():
        stripped = raw.strip()
        if not pending and (not stripped or stripped.startswith("#")):
            continue
        if stripped.endswith("\\") or stripped.endswith("`"):
            pending += stripped[:-1].rstrip() + " "
            continue
        lines.append((pending + stripped).strip())
        pending = ""
    if pending.strip():
        lines.append(pending.strip())
    return lines


def _block_body(number: int, extent: Sequence[tuple[int, str]]) -> str:
    """Return a literal `run: |` block's text, raising on indentation YAML would read differently."""
    body: list[str] = []
    width: int | None = None
    ended = False
    for line_number, line in extent:
        if not line.strip():
            if not ended:
                body.append("")
            continue
        indent = _indent(line)
        comment = line.lstrip().startswith("#")
        if width is None:
            if indent <= _KEY_INDENT:
                raise _unfamiliar(number, "an empty run: block")
            width = indent
        if ended or indent < width:
            # A comment at or left of the key ends the block; any other shallower line is a
            # form YAML reads differently from its layout.
            if comment and indent <= _KEY_INDENT:
                ended = True
                continue
            raise _unfamiliar(line_number, "a run: block line indented less than its first line")
        body.append(line[width:])
    if width is None:
        raise _unfamiliar(number, "an empty run: block")
    return "\n".join(body)


def _step_commands(lines: Sequence[tuple[int, str]]) -> tuple[str, list[str]]:
    """Return one step's label and the logical lines of its `run:` value.

    Each line is (line number, text), with the item's first key re-indented to eight
    spaces. A run: form the parser does not model raises instead of being skipped, as do
    a working-directory, a local action, and a shell other than a plain bash or pwsh.
    """
    label = ""
    commands: list[str] = []
    seen_run = False
    index = 0
    while index < len(lines):
        number, line = lines[index]
        index += 1
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if _indent(line) != _KEY_INDENT:
            raise _unfamiliar(number, "a step line outside any step key's value")
        match = gate_commands._KEY.fullmatch(stripped)
        if match is None:
            raise _unfamiliar(number, f"a step key the parser does not model: {stripped[:60]!r}")
        key, value = match.group(1), (match.group(2) or "").strip()
        # The key's value spans every following blank, comment, or deeper line.
        start = index
        while index < len(lines) and (
            not lines[index][1].strip()
            or lines[index][1].lstrip().startswith("#")
            or _indent(lines[index][1]) > _KEY_INDENT
        ):
            index += 1
        extent = lines[start:index]
        if key in ("name", "uses") and not label:
            label = value
        if key == "uses" and value.strip("'\"").startswith("./"):
            raise _unfamiliar(number, "a local action, whose commands the parity check cannot read")
        if key == "working-directory":
            raise _unfamiliar(number, "a working-directory, which moves the step's commands off the root")
        if key == "shell" and (value not in _STEP_SHELLS or any(
            text.strip() and not text.lstrip().startswith("#") for _number, text in extent
        )):
            # Only a plain name on the key's own line is read; a quoted, flow, block, or
            # continued value is another spelling that YAML may read as a different shell.
            raise _unfamiliar(number, f"a shell: other than a plain bash or pwsh: {value[:40]!r}")
        if key != "run":
            for nested_number, nested in extent:
                if _NESTED_RUN.match(nested.strip()):
                    raise _unfamiliar(nested_number, "a run: key nested under another step key")
            continue
        if seen_run:
            raise _unfamiliar(number, "a second run: key in one step")
        seen_run = True
        if value.startswith("|"):
            if not gate_commands._BLOCK_HEADER.fullmatch(value):
                raise _unfamiliar(number, f"a run: block header the parser does not model: {value!r}")
            commands.extend(_logical_lines(_block_body(number, extent)))
        elif not value or value[0] in _SCALAR_INDICATORS:
            raise _unfamiliar(number, "a quoted, folded, flow, or empty run: value")
        elif " #" in value or "\t#" in value:
            raise _unfamiliar(number, "a comment after a plain run: value")
        elif any(text.strip() and not text.lstrip().startswith("#") for _number, text in extent):
            raise _unfamiliar(number, "a plain run: value continued on later lines")
        else:
            commands.extend(_logical_lines(value))
    return label, commands


def ci_job_commands(workflow: str) -> dict[str, list[tuple[str, str]]]:
    """Map each ci.yml job to its (step label, logical run command) pairs, in file order.

    The parser models this repository's layout: job ids at two spaces, job keys at
    four, step items at six, and step keys at eight. Any other form raises, so a new
    gate step cannot escape the parity check by being written differently.
    """
    lines = workflow.splitlines()
    starts = [number for number, line in enumerate(lines) if gate_commands._JOBS_KEY.fullmatch(line)]
    if len(starts) != 1:
        raise ValueError("workflow must have exactly one top-level jobs mapping")
    for number, line in enumerate(lines):
        if not line.strip() or line[0] in " #":
            continue
        # Every top-level key is checked, before or after jobs, so a quoted, flow, or
        # complex key cannot hide a mapping that applies to every job.
        match = gate_commands._KEY.fullmatch(line)
        if match is None:
            raise _unfamiliar(number + 1, f"a top-level line the parser does not model: {line[:60]!r}")
        if match.group(1) == "defaults":
            raise _unfamiliar(number + 1, "workflow defaults:, whose inherited run settings apply to "
                                          "every run step and which the parity check does not read")
    jobs: dict[str, list[list[tuple[int, str]]] | None] = {}
    job: str | None = None
    steps: list[list[tuple[int, str]]] | None = None
    for number in range(starts[0] + 1, len(lines)):
        line, line_number = lines[number], number + 1
        stripped = line.strip()
        if not stripped:
            if steps:
                steps[-1].append((line_number, line))
            continue
        if "\t" in line[:len(line) - len(line.lstrip())]:
            raise _unfamiliar(line_number, "a tab in indentation")
        indent = _indent(line)
        if stripped.startswith("#"):
            if steps:
                steps[-1].append((line_number, line))
            continue
        if indent == 0:
            # A top-level key ends the jobs mapping.
            break
        if indent >= _KEY_INDENT or (indent == _STEP_INDENT and steps is None):
            if job is None:
                raise _unfamiliar(line_number, "content before the first job id")
            if steps is None:
                continue  # Part of a job key's value, such as strategy or env.
            if not steps:
                raise _unfamiliar(line_number, "content before the job's first step item")
            steps[-1].append((line_number, line))
        elif indent == _STEP_INDENT:
            rest = stripped[2:]
            if not stripped.startswith("- ") or not rest.strip() or rest.lstrip().startswith("#"):
                raise _unfamiliar(line_number, "a step item without a key on its dash line")
            if rest.startswith((" ", "\t")):
                raise _unfamiliar(line_number, "a step item whose first key is not two columns after its dash")
            steps.append([(line_number, " " * _KEY_INDENT + rest)])
        elif indent == 4:
            match = gate_commands._KEY.fullmatch(stripped)
            if job is None or match is None:
                raise _unfamiliar(line_number, f"a job key the parser does not model: {stripped[:60]!r}")
            key, value = match.group(1), (match.group(2) or "").strip()
            if key == "uses":
                raise _unfamiliar(line_number, "a reusable-workflow job, whose steps the parity check cannot read")
            if key == "defaults":
                # A job's run defaults move or rewrap each step, like a step's working-directory.
                raise _unfamiliar(line_number, f"defaults: in job {job}, whose inherited run settings "
                                               f"apply to every run step and which the parity check does not read")
            if key != "steps":
                steps = None
                continue
            if value and not value.startswith("#"):
                raise _unfamiliar(line_number, "a steps: value on its key's line")
            if jobs[job] is not None:
                raise _unfamiliar(line_number, f"a second steps: key in job {job}")
            steps = jobs[job] = []
        elif indent == 2:
            match = gate_commands._KEY.fullmatch(stripped)
            value = (match.group(2) or "").strip() if match else ""
            if match is None or not gate_commands._JOB_ID.fullmatch(match.group(1)) or (value and not value.startswith("#")):
                raise _unfamiliar(line_number, f"a job id the parser does not model: {stripped[:60]!r}")
            job = match.group(1)
            if job in jobs:
                raise _unfamiliar(line_number, f"a second definition of job {job}")
            jobs[job] = None
            steps = None
        else:
            raise _unfamiliar(line_number, f"indentation {indent}, which the parser does not model")
    result: dict[str, list[tuple[str, str]]] = {}
    for name, items in jobs.items():
        if items is None:
            raise ValueError(f"ci.yml job {name} has no steps: list the parity check can read")
        pairs: list[tuple[str, str]] = []
        for item in items:
            label, commands = _step_commands(item)
            pairs.extend((label, command) for command in commands)
        result[name] = pairs
    return result


# The commands each macOS smoke job must run unconditionally, in this order, before the release build.
_MACOS_SMOKE_GATES = (
    "cargo build --locked -p sonicterm-app --example native_split_selection",
    "python3 scripts/native-selection-smoke.py",
    "python3 scripts/perf-compare.py --smoke",
)
_MACOS_RELEASE_BUILD = "cargo build --release -p sonicterm-mac"
# macos-core runs the Apple Silicon smoke on its macos-15 runner; macos-smoke runs the Intel one.
_MACOS_SMOKE_JOBS = ("macos-core", "macos-smoke")


def macos_smoke_ci_problems(workflow: str) -> list[str]:
    """Keep native selection and the perf scenario smoke mandatory and ordered for both macOS architectures."""
    problems = []
    for job in _MACOS_SMOKE_JOBS:
        body = _job_body(workflow, job)
        if body is None:
            problems.append(f"the macOS smoke gates require the {job} job")
            continue
        problems.extend(_macos_smoke_job_problems(job, body))
    return problems


def _macos_smoke_job_problems(job: str, body: str) -> list[str]:
    """Check one macOS smoke job: unconditional, on its architecture, with the gates ordered."""
    problems = []
    if re.search(r"(?m)^    (?:if|continue-on-error):", body):
        problems.append(f"{job} must not be conditional or advisory")
    # When: macos-core packages the aarch64 bundle by name; macos-smoke's matrix must hold only the Intel leg.
    if job == "macos-core":
        covered = "ci mac-aarch64\n" in body
    else:
        covered = (re.findall(r"(?m)^            arch: (\S+)\s*$", body) == ["x86_64"]
                   and not re.search(r"(?m)^        exclude:", body))
    if not covered:
        problems.append("the macOS smoke gates must run on both macOS architectures")
    step_bodies = re.split(r"(?m)^      - ", body)[1:]
    positions: dict[str, int] = {}
    for command in (*_MACOS_SMOKE_GATES, _MACOS_RELEASE_BUILD):
        matches = [index for index, step in enumerate(step_bodies)
                   if "        run: " + command + "\n" in step]
        if len(matches) != 1:
            problems.append(f"{job} needs one mandatory `{command}` step")
            continue
        positions[command] = matches[0]
        if command in _MACOS_SMOKE_GATES and re.search(
                r"(?m)^(?:        )?(?:if|continue-on-error):", step_bodies[matches[0]]):
            problems.append(f"{job} step `{command}` must not be conditional or advisory")
    build, selection, perf = _MACOS_SMOKE_GATES
    for ordered, message in (
        ((build, selection, _MACOS_RELEASE_BUILD),
         "native selection must build then run before the release build and packaging"),
        ((selection, perf, _MACOS_RELEASE_BUILD),
         "the perf scenario smoke must run after the native selection smoke and before the release build"),
    ):
        indexes = [positions[command] for command in ordered if command in positions]
        # Each command owns a distinct step, so a sorted index list is a strictly increasing one.
        if len(indexes) == len(ordered) and indexes != sorted(indexes):
            problems.append(f"{job}: {message}")
    return problems


_WINDOWS_PERF_GATES = (
    "cargo build --locked -p sonicterm-app --example perf_scenarios",
    "python scripts/perf-compare.py --smoke",
)


def _job_body(workflow: str, job: str) -> str | None:
    """Return one ci.yml job's body, from the line after its id to the next job id."""
    match = re.search(rf"(?ms)^  {re.escape(job)}:\n(.*?)(?=^  [A-Za-z0-9_-]+:|\Z)", workflow)
    return None if match is None else match[1]


def _mandatory_step_index(body: str, job: str, command: str, problems: list[str]) -> int | None:
    """Find the one unconditional step running `command` in a job body, reporting a missing or advisory one."""
    step_bodies = re.split(r"(?m)^      - ", body)[1:]
    matches = [index for index, step in enumerate(step_bodies) if "        run: " + command + "\n" in step]
    if len(matches) != 1:
        problems.append(f"{job} needs one mandatory `{command}` step")
        return None
    if re.search(r"(?m)^(?:        )?(?:if|continue-on-error):", step_bodies[matches[0]]):
        problems.append(f"{job} step `{command}` must not be conditional or advisory")
    return matches[0]


def windows_runtime_ci_problems(workflow: str) -> list[str]:
    """Keep the Windows perf harness build and its smoke mandatory, and the build first, in the runtime shard."""
    body = _job_body(workflow, gate_catalog._WINDOWS_RUNTIME)
    if body is None:
        return [f"the Windows perf gates require the {gate_catalog._WINDOWS_RUNTIME} job"]
    problems = []
    if re.search(r"(?m)^    (?:if|continue-on-error):", body):
        problems.append(f"{gate_catalog._WINDOWS_RUNTIME} must not be conditional or advisory")
    positions: dict[str, int] = {}
    for command in _WINDOWS_PERF_GATES:
        index = _mandatory_step_index(body, gate_catalog._WINDOWS_RUNTIME, command, problems)
        if index is not None:
            positions[command] = index
    build, smoke = _WINDOWS_PERF_GATES
    # When: both steps exist once, the smoke must follow the build whose output it reuses.
    if build in positions and smoke in positions and positions[build] > positions[smoke]:
        problems.append("the Windows perf scenario harness must build before its smoke runs")
    return problems


def ci_parity_problems(
    workflow: str, steps: Sequence[gate_model.Step] = gate_catalog.STEPS, entries: Sequence[gate_ci_catalog.CiOnly] = gate_ci_catalog.CI_ONLY
) -> list[str]:
    """Report every disagreement between the table, the CI-only list, and ci.yml."""
    jobs = ci_job_commands(workflow)
    by_command = {gate_harness.command_text(step): step for step in steps}
    problems = (macos_smoke_ci_problems(workflow)
                if any(step.id in ("macos-selection-smoke", "macos-perf-smoke") for step in steps) else [])
    if any(step.id == "windows-perf-smoke" for step in steps):
        problems += windows_runtime_ci_problems(workflow)
    for step in steps:
        text = gate_harness.command_text(step)
        for job in step.ci_jobs:
            if job not in jobs:
                problems.append(f"table step {step.id} names CI job {job}, which ci.yml does not define")
            elif text not in [command for _label, command in jobs[job]]:
                problems.append(f"table step {step.id}: `{text}` is missing from CI job {job}")
    used: set[tuple[int, str]] = set()
    for job, pairs in jobs.items():
        for label, command in pairs:
            step = by_command.get(command)
            if step is not None:
                if job not in step.ci_jobs:
                    problems.append(
                        f"CI job {job} step {label!r} runs table step {step.id}, "
                        f"but the table does not name {job}"
                    )
                continue
            invocations, unsupported = gate_commands.classify_command(command)
            problems.extend(
                f"CI job {job} step {label!r} runs `{command}`, which uses {reason}, "
                f"a spelling the gate classifier does not model"
                for reason in unsupported
            )
            if not invocations and not unsupported:
                continue
            matches = [
                number for number, entry in enumerate(entries)
                if entry.command == command and job in entry.jobs
            ]
            if not matches:
                problems.append(
                    f"CI job {job} step {label!r} runs `{command}`, which is neither a table "
                    f"step nor on the CI-only list"
                )
            used.update((number, job) for number in matches)
    for number, entry in enumerate(entries):
        for job in entry.jobs:
            if (number, job) not in used:
                problems.append(f"CI-only `{entry.command}` no longer runs in CI job {job}")
    return problems
