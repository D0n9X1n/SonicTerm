#!/usr/bin/env python3
"""Reject workflow supply-chain regressions: mutable action refs, stray write grants and
unverified downloaded executables.

Three properties decay silently once they are only conventions.

A `uses:` tag is a *pointer*, not a version. `@v2` can be retargeted at any
commit by whoever holds the upstream repository, so a compromised or coerced
maintainer account changes what this repository executes without any change
landing here to review. A 40-character commit SHA is content-addressed and
cannot be retargeted, which is why it is the only form accepted below.

The second property is blast radius. The token every third-party action
inherits is the workflow's, so a `contents: write` default hands write access to
every action in every job — including the ones that merely compile and package.
Write is therefore allowed only on the specific jobs and scopes enumerated in
`WRITE_BOUNDARY`; widening either is a reviewable edit here rather than an
unnoticed line in a workflow.

The third is what a workflow runs that no action pin covers: an executable it
downloads itself. Text cannot prove what a shell will run, so a workflow may not
download at all. A `curl` or `wget` in a `run` block fails, except the exact
read-only API calls in `API_READS`, and so do `gh release download`,
`cargo binstall`, inline Python naming a network module, and
`actions/download-artifact` from another repository or run. This is a guardrail
against common mistakes, not a boundary; reviewing workflow changes is the
control. Executables come only from `scripts/install-pinned-tool.sh <tool>`,
which takes its version, URL and SHA-256 from `scripts/pinned-tools.json`; every
manifest field must be present and well formed, and each tool must be installed
by that script alone.

Stdlib only, and no network: the Ubuntu CI container installs a bare `python3`,
and a gate that needs the network fails for reasons unrelated to the property
it checks.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
from pathlib import Path
import re
import sys

# A commit SHA, in the only spelling Git resolves here: full length, lowercase.
# An abbreviated SHA is rejected deliberately — it is a *prefix*, and a prefix
# can gain a second match as the upstream repository grows.
SHA_PIN = re.compile(r"^[0-9a-f]{40}$")

# The trailing comment must name a human version so a reviewer can read the pin
# without resolving it, and so Dependabot has the token it rewrites.
VERSION_TOKEN = re.compile(r"^v?\d+(?:\.\d+){0,2}(?:[-+][0-9A-Za-z.-]+)?$")
DOCKER_DIGEST = re.compile(r"^docker://.+@sha256:[0-9a-f]{64}$")

# The only jobs permitted to hold a write-scoped token, as (workflow, job).
# Both publish an artifact the repository has already validated; every other
# job in every workflow runs with the read-only default.
WRITE_BOUNDARY = {
    ("publish-wiki.yml", "publish"): frozenset({"contents"}),
    ("release.yml", "publish"): frozenset({"contents"}),
}

# Read-only GitHub API calls a workflow may make, as (workflow, the logical `run` line exactly).
# Each writes no file and pipes only into a first-party reader of the JSON.
API_READS = (
    (
        "release.yml",
        'curl --fail-with-body --silent --show-error --header "Accept: application/vnd.github+json" '
        '--header "Authorization: Bearer ${GH_TOKEN}" --header "X-GitHub-Api-Version: 2022-11-28" '
        '"https://api.github.com/repos/${GITHUB_REPOSITORY}/actions/workflows/ci.yml/runs?branch=main'
        '&event=push&status=success&head_sha=${RELEASE_SHA}&per_page=100" | '
        'python3 scripts/prepare-release-assets.py check-main-ci --sha "$RELEASE_SHA"',
    ),
)
# A command word that downloads, as its basename.
DOWNLOADERS = frozenset({"curl", "wget", "curl.exe", "wget.exe", "invoke-webrequest", "iwr"})
# The download rule is a guardrail against the common ways a workflow fetches a tool, not a proof
# that none can; reviewing workflow changes is the control. These name the other common ways.
GH_COMMANDS = frozenset({"gh", "gh.exe"})
BINSTALL_COMMANDS = frozenset({"cargo-binstall", "cargo-binstall.exe"})
# Python started with inline source: `-c` and its quote, or a heredoc and its delimiter.
PYTHON_INLINE = re.compile(
    r"(?<![\w.-])(?:\S*/)?python3?(?:\.\d+)?\s(?:.*?\s)?"
    r"(?:-c\s*(?P<quote>['\"])|<<-?\s*(?P<mark>['\"]?)(?P<delimiter>\w+)(?P=mark))"
)
PYTHON_NETWORK = re.compile(r"\b(?:urllib|urlretrieve|requests|http\.client)\b")
DOWNLOAD_ARTIFACT = re.compile(r"^actions/download-artifact@")
# Inputs that point actions/download-artifact outside the current workflow run.
FOREIGN_ARTIFACT_INPUT = re.compile(r"(?:^|[\s{,])(?:repository|run-id)\s*:")
# A package-manager install, whose arguments name packages (such as `curl`) rather than commands.
PACKAGE_INSTALL = re.compile(r"^(?:sudo\s+)?(?:apt-get|apt|brew|dnf|yum|apk|choco)\s+(?:-\S+\s+)*install\b")
PACKAGE_WORD = re.compile(r"^(?:-[A-Za-z0-9-]+(?:=\S+)?|[A-Za-z0-9][A-Za-z0-9.+_-]*)$")
SHELL_SEPARATOR = re.compile(r"\|\||&&|[|;&()`]|\$\(")
PINNED_MANIFEST = "scripts/pinned-tools.json"
PINNED_INSTALL = re.compile(r"^bash scripts/install-pinned-tool\.sh(?:\s+(?P<arguments>.*))?$")
EXACT_VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
# The host names scripts/install-pinned-tool.sh maps `uname` to.
PINNED_OS = frozenset({"linux", "macos"})
PINNED_ARCH = frozenset({"aarch64", "x86_64"})
SHA256_DIGEST = re.compile(r"^[0-9a-f]{64}$")
RELEASE_URL = re.compile(
    r"^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/releases/download/[^/]*\{version\}[^/]*/[^/]+$"
)

# Sentinel key for the inline spellings (`permissions: read-all`), which carry
# their value on the `permissions:` line itself rather than in a nested block.
INLINE = "__inline__"


@dataclass(frozen=True)
class Finding:
    """One violation, addressed by file and line so it is directly navigable."""

    path: str
    line: int
    message: str

    def render(self) -> str:
        return f"{self.path}:{self.line}: {self.message}"


def workflow_paths(root: Path) -> list[Path]:
    """Every workflow file, both YAML spellings, in a stable order."""
    directory = root / ".github" / "workflows"
    return sorted(
        path for pattern in ("*.yml", "*.yaml") for path in directory.glob(pattern)
    )


@dataclass(frozen=True)
class MappingEntry:
    """One directly written YAML mapping entry."""

    indent: int
    sequence: bool
    key: str
    value: str


@dataclass(frozen=True)
class StructuralLine:
    """A YAML line outside comments and block-scalar bodies."""

    index: int
    text: str
    indent: int
    entry: MappingEntry | None


BLOCK_SCALAR = re.compile(r"^[|>](?:[1-9][+-]?|[+-][1-9]?)?$")
SIMPLE_KEY = re.compile(r"^[A-Za-z0-9_-]+$|^<<$")


def _split_comment(text: str) -> tuple[str, str]:
    """Split an unquoted YAML comment from a scalar."""
    single = False
    double = False
    escaped = False
    index = 0
    while index < len(text):
        char = text[index]
        if double:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                double = False
        elif single:
            if char == "'":
                if index + 1 < len(text) and text[index + 1] == "'":
                    index += 1
                else:
                    single = False
        elif char == '"':
            double = True
        elif char == "'":
            single = True
        elif char == "#" and (index == 0 or text[index - 1].isspace()):
            return text[:index].rstrip(), text[index + 1 :].strip()
        index += 1
    return text.rstrip(), ""


def _decode_scalar(token: str) -> str | None:
    """Decode the plain and quoted scalar forms accepted by this checker."""
    token = token.strip()
    if not token:
        return ""
    if token.startswith('"'):
        try:
            value = json.loads(token)
        except (json.JSONDecodeError, TypeError):
            return None
        return value if isinstance(value, str) else None
    if token.startswith("'"):
        if len(token) < 2 or not token.endswith("'"):
            return None
        return token[1:-1].replace("''", "'")
    return token


def _quoted_key(text: str) -> tuple[str, str] | None:
    """Decode a quoted mapping key and return its remaining text."""
    quote = text[0]
    if quote == '"':
        escaped = False
        for index in range(1, len(text)):
            char = text[index]
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                key = _decode_scalar(text[: index + 1])
                return (key, text[index + 1 :]) if key is not None else None
        return None
    index = 1
    while index < len(text):
        if text[index] == quote:
            if index + 1 < len(text) and text[index + 1] == quote:
                index += 2
                continue
            key = _decode_scalar(text[: index + 1])
            return (key, text[index + 1 :]) if key is not None else None
        index += 1
    return None


def _mapping_entry(line: str) -> MappingEntry | None:
    """Parse one block-style mapping entry, including a sequence-item entry."""
    indent = len(line) - len(line.lstrip(" "))
    text = line[indent:]
    sequence = False
    if text.startswith("-") and (len(text) == 1 or text[1].isspace()):
        sequence = True
        text = text[1:].lstrip(" ")
    if not text or text.startswith(("#", "?", "{")):
        return None

    if text[0] in "\"'":
        parsed = _quoted_key(text)
        if parsed is None:
            return None
        key, remainder = parsed
        remainder = remainder.lstrip(" ")
        if not remainder.startswith(":"):
            return None
        value = remainder[1:].lstrip(" ")
    else:
        separator = next(
            (
                index
                for index, char in enumerate(text)
                if char == ":" and (index + 1 == len(text) or text[index + 1].isspace())
            ),
            None,
        )
        if separator is None:
            return None
        key = text[:separator].strip()
        if not SIMPLE_KEY.fullmatch(key):
            return None
        value = text[separator + 1 :].lstrip(" ")
    return MappingEntry(indent, sequence, key, value)


def _structural_lines(lines: list[str]) -> list[StructuralLine]:
    """Return YAML structure while excluding block-scalar payloads."""
    result: list[StructuralLine] = []
    scalar_indent: int | None = None
    for index, line in enumerate(lines):
        if scalar_indent is not None:
            if not line.strip():
                continue
            indent = len(line) - len(line.lstrip(" "))
            if indent > scalar_indent:
                continue
            scalar_indent = None
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip(" "))
        entry = _mapping_entry(line)
        result.append(StructuralLine(index, line, indent, entry))
        if entry is not None:
            value, _ = _split_comment(entry.value)
            if BLOCK_SCALAR.fullmatch(value.strip()):
                scalar_indent = entry.indent
    return result


def _direct_entries(
    structural: list[StructuralLine], start: int, end: int, parent_indent: int
) -> list[int]:
    """Positions of direct mapping children within one block."""
    child_indent: int | None = None
    positions: list[int] = []
    for position in range(start + 1, end):
        item = structural[position]
        if item.indent <= parent_indent:
            break
        if child_indent is None:
            child_indent = item.indent
        if item.indent == child_indent:
            positions.append(position)
    return positions


def _permission_scopes(
    path: Path,
    structural: list[StructuralLine],
    position: int,
    end: int,
) -> tuple[dict[str, str], list[Finding]]:
    """Read one canonical permissions scalar or direct block."""
    item = structural[position]
    entry = item.entry
    if entry is None:
        return {}, [Finding(path.name, item.index + 1, "permissions key is not directly auditable")]
    raw, _ = _split_comment(entry.value)
    if raw.strip():
        value = _decode_scalar(raw)
        if value not in {"read-all", "write-all"}:
            return {}, [
                Finding(
                    path.name,
                    item.index + 1,
                    "permissions must use a direct block or read-all/write-all scalar",
                )
            ]
        return {INLINE: value}, []

    scopes: dict[str, str] = {}
    findings: list[Finding] = []
    for child_position in _direct_entries(structural, position, end, entry.indent):
        child = structural[child_position]
        child_entry = child.entry
        if child_entry is None or child_entry.sequence:
            findings.append(
                Finding(path.name, child.index + 1, "permissions block is not directly auditable")
            )
            continue
        if child_entry.key in scopes:
            findings.append(
                Finding(
                    path.name,
                    child.index + 1,
                    f"permissions scope '{child_entry.key}' is declared more than once",
                )
            )
            continue
        value_raw, _ = _split_comment(child_entry.value)
        value = _decode_scalar(value_raw)
        if value not in {"read", "write", "none"}:
            findings.append(
                Finding(
                    path.name,
                    child.index + 1,
                    f"permissions scope '{child_entry.key}' has unsupported value {value_raw!r}",
                )
            )
            continue
        scopes[child_entry.key] = value
    return scopes, findings


def write_scopes(scopes: dict[str, str]) -> set[str]:
    """Normalized permission scopes that hand out write access."""
    if scopes.get(INLINE) == "write-all":
        return {INLINE}
    return {scope for scope, value in scopes.items() if value == "write"}


def grants_write(scopes: dict[str, str]) -> bool:
    """Whether normalized permission scopes hand out write access."""
    return bool(write_scopes(scopes))


def check_yaml_shape(path: Path, lines: list[str]) -> list[Finding]:
    """Reject valid YAML forms that can hide security-sensitive structure."""
    findings: list[Finding] = []
    for item in _structural_lines(lines):
        leading = item.text[: len(item.text) - len(item.text.lstrip())]
        stripped = item.text.lstrip()
        if "\t" in leading:
            findings.append(Finding(path.name, item.index + 1, "tabs are not allowed in YAML indentation"))
        if stripped.startswith("?"):
            findings.append(
                Finding(path.name, item.index + 1, "explicit YAML mapping keys are not supported")
            )
        is_sequence = stripped == "-" or (
            stripped.startswith("-") and len(stripped) > 1 and stripped[1].isspace()
        )
        sequence_value = stripped[1:].lstrip() if is_sequence else stripped
        if sequence_value.startswith("?"):
            findings.append(
                Finding(path.name, item.index + 1, "explicit YAML mapping keys are not supported")
            )
        if sequence_value.startswith("{"):
            findings.append(
                Finding(
                    path.name,
                    item.index + 1,
                    "flow-style sequence mappings are not supported; write each key on its own line",
                )
            )
        if sequence_value.startswith(("&", "*")):
            findings.append(
                Finding(path.name, item.index + 1, "YAML anchors and aliases are not supported")
            )
        if sequence_value.startswith("!"):
            findings.append(
                Finding(path.name, item.index + 1, "explicit YAML type tags are not supported")
            )
        entry = item.entry
        if entry is None:
            scalar = sequence_value if is_sequence else ""
            scalar_value, _ = _split_comment(scalar)
            if not scalar or _decode_scalar(scalar_value) is None:
                findings.append(
                    Finding(
                        path.name,
                        item.index + 1,
                        "YAML structure is outside the directly auditable grammar",
                    )
                )
            continue
        value, _ = _split_comment(entry.value)
        value = value.strip()
        if entry.key == "<<":
            findings.append(Finding(path.name, item.index + 1, "YAML merge keys are not supported"))
        if value.startswith("{"):
            findings.append(
                Finding(
                    path.name,
                    item.index + 1,
                    "flow-style mappings are not supported; write each key on its own line",
                )
            )
        if value.startswith(("&", "*")):
            findings.append(
                Finding(path.name, item.index + 1, "YAML anchors and aliases are not supported")
            )
        if value.startswith("!"):
            findings.append(
                Finding(path.name, item.index + 1, "explicit YAML type tags are not supported")
            )
        if entry.key in {"jobs", "steps"} and value:
            findings.append(
                Finding(path.name, item.index + 1, f"{entry.key} must use a direct block mapping")
            )
    return findings


def check_refs(path: Path, lines: list[str], pins: dict[str, set[str]]) -> list[Finding]:
    """Verify every directly written `uses` value is immutable and legible."""
    findings: list[Finding] = []
    for item in _structural_lines(lines):
        entry = item.entry
        if entry is None or entry.key != "uses":
            continue
        raw, comment = _split_comment(entry.value)
        ref = _decode_scalar(raw)
        number = item.index + 1
        if not ref or ref != ref.strip() or any(char.isspace() for char in ref):
            findings.append(
                Finding(path.name, number, "uses value must be one directly written scalar")
            )
            continue

        # A local action ships in this repository and is reviewed by the same
        # pull request that changes it, so there is no external ref to pin.
        if ref.startswith("./"):
            continue

        if ref.startswith("docker://"):
            if not DOCKER_DIGEST.fullmatch(ref):
                findings.append(
                    Finding(path.name, number, f"docker reference is not digest-pinned: {ref}")
                )
            continue

        action, separator, revision = ref.rpartition("@")
        if not separator:
            findings.append(
                Finding(path.name, number, f"action reference declares no revision: {ref}")
            )
            continue

        if not SHA_PIN.fullmatch(revision):
            findings.append(
                Finding(
                    path.name,
                    number,
                    f"{action} uses the mutable ref '{revision}'; pin a full "
                    "40-character lowercase commit SHA with a trailing '# version' comment",
                )
            )
            continue

        token = comment.split()[0] if comment else ""
        if not VERSION_TOKEN.fullmatch(token):
            findings.append(
                Finding(
                    path.name,
                    number,
                    f"{action} is pinned but carries no trailing version comment; "
                    "append '# vX.Y.Z' naming the release this SHA is",
                )
            )

        pins.setdefault(action, set()).add(revision)

    return findings


def check_permissions(path: Path, lines: list[str]) -> list[Finding]:
    """Verify read-only workflow defaults and the enumerated write boundary."""
    findings: list[Finding] = []
    structural = _structural_lines(lines)
    root_positions = [
        position
        for position, item in enumerate(structural)
        if item.indent == 0 and item.entry is not None and not item.entry.sequence
    ]
    jobs_positions = [
        position
        for position in root_positions
        if structural[position].entry.key == "jobs"
    ]
    if len(jobs_positions) != 1:
        return [Finding(path.name, 1, "workflow must declare exactly one direct jobs block")]
    jobs_position = jobs_positions[0]
    jobs_entry = structural[jobs_position].entry
    jobs_raw, _ = _split_comment(jobs_entry.value)
    if jobs_raw.strip():
        findings.append(
            Finding(path.name, structural[jobs_position].index + 1, "jobs must use a direct block mapping")
        )

    top_permissions = [
        position
        for position in root_positions
        if structural[position].entry.key == "permissions"
    ]
    if not top_permissions:
        findings.append(
            Finding(
                path.name,
                1,
                "workflow declares no top-level permissions block; the repository "
                "default would apply and can be widened outside this file",
            )
        )
    elif len(top_permissions) > 1:
        findings.append(
            Finding(path.name, structural[top_permissions[1]].index + 1, "top-level permissions is declared more than once")
        )
    for position in top_permissions:
        scopes, scope_findings = _permission_scopes(
            path, structural, position, len(structural)
        )
        findings.extend(scope_findings)
        if grants_write(scopes):
            findings.append(
                Finding(
                    path.name,
                    structural[position].index + 1,
                    "workflow-level permissions grant write, which extends that "
                    "token to every job and every action they run",
                )
            )

    jobs_end = next(
        (
            position
            for position in range(jobs_position + 1, len(structural))
            if structural[position].indent <= jobs_entry.indent
        ),
        len(structural),
    )
    job_positions = _direct_entries(
        structural, jobs_position, jobs_end, jobs_entry.indent
    )
    for job_offset, job_position in enumerate(job_positions):
        job_item = structural[job_position]
        job_entry = job_item.entry
        if job_entry is None or job_entry.sequence:
            findings.append(
                Finding(path.name, job_item.index + 1, "job definition is not directly auditable")
            )
            continue
        job = job_entry.key
        raw, _ = _split_comment(job_entry.value)
        if raw.strip():
            findings.append(
                Finding(path.name, job_item.index + 1, f"job '{job}' must use a direct block mapping")
            )
            continue
        job_end = job_positions[job_offset + 1] if job_offset + 1 < len(job_positions) else jobs_end
        children = _direct_entries(structural, job_position, job_end, job_entry.indent)
        permission_positions = [
            position
            for position in children
            if structural[position].entry is not None
            and structural[position].entry.key == "permissions"
        ]
        if len(permission_positions) > 1:
            findings.append(
                Finding(
                    path.name,
                    structural[permission_positions[1]].index + 1,
                    f"job '{job}' declares permissions more than once",
                )
            )
        for position in permission_positions:
            scopes, scope_findings = _permission_scopes(
                path, structural, position, job_end
            )
            findings.extend(scope_findings)
            writes = write_scopes(scopes)
            if not writes:
                continue
            allowed = WRITE_BOUNDARY.get((path.name, job))
            if allowed is None:
                findings.append(
                    Finding(
                        path.name,
                        structural[position].index + 1,
                        f"job '{job}' grants write outside the documented publish "
                        "boundary; narrow the job or add it to WRITE_BOUNDARY in "
                        "scripts/check-workflow-supply-chain.py with a reason",
                    )
                )
            elif not writes <= allowed:
                findings.append(
                    Finding(
                        path.name,
                        structural[position].index + 1,
                        f"job '{job}' grants write scopes {sorted(writes)} beyond "
                        f"its allowed scopes {sorted(allowed)}",
                    )
                )

    return findings


def run_blocks(lines: list[str]) -> list[list[tuple[int, str]]]:
    """Every `run` payload as its stripped raw lines `(raw index, text)`, one list per step."""
    structural = _structural_lines(lines)
    blocks: list[list[tuple[int, str]]] = []
    for position, item in enumerate(structural):
        if item.entry is None or item.entry.key != "run":
            continue
        value = _split_comment(item.entry.value)[0].strip()
        if BLOCK_SCALAR.fullmatch(value):
            end = structural[position + 1].index if position + 1 < len(structural) else len(lines)
            blocks.append([(index, lines[index].strip()) for index in range(item.index + 1, end)])
        else:
            blocks.append([(item.index, _decode_scalar(value) or "")])
    return blocks


def run_lines(lines: list[str]) -> list[tuple[int, str]]:
    """Every `run` payload as logical shell lines `(raw index, text)`: comment lines dropped,
    `\\` continuations joined, runs of whitespace collapsed to one space."""
    logical: list[tuple[int, str]] = []
    for raw in run_blocks(lines):
        pending: tuple[int, str] | None = None
        for index, text in raw:
            if not text or (pending is None and text.startswith("#")):
                continue
            start, joined = pending if pending is not None else (index, "")
            joined = " ".join(f"{joined} {text}".split())
            if joined.endswith("\\"):
                pending = (start, joined[:-1].rstrip())
                continue
            pending = None
            logical.append((start, joined))
        if pending is not None:
            logical.append(pending)
    return logical


def _basename(word: str) -> str:
    """A command word's basename, lowercased, without surrounding quotes."""
    return re.split(r"[/\\]", word.strip("\"'"))[-1].lower()


