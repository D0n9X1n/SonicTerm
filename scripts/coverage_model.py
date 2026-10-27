"""Coverage model."""

from __future__ import annotations
from collections import Counter
from dataclasses import dataclass, field
from fractions import Fraction
from typing import Dict, List, Mapping, Optional, Tuple


SCHEMA = "sonicterm-coverage-baseline/1"
DEFAULT_TOLERANCE_PP = 1.0
LLVM_EXPORT_TYPE = "llvm.coverage.json.export"
PROPOSAL_BEGIN = "----- BEGIN PROPOSED scripts/coverage-baseline.json -----"
PROPOSAL_END = "----- END PROPOSED scripts/coverage-baseline.json -----"

# The evidence the linux-coverage job uploads, and the record tying it to its run.
PROVENANCE_SCHEMA = "sonicterm-coverage-provenance/1"
EVIDENCE_DIRECTORY = "target/rust-logic-coverage-evidence"
# The report and inventory are published together as this subdirectory of the evidence.
MEASUREMENT_DIRECTORY = "measurement"
# Staging and the checkout pin; never uploaded.
WORK_DIRECTORY = "target/rust-logic-coverage-work"
STAGING_DIRECTORY = "measurement-staging"
PIN_FILE = "checkout-pin.json"
PIN_SCHEMA = "sonicterm-coverage-pin/1"
PIN_KEYS = ("commit", "tree", "tree_entries", "worktree_changes")
# The exit status of a publish record that finds the checkout moved since its pin.
DRIFT_EXIT_STATUS = 3
REPORT_FILE = "coverage-summary.json"
INVENTORY_FILE = "workspace-metadata.json"
PROVENANCE_FILE = "coverage-provenance.json"
EVIDENCE_ARTIFACT_PREFIX = "rust-logic-coverage-evidence"
COVERAGE_WORKFLOW = ".github/workflows/ci.yml"
COVERAGE_JOB = "linux-coverage"
# The job, artifact and record of the macOS-only floor, which scripts/macos-logic-coverage.sh writes in macos-core.
MACOS_JOB = "macos-core"
MACOS_ARTIFACT_PREFIX = "macos-logic-coverage-evidence"
# The manifest whose cargo-llvm-cov release every record must name.
PINNED_TOOLS_PATH = "scripts/pinned-tools.json"
BASELINE_PATH = "scripts/coverage-baseline.json"
REBASELINE_PROCEDURE = 'the "Coverage evidence and rebaselining" section of Development-and-Release'
LOCAL_RUNNER = "local"
# Phases of one coverage run, in order. The measurement is complete once
# `publish` has moved the report and inventory into place; the two checks then
# judge the complete measurement.
MEASUREMENT_PHASES = ("self-test", "toolchain", "instrumented-tests", "report", "inventory", "publish")
CHECK_PHASES = ("subset-gate", "floor")
RUN_STATES = ("running", "failed", "done")
GIT_TIMEOUT_S = 60
# Object ID lengths by `git rev-parse --show-object-format`, and the file modes Git writes in a tree.
OBJECT_ID_LENGTHS = {"sha1": 40, "sha256": 64}
GIT_FILE_MODES = frozenset({"100644", "100755", "120000", "160000"})
TREE_DIFF_LIMIT = 20
# Fields a record must carry as non-empty strings before it can change a floor.
REQUIRED_RECORD_FIELDS = (
    "measurement", "target", "runner", "image_version", "rustc_version", "rustc_verbose",
    "cargo_llvm_cov", "repository", "workflow", "run_id", "run_attempt", "job", "event",
    "commit", "tree", "artifact", "report_sha256", "inventory_sha256",
)

# Upstream source trees kept as reviewed imports. The tuple mirrors the `path`
# of every library in scripts/native-dependencies.json; a contract test keeps
# the two identical.
VENDORED_ROOTS = (
    "crates/sonicterm-freetype/freetype2",
    "crates/sonicterm-freetype/libpng",
    "crates/sonicterm-freetype/zlib",
    "crates/sonicterm-harfbuzz/harfbuzz",
    "crates/sonicterm-winit",
)

# Cargo's conventional integration-test, benchmark, and example directories.
TEST_DIRECTORIES = frozenset({"tests", "benches", "examples"})

# Committed generator output inside first-party crates, with its generator.
# These files count under `generated`, so regenerating bindings never moves a
# crate's figure. A contract test keeps this set equal to the first-party
# sources that carry a generator marker.
GENERATED_SOURCES = {
    "crates/sonicterm-freetype/src/lib.rs": "rust-bindgen 0.71.1 via scripts/regenerate-freetype.sh",
    "crates/sonicterm-freetype/src/types.rs": "rust-bindgen 0.71.1 via scripts/regenerate-freetype.sh",
    "crates/sonicterm-harfbuzz/src/lib.rs": "rust-bindgen 0.71.1 via scripts/regenerate-harfbuzz.sh",
}

