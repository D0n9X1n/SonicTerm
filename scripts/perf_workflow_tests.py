"""Perf workflow tests."""

from __future__ import annotations
import copy
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest

import perf_expression_fixture
import perf_shards
import perf_workflow_fixture


# A fake command that records its argv under RUNNER_TEMP and succeeds.
RECORD_ARGV = '#!/usr/bin/env bash\nprintf \'%s\\n\' "$@" >"$RUNNER_TEMP/argv"\n'
# A fake command that must never run: reaching it fails the step.
UNEXPECTED_COMMAND = '#!/usr/bin/env bash\necho "unexpected $(basename "$0") $*" >&2\nexit 97\n'
# The simulator's python3 and python: the plan's partition script runs for real from this checkout (or fails when
# FAKE_SHARDS=fail); perf-compare.py records the release LTO it inherited and its argv per runner and partition,
# or, with FAKE_COMPARE=refuse, fails like any command a first release must never reach.
FAKE_PYTHON = """#!/usr/bin/env bash
case "$1" in
  */perf_shards.py)
    if [ "$FAKE_SHARDS" = fail ]; then echo "perf-shards: broken" >&2; exit 1; fi
    shift
    exec "$REAL_PYTHON" "$SCRIPTS_DIR/perf_shards.py" "$@" ;;
  */perf-compare.py)
    if [ "$FAKE_COMPARE" = refuse ]; then echo "unexpected perf-compare.py $*" >&2; exit 97; fi
    shift
    { printf '%s\\n' "${CARGO_PROFILE_RELEASE_LTO-}"; printf '%s\\n' "$@"; } >"$COMPARE_RECORDS/$RUNNER_OS-$PERF_SHARD" ;;
  *) echo "unexpected python $*" >&2; exit 97 ;;
esac
"""


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

    Checkout, the toolchain and the cache restore succeed; an upload records its artifact name; pwsh steps succeed
    without running. Git is FAKE_GIT in `git_mode`, brew succeeds, and python is FAKE_PYTHON: the plan's partition
    script really runs, and each comparison's argv is kept in `compares` under (platform, partition) unless
    `compare` is "refuse". A matrix that reads the plan's output is expanded from that output, as GitHub does.
    """

    RUNNER_OS = {"plan": "Linux", "compare-macos": "macOS", "compare-windows": "Windows", "perf-result": "Linux"}

    def __init__(self, workflow, root, git_mode, github=None, compare="record", shards="real"):
        self.workflow, self.root, self.git_mode = workflow, root, git_mode
        self.compare_mode, self.shards_mode = compare, shards
        self.fake_bin = root / "bin"
        self.fake_bin.mkdir()
        self.records = root / "compare-records"
        self.records.mkdir()
        for name, body in (("git", perf_workflow_fixture.FAKE_GIT), ("python3", FAKE_PYTHON), ("python", FAKE_PYTHON),
                           ("brew", "#!/usr/bin/env bash\nexit 0\n")):
            (self.fake_bin / name).write_text(body, encoding="utf-8")
            (self.fake_bin / name).chmod(0o755)
        self.github = github or {"event_name": "push", "run_id": "7", "run_attempt": "1", "sha": "tag-commit",
                                 "ref_name": "v1.4.0", "workspace": str(root / "workspace"), "event": {}}
        self.results, self.outputs, self.traces, self.artifacts, self.names = {}, {}, {}, {}, {}
        self.compares = {}

    def run(self):
        """Run every job in file order; each job's needs have finished before it starts."""
        for job_id in self.workflow["jobs"]:
            self.run_job(job_id)
        # Read before the caller's temporary directory goes: (lto, argv without the script) per partition.
        for record in sorted(self.records.iterdir()):
            platform_name, partition = record.name.split("-", 1)
            lines = record.read_text(encoding="utf-8").splitlines()
            self.compares[(platform_name, partition)] = (lines[0], lines[1:])
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
        include = (job.get("strategy") or {}).get("matrix", {}).get("include")
        # When: the include list is an expression, it is the plan's JSON output, parsed as GitHub parses it.
        entries = perf_expression_fixture.evaluate_value(include, context) if isinstance(include, str) else include
        states, traces, outputs = [], [], {}
        for entry in entries or [{}]:
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
                       FAKE_GIT_MODE=self.git_mode, REAL_PYTHON=sys.executable,
                       SCRIPTS_DIR=str(Path(__file__).resolve().parent), COMPARE_RECORDS=str(self.records),
                       FAKE_COMPARE=self.compare_mode, FAKE_SHARDS=self.shards_mode)
        completed = subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", str(script)], cwd=workspace,
                                   env=environ, capture_output=True, text=True, timeout=30, check=False)
        env.update(perf_expression_fixture.read_key_values(env_file))
        return completed.returncode == 0, perf_expression_fixture.read_key_values(outputs)


