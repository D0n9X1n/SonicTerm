"""Gate files."""

from __future__ import annotations
from dataclasses import dataclass, replace
import hashlib
import os
from pathlib import Path
import signal
import stat
import subprocess
import tempfile
from typing import Iterable, Mapping, Sequence, TextIO

import gate_model
import gate_process


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
        timeout=gate_model.GIT_TIMEOUT_S,
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


def runner_outputs(log_dir: Path, steps: Sequence[gate_model.Step]) -> list[Path]:
    """Return every path the runner writes under log_dir: one log per step and both summaries."""
    outputs = [gate_process._step_log_path(log_dir, index, step) for index, step in enumerate(steps, 1)]
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
