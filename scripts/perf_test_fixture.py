"""Perf test fixture."""

from __future__ import annotations
import importlib.util
from pathlib import Path
import sys


SPEC = importlib.util.spec_from_file_location(
    "perf_compare", Path(__file__).with_name("perf-compare.py")
)
assert SPEC and SPEC.loader
import perf_compare as perf
