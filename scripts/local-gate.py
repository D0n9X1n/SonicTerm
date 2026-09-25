#!/usr/bin/env python3
"""Run SonicTerm's local verification gate from one declarative, host-aware step table.

STEPS is the only definition of the local gate. CLAUDE.md and both
Development-and-Release wiki files embed its rendered form, and ci.yml keeps
explicit steps that local-gate_tests.py checks against it, so a gate command
edited in one place alone fails a test.

The runner executes the steps selected for the current host in table order. Each
step runs in its own process group under a deadline that kills that group,
reusing the native smoke runner's launch and tree-kill logic, and later steps
still run after a failure, a timeout, or a launch error. On POSIX a step also
fails when members of its process group outlive the leader by LEFTOVER_GRACE_S;
they are killed and counted while the unreaped leader still pins the group id.
A child that leaves the group, such as through setsid, is outside both bounds.
Tracked and untracked Git state, including file modes, is recorded before and
after the run; changes are reported, never cleaned.
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
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
from typing import Iterable, Mapping, Sequence, TextIO

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
# How long a POSIX step's process group may outlive its leader before its
# remaining members are killed and the step fails.
LEFTOVER_GRACE_S = 2.0
PS_TIMEOUT_S = 10
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
    "win-target": (
        "the `x86_64-pc-windows-msvc` standard library (`rustup target add x86_64-pc-windows-msvc`).",
        "`x86_64-pc-windows-msvc` 标准库（`rustup target add x86_64-pc-windows-msvc`）。",
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
    Step("windows-target", ("bash", "scripts/check-windows-target.sh"), ("macos",), 600,
         "optional", ("rust", "win-target", "bash"), ()),
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

# Cargo's built-in aliases for gate subcommands, and the subcommands a gate runs.
_CARGO_ALIASES = {"t": "test", "d": "doc"}
_CARGO_GATES = ("fmt", "clippy", "doc", "test")
_INTERPRETERS = ("bash", "sh", "python", "python3", "py", "pwsh", "powershell")
_ASSIGNMENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*=.*", re.S)
_VARIABLE_WORD = re.compile(r"\$\{?[A-Za-z_][A-Za-z0-9_]*\}?")
_SCRIPT_PATH = re.compile(r"(?:\./)*scripts/([A-Za-z0-9_.-]+)")
_SCRIPT_MENTION = re.compile(r"(?<![\w-])scripts[/\\]")
# A loose scan for text the tokenizer cannot split or does not model: `cargo`
# followed by a gate subcommand or alias, or a first-party script path.
_GATE_MENTION = re.compile(
    r"(?<![\w.-])cargo(?![\w-]).*?(?<![\w-])(?:fmt|clippy|doc|test|t|d)(?![\w-])"
    r"|(?<![\w-])scripts[/\\]",
    re.S,
)
# The runner substitutes GitHub expressions before the shell reads the line.
_EXPRESSION = re.compile(r"\$\{\{.*?\}\}", re.S)
_EXPRESSION_WORD = "\x00expression\x00"
# A gate beside one of these can run without its exit status failing the step.
_UNMODELED_OPERATORS = ("|", "|&", "||", "&", "(", ")")
# A first-party test entry point: these must be table steps, never CI-only.
_SELF_TEST_NAME = re.compile(r"[A-Za-z0-9_.-]+_tests\.(?:py|ps1|sh)")
_BARE_TEST_NAME = re.compile(r"test-[A-Za-z0-9_.-]+\.(?:py|ps1|sh)")
_CARGO_NAME = re.compile(r"[A-Za-z0-9_-]+")
WORKSPACE_TEST_COMMAND = re.compile(
    r"(?m)^cargo test --workspace --lib --bins --tests --no-fail-fast$"
)


@dataclass(frozen=True)
class Invocation:
    """One gate command after spelling normalization: a cargo gate subcommand or a first-party script."""

    # "cargo" or "script".
    kind: str
    # The cargo subcommand with aliases resolved, or the script's file name under scripts/.
    name: str
    args: tuple[str, ...]
    env: tuple[str, ...] = ()
    toolchain: str | None = None


class _Unsupported(ValueError):
    """A shell spelling the gate classifier does not model."""


_WORD_END = frozenset(" \t;&|()<>")
# Unquoted, bash reads a backslash before these as an escape and PowerShell as a
# literal, so the line means different things to the two shells.
_AMBIGUOUS_ESCAPE = frozenset(" \t\"'$`#;&|()<>\\")


def _substitution_span(line: str, index: int) -> int:
    """Return the index just past a `$(...)` or backtick substitution that starts at index."""
    if line[index] == "`":
        end = line.find("`", index + 1)
        if end < 0:
            raise _Unsupported("an unterminated backtick")
        return end + 1
    depth = 1
    quote = ""
    index += 2
    while index < len(line):
        char = line[index]
        if quote:
            if char == "\\" and quote == '"':
                index += 1
            elif char == quote:
                quote = ""
        elif char in "'\"":
            quote = char
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    raise _Unsupported("an unterminated command substitution")


def _read_word(line: str, index: int) -> tuple[str, bool, bool, int]:
    """Read one shell word from index.

    Returns the word with its quotes removed, whether any part was quoted, whether it
    holds a command substitution, and the index just past it.
    """
    parts: list[str] = []
    quoted = substitutes = False
    while index < len(line) and line[index] not in _WORD_END:
        char = line[index]
        if char == "'":
            end = line.find("'", index + 1)
            if end < 0:
                raise _Unsupported("an unterminated single quote")
            parts.append(line[index + 1:end])
            quoted, index = True, end + 1
        elif char == '"':
            quoted, index = True, index + 1
            while True:
                if index >= len(line):
                    raise _Unsupported("an unterminated double quote")
                char = line[index]
                if char == '"':
                    index += 1
                    break
                if char == "\\" and line[index + 1:index + 2] in ('"', "\\", "$", "`"):
                    parts.append(line[index + 1])
                    index += 2
                elif char == "`" or line.startswith("$(", index):
                    end = _substitution_span(line, index)
                    parts.append(line[index:end])
                    substitutes, index = True, end
                else:
                    parts.append(char)
                    index += 1
        elif char == "`" or line.startswith("$(", index):
            end = _substitution_span(line, index)
            parts.append(line[index:end])
            substitutes, index = True, end
        elif char == "\\" and (index + 1 >= len(line) or line[index + 1] in _AMBIGUOUS_ESCAPE):
            raise _Unsupported("a backslash escape, which bash and PowerShell read differently")
        else:
            parts.append(char)
            index += 1
    return "".join(parts), quoted, substitutes, index


def _tokens(line: str) -> list[tuple[str, str, bool]]:
    """Split one logical shell line into ("word", text, substitutes) and ("op", text, False) tokens."""
    tokens: list[tuple[str, str, bool]] = []
    index = 0
    while index < len(line):
        char = line[index]
        if char in " \t":
            index += 1
        elif char == "#":
            # A word that starts with `#` begins a comment in bash and PowerShell.
            break
        elif char in "<>" or line.startswith("&>", index):
            index += 2 if char == "&" else 1
            if line[index:index + 1] in ("<", ">", "&"):
                index += 1
            tokens.append(("op", "redirect", False))
        elif char in ";&|()":
            pair = line[index:index + 2]
            text = pair if pair in ("&&", "||", "|&", ";;") else char
            index += len(text)
            tokens.append(("op", text, False))
        else:
            word, quoted, substitutes, end = _read_word(line, index)
            index = end
            if not quoted and word.isdigit() and line[end:end + 1] in ("<", ">"):
                # A file-descriptor number belongs to the redirect after it.
                continue
            tokens.append(("word", word, substitutes))
    return tokens


def _simple_commands(
    tokens: Sequence[tuple[str, str, bool]]
) -> list[tuple[tuple[tuple[str, bool], ...], str, str, bool]]:
    """Group tokens into (words, operator before, operator after, redirected) simple commands."""
    commands = []
    words: list[tuple[str, bool]] = []
    before = ""
    redirected = False
    for kind, text, substitutes in tokens:
        if kind == "word":
            words.append((text, substitutes))
        elif text == "redirect":
            redirected = True
        else:
            commands.append((tuple(words), before, text, redirected))
            words, before, redirected = [], text, False
    commands.append((tuple(words), before, "", redirected))
    return [command for command in commands if command[0]]


def _script_name(word: str) -> str | None:
    """Return the file name of a first-party scripts/ path in either separator style, or None."""
    match = _SCRIPT_PATH.fullmatch(word.replace("\\", "/"))
    return match.group(1) if match else None


def _program_name(word: str) -> str:
    """Return a command word's lowercase basename without a Windows `.exe` suffix."""
    name = re.split(r"[/\\]", word)[-1].lower()
    return name[:-4] if name.endswith(".exe") else name


