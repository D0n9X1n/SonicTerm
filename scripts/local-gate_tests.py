#!/usr/bin/env python3
"""Runner, parity, and gate-script tests for scripts/local-gate.py.

The runner tests inject failing, hanging, and non-launchable steps into the real
runner and require every later step to run, the hang to die with its process
tree at its deadline, and a dirty tree to be reported and left unchanged. The
parity tests tie the step table to ci.yml, CLAUDE.md, and both
Development-and-Release wiki files; each also proves that a one-sided edit
fails it, so a parser that silently finds nothing cannot pass.

SONICTERM_GATE_ROOT selects the repository these tests read; it defaults to
this script's repository.
"""

from __future__ import annotations

import dataclasses
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock

ROOT = Path(
    os.environ.get("SONICTERM_GATE_ROOT") or Path(__file__).resolve().parent.parent
).resolve()
_SPEC = importlib.util.spec_from_file_location("local_gate", ROOT / "scripts" / "local-gate.py")
assert _SPEC is not None and _SPEC.loader is not None
gate = importlib.util.module_from_spec(_SPEC)
# Registered before execution: dataclasses resolve string annotations through sys.modules.
sys.modules[_SPEC.name] = gate
_SPEC.loader.exec_module(gate)

WORKFLOW = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")

# Fake tools are extensionless /bin/sh scripts found through PATH, which only a
# POSIX host resolves reliably; macOS and Ubuntu CI run these tests.
POSIX_ONLY = unittest.skipIf(os.name == "nt", "fake shell tools need a POSIX host")


def python_step(step_id: str, code: str, timeout_s: int = 30, **fields):
    """Return an injected step that runs Python code under the real runner."""
    values = dict(
        argv=(sys.executable, "-c", code),
        hosts=gate.HOSTS,
        timeout_s=timeout_s,
        evidence="local",
        prerequisites=("rust",),
        ci_jobs=(),
    )
    values.update(fields)
    return gate.Step(step_id, **values)


