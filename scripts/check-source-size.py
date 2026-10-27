#!/usr/bin/env python3
"""Run source-size contract tests, then enforce 1,000 physical lines per tracked first-party source."""

from pathlib import Path
import subprocess
import sys

import source_size


def main() -> int:
    """Reject a broken checker before scanning the working-tree bytes of tracked sources."""
    tests = Path(__file__).with_name("check-source-size_tests.py")
    completed = subprocess.run([sys.executable, "-B", str(tests)], check=False, timeout=120)
    if completed.returncode:
        return completed.returncode
    return source_size.main()


if __name__ == "__main__":
    raise SystemExit(main())
