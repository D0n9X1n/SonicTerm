#!/usr/bin/env python3
"""Run SonicTerm's local verification gate from one declarative, host-aware step table.

STEPS is the only definition of the local gate. CLAUDE.md and both
Development-and-Release wiki files embed its rendered form, and ci.yml keeps
explicit steps that local-gate_tests.py checks against it, so a gate command
edited in one place alone fails a test.

The runner executes the steps selected for the current host in table order. Each
step runs in its own process group under a whole-tree deadline, reusing the
native smoke runner's launch and tree-kill logic, and later steps still run
after a failure, a timeout, or a launch error. Tracked and untracked Git state
is recorded before and after the run; changes are reported, never cleaned.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import difflib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from typing import Mapping, Sequence, TextIO

_HERE = Path(__file__).resolve().parent


def _load_smoke_runner():
    """Load native-smoke-runner.py so every step reuses its process-group launch and tree kill."""
    spec = importlib.util.spec_from_file_location(
        "sonicterm_native_smoke_runner", _HERE / "native-smoke-runner.py"
    )
    if spec is None or spec.loader is None:
        raise ImportError("cannot load scripts/native-smoke-runner.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SMOKE_RUNNER = _load_smoke_runner()

HOSTS = ("macos", "windows", "linux")
HOST_LABELS = {"macos": "macOS", "windows": "Windows", "linux": "Linux"}

# `local` is the mandatory default gate. `release` is release-preparation
# evidence. `optional` is a pre-push aid that never names a CI job.
EVIDENCE_CLASSES = ("local", "release", "optional")

PASS = "PASS"
FAIL = "FAIL"
TIMEOUT = "TIMEOUT"
LAUNCH = "LAUNCH"
INTERRUPTED = "INTERRUPTED"

# Git commands run against a repository with large vendored trees; this bounds
# them without making a slow disk look like a gate failure.
GIT_TIMEOUT_S = 300
TAIL_LINES = 20
PRE_EXISTING_LIMIT = 40

# Each prerequisite key maps to its (English, Chinese) legend text.
PREREQUISITES = {
    "rust": (
        "the Rust toolchain from `rust-toolchain.toml`, with rustfmt and clippy.",
        "`rust-toolchain.toml` 指定的 Rust 工具链，包含 rustfmt 与 clippy。",
    ),
    "native": (
        "the platform's native build libraries: Cairo and pkg-config on macOS "
        "(`brew install cairo pkg-config`), Cairo from `scripts/setup-windows-cairo.ps1` "
        "on Windows, and the packages the `linux-core` job installs on Linux.",
        "平台原生构建库：macOS 上的 Cairo 与 pkg-config（`brew install cairo pkg-config`），"
        "Windows 上由 `scripts/setup-windows-cairo.ps1` 安装的 Cairo，以及 Linux 上 "
        "`linux-core` job 安装的软件包。",
    ),
    "bash": (
        "`bash` on `PATH`; on Windows, run the gate from Git Bash so Git's `bash` is found first.",
        "`PATH` 上的 `bash`；在 Windows 上请从 Git Bash 运行 gate，使 Git 的 `bash` 优先被找到。",
    ),
    "pwsh": (
        "PowerShell 7 (`pwsh`) on `PATH`.",
        "`PATH` 上的 PowerShell 7（`pwsh`）。",
    ),
    "llvm-cov": (
        "`cargo-llvm-cov` at the `CARGO_LLVM_COV_VERSION` that `ci.yml` pins.",
        "`ci.yml` 中 `CARGO_LLVM_COV_VERSION` 固定版本的 `cargo-llvm-cov`。",
    ),
    "warp": (
        "a DX12 WARP adapter with allocator reporting.",
        "支持 allocator report 的 DX12 WARP adapter。",
    ),
}


@dataclass(frozen=True)
class Step:
    """One gate command and the facts the runner, the docs, and the parity tests share."""

    id: str
    argv: tuple[str, ...]
    hosts: tuple[str, ...]
    timeout_s: int
    evidence: str
    prerequisites: tuple[str, ...]
    ci_jobs: tuple[str, ...]
    env: tuple[tuple[str, str], ...] = ()
    # `pwsh` runs argv[0] as a PowerShell script; None runs argv directly.
    shell: str | None = None


_PLAIN_WORD = re.compile(r"^[A-Za-z0-9_@%+=:,./\\-]+$")


def _display_word(word: str) -> str:
    """Quote a word only when a shell would otherwise split or expand it."""
    if _PLAIN_WORD.match(word):
        return word
    return '"' + word.replace('"', '\\"') + '"'


def command_text(step: Step) -> str:
    """Render a step as the single command line that ci.yml and the docs show."""
    words = [f"{name}={_display_word(value)}" for name, value in step.env]
    words.extend(_display_word(word) for word in step.argv)
    return " ".join(words)


def launch_argv(step: Step) -> tuple[str, ...]:
    """Return the argv the runner starts, wrapping a PowerShell script in `pwsh -File`."""
    if step.shell == "pwsh":
        return ("pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-File", *step.argv)
    return step.argv


_RUSTDOC_WARNINGS = (("RUSTDOCFLAGS", "-D warnings"),)
_CORE_CHECKS = ("macos-core", "windows-checks", "linux-core")
_CORE_TESTS = ("macos-core", "windows-tests", "linux-core")

# Local timeouts reuse each command's CI step budget, which is sized above
# recent cold-cache runtime. Commands that CI runs only inside a combined step
# reuse that step's budget, capped at five minutes for the shell tooling tests;
# a step that no CI job runs is sized well above its measured runtime.
# Prerequisites include what a command reaches indirectly, such as a test suite
# that reads `cargo metadata`.
STEPS = (
    Step("fmt", ("cargo", "fmt", "--all", "--check"), HOSTS, 300, "local",
         ("rust",), _CORE_CHECKS),
    Step("clippy", ("cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"),
         HOSTS, 900, "local", ("rust", "native"), _CORE_CHECKS),
    Step("doc", ("cargo", "doc", "--workspace", "--no-deps"), HOSTS, 600, "local",
         ("rust", "native"), _CORE_CHECKS, env=_RUSTDOC_WARNINGS),
    Step("doc-resource-features",
         ("cargo", "doc", "-p", "sonicterm-resource", "--all-features", "--no-deps"),
         HOSTS, 600, "local", ("rust",), ("linux-core",), env=_RUSTDOC_WARNINGS),
    Step("authored-comments", ("bash", "scripts/check-authored-rust-comments.sh"), HOSTS, 300,
         "local", ("bash",), _CORE_CHECKS),
    Step("no-raw-exit", ("bash", "scripts/check-no-raw-process-exit.sh"), HOSTS, 120, "local",
         ("bash",), _CORE_CHECKS),
    Step("rust-version", ("bash", "scripts/check-rust-version.sh"), HOSTS, 300, "local",
         ("rust", "bash"), _CORE_CHECKS),
    Step("window-owner", ("bash", "scripts/check-window-owner-registration.sh"), HOSTS, 120,
         "local", ("bash",), _CORE_CHECKS),
    Step("workflow-supply-chain", ("bash", "scripts/check-workflow-supply-chain.sh"), HOSTS, 120,
         "local", ("rust", "bash"), _CORE_CHECKS),
    Step("workspace-crates", ("bash", "scripts/check-workspace-crates.sh"), HOSTS, 2100, "local",
         ("rust", "native", "bash"), _CORE_TESTS),
    # After workspace-crates, so the libraries the doctests link are already built.
    Step("doctests", ("cargo", "test", "--workspace", "--doc", "--no-fail-fast"), HOSTS, 900,
         "local", ("rust", "native"), _CORE_TESTS),
    Step("pty-feasibility", ("bash", "scripts/pty-backend-feasibility.sh", "--check"), HOSTS, 300,
         "local", ("rust", "bash"), ("macos-core", "windows-tests")),
    Step("resource-inventory", ("bash", "scripts/test-resource-inventory.sh"), HOSTS, 300, "local",
         ("bash",), ("macos-core", "windows-tests")),
    Step("resource-baseline-tests", ("bash", "scripts/test-resource-baseline-evidence.sh"), HOSTS,
         300, "local", ("bash",), ("macos-core", "windows-tests")),
    Step("soak-harness", ("bash", "scripts/test-soak-harness.sh"), HOSTS, 300, "local",
         ("bash",), ("macos-core", "windows-tests")),
    Step("linux-packages-tests", ("bash", "scripts/test-linux-packages.sh"), HOSTS, 300, "local",
         ("bash",), ("linux-core",)),
    Step("release-assets-tests", ("bash", "scripts/test-release-assets.sh"), HOSTS, 300, "local",
         ("rust", "bash"), ("linux-core",)),
    Step("release-notes-tests", ("bash", "scripts/test-release-notes.sh"), HOSTS, 300, "local",
         ("bash",), _CORE_TESTS),
    Step("wiki-publish-tests", ("bash", "scripts/test-wiki-publish.sh"), HOSTS, 300, "local",
         ("rust", "bash"), _CORE_TESTS),
    # Executed directly, as ci.yml does, so it needs a POSIX host.
    Step("logic-coverage", ("scripts/rust-logic-coverage.sh",), ("macos", "linux"), 1500,
         "local", ("rust", "native", "llvm-cov"), ("macos-coverage",)),
    Step("windows-warp-allocator",
         ("cargo", "test", "-p", "sonicterm-gpu", "--test", "windows_warp_allocator_baseline",
          "--", "--nocapture"),
         ("windows",), 300, "local", ("rust", "native", "warp"), ("windows-tests",)),
    Step("msi-validator-tests", (".\\scripts\\validate-windows-msi_tests.ps1",), ("windows",), 300,
         "local", ("pwsh",), ("windows-tests",), shell="pwsh"),
    Step("release-macos", ("cargo", "build", "--release", "-p", "sonicterm-mac"), ("macos",), 1500,
         "release", ("rust", "native"), ("macos-smoke",)),
    Step("release-windows", ("cargo", "build", "--release", "-p", "sonicterm-windows"),
         ("windows",), 1500, "release", ("rust", "native"), ("windows-smoke",)),
    Step("release-linux", ("cargo", "build", "--release", "-p", "sonicterm-linux"), ("linux",),
         1800, "release", ("rust", "native"), ("linux-packages",)),
)


@dataclass(frozen=True)
class CiOnly:
    """A ci.yml gate invocation that deliberately has no table step, and why."""

    kind: str
    command: str
    jobs: tuple[str, ...]
    reason: str


# `setup` installs dependencies and gates nothing. `evidence-rerun` reruns an
# integration test that the same job's workspace step already runs, only to
# print its report. `runtime-evidence` and `package-evidence` need hosted
# runners, release binaries, or built packages; their scripts' own tests are
# table steps. A self-test can never be listed here.
CI_ONLY_KINDS = ("setup", "evidence-rerun", "runtime-evidence", "package-evidence")

CI_ONLY = (
    CiOnly("setup", ".\\scripts\\setup-windows-cairo.ps1",
           ("windows-native", "windows-checks", "windows-tests", "windows-smoke"),
           "installs the vcpkg Cairo that the native prerequisite names; it checks nothing"),
    CiOnly("evidence-rerun",
           "cargo test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture",
           ("macos-core", "windows-tests"),
           "prints the hosted window-capability report; workspace-crates already runs the test"),
    CiOnly("evidence-rerun",
           "cargo test -p sonicterm-gpu --test ci_adapter_classification_probe -- --nocapture",
           ("macos-core", "windows-tests"),
           "prints the hosted adapter classification; workspace-crates already runs the test"),
    CiOnly("evidence-rerun",
           "cargo test -p sonicterm-gpu --test renderer_churn_baseline -- --nocapture",
           ("macos-core", "windows-tests"),
           "prints the renderer churn baseline on Windows; the target compiles only for Windows, "
           "so on macOS this step and workspace-crates both run no tests"),
    CiOnly("evidence-rerun",
           "cargo test -p sonicterm-app --test windows_software_selection_present -- --nocapture",
           ("windows-tests",),
           "prints the selection-presentation report; workspace-crates already runs the test"),
    CiOnly("runtime-evidence",
           "python scripts/native-smoke-runner.py --timeout-seconds 120 "
           "--log-file \"$env:RUNNER_TEMP\\sonicterm-windows-gdi.log\" --require-capability EXERCISED "
           "-- cargo test -p sonicterm-gpu --test windows_software_present_capability -- --nocapture",
           ("windows-tests",),
           "requires the hosted runner's unique capability=EXERCISED verdict; workspace-crates "
           "runs the same test, which accepts HOST_INCAPABLE"),
    CiOnly("runtime-evidence",
           "python3 scripts/resource-baseline-evidence.py --runner-label macos-14 "
           "--output-dir target/v1.2.0-baseline/evidence-macos-14",
           ("macos-core",),
           "captures real resource evidence under the hosted runner label for upload; "
           "resource-baseline-tests covers the collector"),
    CiOnly("runtime-evidence",
           "\"$python_cmd\" scripts/resource-baseline-evidence.py --runner-label windows-latest "
           "--output-dir target/v1.2.0-baseline/evidence-windows-latest",
           ("windows-tests",),
           "captures real resource evidence under the hosted runner label for upload; "
           "resource-baseline-tests covers the collector"),
    CiOnly("runtime-evidence",
           "python3 scripts/native-smoke-runner.py --timeout-seconds 45 "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-smoke-${{ matrix.arch }}\" "
           "--log-file \"$RUNNER_TEMP/sonicterm-macos-smoke-${{ matrix.arch }}.log\" "
           "-- target/release/sonicterm-mac --runtime-smoke",
           ("macos-smoke",),
           "runs the release-macos binary's native runtime smoke on each hosted architecture"),
    CiOnly("runtime-evidence",
           "python scripts/native-smoke-runner.py --timeout-seconds 45 "
           "--state-dir \"$env:RUNNER_TEMP\\sonicterm-windows-smoke\" "
           "--log-file \"$env:RUNNER_TEMP\\sonicterm-windows-smoke.log\" "
           "-- ./target/release/sonicterm-windows.exe --runtime-smoke",
           ("windows-smoke",),
           "runs the release-windows binary's native runtime smoke on the hosted runner"),
    CiOnly("package-evidence",
           "bash scripts/make-macos-dmg.sh target/release/sonicterm-mac ci mac-${{ matrix.arch }}",
           ("macos-smoke",),
           "packages the release-macos binary; workspace-crates runs the bundle tool tests"),
    CiOnly("package-evidence",
           "python3 scripts/test-macos-package.py "
           "--dmg \"dist/SonicTerm-ci-mac-${{ matrix.arch }}.dmg\" "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-package-${{ matrix.arch }}\" "
           "--max-minimum-macos \"${{ matrix.minimum }}\"",
           ("macos-smoke",),
           "validates the packaged DMG; workspace-crates runs test-macos-package_tests.py"),
    CiOnly("package-evidence",
           "python3 scripts/prepare-release-assets.py check-version --tag \"$tag\"",
           ("linux-packages",),
           "checks the package tag before packaging; release-assets-tests covers the tool"),
    CiOnly("package-evidence",
           "SOURCE_DATE_EPOCH=\"$(git show -s --format=%ct HEAD)\" "
           "bash scripts/make-linux-packages.sh target/release/sonicterm \"$tag\" dist",
           ("linux-packages",),
           "builds the .tar.gz and .deb from the release-linux binary"),
    CiOnly("package-evidence", "bash scripts/test-linux-packages.sh \"$tarball\" \"$deb\"",
           ("linux-packages",),
           "validates the built packages; linux-packages-tests runs the script's own tests"),
    CiOnly("runtime-evidence",
           "bash scripts/smoke-linux-packages.sh \"${{ steps.packages.outputs.tarball }}\" "
           "\"${{ steps.packages.outputs.deb }}\"",
           ("linux-packages",),
           "runs both package layouts on X11/Xvfb and Wayland/Weston with lavapipe"),
)

# A step that invokes a first-party script or one of the gate's Cargo commands.
GATE_INVOCATION = re.compile(
    r"(?:^|[\s\"'/\\.])scripts[/\\]|(?:^|\s)cargo\s+(?:\+\S+\s+)?(?:fmt|clippy|doc|test)\b"
)
# A first-party test entry point: these must be table steps, never CI-only.
SELF_TEST = re.compile(
    r"scripts[/\\][A-Za-z0-9_.-]+_tests\.(?:py|ps1|sh)(?:\s|$)"
    r"|(?:^|\s)bash\s+scripts/test-[A-Za-z0-9_.-]+\.sh$"
)
_CARGO_TEST_TARGET = re.compile(r"(?:^|\s)cargo test -p (\S+) --test (\S+) -- --nocapture(?:\s|$)")
WORKSPACE_TEST_COMMAND = re.compile(
    r"(?m)^cargo test --workspace --lib --bins --tests --no-fail-fast$"
)

_JOB_HEADER = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")
_STEP_START = "      - "
_STEP_KEY = re.compile(r"^        ([A-Za-z0-9_-]+):(?:[ \t]+(.*))?$")


def job_host(job: str) -> str | None:
    """Return the host a platform shard runs on, from its job-name prefix."""
    for host in HOSTS:
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


def _step_commands(lines: list[str]) -> tuple[str, list[str]]:
    """Return one step's label and the logical lines of its `run:` value."""
    label = ""
    commands: list[str] = []
    index = 0
    while index < len(lines):
        match = _STEP_KEY.match(lines[index])
        index += 1
        if match is None:
            continue
        key, value = match.group(1), (match.group(2) or "").strip()
        if key in ("name", "uses") and not label:
            label = value
        if key != "run":
            continue
        if value in ("|", "|-", "|+"):
            block: list[str] = []
            while index < len(lines) and (
                not lines[index].strip() or lines[index].startswith("          ")
            ):
                block.append(lines[index])
                index += 1
            commands.extend(_logical_lines("\n".join(block)))
        elif value.startswith((">", "'", '"')):
            # Folded or quoted run values are unused here; parsing them
            # approximately would let a gate command escape the parity check.
            raise ValueError(f"unsupported run: scalar style in step {label!r}")
        elif value:
            commands.extend(_logical_lines(value))
    return label, commands


