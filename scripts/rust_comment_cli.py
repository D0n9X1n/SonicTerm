"""Rust comment cli."""

from __future__ import annotations
import argparse
import json
from pathlib import Path
import subprocess
import sys
from typing import Iterable, Iterator, Sequence

import rust_comment_analysis
import rust_comment_model


def _repository_root(cwd: Path) -> Path:
    completed = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise rust_comment_model.AnalysisError(completed.stderr.decode("utf-8", errors="replace").strip())
    return Path(completed.stdout.decode("utf-8").strip()).resolve()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--check", action="store_true")
    modes.add_argument("--inventory", action="store_true")
    modes.add_argument("--semantic-candidates", action="store_true")
    parser.add_argument("--paths", nargs="+")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    """Run the selected checker mode from any directory in the repository."""
    args = _parser().parse_args(argv)
    try:
        root = _repository_root(Path.cwd())
        report = rust_comment_analysis.analyze_repository(root, paths=args.paths, caller_cwd=Path.cwd())
    except (rust_comment_model.AnalysisError, OSError) as error:
        print(f"check-authored-rust-comments: {error}", file=sys.stderr)
        return 2
    if args.inventory:
        print(json.dumps(report.inventory(), sort_keys=True, indent=2))
        return 0
    if args.semantic_candidates:
        for diagnostic in report.semantic_candidates:
            print(diagnostic.format())
        return 0
    for diagnostic in report.diagnostics:
        print(diagnostic.format())
    return 1 if report.diagnostics else 0
