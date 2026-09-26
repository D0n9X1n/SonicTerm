#!/usr/bin/env python3
"""Require the complete macOS native split-selection matrix on a Metal adapter."""

from __future__ import annotations

import argparse
from collections import Counter
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import sys
import tempfile
from typing import Mapping, Sequence

ROOT = Path(__file__).resolve().parent.parent
FINAL_PASS = (
    "PASS native split selection: native windows/renderers, synthetic App pointer events, "
    "memory clipboard; no physical gesture or pixel-readback claim"
)
MAX_LOG_BYTES = 8 * 1024 * 1024
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
CASE = re.compile(
    r"PASS native selection child=(false|true) topology=(Horizontal|Vertical|Nested) "
    r"press_pane=(\d+) foreign_pane=(\d+)"
)
EXPECTED_CASES = {(child, topology) for child in ("false", "true")
                  for topology in ("Horizontal", "Vertical", "Nested")}


def load_gate():
    """Reuse the canonical launcher, including its POSIX post-exit group check."""
    spec = importlib.util.spec_from_file_location(
        "selection_local_gate", ROOT / "scripts" / "local-gate.py"
    )
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def probe_environment(base: Mapping[str, str]) -> dict[str, str]:
    """Keep HOME and the native temp root while selecting observable Metal rendering."""
    environment = dict(base)
    environment.pop("NO_COLOR", None)
    environment["WGPU_BACKEND"] = "metal"
    environment["RUST_LOG"] = "warn,sonicterm_gpu::core=info"
    environment["TMPDIR"] = tempfile.gettempdir()
    return environment


def verdict_problems(
    output: str, *, exit_code: int | None = 0, step_status: str = "PASS",
    leftover_processes: int | None = 0, fixture_exists: bool = False,
    overflow: bool = False,
) -> list[str]:
    """Reject partial, skipped, software-only or unsuccessfully cleaned native runs."""
    problems = []
    if exit_code != 0 or step_status != "PASS" or leftover_processes != 0:
        problems.append(f"native launcher status={step_status} exit={exit_code} leftovers={leftover_processes}")
    if fixture_exists:
        problems.append("fixture directory survived the native run")
    if overflow:
        problems.append("native log exceeded the evidence limit")
    lines = ANSI.sub("", output).splitlines()
    case_lines = [(index, line) for index, line in enumerate(lines)
                  if line.startswith("PASS native selection ")]
    cases = []
    for _, line in case_lines:
        match = CASE.fullmatch(line)
        if not match or match[3] == match[4]:
            problems.append("malformed native selection case: " + line)
        else:
            cases.append((match[1], match[2]))
    if Counter(cases) != Counter({case: 1 for case in EXPECTED_CASES}):
        problems.append("native selection cases are missing, duplicated or unexpected")
    finals = [index for index, line in enumerate(lines) if line == FINAL_PASS]
    if len(finals) != 1 or (case_lines and finals[0] <= case_lines[-1][0]):
        problems.append("final PASS is missing, duplicated or precedes a case")
    if any(re.search(r"NOT_EXERCISED|HOST_INCAPABLE|BLOCKED|\bFAIL\b|panicked|cleanup failed", line)
           for line in lines):
        problems.append("native output contains a failure, skip or cleanup warning")
    adapters = [line for line in lines if "wgpu adapter selected" in line]
    if len(adapters) != len(EXPECTED_CASES):
        problems.append("one selected-adapter record is required for each native case")
    for line in adapters:
        backend = re.search(r"\bbackend=([A-Za-z0-9]+)\b", line)
        device = re.search(r"\bdevice_type=([A-Za-z0-9]+)\b", line)
        software = re.search(r"\bsoftware_rendering=(true|false)\b", line)
        if (not backend or backend[1] != "Metal" or not device
                or device[1] not in ("IntegratedGpu", "DiscreteGpu", "VirtualGpu", "Other")
                or not software or software[1] != "false"):
            problems.append("selected adapter is not a recorded non-CPU Metal device: " + line)
    return problems


def main(argv: Sequence[str] | None = None) -> int:
    argparse.ArgumentParser(description=__doc__).parse_args(argv)
    if sys.platform != "darwin":
        print("native selection gate requires macOS", file=sys.stderr)
        return 2
    if signal.getsignal(signal.SIGCHLD) == signal.SIG_IGN:
        print("native selection gate refuses ignored SIGCHLD", file=sys.stderr)
        return 2
    gate = load_gate()
    if not gate.leader_watches():
        print("native selection gate needs waitid or kqueue leader observation", file=sys.stderr)
        return 2
    with tempfile.TemporaryDirectory(prefix="sonicterm-selection-fixture-") as temporary:
        fixture = Path(temporary) / "fixture"
        log_dir = Path(tempfile.mkdtemp(prefix="sonicterm-selection-evidence-"))
        github_env = os.environ.get("GITHUB_ENV")
        if github_env:
            with Path(github_env).open("a", encoding="utf-8") as stream:
                stream.write(f"SONICTERM_SELECTION_LOG_DIR={log_dir}\n")
        # Cargo resolves configured output layouts and checks freshness before running the example.
        command = ("cargo", "run", "--locked", "-p", "sonicterm-app", "--example",
                   "native_split_selection", "--", "--run", str(fixture))
        step = gate.Step("native-selection", command,
                         ("macos",), 190, "local", ("rust", "native"), ())
        result = gate.run_step(step, 1, ROOT, log_dir, probe_environment(os.environ),
                               output_limit_bytes=MAX_LOG_BYTES)
        with result.log_path.open("rb") as stream:
            raw = stream.read(MAX_LOG_BYTES + 65537)
        overflow = len(raw) > MAX_LOG_BYTES + 65536
        output = raw[:MAX_LOG_BYTES + 65536].decode("utf-8", errors="replace")
        problems = verdict_problems(output, exit_code=result.exit_code, step_status=result.status,
                                    leftover_processes=result.leftover_processes,
                                    fixture_exists=fixture.exists(), overflow=overflow)
        print(output, end="" if output.endswith("\n") else "\n")
        report = {"status": "FAIL" if problems else "PASS", "problems": problems,
                  "exit_code": result.exit_code, "launcher_status": result.status,
                  "leftover_processes": result.leftover_processes, "detail": result.detail,
                  "command": list(command), "log": str(result.log_path)}
        (log_dir / "result.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        for problem in problems:
            print("[native-selection] " + problem, file=sys.stderr)
        print(f"[native-selection] verdict={report['status']} evidence={log_dir}")
        return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
