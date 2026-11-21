"""Perf workflow fixture."""

from __future__ import annotations
import json
import re
from pathlib import Path



# --- perf.yml as a job graph: a YAML reader, an `if:` evaluator and a push simulator ---------------

PERF_WORKFLOW = Path(__file__).resolve().parent.parent / ".github" / "workflows" / "perf.yml"
# The harness's scenario catalogue, the source `--list` prints; every variant a profile names must be in it.
SCENARIO_CATALOGUE = (Path(__file__).resolve().parent.parent / "crates" / "sonicterm-app" / "examples"
                      / "perf_scenarios" / "scenarios.rs")
# The plan's and the result's eligibility: a tag push, or a `perf` or `perf-full` pull request whose event is not
# another label.
ELIGIBILITY = ("github.event_name == 'push' || ((contains(github.event.pull_request.labels.*.name, 'perf') || "
               "contains(github.event.pull_request.labels.*.name, 'perf-full')) && "
               "(github.event.action != 'labeled' || github.event.label.name == 'perf' || "
               "github.event.label.name == 'perf-full'))")
PERF_JOBS = ("plan", "compare-macos", "compare-windows", "perf-result")
COMPARISON_JOBS = ("compare-macos", "compare-windows")
# Each comparison job's platform, its fixed runner and the plan output its matrix reads.
COMPARISON_PLATFORMS = {"compare-macos": "macOS", "compare-windows": "Windows"}
COMPARISON_RUNNERS = {"compare-macos": "macos-15", "compare-windows": "windows-latest"}
COMPARISON_OUTPUTS = {"compare-macos": "macos", "compare-windows": "windows"}
PLAN_NAME = "Plan the comparison"
REF_STEP = "Choose the refs and the run length"
PARTITION_STEP = "Choose the profile and its partitions"
PROFILE_STEP = "Relax the release profile for a pull request"
COMPARE_STEP = "Compare the base and the head"
SUMMARY_STEP = "Publish the table in the job summary"
EVIDENCE_STEP = "Upload the comparison evidence"
RESULT_STEP = "Require every comparison job to succeed"
RESULT_NAME = "Performance comparison result"
RESULT_NOT_RUN = "Performance comparison result (not run)"
JOB_RESULTS = ("success", "failure", "cancelled", "skipped", "")
# Variants only one platform measures. The Windows runner presents in software, where the cursor never glides,
# and kitty graphics do not pass through ConPTY there; S11/gdi and S11/wgpu are Windows' presenter controls. The
# other macOS-only variants are release-only workloads the Windows partitions never ran.
PLATFORM_ONLY = {"S2/glide": "macOS", "S11/kitty-placeholders": "macOS", "S2/spawn": "macOS",
                 "S3/history": "macOS", "S3/resize": "macOS", "S11/burst": "macOS", "S11/burst-text": "macOS",
                 "S11/gdi": "Windows", "S11/wgpu": "Windows"}
# Variants only a release tag measures: full-length workloads a pull request's budget cannot hold.
RELEASE_ONLY = ("S2/spawn", "S3/history", "S3/resize", "S11/burst", "S11/burst-text", "S13/wide-8", "S13/wide-16",
                "S13/wide-32")
# Catalogue variants no profile compares: the Windows smoke's presenter and role-exit cases, and S5's presenter runs.
NOT_COMPARED = ("S1/gdi", "S1/wgpu", "S1/role-exit", "S5/gdi", "S5/wgpu")
# The pull-request core set, as reviewed: the variants the PR default must keep measuring.
CORE_VARIANTS = ("S1/default", "S2/default", "S2/flood", "S3/default", "S6/rest", "S9/default", "S10/sync",
                 "S11/default", "S12/default", "S13/default", "S13/sustained-4")