def ci_job_commands(workflow: str) -> dict[str, list[tuple[str, str]]]:
    """Map each ci.yml job to its (step label, logical run command) pairs, in file order."""
    if "\njobs:\n" not in workflow:
        raise ValueError("workflow has no top-level jobs mapping")
    body = workflow.split("\njobs:\n", 1)[1].splitlines()
    jobs: dict[str, list[list[str]]] = {}
    current: list[list[str]] | None = None
    for line in body:
        header = _JOB_HEADER.match(line)
        if header:
            current = jobs.setdefault(header.group(1), [])
            continue
        if current is None:
            continue
        if line.startswith(_STEP_START):
            current.append(["        " + line[len(_STEP_START):]])
        elif current and (not line.strip() or line.startswith("        ")):
            current[-1].append(line)
        elif line.strip() and not line.startswith("      "):
            # A job-level key after the steps closes the current step.
            current.append([])
    result: dict[str, list[tuple[str, str]]] = {}
    for job, steps in jobs.items():
        pairs: list[tuple[str, str]] = []
        for step_lines in steps:
            if not step_lines:
                continue
            label, commands = _step_commands(step_lines)
            pairs.extend((label, command) for command in commands)
        result[job] = pairs
    return result


def ci_parity_problems(
    workflow: str, steps: Sequence[Step] = STEPS, entries: Sequence[CiOnly] = CI_ONLY
) -> list[str]:
    """Report every disagreement between the table, the CI-only list, and ci.yml."""
    jobs = ci_job_commands(workflow)
    by_command = {command_text(step): step for step in steps}
    problems: list[str] = []
    for step in steps:
        text = command_text(step)
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
            if not GATE_INVOCATION.search(command):
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