def _gate_word(word: str) -> bool:
    """Report whether a word is cargo itself or names a first-party script path."""
    return _program_name(word) == "cargo" or bool(_SCRIPT_MENTION.search(word))


def _classify_cargo(
    args: tuple[str, ...], env: tuple[str, ...]
) -> tuple[list[Invocation], list[str]]:
    """Classify a cargo command, normalizing a `+toolchain` override and the gate aliases."""
    toolchain = None
    if args and args[0].startswith("+"):
        toolchain, args = args[0], args[1:]
    if not args:
        return [], []
    if _EXPRESSION_WORD in args[0]:
        return [], ["a cargo subcommand that a workflow expression supplies"]
    subcommand = _CARGO_ALIASES.get(args[0], args[0])
    if subcommand in _CARGO_GATES:
        return [Invocation("cargo", subcommand, args[1:], env, toolchain)], []
    if args[0].startswith("-"):
        # After a leading option, any later gate word or expression may be the subcommand.
        if any(_CARGO_ALIASES.get(word, word) in _CARGO_GATES for word in args):
            return [], ["a cargo option before the gate subcommand"]
        if any(_EXPRESSION_WORD in word for word in args):
            return [], ["a cargo option before a subcommand that a workflow expression may supply"]
    return [], []


def _classify_script(
    name: str, args: tuple[str, ...], env: tuple[str, ...]
) -> tuple[list[Invocation], list[str]]:
    """Classify a first-party script and the command it runs after a `--` separator, if any."""
    head, nested = args, ()
    if "--" in args:
        split = args.index("--")
        head, nested = args[:split], args[split + 1:]
    invocations = [Invocation("script", name, args, env)]
    reasons = []
    if any(_gate_word(word) for word in head):
        reasons.append(f"cargo or a script path as an argument to scripts/{name}")
    if nested:
        found, why = _classify_words(tuple((word, False) for word in nested))
        invocations.extend(found)
        reasons.extend(why)
    return invocations, reasons