# The variants a release runs the counters set for, where a platform measures them: the counters-derived figures a
# release reads (atlas recovery, echo and flood counters, S10's attribution, S12's covered window, S13's verdict and
# scaling rule). Every other release variant runs its timed set only, and S9 its laps set too.
RELEASE_COUNTERS = ("S1/default", "S1/atlas-retry", "S2/default", "S2/flood", "S3/default", "S6/rest", "S10/default",
                    "S10/sync", "S12/default", "S13/default", "S13/sustained-4", "S13/wide-8", "S13/wide-16",
                    "S13/wide-32")
# Variants one report reads together, so each group shares a partition: S13's contention verdict reads both two-
# and four-pane runs, its scaling rule all three wide layouts, and S11/burst is read against S11/burst-text.
SHARED_PARTITION = (("S13/default", "S13/sustained-4"), ("S13/wide-8", "S13/wide-16", "S13/wide-32"),
                    ("S11/burst", "S11/burst-text"))

FAKE_GIT = """#!/usr/bin/env bash
# Answers the ref step's Git calls for one mode: previous, first, root, broken or unreadable for a tag push; a pull
# request's fetch and merge-base in every mode.
mode=$FAKE_GIT_MODE
case "$1" in
  fetch) exit 0 ;;
  merge-base) echo merge-base-commit ;;
  rev-list)
    # `rev-list --parents -n 1 <sha>` prints the commit, then its parents.
    case "$mode" in
      root) echo tag-commit ;;
      unreadable) echo "fatal: unable to read commit object" >&2; exit 128 ;;
      *) echo "tag-commit parent-commit" ;;
    esac ;;
  rev-parse)
    if [[ "$*" == *"{commit}"* ]]; then echo base-commit; exit 0; fi
    if [ "$mode" = root ]; then exit 1; fi
    if [ "$mode" = unreadable ]; then echo "fatal: unable to read commit object" >&2; exit 128; fi
    echo parent-commit ;;
  tag)
    case "$mode" in
      previous) echo v1.3.8 ;;
      broken) echo "fatal: unable to read tree: object missing" >&2; exit 128 ;;
    esac ;;
  describe)
    if [ "$mode" = previous ]; then echo v1.3.8; else echo "fatal: No names found" >&2; exit 128; fi ;;
  *) echo "unexpected git $*" >&2; exit 2 ;;
esac
"""
PR_HEAD_SHA = "c" * 40


def pull_request_github(action, labels, label=None, run_id="70"):
    """A pull_request event's context: its action, the labels the PR carries and the label just added."""
    event = {"action": action, "pull_request": {"number": 1575, "labels": [{"name": name} for name in labels],
                                                "head": {"sha": PR_HEAD_SHA}, "base": {"ref": "main"}}}
    if label is not None:
        event["label"] = {"name": label}
    return {"github": {"event_name": "pull_request", "event": event, "run_id": run_id, "ref_name": "1575/merge",
                       "run_attempt": "1", "sha": "merge-commit", "workspace": "/w"}}


def push_github(run_id="71"):
    """A tag push's context."""
    return {"github": {"event_name": "push", "event": {}, "run_id": run_id, "ref_name": "v1.4.0",
                       "run_attempt": "1", "sha": "tag-commit", "workspace": "/w"}}


_CATALOGUE_ENTRY = re.compile(r'spec\(\s*"(?P<id>S[0-9]+)",\s*"[^"]*",\s*&\[(?P<variants>[^\]]*)\]', re.S)


def harness_catalogue():
    """Every `ID/variant` label the harness catalogue declares, read from the source `--list` prints."""
    source = SCENARIO_CATALOGUE.read_text(encoding="utf-8")
    labels = set()
    scenarios = set()
    for entry in _CATALOGUE_ENTRY.finditer(source):
        scenarios.add(entry["id"])
        labels.update(f"{entry['id']}/{variant}" for variant in re.findall(r'"([^"]+)"', entry["variants"]))
    # A format change that read nothing must fail here, not pass every membership test against an empty set.
    if len(scenarios) != 13:
        raise AssertionError(f"read {len(scenarios)} scenarios from {SCENARIO_CATALOGUE}, expected S1 to S13")
    return labels


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