def _test_section_disables(manifest: str, target: str) -> bool:
    """Report whether a [[test]] section turns the target off or gates it on features."""
    for section in re.split(r"(?m)^\[\[test\]\]\s*$", manifest)[1:]:
        body = re.split(r"(?m)^\[", section, maxsplit=1)[0]
        if re.search(rf'(?m)^name\s*=\s*"{re.escape(target)}"', body):
            return bool(re.search(r"(?m)^(?:test\s*=\s*false|required-features\s*=)", body))
    return False


def _rerun_problems(
    root: Path, entry: CiOnly, jobs: Mapping[str, list[tuple[str, str]]], workspace: str
) -> list[str]:
    """Prove that a CI-only cargo test only reruns a target the local workspace step runs."""
    label = f"CI-only {entry.kind} `{entry.command}`"
    match = _CARGO_TEST_TARGET.search(entry.command)
    if match is None:
        return [f"{label}: must name exactly one package and integration test"]
    package, target = match.groups()
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
        if workspace not in [command for _label, command in jobs.get(job, [])]:
            problems.append(f"{label}: {job} does not run `{workspace}`, so nothing runs it locally")
    return problems


def ci_only_problems(
    root: Path, workflow: str, steps: Sequence[Step] = STEPS, entries: Sequence[CiOnly] = CI_ONLY
) -> list[str]:
    """Check that each CI-only entry is reasoned and is not a missing local test."""
    jobs = ci_job_commands(workflow)
    table = {command_text(step) for step in steps}
    workspace_step = next((step for step in steps if step.id == "workspace-crates"), None)
    problems: list[str] = []
    if workspace_step is None:
        problems.append("the table has no workspace-crates step to back CI test reruns")
        workspace = ""
    else:
        workspace = command_text(workspace_step)
    script = root / "scripts" / "check-workspace-crates.sh"
    if not script.is_file() or not WORKSPACE_TEST_COMMAND.search(script.read_text(encoding="utf-8")):
        problems.append("check-workspace-crates.sh no longer runs every workspace test target once")
    for entry in entries:
        label = f"CI-only `{entry.command}`"
        if entry.kind not in CI_ONLY_KINDS:
            problems.append(f"{label}: unknown kind {entry.kind!r}")
        if not entry.reason.strip():
            problems.append(f"{label}: has no reason")
        if not entry.jobs:
            problems.append(f"{label}: names no CI job")
        if entry.command in table:
            problems.append(f"{label}: is already a table step")
        if not GATE_INVOCATION.search(entry.command):
            problems.append(f"{label}: is not a gate invocation, so it needs no entry")
        if SELF_TEST.search(entry.command):
            problems.append(f"{label}: is a first-party self-test; make it a table step")
        if re.search(r"(?:^|\s)cargo test\b", entry.command):
            if entry.kind not in ("evidence-rerun", "runtime-evidence"):
                problems.append(f"{label}: a cargo test run in CI only is a missing local test")
            problems.extend(_rerun_problems(root, entry, jobs, workspace))
        elif entry.kind == "evidence-rerun":
            problems.append(f"{label}: an evidence rerun must rerun a cargo test target")
    return problems


