#!/usr/bin/env python3
"""Contract tests for scripts/coverage-floor.py.

The floor's dangerous failure mode is a quiet pass: a crate that vanishes from
the report, a baseline measured on another host, or a crate with no eligible
line printed as 0% or 100%. Every rule is asserted against fixture JSON built
in code and written to temporary files, so the flat scripts/ directory needs no
fixture folder. The committed baseline and the gate script are checked against
the repository itself.

Run directly:  python3 scripts/coverage-floor_tests.py
Or discovered:  python3 -m unittest coverage-floor_tests   (from scripts/)
"""

from __future__ import annotations

from contextlib import contextmanager
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent
_FLOOR_PATH = _HERE / "coverage-floor.py"

_spec = importlib.util.spec_from_file_location("coverage_floor", _FLOOR_PATH)
floor = importlib.util.module_from_spec(_spec)
# Dataclasses resolve string annotations through sys.modules, so register first.
sys.modules[_spec.name] = floor
_spec.loader.exec_module(floor)

TARGET = "aarch64-apple-darwin"
RUNNER = "macos14/ARM64"
CI_ENV = {"GITHUB_ACTIONS": "true", "ImageOS": "macos14", "RUNNER_ARCH": "ARM64"}
LOCAL_ENV: dict = {}
# A path that exists on no host, so symlink resolution cannot rewrite it.
ROOT = "/nonexistent-sonicterm-fixture/checkout"

MEMBERS = ["sonicterm-grid", "sonicterm-vt"]
FILES = {
    "crates/sonicterm-grid/src/grid.rs": (400, 360),
    "crates/sonicterm-vt/src/vt.rs": (190, 165),
    "crates/sonicterm-vt/src/vt/staging.rs": (10, 9),
}
# grid 360/400 = 90.00%; vt (165 + 9)/(190 + 10) = 174/200 = 87.00%.
FLOORS = {"sonicterm-grid": (400, 360), "sonicterm-vt": (200, 174)}


def export(files, root=ROOT):
    """Build an llvm-cov JSON summary export from {path: (lines, covered)}."""
    return {
        "type": floor.LLVM_EXPORT_TYPE,
        "version": "3.1.0",
        "cargo_llvm_cov": {"version": "0.9.0", "manifest_path": f"{root}/Cargo.toml"},
        "data": [{
            "files": [
                {
                    "filename": path if path.startswith("/") else f"{root}/{path}",
                    "summary": {"lines": {"count": lines, "covered": covered,
                                          "percent": 100.0 * covered / lines if lines else 0.0}},
                }
                for path, (lines, covered) in files.items()
            ],
            "totals": {},
        }],
    }


def metadata(names, root=ROOT, build_scripts=()):
    """Build `cargo metadata --no-deps` output for members under crates/<name>."""
    packages = []
    for name in names:
        targets = [{"kind": ["lib"], "name": name, "src_path": f"{root}/crates/{name}/src/lib.rs"}]
        if name in build_scripts:
            targets.append({"kind": ["custom-build"], "name": "build-script-build",
                            "src_path": f"{root}/crates/{name}/build.rs"})
        packages.append({"id": f"path+file://{root}/crates/{name}#{name}@1.0.0", "name": name,
                         "manifest_path": f"{root}/crates/{name}/Cargo.toml", "targets": targets})
    return {
        "packages": packages,
        "workspace_members": [package["id"] for package in packages],
        "workspace_root": root,
        "target_directory": f"{root}/target/rust-logic-coverage",
        "version": 1,
    }


def baseline(crates=None, not_measured=None, target=TARGET, runner=RUNNER, tolerance=1.0, reason="fixture floor"):
    """Build a baseline document from {crate: (lines, covered)}."""
    return {
        "schema": floor.SCHEMA,
        "host": {"target": target, "runner": runner},
        "tolerance_pp": tolerance,
        "reason": reason,
        "crates": {name: {"lines": lines, "covered": covered, "percent": round(covered * 100 / lines, 2)}
                   for name, (lines, covered) in (crates or {}).items()},
        "not_measured": dict(not_measured or {}),
    }


class Run:
    """Captured result of one coverage-floor invocation."""

    def __init__(self, status, stdout, stderr):
        self.status, self.stdout, self.stderr = status, stdout, stderr


@contextmanager
def workspace(report, meta, base):
    """Write the input documents to a temporary directory and yield their paths; a str is written verbatim."""
    with tempfile.TemporaryDirectory() as directory:
        paths = {}
        for key, document in (("report", report), ("metadata", meta), ("baseline", base)):
            path = Path(directory) / f"{key}.json"
            if document is not None:
                text = document if isinstance(document, str) else json.dumps(document, indent=2) + "\n"
                path.write_text(text, encoding="utf-8")
            paths[key] = path
        yield paths


def invoke(paths, env, *extra, target=TARGET):
    """Run the floor in-process with an explicit environment and capture its output."""
    stdout, stderr = io.StringIO(), io.StringIO()
    argv = ["--report", str(paths["report"]), "--metadata", str(paths["metadata"]),
            "--baseline", str(paths["baseline"]), "--target", target,
            "--toolchain", "rustc 1.98.1 (fixture)", *extra]
    status = floor.main(argv, env=env, stdout=stdout, stderr=stderr)
    return Run(status, stdout.getvalue(), stderr.getvalue())


def check(files, members, base, env, *extra, target=TARGET, build_scripts=()):
    """Check one fixture report against one fixture baseline."""
    with workspace(export(files), metadata(members, build_scripts=build_scripts), base) as paths:
        return invoke(paths, env, *extra, target=target)


def row(output, crate):
    """Return the per-crate table row for `crate`."""
    for line in output.splitlines():
        if line.startswith(crate + " "):
            return line
    raise AssertionError(f"no table row for {crate} in:\n{output}")


def proposal(output):
    """Parse the proposed baseline printed between the markers."""
    begin = output.index(floor.PROPOSAL_BEGIN) + len(floor.PROPOSAL_BEGIN)
    return json.loads(output[begin:output.index(floor.PROPOSAL_END)])


def clean_env():
    """Return this process's environment without any CI runner identity."""
    return {key: value for key, value in os.environ.items()
            if not key.startswith("GITHUB_") and key not in ("ImageOS", "RUNNER_ARCH")}


RUSTC_VERBOSE = ("rustc 1.98.1 (48a229cea 2026-09-01)\nbinary: rustc\ncommit-hash: 48a229cea\n"
                 "host: aarch64-apple-darwin\nrelease: 1.98.1\nLLVM version: 20.1.8")
# The macos-coverage job's environment; the runner sets every variable here.
RUN_ENV = {
    **CI_ENV,
    "ImageVersion": "20260915.0150",
    "GITHUB_REPOSITORY": "D0n9X1n/SonicTerm",
    "GITHUB_WORKFLOW_REF": "D0n9X1n/SonicTerm/.github/workflows/ci.yml@refs/heads/main",
    "GITHUB_RUN_ID": "36087392875",
    "GITHUB_RUN_ATTEMPT": "1",
    "GITHUB_JOB": "macos-coverage",
    "GITHUB_EVENT_NAME": "push",
    "GITHUB_REF": "refs/heads/main",
    "GITHUB_SERVER_URL": "https://github.com",
}
ARTIFACT = "rust-logic-coverage-evidence-36087392875-1"
# Tracked content of the checkout a retained run measured.
CHECKOUT = {
    "crates/sonicterm-grid/src/grid.rs": "pub fn grid() {}\n",
    "crates/sonicterm-vt/src/vt.rs": "pub fn vt() {}\n",
    "scripts/coverage-floor.py": "# policy\n",
    "README.md": "# SonicTerm\n",
    "wiki/Home.md": "# Home\n",
    ".gitignore": "/target\n__pycache__/\n",
}


def git(root, *args):
    """Run git in a fixture checkout with a fixed identity and without the host's signing or hooks."""
    return subprocess.run(["git", "-C", str(root), "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                           "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *args],
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8",
                          check=True, timeout=60)


def commit_files(root, files, message="fixture"):
    """Write `files` into the checkout, then commit every change in it."""
    for relative, content in files.items():
        target = Path(root) / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
    git(root, "add", "-A")
    git(root, "commit", "-q", "-m", message)


def pin_checkout(root, pin):
    """Pin a fixture checkout through the floor's CLI, as the gate script does before its first record."""
    stderr = io.StringIO()
    status = floor.main(["pin-checkout", "--root", str(root), "--output", str(pin)], env={}, stdout=io.StringIO(),
                        stderr=stderr)
    if status != 0:
        raise AssertionError(f"pin-checkout exited {status}: {stderr.getvalue()}")


def record(root, output, phase, state, env, exit_status=0, report=None, meta=None, expect=0):
    """Write one provenance record through the floor's CLI and return it parsed.

    The first record for `output` pins the checkout beside it, as the gate script pins before its first record.
    """
    pin = Path(output).with_name(Path(output).name + ".pin.json")
    if not pin.exists():
        pin_checkout(root, pin)
    argv = ["record-provenance", "--output", str(output), "--root", str(root), "--pin", str(pin), "--phase", phase,
            "--state", state, "--exit-status", str(exit_status), "--target", TARGET, "--rustc-verbose",
            RUSTC_VERBOSE, "--llvm-cov", "cargo-llvm-cov 0.9.0"]
    if report is not None:
        argv += ["--report", str(report), "--metadata", str(meta)]
    stderr = io.StringIO()
    status = floor.main(argv, env=env, stdout=io.StringIO(), stderr=stderr)
    if status != expect:
        raise AssertionError(f"record-provenance exited {status}, not {expect}: {stderr.getvalue()}")
    return json.loads(Path(output).read_text(encoding="utf-8"))


@contextmanager
def retained_run(files=FILES, members=MEMBERS, base=None, env=RUN_ENV, phase="floor", state="failed", exit_status=1):
    """Yield a checkout holding the baseline, plus a retained run's report, inventory, and record.

    The default run completed its measurement and then failed the floor, as a run that reports a
    DROP does. The evidence sits outside the checkout, where the documented download puts it.
    """
    with tempfile.TemporaryDirectory() as directory:
        checkout, evidence = Path(directory) / "checkout", Path(directory) / "evidence"
        checkout.mkdir()
        evidence.mkdir()
        git(checkout, "init", "-q")
        document = baseline(FLOORS) if base is None else base
        commit_files(checkout, {**CHECKOUT, "scripts/coverage-baseline.json": json.dumps(document, indent=2) + "\n"})
        paths = {"root": checkout, "baseline": checkout / "scripts" / "coverage-baseline.json",
                 "report": evidence / floor.REPORT_FILE, "metadata": evidence / floor.INVENTORY_FILE,
                 "provenance": evidence / floor.PROVENANCE_FILE}
        paths["report"].write_text(json.dumps(export(files), indent=2) + "\n", encoding="utf-8")
        paths["metadata"].write_text(json.dumps(metadata(members), indent=2) + "\n", encoding="utf-8")
        record(checkout, paths["provenance"], phase, state, env, exit_status, paths["report"], paths["metadata"])
        yield paths


def update(paths, *extra, target=TARGET):
    """Run --update-baseline against a retained run's files."""
    stdout, stderr = io.StringIO(), io.StringIO()
    argv = ["--report", str(paths["report"]), "--metadata", str(paths["metadata"]),
            "--baseline", str(paths["baseline"]), "--target", target, "--update-baseline", *extra]
    status = floor.main(argv, env=LOCAL_ENV, stdout=stdout, stderr=stderr)
    return Run(status, stdout.getvalue(), stderr.getvalue())


def rewrite(path, **fields):
    """Replace top-level fields of a JSON document in place."""
    document = json.loads(Path(path).read_text(encoding="utf-8"))
    document.update(fields)
    Path(path).write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")


# The gate harness's deadline, and how long a killed process group may take to release the output pipes.
GATE_TIMEOUT_S = 120
GROUP_EXIT_TIMEOUT_S = 10


