"""Compare SonicTerm performance between two Git refs with the perf_scenarios harness.

`--base <ref> --head <ref>` builds each ref in its own worktree and target
directory with the head's harness overlaid, runs every selected scenario in
alternating A B B A order until each side has `--runs` valid runs, and prints
the comparison table, the host block, both SHAs and the harness hash.
`--smoke` is the `macos-perf-smoke` gate step: a debug build of the current
tree, short S1 and S3 runs and one S1 run ended the way a step deadline ends
it, checking the result schema, focus safety and cleanup, with no timing
assertion.

Every child process for a build, a harness run or a `footprint` sample goes
through local-gate.py's `run_step`, which bounds it and reaps its process
tree. PTY sessions leave that tree through `setsid`, so each run's sessions
are cleaned through the anchor records the scenario scripts write: the anchor
is revalidated, every live member of the recorded session is enumerated and
rechecked immediately before it is signalled, and the anchor is signalled
last. No pid that failed validation is ever signalled.

On Windows the gate's job object owns every process a run starts, so the
job's custody record, not anchor records, proves a run's teardown. Each role
program's record is acknowledged once its process is the harness's child,
runs the harness image and was created after the launch, and the deadline
case ends the harness with TerminateProcess after rechecking its image and
creation time.

The module imports on every host. POSIX-only calls (`os.getsid`,
`os.killpg`, `signal.SIGKILL`), the macOS libproc binding and the Windows
kernel32 binding are resolved inside the code paths that need them, so the
tests drive fakes everywhere.
"""

