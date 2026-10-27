"""Perf workflow tests."""

from __future__ import annotations
import copy
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest

import perf_expression_fixture
import perf_workflow_fixture


# A fake command that records its argv under RUNNER_TEMP and succeeds.
RECORD_ARGV = '#!/usr/bin/env bash\nprintf \'%s\\n\' "$@" >"$RUNNER_TEMP/argv"\n'
# A fake command that must never run: reaching it fails the step.
UNEXPECTED_COMMAND = '#!/usr/bin/env bash\necho "unexpected $(basename "$0") $*" >&2\nexit 97\n'


# Windows skips every test that runs a workflow step through `run_bash_step`: a bare `bash` there can
# resolve to the WSL launcher rather than Git Bash, and the extensionless fake tools are not executable,
# so the step exits without output. macOS and Ubuntu CI run these tests.
def run_bash_step(script, environ, fakes=None):
    """Run a workflow `run:` block as GitHub does (`bash --noprofile --norc -eo pipefail`).

    `fakes` maps command names to scripts placed first on PATH. Returns the exit code, GITHUB_OUTPUT,
    GITHUB_ENV, stderr, the step summary and the argv a RECORD_ARGV fake saw.
    """
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        fake_bin = root / "bin"
        fake_bin.mkdir()
        for name, body in (fakes or {}).items():
            (fake_bin / name).write_text(body, encoding="utf-8")
            (fake_bin / name).chmod(0o755)
        script_path, outputs, env_file, summary = root / "step.sh", root / "outputs", root / "env", root / "summary"
        script_path.write_text(script, encoding="utf-8")
        for record in (outputs, env_file, summary):
            record.touch()
        full_environ = dict(os.environ, RUNNER_TEMP=str(root), GITHUB_OUTPUT=str(outputs), GITHUB_ENV=str(env_file),
                            GITHUB_STEP_SUMMARY=str(summary), PATH=f"{fake_bin}{os.pathsep}{os.environ['PATH']}")
        full_environ.update(environ)
        completed = subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", str(script_path)],
                                   env=full_environ, capture_output=True, text=True, timeout=30, check=False)
        argv_file = root / "argv"
        return SimpleNamespace(code=completed.returncode, outputs=perf_expression_fixture.read_key_values(outputs),
                               env=perf_expression_fixture.read_key_values(env_file), stderr=completed.stderr, root=root,
                               summary=summary.read_text(encoding="utf-8"),
                               argv=argv_file.read_text(encoding="utf-8").splitlines() if argv_file.exists() else [])


