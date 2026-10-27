#!/usr/bin/env python3
"""Contract tests for scripts/check-workflow-supply-chain.py.

The checker's failure mode that matters is the quiet one: a parser change that
stops *finding* `uses:` lines reports a clean scan, and a clean scan is
indistinguishable from a compliant repository. So every rule here is asserted
in both directions — a compliant fixture passes, and a fixture violating
exactly that rule fails — and one test runs the checker against the real
workflows so the gate cannot pass by scanning nothing.
"""

from __future__ import annotations
from contextlib import contextmanager
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

# Keep the existing script import surface; implementation modules own their state.
from workflow_test_fixture import (
    _HERE,
    load_local_gate,
    _CHECKER_PATH,
    _spec,
    checker,
    PINNED_CHECKOUT,
    workflow,
    repository,
    compliant,
    messages,
    findings_for,
    WINDOWS_TEST_SHARDS,
    WINDOWS_TEST_WORK,
    WINDOWS_SHARD_SETUP,
    WINDOWS_SHARD_WORK,
    windows_work_step,
    ORACLE_CONSUMER_SETUP,
    ORACLE_PRODUCER_BEFORE,
    ORACLE_PRODUCER_AFTER,
    windows_shard_problems,
    job_block,
    shell_commands,
    optional_feature_packages,
)
from workflow_permissions_tests import (
    CompliantFixtureTests,
    MutableRefTests,
    VersionCommentTests,
    PermissionTests,
)
from workflow_download_tests import (
    RELEASE_WORKFLOW,
    CI_WORKFLOW,
    DENY_URL,
    DIGEST,
    PINNED_DOWNLOAD_ARTIFACT,
    run_workflow,
    checked,
    manifest,
    artifact_workflow,
    INSTALL,
    DownloadTests,
    GuardrailTests,
    PinnedToolTests,
    ConsistencyTests,
    ScanCoverageTests,
)
from workflow_repository_tests import (
    RepositoryTests,
)
from workflow_packages_tests import (
    MacPackageGateTests,
    CommandLineTests,
)

if __name__ == "__main__":
    unittest.main(verbosity=2)
