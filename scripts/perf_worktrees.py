"""Perf worktrees."""

from __future__ import annotations
import os
from pathlib import Path
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_focus
import perf_scenarios
import perf_settings


def work_directory(stamp: str) -> Path:
    """Return a comparison's directory for worktrees and target directories, under the ignored target/ tree."""
    return perf_settings.ROOT / "target" / "perf-compare" / f"work-{stamp}"


class Worktrees:
    """The detached worktrees this comparison created; only these are ever removed.

    `git worktree add` and `remove` change the repository's worktree list, never the main
    checkout's index, branch or working tree.
    """

    def __init__(self, git: Callable[[Sequence[str]], CommandRecord], directory: Path) -> None:
        self.git = git
        self.directory = directory
        self.created: list[Path] = []

    def create(self, side: str, sha: str) -> Path:
        """Add a detached worktree for one side at a path that does not exist yet; one per side, even for one SHA."""
        path = self.directory / side
        if os.path.lexists(path):
            raise ValueError(f"worktree path {path} already exists; a comparison never reuses one")
        self.directory.mkdir(parents=True, exist_ok=True)
        record = self.git(("git", "worktree", "add", "--detach", str(path), sha))
        failure = perf_focus._command_failure(record)
        if failure:
            raise ValueError(f"{failure}: {record.stderr.strip() or record.stdout.strip()}")
        self.created.append(path)
        return path

    def remove(self) -> list[str]:
        """Force-remove each created worktree, newest first, since the overlay modified them; return failures."""
        problems = []
        for path in reversed(self.created):
            record = self.git(("git", "worktree", "remove", "--force", str(path)))
            failure = perf_focus._command_failure(record)
            if failure:
                problems.append(f"{failure}: {record.stderr.strip()}")
        self.created = []
        return problems


def build_error(text: str) -> str:
    """Name a failed build by its first compiler error, or by its last lines when there is none."""
    errors = [line.strip() for line in text.splitlines() if line.startswith("error")]
    return errors[0] if errors else perf_scenarios.log_tail(text, 3)


HOST_COMMANDS = {
    "model": ("sysctl", "-n", "hw.model"),
    "cpu": ("sysctl", "-n", "machdep.cpu.brand_string"),
    "memory": ("sysctl", "-n", "hw.memsize"),
    "os": ("sw_vers",),
    "displays": ("system_profiler", "SPDisplaysDataType"),
    "power": ("pmset", "-g", "batt"),
    "power_settings": ("pmset", "-g"),
}