GATE_BEGIN = "<!-- local-gate:begin -->"
GATE_END = "<!-- local-gate:end -->"
INVOCATION = "python3 scripts/local-gate.py"
DOCUMENTS = (
    ("CLAUDE.md", "en"),
    ("wiki/Development-and-Release.md", "en"),
    ("wiki/Development-and-Release-zh-CN.md", "zh-CN"),
)

_RENDER_TEXT = {
    "en": {
        "header": ("Step", "Command", "Hosts", "Class", "Needs", "CI jobs"),
        "joiner": ", ",
        "classes": "Classes: `local` steps run by default; `release` steps run with "
        "`--with-release`; `optional` steps run with `--with-optional` and never run in CI.",
        "needs": "Needs:",
        "always": "Every step also needs Git and Python 3 on `PATH`.",
        "colon": ": ",
        "index": 0,
    },
    "zh-CN": {
        "header": ("步骤", "命令", "主机", "类别", "前置条件", "CI job"),
        "joiner": "、",
        "classes": "类别：`local` 步骤默认运行；`release` 步骤需加 `--with-release`；"
        "`optional` 步骤需加 `--with-optional`，且从不在 CI 中运行。",
        "needs": "前置条件：",
        "always": "每个步骤还需要 `PATH` 上的 Git 与 Python 3。",
        "colon": "：",
        "index": 1,
    },
}


