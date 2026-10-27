"""Perf home."""

from __future__ import annotations
import errno
import os
from pathlib import Path
import re
import stat
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_sessions


# --- Home-write check -----------------------------------------------------------------------

# `breadcrumbs-<stamp>-<pid>-<n>.log`; the stamp may hold hyphens, so the pid is the second-to-last field.
_BREADCRUMB = re.compile(r"breadcrumbs-.+-([0-9]+)-[0-9]+\.(?:log|tmp)")
_MAIN_LOG = re.compile(r"sonicterm\.log(?:\..+)?")
OTHER_INSTANCE_COMMANDS = frozenset(("sonicterm-mac", "sonicterm-linux", "sonicterm-windows.exe"))
# Appended bytes beyond this cannot all be read, so such growth is never attributed elsewhere.
APPENDED_READ_LIMIT_BYTES = 64 * 1024 * 1024


# The home check reads at most this many entries and directory levels; past either bound it is unresolved.
HOME_MAX_ENTRIES = 200_000
HOME_MAX_DEPTH = 32


class HomeSnapshotUnresolved(Exception):
    """The home could not be read whole, so the check cannot clear a run of a write; the run is invalid."""


def snapshot_home(home: Path, max_entries: int = HOME_MAX_ENTRIES,
                  max_depth: int = HOME_MAX_DEPTH) -> dict[str, tuple] | None:
    """Map every file and link under the SonicTerm home to (size, mtime_ns, link text); None when absent.

    A symlink records its target text and, following it, the target's size and mtime, so a write
    through the link or a retargeted link is a change; a dangling link records only its text.
    Symlinked directories are walked too, each real directory once, so a link cycle ends. Past a
    bound, or at an entry or target it cannot read, it raises HomeSnapshotUnresolved instead of
    skipping anything. It only reads: nothing is created there, not even the directory.
    """
    if not home.is_dir():
        return None
    snapshot: dict[str, tuple] = {}
    visited = {os.path.realpath(home)}
    pending = [(home, "")]
    examined = 0
    while pending:
        directory, prefix = pending.pop()
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except FileNotFoundError:
            continue  # Removed while the walk ran; the later snapshot records the removal.
        except OSError as error:
            raise HomeSnapshotUnresolved(f"cannot list {directory}: {error}") from None
        for entry in entries:
            if examined >= max_entries:
                raise HomeSnapshotUnresolved(f"more than {max_entries} entries under {home}")
            examined += 1
            relative, path = prefix + entry.name, Path(entry.path)
            try:
                link_text = os.readlink(path) if entry.is_symlink() else None
            except FileNotFoundError:
                continue  # Removed while the walk ran.
            except OSError as error:
                raise HomeSnapshotUnresolved(f"cannot read the link {path}: {error}") from None
            try:
                # Follows a link, so a write through it shows in its target's size and time.
                info = os.stat(path)
            except OSError as error:
                if link_text is not None and error.errno in (errno.ENOENT, errno.ENOTDIR, errno.ELOOP):
                    snapshot[relative] = (None, None, link_text)  # Dangling: no target a write could reach.
                    continue
                if link_text is None and isinstance(error, FileNotFoundError):
                    continue  # Removed while the walk ran.
                raise HomeSnapshotUnresolved(f"cannot read {path}: {error}") from None
            if not stat.S_ISDIR(info.st_mode):
                snapshot[relative] = (info.st_size, info.st_mtime_ns, link_text)
                continue
            if link_text is not None:
                snapshot[relative] = (None, None, link_text)
            real = os.path.realpath(path)
            if real in visited:
                continue  # A link back into a directory already walked: recorded, not walked again.
            if relative.count("/") + 1 > max_depth:
                raise HomeSnapshotUnresolved(f"{path} is deeper than {max_depth} levels")
            visited.add(real)
            pending.append((path, relative + "/"))
    return snapshot