def _download_tool(text: str) -> str | None:
    """The downloader a logical line runs in any command, outside a package install's package
    names: `curl or wget`, `` `gh release download` `` or `` `cargo binstall` ``."""
    for command in SHELL_SEPARATOR.split(text):
        words = command.split()
        while words and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=\S*", words[0]):
            words = words[1:]
        if not words:
            continue
        head = _basename(words[0])
        if head in GH_COMMANDS and " release download " in f" {' '.join(words[1:])} ":
            return "`gh release download`"
        subcommands = [word for word in words[1:] if not word.startswith("+")]
        if head in BINSTALL_COMMANDS or (head == "cargo" and subcommands[:1] == ["binstall"]):
            return "`cargo binstall`"
        if PACKAGE_INSTALL.match(" ".join(words)):
            start = next(index for index, word in enumerate(words) if word == "install") + 1
            if all(PACKAGE_WORD.fullmatch(word) for word in words[start:]):
                continue
        if any(_basename(word) in DOWNLOADERS for word in words):
            return "curl or wget"
    return None


def _inline_python(raw: list[tuple[int, str]]) -> list[int]:
    """Raw indexes where Python starts with inline source (`-c` or a heredoc) that names a
    network module. The source runs to the closing quote or the heredoc delimiter line."""
    found: list[int] = []
    for position, (index, text) in enumerate(raw):
        match = PYTHON_INLINE.search(text)
        if match is None:
            continue
        if match.group("quote"):
            source = text[match.end():]
            following = iter(raw[position + 1:])
            while match.group("quote") not in source:
                line = next(following, None)
                if line is None:
                    break
                source += "\n" + line[1]
            source = source.split(match.group("quote"))[0]
        else:
            body = []
            for _, line in raw[position + 1:]:
                if line == match.group("delimiter"):
                    break
                body.append(line)
            source = "\n".join(body)
        if PYTHON_NETWORK.search(source):
            found.append(index)
    return found