def render_gate_block(language: str, steps: Sequence[Step] = STEPS) -> str:
    """Render the table block that CLAUDE.md and the wiki embed between the gate markers."""
    text = _RENDER_TEXT[language]
    joiner = text["joiner"]
    header = text["header"]
    lines = [GATE_BEGIN, "", "| " + " | ".join(header) + " |", "|" + " --- |" * len(header)]
    for step in steps:
        hosts = joiner.join(HOST_LABELS[host] for host in step.hosts)
        needs = joiner.join(f"`{key}`" for key in step.prerequisites)
        jobs = joiner.join(f"`{job}`" for job in step.ci_jobs) or "—"
        lines.append(
            f"| `{step.id}` | `{command_text(step)}` | {hosts} | `{step.evidence}` | {needs} | {jobs} |"
        )
    lines.extend(["", text["classes"], "", text["needs"], ""])
    used = {key for step in steps for key in step.prerequisites}
    for key in PREREQUISITES:
        if key in used:
            lines.append(f"- `{key}`{text['colon']}{PREREQUISITES[key][text['index']]}")
    lines.extend([f"- {text['always']}", "", GATE_END])
    return "\n".join(lines)


def doc_parity_problems(
    name: str, text: str, language: str, steps: Sequence[Step] = STEPS
) -> list[str]:
    """Compare one document's gate block with the table's rendered form."""
    if text.count(GATE_BEGIN) != 1 or text.count(GATE_END) != 1:
        return [f"{name}: expected exactly one {GATE_BEGIN} ... {GATE_END} block"]
    start = text.index(GATE_BEGIN)
    end = text.index(GATE_END) + len(GATE_END)
    if end < start:
        return [f"{name}: {GATE_END} precedes {GATE_BEGIN}"]
    problems: list[str] = []
    actual = text[start:end]
    expected = render_gate_block(language, steps)
    if actual != expected:
        diff = difflib.unified_diff(
            actual.splitlines(), expected.splitlines(), f"{name} (documented)",
            f"local-gate.py --render {language}", lineterm="",
        )
        problems.append(f"{name}: gate block differs from the table:\n" + "\n".join(diff))
    if INVOCATION not in text:
        problems.append(f"{name}: does not show the `{INVOCATION}` invocation")
    return problems


def resolve_program(program: str, root: Path, env: Mapping[str, str]) -> str | None:
    """Find a step's executable: a path relative to the repository root, or a name on PATH."""
    if "/" in program or "\\" in program:
        candidate = Path(program)
        if not candidate.is_absolute():
            candidate = root / candidate
        if candidate.is_file() and (os.name == "nt" or os.access(candidate, os.X_OK)):
            return str(candidate)
        return None
    return shutil.which(program, path=env.get("PATH"))


@dataclass(frozen=True)
class StepResult:
    """The verdict, exit status, duration, and log of one executed step."""

    id: str
    status: str
    exit_code: int | None
    elapsed_s: float
    log_path: Path
    detail: str = ""


def _copy_output(pipe, log) -> None:
    """Stream a step's combined output into its log as it arrives, so the log can be tailed."""
    try:
        while True:
            chunk = pipe.read1(65536)
            if not chunk:
                return
            log.write(chunk)
            log.flush()
    except (OSError, ValueError):
        # The pipe or log closed under a killed or detached step; the
        # result line already records why the output ends here.
        return


def _write_header(log, step: Step, argv: Sequence[str], root: Path) -> None:
    lines = [
        f"[local-gate] step={step.id} evidence={step.evidence} timeout={step.timeout_s}s",
        f"[local-gate] command={command_text(step)}",
        f"[local-gate] argv={json.dumps(list(argv))}",
        f"[local-gate] cwd={root}",
    ]
    log.write(("\n".join(lines) + "\n\n").encode("utf-8", errors="replace"))
    log.flush()


def _finish(log, step: Step, log_path: Path, started: float, status: str,
            exit_code: int | None, detail: str) -> StepResult:
    elapsed = time.monotonic() - started
    footer = f"\n[local-gate] result={status} exit={_exit_text(exit_code)} elapsed={elapsed:.1f}s"
    if detail:
        footer += f" detail={detail}"
    try:
        log.write((footer + "\n").encode("utf-8", errors="replace"))
        log.flush()
    except (OSError, ValueError):
        # The verdict still stands when the log cannot take its footer.
        pass
    return StepResult(step.id, status, exit_code, elapsed, log_path, detail)


