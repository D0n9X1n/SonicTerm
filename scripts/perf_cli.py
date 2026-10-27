"""Perf cli."""

from __future__ import annotations
import argparse
import os
from pathlib import Path
import shutil
import sys
import time
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_build_policy
import perf_driver
import perf_focus
import perf_harness_sources
import perf_report
import perf_scenarios
import perf_sets
import perf_settings
import perf_worktrees


def previous_release(host_run: Callable, root: Path) -> tuple[str, str, str] | None:
    """`(head SHA, previous tag, its commit)` by the release workflow's rule, applied to HEAD's parent: the nearest
    `v[0-9]*` tag merged into it. None for a parentless HEAD or when no earlier tag exists. A shallow
    clone is refused, because its missing history and tags could select the wrong tag."""
    def git(*argv: str) -> str:
        record = host_run(("git", "-C", str(root), *argv))
        if perf_focus._command_failure(record):
            raise ValueError(f"git {' '.join(argv)} failed: {record.stderr.strip() or record.stdout.strip()}")
        return record.stdout.strip()
    if git("rev-parse", "--is-shallow-repository") != "false":
        raise ValueError("the checkout is shallow; the previous release check needs every tag and commit")
    head_sha = git("rev-parse", "--verify", "HEAD^{commit}")
    parents = git("rev-list", "--parents", "-n", "1", head_sha).split()
    if len(parents) < 2:
        return None
    if not git("tag", "--list", "v[0-9]*", "--merged", parents[1]):
        return None
    tag = git("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*", parents[1])
    return head_sha, tag, git("rev-parse", "--verify", f"{tag}^{{commit}}")


def check_previous_release(gate, host_run: Callable, out: Path, work: Path, environ: Mapping[str, str],
                           head_root: Path = perf_settings.ROOT) -> int:
    """Overlay the head's harness and example declarations on the previous release tag and check both harness
    examples there, in an isolated target directory, with the features and harness cfgs a comparison would
    derive. Removes only the worktree and target it created."""
    found = previous_release(host_run, head_root)
    if found is None:
        print("[perf-compare] previous release: no earlier release tag, nothing to check", flush=True)
        return perf_settings.EXIT_PASS
    head_sha, tag, base_sha = found
    worktrees = perf_worktrees.Worktrees(host_run, work)
    try:
        tree = worktrees.create("base", base_sha)
        perf_harness_sources.overlay_harness(head_root, tree)
        if perf_harness_sources.tree_harness_hash(tree) != perf_harness_sources.tree_harness_hash(head_root):
            raise ValueError("the overlaid harness differs from the head's")
        harness_cfgs, tree_cfgs = perf_harness_sources.harness_cfg_decision({"head": head_root, "base": tree}, gate.HARNESS_API_CFGS)
        features = perf_harness_sources.tree_features(tree)
        step = gate.PREVIOUS_RELEASE_CHECKS[(harness_cfgs, features)]
        # The flags a comparison would compose; an inherited flag against the decision fails before the check.
        rustflags = gate.compose_harness_rustflags(environ, harness_cfgs)
        print(f"[perf-compare] previous release: head={head_sha} base={tag}={base_sha} "
              f"features={','.join(features) or 'none'} harness cfgs={','.join(harness_cfgs) or 'none'} "
              f"(base defines {','.join(tree_cfgs['base']) or 'none'}) rustflags={' '.join(rustflags)}", flush=True)
        out.mkdir(parents=True, exist_ok=True)
        result = gate.run_step(step, 1, tree, out, dict(environ, CARGO_TARGET_DIR=str(work / "target")))
        if not perf_scenarios.build_passed(result):
            print(perf_scenarios.log_tail(perf_scenarios.read_log(result.log_path), 40), flush=True)
            print(f"[perf-compare] FAIL: the head's harness does not build on {tag} ({result.status}; "
                  f"log {result.log_path})", flush=True)
            return perf_settings.EXIT_FAIL
        print(f"[perf-compare] PASS: the head's harness builds on {tag}", flush=True)
        return perf_settings.EXIT_PASS
    finally:
        for failure in worktrees.remove():
            print(f"[perf-compare] worktree removal failed: {failure}", file=sys.stderr)
        shutil.rmtree(work / "target", ignore_errors=True)
        try:
            work.rmdir()
        except OSError:
            pass  # A worktree git could not remove stays, and was reported above.