def run_in_session(argv, cwd, env, timeout):
    """Run `argv` in its own session; on timeout, kill its whole process group, reap the leader, and raise.

    subprocess.run's own timeout kills only the direct child and leaves a hung grandchild running. The group is
    signalled while its leader is still unreaped, so its ID cannot name another group. POSIX only, like the gate
    script these tests run.
    """
    process = subprocess.Popen(argv, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                               encoding="utf-8", start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass  # Every member exited between the timeout and the kill.
        # This reaps the leader and drains the pipes; the killed descendants are reaped by their new parent.
        stdout, stderr = process.communicate(timeout=GROUP_EXIT_TIMEOUT_S)
        raise subprocess.TimeoutExpired(argv, timeout, output=stdout, stderr=stderr) from None
    return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)


def ci_job(name):
    """Return one job's block from ci.yml."""
    workflow = (_REPO / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    return re.split(r"(?m)^  [A-Za-z0-9_-]+:\n", workflow.split(f"\n  {name}:\n", 1)[1], maxsplit=1)[0]


def ci_step(job, name):
    """Return one step's block, up to the next step item or step-level comment."""
    return re.split(r"(?m)^      (?:- |# )", job.split(f"      - name: {name}\n", 1)[1], maxsplit=1)[0]


# Stand-ins for cargo and rustc in the gate-script tests. FAKE_COVERAGE_MODE picks the outcome:
# `tests-fail` fails the instrumented run, `gate-fails` fails the 80% subset gate, `drift` commits a change
# during the instrumented run, `kill-at-inventory` kills the gate script while it writes the inventory, and
# anything else passes. The gate fails wherever --fail-under-lines appears, so a script that gates inside
# the instrumented command fails before it writes any evidence.
FAKE_CARGO = r"""#!/usr/bin/env python3
import os, signal, subprocess, sys
args = sys.argv[1:]
mode = os.environ["FAKE_COVERAGE_MODE"]
with open(os.environ["FAKE_CARGO_LOG"], "a", encoding="utf-8") as log:
    log.write(" ".join(args) + "\n")
instrumented = args[:1] == ["llvm-cov"] and args[1:2] not in (["report"], ["--version"])
if args[:2] == ["llvm-cov", "--version"]:
    print("cargo-llvm-cov 0.9.0")
elif args[:1] == ["metadata"]:
    if mode == "kill-at-inventory":
        os.kill(os.getppid(), signal.SIGKILL)
        sys.exit(1)
    print(os.environ["FAKE_METADATA"])
elif args[:2] == ["llvm-cov", "report"] and "--json" in args:
    output = args[args.index("--output-path") + 1]
    os.makedirs(os.path.dirname(output), exist_ok=True)
    with open(output, "w", encoding="utf-8") as handle:
        handle.write(os.environ["FAKE_REPORT"])
elif instrumented and mode == "tests-fail":
    sys.exit(101)
elif instrumented and mode == "drift":
    with open("crates/sonicterm-vt/src/vt.rs", "a", encoding="utf-8") as handle:
        handle.write("// changed during the measurement\n")
    # Bounded like every other git call in these fixtures, well inside the harness's group timeout.
    subprocess.run(["git", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "-c",
                    "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "commit", "-q", "-am", "drift"],
                   check=True, timeout=60)
elif "--fail-under-lines" in args:
    sys.exit(1 if mode == "gate-fails" else 0)
elif args[:1] == ["llvm-cov"]:
    sys.exit(0)
else:
    sys.exit(97)
"""
# A python3 stand-in for the publication tests. On an invocation whose arguments contain FAKE_FAULT_MATCH,
# FAKE_FAULT_ACTION `kill` kills the gate script, `fail` exits 1, and `kill-in-write` runs the tool but kills
# it inside its atomic record write. Every other call runs the real interpreter.
FAULT_SHIM = r"""#!/bin/sh
case "$*" in
  *"$FAKE_FAULT_MATCH"*)
    case "$FAKE_FAULT_ACTION" in
      kill) kill -9 "$PPID"; exit 137 ;;
      fail) exit 1 ;;
      kill-in-write)
        exec "$FAKE_REAL_PYTHON" -c 'import os, runpy, signal, sys
os.replace = lambda *args: os.kill(os.getpid(), signal.SIGKILL)
sys.argv = sys.argv[1:]
runpy.run_path(sys.argv[0], run_name="__main__")' "$@" ;;
    esac ;;
esac
exec "$FAKE_REAL_PYTHON" "$@"
"""
FAKE_RUSTC = "#!/bin/sh\ncat <<'EOF'\n" + RUSTC_VERBOSE + "\nEOF\n"


# Markers generators write at the top of their output; scanning only the first
# lines keeps prose that mentions a generator from matching.
GENERATOR_MARKER = re.compile(r"automatically generated by|@generated|do not edit", re.IGNORECASE)
MARKER_LINES = 5


def first_party_rust_sources():
    """Return the repository's first-party .rs paths, relative and POSIX-style."""
    if (_REPO / ".git").exists():
        listed = subprocess.run(["git", "-C", str(_REPO), "ls-files", "--", "*.rs"], stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, encoding="utf-8", check=True, timeout=60)
        paths = listed.stdout.splitlines()
    else:
        # A `git archive` export has no index; every file in it is tracked content.
        paths = [path.relative_to(_REPO).as_posix() for path in _REPO.rglob("*.rs")]
    vendored = tuple(root + "/" for root in floor.VENDORED_ROOTS)
    return sorted(path for path in paths if not path.startswith(vendored) and not path.startswith("target/"))


def has_generator_marker(path):
    """Whether the first lines of the repository file `path` carry a generator marker."""
    with open(_REPO / path, encoding="utf-8", errors="replace") as handle:
        head = "".join(line for _, line in zip(range(MARKER_LINES), handle))
    return bool(GENERATOR_MARKER.search(head))


class FloorComparisonTests(unittest.TestCase):
    """A measured crate is held to its floor within the tolerance, never beyond it."""

    def test_drop_beyond_tolerance_fails(self):
        # vt falls from 87.00% to 85.50%: 1.5 pp, more than the 1.0 pp tolerance.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 162)}
        run = check(files, MEMBERS, baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("DROP", row(run.stdout, "sonicterm-vt"))
        self.assertIn("-1.50", row(run.stdout, "sonicterm-vt"))
        self.assertIn("1.50 pp below its floor 87.00% (174/200)", run.stdout)
        self.assertIn("coverage floor FAIL", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        # Lowering a floor is a reviewer's decision, so a drop prints no proposal.
        self.assertNotIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_drop_within_tolerance_passes(self):
        # vt falls from 87.00% to 86.50%, inside the tolerance.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 164)}
        run = check(files, MEMBERS, baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(row(run.stdout, "sonicterm-vt"), r"\sok\s")
        self.assertIn("-0.50", row(run.stdout, "sonicterm-vt"))
        self.assertIn("coverage floor PASS: 2 measured crates within 1.0 pp", run.stdout)

    def test_drop_of_exactly_the_tolerance_holds(self):
        # 55% -> 54% is exactly 1.0 pp, but float arithmetic puts this pair past the
        # boundary, so the comparison must use exact fractions to hold here.
        self.assertLess(27 / 50 * 100 - 11 / 20 * 100, -1.0)
        files = {"crates/sonicterm-grid/src/grid.rs": (400, 360), "crates/sonicterm-vt/src/vt.rs": (50, 27)}
        floors = {"sonicterm-grid": (400, 360), "sonicterm-vt": (20, 11)}
        run = check(files, MEMBERS, baseline(floors), CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("-1.00", row(run.stdout, "sonicterm-vt"))

    def test_improvement_never_raises_the_floor(self):
        # A rise is reported, but only a reviewed update moves the floor.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 181)}
        with workspace(export(files), metadata(MEMBERS), baseline(FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, CI_ENV)
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("+8.00", row(run.stdout, "sonicterm-vt"))


class MissingBaselineTests(unittest.TestCase):
    """A measured crate without a floor fails visibly and is proposed for review."""

    def test_crate_without_baseline_fails_visibly_with_a_proposal(self):
        run = check(FILES, MEMBERS, baseline({"sonicterm-grid": (400, 360)}), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NO BASELINE", row(run.stdout, "sonicterm-vt"))
        self.assertIn("sonicterm-vt: measures 87.00% (174/200 lines) but has no baseline entry", run.stdout)
        proposed = proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-vt"], {"lines": 200, "covered": 174, "percent": 87.0})
        self.assertEqual(proposed["crates"]["sonicterm-grid"], {"lines": 400, "covered": 360, "percent": 90.0})
        # The reason is the reviewer's to write; an unfilled proposal cannot load.
        self.assertEqual(proposed["reason"], "")
        with self.assertRaises(floor.InputError):
            floor.parse_baseline(proposed, "proposal")

    def test_initial_baseline_without_floors_fails_and_proposes_every_crate(self):
        # The committed initial baseline carries only host and declarations.
        declared = {"sonicterm-harfbuzz": "declarations only"}
        run = check(FILES, MEMBERS + ["sonicterm-harfbuzz"], baseline({}, declared), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        proposed = proposal(run.stdout)
        self.assertEqual(sorted(proposed["crates"]), ["sonicterm-grid", "sonicterm-vt"])
        self.assertEqual(proposed["not_measured"], declared)
        self.assertEqual(proposed["host"], {"target": TARGET, "runner": RUNNER})
        filled = dict(proposed, reason="initial floors from the first CI run")
        self.assertEqual(floor.parse_baseline(filled, "filled").crates["sonicterm-vt"], floor.Entry(200, 174))

    def test_new_measured_crate_fails_while_existing_floors_hold(self):
        files = {**FILES, "crates/sonicterm-new/src/lib.rs": (50, 40)}
        run = check(files, MEMBERS + ["sonicterm-new"], baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NO BASELINE", row(run.stdout, "sonicterm-new"))
        self.assertRegex(row(run.stdout, "sonicterm-grid"), r"\sok\s")
        self.assertRegex(row(run.stdout, "sonicterm-vt"), r"\sok\s")
        proposed = proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-new"]["percent"], 80.0)
        # Existing floors are copied verbatim, so the adopted diff only adds the new crate.
        self.assertEqual(proposed["crates"]["sonicterm-vt"], baseline(FLOORS)["crates"]["sonicterm-vt"])


class NotMeasuredTests(unittest.TestCase):
    """A crate without eligible lines is `not measured` with a reason, never 0% or 100%."""

    def test_target_inapplicable_crate_is_not_measured_with_its_reason(self):
        # A crate that cannot build for this host is declared with a reason and passes without a number.
        why = "target-inapplicable here: linked only for non-macOS Unix"
        run = check(FILES, MEMBERS + ["sonicterm-fontconfig"], baseline(FLOORS, {"sonicterm-fontconfig": why}), CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        line = row(run.stdout, "sonicterm-fontconfig")
        self.assertIn("declared", line)
        self.assertIn("not measured", line)
        self.assertIn(why, line)
        self.assertNotIn("%", line)
        self.assertIn("1 declared not measured", run.stdout)

    def test_zero_eligible_line_crate_is_not_measured_never_zero_percent(self):
        # Only test files are reported for harfbuzz, so no line is eligible.
        files = {**FILES, "crates/sonicterm-harfbuzz/src/lib_tests.rs": (30, 30),
                 "crates/sonicterm-harfbuzz/tests/shape.rs": (12, 12)}
        members = MEMBERS + ["sonicterm-harfbuzz"]
        declared = check(files, members, baseline(FLOORS, {"sonicterm-harfbuzz": "declarations only"}), CI_ENV)
        self.assertEqual(declared.status, 0, declared.stdout)
        self.assertIn("not measured", row(declared.stdout, "sonicterm-harfbuzz"))
        self.assertNotIn("%", row(declared.stdout, "sonicterm-harfbuzz"))
        undeclared = check(files, members, baseline(FLOORS), CI_ENV)
        self.assertEqual(undeclared.status, 1, undeclared.stdout)
        line = row(undeclared.stdout, "sonicterm-harfbuzz")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("every reported file is excluded (test: 2)", line)
        self.assertNotIn("%", line)

    def test_files_without_instrumented_lines_are_not_measured(self):
        # A reported file with zero instrumented lines is eligible but yields no percentage.
        files = {**FILES, "crates/sonicterm-fontconfig/src/lib.rs": (0, 0)}
        run = check(files, MEMBERS + ["sonicterm-fontconfig"], baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = row(run.stdout, "sonicterm-fontconfig")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("1 reported file(s) contain no instrumented line", line)
        self.assertNotIn("%", line)

    def test_unexplained_inventory_member_fails(self):
        run = check(FILES, MEMBERS + ["sonicterm-mystery"], baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = row(run.stdout, "sonicterm-mystery")
        self.assertIn("UNEXPLAINED", line)
        self.assertIn("no reported file", line)
        self.assertNotIn("%", line)
        self.assertIn("no floor, and no not-measured declaration", run.stdout)
        # The proposal names the member with a blank reason a reviewer must write.
        self.assertEqual(proposal(run.stdout)["not_measured"], {"sonicterm-mystery": ""})

    def test_declared_crate_that_starts_reporting_lines_fails(self):
        # A declaration must not hide a crate that now has lines; it needs a floor instead.
        files = {**FILES, "crates/sonicterm-fontconfig/src/lib.rs": (20, 10)}
        base = baseline(FLOORS, {"sonicterm-fontconfig": "declarations only"})
        run = check(files, MEMBERS + ["sonicterm-fontconfig"], base, CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("NOW MEASURED", row(run.stdout, "sonicterm-fontconfig"))
        proposed = proposal(run.stdout)
        self.assertEqual(proposed["crates"]["sonicterm-fontconfig"]["percent"], 50.0)
        self.assertNotIn("sonicterm-fontconfig", proposed["not_measured"])

    def test_declaration_for_a_non_member_fails(self):
        # Declarations reconcile against the inventory, so one naming a removed member fails.
        run = check(FILES, MEMBERS, baseline(FLOORS, {"sonicterm-removed": "declarations only"}), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("sonicterm-removed: declared not measured but is not a workspace member", run.stdout)


class MissingCrateTests(unittest.TestCase):
    """A crate with a floor never leaves the floor silently."""

    def test_previously_measured_crate_missing_from_report_fails(self):
        # A vanished crate's row carries no number; its old floor appears only in the finding.
        run = check({"crates/sonicterm-grid/src/grid.rs": (400, 360)}, MEMBERS, baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        line = row(run.stdout, "sonicterm-vt")
        self.assertIn("MISSING", line)
        self.assertIn("not measured", line)
        self.assertIn("no reported file", line)
        self.assertNotIn("%", line)
        self.assertIn("has a floor of 87.00% (174/200)", run.stdout)
        self.assertIn("must not leave the floor silently", run.stdout)
        # Removing a floor is a reviewer's decision, so no proposal is printed.
        self.assertNotIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_previously_measured_crate_removed_from_workspace_fails(self):
        # Deleting a member does not delete its floor silently.
        run = check({"crates/sonicterm-grid/src/grid.rs": (400, 360)}, ["sonicterm-grid"], baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("sonicterm-vt: has a floor of 87.00% but is no longer a workspace member", run.stdout)


class HostIdentityTests(unittest.TestCase):
    """A baseline from another host is never a passing result."""

    def test_runner_mismatch_in_ci_fails_as_not_comparable(self):
        run = check(FILES, MEMBERS, baseline(FLOORS), dict(CI_ENV, ImageOS="macos15"))
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("coverage floor FAIL: not comparable", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        # The replacement proposal is keyed to the host that measured it.
        self.assertEqual(proposal(run.stdout)["host"], {"target": TARGET, "runner": "macos15/ARM64"})

    def test_target_mismatch_in_ci_fails_as_not_comparable(self):
        # Another target triple is another host, even on the same runner image.
        run = check(FILES, MEMBERS, baseline(FLOORS), CI_ENV, target="x86_64-apple-darwin")
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn("not comparable", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertEqual(proposal(run.stdout)["host"]["target"], "x86_64-apple-darwin")

    def test_local_run_is_informational_and_never_passes(self):
        # Outside CI the floor decides nothing, even when every number matches.
        run = check(FILES, MEMBERS, baseline(FLOORS), LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("informational only; no verdict outside CI", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertNotIn("FAIL", run.stdout)
        self.assertNotIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_local_run_reports_deltas_without_failing(self):
        # A drop and an unexplained member are shown locally but decide nothing.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 150)}
        run = check(files, MEMBERS + ["sonicterm-mystery"], baseline(FLOORS), LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertIn("delta (info)", run.stdout)
        self.assertIn("DROP", row(run.stdout, "sonicterm-vt"))
        self.assertIn("CI on the baseline host would fail on each", run.stdout)
        self.assertNotIn("PASS", run.stdout)
        self.assertNotIn("FAIL", run.stdout)
        # Outside CI there is no artifact to name, so the DROP guidance is not printed.
        self.assertNotIn("A DROP is never proposed", run.stdout)

    def test_baseline_recorded_as_local_still_gives_no_local_verdict(self):
        # Matching a hand-written `local` identity must not turn a local run into a pass.
        run = check(FILES, MEMBERS, baseline(FLOORS, runner="local"), LOCAL_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertNotIn("PASS", run.stdout)

    def test_runner_identity_comes_from_the_ci_runner_image(self):
        # The identity is ImageOS/RUNNER_ARCH in CI and `local` everywhere else.
        self.assertEqual(floor.runner_identity(CI_ENV), "macos14/ARM64")
        self.assertEqual(floor.runner_identity({}), "local")
        self.assertEqual(floor.runner_identity({"GITHUB_ACTIONS": "false", "ImageOS": "macos14"}), "local")
        self.assertEqual(floor.runner_identity({"GITHUB_ACTIONS": "true"}), "unknown-image/unknown-arch")


class UpdateBaselineTests(unittest.TestCase):
    """The baseline changes only through an explicit, reasoned update."""

    def test_reviewed_update_writes_a_baseline_that_then_passes(self):
        # A verified update writes exact counts that the next CI check accepts; its reason names the run.
        declared = {"sonicterm-harfbuzz": "declarations only"}
        members = MEMBERS + ["sonicterm-harfbuzz"]
        with retained_run(members=members, base=baseline({}, declared), state="done", exit_status=0) as paths:
            checks = json.loads(paths["provenance"].read_text(encoding="utf-8"))["checks"]
            written = update(paths, "--reason", "initial floors from CI run 1", "--provenance",
                             str(paths["provenance"]))
            stored = floor.load_baseline(str(paths["baseline"]))
            checked = invoke(paths, CI_ENV)
        self.assertEqual(checks, {"subset-gate": "exit status 0", "floor": "exit status 0"})
        self.assertEqual(written.status, 0, written.stderr)
        self.assertEqual(stored.crates, {"sonicterm-grid": floor.Entry(400, 360),
                                         "sonicterm-vt": floor.Entry(200, 174)})
        self.assertEqual(stored.not_measured, declared)
        self.assertTrue(stored.reason.startswith(
            "initial floors from CI run 1. Measured by https://github.com/D0n9X1n/SonicTerm/actions/runs/"
            "36087392875 attempt 1, job macos-coverage"), stored.reason)
        for fact in (ARTIFACT, "macos14/ARM64 (image 20260915.0150)", "rustc 1.98.1 (48a229cea 2026-09-01)",
                     "cargo-llvm-cov 0.9.0"):
            self.assertIn(fact, stored.reason)
        self.assertIn("the verified provenance record", written.stdout)
        self.assertEqual((stored.target, stored.runner), (TARGET, RUNNER))
        self.assertIn("+ sonicterm-vt: 87.00% (174/200)", written.stdout)
        self.assertEqual(checked.status, 0, checked.stdout)
        self.assertIn("coverage floor PASS", checked.stdout)

    def test_update_without_a_reason_is_refused_and_writes_nothing(self):
        # A missing or blank reason leaves the committed floor untouched.
        for extra in ((), ("--reason", ""), ("--reason", "   ")):
            with self.subTest(extra=extra):
                with workspace(export(FILES), metadata(MEMBERS), baseline({})) as paths:
                    before = paths["baseline"].read_bytes()
                    run = invoke(paths, LOCAL_ENV, "--update-baseline", "--runner", RUNNER, *extra)
                    self.assertEqual(paths["baseline"].read_bytes(), before)
                self.assertEqual(run.status, 2)
                self.assertIn("--reason", run.stderr)

    def test_update_without_a_runner_is_refused(self):
        # The host comes from a verified record or an explicit local runner, never from the environment.
        with workspace(export(FILES), metadata(MEMBERS), baseline({})) as paths:
            run = invoke(paths, CI_ENV, "--update-baseline", "--reason", "floors")
        self.assertEqual(run.status, 2)
        self.assertIn("--runner", run.stderr)
        self.assertIn("--provenance", run.stderr)

    def test_update_refuses_an_unmeasured_undeclared_member(self):
        # The tool cannot invent why a crate is unmeasured; a local baseline needs no provenance to show it.
        local = baseline({}, runner="local")
        with workspace(export(FILES), metadata(MEMBERS + ["sonicterm-mystery"]), local) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", "local")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("not measured and not declared: sonicterm-mystery (no reported file)", run.stderr)

    def test_partial_update_changes_only_the_named_crate(self):
        # `--crate` rewrites one floor and keeps every other entry verbatim; a local baseline needs no record.
        files = {"crates/sonicterm-grid/src/grid.rs": (400, 300), "crates/sonicterm-vt/src/vt.rs": (200, 150)}
        with workspace(export(files), metadata(MEMBERS), baseline(FLOORS, runner="local")) as paths:
            run = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "vt drops dead parser paths",
                         "--runner", "local", "--crate", "sonicterm-vt")
            stored = floor.load_baseline(str(paths["baseline"]))
        self.assertEqual(run.status, 0, run.stderr)
        self.assertEqual(stored.crates["sonicterm-vt"], floor.Entry(200, 150))
        self.assertEqual(stored.crates["sonicterm-grid"], floor.Entry(400, 360))

    def test_partial_update_refuses_another_host(self):
        # `--crate` keeps the same-host rule: a verified record from another runner image cannot change one floor.
        with retained_run(env=dict(RUN_ENV, ImageOS="macos15")) as paths:
            before = paths["baseline"].read_bytes()
            run = update(paths, "--reason", "floors", "--provenance", str(paths["provenance"]),
                         "--crate", "sonicterm-vt")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("cannot mix hosts: the baseline is for aarch64-apple-darwin on macos14/ARM64, "
                      "this report is aarch64-apple-darwin on macos15/ARM64", run.stderr)

    def test_check_never_rewrites_the_baseline(self):
        # Even a failing run that prints a proposal leaves the baseline untouched.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 100), "crates/sonicterm-new/src/lib.rs": (10, 1)}
        with workspace(export(files), metadata(MEMBERS + ["sonicterm-new"]), baseline(FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, CI_ENV)
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 1)
        self.assertIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_update_only_options_are_rejected_in_check_mode(self):
        # Check mode reads the runner from CI, so a stated runner cannot fake a match.
        for extra in (("--reason", "x"), ("--runner", RUNNER), ("--crate", "sonicterm-vt"),
                      ("--provenance", "record.json")):
            with self.subTest(extra=extra):
                run = check(FILES, MEMBERS, baseline(FLOORS), CI_ENV, *extra)
                self.assertEqual(run.status, 2)
                self.assertIn("only valid with --update-baseline", run.stderr)


class ProvenanceRecordTests(unittest.TestCase):
    """Each run's record says how far the measurement got and never claims a report it lacks."""

    def scratch_checkout(self):
        """Return a committed scratch checkout and a record path outside it."""
        directory = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, directory, True)
        checkout = Path(directory) / "checkout"
        checkout.mkdir()
        git(checkout, "init", "-q")
        commit_files(checkout, CHECKOUT)
        return checkout, Path(directory) / "record.json"

    def test_incomplete_run_names_its_failed_phase_and_carries_no_report(self):
        # A build or test failure leaves an incomplete record: the failed phase, its status, and no digest.
        checkout, output = self.scratch_checkout()
        document = record(checkout, output, "instrumented-tests", "failed", RUN_ENV, 101)
        self.assertEqual(document["schema"], floor.PROVENANCE_SCHEMA)
        self.assertEqual(document["measurement"], "incomplete")
        self.assertEqual((document["failed_phase"], document["failure"]), ("instrumented-tests", "exit status 101"))
        self.assertEqual(document["checks"], {"subset-gate": "not run", "floor": "not run"})
        self.assertIsNone(document["report_sha256"])
        self.assertIsNone(document["inventory_sha256"])
        self.assertEqual(document["commit"], git(checkout, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual((document["runner"], document["image_version"]), ("macos14/ARM64", "20260915.0150"))
        self.assertEqual((document["artifact"], document["workflow"]), (ARTIFACT, ".github/workflows/ci.yml"))
        self.assertEqual(document["rustc_version"], "rustc 1.98.1 (48a229cea 2026-09-01)")
        self.assertEqual(document["cargo_llvm_cov"], "cargo-llvm-cov 0.9.0")
        self.assertEqual(document["worktree_changes"], [])
        self.assertIn("crates/sonicterm-vt/src/vt.rs", document["tree_entries"])

    def test_interrupted_phase_leaves_its_write_ahead_record(self):
        # A run killed inside a phase keeps the record written when that phase started.
        checkout, output = self.scratch_checkout()
        document = record(checkout, output, "report", "running", RUN_ENV)
        self.assertEqual(document["measurement"], "incomplete")
        self.assertEqual((document["failed_phase"], document["failure"]),
                         ("report", "interrupted before the phase finished"))

    def test_complete_run_digests_its_files_even_when_a_check_fails(self):
        # The measurement is complete once the inventory exists; a failing 80% gate does not undo it.
        with retained_run(phase="subset-gate", state="failed", exit_status=1) as paths:
            document = json.loads(paths["provenance"].read_text(encoding="utf-8"))
            digests = (hashlib.sha256(paths["report"].read_bytes()).hexdigest(),
                       hashlib.sha256(paths["metadata"].read_bytes()).hexdigest())
        self.assertEqual(document["measurement"], "complete")
        self.assertIsNone(document["failed_phase"])
        self.assertEqual(document["checks"], {"subset-gate": "exit status 1", "floor": "not run"})
        self.assertEqual((document["report_sha256"], document["inventory_sha256"]), digests)

    def test_complete_record_without_its_files_is_refused(self):
        # The writer never claims a report it cannot digest.
        checkout, output = self.scratch_checkout()
        pin = output.with_name("pin.json")
        pin_checkout(checkout, pin)
        stderr = io.StringIO()
        status = floor.main(["record-provenance", "--output", str(output), "--root", str(checkout), "--pin", str(pin),
                             "--phase", "floor", "--state", "done"], env=RUN_ENV, stdout=io.StringIO(), stderr=stderr)
        self.assertEqual(status, 2)
        self.assertFalse(output.exists())
        self.assertIn("needs --report and --metadata", stderr.getvalue())


class ProvenanceWriterV2Tests(unittest.TestCase):
    """Records carry the pinned checkout, name drift at publish, and only write check pairs a run can reach."""

    def test_publish_record_names_each_kind_of_checkout_drift(self):
        # The publish record compares HEAD, its tree, and the coverage-relevant uncommitted changes with the pin;
        # documentation changes alone are not drift, and the record keeps the pinned commit.
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, str(directory), True)
        checkout, output = directory / "checkout", directory / "record.json"
        checkout.mkdir()
        git(checkout, "init", "-q")
        commit_files(checkout, CHECKOUT)
        document = record(checkout, output, "self-test", "running", RUN_ENV)
        pin = json.loads(output.with_name(output.name + ".pin.json").read_text(encoding="utf-8"))
        self.assertEqual(floor.checkout_drift(pin, str(checkout)), [])
        (checkout / "README.md").write_text("# reworded\n", encoding="utf-8")
        self.assertEqual(floor.checkout_drift(pin, str(checkout)), [])
        (checkout / "crates/sonicterm-vt/src/vt.rs").write_text("pub fn vt() { changed(); }\n", encoding="utf-8")
        self.assertEqual(floor.checkout_drift(pin, str(checkout)),
                         ["the coverage-relevant uncommitted changes moved from [] to [crates/sonicterm-vt/src/vt.rs]"])
        commit_files(checkout, {})
        head = git(checkout, "rev-parse", "HEAD").stdout.strip()
        drift = floor.checkout_drift(pin, str(checkout))
        self.assertEqual(len(drift), 2, drift)
        self.assertEqual(drift[0], f"HEAD moved from {document['commit']} to {head}")
        self.assertTrue(drift[1].startswith("the tree moved from "), drift)
        published = record(checkout, output, "publish", "running", RUN_ENV, expect=floor.DRIFT_EXIT_STATUS)
        self.assertEqual((published["measurement"], published["failed_phase"]), ("incomplete", "publish"))
        self.assertEqual(published["failure"], "checkout drifted from its pin: " + "; ".join(drift))
        self.assertEqual(published["checkout_drift"], drift)
        self.assertEqual((published["commit"], published["tree"]), (document["commit"], document["tree"]))

    def test_every_complete_record_the_writer_produces_passes_the_checks_rule(self):
        # The rule refuses only pairs the gate script cannot write, so no genuine complete record trips it.
        with retained_run() as paths:
            for phase, state, status in (("subset-gate", "running", 0), ("subset-gate", "failed", 1),
                                         ("subset-gate", "failed", 137), ("floor", "running", 0),
                                         ("floor", "failed", 1), ("floor", "failed", 2), ("floor", "done", 0)):
                with self.subTest(phase=phase, state=state, status=status):
                    document = record(paths["root"], paths["provenance"], phase, state, RUN_ENV, status,
                                      paths["report"], paths["metadata"])
                    self.assertEqual(document["measurement"], "complete")
                    self.assertIsNone(document["checkout_drift"])
                    self.assertEqual(floor.checks_problems(document["checks"]), [])


class GateScriptEvidenceTests(unittest.TestCase):
    """The gate script pins its checkout and publishes its report and inventory as one unit, before either check."""

    def run_gate(self, mode, env_extra=None, fault=None, broken_tool=False):
        """Run a copy of the gate script in a scratch checkout with stand-in cargo and rustc.

        `fault` is an (action, match) pair for the python3 stand-in. Returns the completed process, the
        checkout, and the cargo calls.
        """
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, str(directory), True)
        root, bin_dir, log = directory / "checkout", directory / "bin", directory / "cargo.log"
        (root / "scripts").mkdir(parents=True)
        bin_dir.mkdir()
        for name in ("rust-logic-coverage.sh", "coverage-floor.py"):
            shutil.copyfile(_HERE / name, root / "scripts" / name)
        if broken_tool:
            (root / "scripts" / "coverage-floor.py").write_text("raise SystemExit(5)\n", encoding="utf-8")
        # A stand-in self-test, so the copied gate does not run this suite again.
        (root / "scripts" / "coverage-floor_tests.py").write_text("", encoding="utf-8")
        (root / "scripts" / "coverage-baseline.json").write_text(json.dumps(baseline(FLOORS), indent=2) + "\n",
                                                                 encoding="utf-8")
        (root / ".gitignore").write_text("/target\n__pycache__/\n", encoding="utf-8")
        git(root, "init", "-q")
        commit_files(root, {"crates/sonicterm-vt/src/vt.rs": "pub fn vt() {}\n"})
        tools = {"cargo": FAKE_CARGO, "rustc": FAKE_RUSTC, **({"python3": FAULT_SHIM} if fault else {})}
        for name, body in tools.items():
            (bin_dir / name).write_text(body, encoding="utf-8")
            (bin_dir / name).chmod(0o755)
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 150)} if mode == "floor-fails" else FILES
        env = {key: value for key, value in clean_env().items()
               if not key.startswith(("CARGO", "RUSTUP", "GIT_", "COVERAGE_", "FAKE_")) and key != "ImageVersion"}
        env.update(env_extra or {})
        env.update({"PATH": f"{bin_dir}{os.pathsep}{env.get('PATH', '')}", "FAKE_COVERAGE_MODE": mode,
                    "FAKE_CARGO_LOG": str(log), "FAKE_REPORT": json.dumps(export(files, root=str(root))),
                    "FAKE_METADATA": json.dumps(metadata(MEMBERS, root=str(root))), "FAKE_REAL_PYTHON": sys.executable})
        if fault:
            env.update({"FAKE_FAULT_ACTION": fault[0], "FAKE_FAULT_MATCH": fault[1]})
        completed = run_in_session(["bash", str(root / "scripts" / "rust-logic-coverage.sh")], str(root), env,
                                   GATE_TIMEOUT_S)
        calls = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
        return completed, root, calls

    def visible(self, root):
        """Return the evidence directory and its visible entries, which upload-artifact collects by default."""
        evidence = root / floor.EVIDENCE_DIRECTORY
        return evidence, sorted(path.name for path in evidence.iterdir() if not path.name.startswith("."))

    def update_from(self, root, measurement):
        """Run --update-baseline in the scratch checkout from its record and a directory holding a report and inventory."""
        stdout, stderr = io.StringIO(), io.StringIO()
        status = floor.main(["--report", str(measurement / floor.REPORT_FILE), "--metadata",
                             str(measurement / floor.INVENTORY_FILE), "--baseline",
                             str(root / "scripts" / "coverage-baseline.json"), "--target", TARGET, "--update-baseline",
                             "--reason", "rebaseline", "--provenance",
                             str(root / floor.EVIDENCE_DIRECTORY / floor.PROVENANCE_FILE)],
                            env=LOCAL_ENV, stdout=stdout, stderr=stderr)
        return Run(status, stdout.getvalue(), stderr.getvalue())

    def test_failed_subset_gate_keeps_the_published_measurement(self):
        # The measurement is published before the 80% gate runs, and the run still fails with the gate's status.
        completed, root, calls = self.run_gate("gate-fails")
        self.assertEqual(completed.returncode, 1, completed.stdout + completed.stderr)
        evidence, names = self.visible(root)
        self.assertEqual(names, [floor.PROVENANCE_FILE, floor.MEASUREMENT_DIRECTORY])
        measurement = evidence / floor.MEASUREMENT_DIRECTORY
        self.assertEqual(sorted(path.name for path in measurement.iterdir()), [floor.REPORT_FILE, floor.INVENTORY_FILE])
        # The staged directory was renamed into place, not copied.
        self.assertFalse((root / floor.WORK_DIRECTORY / floor.STAGING_DIRECTORY).exists())
        document = json.loads((evidence / floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        self.assertEqual(document["measurement"], "complete")
        self.assertEqual(document["checks"], {"subset-gate": "exit status 1", "floor": "not run"})
        self.assertEqual(document["report_sha256"],
                         hashlib.sha256((measurement / floor.REPORT_FILE).read_bytes()).hexdigest())
        self.assertEqual(document["inventory_sha256"],
                         hashlib.sha256((measurement / floor.INVENTORY_FILE).read_bytes()).hexdigest())
        # Bytecode and target output are ignored, so the pinned checkout is clean.
        self.assertEqual(document["worktree_changes"], [])
        self.assertEqual(document["commit"], git(root, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual([call.split(" ")[:2] for call in calls],
                         [["llvm-cov", "--version"], ["llvm-cov", "--workspace"], ["llvm-cov", "report"],
                          ["metadata", "--no-deps"], ["llvm-cov", "report"]])
        self.assertIn("--no-report", calls[1])
        self.assertIn("--fail-under-lines 80", calls[4])

    def test_failed_floor_keeps_the_evidence_and_names_the_artifact(self):
        # A DROP fails the run after the measurement is published, and its message names this run's artifact.
        completed, root, _calls = self.run_gate("floor-fails", RUN_ENV)
        self.assertEqual(completed.returncode, 1, completed.stdout + completed.stderr)
        document = json.loads((root / floor.EVIDENCE_DIRECTORY / floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        self.assertEqual(document["measurement"], "complete")
        self.assertEqual(document["checks"], {"subset-gate": "exit status 0", "floor": "exit status 1"})
        self.assertEqual(document["artifact"], ARTIFACT)
        self.assertIn("coverage floor FAIL", completed.stdout)
        self.assertIn(f"the artifact {ARTIFACT}", completed.stdout)
        self.assertNotIn(floor.PROPOSAL_BEGIN, completed.stdout)

    def test_failed_test_run_uploads_only_its_incomplete_record(self):
        # A run whose instrumented tests fail publishes nothing, only a record marked incomplete.
        completed, root, calls = self.run_gate("tests-fail")
        self.assertEqual(completed.returncode, 101, completed.stdout + completed.stderr)
        evidence = root / floor.EVIDENCE_DIRECTORY
        self.assertEqual([path.name for path in evidence.iterdir()], [floor.PROVENANCE_FILE])
        document = json.loads((evidence / floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        self.assertEqual(document["measurement"], "incomplete")
        self.assertEqual((document["failed_phase"], document["failure"]), ("instrumented-tests", "exit status 101"))
        self.assertIsNone(document["report_sha256"])
        self.assertEqual(document["cargo_llvm_cov"], "cargo-llvm-cov 0.9.0")
        self.assertEqual(len(calls), 2)

    def test_checkout_drift_during_the_measurement_is_named_and_publishes_nothing(self):
        # A clean commit during the instrumented tests fails the run at publish: nothing is published, the record
        # keeps the pinned commit and names the drift, and the checker refuses it.
        completed, root, _calls = self.run_gate("drift")
        self.assertEqual(completed.returncode, floor.DRIFT_EXIT_STATUS, completed.stdout + completed.stderr)
        evidence = root / floor.EVIDENCE_DIRECTORY
        self.assertEqual([path.name for path in evidence.iterdir()], [floor.PROVENANCE_FILE])
        document = json.loads((evidence / floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        pinned, head = (git(root, "rev-parse", rev).stdout.strip() for rev in ("HEAD~1", "HEAD"))
        self.assertEqual((document["measurement"], document["failed_phase"]), ("incomplete", "publish"))
        self.assertIn(f"checkout drifted from its pin: HEAD moved from {pinned} to {head}", document["failure"])
        self.assertEqual(document["commit"], pinned)
        self.assertTrue(document["checkout_drift"])
        run = self.update_from(root, root / floor.WORK_DIRECTORY / floor.STAGING_DIRECTORY)
        self.assertEqual(run.status, 2, run.stdout + run.stderr)
        self.assertIn("is incomplete: the run stopped in phase 'publish' (checkout drifted from its pin", run.stderr)

    def test_publication_leaves_both_files_or_neither_at_every_boundary(self):
        # Killed or failed around publication, the evidence holds the record alone or the record beside both
        # files, and no record short of complete vouches for them.
        final = "--phase subset-gate --state running"
        cases = {
            "killed while staging the inventory": ("kill-at-inventory", None, -9, "inventory", False),
            "killed before the rename": ("passes", ("kill", "os.rename"), -9, "publish", False),
            "the rename fails": ("passes", ("fail", "os.rename"), 1, "publish", False),
            "killed after the rename, before the complete record": ("passes", ("kill", final), -9, "publish", True),
            "killed inside the complete record's atomic write": ("passes", ("kill-in-write", final), 137, "publish",
                                                                 True),
        }
        for label, (mode, fault, status, phase, published) in cases.items():
            with self.subTest(boundary=label):
                completed, root, _calls = self.run_gate(mode, fault=fault)
                self.assertEqual(completed.returncode, status, completed.stdout + completed.stderr)
                evidence, names = self.visible(root)
                self.assertEqual(names, [floor.PROVENANCE_FILE] + ([floor.MEASUREMENT_DIRECTORY] if published else []))
                if published:
                    self.assertEqual(sorted(path.name for path in (evidence / floor.MEASUREMENT_DIRECTORY).iterdir()),
                                     [floor.REPORT_FILE, floor.INVENTORY_FILE])
                document = json.loads((evidence / floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
                self.assertEqual((document["measurement"], document["failed_phase"]), ("incomplete", phase))
                self.assertIsNone(document["report_sha256"])
                # Only a write killed inside the atomic replace leaves a temporary file, and it is hidden.
                hidden = [path.name for path in evidence.iterdir() if path.name.startswith(".")]
                self.assertEqual(len(hidden), 1 if fault and fault[0] == "kill-in-write" else 0, hidden)
                if published:
                    run = self.update_from(root, evidence / floor.MEASUREMENT_DIRECTORY)
                    self.assertEqual(run.status, 2, run.stdout + run.stderr)
                    self.assertIn("is incomplete: the run stopped in phase 'publish'", run.stderr)

    def test_run_whose_provenance_writer_cannot_run_still_leaves_an_incomplete_record(self):
        # Once the coverage step starts, even a broken floor tool leaves a record that names where the run stopped.
        completed, root, _calls = self.run_gate("passes", broken_tool=True)
        self.assertEqual(completed.returncode, 5, completed.stdout + completed.stderr)
        evidence = root / floor.EVIDENCE_DIRECTORY
        self.assertEqual([path.name for path in evidence.iterdir()], [floor.PROVENANCE_FILE])
        document = json.loads((evidence / floor.PROVENANCE_FILE).read_text(encoding="utf-8"))
        self.assertEqual((document["schema"], document["measurement"], document["failed_phase"]),
                         (floor.PROVENANCE_SCHEMA, "incomplete", "self-test"))
        self.assertEqual(document["failure"], "exit status 5; no provenance record could be written")


class SessionRunnerTests(unittest.TestCase):
    """The gate harness bounds and reaps the whole process tree it starts, not only its direct child."""

    def test_timeout_kills_the_grandchild_of_a_hung_stand_in(self):
        # A hung stand-in for cargo leaves a grandchild holding the output pipes. subprocess.run's timeout would kill
        # only the gate script and leave both running; the harness kills the group and returns promptly.
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, str(directory), True)
        pid_file, cargo, gate = directory / "grandchild.pid", directory / "cargo", directory / "gate.sh"
        cargo.write_text('#!/bin/sh\nsleep 300 &\necho $! > "$1"\nwait\n', encoding="utf-8")
        cargo.chmod(0o755)
        # A second command keeps bash from replacing itself with the stand-in, so the sleep is a grandchild.
        gate.write_text(f'"{cargo}" "{pid_file}"\necho unreachable\n', encoding="utf-8")
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            run_in_session(["bash", str(gate)], str(directory), clean_env(), 2)
        self.assertLess(time.monotonic() - started, 30)
        grandchild = int(pid_file.read_text(encoding="utf-8"))

        def alive():
            try:
                os.kill(grandchild, 0)
            except ProcessLookupError:
                return False
            return True

        # The killed grandchild is reparented and reaped by its new parent; allow a moment for that.
        deadline = time.monotonic() + 5
        while alive() and time.monotonic() < deadline:
            time.sleep(0.05)
        if alive():
            os.kill(grandchild, signal.SIGKILL)
            self.fail(f"grandchild {grandchild} survived the harness timeout")


class RebaselineProvenanceTests(unittest.TestCase):
    """A floor on the CI host changes only from a verified, complete record of the run that measured it."""

    def refuse(self, paths, *needles, extra=(), target=TARGET):
        """Assert that an update from the retained run is refused, names each needle, and writes nothing."""
        before = paths["baseline"].read_bytes()
        run = update(paths, "--reason", "toolchain moved coverage", "--provenance", str(paths["provenance"]),
                     *extra, target=target)
        self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2, run.stdout + run.stderr)
        for needle in needles:
            self.assertIn(needle, run.stderr)

    def test_ci_host_baseline_change_without_provenance_is_refused(self):
        # A stated runner is no longer enough to change a floor CI enforces.
        with workspace(export(FILES), metadata(MEMBERS), baseline(FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", RUNNER,
                         "--crate", "sonicterm-vt")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("requires --provenance FILE whenever the baseline names a CI host "
                      "(aarch64-apple-darwin on macos14/ARM64)", run.stderr)
        # A first baseline for the CI host needs a record too.
        with workspace(export(FILES), metadata(MEMBERS), None) as paths:
            run = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", RUNNER)
            self.assertFalse(paths["baseline"].exists())
        self.assertEqual(run.status, 2)
        self.assertIn("requires --provenance FILE", run.stderr)

    def test_incomplete_record_is_refused(self):
        # A run that stopped before its measurement completed can never change a floor.
        with retained_run() as paths:
            record(paths["root"], paths["provenance"], "instrumented-tests", "failed", RUN_ENV, 101)
            self.refuse(paths, "is incomplete: the run stopped in phase 'instrumented-tests' (exit status 101)")

    def test_record_missing_a_field_is_refused(self):
        # Every listed field must be present and non-empty; the refusal names the missing one.
        for field_name in ("image_version", "run_id", "rustc_verbose", "tree_entries"):
            with self.subTest(field=field_name):
                with retained_run() as paths:
                    rewrite(paths["provenance"], **{field_name: None})
                    self.refuse(paths, f"is missing required field(s): {field_name}")
        with retained_run(env=dict(RUN_ENV, GITHUB_EVENT_NAME="pull_request")) as paths:
            self.refuse(paths, "is missing required field(s): pull_request_head")

    def test_record_from_outside_ci_is_refused(self):
        # A local run has no run to verify, so its record cannot change a floor.
        with retained_run(env=LOCAL_ENV) as paths:
            self.refuse(paths, "was measured outside CI")

    def test_digest_mismatch_is_refused(self):
        # A report or inventory that differs from the record fails, naming both digests.
        with retained_run() as paths:
            recorded = json.loads(paths["provenance"].read_text(encoding="utf-8"))["report_sha256"]
            paths["report"].write_text(json.dumps(export({**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 166)})),
                                       encoding="utf-8")
            actual = hashlib.sha256(paths["report"].read_bytes()).hexdigest()
            self.refuse(paths, f"has SHA-256 {actual}, but the record's report_sha256 is {recorded}")
        with retained_run() as paths:
            paths["metadata"].write_text(json.dumps(metadata(MEMBERS)), encoding="utf-8")
            self.refuse(paths, "but the record's inventory_sha256 is")

    def test_conflicting_target_or_runner_is_refused(self):
        # The stated host must agree with the record; each conflict is named.
        with retained_run() as paths:
            self.refuse(paths, "--target x86_64-apple-darwin conflicts with the record's target aarch64-apple-darwin",
                        target="x86_64-apple-darwin")
            self.refuse(paths, "--runner macos15/ARM64 conflicts with the record's runner macos14/ARM64",
                        extra=("--runner", "macos15/ARM64"))

    def test_measured_tree_with_other_coverage_relevant_content_is_refused(self):
        # Source, policy, and a new test target each change what is measured, committed or not.
        cases = {"source": ("crates/sonicterm-vt/src/vt.rs", "pub fn vt() { changed(); }\n"),
                 "policy": ("scripts/coverage-floor.py", "# policy changed\n"),
                 "new test target": ("crates/sonicterm-vt/tests/new.rs", "#[test]\nfn new() {}\n")}
        for label, (relative, content) in cases.items():
            with self.subTest(change=label):
                with retained_run() as paths:
                    changed = paths["root"] / relative
                    changed.parent.mkdir(parents=True, exist_ok=True)
                    changed.write_text(content, encoding="utf-8")
                    self.refuse(paths, f"uncommitted coverage-relevant changes in {relative}")
                    commit_files(paths["root"], {})
                    self.refuse(paths, f"differs from this checkout's HEAD in {relative}")

    def test_baseline_or_documentation_only_changes_are_accepted(self):
        # A tree that changes only the baseline or documentation still matches the measured tree.
        with retained_run() as paths:
            commit_files(paths["root"], {
                "README.md": "# SonicTerm, reworded\n", "wiki/Home.md": "# Home, reworded\n", "docs/notes.md": "notes\n",
                "crates/sonicterm-vt/CLAUDE.md": "# vt\n",
                "scripts/coverage-baseline.json": json.dumps(baseline(FLOORS, reason="edited"), indent=2) + "\n"})
            (paths["root"] / "wiki" / "Draft.md").write_text("draft\n", encoding="utf-8")
            run = update(paths, "--reason", "vt parser paths moved", "--provenance", str(paths["provenance"]),
                         "--crate", "sonicterm-vt")
        self.assertEqual(run.status, 0, run.stderr)

    def test_coverage_relevant_paths(self):
        # Only the baseline file and documentation are exempt from the measured-tree comparison.
        relevant = ["crates/sonicterm-vt/src/vt.rs", "crates/sonicterm-vt/Cargo.toml", "Cargo.lock",
                    "rust-toolchain.toml", "scripts/rust-logic-coverage.sh", "scripts/coverage-floor.py",
                    ".github/workflows/ci.yml", "crates/sonicterm-vt/tests/autowrap.rs", "wikipedia.rs"]
        exempt = ["scripts/coverage-baseline.json", "README.md", "CLAUDE.md", "crates/sonicterm-vt/CLAUDE.md",
                  "wiki/Development-and-Release.md", "wiki/logo.png", "docs/specs/plan.txt", "CHANGELOG.MD"]
        for path in relevant:
            with self.subTest(path=path):
                self.assertTrue(floor.coverage_relevant(path))
        for path in exempt:
            with self.subTest(path=path):
                self.assertFalse(floor.coverage_relevant(path))

    def test_crate_update_reproduces_the_retained_floor_exactly(self):
        # With verified provenance, --crate writes the retained run's counts for that crate and nothing else.
        files = {"crates/sonicterm-grid/src/grid.rs": (400, 300), "crates/sonicterm-vt/src/vt.rs": (190, 141),
                 "crates/sonicterm-vt/src/vt/staging.rs": (10, 9)}
        with retained_run(files=files) as paths:
            run = update(paths, "--reason", "vt lost dead parser paths", "--provenance", str(paths["provenance"]),
                         "--crate", "sonicterm-vt")
            stored = floor.load_baseline(str(paths["baseline"]))
            checked = invoke(paths, CI_ENV)
        self.assertEqual(run.status, 0, run.stderr)
        self.assertEqual(stored.crates["sonicterm-vt"], floor.Entry(200, 150))
        self.assertEqual(stored.crates["sonicterm-grid"], floor.Entry(400, 360))
        self.assertIn(ARTIFACT, stored.reason)
        self.assertRegex(row(checked.stdout, "sonicterm-vt"), r"\sok\s")
        self.assertIn("DROP", row(checked.stdout, "sonicterm-grid"))

    def test_full_update_migrates_the_host_and_names_both(self):
        # A new runner image needs every floor re-measured: a full update records the move and both hosts.
        with retained_run(env=dict(RUN_ENV, ImageOS="macos15")) as paths:
            run = update(paths, "--reason", "The runner image moved to macOS 15", "--provenance",
                         str(paths["provenance"]))
            stored = floor.load_baseline(str(paths["baseline"]))
        self.assertEqual(run.status, 0, run.stderr)
        self.assertEqual((stored.target, stored.runner), (TARGET, "macos15/ARM64"))
        self.assertTrue(stored.reason.startswith(
            "Host migration from aarch64-apple-darwin on macos14/ARM64 to aarch64-apple-darwin on macos15/ARM64. "
            "The runner image moved to macOS 15. Measured by"), stored.reason)
        self.assertIn("host: aarch64-apple-darwin on macos14/ARM64 -> aarch64-apple-darwin on macos15/ARM64",
                      run.stdout)
        self.assertEqual(stored.crates, {"sonicterm-grid": floor.Entry(400, 360),
                                         "sonicterm-vt": floor.Entry(200, 174)})

    def test_forged_identity_and_empty_checks_are_refused(self):
        # A path map alone cannot vouch for a tree or commit it never hashed to, and a complete measurement
        # carries both checks.
        with retained_run() as paths:
            rewrite(paths["provenance"], tree="0" * 40, commit="not-a-commit", checks={})
            self.refuse(paths, "the record's commit 'not-a-commit' is not a full lowercase sha1 object id",
                        "not the recorded tree " + "0" * 40, "the record's checks lacks floor, subset-gate")

    def test_path_map_that_does_not_hash_to_the_recorded_tree_is_refused(self):
        # One changed or one dropped entry no longer hashes to the tree, even where the tree comparison is exempt.
        for label in ("changed", "dropped"):
            with self.subTest(mutation=label):
                with retained_run() as paths:
                    document = json.loads(paths["provenance"].read_text(encoding="utf-8"))
                    entries = dict(document["tree_entries"])
                    if label == "changed":
                        mode, object_id = entries["README.md"].split(" ")
                        entries["README.md"] = f"{mode} {'f' * len(object_id)}"
                    else:
                        del entries["README.md"]
                    rewrite(paths["provenance"], tree_entries=entries)
                    self.refuse(paths, "the record's tree_entries hash to tree",
                                f"not the recorded tree {document['tree']}")

    def test_object_ids_must_be_full_lowercase_ids_in_the_checkout_format(self):
        # Abbreviated, uppercase, or wrong-length IDs are refused by name; this fixture checkout uses SHA-1.
        mutations = {"abbreviated commit": ("commit", lambda value: value[:12]),
                     "uppercase commit": ("commit", str.upper),
                     "sha256-length tree": ("tree", lambda value: (value * 2)[:64])}
        for label, (field_name, mutate) in mutations.items():
            with self.subTest(case=label):
                with retained_run() as paths:
                    value = mutate(json.loads(paths["provenance"].read_text(encoding="utf-8"))[field_name])
                    rewrite(paths["provenance"], **{field_name: value})
                    self.refuse(paths, f"the record's {field_name} {value!r} is not a full lowercase sha1 object id")
        with retained_run(env=dict(RUN_ENV, GITHUB_EVENT_NAME="pull_request",
                                   COVERAGE_PULL_REQUEST_HEAD="c209d05")) as paths:
            self.refuse(paths, "the record's pull_request_head 'c209d05' is not a full lowercase sha1 object id")

    def test_incomplete_or_impossible_checks_are_refused(self):
        # A complete measurement carries exactly both checks, each with a status in a pair the gate script writes.
        cases = {
            "missing floor": ({"subset-gate": "exit status 0"}, "the record's checks lacks floor"),
            "unknown status": ({"subset-gate": "passed", "floor": "not run"}, "which the gate script never writes"),
            "passed gate, floor never run": ({"subset-gate": "exit status 0", "floor": "not run"},
                                             "which the gate script never writes"),
            "failed gate, floor ran": ({"subset-gate": "exit status 1", "floor": "exit status 0"},
                                       "which the gate script never writes"),
            "negative status": ({"subset-gate": "exit status -1", "floor": "not run"},
                                "which the gate script never writes"),
            "extra entry": ({"subset-gate": "exit status 0", "floor": "exit status 0", "doctests": "exit status 0"},
                            "has unknown entries doctests"),
        }
        for label, (checks, needle) in cases.items():
            with self.subTest(case=label):
                with retained_run() as paths:
                    rewrite(paths["provenance"], checks=checks)
                    self.refuse(paths, needle)

    def test_record_naming_checkout_drift_is_refused(self):
        # A complete record never names drift, so one that does is refused even if everything else verifies.
        with retained_run() as paths:
            rewrite(paths["provenance"], checkout_drift=["HEAD moved from a to b"])
            self.refuse(paths, "the record names checkout drift: HEAD moved from a to b")

    def test_ci_host_baseline_cannot_move_to_local_without_provenance(self):
        # Declaring the host `local` does not sidestep provenance: the existing baseline names the CI host.
        with workspace(export(FILES), metadata(MEMBERS), baseline(FLOORS)) as paths:
            before = paths["baseline"].read_bytes()
            run = invoke(paths, LOCAL_ENV, "--update-baseline", "--reason", "floors", "--runner", "local")
            self.assertEqual(paths["baseline"].read_bytes(), before)
        self.assertEqual(run.status, 2)
        self.assertIn("requires --provenance FILE whenever the baseline names a CI host "
                      "(aarch64-apple-darwin on macos14/ARM64)", run.stderr)


class DropGuidanceTests(unittest.TestCase):
    """A DROP still fails without a proposal of its own and points at its evidence and the procedure."""

    def test_drop_names_this_runs_artifact_and_the_procedure(self):
        # The message names the run's artifact and the documented procedure, and prints no proposal.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 162)}
        run = check(files, MEMBERS, baseline(FLOORS), RUN_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn(f"The evidence for this run is the artifact {ARTIFACT} of "
                      "https://github.com/D0n9X1n/SonicTerm/actions/runs/36087392875.", run.stdout)
        self.assertIn(floor.REBASELINE_PROCEDURE, run.stdout)
        self.assertIn("--update-baseline --provenance", run.stdout)
        self.assertNotIn(floor.PROPOSAL_BEGIN, run.stdout)

    def test_drop_beside_a_missing_floor_keeps_the_dropped_floor_in_the_preview(self):
        # The additions-only preview never lowers the dropped floor, and the DROP guidance still prints.
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 162), "crates/sonicterm-new/src/lib.rs": (10, 8)}
        run = check(files, MEMBERS + ["sonicterm-new"], baseline(FLOORS), RUN_ENV)
        self.assertEqual(run.status, 1, run.stdout)
        self.assertIn(f"the artifact {ARTIFACT}", run.stdout)
        self.assertEqual(proposal(run.stdout)["crates"]["sonicterm-vt"], baseline(FLOORS)["crates"]["sonicterm-vt"])
        self.assertIn("a preview only", run.stdout)


class CoverageWorkflowTests(unittest.TestCase):
    """The macos-coverage job uploads its evidence after success and after failure, inside its deadline."""

    def test_coverage_job_uploads_its_evidence_within_the_job_deadline(self):
        # One pinned upload runs unless the run was cancelled or never reached the gate, with a bounded budget.
        job = ci_job("macos-coverage")
        gate = ci_step(job, "Run Rust logic coverage gate")
        upload = ci_step(job, "Upload coverage evidence")
        self.assertIn("id: coverage", gate)
        self.assertIn("run: scripts/rust-logic-coverage.sh", gate)
        # Run metadata reaches the script through env, never as an expression in the command.
        self.assertIn("COVERAGE_PULL_REQUEST_HEAD: ${{ github.event.pull_request.head.sha }}", gate)
        self.assertNotIn("${{", re.search(r"(?m)^        run: (.*)$", gate).group(1))
        self.assertIn("if: ${{ !cancelled() && steps.coverage.conclusion != 'skipped' }}", upload)
        self.assertIn("uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1", upload)
        self.assertIn(f"name: {floor.EVIDENCE_ARTIFACT_PREFIX}-${{{{ github.run_id }}}}-${{{{ github.run_attempt }}}}",
                      upload)
        self.assertIn(f"path: {floor.EVIDENCE_DIRECTORY}\n", upload)
        self.assertIn("retention-days: 90", upload)
        self.assertIn("if-no-files-found: error", upload)
        self.assertLess(job.index("- name: Run Rust logic coverage gate"), job.index("- name: Upload coverage evidence"))

        def minutes(block):
            return int(re.search(r"(?m)^        timeout-minutes: (\d+)$", block).group(1))

        job_minutes = int(re.search(r"(?m)^    timeout-minutes: (\d+)$", job).group(1))
        # The gate's deadline plus the upload's leaves setup ten minutes; it took under two in recent runs.
        self.assertLessEqual(minutes(gate) + minutes(upload) + 10, job_minutes)

    def test_gate_script_writes_the_evidence_the_workflow_uploads(self):
        # The script's evidence directory and file names are the ones the upload and the floor expect.
        script = (_HERE / "rust-logic-coverage.sh").read_text(encoding="utf-8")
        self.assertEqual(floor.EVIDENCE_DIRECTORY, "target/rust-logic-coverage-evidence")
        self.assertEqual(floor.WORK_DIRECTORY, "target/rust-logic-coverage-work")
        for line in ('EVIDENCE_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage-evidence"',
                     'WORK_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage-work"',
                     f'STAGE_DIR="${{WORK_DIR}}/{floor.STAGING_DIRECTORY}"',
                     f'MEASUREMENT_DIR="${{EVIDENCE_DIR}}/{floor.MEASUREMENT_DIRECTORY}"',
                     f'REPORT="${{MEASUREMENT_DIR}}/{floor.REPORT_FILE}"',
                     f'INVENTORY="${{MEASUREMENT_DIR}}/{floor.INVENTORY_FILE}"',
                     f'RECORD="${{EVIDENCE_DIR}}/{floor.PROVENANCE_FILE}"', f'PIN="${{WORK_DIR}}/{floor.PIN_FILE}"'):
            self.assertIn(line, script)
        # The fallback record the exit trap writes uses the schema the checker reads.
        self.assertIn(floor.PROVENANCE_SCHEMA, script)


class GitTreeIdTests(unittest.TestCase):
    """The offline checker rebuilds the Git tree a record's path map names, exactly as Git hashes it."""

    def test_head_tree_is_recomputed_from_ls_tree(self):
        # Executables, symlinks, gitlinks, nested directories, names whose order depends on Git's directory
        # rule, and non-UTF-8 names all rehash to HEAD's tree, in SHA-1 and, where Git supports it, SHA-256.
        for form in ("sha1", "sha256"):
            with self.subTest(object_format=form):
                directory = Path(tempfile.mkdtemp())
                self.addCleanup(shutil.rmtree, str(directory), True)
                made = subprocess.run(["git", "init", "-q", f"--object-format={form}", str(directory)],
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=60)
                if made.returncode != 0:
                    self.assertEqual(form, "sha256", made.stderr)
                    self.skipTest("this Git cannot create a SHA-256 repository")

                def blob(content):
                    return subprocess.run(["git", "-C", str(directory), "hash-object", "-w", "--stdin"],
                                          input=content, stdout=subprocess.PIPE, check=True,
                                          timeout=60).stdout.decode().strip()

                gitlink = "1" * floor.OBJECT_ID_LENGTHS[form]
                entries = [("100644", blob(b"plain\n"), b"a.txt"), ("100755", blob(b"#!/bin/sh\n"), b"bin/run.sh"),
                           ("120000", blob(b"a.txt"), b"link"), ("160000", gitlink, b"vendor/sub"),
                           ("100644", blob(b"x"), b"a-b"), ("100644", blob(b"y"), b"a/inner.rs"),
                           ("100644", blob(b"z"), b"a0"), ("100644", blob(b"deep"), b"a/b/c/deep.rs"),
                           ("100644", blob(b"u"), "café.md".encode()), ("100644", blob(b"r"), b"raw\xff.bin")]
                for mode, object_id, name in entries:
                    subprocess.run([b"git", b"-C", str(directory).encode(), b"update-index", b"--add", b"--cacheinfo",
                                    f"{mode},{object_id},".encode() + name], check=True, timeout=60)
                git(directory, "commit", "-q", "-m", "tree fixture")
                head_tree = git(directory, "rev-parse", "HEAD^{tree}").stdout.strip()
                listed = floor.tree_entries(str(directory))
                self.assertEqual(len(listed), len(entries))
                self.assertEqual(floor.git_tree_id(listed, form), head_tree)

    def test_a_path_map_git_could_not_write_is_refused(self):
        # An unknown mode, a short object id, or a file with a path beneath it cannot hash to any Git tree.
        good = "100644 " + "a" * 40
        for label, entries in {"unknown mode": {"a.txt": "100664 " + "a" * 40},
                               "short object id": {"a.txt": "100644 abc"},
                               "path beneath a file": {"a": good, "a/b": good}}.items():
            with self.subTest(case=label):
                with self.assertRaises(floor.InputError):
                    floor.git_tree_id(entries, "sha1")


class ClassificationTests(unittest.TestCase):
    """Report files are attributed consistently and every exclusion is counted."""

    def test_paths_are_normalized_before_classification(self):
        # llvm-cov reports `src/../build_config.rs` for a `#[path]` module.
        files = {
            "crates/sonicterm-freetype/src/../build_config.rs": (58, 56),
            "crates/sonicterm-freetype/build.rs": (40, 0),
            f"{ROOT}/crates\\sonicterm-vt\\src\\vt.rs": (200, 174),
        }
        base = baseline({"sonicterm-freetype": (58, 56), "sonicterm-vt": (200, 174)})
        run = check(files, ["sonicterm-freetype", "sonicterm-vt"], base, CI_ENV, build_scripts=("sonicterm-freetype",))
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(row(run.stdout, "sonicterm-freetype"), r"\s58\s+56\s")
        self.assertRegex(run.stdout, r"build-script\s+files=1\s+lines=40\s")

    def test_exclusions_are_reported_with_counts_and_reasons(self):
        files = {
            **FILES,
            "crates/sonicterm-vt/src/vt_tests.rs": (500, 500),
            "crates/sonicterm-vt/tests/autowrap/main.rs": (40, 40),
            "crates/sonicterm-vt/benches/parse.rs": (7, 7),
            "crates/sonicterm-vt/examples/demo.rs": (5, 5),
            "crates/sonicterm-winit/src/platform_impl/macos/mod.rs": (100, 50),
            "crates/sonicterm-harfbuzz/harfbuzz/src/rust/lib.rs": (20, 0),
            "target/rust-logic-coverage/llvm-cov-target/debug/build/x-1/out/bindings.rs": (9, 9),
            "/Users/runner/.cargo/registry/src/index/foo-1.0.0/src/lib.rs": (11, 1),
            "crates/sonicterm-vt/build.rs": (3, 0),
            "scripts/freetype-config.rs": (4, 4),
        }
        base = baseline(FLOORS, {"sonicterm-harfbuzz": "declarations only"})
        run = check(files, MEMBERS + ["sonicterm-harfbuzz"], base, CI_ENV, build_scripts=("sonicterm-vt",))
        self.assertEqual(run.status, 0, run.stdout)
        # Only vt's two src files count toward its row.
        self.assertRegex(row(run.stdout, "sonicterm-vt"), r"\s200\s+174\s+87\.00%")
        expected = {"test": (4, 552), "vendored": (2, 120), "generated": (1, 9),
                    "outside-repository": (1, 11), "build-script": (1, 3), "outside-members": (1, 4)}
        for category, (count, lines) in expected.items():
            with self.subTest(category=category):
                self.assertRegex(run.stdout, rf"(?m)^  {re.escape(category)}\s+files={count}\s+lines={lines}\s+\S")
        self.assertIn("      scripts/freetype-config.rs", run.stdout)
        self.assertIn("      crates/sonicterm-vt/build.rs", run.stdout)

    def test_committed_generated_bindings_count_under_generated(self):
        # Tracked rust-bindgen output is excluded like build output; FreeType keeps only authored code.
        files = {
            "crates/sonicterm-freetype/src/lib.rs": (22, 0),
            "crates/sonicterm-freetype/src/types.rs": (5, 0),
            "crates/sonicterm-freetype/src/fixed_point.rs": (18, 18),
            "crates/sonicterm-freetype/src/../build_config.rs": (58, 56),
            "crates/sonicterm-harfbuzz/src/lib.rs": (9, 0),
        }
        members = ["sonicterm-freetype", "sonicterm-harfbuzz"]
        base = baseline({"sonicterm-freetype": (76, 74)}, {"sonicterm-harfbuzz": "generated only"})
        run = check(files, members, base, CI_ENV)
        self.assertEqual(run.status, 0, run.stdout)
        self.assertRegex(row(run.stdout, "sonicterm-freetype"), r"\s76\s+74\s+97\.37%")
        self.assertRegex(run.stdout, r"(?m)^  generated\s+files=3\s+lines=36\s")
        for path in ("crates/sonicterm-freetype/src/lib.rs", "crates/sonicterm-freetype/src/types.rs",
                     "crates/sonicterm-harfbuzz/src/lib.rs"):
            self.assertIn("      " + path, run.stdout)
        self.assertNotIn("%", row(run.stdout, "sonicterm-harfbuzz"))
        undeclared = check(files, members, baseline({"sonicterm-freetype": (76, 74)}), CI_ENV)
        self.assertIn("every reported file is excluded (generated: 1)",
                      row(undeclared.stdout, "sonicterm-harfbuzz"))

    def test_change_confined_to_generated_bindings_does_not_move_the_crate(self):
        # Counting these generated lines would read two added uncovered lines as a 1.51 pp drop (74/100 against 74/98).
        authored = {"crates/sonicterm-freetype/src/fixed_point.rs": (18, 18),
                    "crates/sonicterm-freetype/src/../build_config.rs": (58, 56)}
        base = baseline({"sonicterm-freetype": (76, 74)})
        for generated_lines in (22, 24):
            with self.subTest(generated_lines=generated_lines):
                files = {**authored, "crates/sonicterm-freetype/src/lib.rs": (generated_lines, 0)}
                run = check(files, ["sonicterm-freetype"], base, CI_ENV)
                self.assertEqual(run.status, 0, run.stdout)
                self.assertRegex(row(run.stdout, "sonicterm-freetype"),
                                 r"\s76\s+74\s+97\.37%\s+97\.37%\s+\+0\.00")
                self.assertNotIn("DROP", run.stdout)
        # Counted as authored code, the same probe would cross the tolerance: 74/98 against 74/100.
        self.assertGreater(74 * 100 / 98 - 74 * 100 / 100, 1.0)

    def test_source_root_maps_a_report_from_another_checkout(self):
        other = "/private/tmp/sonicterm-fixture-other-checkout"
        with workspace(export(FILES, root=other), metadata(MEMBERS), baseline(FLOORS)) as paths:
            unmapped = invoke(paths, CI_ENV)
            mapped = invoke(paths, CI_ENV, "--source-root", other)
        # Unmapped, every file is outside the repository and both floors go missing.
        self.assertEqual(unmapped.status, 1, unmapped.stdout)
        self.assertRegex(unmapped.stdout, r"outside-repository\s+files=3\s")
        self.assertIn("MISSING", row(unmapped.stdout, "sonicterm-vt"))
        self.assertEqual(mapped.status, 0, mapped.stdout)

    def test_platform_crates_are_labelled_as_compiled_on_the_host(self):
        # Single-platform crates measured on another OS are labelled, never shown as native coverage.
        files = {"crates/sonicterm-mac/src/main.rs": (100, 10), "crates/sonicterm-windows/src/main.rs": (100, 20),
                 "crates/sonicterm-linux/src/main.rs": (70, 66)}
        members = ["sonicterm-linux", "sonicterm-mac", "sonicterm-windows"]
        base = baseline({"sonicterm-mac": (100, 10), "sonicterm-windows": (100, 20), "sonicterm-linux": (70, 66)})
        on_mac = check(files, members, base, CI_ENV)
        self.assertEqual(on_mac.status, 0, on_mac.stdout)
        self.assertIn("compiled on macOS; not Windows execution coverage", row(on_mac.stdout, "sonicterm-windows"))
        self.assertIn("compiled on macOS; not Linux execution coverage", row(on_mac.stdout, "sonicterm-linux"))
        self.assertNotIn("compiled on", row(on_mac.stdout, "sonicterm-mac"))
        on_linux = check(files, members, base, LOCAL_ENV, target="x86_64-unknown-linux-gnu")
        self.assertIn("compiled on Linux; not macOS execution coverage", row(on_linux.stdout, "sonicterm-mac"))
        self.assertNotIn("compiled on", row(on_linux.stdout, "sonicterm-linux"))

    def test_one_file_reported_twice_is_refused(self):
        # Two spellings of one file would double its lines.
        files = {**FILES, "crates/sonicterm-vt/src/vt/../vt.rs": (190, 165)}
        run = check(files, MEMBERS, baseline(FLOORS), CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("lists crates/sonicterm-vt/src/vt.rs twice", run.stderr)


class InputValidationTests(unittest.TestCase):
    """Broken inputs are errors, never an empty or passing comparison."""

    def test_empty_report_is_refused(self):
        # An empty export is a broken run; comparing it would mark every crate missing.
        with workspace(export({}), metadata(MEMBERS), baseline(FLOORS)) as paths:
            run = invoke(paths, CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("lists no files", run.stderr)

    def test_document_that_is_not_an_llvm_export_is_refused(self):
        # Only an llvm-cov JSON export is accepted as a report.
        with workspace(dict(export(FILES), type="something.else"), metadata(MEMBERS), baseline(FLOORS)) as paths:
            run = invoke(paths, CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("is not an llvm.coverage.json.export document", run.stderr)

    def test_malformed_baselines_are_refused(self):
        # Each schema rule rejects its own violation, so a hand edit cannot weaken the floor.
        good = baseline(FLOORS, {"sonicterm-fontconfig": "linked only on non-macOS Unix"})
        mutations = {
            "schema": lambda doc: doc.update(schema="other/1"),
            "percent": lambda doc: doc["crates"]["sonicterm-vt"].update(percent=50.0),
            "overlap": lambda doc: doc["not_measured"].update({"sonicterm-vt": "both"}),
            "empty declaration": lambda doc: doc["not_measured"].update({"sonicterm-fontconfig": " "}),
            "unknown key": lambda doc: doc.update(floor_pp=1.0),
            "zero tolerance": lambda doc: doc.update(tolerance_pp=0),
            "empty reason": lambda doc: doc.update(reason=""),
            "empty runner": lambda doc: doc["host"].update(runner=""),
            "zero lines": lambda doc: doc["crates"]["sonicterm-vt"].update(lines=0),
        }
        for label, mutate in mutations.items():
            with self.subTest(mutation=label):
                document = json.loads(json.dumps(good))
                mutate(document)
                run = check(FILES, MEMBERS + ["sonicterm-fontconfig"], document, CI_ENV)
                self.assertEqual(run.status, 2, run.stdout)
                self.assertIn("is invalid", run.stderr)

    def test_non_finite_baseline_numbers_are_refused(self):
        # NaN compares false and 1e309 overflows to inf; each must exit 2 instead of passing or crashing.
        text = json.dumps(baseline(FLOORS), indent=2)
        self.assertEqual(text.count('"tolerance_pp": 1.0'), 1)
        self.assertEqual(text.count('"percent": 87.0'), 1)
        cases = {
            "tolerance NaN": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": NaN'),
            "tolerance Infinity": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": Infinity'),
            "tolerance -Infinity": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": -Infinity'),
            "tolerance 1e309": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": 1e309'),
            "tolerance 1 followed by 400 zeros": text.replace('"tolerance_pp": 1.0', '"tolerance_pp": 1' + "0" * 400),
            "percent NaN": text.replace('"percent": 87.0', '"percent": NaN'),
            "percent 1e309": text.replace('"percent": 87.0', '"percent": 1e309'),
        }
        for label, document in cases.items():
            with self.subTest(case=label):
                with workspace(export(FILES), metadata(MEMBERS), document) as paths:
                    run = invoke(paths, CI_ENV)
                self.assertEqual(run.status, 2, run.stdout)
                self.assertIn("finite", run.stderr)
                self.assertNotIn("PASS", run.stdout)
        # Documents that bypass the JSON reader, such as proposals, are held to the same rule.
        mutations = {
            "tolerance nan": lambda doc: doc.update(tolerance_pp=float("nan")),
            "tolerance inf": lambda doc: doc.update(tolerance_pp=float("inf")),
            "percent nan": lambda doc: doc["crates"]["sonicterm-vt"].update(percent=float("nan")),
        }
        for label, mutate in mutations.items():
            with self.subTest(case=label):
                document = baseline(FLOORS)
                mutate(document)
                with self.assertRaisesRegex(floor.InputError, "finite"):
                    floor.parse_baseline(document, "in-memory")

    def test_missing_input_file_is_an_input_error(self):
        # A missing input exits 2 instead of comparing against nothing.
        with workspace(export(FILES), metadata(MEMBERS), None) as paths:
            run = invoke(paths, CI_ENV)
        self.assertEqual(run.status, 2)
        self.assertIn("cannot read baseline", run.stderr)


class RepositoryContractTests(unittest.TestCase):
    """The committed baseline and gate script agree with the repository."""

    def test_committed_baseline_is_valid_and_keyed_to_the_macos_arm64_ci_runner(self):
        # The committed floor loads and names the host CI enforces it on.
        committed = floor.load_baseline(str(_HERE / "coverage-baseline.json"))
        self.assertEqual((committed.target, committed.runner), ("aarch64-apple-darwin", "macos14/ARM64"))
        self.assertEqual(committed.tolerance_pp, 1.0)

    def test_baseline_names_are_workspace_packages(self):
        # A typo fails here in seconds; the full inventory check needs the instrumented run.
        manifest = (_REPO / "Cargo.toml").read_text(encoding="utf-8")
        table = re.search(r"(?ms)^\[workspace\]\n(.*?)^\[", manifest).group(1)
        members = re.findall(r'"([^"]+)"', re.search(r"(?ms)^members\s*=\s*\[(.*?)\]", table).group(1))
        names = set()
        for member in members:
            text = (_REPO / member / "Cargo.toml").read_text(encoding="utf-8")
            package = re.search(r"(?ms)^\[package\]\n(.*?)(?=^\[|\Z)", text).group(1)
            names.add(re.search(r'(?m)^name\s*=\s*"([^"]+)"', package).group(1))
        self.assertIn("sonicterm-vt", names)
        committed = floor.load_baseline(str(_HERE / "coverage-baseline.json"))
        self.assertLessEqual(set(committed.crates) | set(committed.not_measured), names)

    def test_generated_sources_equal_the_marked_first_party_files(self):
        # A newly committed generated file cannot be counted as authored code without failing here.
        sources = first_party_rust_sources()
        self.assertGreater(len(sources), 100)
        marked = {path for path in sources if has_generator_marker(path)}
        for path in floor.GENERATED_SOURCES:
            with self.subTest(path=path):
                self.assertTrue((_REPO / path).is_file())
        # A listed file without a marker must be pinned here with the reason it is generated.
        unmarked_generated = {}
        self.assertLessEqual(set(unmarked_generated), set(floor.GENERATED_SOURCES))
        self.assertEqual(set(floor.GENERATED_SOURCES) - set(unmarked_generated), marked)
        # Pinned authored: hand-written FFI declarations stay counted even though they resemble bindings.
        hand_written = {
            "crates/sonicterm-fontconfig/src/lib.rs":
                "Servo's hand-written Fontconfig declarations; the file opens with the Servo copyright header",
        }
        for path, reason in hand_written.items():
            with self.subTest(path=path, reason=reason):
                self.assertNotIn(path, floor.GENERATED_SOURCES)
                self.assertNotIn(path, marked)
                first = (_REPO / path).read_text(encoding="utf-8").splitlines()[0]
                self.assertEqual(first, "// Copyright 2013 The Servo Project Developers. See the COPYRIGHT")

    def test_vendored_roots_match_the_native_dependency_manifest(self):
        # Vendored exclusions follow the pinned import list instead of a second copy.
        manifest = json.loads((_HERE / "native-dependencies.json").read_text(encoding="utf-8"))
        self.assertEqual(sorted(floor.VENDORED_ROOTS), sorted(library["path"] for library in manifest["libraries"]))

    def test_gate_measures_vt_keeps_the_subset_floor_and_runs_the_crate_floor(self):
        script = (_HERE / "rust-logic-coverage.sh").read_text(encoding="utf-8")
        regex = re.search(r"(?m)^IGNORE_REGEX='(.*)'$", script).group(1)
        prefix = "(sonicterm-("
        crates = regex[regex.index(prefix) + len(prefix):].split(")", 1)[0].split("|")
        self.assertNotIn("vt", crates)
        self.assertNotIn("block-glyph", crates)
        # CLAUDE.md and both Development-and-Release pages state this count.
        self.assertEqual(len(crates), 9)
        self.assertEqual(script.count("--fail-under-lines 80"), 1)
        # The report and inventory are published before the 80% subset gate or the floor can fail the run.
        markers = ["pin-checkout", "record self-test running", "coverage-floor_tests.py", "begin instrumented-tests",
                   "cargo llvm-cov --workspace --lib --bins --tests --no-report", "begin report",
                   "cargo llvm-cov report --json --summary-only", "begin inventory", "cargo metadata --no-deps",
                   "begin publish", "os.rename", "begin subset-gate", "--fail-under-lines 80", "begin floor",
                   '--baseline "$ROOT/scripts/coverage-baseline.json"', 'record floor "done"']
        positions = [script.index(marker) for marker in markers]
        self.assertEqual(positions, sorted(positions))


class CliTests(unittest.TestCase):
    """The script's exit status holds when run as a real process."""

    def test_exit_status_from_a_real_process(self):
        # Exit 1 fails CI, 0 is informational, 2 is misuse; the gate step relies on these codes.
        env = clean_env()
        files = {**FILES, "crates/sonicterm-vt/src/vt.rs": (190, 100)}
        with workspace(export(files), metadata(MEMBERS), baseline(FLOORS)) as paths:
            argv = [sys.executable, str(_FLOOR_PATH), "--report", str(paths["report"]),
                    "--metadata", str(paths["metadata"]), "--baseline", str(paths["baseline"]), "--target", TARGET]

            def run(arguments, environment):
                return subprocess.run(arguments, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      text=True, encoding="utf-8", check=False, timeout=60)

            failing = run(argv, {**env, **CI_ENV})
            local = run(argv, env)
            incomplete = run(argv[:4], env)
        self.assertEqual(failing.returncode, 1, failing.stdout + failing.stderr)
        self.assertEqual(local.returncode, 0, local.stdout + local.stderr)
        self.assertEqual(incomplete.returncode, 2)
        self.assertIn("required", incomplete.stderr)

    def test_overflowing_baseline_number_exits_2_without_a_traceback(self):
        # 1e309 overflows to inf; the reader must refuse it with exit 2, not raise an uncaught ValueError.
        document = json.dumps(baseline(FLOORS), indent=2).replace(
            '"tolerance_pp": 1.0', '"tolerance_pp": 1e309')
        with workspace(export(FILES), metadata(MEMBERS), document) as paths:
            completed = subprocess.run(
                [sys.executable, str(_FLOOR_PATH), "--report", str(paths["report"]), "--metadata",
                 str(paths["metadata"]), "--baseline", str(paths["baseline"]), "--target", TARGET],
                env={**clean_env(), **CI_ENV}, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True, encoding="utf-8", check=False, timeout=60)
        self.assertEqual(completed.returncode, 2, completed.stdout + completed.stderr)
        self.assertNotIn("Traceback", completed.stderr)
        self.assertIn("non-finite number 1e309 is not allowed", completed.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