def _classify_words(words: Sequence[tuple[str, bool]]) -> tuple[list[Invocation], list[str]]:
    """Classify one simple command's words, after its leading variable assignments."""
    reasons = [
        "a gate inside a command substitution"
        for text, substitutes in words if substitutes and _GATE_MENTION.search(text)
    ]
    index = 0
    while index < len(words) and _ASSIGNMENT.fullmatch(words[index][0]):
        index += 1
    env = tuple(text for text, _substitutes in words[:index])
    rest = [text for text, _substitutes in words[index:]]
    if not rest:
        return [], reasons
    program, args = rest[0], tuple(rest[1:])
    if _EXPRESSION_WORD in program:
        return [], reasons + ["a command word that a workflow expression supplies"]
    name = _program_name(program)
    if name == "cargo":
        found, why = _classify_cargo(args, env)
        return found, reasons + why
    script = _script_name(program)
    if script is None and args and (name in _INTERPRETERS or _VARIABLE_WORD.fullmatch(program)):
        # An interpreter, or a variable holding one, runs the script path it is given first.
        if _EXPRESSION_WORD in args[0] or (
            args[0].startswith("-") and any(_EXPRESSION_WORD in word for word in args)
        ):
            # The expression may be the script itself, or follow options that make it the script.
            return [], reasons + [f"a script argument to `{program}` that a workflow expression may supply"]
        script = _script_name(args[0])
        args = args[1:] if script is not None else args
    if script is not None:
        found, why = _classify_script(script, args, env)
        return found, reasons + why
    if any(_gate_word(word) for word in rest) or _GATE_MENTION.search(" ".join(rest)):
        reasons.append(f"a gate after `{program}`, which the classifier does not model as a command")
    return [], reasons


def classify_command(line: str) -> tuple[list[Invocation], list[str]]:
    """Normalize one logical run line into its gate invocations and the spellings it cannot model.

    Every simple command joined by `&&` or `;` is classified. A gate the classifier cannot
    prove runs with its exit status observed, such as one behind a pipe, `||`, a wrapper
    command, or a command substitution, yields a reason and is never skipped.
    """
    try:
        tokens = _tokens(_EXPRESSION.sub(_EXPRESSION_WORD, line))
    except _Unsupported as error:
        return [], [str(error)] if _GATE_MENTION.search(line) else []
    invocations: list[Invocation] = []
    reasons: list[str] = []
    for words, before, after, redirected in _simple_commands(tokens):
        found, why = _classify_words(words)
        invocations.extend(found)
        reasons.extend(why)
        if not (found or why):
            continue
        operators = [operator for operator in (before, after) if operator in _UNMODELED_OPERATORS]
        if operators:
            reasons.append(f"a gate beside `{operators[0]}`, which can hide its exit status")
        if redirected:
            reasons.append("a redirected gate")
    return invocations, reasons


def is_self_test(invocation: Invocation) -> bool:
    """Report whether an invocation is a first-party test entry point, which must be a table step."""
    if invocation.kind != "script":
        return False
    return bool(
        _SELF_TEST_NAME.fullmatch(invocation.name)
        or (_BARE_TEST_NAME.fullmatch(invocation.name) and not invocation.args)
    )


_KEY = re.compile(r"([A-Za-z0-9_-]+):(?:[ \t]+(.*))?")
_JOB_ID = re.compile(r"[A-Za-z_][A-Za-z0-9_-]*")
_JOBS_KEY = re.compile(r"jobs:[ \t]*(?:#.*)?")
_BLOCK_HEADER = re.compile(r"\|[-+]?(?:[ \t]+#.*)?")
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
        match = _KEY.fullmatch(stripped)
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
            if not _BLOCK_HEADER.fullmatch(value):
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
    starts = [number for number, line in enumerate(lines) if _JOBS_KEY.fullmatch(line)]
    if len(starts) != 1:
        raise ValueError("workflow must have exactly one top-level jobs mapping")
    for number, line in enumerate(lines):
        if not line.strip() or line[0] in " #":
            continue
        # Every top-level key is checked, before or after jobs, so a quoted, flow, or
        # complex key cannot hide a mapping that applies to every job.
        match = _KEY.fullmatch(line)
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
            match = _KEY.fullmatch(stripped)
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
            match = _KEY.fullmatch(stripped)
            value = (match.group(2) or "").strip() if match else ""
            if match is None or not _JOB_ID.fullmatch(match.group(1)) or (value and not value.startswith("#")):
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
            invocations, unsupported = classify_command(command)
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


def _test_section_disables(manifest: str, target: str) -> bool:
    """Report whether a [[test]] section turns the target off or gates it on features."""
    for section in re.split(r"(?m)^\[\[test\]\]\s*$", manifest)[1:]:
        body = re.split(r"(?m)^\[", section, maxsplit=1)[0]
        if re.search(rf'(?m)^name\s*=\s*"{re.escape(target)}"', body):
            return bool(re.search(r"(?m)^(?:test\s*=\s*false|required-features\s*=)", body))
    return False


