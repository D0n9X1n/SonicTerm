"""Gate commands tests."""

from __future__ import annotations
import unittest

import gate_ci_catalog
import gate_ci_checks
import gate_commands
import gate_test_fixture
import gate_workflow


def _with_step(step: str) -> str:
    """Insert a step before macos-core's Clippy step in the complete workflow."""
    anchor = "      - name: Clippy\n"
    if gate_test_fixture.WORKFLOW.count(anchor) != 3:
        raise AssertionError("the Clippy anchor moved; update the mutation tests")
    return gate_test_fixture.WORKFLOW.replace(anchor, step + anchor, 1)


class CiParserTests(unittest.TestCase):
    """The ci.yml reader parses the forms it models and raises on every other form."""

    def test_a_block_header_comment_is_read_as_a_block(self):
        # Protect `run: | # comment`: the block is parsed, so a new gate in it is still checked.
        workflow = _with_step(
            "      - name: New gate\n"
            "        timeout-minutes: 2\n"
            "        run: | # explained here\n"
            "          bash scripts/check-something-new.sh\n\n"
        )
        commands = [command for _label, command in gate_workflow.ci_job_commands(workflow)["macos-core"]]
        self.assertIn("bash scripts/check-something-new.sh", commands)
        problems = gate_workflow.ci_parity_problems(workflow)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("neither a table step nor on the CI-only list", problems[0])

    def test_every_unmodeled_run_form_raises(self):
        # Protect parity from a new gate step that an unfamiliar spelling would hide: each of
        # these forms raises against the complete workflow instead of being skipped.
        new = "bash scripts/check-something-new.sh"
        forms = {
            "double-quoted key": f'        "run": {new}\n',
            "single-quoted key": f"        'run': {new}\n",
            "value on the next line": f"        run:\n          {new}\n",
            "continued plain value": f"        run: {new}\n          --extra\n",
            "folded block": f"        run: >\n          {new}\n",
            "stripped folded block": f"        run: >-\n          {new}\n",
            "indentation indicator": f"        run: |2\n          {new}\n",
            "unknown header text": f"        run: |x\n          {new}\n",
            "double-quoted value": f'        run: "{new}"\n',
            "single-quoted value": f"        run: '{new}'\n",
            "flow value": f"        run: [{new}]\n",
            "comment after a plain value": f"        run: {new} # note\n",
            "misaligned key": f"         run: {new}\n",
            "tab in indentation": f"        run: |\n        \t{new}\n",
            "block line left of its first line": f"        run: |\n            echo first\n          {new}\n",
            "empty block": "        run: |\n",
            "second run key": f"        run: echo first\n        run: {new}\n",
            "working directory": f"        working-directory: crates\n        run: {new}\n",
            "local action": "        uses: ./.github/actions/gate\n",
        }
        for form, body in forms.items():
            with self.subTest(form=form):
                workflow = _with_step("      - name: New gate\n        timeout-minutes: 2\n" + body + "\n")
                with self.assertRaises(ValueError):
                    gate_workflow.ci_job_commands(workflow)

    def test_every_unmodeled_job_form_raises(self):
        # Protect parity from jobs whose steps the reader cannot see: each form raises.
        anchor = "  linux-coverage:\n"
        self.assertEqual(gate_test_fixture.WORKFLOW.count(anchor), 1)
        forms = {
            "reusable workflow job": "  reusable:\n    uses: ./.github/workflows/other.yml\n\n",
            "job without steps": "  empty:\n    runs-on: ubuntu-latest\n\n",
            "flow-style steps": "  flow:\n    runs-on: ubuntu-latest\n    steps: []\n\n",
            "quoted job id": '  "quoted":\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n\n',
            "step item without a key": (
                "  bare:\n    runs-on: ubuntu-latest\n    steps:\n      -\n        run: bash scripts/x.sh\n\n"
            ),
            "odd indentation": "  odd:\n    runs-on: ubuntu-latest\n    steps:\n     - run: bash scripts/x.sh\n\n",
        }
        for form, job in forms.items():
            with self.subTest(form=form):
                with self.assertRaises(ValueError):
                    gate_workflow.ci_job_commands(gate_test_fixture.WORKFLOW.replace(anchor, job + anchor, 1))

    def test_inherited_run_defaults_raise_like_a_step_working_directory(self):
        # Protect parity from defaults that move or rewrap every run step: workflow and job
        # defaults, in block or flow form and before or after jobs, raise as a step's own
        # working-directory does.
        job = "  macos-core:\n    name: macOS core gates\n"
        self.assertEqual(gate_test_fixture.WORKFLOW.count(job), 1)
        block = "defaults:\n  run:\n    working-directory: other-project\n"
        flow = "defaults: {run: {working-directory: other-project}}\n"
        forms = {
            "workflow block": gate_test_fixture.WORKFLOW.replace("\njobs:\n", "\n" + block + "\njobs:\n", 1),
            "workflow flow": gate_test_fixture.WORKFLOW.replace("\njobs:\n", "\n" + flow + "\njobs:\n", 1),
            "workflow block after jobs": gate_test_fixture.WORKFLOW.rstrip("\n") + "\n\n" + block,
            "quoted workflow key": gate_test_fixture.WORKFLOW.replace(
                "\njobs:\n", '\n"defaults": {run: {shell: bash}}\n\njobs:\n', 1
            ),
            "job block": gate_test_fixture.WORKFLOW.replace(
                job, job + "    defaults:\n      run:\n        working-directory: other-project\n", 1
            ),
            "job flow": gate_test_fixture.WORKFLOW.replace(
                job, job + "    defaults: {run: {working-directory: other-project}}\n", 1
            ),
        }
        for form, workflow in forms.items():
            with self.subTest(form=form):
                self.assertNotEqual(workflow, gate_test_fixture.WORKFLOW)
                with self.assertRaisesRegex(ValueError, "defaults"):
                    gate_workflow.ci_job_commands(workflow)
                with self.assertRaisesRegex(ValueError, "defaults"):
                    gate_workflow.ci_parity_problems(workflow)

    def test_a_step_shell_other_than_bash_or_pwsh_raises(self):
        # Protect parity from a shell that moves or rewraps a step's commands: a step shell:
        # other than a plain bash or pwsh raises as a working-directory does, in any spelling,
        # while the plain names the shipped workflow uses are still read.
        step = (
            "      - name: Rewrapped gate\n"
            "        timeout-minutes: 2\n"
            "SHELL"
            "        run: cargo fmt --all --check\n\n"
        )
        forms = {
            "sh": "        shell: sh\n",
            "cmd": "        shell: cmd\n",
            "python": "        shell: python\n",
            "powershell": "        shell: powershell\n",
            "custom template": "        shell: env -C other-project bash -e {0}\n",
            "expression": "        shell: ${{ matrix.shell }}\n",
            "comment after the name": "        shell: bash # note\n",
            "single-quoted value": "        shell: 'bash'\n",
            "double-quoted value": '        shell: "pwsh"\n',
            "quoted key": '        "shell": sh\n',
            "flow sequence": "        shell: [bash]\n",
            "flow mapping": "        shell: {bash: -e}\n",
            "literal block": "        shell: |\n          bash\n",
            "folded block": "        shell: >-\n          env -C other-project bash -e {0}\n",
            "value on the next line": "        shell:\n          bash\n",
            "continued plain value": "        shell: bash\n          -e {0}\n",
            "continued template": "        shell: bash\n          -c 'cd other-project; bash {0}'\n",
        }
        for form, shell in forms.items():
            with self.subTest(form=form):
                workflow = _with_step(step.replace("SHELL", shell))
                with self.assertRaisesRegex(ValueError, "shell"):
                    gate_workflow.ci_job_commands(workflow)
                with self.assertRaisesRegex(ValueError, "shell"):
                    gate_workflow.ci_parity_problems(workflow)
        # Named literally, so widening the allowed set needs a test change too.
        for name in ("bash", "pwsh"):
            with self.subTest(shell=name):
                workflow = _with_step(step.replace("SHELL", f"        shell: {name}\n"))
                commands = [command for _label, command in gate_workflow.ci_job_commands(workflow)["macos-core"]]
                self.assertEqual(commands.count("cargo fmt --all --check"), 2)