def git(root: Path, *args: str) -> subprocess.CompletedProcess[bytes]:
    """Run git in a fixture repository and fail the test on a git error."""
    return subprocess.run(
        ["git", "-C", str(root), *args],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def fixture_repository(directory: Path) -> Path:
    """Create a repository with one committed file, isolated from user hooks and signing."""
    root = directory / "repository"
    root.mkdir()
    git(root, "init", "-q")
    git(root, "config", "core.autocrlf", "false")
    (root / "tracked.txt").write_bytes(b"committed\n")
    git(root, "add", "tracked.txt")
    git(
        root,
        "-c", "user.name=fixture",
        "-c", "user.email=fixture@example.invalid",
        "-c", "commit.gpgsign=false",
        "-c", f"core.hooksPath={directory / 'no-hooks'}",
        "commit", "-q", "--no-verify", "-m", "fixture",
    )
    return root


def tree_bytes(root: Path) -> dict[str, bytes]:
    """Capture every working-tree file outside .git, so a test can prove nothing changed."""
    files = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if relative.parts[0] == ".git" or not path.is_file():
            continue
        files[relative.as_posix()] = path.read_bytes()
    return files


def run_quietly(steps, root: Path, log_dir: Path):
    """Run the real gate with its console captured, returning the report and the console text."""
    console = io.StringIO()
    report = gate.run_gate(steps, root, log_dir, "test", console=console)
    return report, console.getvalue()


def marker_code(target: Path) -> str:
    """Return Python code that proves a step ran by writing a marker file."""
    return f"import pathlib; pathlib.Path({str(target)!r}).write_text('ran')"


class RunnerTests(unittest.TestCase):
    """The runner keeps going after every kind of step failure and bounds every step."""

    def test_failure_hang_and_launch_error_each_let_later_steps_run(self):
        # Protect run-to-the-end: no step outcome stops the steps after it, and a hang
        # dies with its whole process tree at its own deadline.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            survivor = base / "grandchild-survived"
            grandchild = (
                "import pathlib, time; time.sleep(3); "
                f"pathlib.Path({str(survivor)!r}).write_text('alive')"
            )
            hang = (
                "import subprocess, sys, time; "
                f"subprocess.Popen([sys.executable, '-c', {grandchild!r}]); "
                "print('hanging', flush=True); time.sleep(60)"
            )
            steps = [
                python_step("fails", "import sys; print('failing', flush=True); sys.exit(3)"),
                python_step("after-failure", marker_code(base / "after-failure")),
                python_step("hangs", hang, timeout_s=1),
                python_step("after-timeout", marker_code(base / "after-timeout")),
                python_step("missing", "", argv=("sonicterm-no-such-program-for-gate-tests",)),
                python_step("after-launch-error", marker_code(base / "after-launch-error")),
            ]
            started = time.monotonic()
            report, console = run_quietly(steps, root, base / "logs")
            elapsed = time.monotonic() - started
            # Outlast the grandchild's own sleep, so a survivor would have written its marker.
            time.sleep(3.5)

            self.assertEqual(
                [result.status for result in report.results],
                [gate.FAIL, gate.PASS, gate.TIMEOUT, gate.PASS, gate.LAUNCH, gate.PASS],
            )
            self.assertEqual(report.results[0].exit_code, 3)
            for name in ("after-failure", "after-timeout", "after-launch-error"):
                self.assertTrue((base / name).is_file(), f"{name} did not run")
            hung = report.results[2]
            self.assertGreaterEqual(hung.elapsed_s, 1)
            self.assertLess(hung.elapsed_s, 20)
            self.assertLess(elapsed, 60)
            self.assertIn("deadline of 1s reached; process tree killed", hung.detail)
            self.assertIn("hanging", hung.log_path.read_text(encoding="utf-8"))
            self.assertFalse(survivor.exists(), "the hung step's grandchild outlived its deadline")
            self.assertIn("cannot find", report.results[4].detail)
            self.assertEqual(report.exit_code, 1)
            # A non-passing step's log tail reaches the console.
            self.assertIn("    | failing", console)

    @POSIX_ONLY
    def test_a_program_the_kernel_refuses_is_a_launch_error(self):
        # Protect the Popen failure path: an exec error is a verdict, never an exception.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            program = base / "not-a-program"
            program.write_bytes(b"\x00\x01\x02 not an executable format\n")
            program.chmod(0o755)
            steps = [
                python_step("refused", "", argv=(str(program),)),
                python_step("after", marker_code(base / "after")),
            ]
            report, _console = run_quietly(steps, root, base / "logs")

            self.assertEqual([result.status for result in report.results], [gate.LAUNCH, gate.PASS])
            self.assertIn("launch failed", report.results[0].detail)
            self.assertTrue((base / "after").is_file())

    def test_logs_and_summaries_record_every_step(self):
        # Protect the evidence a watcher reads: step logs and both summaries name every verdict.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            logs = base / "logs"
            steps = [
                python_step("passes", "print('passing output')"),
                python_step("fails", "import sys; sys.exit(5)"),
            ]
            report, console = run_quietly(steps, root, logs)

            self.assertEqual(report.exit_code, 1)
            first = (logs / "01-passes.log").read_text(encoding="utf-8")
            self.assertIn("[local-gate] step=passes", first)
            self.assertIn("passing output", first)
            self.assertIn("result=PASS exit=0", first)
            self.assertIn("result=FAIL exit=5", (logs / "02-fails.log").read_text(encoding="utf-8"))
            self.assertIn("verdict=FAIL exit=1", (logs / "summary.txt").read_text(encoding="utf-8"))
            document = json.loads((logs / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(
                [(step["id"], step["status"], step["exit_code"]) for step in document["steps"]],
                [("passes", "PASS", 0), ("fails", "FAIL", 5)],
            )
            self.assertEqual(document["exit_code"], 1)
            self.assertIn("[local-gate] start passes", console)
            self.assertIn("[local-gate] finish fails result=FAIL exit=5", console)

    def test_step_environment_reaches_the_child(self):
        # Protect env-carried flags such as RUSTDOCFLAGS from being dropped at launch.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            seen = base / "seen"
            code = (
                "import os, pathlib; "
                f"pathlib.Path({str(seen)!r}).write_text(os.environ['LOCAL_GATE_PROBE'])"
            )
            step = python_step("env", code, env=(("LOCAL_GATE_PROBE", "-D warnings"),))
            report, _console = run_quietly([step], root, base / "logs")

            self.assertEqual(report.exit_code, 0)
            self.assertEqual(seen.read_text(encoding="utf-8"), "-D warnings")


class GitStateTests(unittest.TestCase):
    """The runner reports Git state around the run and never cleans the tree."""

    def test_a_dirty_tree_is_reported_as_pre_existing_and_left_unchanged(self):
        # Protect contributors' uncommitted work: dirt present before the run is reported, not
        # blamed on the run, and every byte, status line, and index entry survives it.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            (root / "tracked.txt").write_bytes(b"edited before the run\n")
            (root / "untracked.txt").write_bytes(b"untracked\n")
            (root / "staged.txt").write_bytes(b"staged\n")
            git(root, "add", "staged.txt")
            tree = tree_bytes(root)
            status = git(root, "status", "--porcelain=v1", "--untracked-files=all").stdout
            index = git(root, "ls-files", "--stage").stdout

            report, console = run_quietly([python_step("passes", "pass")], root, base / "logs")

            self.assertEqual(report.exit_code, 0)
            self.assertEqual(report.changes, ())
            self.assertEqual(
                {path for path, _code, _fingerprint in report.before.entries},
                {"tracked.txt", "untracked.txt", "staged.txt"},
            )
            self.assertEqual(tree_bytes(root), tree)
            self.assertEqual(
                git(root, "status", "--porcelain=v1", "--untracked-files=all").stdout, status
            )
            self.assertEqual(git(root, "ls-files", "--stage").stdout, index)
            self.assertIn("pre-existing changes, present before the run and not caused by it (3)", console)
            self.assertIn("the run left tracked and untracked state unchanged", console)

    def test_a_change_made_during_the_run_fails_the_gate_and_is_kept(self):
        # Protect the verdict's meaning: a run that edited the tree it tested fails, and the
        # edit, including a further edit to an already-dirty file, stays for review.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = fixture_repository(base)
            (root / "tracked.txt").write_bytes(b"edited before the run\n")
            code = (
                "import pathlib; "
                "pathlib.Path('tracked.txt').write_bytes(b'edited by the step\\n'); "
                "pathlib.Path('made-by-step.txt').write_bytes(b'new\\n')"
            )
            report, console = run_quietly([python_step("edits", code)], root, base / "logs")

            self.assertEqual(report.results[0].status, gate.PASS)
            self.assertEqual(report.exit_code, 1)
            changes = "\n".join(report.changes)
            self.assertIn("tracked.txt: M sha256:", changes)
            self.assertIn("made-by-step.txt: clean -> ??", changes)
            self.assertEqual((root / "tracked.txt").read_bytes(), b"edited by the step\n")
            self.assertTrue((root / "made-by-step.txt").is_file())
            self.assertIn("nothing was reverted", console)
            self.assertIn("verdict=FAIL", console)

    def test_outside_a_git_work_tree_state_is_unavailable_not_fatal(self):
        # Protect exported trees: without Git the run still completes and says what it lacks.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve() / "plain"
            root.mkdir()
            ceiling = {"GIT_CEILING_DIRECTORIES": str(root.parent)}
            with mock.patch.dict(os.environ, ceiling):
                report, console = run_quietly(
                    [python_step("passes", "pass")], root, Path(directory) / "logs"
                )

            self.assertFalse(report.before.available)
            self.assertEqual(report.exit_code, 0)
            self.assertIn("Git state unavailable", console)


class TableTests(unittest.TestCase):
    """The step table is well formed and selects the right steps per host."""

    def test_every_step_is_well_formed(self):
        # Protect the invariants the runner, the renderer, and the parity checks rely on.
        ids = [step.id for step in gate.STEPS]
        self.assertEqual(len(ids), len(set(ids)))
        commands = [gate.command_text(step) for step in gate.STEPS]
        self.assertEqual(len(commands), len(set(commands)))
        for step in gate.STEPS:
            with self.subTest(step=step.id):
                self.assertRegex(step.id, r"^[a-z][a-z0-9-]*$")
                self.assertTrue(step.argv)
                self.assertTrue(step.hosts)
                self.assertLessEqual(set(step.hosts), set(gate.HOSTS))
                self.assertEqual(len(step.hosts), len(set(step.hosts)))
                self.assertIn(step.evidence, gate.EVIDENCE_CLASSES)
                self.assertGreater(step.timeout_s, 0)
                self.assertTrue(step.prerequisites)
                self.assertLessEqual(set(step.prerequisites), set(gate.PREREQUISITES))
                self.assertIn(step.shell, (None, "pwsh"))
                if step.evidence == "optional":
                    self.assertEqual(step.ci_jobs, ())
                for job in step.ci_jobs:
                    self.assertIn(gate.job_host(job), step.hosts)

    def test_host_selection_keeps_table_order_and_opt_in_classes(self):
        # Protect the default gate: each host runs its local steps in table order, release
        # steps only on request, and the doctests after the build they reuse.
        macos = [step.id for step in gate.select_steps("macos")]
        self.assertEqual(
            macos,
            [step.id for step in gate.STEPS if "macos" in step.hosts and step.evidence == "local"],
        )
        self.assertNotIn("release-macos", macos)
        self.assertIn("release-macos", [step.id for step in gate.select_steps("macos", release=True)])
        self.assertEqual(macos.index("doctests"), macos.index("workspace-crates") + 1)
        windows = [step.id for step in gate.select_steps("windows")]
        self.assertIn("msi-validator-tests", windows)
        self.assertIn("windows-warp-allocator", windows)
        self.assertNotIn("logic-coverage", windows)
        self.assertEqual(
            [step.id for step in gate.select_steps("linux", ids=["doctests", "fmt"])],
            ["fmt", "doctests"],
        )
        with self.assertRaises(ValueError):
            gate.select_steps("macos", ids=["no-such-step"])
        with self.assertRaises(ValueError):
            gate.select_steps("macos", ids=["msi-validator-tests"])

    def test_doctest_step_compiles_workspace_doctests_without_an_exemption(self):
        # Protect the first-party doctest from going uncompiled: the step runs every workspace
        # doctest in each platform's workspace-test job, and no manifest opts a library out.
        step = next(step for step in gate.STEPS if step.id == "doctests")
        self.assertEqual(gate.command_text(step), "cargo test --workspace --doc --no-fail-fast")
        self.assertEqual(step.evidence, "local")
        self.assertEqual(set(step.ci_jobs), {"macos-core", "windows-tests", "linux-core"})
        logging = (ROOT / "crates" / "sonicterm-logging" / "src" / "lib.rs").read_text(encoding="utf-8")
        self.assertIn("//! ```no_run", logging)
        for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
            if manifest.parent.name == "sonicterm-winit":
                continue
            with self.subTest(manifest=manifest.parent.name):
                self.assertNotRegex(manifest.read_text(encoding="utf-8"), r"(?m)^doctest\s*=\s*false")

    def test_command_text_matches_ci_spelling(self):
        # Protect verbatim parity: env prefixes and PowerShell paths render exactly as ci.yml
        # spells them, while the runner still launches the PowerShell script through pwsh.
        by_id = {step.id: step for step in gate.STEPS}
        self.assertEqual(
            gate.command_text(by_id["doc"]),
            'RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps',
        )
        self.assertEqual(
            gate.command_text(by_id["msi-validator-tests"]),
            ".\\scripts\\validate-windows-msi_tests.ps1",
        )
        self.assertEqual(
            gate.launch_argv(by_id["msi-validator-tests"]),
            ("pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-File",
             ".\\scripts\\validate-windows-msi_tests.ps1"),
        )

    def test_host_detection(self):
        # Protect step selection on each desktop host, including Git Bash's MSYS Python.
        for system, host in (
            ("Darwin", "macos"),
            ("Linux", "linux"),
            ("Windows", "windows"),
            ("MSYS_NT-10.0-26100", "windows"),
            ("FreeBSD", None),
        ):
            with self.subTest(system=system):
                self.assertEqual(gate.detect_host(system), host)


class CiParityTests(unittest.TestCase):
    """The table and the reasoned CI-only list account for every ci.yml gate invocation."""

    def test_the_parser_reads_every_ci_job(self):
        # Protect parity from passing vacuously: the scan must find the jobs and commands it checks.
        jobs = gate.ci_job_commands(WORKFLOW)
        self.assertEqual(
            set(jobs),
            {
                "macos-core", "macos-coverage", "macos-smoke", "macos",
                "windows-native", "windows-checks", "windows-tests", "windows-smoke", "windows",
                "linux-core", "linux-packages", "linux",
            },
        )
        commands = {job: [command for _label, command in pairs] for job, pairs in jobs.items()}
        for job in ("macos-core", "windows-checks", "linux-core"):
            self.assertIn("cargo fmt --all --check", commands[job])
        # A multi-command block and a PowerShell continuation both split into their commands.
        self.assertIn("bash scripts/test-wiki-publish.sh", commands["linux-core"])
        self.assertIn(
            'RUSTDOCFLAGS="-D warnings" cargo doc -p sonicterm-resource --all-features --no-deps',
            commands["linux-core"],
        )
        self.assertTrue(any(
            command.startswith("python scripts/native-smoke-runner.py --timeout-seconds 120 ")
            and command.endswith("-- --nocapture")
            for command in commands["windows-tests"]
        ))

    def test_the_shipped_workflow_matches_the_table(self):
        # Protect the real repository: every table step runs verbatim where it says, and every
        # ci.yml gate invocation is a table step or a reasoned CI-only entry.
        self.assertEqual(gate.ci_parity_problems(WORKFLOW), [])
        self.assertEqual(gate.ci_only_problems(ROOT, WORKFLOW), [])

    def test_editing_a_ci_gate_step_alone_fails_parity(self):
        # Protect against a ci.yml gate command drifting from the table.
        old = "run: cargo clippy --workspace --all-targets -- -D warnings"
        self.assertEqual(WORKFLOW.count(old), 3)
        mutated = WORKFLOW.replace(old, old + " -W clippy::pedantic", 1)
        problems = gate.ci_parity_problems(mutated)
        self.assertTrue(any("`cargo clippy --workspace --all-targets -- -D warnings` is missing "
                            "from CI job macos-core" in problem for problem in problems), problems)
        self.assertTrue(any("neither a table step nor on the CI-only list" in problem
                            for problem in problems), problems)

    def test_a_new_ci_gate_invocation_must_be_tabled_or_allow_listed(self):
        # Protect against a new CI-only gate that a local run would never execute.
        anchor = "      - name: Clippy\n"
        self.assertEqual(WORKFLOW.count(anchor), 3)
        added = (
            "      - name: New gate\n"
            "        timeout-minutes: 2\n"
            "        run: bash scripts/check-something-new.sh\n\n"
        )
        problems = gate.ci_parity_problems(WORKFLOW.replace(anchor, added + anchor, 1))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("`bash scripts/check-something-new.sh`, which is neither a table step", problems[0])

    def test_a_table_step_must_run_in_exactly_the_jobs_it_names(self):
        # Protect the table's CI-jobs column from naming a job that does not run the step, or
        # omitting one that does.
        fmt = next(step for step in gate.STEPS if step.id == "fmt")
        for replacement, expected in (
            (dataclasses.replace(fmt, ci_jobs=fmt.ci_jobs + ("windows-tests",)),
             "`cargo fmt --all --check` is missing from CI job windows-tests"),
            (dataclasses.replace(fmt, ci_jobs=("macos-core", "windows-checks")),
             "runs table step fmt, but the table does not name linux-core"),
        ):
            steps = tuple(replacement if step.id == "fmt" else step for step in gate.STEPS)
            problems = gate.ci_parity_problems(WORKFLOW, steps=steps)
            self.assertTrue(any(expected in problem for problem in problems), problems)

    def test_a_ci_only_cargo_test_must_rerun_a_local_target(self):
        # Protect the evidence-rerun distinction: an allow-listed cargo test that no local step
        # runs is a missing local test, whatever the entry calls itself.
        missing = gate.CiOnly(
            "evidence-rerun",
            "cargo test -p sonicterm-gpu --test no_such_target -- --nocapture",
            ("macos-core",),
            "fixture",
        )
        problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(missing,))
        self.assertTrue(any("tests/no_such_target.rs does not exist" in problem
                            for problem in problems), problems)
        rerun = next(entry for entry in gate.CI_ONLY if entry.kind == "evidence-rerun")
        problems = gate.ci_only_problems(ROOT, WORKFLOW, entries=(dataclasses.replace(rerun, kind="setup"),))
        self.assertTrue(any("missing local test" in problem for problem in problems), problems)

    def test_a_first_party_self_test_cannot_be_ci_only(self):
        # Protect the MSI-validator class of gap: a script's own tests must be a table step.
        without = tuple(step for step in gate.STEPS if step.id != "msi-validator-tests")
        entry = gate.CiOnly(
            "runtime-evidence", ".\\scripts\\validate-windows-msi_tests.ps1", ("windows-tests",), "fixture"
        )
        entries = gate.CI_ONLY + (entry,)
        # Parity alone would accept the entry, so only the self-test rule catches it.
        self.assertEqual(gate.ci_parity_problems(WORKFLOW, steps=without, entries=entries), [])
        problems = gate.ci_only_problems(ROOT, WORKFLOW, steps=without, entries=entries)
        self.assertTrue(any("first-party self-test" in problem for problem in problems), problems)

    def test_rerun_evidence_is_checked_against_the_test_source(self):
        # Protect reruns from hiding a target that the workspace pass skips as ignored.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            crate = root / "crates" / "fixture-crate"
            (crate / "tests").mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "fixture-crate"\n', encoding="utf-8")
            (root / "scripts").mkdir()
            (root / "scripts" / "check-workspace-crates.sh").write_text(
                "cargo test --workspace --lib --bins --tests --no-fail-fast\n", encoding="utf-8"
            )
            workflow = textwrap.dedent(
                """\
                name: Fixture
                jobs:
                  macos-core:
                    runs-on: macos-14
                    timeout-minutes: 5
                    steps:
                      - name: Run workspace unit and integration tests
                        timeout-minutes: 5
                        run: bash scripts/check-workspace-crates.sh

                      - name: Report probe
                        timeout-minutes: 5
                        run: cargo test -p fixture-crate --test probe -- --nocapture
                """
            )
            entry = gate.CiOnly(
                "evidence-rerun",
                "cargo test -p fixture-crate --test probe -- --nocapture",
                ("macos-core",),
                "fixture",
            )
            probe = crate / "tests" / "probe.rs"
            probe.write_text("#[test]\n#[ignore]\nfn probe() {}\n", encoding="utf-8")
            problems = gate.ci_only_problems(root, workflow, entries=(entry,))
            self.assertTrue(any("ignored tests" in problem for problem in problems), problems)

            probe.write_text("#[test]\nfn probe() {}\n", encoding="utf-8")
            self.assertEqual(gate.ci_only_problems(root, workflow, entries=(entry,)), [])

    def test_a_stale_ci_only_entry_is_reported(self):
        # Protect the allow-list from outliving the step it excuses.
        line = "run: cargo test -p sonicterm-gpu --test renderer_churn_baseline -- --nocapture"
        self.assertEqual(WORKFLOW.count(line), 2)
        problems = gate.ci_parity_problems(WORKFLOW.replace(line, "run: echo removed"))
        for job in ("macos-core", "windows-tests"):
            self.assertTrue(any(f"no longer runs in CI job {job}" in problem
                                for problem in problems), problems)

    def test_msi_validator_tests_join_the_windows_table(self):
        # Protect the Windows gate from omitting a test that CI runs.
        step = next(step for step in gate.STEPS if step.id == "msi-validator-tests")
        self.assertEqual(step.hosts, ("windows",))
        self.assertEqual(step.ci_jobs, ("windows-tests",))
        self.assertEqual(step.evidence, "local")
        self.assertNotIn(gate.command_text(step), [entry.command for entry in gate.CI_ONLY])

    def test_doctests_follow_each_shards_workspace_tests(self):
        # Protect the doctest step's reuse of the libraries the workspace step just built.
        jobs = gate.ci_job_commands(WORKFLOW)
        for job in ("macos-core", "windows-tests", "linux-core"):
            with self.subTest(job=job):
                commands = [command for _label, command in jobs[job]]
                index = commands.index("bash scripts/check-workspace-crates.sh")
                self.assertEqual(commands[index + 1], "cargo test --workspace --doc --no-fail-fast")


class DocParityTests(unittest.TestCase):
    """CLAUDE.md and both wiki files embed exactly the table's rendered form."""

    def test_each_documented_gate_equals_the_rendered_table(self):
        # Protect every documented copy of the gate from drifting from the runnable table.
        self.assertEqual(
            [name for name, _language in gate.DOCUMENTS],
            ["CLAUDE.md", "wiki/Development-and-Release.md", "wiki/Development-and-Release-zh-CN.md"],
        )
        for name, language in gate.DOCUMENTS:
            with self.subTest(document=name):
                text = (ROOT / name).read_text(encoding="utf-8")
                self.assertEqual(gate.doc_parity_problems(name, text, language), [])

    def test_editing_one_documented_gate_line_alone_fails_parity(self):
        # Protect each copy separately: an edit to one document's gate line is caught.
        old = "| `fmt` | `cargo fmt --all --check` |"
        for name, language in gate.DOCUMENTS:
            with self.subTest(document=name):
                text = (ROOT / name).read_text(encoding="utf-8")
                self.assertEqual(text.count(old), 1)
                mutated = text.replace(old, "| `fmt` | `cargo fmt --all` |")
                problems = gate.doc_parity_problems(name, mutated, language)
                self.assertTrue(any("gate block differs from the table" in problem
                                    for problem in problems), problems)

    def test_editing_the_table_alone_fails_every_document(self):
        # Protect the other direction: a table change must be pasted into every document.
        fmt = next(step for step in gate.STEPS if step.id == "fmt")
        steps = tuple(
            dataclasses.replace(fmt, hosts=("macos",), ci_jobs=("macos-core",)) if step.id == "fmt"
            else step
            for step in gate.STEPS
        )
        for name, language in gate.DOCUMENTS:
            with self.subTest(document=name):
                text = (ROOT / name).read_text(encoding="utf-8")
                self.assertNotEqual(gate.doc_parity_problems(name, text, language, steps), [])

    def test_the_block_markers_must_appear_exactly_once(self):
        # Protect extraction: a missing or duplicated block cannot pass as the gate.
        block = gate.render_gate_block("en")
        prose = f"{gate.INVOCATION}\n\n"
        self.assertEqual(gate.doc_parity_problems("doc", prose + block, "en"), [])
        self.assertNotEqual(gate.doc_parity_problems("doc", prose + block + "\n" + block, "en"), [])
        self.assertNotEqual(gate.doc_parity_problems("doc", prose, "en"), [])
        self.assertNotEqual(gate.doc_parity_problems("doc", block, "en"), [])

    def test_both_languages_render_the_same_rows(self):
        # Protect equivalent facts across the English and Chinese blocks.
        def rows(language):
            return [line for line in gate.render_gate_block(language).splitlines() if line.startswith("| `")]

        english = rows("en")
        self.assertEqual(len(english), len(gate.STEPS))
        self.assertEqual([row.replace(", ", "、") for row in english], rows("zh-CN"))


class CommandLineTests(unittest.TestCase):
    """The CLI renders, lists, and rejects invalid selections without running a step."""

    def cli(self, *arguments: str) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "local-gate.py"), *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )

    def test_render_prints_the_documented_block(self):
        # Protect the paste source that the docs parity tests compare against.
        for language in ("en", "zh-CN"):
            with self.subTest(language=language):
                completed = self.cli("--render", language)
                self.assertEqual(completed.returncode, 0, completed.stderr)
                self.assertEqual(
                    completed.stdout.decode("utf-8").replace("\r\n", "\n"),
                    gate.render_gate_block(language) + "\n",
                )

    def test_list_shows_another_hosts_steps(self):
        # Protect review of a host's gate from any other host.
        completed = self.cli("--list", "--host", "windows")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        listing = completed.stdout.decode("utf-8")
        self.assertIn("msi-validator-tests [local]", listing)
        self.assertIn(".\\scripts\\validate-windows-msi_tests.ps1", listing)
        self.assertNotIn("logic-coverage", listing)

    def test_invalid_selections_exit_2_without_running_a_step(self):
        # Protect callers from a typo or a foreign host silently running a different gate.
        detected = gate.detect_host()
        foreign = next(host for host in gate.HOSTS if host != detected)
        for arguments in (("--host", foreign), ("--step", "no-such-step")):
            with self.subTest(arguments=arguments):
                completed = self.cli(*arguments)
                self.assertEqual(completed.returncode, 2, completed.stderr)