from __future__ import annotations
import argparse
from pathlib import Path
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_build_policy
import perf_cli
import perf_echo_analysis
import perf_settings


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    """Parse the command line: `--smoke` alone, or a comparison of `--base` and `--head`."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--smoke", action="store_true",
                        help="run the macos-perf-smoke gate step: a debug build of this tree, short S1 and S3 "
                             "runs and S1 ended as a step deadline ends it")
    parser.add_argument("--check-previous-release", action="store_true",
                        help="check that the head's harness, overlaid on the previous release tag, still builds "
                             "(the perf-previous-release gate step); takes only --out")
    parser.add_argument("--base", help="the baseline ref, usually the merge-base")
    parser.add_argument("--head", help="the ref under test")
    parser.add_argument("--scenario", action="extend", nargs="+", metavar="ID[/VARIANT]",
                        help="`all` (every scenario's default variant), an ID or an ID/variant; several values "
                             "or repeated flags run in the order given, without repeats (default: all)")
    parser.add_argument("--runs", type=perf_cli.positive_int,
                        help=f"valid runs per side and scenario (default: {perf_settings.DEFAULT_RUNS})")
    parser.add_argument("--short", action="store_true",
                        help="run every scenario with the harness's --short holds (5 s) and smaller floods, "
                             "for a quick comparison; the table says so")
    parser.add_argument("--laps", action="store_true",
                        help="also run a --laps set and print its render_timing laps table")
    parser.add_argument("--laps-scenario", action="append", metavar="ID[/VARIANT]",
                        help="run the separate laps set for this selected variant only (a bare ID is its default "
                             "variant); repeatable; an error with --laps")
    parser.add_argument("--laps-runs", type=perf_cli.positive_int, metavar="N",
                        help="valid runs of the laps set (default: --runs); needs --laps or --laps-scenario")
    parser.add_argument("--alloc", action="store_true",
                        help="also build and run perf_scenarios_alloc and print allocations per frame")
    parser.add_argument("--counters", action="store_true",
                        help="when the head declares the perf-counters feature, also run a --counters set on "
                             "it, and on the base too when the base declares it; print the frame-counter "
                             "table and the S2/S3 overhead table")
    parser.add_argument("--counters-runs", type=perf_cli.positive_int, metavar="N",
                        help="valid runs of the counters set (default: --runs)")
    parser.add_argument("--keep", action="store_true", help="keep the worktrees and target directories")
    parser.add_argument("--build-only", type=Path, metavar="DIR",
                        help="build both refs once, list both binaries and publish them with manifest.json under "
                             "an empty DIR, printing manifest_sha256=<hex>; measures nothing (needs "
                             "--require-base)")
    parser.add_argument("--prebuilt", type=Path, metavar="DIR",
                        help="measure the binaries a --build-only job published in DIR instead of building; "
                             "refused unless its manifest matches this job")
    parser.add_argument("--prebuilt-run-id", metavar="ID", help="the workflow run that must have built --prebuilt")
    parser.add_argument("--prebuilt-attempt", metavar="N", help="the producer job's run attempt")
    parser.add_argument("--prebuilt-manifest-sha256", metavar="HEX",
                        help="the manifest digest the producer job published")
    parser.add_argument("--require-base", action="store_true",
                        help="fail unless the base builds, lists and gets every valid run the head does (CI "
                             "passes it); a counters set on a base without perf-counters still reads n/a")
    parser.add_argument("--out", type=Path,
                        help="an empty directory for the evidence and comparison.md "
                             "(default: target/perf-compare/out-<stamp>)")
    parser.add_argument("--echo-attribution", nargs="+", type=Path, metavar="ARTIFACT_DIR",
                        help="analysis only: apply #1607's frozen §4 attribution to the S2/flood echo timelines of "
                             "two replay executions' downloaded perf-comparison artifacts; builds and runs nothing")
    parser.add_argument("--echo-control", nargs=2, action="append", metavar=("NAME", "ARTIFACT_DIR"),
                        help="with --echo-attribution: one control execution's artifact, NAME one of C1-C5; give "
                             "each control twice, paired in order with the replay executions")
    parser.add_argument("--echo-platform", choices=perf_echo_analysis.ECHO_PLATFORMS,
                        help="with --echo-attribution: the platform whose S2/flood counters set is read "
                             "(default: macos)")
    args = parser.parse_args(argv)
    if args.echo_attribution is not None:
        options = (args.base, args.head, args.scenario, args.runs, args.counters_runs, args.build_only, args.prebuilt,
                   args.prebuilt_run_id, args.prebuilt_attempt, args.prebuilt_manifest_sha256, args.laps_runs,
                   args.out)
        if any(value is not None for value in options) or args.smoke or args.check_previous_release or args.short \
                or args.laps or args.alloc or args.keep or args.counters or args.require_base or args.laps_scenario:
            parser.error("--echo-attribution is analysis only: it takes no comparison, build, smoke or --out option")
        for name, _artifact in args.echo_control or []:
            if name not in perf_echo_analysis.ECHO_CONTROL_NAMES:
                parser.error(f"--echo-control NAME is one of {', '.join(perf_echo_analysis.ECHO_CONTROL_NAMES)}, not {name!r}")
        return args
    if args.echo_control or args.echo_platform:
        parser.error("--echo-control and --echo-platform need --echo-attribution")
    if args.check_previous_release:
        options = (args.base, args.head, args.scenario, args.runs, args.counters_runs, args.build_only,
                   args.prebuilt, args.prebuilt_run_id, args.prebuilt_attempt, args.prebuilt_manifest_sha256,
                   args.laps_runs)
        if any(value is not None for value in options) or args.smoke or args.short or args.laps or args.alloc \
                or args.keep or args.counters or args.require_base or args.laps_scenario:
            parser.error("--check-previous-release takes only --out")
        return args
    if args.smoke:
        options = (args.base, args.head, args.scenario, args.runs, args.out, args.counters_runs)
        binding = (args.build_only, args.prebuilt, args.prebuilt_run_id, args.prebuilt_attempt,
                   args.prebuilt_manifest_sha256)
        if any(value is not None for value in options + binding) or args.short or args.laps or args.alloc \
                or args.keep or args.counters or args.require_base or args.laps_scenario \
                or args.laps_runs is not None:
            parser.error("--smoke takes no comparison option")
    elif args.base is None or args.head is None:
        parser.error("a comparison needs --base and --head (or run --smoke)")
    elif args.laps and args.laps_scenario:
        parser.error("--laps-scenario restricts the laps set; it is an error with --laps")
    elif args.laps_runs is not None and not (args.laps or args.laps_scenario):
        parser.error("--laps-runs needs --laps or --laps-scenario")
    elif args.counters_runs is not None and not args.counters:
        parser.error("--counters-runs needs --counters")
    elif args.build_only is not None:
        measured = (args.prebuilt, args.prebuilt_run_id, args.prebuilt_attempt, args.prebuilt_manifest_sha256,
                    args.scenario, args.runs, args.counters_runs, args.out, args.laps_scenario, args.laps_runs)
        if any(value is not None for value in measured) or args.short or args.laps or args.counters or args.keep:
            parser.error("--build-only measures nothing: it takes no --prebuilt*, run, counters, --keep or --out "
                         "option (only --alloc)")
        if not args.require_base:
            parser.error("--build-only needs --require-base: a manifest always holds both refs")
    else:
        binding = (args.prebuilt_run_id, args.prebuilt_attempt, args.prebuilt_manifest_sha256)
        if args.prebuilt is not None and any(value is None for value in binding):
            parser.error("--prebuilt needs --prebuilt-run-id, --prebuilt-attempt and --prebuilt-manifest-sha256")
        if args.prebuilt is None and any(value is not None for value in binding):
            parser.error("--prebuilt-run-id, --prebuilt-attempt and --prebuilt-manifest-sha256 need --prebuilt")
        if args.prebuilt is not None and not (args.prebuilt_run_id.isdigit() and args.prebuilt_attempt.isdigit()
                                              and perf_build_policy._HEX_SHA256.fullmatch(args.prebuilt_manifest_sha256)):
            parser.error("--prebuilt-run-id and --prebuilt-attempt are numbers; --prebuilt-manifest-sha256 is 64 "
                         "lowercase hex digits")
    return args