def check_downloads(path: Path, lines: list[str]) -> list[Finding]:
    """A workflow downloads nothing itself, except the exact read-only calls in `API_READS`."""
    allowed = {text for workflow, text in API_READS if workflow == path.name}
    advice = "install executables only with `bash scripts/install-pinned-tool.sh <tool>`"
    findings = []
    for index, text in run_lines(lines):
        tool = _download_tool(text)
        if tool is None or text in allowed:
            continue
        extra = ", or add an exact read-only call to API_READS" if tool == "curl or wget" else ""
        findings.append(Finding(path.name, index + 1, f"downloads with {tool}; {advice}{extra}"))
    for raw in run_blocks(lines):
        findings.extend(
            Finding(path.name, index + 1, "downloads with inline Python (urllib, requests or "
                    f"http.client); {advice}")
            for index in _inline_python(raw)
        )
    return findings + _foreign_artifacts(path, lines)


def _foreign_artifacts(path: Path, lines: list[str]) -> list[Finding]:
    """Each actions/download-artifact step with a `repository` or `run-id` input, which pulls
    from outside this run. Same-run artifacts were built by this workflow and pass."""
    structural = _structural_lines(lines)
    findings = []
    for position, item in enumerate(structural):
        entry = item.entry
        value = _split_comment(entry.value)[0].strip().strip("\"'") if entry else ""
        if entry is None or entry.key != "uses" or not DOWNLOAD_ARTIFACT.match(value):
            continue
        # The step is the nearest sequence item at or above `uses`; it ends at the next line
        # indented no deeper than its dash.
        start = next(
            (back for back in range(position, -1, -1)
             if structural[back].entry is not None and structural[back].entry.sequence
             and structural[back].indent <= item.indent),
            position,
        )
        dash = structural[start].indent
        end = next((structural[after].index for after in range(start + 1, len(structural))
                    if structural[after].indent <= dash), len(lines))
        if any(FOREIGN_ARTIFACT_INPUT.search(_split_comment(lines[index])[0])
               for index in range(structural[start].index, end)):
            findings.append(Finding(path.name, item.index + 1, "downloads an artifact from another "
                                    "repository or run; download only artifacts this run built"))
    return findings


