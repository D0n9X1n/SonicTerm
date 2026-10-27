"""Gate test fixture."""

from __future__ import annotations
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import unittest

import gate_cli
import gate_model

ROOT = Path(
    os.environ.get("SONICTERM_GATE_ROOT") or Path(__file__).resolve().parent.parent
).resolve()
import local_gate as gate

WORKFLOW = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")

# Fake tools are extensionless /bin/sh scripts found through PATH, which only a
# POSIX host resolves reliably; macOS and Ubuntu CI run these tests.
POSIX_ONLY = unittest.skipIf(os.name == "nt", "fake shell tools need a POSIX host")


def python_step(step_id: str, code: str, timeout_s: int = 30, **fields):
    """Return an injected step that runs Python code under the real runner."""
    values = dict(
        argv=(sys.executable, "-c", code),
        hosts=gate_model.HOSTS,
        timeout_s=timeout_s,
        evidence="local",
        prerequisites=("rust",),
        ci_jobs=(),
    )
    values.update(fields)
    return gate_model.Step(step_id, **values)


# Fixture Git commands take well under a second; the bound keeps a hung
# credential helper, lock, or filesystem from stalling the suite.
FIXTURE_GIT_TIMEOUT_S = 60


def _reap_killed(process: subprocess.Popen[bytes]) -> None:
    """Collect a killed fixture process, closing its pipes if a stray descendant still holds them."""
    try:
        process.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        for pipe in (process.stdout, process.stderr):
            if pipe is not None:
                pipe.close()
        process.wait(timeout=10)


def git(root: Path, *args: str, timeout: float = FIXTURE_GIT_TIMEOUT_S) -> subprocess.CompletedProcess[bytes]:
    """Run git in a fixture repository under a deadline, and fail the test on a git error.

    Git starts as the root of its own process tree, so a timeout kills and reaps the
    whole tree instead of leaving a hung helper behind.
    """
    command = ["git", "-C", str(root), *args]
    process = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        **gate_model.SMOKE_RUNNER.process_group_options(),
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        gate_model.SMOKE_RUNNER.terminate_process_tree(process)
        _reap_killed(process)
        raise
    if process.returncode != 0:
        raise subprocess.CalledProcessError(process.returncode, command, stdout, stderr)
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def commit(root: Path, message: str = "fixture") -> None:
    """Commit a fixture repository's index, isolated from user hooks and signing."""
    git(
        root,
        "-c", "user.name=fixture",
        "-c", "user.email=fixture@example.invalid",
        "-c", "commit.gpgsign=false",
        "-c", f"core.hooksPath={root.parent / 'no-hooks'}",
        "commit", "-q", "--no-verify", "-m", message,
    )


def fixture_repository(directory: Path) -> Path:
    """Create a repository with one committed file, isolated from user hooks and signing."""
    root = directory / "repository"
    root.mkdir()
    git(root, "init", "-q")
    git(root, "config", "core.autocrlf", "false")
    (root / "tracked.txt").write_bytes(b"committed\n")
    git(root, "add", "tracked.txt")
    commit(root)
    return root


def tree_bytes(root: Path) -> dict[str, bytes]:
    """Capture every working-tree file outside .git, so a test can prove nothing changed."""
    files = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if relative.parts[0] == ".git" or not path.is_file():
            continue
        files[relative.as_posix()] = path.read_bytes()
    return files


def run_quietly(steps, root: Path, log_dir: Path):
    """Run the real gate with its console captured, returning the report and the console text."""
    console = io.StringIO()
    report = gate_cli.run_gate(steps, root, log_dir, "test", console=console)
    return report, console.getvalue()


def marker_code(target: Path) -> str:
    """Return Python code that proves a step ran by writing a marker file."""
    return f"import pathlib; pathlib.Path({str(target)!r}).write_text('ran', encoding='utf-8')"


# The Windows error for removing a folder that another handle still holds open.
ERROR_SHARING_VIOLATION = 32