_FAKE_CARGO = """#!/bin/sh
# Record CARGO_TARGET_DIR and every argument, separated by the unit separator.
{
  printf '%s' "${CARGO_TARGET_DIR-<unset>}"
  for argument in "$@"; do printf '\\037%s' "$argument"; done
  printf '\\n'
} >> "$FAKE_LOG"
if [ -n "$FAKE_FAIL" ]; then
  case " $* " in
    *"$FAKE_FAIL"*) exit 7 ;;
  esac
fi
exit 0
"""
_FAKE_SUCCESS = "#!/bin/sh\nexit 0\n"


def _write_tools(directory: Path, tools: dict[str, str]) -> Path:
    """Write executable fake tools into a directory that goes first on PATH."""
    bin_dir = directory / "bin"
    bin_dir.mkdir()
    for name, body in tools.items():
        path = bin_dir / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)
    return bin_dir


def _calls(log: Path) -> list[tuple[str, list[str]]]:
    """Read the fake cargo's (CARGO_TARGET_DIR, arguments) record, one entry per call."""
    if not log.exists():
        return []
    calls = []
    for line in log.read_text(encoding="utf-8").splitlines():
        target_dir, *arguments = line.split("\x1f")
        calls.append((target_dir, arguments))
    return calls


def _tool_environment(bin_dir: Path, log: Path, target_dir: str | None, **extra: str) -> dict[str, str]:
    """Return an environment that finds the fake tools first and records into log."""
    env = dict(os.environ)
    env.pop("CARGO_TARGET_DIR", None)
    if target_dir is not None:
        env["CARGO_TARGET_DIR"] = target_dir
    env["PATH"] = str(bin_dir) + os.pathsep + env.get("PATH", "")
    env["FAKE_LOG"] = str(log)
    env.update(extra)
    return env