class PushSimulator:
    """Runs perf.yml's jobs for one event (a tag push by default) in file order; bash steps really run.

    Checkout, the toolchain and the cache restore succeed; an upload records its artifact name and a
    download fails unless that name was uploaded; pwsh steps succeed without running. Git is FAKE_GIT in
    `git_mode`; python, tar and shasum fail if any step reaches them, since a first release builds nothing.
    """

    RUNNER_OS = {"perf-build-macos": "macOS", "compare-macos": "macOS", "compare-windows": "Windows",
                 "perf-result": "Linux"}

    def __init__(self, workflow, root, git_mode, github=None):
        # Resolve this runtime-only dependency after module initialization.
        import perf_workflow_identity_tests
        self.workflow, self.root, self.git_mode = workflow, root, git_mode
        self.fake_bin = root / "bin"
        self.fake_bin.mkdir()
        for name, body in (("git", perf_workflow_identity_tests.FAKE_GIT), ("python3", UNEXPECTED_COMMAND), ("python", UNEXPECTED_COMMAND),
                           ("tar", UNEXPECTED_COMMAND), ("shasum", UNEXPECTED_COMMAND),
                           ("brew", "#!/usr/bin/env bash\nexit 0\n")):
            (self.fake_bin / name).write_text(body, encoding="utf-8")
            (self.fake_bin / name).chmod(0o755)
        self.github = github or {"event_name": "push", "run_id": "7", "run_attempt": "1", "sha": "tag-commit",
                                 "ref_name": "v1.4.0", "workspace": str(root / "workspace"), "event": {}}
        self.results, self.outputs, self.traces, self.artifacts, self.names = {}, {}, {}, {}, {}

    def run(self):
        """Run every job in file order; each job's needs have finished before it starts."""
        for job_id in self.workflow["jobs"]:
            self.run_job(job_id)
        return self

    def run_job(self, job_id):
        """Decide a job's `if:` from its needs' results, then run each matrix entry; any failed entry fails it."""
        job = self.workflow["jobs"][job_id]
        needs = job.get("needs", [])
        needs = [needs] if isinstance(needs, str) else needs
        context = {"github": self.github,
                   "needs": {name: {"result": self.results[name], "outputs": self.outputs[name]} for name in needs}}
        upstream_succeeded = all(self.results[name] == "success" for name in needs)
        # A matrix job's name reads its entry; every other job's name is the check's name, even when skipped.
        if "strategy" not in job:
            self.names[job_id] = perf_expression_fixture.substitute(job["name"], context)
        if not perf_expression_fixture.condition_holds(job.get("if"), context, upstream_succeeded):
            self.results[job_id], self.outputs[job_id], self.traces[job_id] = "skipped", {}, []
            return
        entries = (job.get("strategy") or {}).get("matrix", {}).get("include") or [{}]
        states, traces, outputs = [], [], {}
        for entry in entries:
            state, outputs, trace = self.run_entry(job_id, job, dict(context, matrix=entry))
            states.append(state)
            traces.append(trace)
        self.results[job_id] = "failure" if "failure" in states else "success"
        self.outputs[job_id], self.traces[job_id] = outputs, traces

    def run_entry(self, job_id, job, context):
        """Run one matrix entry's steps; return its result, its job outputs and (name, state, outputs) per step."""
        workspace = Path(tempfile.mkdtemp(dir=self.root))
        temp = workspace / "runner-temp"
        temp.mkdir()
        context = dict(context, runner={"os": self.RUNNER_OS[job_id], "temp": str(temp)})
        env = {}
        for scope in (self.workflow.get("env") or {}, job.get("env") or {}):
            env.update({name: perf_expression_fixture.substitute(value, context) for name, value in scope.items()})
        steps, trace, failed = {}, [], False
        for step in job["steps"]:
            name = step.get("name") or step["uses"].split("@")[0]
            step_context = dict(context, steps=steps)
            if not perf_expression_fixture.condition_holds(step.get("if"), step_context, not failed):
                trace.append((name, "skipped", {}))
                if "id" in step:
                    steps[step["id"]] = {"outputs": {}}
                continue
            succeeded, outputs = self.execute(step, step_context, env, workspace, temp)
            trace.append((name, "success" if succeeded else "failure", outputs))
            if "id" in step:
                steps[step["id"]] = {"outputs": outputs}
            failed = failed or not succeeded
        output_context = dict(context, steps=steps)
        outputs = {name: perf_expression_fixture.substitute(value, output_context) for name, value in (job.get("outputs") or {}).items()}
        return ("failure" if failed else "success"), outputs, trace

    def execute(self, step, context, env, workspace, temp):
        """Run one step; return whether it succeeded and its outputs."""
        if "uses" in step:
            action = step["uses"].split("@")[0]
            arguments = step.get("with") or {}
            if action == "actions/upload-artifact":
                self.artifacts[perf_expression_fixture.substitute(arguments["name"], context)] = True
                return True, {}
            if action == "actions/download-artifact":
                return perf_expression_fixture.substitute(arguments["name"], context) in self.artifacts, {}
            if action in ("actions/checkout", "dtolnay/rust-toolchain", "actions/cache/restore"):
                return True, {}
            raise AssertionError(f"the simulator does not model {action}")
        if step.get("shell") == "pwsh":
            return True, {}
        outputs, env_file, summary, script = temp / "output", temp / "env", temp / "summary", temp / "step.sh"
        for record in (outputs, env_file):
            record.write_text("", encoding="utf-8")
        summary.touch()
        script.write_text(perf_expression_fixture.substitute(step["run"], context), encoding="utf-8")
        environ = dict(env, **{name: perf_expression_fixture.substitute(value, context) for name, value in (step.get("env") or {}).items()})
        environ.update(PATH=f"{self.fake_bin}{os.pathsep}{os.environ['PATH']}", HOME=os.environ.get("HOME", ""),
                       GITHUB_OUTPUT=str(outputs), GITHUB_ENV=str(env_file), GITHUB_STEP_SUMMARY=str(summary),
                       RUNNER_TEMP=str(temp), RUNNER_OS=context["runner"]["os"], GITHUB_RUN_ID=self.github["run_id"],
                       GITHUB_RUN_ATTEMPT=self.github["run_attempt"], GITHUB_SHA=self.github["sha"],
                       FAKE_GIT_MODE=self.git_mode)
        completed = subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", str(script)], cwd=workspace,
                                   env=environ, capture_output=True, text=True, timeout=30, check=False)
        env.update(perf_expression_fixture.read_key_values(env_file))
        return completed.returncode == 0, perf_expression_fixture.read_key_values(outputs)