class CommandClassificationTests(unittest.TestCase):
    """Gate commands are normalized, then every invocation in a line is classified and proved."""

    def test_spellings_normalize_to_one_invocation(self):
        # Protect classification from spelling: a toolchain override, quotes, and either path
        # separator name the same gate.
        plain, reasons = gate_commands.classify_command("cargo test -p sonicterm-gpu --test probe -- --nocapture")
        self.assertEqual(reasons, [])
        pinned, reasons = gate_commands.classify_command(
            "cargo +stable test -p sonicterm-gpu --test probe -- --nocapture"
        )
        self.assertEqual(reasons, [])
        self.assertEqual(
            [(invocation.kind, invocation.name, invocation.args) for invocation in pinned],
            [(invocation.kind, invocation.name, invocation.args) for invocation in plain],
        )
        self.assertEqual(pinned[0].toolchain, "+stable")
        for spelling in (
            "python3 scripts/check_tests.py",
            'python3 "scripts/check_tests.py"',
            "python3 'scripts/check_tests.py'",
            "python3 scripts\\check_tests.py",
            "python3 ./scripts/check_tests.py",
            '"$python_cmd" scripts/check_tests.py',
        ):
            with self.subTest(spelling=spelling):
                invocations, reasons = gate_commands.classify_command(spelling)
                self.assertEqual(reasons, [])
                self.assertEqual(
                    [(invocation.kind, invocation.name) for invocation in invocations],
                    [("script", "check_tests.py")],
                )
                self.assertTrue(gate_commands.is_self_test(invocations[0]))

    def test_cargo_deny_is_a_gate_in_every_spelling(self):
        # Protect the audit from escaping classification: `cargo deny` normalizes like the other
        # cargo gates, and a spelling that could hide its exit status is refused.
        plain, reasons = gate_commands.classify_command("cargo deny --locked check")
        self.assertEqual(reasons, [])
        self.assertEqual([(invocation.kind, invocation.name, invocation.args) for invocation in plain],
                         [("cargo", "deny", ("--locked", "check"))])
        pinned, reasons = gate_commands.classify_command("cargo +stable deny --locked check")
        self.assertEqual(reasons, [])
        self.assertEqual((pinned[0].name, pinned[0].args, pinned[0].toolchain),
                         ("deny", ("--locked", "check"), "+stable"))
        for spelling, reason in {
            "cargo deny --locked check | tee deny.log": "beside `|`",
            'echo "$(cargo deny --locked check)"': "a gate inside a command substitution",
            "env cargo deny --locked check": "a gate after `env`",
            "cargo --locked deny check": "a cargo option before the gate subcommand",
        }.items():
            with self.subTest(spelling=spelling):
                _invocations, reasons = gate_commands.classify_command(spelling)
                self.assertTrue(any(reason in text for text in reasons), reasons)

    def test_a_toolchain_override_is_still_a_cargo_test(self):
        # Protect the missing-local-test rule from `cargo +toolchain test`, which the plain
        # spelling would not match.
        probe = "cargo +stable test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture"
        setup = gate_ci_catalog.CiOnly("setup", probe, ("macos-core",), "fixture")
        problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(setup,))
        self.assertTrue(any("missing local test" in problem for problem in problems), problems)
        missing = gate_ci_catalog.CiOnly(
            "evidence-rerun",
            "cargo +nightly test -p sonicterm-gpu --test no_such_target -- --nocapture",
            ("macos-core",),
            "fixture",
        )
        problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(missing,))
        self.assertTrue(any("no_such_target.rs does not exist" in problem for problem in problems), problems)
        # A rerun that widens the run, such as with --ignored, is not the workspace pass.
        widened = gate_ci_catalog.CiOnly("evidence-rerun", probe + " --ignored", ("macos-core",), "fixture")
        problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(widened,))
        self.assertTrue(any("must be exactly" in problem for problem in problems), problems)
        mutated = gate_test_fixture.WORKFLOW.replace("run: cargo fmt --all --check", "run: cargo +stable fmt --all --check", 1)
        problems = gate_workflow.ci_parity_problems(mutated)
        self.assertTrue(any("`cargo +stable fmt --all --check`, which is neither a table step"
                            in problem for problem in problems), problems)

    def test_quoted_or_backslash_self_test_paths_cannot_be_ci_only(self):
        # Protect the self-test rule from quoting and path separators.
        for command in (
            'python3 "scripts/check-something_tests.py"',
            "bash scripts\\test-soak-harness.sh",
            'bash "scripts/test-soak-harness.sh"',
            '".\\scripts\\validate-windows-msi_tests.ps1"',
        ):
            with self.subTest(command=command):
                entry = gate_ci_catalog.CiOnly("runtime-evidence", command, ("windows-tests-workspace",), "fixture")
                problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(entry,))
                self.assertTrue(any("first-party self-test" in problem for problem in problems), problems)
        workflow = _with_step(
            "      - name: New self-test\n"
            "        timeout-minutes: 2\n"
            '        run: python3 "scripts/check-something_tests.py"\n\n'
        )
        problems = gate_workflow.ci_parity_problems(workflow)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("scripts/check-something_tests.py", problems[0])

    def test_every_command_of_a_compound_line_is_checked(self):
        # Protect proofs from covering only the first command: each command joined by `&&` or
        # `;` is classified and proved, and a compact compound still fails parity.
        probe = "cargo test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture"
        hidden = "cargo test -p sonicterm-gpu --test no_such_target -- --nocapture"
        for joiner in (" && ", "; ", ";"):
            with self.subTest(joiner=joiner):
                entry = gate_ci_catalog.CiOnly("evidence-rerun", probe + joiner + hidden, ("macos-core",), "fixture")
                problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(entry,))
                self.assertTrue(any("no_such_target.rs does not exist" in problem
                                    for problem in problems), problems)
                entry = gate_ci_catalog.CiOnly(
                    "runtime-evidence", probe + joiner + "bash scripts/test-soak-harness.sh",
                    ("macos-core",), "fixture",
                )
                problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(entry,))
                self.assertTrue(any("first-party self-test" in problem for problem in problems), problems)
        problems = gate_workflow.ci_parity_problems(_with_step(
            "      - name: Compact\n        timeout-minutes: 2\n        run: echo start;cargo test --workspace\n\n"
        ))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("neither a table step nor on the CI-only list", problems[0])

    def test_every_line_of_a_run_block_is_checked(self):
        # Protect multi-line blocks: a gate on a later line of a block that starts with a
        # table command is still checked.
        mutated = gate_test_fixture.WORKFLOW.replace(
            "        run: cargo fmt --all --check\n",
            "        run: |\n          cargo fmt --all --check\n          python3 scripts/check-something_tests.py\n",
            1,
        )
        self.assertNotEqual(mutated, gate_test_fixture.WORKFLOW)
        problems = gate_workflow.ci_parity_problems(mutated)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("`python3 scripts/check-something_tests.py`, which is neither", problems[0])

    def test_unmodeled_spellings_are_rejected(self):
        # Protect parity from gates the classifier cannot prove run with their exit status
        # seen: each spelling fails loudly instead of passing as a non-gate line.
        spellings = {
            "cargo test --workspace | tee test.log": "beside `|`",
            "cargo test --workspace || true": "beside `||`",
            "cargo test --workspace &": "beside `&`",
            "(cargo test --workspace)": "beside `(`",
            "cargo test --workspace > test.log": "a redirected gate",
            "env cargo test --workspace": "a gate after `env`",
            "time cargo test --workspace": "a gate after `time`",
            "cargo --locked test --workspace": "a cargo option before the gate subcommand",
            "bash -x scripts/check-something-new.sh": "a gate after `bash`",
            'echo "$(cargo test --workspace)"': "a gate inside a command substitution",
            "python3 -c \"import subprocess; subprocess.run(['cargo', 'test'])\"": "a gate after `python3`",
            "cargo test --workspace 'unterminated": "an unterminated single quote",
        }
        for spelling, reason in spellings.items():
            with self.subTest(spelling=spelling):
                _invocations, reasons = gate_commands.classify_command(spelling)
                self.assertTrue(any(reason in text for text in reasons), reasons)
                workflow = _with_step(
                    f"      - name: Unmodeled\n        timeout-minutes: 2\n        run: |\n          {spelling}\n\n"
                )
                problems = gate_workflow.ci_parity_problems(workflow)
                self.assertTrue(any("a spelling the gate classifier does not model" in problem
                                    for problem in problems), problems)
        entry = gate_ci_catalog.CiOnly(
            "runtime-evidence",
            "cargo test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture | tee probe.log",
            ("macos-core",),
            "fixture",
        )
        problems = gate_ci_checks.ci_only_problems(gate_test_fixture.ROOT, gate_test_fixture.WORKFLOW, entries=(entry,))
        self.assertTrue(any("beside `|`" in problem for problem in problems), problems)

    def test_an_expression_that_decides_what_runs_is_rejected(self):
        # Protect parity from a workflow expression that picks the gate: in the cargo
        # subcommand, a script argument, or the command after `--`, it is reported as
        # unmodeled, while an expression in an ordinary data argument stays supported.
        spellings = (
            "cargo ${{ 'test' }} --workspace",
            "cargo ${{ matrix.cmd }} --workspace",
            "cargo +stable ${{ env.CARGO_GATE }} --workspace",
            "cargo --locked ${{ matrix.cmd }} --workspace",
            "python3 ${{ 'scripts/new_tests.py' }}",
            "bash ${{ matrix.script }}",
            "python3 -u ${{ env.SCRIPT }}",
            '"$python_cmd" ${{ matrix.script }}',
            "python3 scripts/native-smoke-runner.py -- cargo ${{ 'test' }} --workspace",
            "python3 scripts/native-smoke-runner.py -- python3 ${{ matrix.script }}",
        )
        for spelling in spellings:
            with self.subTest(spelling=spelling):
                _invocations, reasons = gate_commands.classify_command(spelling)
                self.assertTrue(any("workflow expression" in reason for reason in reasons), reasons)
                problems = gate_workflow.ci_parity_problems(_with_step(
                    f"      - name: Expression\n        timeout-minutes: 2\n        run: |\n          {spelling}\n\n"
                ))
                self.assertTrue(any("a spelling the gate classifier does not model" in problem
                                    for problem in problems), problems)
        # The shipped smoke and package lines put expressions only in data arguments.
        for entry in gate_ci_catalog.CI_ONLY:
            if "${{" in entry.command:
                with self.subTest(entry=entry.command):
                    invocations, reasons = gate_commands.classify_command(entry.command)
                    self.assertEqual(reasons, [])
                    self.assertEqual(invocations[0].kind, "script")
