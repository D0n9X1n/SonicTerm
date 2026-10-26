#!/usr/bin/env python3
"""Compile harness-only cfgs without invalidating dependency fingerprints, then run its tests."""
from __future__ import annotations

import argparse
import importlib.util
import os
from pathlib import Path
import sys
import tempfile

SPEC = importlib.util.spec_from_file_location("target_gate", Path(__file__).with_name("local-gate.py"))
gate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gate
SPEC.loader.exec_module(gate)


def canonical_step(command, cfgs):
    """Accept only a maintained gate command and its exact cfg decision."""
    candidates = [*gate.STEPS, *gate.PREVIOUS_RELEASE_CHECKS.values(),
                  *(step for catalog in gate.PERF_CFG_BUILDS.values() for step in catalog.values())]
    for step in candidates:
        if step.argv == tuple(command) and step.harness_cfgs == tuple(cfgs):
            return step
    raise ValueError("not a canonical harness cfg command")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cfgs", required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    cfgs = () if args.cfgs == "none" else tuple(args.cfgs.split(","))
    try:
        original = canonical_step(command, cfgs)
        with tempfile.TemporaryDirectory(prefix="sonicterm-target-") as directory:
            result = gate.run_step(original, 1, Path.cwd(), Path(directory), os.environ)
            print(result.log_path.read_text(encoding="utf-8", errors="replace"), end="", flush=True)
            return 0 if result.accepted else 1
    except (ValueError, OSError) as error:
        print(f"harness-target: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