def step_states(simulator, job_id):
    """Each step's states across a job's matrix entries."""
    found = {}
    for trace in simulator.traces[job_id]:
        for name, state, _outputs in trace:
            found.setdefault(name, set()).add(state)
    return found


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
        context = {"github": {"event_name": "push"}}
        eligibility = perf_workflow_fixture.ELIGIBILITY
        self.assertTrue(perf_expression_fixture.condition_holds(eligibility, context, True))
        self.assertFalse(perf_expression_fixture.condition_holds(eligibility, context, False))
        self.assertTrue(perf_expression_fixture.condition_holds(f"always() && ({eligibility})", context, False))
        labelled = perf_workflow_fixture.pull_request_github("labeled", ["perf-full"], "perf-full")
        self.assertTrue(perf_expression_fixture.condition_holds(eligibility, labelled, True))
        with self.assertRaises(perf_expression_fixture.UnsupportedExpression):
            perf_expression_fixture.condition_holds("startsWith(github.event_name, 'p')", context, True)
        with self.assertRaises(perf_expression_fixture.UnsupportedExpression):
            perf_expression_fixture.condition_holds("needs.build.result == 'success'", context, True)
        with self.assertRaises(perf_expression_fixture.UnsupportedExpression):
            perf_expression_fixture.condition_holds("failure()", context, True)

    def test_a_matrix_read_from_a_job_output_is_a_list(self):
        # fromJSON of the plan's text output is the parsed list, as GitHub expands a matrix's include.
        context = {"needs": {"plan": {"outputs": {"macos": '[{"partition": "core-1"}]'}}}}
        self.assertEqual(perf_expression_fixture.evaluate_value("${{ fromJSON(needs.plan.outputs.macos) }}", context),
                         [{"partition": "core-1"}])
        with self.assertRaises(perf_expression_fixture.UnsupportedExpression):
            perf_expression_fixture.evaluate_value("a ${{ github.sha }}", {"github": {"sha": "x"}})

    def test_the_whole_workflow_parses(self):
        # The four jobs, in the order their needs require.
        self.assertEqual(list(perf_workflow_fixture.load_perf_workflow()["jobs"]), list(perf_workflow_fixture.PERF_JOBS))


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash steps with fake tools")
class FirstReleaseWorkflowTests(unittest.TestCase):
    """A first release resolves no base: every job succeeds without building or comparing."""

    def simulate(self, workflow, **options):
        with tempfile.TemporaryDirectory() as temp:
            return PushSimulator(workflow, Path(temp), "first", **options).run()

    def test_the_first_release_path_skips_every_comparison(self):
        # The plan resolves base='' and still emits the release matrices; each partition skips its comparison,
        # nothing is uploaded, and perf-result's real step passes on three successes.
        simulator = self.simulate(perf_workflow_fixture.load_perf_workflow(), compare="refuse")
        self.assertEqual(simulator.results, {job: "success" for job in perf_workflow_fixture.PERF_JOBS})
        self.assertEqual((simulator.outputs["plan"]["base"], simulator.outputs["plan"]["profile"]), ("", "release"))
        release = perf_shards.load_profiles()["release"]
        for job_id, platform_name in perf_workflow_fixture.COMPARISON_PLATFORMS.items():
            with self.subTest(job=job_id):
                self.assertEqual(len(simulator.traces[job_id]), len(release.partitions[platform_name]))
                states = step_states(simulator, job_id)
                for name in (perf_workflow_fixture.COMPARE_STEP, perf_workflow_fixture.SUMMARY_STEP,
                             perf_workflow_fixture.EVIDENCE_STEP):
                    self.assertEqual(states[name], {"skipped"}, name)
        self.assertEqual(step_states(simulator, "perf-result")[perf_workflow_fixture.RESULT_STEP], {"success"})
        self.assertEqual((simulator.artifacts, simulator.compares), ({}, {}))

    def test_without_the_base_guard_a_first_release_fails(self):
        # The mutation the simulator must catch: an unguarded comparison runs with no base and fails the result.
        workflow = copy.deepcopy(perf_workflow_fixture.load_perf_workflow())
        del perf_expression_fixture.job_step(workflow, "compare-macos", perf_workflow_fixture.COMPARE_STEP)["if"]
        simulator = self.simulate(workflow, compare="refuse")
        self.assertEqual(step_states(simulator, "compare-macos")[perf_workflow_fixture.COMPARE_STEP], {"failure"})
        self.assertEqual((simulator.results["compare-macos"], simulator.results["perf-result"]), ("failure", "failure"))

    def test_a_plan_that_fails_skips_every_comparison_and_fails_the_result(self):
        # A partition file that breaks a rule fails the plan: nothing builds, and the result never reads success.
        simulator = self.simulate(perf_workflow_fixture.load_perf_workflow(), shards="fail")
        self.assertEqual(simulator.results, {"plan": "failure", "compare-macos": "skipped",
                                             "compare-windows": "skipped", "perf-result": "failure"})


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash steps with fake tools")
class ComparisonRunTests(unittest.TestCase):
    """Each event's plan chooses a profile, and every partition compares the plan's SHAs with its own selection."""

    def simulate(self, git_mode, github=None):
        with tempfile.TemporaryDirectory() as temp:
            return PushSimulator(perf_workflow_fixture.load_perf_workflow(), Path(temp), git_mode, github=github).run()

    def check(self, simulator, profile_name, base, head, short, lto):
        """Every partition of the profile ran once, on the plan's refs, with its selection and the run length."""
        self.assertEqual(simulator.results, {job: "success" for job in perf_workflow_fixture.PERF_JOBS})
        self.assertEqual(simulator.outputs["plan"]["profile"], profile_name)
        profile = perf_shards.load_profiles()[profile_name]
        expected = {}
        for platform_name in perf_shards.PLATFORMS:
            for partition in profile.partitions[platform_name]:
                argv = ["--base", base, "--head", head, "--require-base",
                        *perf_shards.partition_arguments(profile, partition), *(["--short"] if short else []), "--out"]
                expected[(platform_name, partition.name)] = (lto, argv)
        found = {key: (recorded_lto, argv[:-1]) for key, (recorded_lto, argv) in simulator.compares.items()}
        self.assertEqual(found, expected)
        for _lto, argv in simulator.compares.values():
            self.assertTrue(argv[-1].endswith("perf-comparison"), argv[-1])

    def test_a_perf_pull_request_runs_the_core_partitions_short_on_the_relaxed_profile(self):
        # The merge base against the pull request's head, `--short`, LTO off in every partition.
        github = perf_workflow_fixture.pull_request_github("labeled", ["perf"], "perf")["github"]
        self.check(self.simulate("previous", github), "core", "merge-base-commit", perf_workflow_fixture.PR_HEAD_SHA,
                   short=True, lto="off")

    def test_a_perf_full_pull_request_runs_the_full_partitions(self):
        # perf-full chooses `full`, also when `perf` is set too.
        github = perf_workflow_fixture.pull_request_github("labeled", ["perf", "perf-full"], "perf-full")["github"]
        self.check(self.simulate("previous", github), "full", "merge-base-commit", perf_workflow_fixture.PR_HEAD_SHA,
                   short=True, lto="off")

    def test_a_release_tag_runs_the_release_partitions_at_full_length_on_the_shipping_profile(self):
        # The previous release tag's commit against the tag, no `--short`, and no release-profile override.
        self.check(self.simulate("previous"), "release", "base-commit", "tag-commit", short=False, lto="")


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class PerfResultJobTests(unittest.TestCase):
    """The one stably named check: it passes only when the plan and both comparison jobs succeeded."""

    def job(self):
        return perf_workflow_fixture.load_perf_workflow()["jobs"]["perf-result"]

    def test_the_job_is_one_inline_step_with_no_checkout(self):
        # Nothing from the pull request's tree runs here, so the head cannot change what the check accepts.
        job = self.job()
        self.assertEqual(" ".join(job["name"].split()),
                         f"${{{{ ({perf_workflow_fixture.ELIGIBILITY}) && '{perf_workflow_fixture.RESULT_NAME}' || "
                         f"'{perf_workflow_fixture.RESULT_NOT_RUN}' }}}}")
        self.assertEqual(job["runs-on"], "ubuntu-latest")
        self.assertEqual(job["needs"], ["plan", *perf_workflow_fixture.COMPARISON_JOBS])
        self.assertEqual(" ".join(job["if"].split()), f"always() && ({perf_workflow_fixture.ELIGIBILITY})")
        self.assertEqual(len(job["steps"]), 1)
        step = job["steps"][0]
        self.assertNotIn("uses", step)
        self.assertEqual(step["shell"], "bash")
        self.assertEqual(step["env"], {"PLAN": "${{ needs.plan.result }}",
                                       "MACOS": "${{ needs.compare-macos.result }}",
                                       "WINDOWS": "${{ needs.compare-windows.result }}"})
        for forbidden in ("scripts/", ".github/", "checkout"):
            self.assertNotIn(forbidden, step["run"])

    def accepted(self, script):
        """The combinations of the three results the script exits 0 for."""
        accepted = []
        for plan in perf_workflow_fixture.JOB_RESULTS:
            for macos in perf_workflow_fixture.JOB_RESULTS:
                for windows in perf_workflow_fixture.JOB_RESULTS:
                    result = run_bash_step(script, {"PLAN": plan, "MACOS": macos, "WINDOWS": windows})
                    if result.code == 0:
                        accepted.append((plan, macos, windows))
        return accepted

    def test_only_three_successes_pass(self):
        # All 125 combinations of success, failure, cancelled, skipped and empty, so a skipped plan never passes;
        # a `|| true` mutation passes more.
        script = self.job()["steps"][0]["run"]
        self.assertEqual(self.accepted(script), [("success", "success", "success")])
        mutated = script.replace('= "success" ]', '= "success" ] || true', 1)
        self.assertNotEqual(mutated, script)
        self.assertGreater(len(self.accepted(mutated)), 1)


class EligibilityTests(unittest.TestCase):
    """The plan and the result carry the eligibility; the comparison jobs inherit it through the plan."""

    def test_the_plan_and_the_result_carry_the_eligibility_verbatim(self):
        # A comparison job has no `if:` and needs the plan, so it never runs after a skipped or failed plan; the
        # result job reads the eligibility itself, so a skipped plan cannot pass it.
        jobs = perf_workflow_fixture.load_perf_workflow()["jobs"]
        self.assertEqual(" ".join(jobs["plan"]["if"].split()), perf_workflow_fixture.ELIGIBILITY)
        for job_id in perf_workflow_fixture.COMPARISON_JOBS:
            self.assertNotIn("if", jobs[job_id], job_id)
            self.assertEqual(jobs[job_id]["needs"], "plan", job_id)
        self.assertEqual(" ".join(jobs["perf-result"]["if"].split()), f"always() && ({perf_workflow_fixture.ELIGIBILITY})")

    def test_no_job_keeps_its_own_concurrency_group(self):
        # The workflow-level group cancels a whole superseded eligible run, so per-job groups would be redundant.
        for job_id, job in perf_workflow_fixture.load_perf_workflow()["jobs"].items():
            self.assertNotIn("concurrency", job, job_id)
