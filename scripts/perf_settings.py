"""Perf settings."""

from __future__ import annotations
import importlib.util
from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parent.parent
SCHEMA_VERSION = 1
SIDES = ("base", "head")
DEFAULT_RUNS = 5
# Invalid runs retried per side and scenario before the scenario stops.
RETRY_LIMIT = 3
HARNESS_EXAMPLE = "perf_scenarios"
ALLOC_EXAMPLE = "perf_scenarios_alloc"
HARNESS_DIRECTORY = "crates/sonicterm-app/examples/perf_scenarios"
APP_MANIFEST = "crates/sonicterm-app/Cargo.toml"

# Harness exit codes.
HARNESS_VALID = 0
HARNESS_REFUSED = 2
HARNESS_INVALID = 3
HARNESS_TIMEOUT = 4
HARNESS_BLOCKED = 5
NOT_EXERCISED = "NOT_EXERCISED"

# This script's exit codes; BLOCKED matches the gate's convention for a host that cannot exercise a step.
EXIT_PASS = 0
EXIT_FAIL = 1
EXIT_BLOCKED = 3


def load_gate():
    """Return the canonical gate module and its single process-custody/catalog identity."""
    import local_gate
    return local_gate
