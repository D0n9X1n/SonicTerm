#!/usr/bin/env python3
"""Require each packaged macOS executable to load the system `libhvf` weakly.

FreeType renders Apple's `hvgl` fonts (PingFang) through `/usr/lib/libhvf.dylib`, whose renderer
exists only on macOS 15.4 and later. The package supports macOS 15.0, so the executable must carry
exactly one `LC_LOAD_WEAK_DYLIB` for that path: a strong `LC_LOAD_DYLIB` would stop the app from
launching on 15.0 to 15.3, and no load at all means the build compiled FreeType without HVF. The
command reads `otool -l` for every executable given, prints each problem as `path: problem`, and
exits 1 when any remain.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys

HVF_LIBRARY = "/usr/lib/libhvf.dylib"
WEAK_LOAD = "LC_LOAD_WEAK_DYLIB"
OTOOL_TIMEOUT_S = 60
# One load command: its `cmd` line, then the `name` line of a dylib-loading command.
_DYLIB_LOAD = re.compile(r"^\s+cmd (LC_\w+)\n\s+cmdsize \d+\n\s+name (.+) \(offset \d+\)$", re.MULTILINE)


def hvf_load_problems(otool_output: str) -> list[str]:
    """Every way `otool_output` departs from exactly one weak load of the system `libhvf`."""
    commands = [command for command, name in _DYLIB_LOAD.findall(otool_output) if name == HVF_LIBRARY]
    if not commands:
        return [f"no load of {HVF_LIBRARY}; FreeType was built without HVF"]
    if commands != [WEAK_LOAD]:
        return [f"{HVF_LIBRARY} must be one {WEAK_LOAD}, found {', '.join(commands)}"]
    return []


def otool_load_commands(path: str) -> str:
    """The `otool -l` listing of `path`; raises when otool fails or exceeds its deadline."""
    return subprocess.run(["otool", "-l", path], capture_output=True, text=True, check=True,
                          timeout=OTOOL_TIMEOUT_S).stdout


def main(argv: list[str] | None = None) -> int:
    """Check every executable named on the command line; 0 when each loads `libhvf` weakly."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("executables", nargs="+")
    args = parser.parse_args(argv)
    failed = False
    for path in args.executables:
        try:
            problems = hvf_load_problems(otool_load_commands(path))
        except (OSError, subprocess.SubprocessError) as error:
            problems = [f"otool -l failed: {error}"]
        for problem in problems:
            print(f"{path}: {problem}", file=sys.stderr)
        failed = failed or bool(problems)
        if not problems:
            print(f"{path}: {HVF_LIBRARY} is {WEAK_LOAD}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