def _manifest(root: Path) -> tuple[dict, list[Finding]]:
    """The pinned-tool manifest, with a finding for each malformed entry."""
    path = root / PINNED_MANIFEST
    if not path.exists():
        return {}, []
    try:
        tools = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        return {}, [Finding(PINNED_MANIFEST, error.lineno, f"is not valid JSON: {error.msg}")]
    if not isinstance(tools, dict):
        return {}, [Finding(PINNED_MANIFEST, 1, "must map tool names to entries")]
    findings = []
    for tool, entry in tools.items():
        entry = entry if isinstance(entry, dict) else {}
        if not EXACT_VERSION.fullmatch(str(entry.get("version", ""))):
            findings.append(Finding(PINNED_MANIFEST, 1, f"{tool}: version must be an exact X.Y.Z"))
        if not SHA256_DIGEST.fullmatch(str(entry.get("sha256", ""))):
            findings.append(Finding(PINNED_MANIFEST, 1, f"{tool}: sha256 must be 64 lowercase hex digits"))
        if entry.get("os") not in PINNED_OS:
            findings.append(Finding(PINNED_MANIFEST, 1, f"{tool}: os must be one of {sorted(PINNED_OS)}"))
        if entry.get("arch") not in PINNED_ARCH:
            findings.append(Finding(PINNED_MANIFEST, 1, f"{tool}: arch must be one of {sorted(PINNED_ARCH)}"))
        member = entry.get("member")
        if (not isinstance(member, str) or not member or "\n" in member or member.startswith("/")
                or ".." in member.split("/")):
            findings.append(Finding(PINNED_MANIFEST, 1,
                                    f"{tool}: member must be a relative path inside the archive"))
        arguments = entry.get("version_args")
        if not isinstance(arguments, list) or not arguments or not all(
            isinstance(argument, str) and argument and "\n" not in argument for argument in arguments
        ):
            findings.append(Finding(PINNED_MANIFEST, 1,
                                    f"{tool}: version_args must be a non-empty list of strings"))
        output = entry.get("version_output")
        if not isinstance(output, str) or not output or "\n" in output:
            findings.append(Finding(PINNED_MANIFEST, 1, f"{tool}: version_output must be a non-empty string"))
        if not RELEASE_URL.fullmatch(str(entry.get("url", ""))):
            findings.append(Finding(
                PINNED_MANIFEST, 1,
                f"{tool}: url must be a pinned https release URL "
                "(https://github.com/<owner>/<repo>/releases/download/<tag with {version}>/<asset>)",
            ))
    return tools, findings


