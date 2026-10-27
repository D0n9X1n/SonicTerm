"""Perf workflow fixture."""

from __future__ import annotations
import json
import re
from pathlib import Path



# --- perf.yml as a job graph: a YAML reader, an `if:` evaluator and a push simulator ---------------

PERF_WORKFLOW = Path(__file__).resolve().parent.parent / ".github" / "workflows" / "perf.yml"
# Every perf.yml job's eligibility: a tag push, or a `perf` pull request whose event is not another label.
ELIGIBILITY = ("github.event_name == 'push' || (contains(github.event.pull_request.labels.*.name, 'perf') && "
               "(github.event.action != 'labeled' || github.event.label.name == 'perf'))")
PERF_JOBS = ("perf-build-macos", "compare-macos", "compare-windows", "perf-result")
COMPARISON_JOBS = ("compare-macos", "compare-windows")
REF_STEP = "Choose the refs and the run length"
PROFILE_STEP = "Relax the release profile for a pull request"
BUILD_STEP = "Build both refs once"
PACKAGE_STEP = "Package the binaries"
UPLOAD_BINARIES_STEP = "Upload the binaries"
BUILD_EVIDENCE_STEP = "Upload the build evidence"
PLAN_STEP = "Check the producer's refs"
DOWNLOAD_STEP = "Download the binaries"
UNPACK_STEP = "Unpack the binaries"
COMPARE_STEP = "Compare the base and the head"
SUMMARY_STEP = "Publish the table in the job summary"
EVIDENCE_STEP = "Upload the comparison evidence"
RESULT_STEP = "Require every comparison job to succeed"
# Every scenario set a comparison measures: each runs in exactly one shard per platform. The named variants
# run beside their scenario's default, since a bare ID selects only the default; S1/atlas-retry runs only
# its counters set.
ALL_SCENARIOS = ["S1", "S1/atlas-retry", "S1/status-line", "S2", "S2/flood", "S3", "S4", "S5", "S6", "S6/flood",
                 "S6/selection-drag", "S6/rest", "S7", "S8", "S9", "S10", "S10/sync", "S11", "S12", "S12/status-line"]
# The variants only one platform's shards add: S11/release on both (capped at 1), the presenter controls on Windows.
PLATFORM_SCENARIOS = {"macOS": ["S11/release"], "Windows": ["S11/release", "S11/gdi", "S11/wgpu"]}
JOB_RESULTS = ("success", "failure", "cancelled", "skipped", "")

_YAML_ENTRY = re.compile(r"(?P<key>[A-Za-z0-9_.-]+)\s*:(?:\s+(?P<value>.*))?$")


def _indent(line):
    """The number of leading spaces of a YAML line."""
    return len(line) - len(line.lstrip(" "))


def _strip_yaml_comment(text):
    """Drop a `#` comment that follows whitespace outside quotes."""
    quote = None
    for position, char in enumerate(text):
        if quote:
            if char == quote:
                quote = None
        elif char in "'\"" and (position == 0 or text[position - 1].isspace()):
            quote = char
        elif char == "#" and (position == 0 or text[position - 1].isspace()):
            return text[:position].rstrip()
    return text.strip()


def _flow_items(text):
    """Split a flow sequence's inside on commas outside quotes."""
    items, current, quote = [], "", None
    for char in text:
        if quote and char == quote:
            quote = None
        elif not quote and char in "'\"":
            quote = char
        if char == "," and not quote:
            items.append(current.strip())
            current = ""
        else:
            current += char
    if current.strip():
        items.append(current.strip())
    return items


def _yaml_scalar(text):
    """Decode a plain, quoted or flow-sequence scalar; plain scalars stay strings."""
    text = _strip_yaml_comment(text)
    if text.startswith('"'):
        return json.loads(text)
    if text.startswith("'"):
        return text[1:-1].replace("''", "'")
    if text.startswith("["):
        return [_yaml_scalar(item) for item in _flow_items(text[1:-1])]
    return text