def _appended_mentions(path: Path, start: int, end: int, texts: Sequence[str]) -> bool | None:
    """Whether the bytes appended to a log mention this run; None when they cannot all be read."""
    if end - start > APPENDED_READ_LIMIT_BYTES:
        return None
    try:
        with path.open("rb") as stream:
            stream.seek(start)
            appended = stream.read(end - start)
    except OSError:
        return None
    return any(text.encode("utf-8") in appended for text in texts if text)


def home_violations(home: Path, before: dict[str, tuple] | None,
                    after: dict[str, tuple] | None, sentinel_ns: int, harness_pid: int | None,
                    other_instance: bool, scratch_texts: Sequence[str]) -> list[str]:
    """List the writes under the SonicTerm home this run cannot be cleared of; empty means none.

    A path is a candidate when it was added, removed or changed, or is newer than the sentinel;
    `.DS_Store` is ignored. A link's entry follows it to its target, so a write through it, or a
    retargeted link, is a candidate. A breadcrumb belongs to the pid in its name, so one naming the
    harness is a violation (the harness starts no breadcrumb writer). Growth of
    `logs/sonicterm.log*`, or a removal under `logs/`, belongs to another instance only when one
    is alive and the appended bytes lack this run's scratch path. Every other candidate is a violation.
    """
    if before is None and after is None:
        return []
    violations = []
    if before is None:
        violations.append(f"{home} was created")
    elif after is None:
        violations.append(f"{home} was removed")
    old, new = before or {}, after or {}
    for relative in sorted(set(old) | set(new)):
        name = relative.rsplit("/", 1)[-1]
        previous, current = old.get(relative), new.get(relative)
        newer = current is not None and current[1] is not None and current[1] > sentinel_ns
        if name == ".DS_Store" or (previous == current and not newer):
            continue
        change = ("added" if previous is None else "removed" if current is None
                  else "changed" if previous != current else "newer than the sentinel")
        breadcrumb = _BREADCRUMB.fullmatch(name) if relative.startswith("logs/breadcrumbs/") else None
        if breadcrumb:
            if harness_pid is None or int(breadcrumb[1]) == harness_pid:
                violations.append(f"{relative}: {change}; a breadcrumb that may be the harness's")
            continue
        under_logs = relative.startswith("logs/")
        if other_instance and under_logs and current is None:
            continue  # Another instance's log retention removed it.
        if other_instance and under_logs and current is not None and _MAIN_LOG.fullmatch(name) \
                and relative.count("/") == 1:
            start = previous[0] if previous is not None and previous[0] is not None else 0
            if current[0] is not None and current[0] > start and _appended_mentions(home / relative, start, current[0],
                                                         scratch_texts) is False:
                continue  # Appended by the live instance; this run's scratch path is not in it.
        violations.append(f"{relative}: {change}")
    return violations


def sonicterm_home(environ: Mapping[str, str]) -> Path:
    """The SonicTerm home the App uses: `HOME`, then `USERPROFILE`, then the account's home, plus `.sonicterm`.

    Like the App's `dirs_home`, a set variable is taken as it is, even when empty.
    """
    for name in ("HOME", "USERPROFILE"):
        if name in environ:
            return Path(environ[name]) / ".sonicterm"
    return Path.home() / ".sonicterm"


def other_instance_alive(table, harness_pid: int | None) -> bool:
    """Whether a readable live process other than the harness is a SonicTerm binary.

    A process list that cannot be read, or a process that cannot be identified, attributes
    nothing, so the stricter verdict stands.
    """
    if table is None:
        return False
    pids = table.pids()
    if pids is None:
        return False
    for pid in pids:
        if pid in (harness_pid, os.getpid()):
            continue
        try:
            info = table.read(pid)
        except perf_sessions.ProcessUnreadable:
            continue
        if info is not None and info.command in OTHER_INSTANCE_COMMANDS:
            return True
    return False
