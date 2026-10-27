"""Coverage test fixture."""

from __future__ import annotations
from contextlib import contextmanager
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile


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


def export(files, root=ROOT, version="0.9.0"):
    """Build an llvm-cov JSON summary export from {path: (lines, covered)}, written by cargo-llvm-cov `version`."""
    return {
        "type": floor.LLVM_EXPORT_TYPE,
        "version": "3.1.0",
        "cargo_llvm_cov": {"version": version, "manifest_path": f"{root}/Cargo.toml"},
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
# The coverage job's environment; the runner sets every variable here. The host values are fixtures.
RUN_ENV = {
    **CI_ENV,
    "ImageVersion": "20260915.0150",
    "GITHUB_REPOSITORY": "D0n9X1n/SonicTerm",
    "GITHUB_WORKFLOW_REF": "D0n9X1n/SonicTerm/.github/workflows/ci.yml@refs/heads/main",
    "GITHUB_RUN_ID": "36087392875",
    "GITHUB_RUN_ATTEMPT": "1",
    "GITHUB_JOB": "linux-coverage",
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
    # The release every fixture record's cargo-llvm-cov must match.
    "scripts/pinned-tools.json": json.dumps({"cargo-llvm-cov": {"version": "0.9.0",
                                                               "version_output": "cargo-llvm-cov {version}"}}) + "\n",
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
def retained_run(files=FILES, members=MEMBERS, base=None, env=RUN_ENV, phase="floor", state="failed", exit_status=1,
                 version="0.9.0"):
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
        paths["report"].write_text(json.dumps(export(files, version=version), indent=2) + "\n", encoding="utf-8")
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
sys.path.insert(0, os.path.dirname(os.path.abspath(sys.argv[0])))
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