class WorkflowYaml:
    """The YAML subset the workflows use: block mappings and sequences, plain, quoted and flow scalars, `|` and `>-`.

    PyYAML is not on every runner that runs these tests, and the supply-chain checker already confines the
    workflows to this directly auditable grammar; anything else raises instead of being guessed at.
    """

    def __init__(self, text):
        self.lines = text.splitlines()
        self.index = 0

    @classmethod
    def parse(cls, text):
        """Parse a whole document; a line left unread is an error."""
        reader = cls(text)
        document = reader.node(0)
        reader.skip_blank()
        if reader.index != len(reader.lines):
            raise ValueError(f"unparsed line {reader.index + 1}: {reader.lines[reader.index]!r}")
        return document

    def skip_blank(self):
        """Skip blank and comment lines between nodes (never inside a block scalar)."""
        while self.index < len(self.lines) and (not self.lines[self.index].strip()
                                                or self.lines[self.index].lstrip().startswith("#")):
            self.index += 1

    def node(self, minimum):
        """Read the mapping or sequence that starts at or deeper than `minimum`, or "" when there is none."""
        self.skip_blank()
        if self.index >= len(self.lines) or _indent(self.lines[self.index]) < minimum:
            return ""
        level = _indent(self.lines[self.index])
        text = self.lines[self.index].strip()
        return self.sequence(level) if text == "-" or text.startswith("- ") else self.mapping(level)

    def mapping(self, level):
        """Read `key: value` lines at exactly `level`."""
        result = {}
        while True:
            self.skip_blank()
            if self.index >= len(self.lines) or _indent(self.lines[self.index]) < level:
                return result
            line = self.lines[self.index]
            text = line.strip()
            match = _YAML_ENTRY.match(text)
            if _indent(line) > level or text.startswith("- ") or match is None:
                raise ValueError(f"line {self.index + 1} is not a mapping entry at indent {level}: {line!r}")
            self.index += 1
            key = match.group("key")
            if key in result:
                raise ValueError(f"duplicate key {key!r} at line {self.index}")
            result[key] = self.value(match.group("value") or "", level)

    def value(self, raw, level):
        """Read an entry's value: a block scalar, an inline scalar or a nested node."""
        raw = _strip_yaml_comment(raw)
        if raw in ("|", "|-", ">", ">-"):
            return self.block(raw, level)
        if raw:
            return _yaml_scalar(raw)
        return self.node(level + 1)

    def sequence(self, level):
        """Read `- item` lines at exactly `level`; an item whose line holds a key is a mapping at that key's column."""
        items = []
        while True:
            self.skip_blank()
            if self.index >= len(self.lines) or _indent(self.lines[self.index]) < level:
                return items
            line = self.lines[self.index]
            text = line.strip()
            if _indent(line) > level:
                raise ValueError(f"line {self.index + 1} is deeper than its sequence: {line!r}")
            if not (text == "-" or text.startswith("- ")):
                return items
            rest = text[1:].lstrip()
            column = _indent(line) + len(text) - len(rest)
            if not rest:
                self.index += 1
                items.append(self.node(level + 1))
            elif _YAML_ENTRY.match(rest) and not rest.startswith(("'", '"')):
                # The mapping's first key shares the dash's line: re-read the line as that key at its column.
                self.lines[self.index] = " " * column + rest
                items.append(self.mapping(column))
            else:
                self.index += 1
                items.append(_yaml_scalar(rest))

    def block(self, style, level):
        """Read a literal (`|`) or folded (`>`) block deeper than `level`; `-` drops the final newline."""
        collected = []
        while self.index < len(self.lines):
            line = self.lines[self.index]
            if line.strip() and _indent(line) <= level:
                break
            collected.append(line)
            self.index += 1
        while collected and not collected[-1].strip():
            collected.pop()
        if not collected:
            return ""
        content = min(_indent(line) for line in collected if line.strip())
        body = [line[content:] if line.strip() else "" for line in collected]
        ending = "" if style.endswith("-") else "\n"
        if style.startswith("|"):
            return "\n".join(body) + ending
        return " ".join(line.strip() for line in body if line.strip()) + ending


def load_perf_workflow():
    """perf.yml, parsed."""
    return WorkflowYaml.parse(PERF_WORKFLOW.read_text(encoding="utf-8"))
