#!/usr/bin/env python3
"""Contract tests for scripts/check-authored-rust-comments.py."""

from __future__ import annotations
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

# Keep the existing script import surface; implementation modules own their state.
from rust_comment_test_fixture import (
    _HERE,
    _CHECKER_PATH,
    _spec,
    checker,
    rust,
    repository,
    analyze,
    formatted,
    rule_names,
    cli,
)
from rust_comment_lexer_tests import (
    LexerTests,
    DocumentationTests,
)
from rust_comment_modules_tests import (
    VisibilityAndResolutionTests,
    TestAndBuildContextTests,
)
from rust_comment_branches_tests import (
    BranchRuleTests,
)
from rust_comment_markers_tests import (
    MarkerContractTests,
)
from rust_comment_cli_tests import (
    PathCliAndInventoryTests,
)

if __name__ == "__main__":
    unittest.main(verbosity=2)
