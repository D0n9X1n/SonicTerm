"""Perf main."""

from __future__ import annotations
import os
import sys
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_arguments
import perf_cli
import perf_echo_loader
import perf_smoke


def use_utf8_output() -> None:
    """Write stdout and stderr as UTF-8, whatever the console's legacy code page.

    The tables use `≤` and `–`; a Windows runner's cp1252 console cannot encode them, and a failed print
    after comparison.md is written would turn a finished comparison into exit 1.
    """
    for stream in (sys.stdout, sys.stderr):
        # When: a test redirects a stream to StringIO, which has no encoding to change.
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")


def main(argv: Sequence[str] | None = None) -> int:
    """Run the smoke or a comparison and return its exit code."""
    use_utf8_output()
    args = perf_arguments.parse_args(argv)
    if args.echo_attribution is not None:
        return perf_echo_loader.echo_attribution_main(args)
    if args.check_previous_release:
        return perf_cli.previous_release_main(args)
    if args.smoke:
        return perf_smoke.smoke_main(os.environ)
    return perf_cli.compare_main(args)