@POSIX_ONLY
class WorkspaceGateScriptTests(unittest.TestCase):
    """check-workspace-crates.sh keeps a caller's target directory and runs every phase."""

    def run_script(self, directory: Path, target_dir: str | None = None, fail: str = ""):
        bin_dir = _write_tools(
            directory,
            {"cargo": _FAKE_CARGO, "rustfmt": _FAKE_SUCCESS, "python3": _FAKE_SUCCESS},
        )
        log = directory / "calls.log"
        completed = subprocess.run(
            ["bash", str(ROOT / "scripts" / "check-workspace-crates.sh")],
            env=_tool_environment(bin_dir, log, target_dir, FAKE_FAIL=fail),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
        return completed, _calls(log)

    def test_nested_winit_phases_honor_a_caller_target_dir(self):
        # Protect CARGO_TARGET_DIR isolation through the nested pinned-winit builds.
        with tempfile.TemporaryDirectory() as directory:
            isolated = str(Path(directory) / "isolated-target")
            completed, calls = self.run_script(Path(directory), target_dir=isolated)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(len(calls), 3, calls)
            for _target_dir, arguments in calls[:2]:
                self.assertIn("crates/sonicterm-winit/Cargo.toml", arguments)
                self.assertEqual(arguments[arguments.index("--target-dir") + 1], isolated)
            target_dir, arguments = calls[2]
            self.assertEqual(arguments, ["test", "--workspace", "--lib", "--bins", "--tests", "--no-fail-fast"])
            self.assertEqual(target_dir, isolated)

    def test_without_a_caller_target_dir_winit_shares_the_workspace_target(self):
        # Protect the default cache reuse between the workspace and the pinned winit.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_script(Path(directory))

            self.assertEqual(completed.returncode, 0, completed.stderr)
            expected = os.path.join(os.path.realpath(ROOT), "target")
            for _target_dir, arguments in calls[:2]:
                value = arguments[arguments.index("--target-dir") + 1]
                self.assertEqual(os.path.realpath(value), expected)

    def test_a_failing_phase_does_not_stop_later_phases(self):
        # Protect the script's fail-complete contract across its winit and workspace phases.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_script(Path(directory), fail="sonicterm-winit")

            self.assertNotEqual(completed.returncode, 0)
            self.assertEqual(len(calls), 3, calls)
            self.assertEqual(calls[2][1][:2], ["test", "--workspace"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
