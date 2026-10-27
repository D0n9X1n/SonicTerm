#!/usr/bin/env python3
"""Portable contracts for native package validation and controlled size evidence."""

import contextlib
import dataclasses
import importlib.util
import io
import json
import shutil
import os
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile
import sys
import types
import unittest
from unittest.mock import patch

# Keep the existing script import surface; implementation modules own their state.
from macos_package_test_fixture import (
    SPEC,
    tool,
    FlushedStream,
    FakeAttachmentGate,
    measurement_app,
    scripted_hdiutil,
)
from macos_package_validation_tests import (
    PackageTests,
)
from macos_package_attachment_tests import (
    AttachmentTests,
)

if __name__ == "__main__":
    unittest.main(verbosity=2)