def _rerun_problems(
    root: Path, entry: CiOnly, invocation: Invocation,
    jobs: Mapping[str, list[tuple[str, str]]], workspace: str,
) -> list[str]:
    """Prove that one CI-only cargo test only reruns a target the local workspace step runs."""
    label = f"CI-only {entry.kind} `{entry.command}`"
    args = invocation.args
    if (
        invocation.env or len(args) != 6 or args[0] != "-p" or args[2] != "--test"
        or args[4:] != ("--", "--nocapture")
        or not _CARGO_NAME.fullmatch(args[1]) or not _CARGO_NAME.fullmatch(args[3])
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
        if workspace not in [command for _label, command in jobs.get(job, [])]:
            problems.append(f"{label}: {job} does not run `{workspace}`, so nothing runs it locally")
    return problems


def ci_only_problems(
    root: Path, workflow: str, steps: Sequence[Step] = STEPS, entries: Sequence[CiOnly] = CI_ONLY
) -> list[str]:
    """Check that each CI-only entry is reasoned and hides no missing local test or gate."""
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
        invocations, unsupported = classify_command(entry.command)
        problems.extend(
            f"{label}: uses {reason}, a spelling the gate classifier does not model"
            for reason in unsupported
        )
        if not invocations and not unsupported:
            problems.append(f"{label}: is not a gate invocation, so it needs no entry")
        if any(is_self_test(invocation) for invocation in invocations):
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
        "header": ("Step", "Command", "Local hosts", "Class", "Needs", "CI jobs"),
        "joiner": ", ",
        "classes": "Classes: `local` steps run by default; `release` steps run with "
        "`--with-release`; `optional` steps run with `--with-optional` and never run in CI.",
        "hosts": "Local hosts are the hosts where the runner selects a step. CI jobs are where CI "
        "runs it, which can cover fewer hosts, or none.",
        "needs": "Needs:",
        "always": "Every step also needs Git and Python 3 on `PATH`.",
        "colon": ": ",
        "index": 0,
    },
    "zh-CN": {
        "header": ("步骤", "命令", "本地主机", "类别", "前置条件", "CI job"),
        "joiner": "、",
        "classes": "类别：`local` 步骤默认运行；`release` 步骤需加 `--with-release`；"
        "`optional` 步骤需加 `--with-optional`，且从不在 CI 中运行。",
        "hosts": "本地主机是 runner 会选择该步骤的主机。CI job 是 CI 运行它的位置，可能覆盖更少的主机，"
        "或一个也没有。",
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
    lines.extend(["", text["classes"], "", text["hosts"], "", text["needs"], ""])
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
    # Process-group members killed after the leader exited; None when they could not be counted.
    leftover_processes: int | None = 0


def _step_log_path(log_dir: Path, index: int, step: Step) -> Path:
    return log_dir / f"{index:02d}-{step.id}.log"


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
            exit_code: int | None, detail: str, leftover: int | None = 0) -> StepResult:
    elapsed = time.monotonic() - started
    footer = f"\n[local-gate] result={status} exit={_exit_text(exit_code)} elapsed={elapsed:.1f}s"
    if leftover != 0:
        footer += f" leftover_processes={'unknown' if leftover is None else leftover}"
    if detail:
        footer += f" detail={detail}"
    try:
        log.write((footer + "\n").encode("utf-8", errors="replace"))
        log.flush()
    except (OSError, ValueError):
        # The verdict still stands when the log cannot take its footer.
        pass
    return StepResult(step.id, status, exit_code, elapsed, log_path, detail, leftover)


def _reap_leader(process: subprocess.Popen[bytes], timeout: float) -> bool:
    """Reap an exited or killed leader within timeout; return False if another reaper had it.

    Popen.wait turns a lost child into exit status 0, so POSIX reaps with os.waitpid, which
    reports ECHILD instead. A leader still running at the timeout is left to the caller.
    """
    if process.returncode is not None:
        return True
    if os.name == "nt":
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass
        return True
    end = time.monotonic() + timeout
    while True:
        try:
            pid, status = os.waitpid(process.pid, os.WNOHANG)
        except ChildProcessError:
            return False
        if pid:
            process.returncode = os.waitstatus_to_exitcode(status)
            return True
        if time.monotonic() >= end:
            return True
        time.sleep(0.01)


def _kill_tree(process: subprocess.Popen[bytes], owned: bool) -> str:
    """Kill a timed-out or interrupted step's tree and describe how the kill went.

    Without ownership nothing is signalled: another reaper collected the leader, so its pid and
    group id may already name other processes. macOS refuses a group kill with EPERM when the
    unreaped leader is the only member left, where Linux reports success; that refusal is
    recorded, and the leader is still killed or reaped.
    """
    if not owned:
        return "no signal sent: another reaper collected the leader, so its pid and group id are not reserved"
    try:
        SMOKE_RUNNER.terminate_process_tree(process)
    except PermissionError:
        try:
            process.kill()
        except OSError:
            pass  # The leader cannot be signalled either; the bounded reap still returns.
        return "the group kill was refused with EPERM, so only the leader was killed or reaped"
    return "process tree killed"


def _reap(process: subprocess.Popen[bytes], reader: threading.Thread, owned: bool = True) -> str:
    """Collect a killed step's status and let its output drain, without blocking forever.

    A leader another reaper collected is not waited for, so no synthetic status is recorded.
    """
    if owned:
        _reap_leader(process, 10)
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