def check_pinned_tools(root: Path, workflows: dict[str, list[str]]) -> list[Finding]:
    """Every manifest tool is installed, and only by `bash scripts/install-pinned-tool.sh <tool>`."""
    tools, findings = _manifest(root)
    installed: set[str] = set()
    for name, lines in workflows.items():
        for index, text in run_lines(lines):
            if "install-pinned-tool" in text:
                match = PINNED_INSTALL.fullmatch(text)
                arguments = (match.group("arguments") or "").split() if match else []
                if match is None or len(arguments) != 1:
                    findings.append(Finding(name, index + 1, "runs scripts/install-pinned-tool.sh with "
                                            "something other than only a manifest tool name"))
                elif arguments[0] not in tools:
                    findings.append(Finding(name, index + 1,
                                            f"{arguments[0]} is not in {PINNED_MANIFEST}"))
                else:
                    installed.add(arguments[0])
                continue
            for tool in tools:
                if re.search(rf"(?<![\w-])(?:cargo\s+(?:install|binstall)\b.*\b){re.escape(tool)}(?![\w-])", text):
                    findings.append(Finding(name, index + 1, f"{tool} is installed other than by "
                                            "scripts/install-pinned-tool.sh"))
        for index, line in enumerate(lines):
            for tool in tools:
                if re.search(r"^\s*-?\s*uses:\s*\S*install-action", line) and tool in "\n".join(lines[index:index + 4]):
                    findings.append(Finding(name, index + 1, f"{tool} is installed other than by "
                                            "scripts/install-pinned-tool.sh"))
    for tool in sorted(set(tools) - installed):
        findings.append(Finding(PINNED_MANIFEST, 1, f"{tool} is never installed by "
                                "`bash scripts/install-pinned-tool.sh {tool}` in any workflow"))
    return findings


