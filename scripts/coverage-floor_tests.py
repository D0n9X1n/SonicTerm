#!/usr/bin/env python3
"""Contract tests for scripts/coverage-floor.py.

The floor's dangerous failure mode is a quiet pass: a crate that vanishes from
the report, a baseline measured on another host, or a crate with no eligible
line printed as 0% or 100%. Every rule is asserted against fixture JSON built
in code and written to temporary files, so the flat scripts/ directory needs no
fixture folder. The committed baseline and the gate script are checked against
the repository itself.

Run directly:  python3 scripts/coverage-floor_tests.py
Or discovered:  python3 -m unittest coverage-floor_tests   (from scripts/)
"""

from __future__ import annotations
from contextlib import contextmanager
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

# Keep the existing script import surface; implementation modules own their state.
from coverage_test_fixture import (
    _HERE,
    _REPO,
    _FLOOR_PATH,
    _spec,
    floor,
    TARGET,
    RUNNER,
    CI_ENV,
    LOCAL_ENV,
    ROOT,
    MEMBERS,
    FILES,
    FLOORS,
    export,
    metadata,
    baseline,
    Run,
    workspace,
    invoke,
    check,
    row,
    proposal,
    clean_env,
    RUSTC_VERBOSE,
    RUN_ENV,
    ARTIFACT,
    CHECKOUT,
    git,
    commit_files,
    pin_checkout,
    record,
    retained_run,
    update,
    rewrite,
    GATE_TIMEOUT_S,
    GROUP_EXIT_TIMEOUT_S,
    run_in_session,
    ci_job,
    ci_step,
    FAKE_CARGO,
    FAULT_SHIM,
    FAKE_RUSTC,
    GENERATOR_MARKER,
    MARKER_LINES,
    first_party_rust_sources,
    has_generator_marker,
)
from coverage_comparison_tests import (
    FloorComparisonTests,
    MissingBaselineTests,
    NotMeasuredTests,
    MissingCrateTests,
    HostIdentityTests,
    UpdateBaselineTests,
)
from coverage_records_tests import (
    ProvenanceRecordTests,
    ProvenanceWriterV2Tests,
)
from coverage_execution_tests import (
    GateScriptEvidenceTests,
    SessionRunnerTests,
)
from coverage_provenance_tests import (
    RebaselineProvenanceTests,
    DropGuidanceTests,
)
from coverage_policy_tests import (
    CoverageWorkflowTests,
    GitTreeIdTests,
    ClassificationTests,
    InputValidationTests,
)
from coverage_macos_tests import (
    MACOS_FILE,
    with_macos,
    macos_floor,
    MacosFloorTests,
    MACOS_RUN_ENV,
    MACOS_ARTIFACT,
    macos_retained_run,
    macos_update,
    MacosProvenanceTests,
)
from coverage_repository_tests import (
    RepositoryContractTests,
    CliTests,
)

if __name__ == "__main__":
    unittest.main(verbosity=2)