_WAITID_NAMES = ("waitid", "P_PID", "WEXITED", "WNOHANG", "WNOWAIT")
_KQUEUE_NAMES = ("kqueue", "kevent", "KQ_FILTER_PROC", "KQ_EV_ADD", "KQ_EV_ONESHOT", "KQ_NOTE_EXIT")


def leader_watches(os_module=os, select_module=select) -> tuple[str, ...]:
    """List the ways this host sees a POSIX leader exit without reaping it, preferred first.

    waitid with WNOWAIT is preferred where Python provides it; older macOS builds lack
    os.waitid and use a kqueue exit event. Windows has neither and uses Popen.wait.
    """
    if os_module.name == "nt":
        return ()
    found = []
    if all(hasattr(os_module, name) for name in _WAITID_NAMES):
        found.append("waitid")
    if all(hasattr(select_module, name) for name in _KQUEUE_NAMES):
        found.append("kqueue")
    return tuple(found)


# None falls back to Popen.wait, which reaps the leader before its group is settled.
_LEADER_WATCH = (leader_watches() or (None,))[0]


class _LeaderLost(Exception):
    """Another reaper collected a step's leader, so its exit status and group id are gone."""


def _await_leader(process: subprocess.Popen[bytes], timeout: float, watch: str) -> bool:
    """Wait up to timeout for the leader to exit, leaving it unreaped; report whether it exited.

    watch is "waitid" or "kqueue", from leader_watches(). An unreaped leader keeps its pid,
    which is the group id, so no unrelated group can take that number while the runner
    still polls or kills the group. Nothing here reaps the leader. _LeaderLost means
    another reaper collected it first, as when SIGCHLD is ignored.
    """
    if watch == "waitid":
        end = time.monotonic() + timeout
        delay = 0.0005
        while True:
            try:
                if os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None:
                    return True
            except ChildProcessError:
                raise _LeaderLost from None
            remaining = end - time.monotonic()
            if remaining <= 0:
                return False
            delay = min(delay * 2, remaining, 0.05)
            time.sleep(delay)
    queue = select.kqueue()
    try:
        event = select.kevent(process.pid, select.KQ_FILTER_PROC,
                              select.KQ_EV_ADD | select.KQ_EV_ONESHOT, select.KQ_NOTE_EXIT)
        try:
            queue.control([event], 0, 0)
        except ProcessLookupError:
            pass  # Already exited; signal 0 below tells an unreaped zombie from a lost leader.
        else:
            if not queue.control(None, 1, max(0.0, timeout)):
                return False
    finally:
        queue.close()
    try:
        # Signal 0 reaches an unreaped zombie and fails once another reaper has collected it.
        os.kill(process.pid, 0)
    except OSError:
        raise _LeaderLost from None
    return True