# Members that ship on one platform. On another host their rows measure only
# what compiles there, which is not execution coverage on the target platform.
PLATFORM_CRATES = {
    "sonicterm-mac": "macos",
    "sonicterm-windows": "windows",
    "sonicterm-linux": "linux",
}
OS_LABELS = {"macos": "macOS", "windows": "Windows", "linux": "Linux"}

# Exclusion categories in report order, each printed with its reason.
EXCLUSION_REASONS = {
    "test": "test code: *_tests.rs files and tests/, benches/, examples/ trees",
    "vendored": "vendored upstream source listed in scripts/native-dependencies.json",
    "generated": "generated code: Cargo target output and committed rust-bindgen bindings",
    "outside-repository": "outside the repository source root",
    "build-script": "Cargo build script; runs at compile time, not under test",
    "outside-members": "inside the repository but in no workspace member",
}
# Categories unusual enough that their paths are listed, not only counted.
LISTED_CATEGORIES = frozenset({"generated", "outside-repository", "build-script", "outside-members"})
LISTED_PATH_LIMIT = 10

DROP = "drop"
NO_BASELINE = "no-baseline"
NOW_MEASURED = "declared-now-measured"
MISSING_MEASURED = "missing-measured"
MISSING_MEMBER = "missing-member"
UNEXPLAINED = "unexplained"
STALE_DECLARATION = "stale-declaration"

# Findings a proposal resolves by adding entries. Drops and disappearances need
# a reviewer's decision, so no proposal lowers or deletes an existing floor.
PROPOSABLE = frozenset({NO_BASELINE, NOW_MEASURED, UNEXPLAINED})

# A stored percent is rounded to two decimals; allow that rounding plus float noise.
PERCENT_SLOP = 0.0051


class InputError(Exception):
    """A missing or malformed input, reported with exit status 2."""


@dataclass(frozen=True)
class ReportFile:
    """Line counts for one file of the llvm-cov export."""

    filename: str
    lines: int
    covered: int


@dataclass(frozen=True)
class Report:
    """The llvm-cov export: per-file line counts and the tool that produced them."""

    files: Tuple[ReportFile, ...]
    tool: str


@dataclass(frozen=True)
class Member:
    """A workspace member: package name, repository-relative directory, build scripts."""

    name: str
    directory: str
    build_scripts: frozenset


@dataclass
class Inventory:
    """Workspace members from `cargo metadata --no-deps`."""

    root: str
    target_directory: Optional[str]
    members: Dict[str, Member]

    def owner(self, relative: str) -> Optional[Member]:
        """Return the member whose directory most specifically contains `relative`."""
        # Resolve this runtime-only dependency after module initialization.
        import coverage_reports
        best = None
        for member in self.members.values():
            if coverage_reports._within(relative, member.directory) and (
                best is None or len(member.directory) > len(best.directory)
            ):
                best = member
        return best


@dataclass
class CrateCoverage:
    """Eligible line totals for one member and the count of its excluded files."""

    lines: int = 0
    covered: int = 0
    eligible_files: int = 0
    excluded: Counter = field(default_factory=Counter)

    @property
    def measured(self) -> bool:
        """Whether the member has at least one eligible instrumented line."""
        return self.lines > 0

    @property
    def percent(self) -> Fraction:
        """Exact line coverage in percent; only valid for a measured member."""
        return Fraction(self.covered * 100, self.lines)


@dataclass
class ExclusionTotals:
    """Files and lines excluded under one category."""

    files: int = 0
    lines: int = 0
    paths: List[str] = field(default_factory=list)


@dataclass(frozen=True)
class Entry:
    """One crate's floor: the line counts it was recorded from."""

    lines: int
    covered: int

    @property
    def percent(self) -> Fraction:
        """Exact recorded line coverage in percent."""
        return Fraction(self.covered * 100, self.lines)


@dataclass
class MacosSection:
    """Per-file floors for macOS-only logic, measured on their own macOS host."""

    target: str
    runner: str
    reason: str
    files: Dict[str, Entry]


@dataclass
class Baseline:
    """The reviewed floor and the host identity it is valid for."""

    target: str
    runner: str
    tolerance_pp: float
    reason: str
    crates: Dict[str, Entry]
    not_measured: Dict[str, str]
    macos: Optional[MacosSection] = None


@dataclass(frozen=True)
class Finding:
    """One reason the floor fails in CI."""

    kind: str
    crate: str
    message: str


@dataclass(frozen=True)
class Placement:
    """Where one report file lands: its owning member and exclusion category."""

    path: str
    member: Optional[str]
    category: Optional[str]