def previous_release_main(args: argparse.Namespace) -> int:
    """The `perf-previous-release` gate step: check the head's harness on the previous release tag."""
    gate = perf_settings.load_gate()
    problem = perf_build_policy.gate_problem(gate)
    if problem:
        print(f"[perf-compare] {problem}", file=sys.stderr)
        return perf_settings.EXIT_FAIL
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}"
    out = (args.out or perf_settings.ROOT / "target" / "perf-compare" / f"previous-release-{stamp}").resolve()
    work = perf_worktrees.work_directory(f"previous-release-{stamp}")
    if os.path.lexists(work):
        print(f"[perf-compare] {work} already exists; the check only removes what it created", file=sys.stderr)
        return perf_report.EXIT_USAGE
    runner = gate.SMOKE_RUNNER.run_command

    def host_run(argv: Sequence[str], timeout_s: int = perf_driver.PREVIOUS_RELEASE_TIMEOUT_S) -> CommandRecord:
        return perf_focus.bounded_command(runner, argv, timeout_s)

    try:
        return check_previous_release(gate, host_run, out, work, os.environ)
    except (OSError, ValueError) as error:
        print(f"[perf-compare] {error}", file=sys.stderr)
        return perf_settings.EXIT_FAIL


def compare_main(args: argparse.Namespace) -> int:
    """Compare two refs; remove the worktrees and target directories it created unless --keep."""
    if sys.platform not in perf_report.RUN_PLATFORMS:
        print("[perf-compare] BLOCKED: the harness runs only on macOS and Windows", flush=True)
        return perf_settings.EXIT_BLOCKED
    gate = perf_settings.load_gate()
    problem = perf_build_policy.gate_problem(gate)
    if problem:
        print(f"[perf-compare] {problem}", file=sys.stderr)
        return perf_settings.EXIT_FAIL
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}"
    out = (args.out or perf_settings.ROOT / "target" / "perf-compare" / f"out-{stamp}").resolve()
    if args.build_only is not None:
        destination = args.build_only = args.build_only.resolve()
        if destination.exists() and (not destination.is_dir() or any(destination.iterdir())):
            print(f"[perf-compare] --build-only {destination} is not an empty directory", file=sys.stderr)
            return perf_report.EXIT_USAGE
        # The build logs sit beside the published files; the workflow tars only the manifest and binaries.
        out = destination / "build-logs"
    work = perf_worktrees.work_directory(stamp)
    if out.exists() and (not out.is_dir() or any(out.iterdir())):
        print(f"[perf-compare] --out {out} is not an empty directory; two comparisons never share evidence",
              file=sys.stderr)
        return perf_report.EXIT_USAGE
    if os.path.lexists(work):
        print(f"[perf-compare] {work} already exists; a comparison only removes what it created", file=sys.stderr)
        return perf_report.EXIT_USAGE
    out.mkdir(parents=True, exist_ok=True)
    print(f"[perf-compare] evidence={out}", flush=True)
    print(f"[perf-compare] {perf_focus.focus_rule_line(os.environ)}", flush=True)
    runner = gate.SMOKE_RUNNER.run_command

    def host_run(argv: Sequence[str], timeout_s: int = perf_scenarios.GIT_TIMEOUT_S) -> CommandRecord:
        return perf_focus.bounded_command(runner, argv, timeout_s)

    worktrees = perf_worktrees.Worktrees(host_run, work)
    try:
        return perf_driver._compare(args, gate, out, work, worktrees, host_run)
    except perf_sets.StopComparison as error:
        print(f"[perf-compare] stopped: {error}", file=sys.stderr)
        return perf_settings.EXIT_FAIL
    except (OSError, ValueError) as error:
        print(f"[perf-compare] {error}", file=sys.stderr)
        return perf_settings.EXIT_FAIL
    finally:
        if args.keep:
            print(f"[perf-compare] kept the worktrees and target directories under {work}", flush=True)
        else:
            for failure in worktrees.remove():
                print(f"[perf-compare] worktree removal failed: {failure}", file=sys.stderr)
            # Everything under `work` was created by this run: it did not exist before it started.
            for side in perf_settings.SIDES:
                shutil.rmtree(work / f"target-{side}", ignore_errors=True)
            shutil.rmtree(work / perf_build_policy.PREBUILT_DIRECTORY, ignore_errors=True)
            try:
                work.rmdir()
            except OSError:
                pass  # A worktree git could not remove stays, and was reported above.


def positive_int(text: str) -> int:
    """Parse a positive integer option."""
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError(f"{text} is not a positive integer")
    return value