def _group_members(pgid: int) -> list[int] | None:
    """List the live processes in a POSIX process group, or None when they cannot be read.

    Linux reads /proc, which a minimal container has even without `ps`; other hosts ask
    `ps`. Zombies are left out: they run nothing, and a container's PID 1 may never reap them.
    """
    proc = Path("/proc")
    if sys.platform.startswith("linux") and proc.is_dir():
        try:
            entries = [entry for entry in proc.iterdir() if entry.name.isdigit()]
        except OSError:
            return None
        members = []
        for entry in entries:
            try:
                data = (entry / "stat").read_bytes()
            except OSError:
                continue  # The process exited while the list was read.
            # After the parenthesized command name come the state, the parent, and the group.
            fields = data[data.rfind(b")") + 2:].split()
            if len(fields) >= 3 and fields[2] == str(pgid).encode() and fields[0] not in (b"Z", b"X"):
                members.append(int(entry.name))
        return members
    try:
        completed = subprocess.run(
            ["ps", "-A", "-o", "pid=", "-o", "pgid=", "-o", "stat="],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=PS_TIMEOUT_S,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if completed.returncode != 0:
        return None
    members = []
    for line in completed.stdout.decode("ascii", errors="replace").splitlines():
        fields = line.split()
        if (len(fields) >= 3 and fields[0].isdigit() and fields[1] == str(pgid)
                and not fields[2].startswith("Z")):
            members.append(int(fields[0]))
    return members


def _settle_group(process: subprocess.Popen[bytes], deadline: float) -> tuple[bool, int | None]:
    """Wait up to LEFTOVER_GRACE_S for an exited leader's process group to empty, then kill it.

    Returns whether live members outlived the grace period and were killed, and how many,
    or None when they could not be counted. Where the host can see an exit without reaping
    it, the caller keeps the leader unreaped until after this returns, so the group id cannot
    be reused; emptiness therefore comes from the member list, which leaves the zombie leader
    out, not from signal 0, which counts it.
    Windows has no group-emptiness check, so there a descendant is bounded only by the
    output pipe and the deadline, as in the smoke runner.
    """
    if os.name == "nt":
        return False, 0
    # The leader was started with start_new_session, so its pid is the group id.
    pgid = process.pid
    grace_end = min(time.monotonic() + LEFTOVER_GRACE_S, deadline)
    members = _group_members(pgid)
    # An unreadable list is retried until the grace period ends; it is never proof of empty.
    while members != [] and time.monotonic() < grace_end:
        time.sleep(0.05)
        members = _group_members(pgid)
    if members == []:
        return False, 0
    # Members are still live, or still cannot be listed after the grace period.
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        return False, 0  # The group emptied between the last check and the kill.
    except PermissionError:
        pass
    kill_end = time.monotonic() + 5
    while time.monotonic() < kill_end and _group_members(pgid) != []:
        time.sleep(0.1)
    return True, (None if members is None else len(members))


def run_step(step: Step, index: int, root: Path, log_dir: Path,
             environ: Mapping[str, str]) -> StepResult:
    """Run one step in its own process group under a deadline that kills the group, logging its output.

    On POSIX the step ends once its leader has exited and its process group is empty;
    members that outlive the leader by LEFTOVER_GRACE_S are killed, counted, and fail the
    step. The leader is reaped only after that, so its pid keeps the group id reserved.
    Windows waits for the output pipe until the deadline instead.
    """
    log_path = _step_log_path(log_dir, index, step)
    env = dict(environ)
    env.update(step.env)
    argv = launch_argv(step)
    started = time.monotonic()
    # A new file renamed onto log_path, so a link an earlier step left there is replaced.
    with os.fdopen(_create_output(log_path), "wb") as log:
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
        timed_out = lost = False
        leftover, count = False, 0
        try:
            try:
                remaining = max(0.0, deadline - time.monotonic())
                if _LEADER_WATCH is None:
                    # Reaping first leaves a window where the group id can be reused; only
                    # Windows and POSIX hosts with neither waitid nor kqueue take this path.
                    process.wait(timeout=remaining)
                elif not _await_leader(process, remaining, _LEADER_WATCH):
                    raise subprocess.TimeoutExpired(argv, step.timeout_s)
            except subprocess.TimeoutExpired:
                timed_out = True
            except _LeaderLost:
                # The group id is no longer reserved, so no group is scanned or signalled.
                lost = True
                reader.join(max(0.0, deadline - time.monotonic()))
                timed_out = reader.is_alive()
            else:
                leftover, count = _settle_group(process, deadline)
                if not leftover:
                    # With the group empty, a descendant that left it can still hold the output
                    # pipe, so the step stays under its deadline until the pipe closes.
                    reader.join(max(0.0, deadline - time.monotonic()))
                    timed_out = reader.is_alive()
        except KeyboardInterrupt:
            note = _kill_tree(process, owned=not lost)
            detail = _reap(process, reader, owned=not lost)
            return _finish(log, step, log_path, started, INTERRUPTED, process.returncode,
                           f"interrupted; {note}{detail}")
        if timed_out:
            note = _kill_tree(process, owned=not lost)
            detail = _reap(process, reader, owned=not lost)
            return _finish(log, step, log_path, started, TIMEOUT, process.returncode,
                           f"deadline of {step.timeout_s}s reached; {note}{detail}")
        # The leader has exited, so this reaps it without blocking. Until now its pid kept the
        # group id reserved for every group kill above, including the pipe-timeout one.
        if lost or not _reap_leader(process, 10):
            _close(process.stdout)
            return _finish(log, step, log_path, started, FAIL, None,
                           "exit status unavailable: another reaper collected the leader, so no "
                           "signal was sent to its pid or group")
        if leftover:
            counted = "an unknown number of" if count is None else str(count)
            detail = (f"{counted} leftover process(es) outlived the leader by "
                      f"{LEFTOVER_GRACE_S:g}s; process group killed{_reap(process, reader)}")
            return _finish(log, step, log_path, started, FAIL, process.returncode, detail, count)
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
    """Identify a path's type, permission bits, and content or link target.

    A further edit or chmod to a path that was already dirty stays visible, and a FIFO
    or device is never opened.
    """
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        return "missing"
    except OSError as error:
        return f"unreadable:{error.errno}"
    mode = f"{stat.S_IMODE(info.st_mode):04o}"
    if stat.S_ISLNK(info.st_mode):
        try:
            return f"link {mode} -> {os.readlink(path)}"
        except OSError as error:
            return f"link {mode} unreadable:{error.errno}"
    if stat.S_ISDIR(info.st_mode):
        return f"dir {mode}"
    if not stat.S_ISREG(info.st_mode):
        return f"other {stat.S_IFMT(info.st_mode):o} {mode}"
    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1 << 20), b""):
                digest.update(chunk)
    except OSError as error:
        return f"file {mode} unreadable:{error.errno}"
    return f"file {mode} sha256:{digest.hexdigest()}"


def _real(path: Path) -> str:
    """Return a path's resolved spelling, for comparing Git's paths with the runner's own files."""
    return os.path.normcase(os.path.realpath(path))


def log_dir_problem(root: Path, log_dir: Path) -> str | None:
    """Explain why a log directory is refused, or return None.

    The repository root or an ancestor of it would put every path the snapshot reads
    under the log directory.
    """
    try:
        root_real = root.resolve()
        log_real = log_dir.resolve()
    except OSError as error:
        return f"cannot resolve --log-dir {log_dir}: {error}"
    problem = (f"--log-dir {log_dir} is the repository root or an ancestor of it; "
               f"choose a directory outside the tree or below its root")
    candidates = [root_real, *root_real.parents]
    if log_real in candidates:
        return problem
    if log_dir.exists():
        for candidate in candidates:
            try:
                if os.path.samefile(log_dir, candidate):
                    return problem
            except OSError:
                continue
    return None


