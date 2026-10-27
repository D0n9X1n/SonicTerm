"""Coverage checkout."""

from __future__ import annotations
from dataclasses import dataclass, field
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
from typing import Dict, List, Mapping, Optional, Tuple

import coverage_model
import coverage_reports


def _write_atomic(path: Path, text: str) -> None:
    """Replace `path` with `text` so an interrupted write never leaves a partial baseline."""
    # A hidden name keeps an interrupted write out of an uploaded directory; upload-artifact skips hidden files.
    handle, temporary = tempfile.mkstemp(prefix="." + path.name + ".", suffix=".tmp", dir=str(path.parent))
    try:
        with os.fdopen(handle, "w", encoding="utf-8", newline="\n") as stream:
            stream.write(text)
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def coverage_relevant(path: str) -> bool:
    """Whether a tracked path can change what the instrumented run measures or how the floor judges it.

    Only the baseline file and documentation are exempt: Markdown files and the wiki/ and docs/
    trees. Source, manifests, the lockfile, the toolchain file, the gate script with its ignore
    regex, this tool, and the workflow are all relevant, so a record measured on a tree that
    differs in any of them cannot change a floor.
    """
    normalized = coverage_reports._normalize(path)
    if normalized == coverage_model.BASELINE_PATH or normalized.lower().endswith(".md"):
        return False
    return not any(coverage_reports._within(normalized, root) for root in ("wiki", "docs"))


def _git(root: str, *args: str) -> Optional[bytes]:
    """Run one read-only git command in `root`; None when git or the repository is unavailable."""
    try:
        completed = subprocess.run(["git", "-C", root, *args], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   check=False, timeout=coverage_model.GIT_TIMEOUT_S)
    except (OSError, subprocess.SubprocessError):
        return None
    return completed.stdout if completed.returncode == 0 else None


def _git_text(root: str, *args: str) -> Optional[str]:
    """Return a git command's stripped text output, or None."""
    output = _git(root, *args)
    text = output.decode("utf-8", "replace").strip() if output is not None else ""
    return text or None


def tree_entries(root: str, rev: str = "HEAD") -> Optional[Dict[str, str]]:
    """Map every path in `rev`'s tree to `mode object-id`, or None outside a Git checkout.

    Names keep their exact bytes through surrogateescape, so the tree can be rehashed from the map.
    """
    output = _git(root, "ls-tree", "-r", "-z", "--full-tree", rev)
    if output is None:
        return None
    entries: Dict[str, str] = {}
    for item in output.split(b"\0"):
        if not item:
            continue
        meta, _, name = item.partition(b"\t")
        mode, _kind, object_id = meta.decode("ascii", "replace").split(" ")
        entries[name.decode("utf-8", "surrogateescape")] = f"{mode} {object_id}"
    return entries


def worktree_changes(root: str) -> Optional[List[str]]:
    """List paths that differ from HEAD in the index or worktree, untracked files included; None outside Git."""
    output = _git(root, "status", "--porcelain=v1", "-z", "--untracked-files=all")
    if output is None:
        return None
    fields = output.split(b"\0")
    changed = set()
    index = 0
    while index < len(fields):
        field = fields[index]
        index += 1
        if len(field) < 4:
            continue
        status, name = field[:2], field[3:]
        changed.add(name.decode("utf-8", "surrogateescape"))
        # A rename or copy entry carries its source path in the next field, which also differs from HEAD.
        if b"R" in status or b"C" in status:
            if index < len(fields) and fields[index]:
                changed.add(fields[index].decode("utf-8", "surrogateescape"))
            index += 1
    return sorted(changed)


def file_sha256(path: str) -> str:
    """Return the SHA-256 of a file's bytes."""
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as handle:
            for chunk in iter(lambda: handle.read(1 << 20), b""):
                digest.update(chunk)
    except OSError as error:
        raise coverage_model.InputError(f"cannot read {path}: {error.strerror or error}") from error
    return digest.hexdigest()


def workflow_path(env: Mapping[str, str]) -> Optional[str]:
    """Return the workflow file path from GITHUB_WORKFLOW_REF, which reads `owner/repo/PATH@ref`."""
    ref, repository = env.get("GITHUB_WORKFLOW_REF") or "", env.get("GITHUB_REPOSITORY") or ""
    if not ref or not repository or not ref.startswith(repository + "/"):
        return None
    return ref[len(repository) + 1:].split("@", 1)[0] or None


def checkout_state(root: str, entries: bool = True) -> dict:
    """Sample the checkout's commit, its tree, the tree's path map, and its uncommitted changes, consistently."""
    commit = _git_text(root, "rev-parse", "--verify", "HEAD^{commit}")
    return {
        "commit": commit,
        "tree": _git_text(root, "rev-parse", f"{commit}^{{tree}}") if commit else None,
        "tree_entries": (tree_entries(root, commit) if commit else None) if entries else None,
        "worktree_changes": worktree_changes(root),
    }


