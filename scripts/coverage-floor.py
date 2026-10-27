#!/usr/bin/env python3
"""Per-crate line-coverage report and regression floor for the coverage gate.

Reads the `cargo llvm-cov report --json --summary-only` export and the
`cargo metadata --no-deps` inventory that scripts/rust-logic-coverage.sh writes
from one instrumented run, then:

* aggregates line coverage per workspace member from repository-relative
  `crates/<member>/...` paths;
* excludes test, vendored, generated (Cargo build output and committed
  rust-bindgen bindings), build-script, and out-of-tree files and prints every
  exclusion category with its file and line counts;
* reports a member with no eligible instrumented line as `not measured` with
  its reason, never as 0% or 100%;
* compares the measured members with scripts/coverage-baseline.json.

The baseline records the host that produced it (target triple and runner
image), a tolerance in percentage points, per-crate floors, and the crates
declared not measured. The floor is enforced only in CI (`GITHUB_ACTIONS=true`)
on that host. There it fails when a measured crate drops more than the
tolerance below its floor or has no floor, when a crate with a floor stops being
measured or leaves the workspace, when a member is neither measured nor
declared, when a declared crate starts reporting lines, and when a declaration
names a crate that is not a member. CI on any other host fails as not
comparable: a baseline from another host is never a passing result. Outside CI
every delta is informational and the floor gives no verdict.

The baseline changes only through `--update-baseline --reason TEXT`, whose
result is committed as a reviewed diff. A baseline that names a CI host changes
only with `--provenance FILE`, the record scripts/rust-logic-coverage.sh writes
beside the report through the `record-provenance` subcommand. The update refuses
an incomplete record, a report or inventory whose digest differs from it, object
IDs that are not full IDs in the checkout's format, a path map that does not hash
to the recorded tree, incomplete checks, a conflicting host, and a measured tree
whose coverage-relevant content differs from the checkout. These offline checks
prove only that the record is self-consistent; the documented retrieval through
the GitHub API binds it to its run, commit, and tree. A failing CI run can print a proposed baseline between marker
lines as a preview; a DROP prints none, and the check never writes the baseline.

The optional `macos` section holds per-file floors for macOS-only logic in the
80% subset, which the Linux floors cannot see because it compiles only on macOS.
The `macos-floor` subcommand checks a report from scripts/macos-logic-coverage.sh
against it with the same tolerance, enforced only in CI on the section's own
host. `macos-floor --update --reason TEXT --provenance FILE` rewrites that
section alone, leaving every crate floor verbatim, and only from the verified
record `record-macos-provenance` writes in the macos-core job: like a crate
floor, a macOS floor never changes from a stated host or a local measurement.
Both kinds of record must name the cargo-llvm-cov release scripts/pinned-tools.json pins.

Exit status: 0 when the floor holds or the run is informational, 1 when the
floor fails in CI, 2 for usage or input errors.
"""

from __future__ import annotations
import argparse
from collections import Counter
from dataclasses import dataclass, field
from fractions import Fraction
import hashlib
import json
import math
import os
from pathlib import Path
import posixpath
import subprocess
import sys
import tempfile
from typing import Dict, List, Mapping, Optional, Tuple

# Keep the existing script import surface; implementation modules own their state.
from coverage_model import (
    SCHEMA,
    DEFAULT_TOLERANCE_PP,
    LLVM_EXPORT_TYPE,
    PROPOSAL_BEGIN,
    PROPOSAL_END,
    PROVENANCE_SCHEMA,
    EVIDENCE_DIRECTORY,
    MEASUREMENT_DIRECTORY,
    WORK_DIRECTORY,
    STAGING_DIRECTORY,
    PIN_FILE,
    PIN_SCHEMA,
    PIN_KEYS,
    DRIFT_EXIT_STATUS,
    REPORT_FILE,
    INVENTORY_FILE,
    PROVENANCE_FILE,
    EVIDENCE_ARTIFACT_PREFIX,
    COVERAGE_WORKFLOW,
    COVERAGE_JOB,
    MACOS_JOB,
    MACOS_ARTIFACT_PREFIX,
    PINNED_TOOLS_PATH,
    BASELINE_PATH,
    REBASELINE_PROCEDURE,
    LOCAL_RUNNER,
    MEASUREMENT_PHASES,
    CHECK_PHASES,
    RUN_STATES,
    GIT_TIMEOUT_S,
    OBJECT_ID_LENGTHS,
    GIT_FILE_MODES,
    TREE_DIFF_LIMIT,
    REQUIRED_RECORD_FIELDS,
    VENDORED_ROOTS,
    TEST_DIRECTORIES,
    GENERATED_SOURCES,
    PLATFORM_CRATES,
    OS_LABELS,
    EXCLUSION_REASONS,
    LISTED_CATEGORIES,
    LISTED_PATH_LIMIT,
    DROP,
    NO_BASELINE,
    NOW_MEASURED,
    MISSING_MEASURED,
    MISSING_MEMBER,
    UNEXPLAINED,
    STALE_DECLARATION,
    PROPOSABLE,
    PERCENT_SLOP,
    InputError,
    ReportFile,
    Report,
    Member,
    Inventory,
    CrateCoverage,
    ExclusionTotals,
    Entry,
    MacosSection,
    Baseline,
    Finding,
    Placement,
)
from coverage_reports import (
    _normalize,
    _absolute,
    _within,
    _relative,
    _root_forms,
    _count,
    _finite_number,
    _reject_constant,
    _finite_float,
    _read_json,
    load_report,
    load_inventory,
    place,
    aggregate,
    unmeasured_detail,
)
from coverage_baselines import (
    _BASELINE_KEYS,
    parse_baseline,
    _parse_entry,
    _parse_macos,
    load_baseline,
)
from coverage_comparison import (
    _pct,
    _delta,
    runner_identity,
    host_os,
    platform_note,
    compare,
    render_table,
    render_exclusions,
    _entry_document,
    baseline_document,
    render_document,
    additions_proposal,
    rebaseline_proposal,
    provenance,
    evidence_artifact,
    run_url,
    drop_guidance,
    render_proposal,
    run_check,
    full_update,
    partial_update,
    describe_update,
)
from coverage_checkout import (
    _write_atomic,
    coverage_relevant,
    _git,
    _git_text,
    tree_entries,
    worktree_changes,
    file_sha256,
    workflow_path,
    checkout_state,
    load_pin,
    checkout_drift,
    _hex_id,
    git_tree_id,
    object_format,
    identity_problems,
)
from coverage_records import (
    _exit_status,
    checks_problems,
    build_record,
    _run_fields,
    build_macos_record,
    build_record_parser,
    run_record,
    run_record_macos,
    run_pin,
    Provenance,
)
from coverage_provenance import (
    _text,
    _listed,
    checkout_root,
    measured_tree_problems,
    EvidenceKind,
    CRATE_EVIDENCE,
    MACOS_EVIDENCE,
    pinned_tool_problems,
    verify_provenance,
)
from coverage_updates import (
    run_update,
)
from coverage_macos import (
    _macos_measured,
    run_macos_floor,
    _update_macos,
)
from coverage_cli import (
    build_parser,
    main,
)

if __name__ == "__main__":
    sys.exit(main())