def _reap(process: subprocess.Popen[bytes], reader: threading.Thread) -> str:
    """Collect a killed step's status and let its output drain, without blocking forever."""
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass
    reader.join(5)
    if reader.is_alive():
        # A descendant that left the process group still holds the pipe;
        # the reader is a daemon thread, so it cannot keep the gate alive.
        return "; a descendant outside the process group still holds the output pipe"
    _close(process.stdout)
    return ""


def _close(pipe) -> None:
    if pipe is not None:
        try:
            pipe.close()
        except OSError:
            pass


def _exit_text(exit_code: int | None) -> str:
    return "-" if exit_code is None else str(exit_code)


def run_step(step: Step, index: int, root: Path, log_dir: Path,
             environ: Mapping[str, str]) -> StepResult:
    """Run one step in its own process group under a whole-tree deadline, logging its output."""
    log_path = log_dir / f"{index:02d}-{step.id}.log"
    env = dict(environ)
    env.update(step.env)
    argv = launch_argv(step)
    started = time.monotonic()
    with log_path.open("wb") as log:
        _write_header(log, step, argv, root)
        program = resolve_program(argv[0], root, env)
        if program is None:
            return _finish(log, step, log_path, started, LAUNCH, None,
                           f"cannot find {argv[0]} under the repository root or on PATH")
        try:
            process = subprocess.Popen(
                [program, *argv[1:]],
                cwd=str(root),
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                **SMOKE_RUNNER.process_group_options(),
            )
        except OSError as error:
            return _finish(log, step, log_path, started, LAUNCH, None, f"launch failed: {error}")
        reader = threading.Thread(target=_copy_output, args=(process.stdout, log), daemon=True)
        reader.start()
        deadline = started + step.timeout_s
        timed_out = False
        try:
            try:
                process.wait(timeout=max(0.0, deadline - time.monotonic()))
                # The step ends when every process holding its output has exited,
                # so a descendant that outlives the leader is still under the deadline.
                reader.join(max(0.0, deadline - time.monotonic()))
                timed_out = reader.is_alive()
            except subprocess.TimeoutExpired:
                timed_out = True
        except KeyboardInterrupt:
            SMOKE_RUNNER.terminate_process_tree(process)
            detail = _reap(process, reader)
            return _finish(log, step, log_path, started, INTERRUPTED, process.returncode,
                           "interrupted; process tree killed" + detail)
        if timed_out:
            SMOKE_RUNNER.terminate_process_tree(process)
            detail = _reap(process, reader)
            return _finish(log, step, log_path, started, TIMEOUT, process.returncode,
                           f"deadline of {step.timeout_s}s reached; process tree killed{detail}")
        _close(process.stdout)
        status = PASS if process.returncode == 0 else FAIL
        return _finish(log, step, log_path, started, status, process.returncode, "")


@dataclass(frozen=True)
class GitSnapshot:
    """HEAD, the index, and a fingerprint of every changed or untracked path."""

    available: bool
    head: str | None = None
    index_digest: str | None = None
    # (path, porcelain status, content fingerprint), sorted by path.
    entries: tuple[tuple[str, str, str], ...] = ()
    error: str = ""


def _git(root: Path, *args: str) -> subprocess.CompletedProcess[bytes]:
    # --no-optional-locks keeps `git status` from rewriting the index it inspects.
    return subprocess.run(
        ["git", "--no-optional-locks", "-C", str(root), *args],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=GIT_TIMEOUT_S,
    )


def _first_line(data: bytes) -> str:
    lines = data.decode("utf-8", errors="replace").strip().splitlines()
    return lines[0] if lines else ""


def _porcelain_entries(raw: bytes) -> list[tuple[str, str]]:
    """Parse `git status --porcelain=v1 -z` into (status, path) pairs."""
    fields = raw.split(b"\0")
    entries: list[tuple[str, str]] = []
    index = 0
    while index < len(fields):
        field = fields[index]
        index += 1
        if len(field) < 4:
            continue
        code = field[:2].decode("ascii", errors="replace")
        entries.append((code, os.fsdecode(field[3:])))
        if "R" in code or "C" in code:
            # The original path of a rename or copy follows as its own field.
            index += 1
    return entries


def _fingerprint(path: Path) -> str:
    """Identify a path's current content so a further edit to a dirty file is visible."""
    try:
        if path.is_symlink():
            return "link:" + os.readlink(path)
        if path.is_dir():
            return "dir"
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1 << 20), b""):
                digest.update(chunk)
        return "sha256:" + digest.hexdigest()
    except FileNotFoundError:
        return "missing"
    except OSError as error:
        return f"unreadable:{error.errno}"


def _is_within(path: Path, parent: Path) -> bool:
    try:
        path.resolve().relative_to(parent.resolve())
    except (OSError, ValueError):
        return False
    return True


def snapshot_git(root: Path, exclude: Path | None = None) -> GitSnapshot:
    """Record tracked and untracked Git state without modifying the tree or the index."""
    try:
        inside = _git(root, "rev-parse", "--is-inside-work-tree")
        if inside.returncode != 0 or inside.stdout.strip() != b"true":
            return GitSnapshot(False, error=_first_line(inside.stderr) or "not a Git work tree")
        top = _git(root, "rev-parse", "--show-toplevel")
        head = _git(root, "rev-parse", "--verify", "--quiet", "HEAD")
        index = _git(root, "ls-files", "--stage", "-z")
        status = _git(root, "status", "--porcelain=v1", "-z", "--untracked-files=all")
    except (OSError, subprocess.TimeoutExpired) as error:
        return GitSnapshot(False, error=f"git failed: {error}")
    for completed in (top, index, status):
        if completed.returncode != 0:
            return GitSnapshot(False, error="git failed: " + _first_line(completed.stderr))
    toplevel = Path(os.fsdecode(top.stdout.strip()))
    entries = []
    for code, path in _porcelain_entries(status.stdout):
        absolute = toplevel / path
        if exclude is not None and _is_within(absolute, exclude):
            continue
        entries.append((path, code, _fingerprint(absolute)))
    return GitSnapshot(
        available=True,
        head=head.stdout.strip().decode("ascii", errors="replace") if head.returncode == 0 else None,
        index_digest=hashlib.sha256(index.stdout).hexdigest(),
        entries=tuple(sorted(entries)),
    )