class WorkflowModelTests(unittest.TestCase):
    """The reader and evaluator the workflow tests rely on: they model only what perf.yml uses, and raise otherwise."""

    def test_the_reader_handles_the_forms_perf_yml_uses(self):
        # Nested mappings, a dash line that opens a mapping, flow lists, comments, `|` and `>-` blocks.
        document = perf_workflow_fixture.WorkflowYaml.parse("on:\n  push:\n    tags: [\"v[0-9]*\"]  # tags\njobs:\n  one:\n"
                                      "    needs: [a, b]\n    if: >-\n      x ==\n      'y'\n    steps:\n"
                                      "      - name: Step\n        run: |\n          echo 1  # kept\n\n"
                                      "          echo 2\n      - uses: actions/checkout@abc # v1\n")
        self.assertEqual(document["on"], {"push": {"tags": ["v[0-9]*"]}})
        job = document["jobs"]["one"]
        self.assertEqual((job["needs"], job["if"]), (["a", "b"], "x == 'y'"))
        self.assertEqual(job["steps"], [{"name": "Step", "run": "echo 1  # kept\n\necho 2\n"},
                                        {"uses": "actions/checkout@abc"}])
        with self.assertRaises(ValueError):
            perf_workflow_fixture.WorkflowYaml.parse("a: 1\n   b: 2\n")

    def test_the_evaluator_short_circuits_and_refuses_what_it_does_not_model(self):
        # A push makes the eligibility true without reading the pull request's labels; anything else raises.
        # Resolve this runtime-only dependency after module initialization.
        import perf_workflow_identity_tests
        context = {"github": {"event_name": "push"}}
        self.assertTrue(perf_expression_fixture.condition_holds(perf_workflow_fixture.ELIGIBILITY, context, True))
        self.assertFalse(perf_expression_fixture.condition_holds(perf_workflow_fixture.ELIGIBILITY, context, False))
        self.assertTrue(perf_expression_fixture.condition_holds(f"always() && ({perf_workflow_fixture.ELIGIBILITY})", context, False))
        labelled = {"github": perf_workflow_identity_tests.pull_request_github("labeled", ["perf"], "perf")["github"]}
        self.assertTrue(perf_expression_fixture.condition_holds(perf_workflow_fixture.ELIGIBILITY, labelled, True))
        with self.assertRaises(perf_expression_fixture.UnsupportedExpression):
            perf_expression_fixture.condition_holds("fromJSON('[]')", context, True)
        with self.assertRaises(perf_expression_fixture.UnsupportedExpression):
            perf_expression_fixture.condition_holds("needs.build.result == 'success'", context, True)
        with self.assertRaises(perf_expression_fixture.UnsupportedExpression):
            perf_expression_fixture.condition_holds("failure()", context, True)

    def test_the_whole_workflow_parses(self):
        # The four jobs, in the order their needs require.
        self.assertEqual(list(perf_workflow_fixture.load_perf_workflow()["jobs"]), list(perf_workflow_fixture.PERF_JOBS))


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash steps with fake tools")
class FirstReleaseWorkflowTests(unittest.TestCase):
    """A first release resolves no base: every job succeeds without building, transferring or comparing."""

    def simulate(self, workflow):
        with tempfile.TemporaryDirectory() as temp:
            return PushSimulator(workflow, Path(temp), "first").run()

    @staticmethod
    def states(simulator, job_id):
        """Each step's states across a job's matrix entries."""
        found = {}
        for trace in simulator.traces[job_id]:
            for name, state, _outputs in trace:
                found.setdefault(name, set()).add(state)
        return found

    def test_the_first_release_path_skips_every_build_transfer_and_comparison(self):
        # The producer resolves base='' and publishes no manifest; each macOS shard plans prebuilt=false; nothing
        # downloads or compares, and perf-result's real step passes on three successes.
        simulator = self.simulate(perf_workflow_fixture.load_perf_workflow())
        self.assertEqual(simulator.results, {job: "success" for job in perf_workflow_fixture.PERF_JOBS})
        producer_outputs = simulator.outputs["perf-build-macos"]
        self.assertEqual((producer_outputs["base"], producer_outputs["manifest_sha256"]), ("", ""))
        producer = self.states(simulator, "perf-build-macos")
        self.assertEqual(producer[perf_workflow_fixture.REF_STEP], {"success"})
        for name in (perf_workflow_fixture.BUILD_STEP, perf_workflow_fixture.PACKAGE_STEP, perf_workflow_fixture.UPLOAD_BINARIES_STEP, perf_workflow_fixture.BUILD_EVIDENCE_STEP):
            self.assertEqual(producer[name], {"skipped"}, name)
        macos = self.states(simulator, "compare-macos")
        self.assertEqual((macos[perf_workflow_fixture.REF_STEP], macos[perf_workflow_fixture.PLAN_STEP]), ({"success"}, {"success"}))
        for name in (perf_workflow_fixture.DOWNLOAD_STEP, perf_workflow_fixture.UNPACK_STEP, perf_workflow_fixture.COMPARE_STEP, perf_workflow_fixture.SUMMARY_STEP, perf_workflow_fixture.EVIDENCE_STEP):
            self.assertEqual(macos[name], {"skipped"}, name)
        plans = [outputs for trace in simulator.traces["compare-macos"]
                 for name, _state, outputs in trace if name == perf_workflow_fixture.PLAN_STEP]
        self.assertEqual(plans, [{"prebuilt": "false"}] * len(simulator.traces["compare-macos"]))
        windows = self.states(simulator, "compare-windows")
        for name in (perf_workflow_fixture.COMPARE_STEP, perf_workflow_fixture.SUMMARY_STEP, perf_workflow_fixture.EVIDENCE_STEP):
            self.assertEqual(windows[name], {"skipped"}, name)
        self.assertEqual(self.states(simulator, "perf-result")[perf_workflow_fixture.RESULT_STEP], {"success"})
        self.assertEqual(simulator.artifacts, {})

    def test_without_the_download_guard_a_first_release_fails(self):
        # The mutation the simulator must catch: an unguarded download reaches an artifact nobody uploaded.
        workflow = copy.deepcopy(perf_workflow_fixture.load_perf_workflow())
        del perf_expression_fixture.job_step(workflow, "compare-macos", perf_workflow_fixture.DOWNLOAD_STEP)["if"]
        simulator = self.simulate(workflow)
        self.assertEqual(self.states(simulator, "compare-macos")[perf_workflow_fixture.DOWNLOAD_STEP], {"failure"})
        self.assertEqual(simulator.results["compare-macos"], "failure")
        self.assertEqual(simulator.results["perf-result"], "failure")


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class PlanStepTests(unittest.TestCase):
    """compare-macos's gate: a shard measures the producer's binaries only for the refs it resolved itself."""

    def plan(self, producer_base, producer_head, manifest, base, head):
        """Run the plan step with its env evaluated from the producer's outputs and this shard's ref step."""
        step = perf_expression_fixture.job_step(perf_workflow_fixture.load_perf_workflow(), "compare-macos", perf_workflow_fixture.PLAN_STEP)
        context = {"needs": {"perf-build-macos": {"result": "success", "outputs": {
                       "base": producer_base, "head": producer_head, "manifest_sha256": manifest, "attempt": "1"}}},
                   "steps": {"refs": {"outputs": {"base": base, "head": head}}}, "github": {}}
        environ = {name: perf_expression_fixture.substitute(value, context) for name, value in step["env"].items()}
        return run_bash_step(step["run"], environ)

    def test_a_base_or_head_mismatch_fails(self):
        # A producer that resolved other refs (a moved merge base, say) built binaries this shard must not measure.
        for producer_base, producer_head in (("a" * 40, "c" * 40), ("b" * 40, "d" * 40), ("", "c" * 40)):
            with self.subTest(base=producer_base, head=producer_head):
                result = self.plan(producer_base, producer_head, "e" * 64, "b" * 40, "c" * 40)
                self.assertEqual(result.code, 1)
                self.assertIn("producer resolved", result.stderr)
                self.assertNotIn("prebuilt", result.outputs)

    def test_a_base_without_a_manifest_fails(self):
        # Agreeing refs with no manifest mean the producer skipped its build: nothing to measure.
        result = self.plan("b" * 40, "c" * 40, "", "b" * 40, "c" * 40)
        self.assertEqual(result.code, 1)
        self.assertIn("no manifest", result.stderr)

    def test_agreement_chooses_prebuilt(self):
        # Matching refs with a manifest measure the producer's binaries; a first release (no base) skips.
        self.assertEqual(self.plan("b" * 40, "c" * 40, "e" * 64, "b" * 40, "c" * 40).outputs, {"prebuilt": "true"})
        self.assertEqual(self.plan("", "", "", "", "").outputs, {"prebuilt": "false"})


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class PerfResultJobTests(unittest.TestCase):
    """The one stably named check: it passes only when the producer and both comparison jobs succeeded."""

    def job(self):
        return perf_workflow_fixture.load_perf_workflow()["jobs"]["perf-result"]

    def test_the_job_is_one_inline_step_with_no_checkout(self):
        # Nothing from the pull request's tree runs here, so the head cannot change what the check accepts.
        # Resolve this runtime-only dependency after module initialization.
        import perf_workflow_identity_tests
        job = self.job()
        self.assertEqual(" ".join(job["name"].split()),
                         f"${{{{ ({perf_workflow_fixture.ELIGIBILITY}) && '{perf_workflow_identity_tests.RESULT_NAME}' || '{perf_workflow_identity_tests.RESULT_NOT_RUN}' }}}}")
        self.assertEqual(job["runs-on"], "ubuntu-latest")
        self.assertEqual(job["needs"], ["perf-build-macos", "compare-macos", "compare-windows"])
        self.assertEqual(" ".join(job["if"].split()), f"always() && ({perf_workflow_fixture.ELIGIBILITY})")
        self.assertEqual(len(job["steps"]), 1)
        step = job["steps"][0]
        self.assertNotIn("uses", step)
        self.assertEqual(step["shell"], "bash")
        self.assertEqual(step["env"], {"PRODUCER": "${{ needs.perf-build-macos.result }}",
                                       "MACOS": "${{ needs.compare-macos.result }}",
                                       "WINDOWS": "${{ needs.compare-windows.result }}"})
        for forbidden in ("scripts/", ".github/", "checkout"):
            self.assertNotIn(forbidden, step["run"])

    def accepted(self, script):
        """The combinations of the three results the script exits 0 for."""
        accepted = []
        for producer in perf_workflow_fixture.JOB_RESULTS:
            for macos in perf_workflow_fixture.JOB_RESULTS:
                for windows in perf_workflow_fixture.JOB_RESULTS:
                    result = run_bash_step(script, {"PRODUCER": producer, "MACOS": macos, "WINDOWS": windows})
                    if result.code == 0:
                        accepted.append((producer, macos, windows))
        return accepted

    def test_only_three_successes_pass(self):
        # All 125 combinations of success, failure, cancelled, skipped and empty; a `|| true` mutation passes more.
        script = self.job()["steps"][0]["run"]
        self.assertEqual(self.accepted(script), [("success", "success", "success")])
        mutated = script.replace('= "success" ]', '= "success" ] || true', 1)
        self.assertNotEqual(mutated, script)
        self.assertGreater(len(self.accepted(mutated)), 1)


class EligibilityTests(unittest.TestCase):
    """Every job carries the same eligibility, so another label's run skips them all and reports nothing."""

    def test_every_job_has_the_eligibility_verbatim(self):
        # compare-macos also needs the producer's success, implicitly: it must not run after a failed producer.
        jobs = perf_workflow_fixture.load_perf_workflow()["jobs"]
        for job_id in ("perf-build-macos", "compare-macos", "compare-windows"):
            self.assertEqual(" ".join(jobs[job_id]["if"].split()), perf_workflow_fixture.ELIGIBILITY, job_id)
        self.assertNotIn("always()", jobs["compare-macos"]["if"])
        self.assertEqual(jobs["compare-macos"]["needs"], "perf-build-macos")
        self.assertEqual(" ".join(jobs["perf-result"]["if"].split()), f"always() && ({perf_workflow_fixture.ELIGIBILITY})")

    def test_no_job_keeps_its_own_concurrency_group(self):
        # The workflow-level group cancels a whole superseded eligible run, so per-job groups would be redundant.
        for job_id, job in perf_workflow_fixture.load_perf_workflow()["jobs"].items():
            self.assertNotIn("concurrency", job, job_id)
