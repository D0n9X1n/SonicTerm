"""Coverage cli."""

from __future__ import annotations
import argparse
import os
import sys
from typing import Dict, List, Mapping, Optional, Tuple

import coverage_comparison
import coverage_macos
import coverage_model
import coverage_records
import coverage_updates


def build_parser() -> argparse.ArgumentParser:
    """Return the command-line parser."""
    parser = argparse.ArgumentParser(description="Per-crate line-coverage report and regression floor.")
    parser.add_argument("--report", required=True, help="cargo llvm-cov report --json --summary-only output")
    parser.add_argument("--metadata", required=True, help="cargo metadata --no-deps --format-version 1 output")
    parser.add_argument("--baseline", required=True, help="baseline JSON, normally scripts/coverage-baseline.json")
    parser.add_argument("--target", required=True, help="host target triple of the instrumented build (rustc -vV)")
    parser.add_argument("--toolchain", help="rustc version line, printed as provenance")
    parser.add_argument("--source-root", help="directory the report's absolute filenames sit under "
                                              "(default: the metadata workspace_root)")
    parser.add_argument("--update-baseline", action="store_true",
                        help="write the baseline from this report instead of checking it")
    parser.add_argument("--reason", help="why the baseline changes; required with --update-baseline")
    parser.add_argument("--runner", help="with --update-baseline and no --provenance, the runner identity "
                                         f"{coverage_model.LOCAL_RUNNER!r} of an unenforced baseline; with --provenance, a "
                                         "check against the record")
    parser.add_argument("--provenance", metavar="FILE",
                        help="with --update-baseline, the provenance record of the CI run that measured the "
                             "report; required whenever the baseline names a CI host")
    parser.add_argument("--crate", action="append", metavar="NAME",
                        help="with --update-baseline, replace only this crate's floor (repeatable)")
    return parser


def main(argv: Optional[List[str]] = None, env: Optional[Mapping[str, str]] = None, stdout=None, stderr=None) -> int:
    """Run the check or the baseline update and return the exit status."""
    env = os.environ if env is None else env
    stdout = sys.stdout if stdout is None else stdout
    stderr = sys.stderr if stderr is None else stderr
    argv = list(sys.argv[1:] if argv is None else argv)
    subcommands = {"record-provenance": lambda rest: coverage_records.run_record(rest, env), "pin-checkout": coverage_records.run_pin,
                   "macos-floor": lambda rest: coverage_macos.run_macos_floor(rest, env, stdout),
                   "record-macos-provenance": lambda rest: coverage_records.run_record_macos(rest, env)}
    if argv[:1] and argv[0] in subcommands:
        try:
            return subcommands[argv[0]](argv[1:])
        except coverage_model.InputError as error:
            print(f"coverage-floor: {error}", file=stderr)
            return 2
    args = build_parser().parse_args(argv)
    try:
        if args.update_baseline:
            return coverage_updates.run_update(args, stdout)
        misplaced = [flag for flag, value in (("--reason", args.reason), ("--runner", args.runner),
                                              ("--crate", args.crate), ("--provenance", args.provenance))
                     if value is not None]
        # Check mode takes the runner from the CI environment, so a stated one cannot fake a match.
        if misplaced:
            raise coverage_model.InputError(", ".join(misplaced) + " is only valid with --update-baseline")
        return coverage_comparison.run_check(args, env, stdout)
    except coverage_model.InputError as error:
        print(f"coverage-floor: {error}", file=stderr)
        return 2