def _display_path(path: str) -> str:
    return path.encode("utf-8", errors="surrogateescape").decode("utf-8", errors="backslashreplace")


def describe_changes(before: GitSnapshot, after: GitSnapshot) -> list[str]:
    """List every difference between two snapshots; an empty list means the run changed nothing."""
    if not before.available and not after.available:
        return []
    if before.available != after.available:
        return [f"Git state changed availability during the run: {after.error or 'now available'}"]
    changes: list[str] = []
    if before.head != after.head:
        changes.append(f"HEAD moved: {before.head} -> {after.head}")
    if before.index_digest != after.index_digest:
        changes.append("the index changed")
    old = {path: (code, fingerprint) for path, code, fingerprint in before.entries}
    new = {path: (code, fingerprint) for path, code, fingerprint in after.entries}

    def state(entry: tuple[str, str] | None) -> str:
        if entry is None:
            return "clean"
        return f"{entry[0].strip() or '-'} {entry[1][:19]}"

    for path in sorted(set(old) | set(new)):
        if old.get(path) != new.get(path):
            changes.append(f"{_display_path(path)}: {state(old.get(path))} -> {state(new.get(path))}")
    return changes


@dataclass(frozen=True)
class GateReport:
    """Everything one gate run observed, from which the summary and exit status derive."""

    host: str
    results: tuple[StepResult, ...]
    before: GitSnapshot
    after: GitSnapshot
    changes: tuple[str, ...]
    log_dir: Path
    interrupted: bool = False

    @property
    def exit_code(self) -> int:
        """Return 130 when interrupted, 1 when any step failed or the tree changed, else 0."""
        if self.interrupted:
            return 130
        if self.changes or any(result.status != PASS for result in self.results):
            return 1
        return 0


def summary_lines(report: GateReport) -> list[str]:
    """Render the human summary that is printed and written to summary.txt."""
    counts = {status: 0 for status in (PASS, FAIL, TIMEOUT, LAUNCH, INTERRUPTED)}
    for result in report.results:
        counts[result.status] += 1
    lines = [
        f"[local-gate] summary host={report.host} steps={len(report.results)} "
        f"passed={counts[PASS]} failed={counts[FAIL]} timed_out={counts[TIMEOUT]} "
        f"launch_errors={counts[LAUNCH]} interrupted={counts[INTERRUPTED]}"
    ]
    for result in report.results:
        line = (
            f"  {result.status:<11} {result.id:<24} {result.elapsed_s:8.1f}s "
            f"exit={_exit_text(result.exit_code):<4} log={result.log_path.name}"
        )
        if result.detail:
            line += f" ({result.detail})"
        lines.append(line)
    if not report.before.available:
        lines.append(f"[local-gate] Git state unavailable: {report.before.error}")
    elif report.before.entries:
        lines.append(
            f"[local-gate] pre-existing changes, present before the run and not caused by it "
            f"({len(report.before.entries)}):"
        )
        for path, code, _fingerprint in report.before.entries[:PRE_EXISTING_LIMIT]:
            lines.append(f"  {code} {_display_path(path)}")
        hidden = len(report.before.entries) - PRE_EXISTING_LIMIT
        if hidden > 0:
            lines.append(f"  ... and {hidden} more (see summary.json)")
    else:
        lines.append("[local-gate] the tree was clean before the run")
    if report.changes:
        lines.append(f"[local-gate] the run changed the tree ({len(report.changes)}); nothing was reverted:")
        lines.extend(f"  {change}" for change in report.changes)
    elif report.before.available:
        lines.append("[local-gate] the run left tracked and untracked state unchanged")
    if report.interrupted:
        verdict = "INTERRUPTED"
    else:
        verdict = "PASS" if report.exit_code == 0 else "FAIL"
    lines.append(f"[local-gate] verdict={verdict} exit={report.exit_code} logs={report.log_dir}")
    return lines


def summary_json(report: GateReport, steps: Sequence[Step]) -> dict[str, object]:
    """Return the machine-readable summary written to summary.json."""
    by_id = {step.id: step for step in steps}
    return {
        "host": report.host,
        "exit_code": report.exit_code,
        "interrupted": report.interrupted,
        "steps": [
            {
                "id": result.id,
                "command": command_text(by_id[result.id]) if result.id in by_id else None,
                "status": result.status,
                "exit_code": result.exit_code,
                "elapsed_s": round(result.elapsed_s, 3),
                "timeout_s": by_id[result.id].timeout_s if result.id in by_id else None,
                "log": str(result.log_path),
                "detail": result.detail,
            }
            for result in report.results
        ],
        "git": {
            "available": report.before.available,
            "error": report.before.error,
            "head_before": report.before.head,
            "head_after": report.after.head,
            "pre_existing": [
                {"path": _display_path(path), "status": code}
                for path, code, _fingerprint in report.before.entries
            ],
            "changes": list(report.changes),
        },
    }


def _say(out: TextIO, text: str) -> None:
    print(text, file=out, flush=True)