def check(root: Path) -> list[Finding]:
    """Run every workflow-shape rule over the whole workflow set."""
    paths = workflow_paths(root)
    if not paths:
        return [Finding(".github/workflows", 1, "no workflow files found to check")]

    findings: list[Finding] = []
    pins: dict[str, set[str]] = {}
    workflow_lines: dict[str, list[str]] = {}
    for path in paths:
        lines = path.read_text(encoding="utf-8").splitlines()
        findings.extend(check_yaml_shape(path, lines))
        findings.extend(check_refs(path, lines, pins))
        findings.extend(check_permissions(path, lines))
        findings.extend(check_downloads(path, lines))
        workflow_lines[path.name] = lines

    findings.extend(check_pinned_tools(root, workflow_lines))

    # One action on two SHAs means a Dependabot bump landed on some call sites
    # and not others, leaving the stragglers on the revision it moved away from.
    for action, revisions in sorted(pins.items()):
        if len(revisions) > 1:
            findings.append(
                Finding(
                    ".github/workflows",
                    1,
                    f"{action} is pinned to {len(revisions)} different commits "
                    f"({', '.join(sorted(revisions))}); every call site must "
                    "advance together",
                )
            )

    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root to check (default: this checkout)",
    )
    arguments = parser.parse_args()

    findings = check(arguments.root)
    if findings:
        print("FAILED: workflow supply-chain contract violated:", file=sys.stderr)
        for finding in findings:
            print(f"  {finding.render()}", file=sys.stderr)
        return 1

    paths = workflow_paths(arguments.root)
    print(
        f"check-workflow-supply-chain: {len(paths)} workflows, every remote action "
        "pinned to a commit SHA, write scoped to the publish boundary, no download outside "
        "scripts/install-pinned-tool.sh. OK."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
