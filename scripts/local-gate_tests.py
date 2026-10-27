#!/usr/bin/env python3
"""Runner, parity, and gate-script tests for scripts/local-gate.py.

The runner tests inject failing, hanging, non-launchable, and process-leaking
steps into the real runner and require every later step to run, the hang to die
with its process tree at its deadline, a leftover process to be killed and fail
its step, and a dirty tree to be reported and left unchanged. The parity tests
tie the step table to ci.yml, CLAUDE.md, and both Development-and-Release wiki
files; each also proves that a one-sided edit fails it, and the parser and
classifier tests mutate the complete workflow, so an unfamiliar form raises and
a parser that silently finds nothing cannot pass.

SONICTERM_GATE_ROOT selects the repository these tests read; it defaults to
this script's repository.
"""

from __future__ import annotations
import dataclasses
import errno
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import types
import unittest
from unittest import mock

# Keep the existing script import surface; implementation modules own their state.
from gate_test_fixture import (
    ROOT,
    gate,
    WORKFLOW,
    POSIX_ONLY,
    python_step,
    FIXTURE_GIT_TIMEOUT_S,
    _reap_killed,
    git,
    commit,
    fixture_repository,
    tree_bytes,
    run_quietly,
    marker_code,
    ERROR_SHARING_VIOLATION,
)
from gate_custody_tests import (
    WindowsCustodyTests,
    CustodyPolicyTests,
)
from gate_preparations_tests import (
    WindowsPreparationTests,
)
from gate_execution_tests import (
    RunnerTests,
    OutputLimitTests,
)
from gate_files_tests import (
    GitStateTests,
)
from gate_catalog_tests import (
    TableTests,
)
from gate_workflow_tests import (
    CiParityTests,
)
from gate_commands_tests import (
    _with_step,
    CiParserTests,
    CommandClassificationTests,
)
from gate_docs_tests import (
    DocParityTests,
    CommandLineTests,
)
from gate_script_tests import (
    _FAKE_CARGO,
    _FAKE_SUCCESS,
    _write_tools,
    _calls,
    _tool_environment,
    WorkspaceGateScriptTests,
    _FAKE_RUSTC,
    VERIFIED,
    EXCLUDED,
    _workspace_package_names,
    _fake_metadata,
    _script_list,
    WindowsTargetClassificationTests,
    WindowsTargetCheckTests,
)
from gate_harness_tests import (
    DECLARED_HARNESS_CFGS,
    workspace_check_cfgs,
    HarnessFlagTests,
)

if __name__ == "__main__":
    unittest.main(verbosity=2)