def _print_tail(out: TextIO, log_path: Path) -> None:
    try:
        data = log_path.read_bytes()
    except OSError:
        return
    for line in data.decode("utf-8", errors="replace").splitlines()[-TAIL_LINES:]:
        _say(out, f"    | {line}")


def run_gate(steps: Sequence[Step], root: Path, log_dir: Path, host: str, *,
             environ: Mapping[str, str] | None = None,
             console: TextIO | None = None) -> GateReport:
    """Run every step in order, record Git state around the run, and write the summaries."""
    out = console if console is not None else sys.stdout
    env = dict(os.environ if environ is None else environ)
    log_dir.mkdir(parents=True, exist_ok=True)
    _say(out, f"[local-gate] host={host} steps={len(steps)} root={root}")
    _say(out, f"[local-gate] logs={log_dir}")
    before = snapshot_git(root, exclude=log_dir)
    results: list[StepResult] = []
    interrupted = False
    for index, step in enumerate(steps, 1):
        _say(out, f"[local-gate] start {step.id} timeout={step.timeout_s}s: {command_text(step)}")
        result = run_step(step, index, root, log_dir, env)
        results.append(result)
        _say(out, f"[local-gate] finish {step.id} result={result.status} "
                  f"exit={_exit_text(result.exit_code)} elapsed={result.elapsed_s:.1f}s")
        if result.status != PASS:
            _print_tail(out, result.log_path)
        if result.status == INTERRUPTED:
            interrupted = True
            break
    after = snapshot_git(root, exclude=log_dir)
    report = GateReport(host, tuple(results), before, after,
                        tuple(describe_changes(before, after)), log_dir, interrupted)
    lines = summary_lines(report)
    (log_dir / "summary.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (log_dir / "summary.json").write_text(
        json.dumps(summary_json(report, steps), indent=2) + "\n", encoding="utf-8"
    )
    for line in lines:
        _say(out, line)
    return report


def detect_host(system: str | None = None) -> str | None:
    """Map platform.system() to a table host, or None for an unsupported host."""
    name = platform.system() if system is None else system
    if name == "Darwin":
        return "macos"
    if name == "Linux":
        return "linux"
    if name == "Windows" or name.upper().startswith(("CYGWIN", "MSYS", "MINGW")):
        return "windows"
    return None


def select_steps(host: str, *, release: bool = False, optional: bool = False,
                 ids: Sequence[str] = (), steps: Sequence[Step] = STEPS) -> list[Step]:
    """Choose the host's steps in table order, by class flags or by explicit ids."""
    known = {step.id for step in steps}
    unknown = [name for name in ids if name not in known]
    if unknown:
        raise ValueError("unknown step id(s): " + ", ".join(unknown))
    if ids:
        chosen = [step for step in steps if step.id in set(ids)]
        foreign = [step.id for step in chosen if host not in step.hosts]
        if foreign:
            raise ValueError(f"step(s) not defined for {host}: " + ", ".join(foreign))
        return chosen
    classes = {"local"}
    if release:
        classes.add("release")
    if optional:
        classes.add("optional")
    return [step for step in steps if host in step.hosts and step.evidence in classes]


def format_listing(steps: Sequence[Step], host: str) -> str:
    """Describe the selected steps with their deadlines, prerequisites, and CI jobs."""
    lines = [f"[local-gate] {len(steps)} step(s) for {HOST_LABELS[host]}:"]
    for step in steps:
        lines.append(
            f"  {step.id} [{step.evidence}] timeout={step.timeout_s}s "
            f"needs={','.join(step.prerequisites)} ci={','.join(step.ci_jobs) or '-'}"
        )
        lines.append(f"      {command_text(step)}")
    return "\n".join(lines)


def main(argv: Sequence[str] | None = None) -> int:
    """Parse the command line, then list, render, or run the selected steps."""
    parser = argparse.ArgumentParser(description="Run SonicTerm's local verification gate.")
    parser.add_argument("--root", type=Path, default=_HERE.parent,
                        help="repository root (default: this script's repository)")
    parser.add_argument("--host", choices=HOSTS,
                        help="show another host's steps; only valid with --list")
    parser.add_argument("--with-release", action="store_true",
                        help="also run the host's release-preparation steps")
    parser.add_argument("--with-optional", action="store_true",
                        help="also run the host's optional pre-push steps")
    parser.add_argument("--step", action="append", default=[], metavar="ID",
                        help="run only this step; repeat to run several, in table order")
    parser.add_argument("--log-dir", type=Path,
                        help="directory for per-step logs and summaries (default: a new temp dir)")
    parser.add_argument("--list", action="store_true", help="print the selected steps and exit")
    parser.add_argument("--render", choices=tuple(_RENDER_TEXT),
                        help="print the documented gate block in a language and exit")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.render:
        # UTF-8 bytes, so the Chinese block prints on consoles with a legacy code page.
        sys.stdout.buffer.write((render_gate_block(args.render) + "\n").encode("utf-8"))
        sys.stdout.flush()
        return 0
    detected = detect_host()
    host = args.host or detected
    if host is None:
        parser.error(f"unsupported host {platform.system()!r}; use --host with --list")
    if args.host and args.host != detected and not args.list:
        parser.error("--host shows another host's table and requires --list")
    try:
        steps = select_steps(host, release=args.with_release, optional=args.with_optional,
                             ids=args.step)
    except ValueError as error:
        parser.error(str(error))
    if args.list:
        print(format_listing(steps, host), flush=True)
        return 0
    root = args.root.resolve()
    log_dir = (args.log_dir.resolve() if args.log_dir
               else Path(tempfile.mkdtemp(prefix="sonicterm-local-gate-")))
    try:
        return run_gate(steps, root, log_dir, host).exit_code
    except KeyboardInterrupt:
        print("[local-gate] interrupted", file=sys.stderr, flush=True)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
