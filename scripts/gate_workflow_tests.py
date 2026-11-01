"""Gate workflow tests."""

from __future__ import annotations
import dataclasses
from pathlib import Path
import re
import tempfile
import textwrap
import unittest

import gate_catalog
import gate_ci_catalog
import gate_ci_checks
import gate_cli
import gate_harness
import gate_model
import gate_test_fixture
import gate_workflow


class CiParityTests(unittest.TestCase):
    """The table and the reasoned CI-only list account for every ci.yml gate invocation."""

    def test_the_parser_reads_every_ci_job(self):
        # Protect parity from passing vacuously: the scan must find the jobs and commands it checks.
        jobs = gate_workflow.ci_job_commands(gate_test_fixture.WORKFLOW)
        self.assertEqual(
            set(jobs),
            {
                "macos-core", "macos-smoke", "macos", "tooling",
                "windows-native", "windows-glyph-oracle", "windows-checks", "windows-tests-workspace", "windows-tests-harness",
                "windows-tests-harness-features", "windows-tests-harness-api", "windows-tests-runtime",
                "windows-smoke", "windows",
                "linux-core", "linux-coverage", "linux-packages", "linux",
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
            for command in commands["windows-tests-runtime"]
        ))

    def test_the_shipped_workflow_matches_the_table(self):
        # Protect the real repository: every table step runs verbatim where it says, and every
        # ci.yml gate invocation is a table step or a reasoned CI-only entry.
        self.assertEqual(gate_workflow.ci_parity_problems(gate_test_fixture.WORKFLOW), [])
        self.assertEqual(gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW), [])

    def test_macos_smoke_gates_cannot_be_skipped_or_made_advisory(self):
        # Both architectures inherit the mandatory selection build, selection smoke and perf scenario smoke:
        # Apple Silicon in macos-core, Intel in macos-smoke. No job- or step-level `if:` or
        # `continue-on-error:` may bypass any of them in either job.
        self.assertEqual(gate_workflow.macos_smoke_ci_problems(gate_test_fixture.WORKFLOW), [])
        for command in (
            "cargo build --locked -p sonicterm-app --example native_split_selection",
            "python3 scripts/native-selection-smoke.py",
            "python3 scripts/perf-compare.py --smoke",
        ):
            line = "        run: " + command
            # One step in each job: macos-core's is the file's first, macos-smoke's its last.
            self.assertEqual(gate_test_fixture.WORKFLOW.count(line), 2)
            for job, occurrence in (("macos-core", 0), ("macos-smoke", 1)):
                def mutate(new_line: str) -> str:
                    """Replace only this job's occurrence of the step's run line."""
                    pieces = gate_test_fixture.WORKFLOW.split(line)
                    return line.join(pieces[:occurrence + 1]) + new_line + line.join(pieces[occurrence + 1:])
                for bypass in ("if: false", "continue-on-error: true"):
                    mutated = mutate("        " + bypass + "\n" + line)
                    self.assertTrue(gate_workflow.ci_parity_problems(mutated))
                    # The guard itself names the bypassed job and step, not only some other parity finding.
                    self.assertIn(f"{job} step `{command}` must not be conditional or advisory",
                                  gate_workflow.macos_smoke_ci_problems(mutated))
                self.assertTrue(gate_workflow.ci_parity_problems(mutate("        run: echo omitted")))
        for job in ("macos-core", "macos-smoke"):
            for bypass in ("if: false", "continue-on-error: true"):
                mutated = gate_test_fixture.WORKFLOW.replace(f"  {job}:\n", f"  {job}:\n    " + bypass + "\n", 1)
                self.assertTrue(gate_workflow.ci_parity_problems(mutated))
                self.assertIn(f"{job} must not be conditional or advisory", gate_workflow.macos_smoke_ci_problems(mutated))
        # Neither architecture may drop out, and the Intel matrix may not regain an Apple Silicon leg that
        # would run that architecture twice.
        message = "the macOS smoke gates must run on both macOS architectures"
        for mutated in (
            gate_test_fixture.WORKFLOW.replace("arch: x86_64", "arch: omitted", 1),
            gate_test_fixture.WORKFLOW.replace("ci mac-aarch64\n", "ci mac-omitted\n", 1),
            gate_test_fixture.WORKFLOW.replace("          - runner: macos-15-intel\n",
                             "          - runner: macos-15\n            arch: aarch64\n"
                             "          - runner: macos-15-intel\n", 1),
        ):
            self.assertNotEqual(mutated, gate_test_fixture.WORKFLOW)
            self.assertIn(message, gate_workflow.macos_smoke_ci_problems(mutated))
            self.assertTrue(gate_workflow.ci_parity_problems(mutated))

    def test_native_selection_build_and_run_precede_release_packaging(self):
        # A retained executable must never stand in for the current source's fixture build.
        build = (
            "      - name: Build macOS native selection fixture\n"
            "        run: cargo build --locked -p sonicterm-app --example native_split_selection\n\n"
        )
        smoke = (
            "      - name: Require macOS native split selection\n"
            "        run: python3 scripts/native-selection-smoke.py\n\n"
        )
        self.assertEqual(gate_test_fixture.WORKFLOW.count(build + smoke), 2)
        message = "native selection must build then run before the release build and packaging"
        # The first occurrence is macos-core's Apple Silicon steps, the last macos-smoke's Intel ones.
        reversed_steps = gate_test_fixture.WORKFLOW.replace(build + smoke, smoke + build, 1)
        self.assertIn(f"macos-core: {message}", gate_workflow.macos_smoke_ci_problems(reversed_steps))
        late_steps = gate_test_fixture.WORKFLOW.replace(build + smoke, "", 1).replace(
            "      - name: Upload macOS package evidence\n",
            build + smoke + "      - name: Upload macOS package evidence\n", 1)
        self.assertIn(f"macos-core: {message}", gate_workflow.macos_smoke_ci_problems(late_steps))
        intel_reversed = (smoke + build).join(gate_test_fixture.WORKFLOW.rsplit(build + smoke, 1))
        self.assertIn(f"macos-smoke: {message}", gate_workflow.macos_smoke_ci_problems(intel_reversed))

    def test_perf_smoke_runs_after_the_selection_smoke_and_before_the_release_build(self):
        # Both architectures run the perf scenario smoke between the native selection smoke and the release build;
        # moving it before the selection smoke or after the release build fails the guard.
        selection = (
            "      - name: Require macOS native split selection\n"
            "        run: python3 scripts/native-selection-smoke.py\n\n"
        )
        perf = (
            "      - name: Require macOS perf scenario smoke\n"
            "        run: python3 scripts/perf-compare.py --smoke\n\n"
        )
        release = "      - name: Build macOS release binary for runtime smoke\n"
        self.assertIn(selection + perf + release, gate_test_fixture.WORKFLOW)
        message = ("the perf scenario smoke must run after the native selection smoke "
                   "and before the release build")
        # The first occurrence is macos-core's Apple Silicon steps, the last macos-smoke's Intel ones.
        early = gate_test_fixture.WORKFLOW.replace(selection + perf, perf + selection, 1)
        self.assertIn(f"macos-core: {message}", gate_workflow.macos_smoke_ci_problems(early))
        late = gate_test_fixture.WORKFLOW.replace(perf, "", 1).replace(
            "      - name: Upload macOS package evidence\n",
            perf + "      - name: Upload macOS package evidence\n", 1)
        self.assertIn(f"macos-core: {message}", gate_workflow.macos_smoke_ci_problems(late))
        self.assertTrue(gate_workflow.ci_parity_problems(late))
        intel_early = (perf + selection).join(gate_test_fixture.WORKFLOW.rsplit(selection + perf, 1))
        self.assertIn(f"macos-smoke: {message}", gate_workflow.macos_smoke_ci_problems(intel_early))

    def test_windows_perf_smoke_is_required_locally_on_windows(self):
        # The scenario harness must build and run on Windows: the compile-only build first, then the smoke,
        # both local Windows steps that the windows-tests-runtime shard runs.
        by_id = {step.id: step for step in gate_catalog.STEPS}
        for name, command, timeout, policy in (
            ("windows-perf-build", "cargo build --locked -p sonicterm-app --example perf_scenarios", 1500,
             gate_model.WindowsPolicy.COMPILE_ONLY),
            ("windows-perf-smoke", "python scripts/perf-compare.py --smoke", 4200, gate_model.WindowsPolicy.STRICT),
        ):
            step = by_id[name]
            self.assertEqual(gate_harness.command_text(step), command)
            self.assertEqual(step.hosts, ("windows",))
            self.assertEqual(step.evidence, "local")
            self.assertEqual(step.prerequisites, ("rust", "native"))
            self.assertEqual(step.ci_jobs, ("windows-tests-runtime",))
            self.assertEqual(step.timeout_s, timeout)
            self.assertEqual(step.windows_policy, policy)
            self.assertIn(step, gate_cli.select_steps("windows"))
            self.assertNotIn(step, gate_cli.select_steps("macos"))
        selected = [step.id for step in gate_cli.select_steps("windows")]
        self.assertEqual(selected.index("windows-perf-smoke"), selected.index("windows-perf-build") + 1)

    def test_windows_perf_gates_cannot_be_skipped_or_made_advisory(self):
        # windows-tests-runtime runs the harness build and the perf smoke unconditionally; a step-level `if:` or
        # `continue-on-error:` on either is named by the guard and fails parity.
        self.assertEqual(gate_workflow.windows_runtime_ci_problems(gate_test_fixture.WORKFLOW), [])
        for command in ("cargo build --locked -p sonicterm-app --example perf_scenarios",
                        "python scripts/perf-compare.py --smoke"):
            line = "        run: " + command + "\n"
            self.assertEqual(gate_test_fixture.WORKFLOW.count(line), 1)
            for bypass in ("if: false", "continue-on-error: true"):
                mutated = gate_test_fixture.WORKFLOW.replace(line, "        " + bypass + "\n" + line, 1)
                self.assertIn(f"windows-tests-runtime step `{command}` must not be conditional or advisory",
                              gate_workflow.windows_runtime_ci_problems(mutated))
                self.assertTrue(gate_workflow.ci_parity_problems(mutated))
            self.assertTrue(gate_workflow.ci_parity_problems(gate_test_fixture.WORKFLOW.replace(line, "        run: echo omitted\n", 1)))
        for bypass in ("if: false", "continue-on-error: true"):
            mutated = gate_test_fixture.WORKFLOW.replace("  windows-tests-runtime:\n", "  windows-tests-runtime:\n    " + bypass + "\n", 1)
            self.assertIn("windows-tests-runtime must not be conditional or advisory",
                          gate_workflow.windows_runtime_ci_problems(mutated))

    def test_windows_perf_build_precedes_the_smoke(self):
        # The smoke's own build must find the harness already built by the compile-only step, so the
        # build step comes first; reversing them fails the guard.
        build = ("      - name: Build Windows perf scenario harness\n"
                 "        run: cargo build --locked -p sonicterm-app --example perf_scenarios\n\n")
        smoke = ("      - name: Require Windows perf scenario smoke\n"
                 "        run: python scripts/perf-compare.py --smoke\n\n")
        self.assertIn(build + smoke, gate_test_fixture.WORKFLOW)
        reversed_steps = gate_test_fixture.WORKFLOW.replace(build + smoke, smoke + build, 1)
        self.assertIn("the Windows perf scenario harness must build before its smoke runs",
                      gate_workflow.windows_runtime_ci_problems(reversed_steps))
        self.assertTrue(gate_workflow.ci_parity_problems(reversed_steps))

    def test_editing_a_ci_gate_step_alone_fails_parity(self):
        # Protect against a ci.yml gate command drifting from the table.
        old = "run: cargo clippy --workspace --all-targets -- -D warnings"
        self.assertEqual(gate_test_fixture.WORKFLOW.count(old), 3)
        mutated = gate_test_fixture.WORKFLOW.replace(old, old + " -W clippy::pedantic", 1)
        problems = gate_workflow.ci_parity_problems(mutated)
        self.assertTrue(any("`cargo clippy --workspace --all-targets -- -D warnings` is missing "
                            "from CI job macos-core" in problem for problem in problems), problems)
        self.assertTrue(any("neither a table step nor on the CI-only list" in problem
                            for problem in problems), problems)

    def test_cargo_deny_runs_once_in_linux_core_and_any_other_spelling_fails_parity(self):
        # Protect the dependency audit: CI runs exactly the table's `cargo deny --locked check` in
        # linux-core; dropping it, altering it, or adding another audit invocation fails parity.
        line = "        run: cargo deny --locked check\n"
        self.assertEqual(gate_test_fixture.WORKFLOW.count(line), 1)
        self.assertEqual(gate_workflow.ci_parity_problems(gate_test_fixture.WORKFLOW), [])
        missing = "`cargo deny --locked check` is missing from CI job linux-core"
        unlisted = "neither a table step nor on the CI-only list"
        for mutated, expected in (
            (gate_test_fixture.WORKFLOW.replace(line, "        run: cargo deny check\n"), (missing, unlisted)),
            (gate_test_fixture.WORKFLOW.replace(line, "        run: cargo deny --locked check advisories\n"),
             (missing, unlisted)),
            (gate_test_fixture.WORKFLOW.replace(line, "        run: cargo --locked deny check\n"),
             (missing, "a cargo option before the gate subcommand")),
            (gate_test_fixture.WORKFLOW.replace(line, "        run: cargo deny --locked check || true\n"),
             ("beside `||`",)),
            (gate_test_fixture.WORKFLOW.replace(line, "        run: echo skipped\n"), (missing,)),
            (gate_test_fixture.WORKFLOW.replace("      - name: Clippy\n",
                              "      - name: Audit\n        run: cargo deny --locked check bans\n\n"
                              "      - name: Clippy\n", 1),
             (unlisted,)),
        ):
            problems = gate_workflow.ci_parity_problems(mutated)
            for text in expected:
                with self.subTest(expected=text):
                    self.assertTrue(any(text in problem for problem in problems), problems)
        # Without its table step, the CI audit is an unlisted gate.
        steps = tuple(step for step in gate_catalog.STEPS if step.id != "cargo-deny")
        problems = gate_workflow.ci_parity_problems(gate_test_fixture.WORKFLOW, steps=steps)
        self.assertTrue(any("`cargo deny --locked check`, which is neither a table step" in problem
                            for problem in problems), problems)

    def test_a_new_ci_gate_invocation_must_be_tabled_or_allow_listed(self):
        # Protect against a new CI-only gate that a local run would never execute.
        anchor = "      - name: Clippy\n"
        self.assertEqual(gate_test_fixture.WORKFLOW.count(anchor), 3)
        added = (
            "      - name: New gate\n"
            "        timeout-minutes: 2\n"
            "        run: bash scripts/check-something-new.sh\n\n"
        )
        problems = gate_workflow.ci_parity_problems(gate_test_fixture.WORKFLOW.replace(anchor, added + anchor, 1))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("`bash scripts/check-something-new.sh`, which is neither a table step", problems[0])

    def test_a_table_step_must_run_in_exactly_the_jobs_it_names(self):
        # Protect the table's CI-jobs column from naming a job that does not run the step, or
        # omitting one that does.
        fmt = next(step for step in gate_catalog.STEPS if step.id == "fmt")
        for replacement, expected in (
            (dataclasses.replace(fmt, ci_jobs=fmt.ci_jobs + ("windows-tests-workspace",)),
             "`cargo fmt --all --check` is missing from CI job windows-tests-workspace"),
            (dataclasses.replace(fmt, ci_jobs=("macos-core", "windows-checks")),
             "runs table step fmt, but the table does not name linux-core"),
        ):
            steps = tuple(replacement if step.id == "fmt" else step for step in gate_catalog.STEPS)
            problems = gate_workflow.ci_parity_problems(gate_test_fixture.WORKFLOW, steps=steps)
            self.assertTrue(any(expected in problem for problem in problems), problems)

    def test_a_ci_only_cargo_test_must_rerun_a_local_target(self):
        # Protect the evidence-rerun distinction: an allow-listed cargo test that no local step
        # runs is a missing local test, whatever the entry calls itself.
        missing = gate_ci_catalog.CiOnly(
            "evidence-rerun",
            "cargo test -p sonicterm-gpu --test no_such_target -- --nocapture",
            ("macos-core",),
            "fixture",
        )
        problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(missing,))
        self.assertTrue(any("tests/no_such_target.rs does not exist" in problem
                            for problem in problems), problems)
        rerun = next(entry for entry in gate_ci_catalog.CI_ONLY if entry.kind == "evidence-rerun")
        problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(dataclasses.replace(rerun, kind="setup"),))
        self.assertTrue(any("missing local test" in problem for problem in problems), problems)

    def test_a_first_party_self_test_cannot_be_ci_only(self):
        # Protect the MSI-validator class of gap: a script's own tests must be a table step.
        without = tuple(step for step in gate_catalog.STEPS if step.id != "msi-validator-tests")
        entry = gate_ci_catalog.CiOnly(
            "runtime-evidence", ".\\scripts\\validate-windows-msi_tests.ps1", ("windows-tests-workspace",), "fixture"
        )
        entries = gate_ci_catalog.CI_ONLY + (entry,)
        # Parity alone would accept the entry, so only the self-test rule catches it.
        self.assertEqual(gate_workflow.ci_parity_problems(gate_test_fixture.WORKFLOW, steps=without, entries=entries), [])
        problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, steps=without, entries=entries)
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
                    runs-on: macos-15
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
            entry = gate_ci_catalog.CiOnly(
                "evidence-rerun",
                "cargo test -p fixture-crate --test probe -- --nocapture",
                ("macos-core",),
                "fixture",
            )
            probe = crate / "tests" / "probe.rs"
            probe.write_text("#[test]\n#[ignore]\nfn probe() {}\n", encoding="utf-8")
            problems = gate_ci_checks.ci_only_problems(root, workflow, entries=(entry,))
            self.assertTrue(any("ignored tests" in problem for problem in problems), problems)

            probe.write_text("#[test]\nfn probe() {}\n", encoding="utf-8")
            self.assertEqual(gate_ci_checks.ci_only_problems(root, workflow, entries=(entry,)), [])

    def test_a_stale_ci_only_entry_is_reported(self):
        # Protect the allow-list from outliving the step it excuses.
        line = "run: cargo test -p sonicterm-gpu --test renderer_churn_baseline -- --nocapture"
        self.assertEqual(gate_test_fixture.WORKFLOW.count(line), 2)
        problems = gate_workflow.ci_parity_problems(gate_test_fixture.WORKFLOW.replace(line, "run: echo removed"))
        for job in ("macos-core", "windows-tests-runtime"):
            self.assertTrue(any(f"no longer runs in CI job {job}" in problem
                                for problem in problems), problems)

    def test_runtime_evidence_reruns_are_backed_by_the_windows_workspace_shard(self):
        # The runtime shard reruns integration tests only to print their reports; the workspace shard must run
        # those tests, and both must be mandatory Windows jobs that the fail-closed aggregate checks.
        workspace = "bash scripts/check-workspace-crates.sh"
        jobs = gate_workflow.ci_job_commands(gate_test_fixture.WORKFLOW)
        self.assertEqual(gate_ci_checks.rerun_backing_problems(gate_test_fixture.WORKFLOW, jobs, workspace, ["windows-tests-runtime"]), [])
        self.assertEqual(gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW), [])
        head, tail = gate_test_fixture.WORKFLOW.split("  windows-tests-workspace:\n", 1)
        body, rest = tail.split("  windows-tests-harness:\n", 1)
        line = "        run: bash scripts/check-workspace-crates.sh\n"
        self.assertEqual(body.count(line), 1)

        before_aggregate, after_aggregate = gate_test_fixture.WORKFLOW.split("  windows:\n", 1)
        aggregate, after_linux = after_aggregate.split("  linux-core:\n", 1)
        verify = "      - name: Verify Windows shards\n        shell: bash\n"
        compare = '            test "$result" = "success"\n'
        loop_header = re.search(r"(?m)^          for result in .+; do$", aggregate)[0]

        def in_aggregate(old: str, new: str) -> str:
            """Mutate only the Windows aggregate's body; the macOS and Linux aggregates carry the same lines."""
            self.assertEqual(aggregate.count(old), 1, old)
            return before_aggregate + "  windows:\n" + aggregate.replace(old, new, 1) + "  linux-core:\n" + after_linux

        def in_workspace(old: str, new: str) -> str:
            """Mutate only the workspace shard's body."""
            self.assertIn(old, body)
            return head + "  windows-tests-workspace:\n" + body.replace(old, new, 1) + "  windows-tests-harness:\n" + rest

        mutations = {
            "the backing workspace gate removed": (in_workspace(line, "        run: echo removed\n"),
                                                   "does not run `bash scripts/check-workspace-crates.sh`"),
            "the backing workspace gate made advisory": (
                in_workspace(line, "        continue-on-error: true\n" + line), "must not be conditional or advisory"),
            "the backing shard made conditional": (
                in_workspace("    runs-on: windows-latest\n", "    runs-on: windows-latest\n    if: false\n"),
                "must not be conditional or advisory"),
            "the backing shard moved off Windows": (
                in_workspace("    runs-on: windows-latest\n", "    runs-on: ubuntu-latest\n"),
                "must run on windows-latest"),
            "the backing shard left out of the aggregate": (
                gate_test_fixture.WORKFLOW.replace('"$TESTS_WORKSPACE" ', "", 1), "aggregate must need and check windows-tests-workspace"),
            "the rerun shard left out of the aggregate": (
                gate_test_fixture.WORKFLOW.replace("windows-tests-runtime, windows-smoke]", "windows-smoke]", 1),
                "aggregate must need and check windows-tests-runtime"),
            "the aggregate's verification step skipped": (
                in_aggregate(verify, verify + "        if: false\n"),
                "aggregate must need and check windows-tests-workspace"),
            "the aggregate's verification step made advisory": (
                in_aggregate(verify, verify + "        continue-on-error: true\n"),
                "aggregate must need and check windows-tests-workspace"),
            "the aggregate's comparison bypassed": (
                in_aggregate(compare, compare.rstrip("\n") + " || true\n"),
                "aggregate must need and check windows-tests-workspace"),
            "the aggregate made conditional on success": (
                in_aggregate("    if: always()\n", "    if: success()\n"),
                "aggregate must need and check windows-tests-workspace"),
            "the aggregate's results consumed by a no-op loop": (
                in_aggregate(loop_header, loop_header.replace("; do", ' ; do :; done; for result in "success"; do', 1)),
                "aggregate must need and check windows-tests-workspace"),
            "an early exit behind a commented run key": (
                in_aggregate(loop_header, "          exit 0\n          #        run: |\n" + loop_header),
                "aggregate must need and check windows-tests-workspace"),
            "an extra unbound loop operand": (
                in_aggregate(loop_header, loop_header.replace("; do", ' "$UNBOUND"; do', 1)),
                "aggregate must need and check windows-tests-workspace"),
        }
        for label, (mutated, expected) in mutations.items():
            with self.subTest(mutation=label):
                self.assertTrue(mutated != gate_test_fixture.WORKFLOW, "the mutation did not apply")
                problems = gate_ci_checks.rerun_backing_problems(
                    mutated, gate_workflow.ci_job_commands(mutated), workspace, ["windows-tests-runtime"])
                self.assertTrue(any(expected in problem for problem in problems), problems)
                self.assertTrue(gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, mutated))
        # A job with no reviewed backing pair still needs the workspace step in the same job.
        self.assertEqual(gate_ci_checks.rerun_backing_problems(gate_test_fixture.WORKFLOW, jobs, workspace, ["macos-core"]), [])

    def test_msi_validator_tests_join_the_windows_table(self):
        # Protect the Windows gate from omitting a test that CI runs.
        step = next(step for step in gate_catalog.STEPS if step.id == "msi-validator-tests")
        self.assertEqual(step.hosts, ("windows",))
        self.assertEqual(step.ci_jobs, ("windows-tests-workspace",))
        self.assertEqual(step.evidence, "local")
        self.assertNotIn(gate_harness.command_text(step), [entry.command for entry in gate_ci_catalog.CI_ONLY])

    def test_doctests_follow_each_shards_workspace_tests(self):
        # Protect the doctest step's reuse of the libraries the workspace step just built.
        jobs = gate_workflow.ci_job_commands(gate_test_fixture.WORKFLOW)
        for job in ("macos-core", "windows-tests-workspace", "linux-core"):
            with self.subTest(job=job):
                commands = [command for _label, command in jobs[job]]
                index = commands.index("bash scripts/check-workspace-crates.sh")
                self.assertEqual(commands[index + 1], "cargo test --workspace --doc --no-fail-fast")
