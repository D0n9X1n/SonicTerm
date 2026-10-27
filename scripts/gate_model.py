"""Gate model."""

from __future__ import annotations
from dataclasses import dataclass, replace
from enum import Enum
import importlib.util
import os
from pathlib import Path


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
WINDOWS_JOB = None
if os.name == "nt":
    _JOB_SPEC = importlib.util.spec_from_file_location("windows_process_job", _HERE / "windows-process-job.py")
    if _JOB_SPEC is None or _JOB_SPEC.loader is None:
        raise ImportError("cannot load scripts/windows-process-job.py")
    WINDOWS_JOB = importlib.util.module_from_spec(_JOB_SPEC)
    _JOB_SPEC.loader.exec_module(WINDOWS_JOB)

HOSTS = ("macos", "windows", "linux")
HOST_LABELS = {"macos": "macOS", "windows": "Windows", "linux": "Linux"}

# `local` is the mandatory default gate. `release` is release-preparation
# evidence. `optional` is a pre-push aid that never names a CI job.
EVIDENCE_CLASSES = ("local", "release", "optional")

PASS = "PASS"
CLEANED_NOT_NATURAL = "CLEANED_NOT_NATURAL"
FAIL = "FAIL"
TIMEOUT = "TIMEOUT"
LAUNCH = "LAUNCH"
INTERRUPTED = "INTERRUPTED"
NOT_RUN = "NOT_RUN"

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
    "actionlint": ("`actionlint` at the version `scripts/pinned-tools.json` pins.",
                   "`scripts/pinned-tools.json` 固定版本的 `actionlint`。"),
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
        "`cargo-llvm-cov` at the version `scripts/pinned-tools.json` pins.",
        "`scripts/pinned-tools.json` 固定版本的 `cargo-llvm-cov`。",
    ),
    "cargo-deny": (
        "`cargo-deny` at the version `scripts/pinned-tools.json` pins, and network access for its "
        "advisory database.",
        "`scripts/pinned-tools.json` 固定版本的 `cargo-deny`，以及用于其 advisory 数据库的网络访问。",
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


class WindowsPolicy(str, Enum):
    """Distinguish reviewed compilation cleanup from strict execution lifetime."""

    STRICT = "strict"
    COMPILE_ONLY = "compile-only"


_COMPILE_ONLY_STEPS = frozenset(("clippy", "perf-scenarios-counters-clippy", "perf-scenarios-frame-texture-clippy",
                                 "perf-scenarios-echo-trace-clippy", "perf-scenarios-harness-api-clippy",
                                 "perf-scenarios-harness-api-echo-trace-clippy", "doc",
                                 "doc-resource-features", "release-windows", "windows-perf-build",
                                 "check-previous-release"))


@dataclass(frozen=True)
class Preparation:
    """One explicit compilation command; empty argv derives the canonical test's no-run command."""

    argv: tuple[str, ...] = ()
    env: tuple[tuple[str, str], ...] = ()


_WINIT_TEST = ("cargo", "test", "--locked", "--manifest-path", "crates/sonicterm-winit/Cargo.toml",
               "--target-dir", "$winit_target_dir", "--features", "serde", "--lib", "--tests", "--no-fail-fast", "--no-run")
_WINIT_DOC = ("cargo", "doc", "--locked", "--manifest-path", "crates/sonicterm-winit/Cargo.toml",
              "--target-dir", "$winit_target_dir", "--features", "serde", "--no-deps", "--lib")
_WORKSPACE_TEST = ("cargo", "test", "--workspace", "--lib", "--bins", "--tests", "--no-fail-fast", "--no-run")
_FEASIBILITY_BUILD = ("cargo", "build", "--quiet", "-p", "sonicterm-io", "--example", "pty_backend_feasibility_evidence")


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
    windows_policy: WindowsPolicy = WindowsPolicy.STRICT
    windows_preparations: tuple[Preparation, ...] = ()
    # The harness-only cfgs; None leaves execution unchanged. Each selected example receives declarations
    # and enabled cfgs as final-target arguments, without changing dependency compiler flags.
    harness_cfgs: tuple[str, ...] | None = None