def load_pin(path: str) -> dict:
    """Read the checkout pin that pin-checkout wrote before any phase."""
    document = coverage_reports._read_json(path, "checkout pin")
    if not isinstance(document, dict) or document.get("schema") != coverage_model.PIN_SCHEMA or any(
            key not in document for key in coverage_model.PIN_KEYS):
        raise coverage_model.InputError(f"checkout pin {path} is not a {coverage_model.PIN_SCHEMA} document")
    return document


def checkout_drift(pin: Mapping[str, object], root: str) -> List[str]:
    """Describe how HEAD, its tree, or the list of uncommitted coverage-relevant paths differs from the pin.

    It compares path names, not file contents or status, so it sees only the state at the moment it runs.
    """
    # Resolve this runtime-only dependency after module initialization.
    import coverage_provenance
    now = checkout_state(root, entries=False)
    drift = []
    if now["commit"] != pin["commit"]:
        drift.append(f"HEAD moved from {pin['commit']} to {now['commit']}")
    if now["tree"] != pin["tree"]:
        drift.append(f"the tree moved from {pin['tree']} to {now['tree']}")
    before = sorted(path for path in (pin["worktree_changes"] or []) if coverage_relevant(path))
    after = sorted(path for path in (now["worktree_changes"] or []) if coverage_relevant(path))
    if before != after:
        drift.append(f"the coverage-relevant uncommitted changes moved from [{coverage_provenance._listed(before)}] to "
                     f"[{coverage_provenance._listed(after)}]")
    return drift


def _hex_id(value: object, length: int) -> bool:
    """Whether `value` is a full lowercase hexadecimal object ID of `length` digits."""
    return isinstance(value, str) and len(value) == length and all(char in "0123456789abcdef" for char in value)


def git_tree_id(entries: Mapping[str, str], object_format: str) -> str:
    """Recompute the Git tree ID that a recursive path map of `mode object-id` entries hashes to.

    The nested trees are rebuilt as Git writes them: entries in byte order with a directory compared as
    `name/`, file modes as listed, and `40000` for a tree. A map Git could not have written raises
    InputError instead of hashing to some other tree.
    """
    length = coverage_model.OBJECT_ID_LENGTHS[object_format]
    root: dict = {}
    for path, value in entries.items():
        if not isinstance(path, str) or not isinstance(value, str):
            raise coverage_model.InputError("tree_entries must map each path to its mode and object id")
        mode, _, object_id = value.partition(" ")
        if mode not in coverage_model.GIT_FILE_MODES or not _hex_id(object_id, length):
            raise coverage_model.InputError(f"tree entry {path} has {value!r}, which is not a Git file mode and a full "
                             f"{object_format} object id")
        try:
            parts = path.encode("utf-8", "surrogateescape").split(b"/")
        except UnicodeEncodeError as error:
            raise coverage_model.InputError(f"tree entry {path!r} is not a path Git could store") from error
        if any(part in (b"", b".", b"..") for part in parts):
            raise coverage_model.InputError(f"tree entry {path!r} is not a normalized repository path")
        node = root
        for part in parts[:-1]:
            node = node.setdefault(part, {})
            if not isinstance(node, dict):
                raise coverage_model.InputError(f"tree entry {path} lies under a file")
        if parts[-1] in node:
            raise coverage_model.InputError(f"tree entry {path} collides with a directory")
        node[parts[-1]] = (mode.encode("ascii"), bytes.fromhex(object_id))
    hasher = getattr(hashlib, object_format)

    def write(node: dict) -> bytes:
        body = b"".join(
            (b"40000 " + name + b"\0" + write(node[name])) if isinstance(node[name], dict)
            else (node[name][0] + b" " + name + b"\0" + node[name][1])
            for name in sorted(node, key=lambda item: item + b"/" if isinstance(node[item], dict) else item))
        return hasher(b"tree %d\0" % len(body) + body).digest()

    return write(root).hex()


def object_format(root: str) -> str:
    """Return the checkout's object format; a Git too old to report one supports only SHA-1."""
    reported = _git_text(root, "rev-parse", "--show-object-format")
    return reported if reported and not reported.startswith("-") else "sha1"


def identity_problems(document: dict, root: str) -> List[str]:
    """Refuse object IDs that are not full lowercase IDs in the checkout's format, and a path map that
    does not hash to the recorded tree."""
    form = object_format(root)
    if form not in coverage_model.OBJECT_ID_LENGTHS:
        return [f"this checkout's object format {form} is not one the tool can hash"]
    problems = [f"the record's {name} {document[name]!r} is not a full lowercase {form} object id"
                for name in ("commit", "tree", "pull_request_head")
                if document.get(name) is not None and not _hex_id(document[name], coverage_model.OBJECT_ID_LENGTHS[form])]
    try:
        recomputed = git_tree_id(document["tree_entries"], form)
    except coverage_model.InputError as error:
        problems.append(f"the record's tree_entries cannot be hashed: {error}")
    else:
        if recomputed != document["tree"]:
            problems.append(f"the record's tree_entries hash to tree {recomputed}, not the recorded tree "
                            f"{document['tree']}")
    return problems