def sigchld_problem() -> str | None:
    """Explain why the runner refuses to start because SIGCHLD is ignored, or return None.

    With SIGCHLD ignored the kernel can reap each step's leader as it exits, so the runner could
    neither read a step's exit status nor keep its process-group id reserved.
    """
    if os.name == "nt" or signal.getsignal(signal.SIGCHLD) != signal.SIG_IGN:
        return None
    return ("SIGCHLD is ignored, so the kernel can reap each step before the runner reads its "
            "exit status; restore its default disposition and rerun")


def runner_outputs(log_dir: Path, steps: Sequence[Step]) -> list[Path]:
    """Return every path the runner writes under log_dir: one log per step and both summaries."""
    outputs = [_step_log_path(log_dir, index, step) for index, step in enumerate(steps, 1)]
    return outputs + [log_dir / "summary.txt", log_dir / "summary.json"]


def runner_output_problem(root: Path, outputs: Iterable[Path]) -> str | None:
    """Explain why a runner-owned output path is refused, or return None.

    The summaries are written after the final snapshot, so an output that Git tracks, or
    that a symlink or hard link aliases to other content, could change tracked state that no
    snapshot compares. Tracked paths match case-insensitively, as macOS and Windows resolve them.
    """
    outputs = list(outputs)
    for path in outputs:
        try:
            info = os.lstat(path)
        except FileNotFoundError:
            continue
        except OSError as error:
            return f"cannot inspect runner output {path}: {error}"
        if stat.S_ISLNK(info.st_mode):
            return f"runner output {path} is a symlink; remove it or choose another --log-dir"
        if stat.S_ISREG(info.st_mode) and info.st_nlink > 1:
            return f"runner output {path} is a hard link; remove it or choose another --log-dir"
    try:
        top = _git(root, "rev-parse", "--show-toplevel")
        if top.returncode != 0:
            return None  # Outside a work tree nothing is tracked; the snapshot reports why.
        toplevel = Path(os.fsdecode(top.stdout.strip()))
        listed = _git(toplevel, "ls-files", "-z")
    except (OSError, subprocess.TimeoutExpired):
        return None  # The snapshot then records Git state as unavailable.
    if listed.returncode != 0:
        return None
    tracked = {os.fsdecode(name).casefold() for name in listed.stdout.split(b"\0") if name}
    top_real = os.path.realpath(toplevel)
    for path in outputs:
        try:
            relative = os.path.relpath(os.path.realpath(path), top_real)
        except ValueError:
            continue  # On another Windows drive, so outside the work tree.
        if Path(relative).as_posix().casefold() in tracked:
            return f"runner output {path} is tracked by Git; choose a --log-dir outside the tracked tree"
    return None


def _output_alias(path: Path) -> str | None:
    """Name the link a step left at a runner output path, without following it, or return None."""
    try:
        info = os.lstat(path)
    except OSError:
        return None
    if stat.S_ISLNK(info.st_mode):
        return "a symlink"
    if stat.S_ISREG(info.st_mode) and info.st_nlink > 1:
        return "a hard link"
    return None


def _alias_problem(path: Path, alias: str) -> str:
    """Describe a link found at a runner output during the run, which the runner replaced unread."""
    return f"{path} became {alias} during the run; the runner replaced it without following it"


def _create_output(path: Path) -> int:
    """Create a runner output as a new file and return a writable descriptor for it.

    The file is created exclusively in the log directory and renamed onto path, so a symlink
    or hard link a step left at path is replaced, never written through.
    """
    fd, temporary = tempfile.mkstemp(prefix=".local-gate-", dir=str(path.parent))
    try:
        if os.name != "nt":
            os.replace(temporary, path)
            return fd
        # Windows cannot rename an open file: close, rename, reopen, and check it is the same file.
        created = os.fstat(fd)
        os.close(fd)
        fd = -1
        os.replace(temporary, path)
        fd = os.open(path, os.O_WRONLY | getattr(os, "O_BINARY", 0))
        reopened = os.fstat(fd)
        if (reopened.st_dev, reopened.st_ino) != (created.st_dev, created.st_ino):
            raise OSError(f"{path} changed while the runner created it")
        return fd
    except BaseException:
        if fd >= 0:
            os.close(fd)
        try:
            os.unlink(temporary)
        except OSError:
            pass  # Already renamed onto path, or never created.
        raise


def _write_output(path: Path, text: str) -> None:
    """Write a runner output through a new file renamed onto path, never following a link there."""
    with os.fdopen(_create_output(path), "w", encoding="utf-8") as handle:
        handle.write(text)


def _dir_identity(path: Path) -> tuple[int, int] | None:
    """Return the device and inode of the directory path now resolves to, or None."""
    try:
        info = os.stat(path)
    except OSError:
        return None
    return info.st_dev, info.st_ino


def _log_dir_replaced(log_dir: Path, identity: tuple[int, int] | None) -> str | None:
    """Describe a log directory replaced since the run began, or return None."""
    if identity is None or _dir_identity(log_dir) == identity:
        return None
    return (f"--log-dir {log_dir} was replaced during the run; the runner started no further "
            f"step and wrote no summaries")


def snapshot_git(root: Path, runner_files: Iterable[Path] = ()) -> GitSnapshot:
    """Record tracked and untracked Git state without modifying the tree or the index.

    Only untracked paths among runner_files, the runner's own logs and summaries, are
    left out. A tracked path is always fingerprinted, wherever the logs are written.
    """
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
    own = {_real(path) for path in runner_files}
    entries = []
    for code, path in _porcelain_entries(status.stdout):
        absolute = toplevel / path
        if code == "??" and _real(absolute) in own:
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
        return f"{entry[0].strip() or '-'} {entry[1][:29]}"

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
    # Links or a replaced log directory found at runner outputs during the run.
    output_problems: tuple[str, ...] = ()

    @property
    def exit_code(self) -> int:
        """Return 130 when interrupted, 1 when a step failed or the tree or an output changed, else 0."""
        if self.interrupted:
            return 130
        if self.changes or self.output_problems or any(result.status != PASS for result in self.results):
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
    if report.output_problems:
        lines.append(f"[local-gate] runner outputs changed during the run ({len(report.output_problems)}):")
        lines.extend(f"  {problem}" for problem in report.output_problems)
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
        "output_problems": list(report.output_problems),
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
                "leftover_processes": result.leftover_processes,
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
    """Print one console line, escaping what the console cannot encode instead of raising.

    A log tail can hold any text, and a Windows console or pipe is often cp1252, so a
    diagnostic must never stop the run. The saved step logs keep their raw bytes.
    """
    encoding = getattr(out, "encoding", None) or "utf-8"
    try:
        text.encode(encoding)
    except UnicodeEncodeError:
        text = text.encode(encoding, errors="backslashreplace").decode(encoding, errors="replace")
    except LookupError:
        text = text.encode("ascii", errors="backslashreplace").decode("ascii")
    print(text, file=out, flush=True)


def _print_tail(out: TextIO, log_path: Path) -> None:
    """Print a failed step's log tail, never following a symlink a step left at the log path."""
    flags = (os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
             | getattr(os, "O_BINARY", 0))
    try:
        with os.fdopen(os.open(log_path, flags), "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                return
            data = handle.read()
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
    outputs = runner_outputs(log_dir, steps)
    problem = (sigchld_problem() or log_dir_problem(root, log_dir)
               or runner_output_problem(root, outputs))
    if problem:
        raise ValueError(problem)
    log_dir.mkdir(parents=True, exist_ok=True)
    identity = _dir_identity(log_dir)
    _say(out, f"[local-gate] host={host} steps={len(steps)} root={root}")
    _say(out, f"[local-gate] logs={log_dir}")
    before = snapshot_git(root, outputs)
    results: list[StepResult] = []
    problems: list[str] = []
    interrupted = replaced = False
    for index, step in enumerate(steps, 1):
        # Every output is checked without following links before and after the steps that could
        # plant one; a replaced log directory stops the run, since nothing there is the runner's.
        moved = _log_dir_replaced(log_dir, identity)
        if moved:
            problems.append(moved)
            replaced = True
            break
        log_path = _step_log_path(log_dir, index, step)
        alias = _output_alias(log_path)
        if alias:
            problems.append(_alias_problem(log_path, alias))
        _say(out, f"[local-gate] start {step.id} timeout={step.timeout_s}s: {command_text(step)}")
        result = run_step(step, index, root, log_dir, env)
        results.append(result)
        _say(out, f"[local-gate] finish {step.id} result={result.status} "
                  f"exit={_exit_text(result.exit_code)} elapsed={result.elapsed_s:.1f}s")
        interrupted = result.status == INTERRUPTED
        # Checked before any read of the step's log: through a replaced directory, even a
        # no-follow open of the log path reads whatever that directory now holds.
        moved = _log_dir_replaced(log_dir, identity)
        if moved:
            problems.append(moved)
            replaced = True
            break
        alias = _output_alias(result.log_path)
        if alias:
            problems.append(f"{result.log_path} became {alias} during its step; its tail was not read")
        elif result.status != PASS:
            _print_tail(out, result.log_path)
        if interrupted:
            break
    after = snapshot_git(root, outputs)
    if not replaced:
        moved = _log_dir_replaced(log_dir, identity)
        if moved:
            problems.append(moved)
            replaced = True
    summaries = (log_dir / "summary.txt", log_dir / "summary.json")
    if not replaced:
        for path in summaries:
            alias = _output_alias(path)
            if alias:
                problems.append(_alias_problem(path, alias))
    report = GateReport(host, tuple(results), before, after,
                        tuple(describe_changes(before, after)), log_dir, interrupted, tuple(problems))
    lines = summary_lines(report)
    if not replaced:
        _write_output(summaries[0], "\n".join(lines) + "\n")
        _write_output(summaries[1], json.dumps(summary_json(report, steps), indent=2) + "\n")
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
    problem = sigchld_problem()
    if problem is None and args.log_dir is not None:
        problem = log_dir_problem(root, args.log_dir) or runner_output_problem(
            root, runner_outputs(args.log_dir, steps)
        )
    if problem:
        parser.error(problem)
    log_dir = (args.log_dir.resolve() if args.log_dir
               else Path(tempfile.mkdtemp(prefix="sonicterm-local-gate-")))
    try:
        return run_gate(steps, root, log_dir, host).exit_code
    except KeyboardInterrupt:
        print("[local-gate] interrupted", file=sys.stderr, flush=True)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
