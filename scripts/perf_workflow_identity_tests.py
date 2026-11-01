"""Perf workflow identity tests."""

from __future__ import annotations
import os
import re
import shutil
from pathlib import Path
import tempfile
import unittest

import perf_cell_layout
import perf_execution
import perf_expression_fixture
import perf_workflow_fixture


def pull_request_github(action, labels, label=None, run_id="70"):
    """A pull_request event's context: its action, the labels the PR carries and the label just added."""
    event = {"action": action, "pull_request": {"number": 1575, "labels": [{"name": name} for name in labels]}}
    if label is not None:
        event["label"] = {"name": label}
    return {"github": {"event_name": "pull_request", "event": event, "run_id": run_id, "ref_name": "1575/merge",
                       "run_attempt": "1", "sha": "merge-commit", "workspace": "/w"}}


def push_github(run_id="71"):
    """A tag push's context."""
    return {"github": {"event_name": "push", "event": {}, "run_id": run_id, "ref_name": "v1.4.0",
                       "run_attempt": "1", "sha": "tag-commit", "workspace": "/w"}}


RESULT_NAME = "Performance comparison result"
RESULT_NOT_RUN = "Performance comparison result (not run)"
# (context, eligible): a tag push; perf added; a push while perf is set; another label on a perf PR; a PR
# without perf; perf added to a PR that also carries bug.
ELIGIBILITY_CASES = (
    (push_github(), True),
    (pull_request_github("labeled", ["perf"], "perf"), True),
    (pull_request_github("synchronize", ["perf", "bug"]), True),
    (pull_request_github("labeled", ["perf", "bug"], "bug"), False),
    (pull_request_github("opened", ["bug"]), False),
    (pull_request_github("synchronize", []), False),
)


class ResultIdentityTests(unittest.TestCase):
    """Only an eligible run publishes the real result name, and only eligible runs share a concurrency group."""

    def test_only_an_eligible_run_publishes_the_result_name(self):
        # An ineligible run's skipped result job reads "(not run)", so it can never hide an eligible run's check.
        workflow = perf_workflow_fixture.load_perf_workflow()
        job = workflow["jobs"]["perf-result"]
        for context, eligible in ELIGIBILITY_CASES:
            with self.subTest(event=context["github"]["event"].get("action", "push"), eligible=eligible):
                self.assertEqual(perf_expression_fixture.substitute(job["name"], context), RESULT_NAME if eligible else RESULT_NOT_RUN)
                self.assertEqual(perf_expression_fixture.condition_holds(job["if"], context, False), eligible)

    def concurrency(self, context):
        """The workflow-level group and cancel-in-progress for one event."""
        block = perf_workflow_fixture.load_perf_workflow()["concurrency"]
        return perf_expression_fixture.substitute(block["group"], context), perf_expression_fixture.substitute(block["cancel-in-progress"], context)

    def test_eligible_runs_share_a_group_and_ineligible_runs_cancel_nothing(self):
        # Two eligible runs of one PR share a cancelling group; each ineligible run has a group of its own.
        first = self.concurrency(pull_request_github("labeled", ["perf"], "perf", run_id="70"))
        newer = self.concurrency(pull_request_github("synchronize", ["perf"], run_id="80"))
        self.assertEqual(first, ("perf-comparison-1575", "true"))
        self.assertEqual(newer, first)
        ineligible = [self.concurrency(pull_request_github("labeled", ["perf", "bug"], "bug", run_id=str(run_id)))
                      for run_id in (81, 82)]
        self.assertEqual([group for group, _cancel in ineligible], ["perf-ineligible-81", "perf-ineligible-82"])
        self.assertNotIn(first[0], [group for group, _cancel in ineligible])
        self.assertEqual(self.concurrency(push_github()), ("perf-comparison-v1.4.0", "false"))

    def test_a_skipped_duplicate_run_is_all_skipped_under_its_own_name(self):
        # Another label on a perf PR starts a run whose every job skips; its result check reads "(not run)".
        # Resolve this runtime-only dependency after module initialization.
        import perf_workflow_tests
        with tempfile.TemporaryDirectory() as temp:
            context = pull_request_github("labeled", ["perf", "bug"], "bug")
            simulator = perf_workflow_tests.PushSimulator(perf_workflow_fixture.load_perf_workflow(), Path(temp), "first", github=context["github"]).run()
        self.assertEqual(simulator.results, {job: "skipped" for job in perf_workflow_fixture.PERF_JOBS})
        self.assertEqual(simulator.names["perf-result"], RESULT_NOT_RUN)
        self.assertEqual(self.concurrency(context)[0], "perf-ineligible-70")

    @unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the result job's bash step")
    def test_a_superseded_eligible_run_never_reports_success(self):
        # A newer eligible run cancels this one: its result job still runs (always()), under the real name, and fails.
        # Resolve this runtime-only dependency after module initialization.
        import perf_workflow_tests
        workflow = perf_workflow_fixture.load_perf_workflow()
        job = workflow["jobs"]["perf-result"]
        context = pull_request_github("labeled", ["perf"], "perf")
        self.assertTrue(perf_expression_fixture.condition_holds(job["if"], context, False))
        self.assertEqual(perf_expression_fixture.substitute(job["name"], context), RESULT_NAME)
        for results in (("cancelled", "cancelled", "cancelled"), ("success", "cancelled", "success"),
                        ("success", "skipped", "cancelled")):
            with self.subTest(results=results):
                environ = dict(zip(("PRODUCER", "MACOS", "WINDOWS"), results))
                self.assertEqual(perf_workflow_tests.run_bash_step(job["steps"][0]["run"], environ).code, 1)


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class ProfileStepTests(unittest.TestCase):
    """The producer and both comparison jobs set the same release profile, which the manifest then binds."""

    def test_every_job_sets_the_same_profile(self):
        # Byte-for-byte the same step, writing the same GITHUB_ENV on a pull request and nothing on a push.
        # Resolve this runtime-only dependency after module initialization.
        import perf_workflow_tests
        workflow = perf_workflow_fixture.load_perf_workflow()
        steps = [perf_expression_fixture.job_step(workflow, job_id, perf_workflow_fixture.PROFILE_STEP) for job_id in ("perf-build-macos", *perf_workflow_fixture.COMPARISON_JOBS)]
        self.assertEqual(steps[1:], steps[:1] * 2)
        for event, expected in (("pull_request", {"CARGO_PROFILE_RELEASE_LTO": "off",
                                                  "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16"}), ("push", None)):
            with self.subTest(event=event):
                holds = perf_expression_fixture.condition_holds(steps[0]["if"], {"github": {"event_name": event}}, True)
                self.assertEqual(holds, expected is not None)
                if expected:
                    self.assertEqual(perf_workflow_tests.run_bash_step(steps[0]["run"], {}).env, expected)


def comparison_context(job_id, run_length, attempt="2", manifest="d" * 64):
    """A pull request's context for a comparison step: the producer of `attempt` published `manifest`."""
    platform_name = "Windows" if job_id == "compare-windows" else "macOS"
    return {"github": {"event_name": "pull_request", "run_id": "7", "run_attempt": "3"},
            "matrix": {"platform": platform_name, "shard": "S2-S10sync", "scenarios": "S2 S10/sync"},
            "steps": {"refs": {"outputs": {"base": "b" * 40, "head": "c" * 40, "length": run_length}},
                      "plan": {"outputs": {"prebuilt": "true"}}},
            "needs": {"perf-build-macos": {"result": "success",
                                           "outputs": {"attempt": attempt, "manifest_sha256": manifest}}},
            "runner": {"os": platform_name, "temp": "/runner/temp"}}


def compare_argv(job_id, run_length):
    """Run a comparison job's compare step with fake pythons; return the argv perf-compare.py received."""
    # Resolve this runtime-only dependency after module initialization.
    import perf_workflow_tests
    step = perf_expression_fixture.job_step(perf_workflow_fixture.load_perf_workflow(), job_id, perf_workflow_fixture.COMPARE_STEP)
    context = comparison_context(job_id, run_length)
    environ = {name: perf_expression_fixture.substitute(value, context) for name, value in step["env"].items()}
    result = perf_workflow_tests.run_bash_step(step["run"], dict(environ, GITHUB_RUN_ID="7"),
                           fakes={"python3": perf_workflow_tests.RECORD_ARGV, "python": perf_workflow_tests.RECORD_ARGV})
    if result.code != 0:
        raise AssertionError(result.stderr)
    return [argument.replace(str(result.root), "$RUNNER_TEMP") for argument in result.argv]


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with a fake python3")
class CountersWorkflowTests(unittest.TestCase):
    """Each comparison job's step: which run and counters options each mode passes."""

    def test_a_pull_request_runs_two_counters_runs_and_a_release_its_full_count(self):
        # The PR budget allows two counters runs per scenario; a release takes --runs. Both are strict.
        for job_id in perf_workflow_fixture.COMPARISON_JOBS:
            with self.subTest(job=job_id):
                pull_request = compare_argv(job_id, "short")
                self.assertIn("--short", pull_request)
                self.assertIn("--counters", pull_request)
                self.assertEqual(pull_request[pull_request.index("--counters-runs") + 1], "2")
                release = compare_argv(job_id, "full")
                self.assertIn("--counters", release)
                self.assertNotIn("--counters-runs", release)
                self.assertNotIn("--short", release)
                for argv in (pull_request, release):
                    self.assertIn("--require-base", argv)
                    self.assertEqual(argv[argv.index("--runs") + 1], "5")
                    self.assertEqual(argv[argv.index("--scenario") + 1:argv.index("--scenario") + 3],
                                     ["S2", "S10/sync"])


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with fake tools")
class PrebuiltDownloadTests(unittest.TestCase):
    """compare-macos measures exactly the producer attempt's artifact, bound by run, attempt and digest."""

    def test_the_download_names_the_producers_attempt(self):
        # The consumer's name uses the producer's recorded attempt, so a failed-job rerun still finds attempt n.
        workflow = perf_workflow_fixture.load_perf_workflow()
        context = comparison_context("compare-macos", "short", attempt="2")
        download = perf_expression_fixture.job_step(workflow, "compare-macos", perf_workflow_fixture.DOWNLOAD_STEP)
        self.assertEqual(download["uses"], "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c")
        self.assertEqual(perf_expression_fixture.substitute(download["with"]["name"], context), "perf-binaries-macOS-7-2")
        upload = perf_expression_fixture.job_step(workflow, "perf-build-macos", perf_workflow_fixture.UPLOAD_BINARIES_STEP)
        producer = {"github": {"run_id": "7", "run_attempt": "2"}, "runner": {"temp": "/t"}}
        self.assertEqual(perf_expression_fixture.substitute(upload["with"]["name"], producer), "perf-binaries-macOS-7-2")
        self.assertEqual((upload["with"]["retention-days"], upload["with"]["if-no-files-found"]), ("1", "error"))

    def test_every_artifact_step_is_gated_on_the_plan(self):
        # Download, unpack and compare run only when the plan chose prebuilt; summary and evidence also on failure.
        workflow = perf_workflow_fixture.load_perf_workflow()
        for name in (perf_workflow_fixture.DOWNLOAD_STEP, perf_workflow_fixture.UNPACK_STEP, perf_workflow_fixture.COMPARE_STEP):
            self.assertEqual(perf_expression_fixture.job_step(workflow, "compare-macos", name)["if"], "steps.plan.outputs.prebuilt == 'true'")
        for name in (perf_workflow_fixture.SUMMARY_STEP, perf_workflow_fixture.EVIDENCE_STEP):
            self.assertEqual(perf_expression_fixture.job_step(workflow, "compare-macos", name)["if"],
                             "always() && steps.plan.outputs.prebuilt == 'true'")

    def test_the_compare_step_binds_the_producer(self):
        # --require-base plus the prebuilt directory, this run's id, the producer's attempt and its manifest digest.
        argv = compare_argv("compare-macos", "short")
        self.assertIn("--require-base", argv)
        self.assertEqual(argv[argv.index("--prebuilt") + 1], "$RUNNER_TEMP/perf-binaries")
        self.assertEqual(argv[argv.index("--prebuilt-run-id") + 1], "7")
        self.assertEqual(argv[argv.index("--prebuilt-attempt") + 1], "2")
        self.assertEqual(argv[argv.index("--prebuilt-manifest-sha256") + 1], "d" * 64)
        self.assertNotIn("--prebuilt", compare_argv("compare-windows", "short"))


# The producer's fake perf-compare: it writes manifest.json under --build-only and prints a digest, the file's
# own unless FAKE_DIGEST says otherwise.
FAKE_PRODUCER = """#!/usr/bin/env bash
printf '%s\\n' "$@" >"$RUNNER_TEMP/argv"
while [ $# -gt 0 ]; do
  if [ "$1" = "--build-only" ]; then directory=$2; fi
  shift
done
mkdir -p "$directory"
printf '{"schema_version": 1}\\n' >"$directory/manifest.json"
if [ "$FAKE_DIGEST" = "wrong" ]; then
  echo "manifest_sha256=$(printf '0%.0s' $(seq 64))"
else
  echo "manifest_sha256=$(shasum -a 256 "$directory/manifest.json" | cut -d ' ' -f 1)"
fi
"""


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None or shutil.which("shasum") is None,
                 "runs the producer's bash step with fake tools")
class ProducerBuildStepTests(unittest.TestCase):
    """perf-build-macos publishes the digest of the manifest file it built, never just what the script printed."""

    def build(self, digest_mode):
        # Resolve this runtime-only dependency after module initialization.
        import perf_workflow_tests
        step = perf_expression_fixture.job_step(perf_workflow_fixture.load_perf_workflow(), "perf-build-macos", perf_workflow_fixture.BUILD_STEP)
        context = {"steps": {"refs": {"outputs": {"base": "b" * 40, "head": "c" * 40}}}, "github": {}}
        environ = {name: perf_expression_fixture.substitute(value, context) for name, value in step["env"].items()}
        return perf_workflow_tests.run_bash_step(step["run"], dict(environ, FAKE_DIGEST=digest_mode), fakes={"python3": FAKE_PRODUCER})

    def test_the_output_is_the_manifest_files_digest(self):
        # A strict build-only run of both refs, whose printed digest matches manifest.json.
        result = self.build("right")
        self.assertEqual(result.code, 0, result.stderr)
        self.assertRegex(result.outputs["manifest_sha256"], r"^[0-9a-f]{64}$")
        self.assertIn("--require-base", result.argv)
        self.assertEqual(result.argv[result.argv.index("--build-only") + 1], f"{result.root}/perf-binaries")

    def test_a_printed_digest_that_differs_from_the_file_fails(self):
        # The workflow recomputes the digest; a disagreement publishes nothing.
        result = self.build("wrong")
        self.assertNotEqual(result.code, 0)
        self.assertNotIn("manifest_sha256", result.outputs)


class EvidenceArtifactTests(unittest.TestCase):
    """The CI comparison's artifact is the only copy of its evidence once the runner is gone."""

    EVIDENCE_ROOT = "${{ runner.temp }}/perf-comparison"

    def upload_patterns(self, job_id):
        """Return a comparison job's evidence `path:` lines, each as (excluded, pattern relative to the evidence root)."""
        step = perf_expression_fixture.job_step(perf_workflow_fixture.load_perf_workflow(), job_id, perf_workflow_fixture.EVIDENCE_STEP)
        patterns = []
        for entry in step["with"]["path"].splitlines():
            entry = entry.strip()
            excluded = entry.startswith("!")
            entry = entry.lstrip("!")
            self.assertTrue(entry.startswith(self.EVIDENCE_ROOT), entry)
            patterns.append((excluded, entry[len(self.EVIDENCE_ROOT):].lstrip("/")))
        return patterns

    @staticmethod
    def matches(relative, pattern):
        """Match a path relative to the evidence root against an upload glob, `**` spanning directories."""
        if not pattern:
            return True
        expression = re.escape(pattern).replace(r"\*\*", "\0").replace(r"\*", "[^/]*").replace("\0", ".*")
        return re.fullmatch(f"{expression}(/.*)?", relative) is not None

    def test_every_evidence_name_carries_its_attempt(self):
        # A rerun's evidence never shares a name with the attempt before it.
        workflow = perf_workflow_fixture.load_perf_workflow()
        for job_id, name in (("compare-macos", perf_workflow_fixture.EVIDENCE_STEP), ("compare-windows", perf_workflow_fixture.EVIDENCE_STEP),
                             ("perf-build-macos", perf_workflow_fixture.BUILD_EVIDENCE_STEP)):
            with self.subTest(job=job_id):
                self.assertTrue(perf_expression_fixture.job_step(workflow, job_id, name)["with"]["name"].endswith("-${{ github.run_attempt }}"))

    def test_the_artifact_keeps_every_record_a_run_copies_from_its_scratch(self):
        # The workflow uploads the evidence tree; excluding a run's kept scratch would drop result.json,
        # progress.json, the App's logs and the checkpoint footprints, the raw data behind the CI-only table.
        with tempfile.TemporaryDirectory() as temp:
            scratch, evidence = Path(temp) / "scratch", Path(temp) / "evidence"
            for name in ("result.json", "progress.json", "harness.pid", "logs/sonicterm.log.2026-10-02",
                         "sessions/0.json", "acks/0", "checkpoints/0-end.json", "go/0", "workload/fixture.bin"):
                (scratch / name).parent.mkdir(parents=True, exist_ok=True)
                (scratch / name).write_text("x", encoding="utf-8")
            run = evidence / "runs" / "S1-default" / "timed" / "01-base"
            run.mkdir(parents=True)
            perf_execution._keep_scratch(scratch, run / "scratch")
            for name in ("comparison.md", "timing.json", "runs/S1-default/timed/01-base/outcome.json",
                         "runs/S1-default/timed/01-base/01-harness.log"):
                (evidence / name).parent.mkdir(parents=True, exist_ok=True)
                (evidence / name).write_text("x", encoding="utf-8")
            for job_id in perf_workflow_fixture.COMPARISON_JOBS:
                patterns = self.upload_patterns(job_id)
                archived = set()
                for file in evidence.rglob("*"):
                    if file.is_file():
                        relative = file.relative_to(evidence).as_posix()
                        included = any(self.matches(relative, pattern) for excluded, pattern in patterns
                                       if not excluded)
                        dropped = any(self.matches(relative, pattern) for excluded, pattern in patterns if excluded)
                        if included and not dropped:
                            archived.add(relative)
                kept = "runs/S1-default/timed/01-base/scratch/"
                for name in ("result.json", "progress.json", "logs/sonicterm.log.2026-10-02",
                             "checkpoints/0-end.json", "sessions/0.json"):
                    self.assertIn(kept + name, archived)
                for name in ("comparison.md", "timing.json", "runs/S1-default/timed/01-base/outcome.json",
                             "runs/S1-default/timed/01-base/01-harness.log"):
                    self.assertIn(name, archived)
                self.assertFalse(any("workload" in name for name in archived), archived)


FAKE_GIT = """#!/usr/bin/env bash
# Answers the release-ref step's Git calls for one mode: previous, first, root, broken or unreadable.
mode=$FAKE_GIT_MODE
case "$1" in
  rev-list)
    # `rev-list --parents -n 1 <sha>` prints the commit, then its parents.
    case "$mode" in
      root) echo tag-commit ;;
      unreadable) echo "fatal: unable to read commit object" >&2; exit 128 ;;
      *) echo "tag-commit parent-commit" ;;
    esac ;;
  rev-parse)
    if [[ "$*" == *"{commit}"* ]]; then echo base-commit; exit 0; fi
    if [ "$mode" = root ]; then exit 1; fi
    if [ "$mode" = unreadable ]; then echo "fatal: unable to read commit object" >&2; exit 128; fi
    echo parent-commit ;;
  tag)
    case "$mode" in
      previous) echo v1.3.8 ;;
      broken) echo "fatal: unable to read tree: object missing" >&2; exit 128 ;;
    esac ;;
  describe)
    if [ "$mode" = previous ]; then echo v1.3.8; else echo "fatal: No names found" >&2; exit 128; fi ;;
  *) echo "unexpected git $*" >&2; exit 2 ;;
esac
"""


@unittest.skipIf(os.name == "nt" or shutil.which("bash") is None, "runs the workflow's bash step with a fake git")
class ReleaseRefSelectionTests(unittest.TestCase):
    """The release mode's ref step: only a real first release may skip the comparison."""

    def step_script(self):
        """Return the `run:` block of the producer's ref-selection step."""
        return perf_expression_fixture.job_step(perf_workflow_fixture.load_perf_workflow(), "perf-build-macos", perf_workflow_fixture.REF_STEP)["run"]

    def test_every_job_resolves_its_refs_with_the_same_step(self):
        # A shard recomputes the refs itself and compares them with the producer's, so the steps must agree.
        workflow = perf_workflow_fixture.load_perf_workflow()
        steps = [perf_expression_fixture.job_step(workflow, job_id, perf_workflow_fixture.REF_STEP) for job_id in ("perf-build-macos", *perf_workflow_fixture.COMPARISON_JOBS)]
        self.assertEqual(steps[1:], steps[:1] * 2)
        self.assertEqual(steps[0]["id"], "refs")

    def run_step(self, mode):
        """Run the step as a tag push with a fake git; return its exit code, outputs and summary."""
        # Resolve this runtime-only dependency after module initialization.
        import perf_workflow_tests
        result = perf_workflow_tests.run_bash_step(self.step_script(), {"EVENT": "push", "GITHUB_SHA": "tag-commit",
                                                    "FAKE_GIT_MODE": mode}, fakes={"git": FAKE_GIT})
        outputs = "".join(f"{name}={value}\n" for name, value in result.outputs.items())
        return result.code, outputs, result.summary

    def test_a_release_with_an_earlier_tag_compares_against_it(self):
        # The previous release tag's commit is the base, the tag the head, at full length.
        code, outputs, _summary = self.run_step("previous")
        self.assertEqual(code, 0)
        self.assertEqual(outputs.split(), ["base=base-commit", "head=tag-commit", "length=full"])

    def test_only_a_real_first_release_skips_the_comparison(self):
        # An empty tag list, or a root commit, leaves `base` empty and says why in the summary.
        for mode, reason in (("first", "No earlier release tag"), ("root", "no parent")):
            with self.subTest(mode=mode):
                code, outputs, summary = self.run_step(mode)
                self.assertEqual((code, outputs.strip()), (0, "base="))
                self.assertIn(reason, summary)

    def test_a_failed_parent_lookup_fails_the_job(self):
        # An unreadable tagged commit is not a root commit: the step fails and writes no `base`.
        code, outputs, summary = self.run_step("unreadable")
        self.assertNotEqual(code, 0)
        self.assertNotIn("base=", outputs)
        self.assertNotIn("no parent", summary)

    def test_a_failed_tag_lookup_fails_the_job(self):
        # A Git error is not a first release: the step fails and writes no `base`, so nothing is skipped silently.
        code, outputs, summary = self.run_step("broken")
        self.assertNotEqual(code, 0)
        self.assertNotIn("base=", outputs)
        self.assertNotIn("No earlier release tag", summary)


class WindowsComparisonLegTests(unittest.TestCase):
    """perf.yml compares on Windows too: every scenario once per platform, on a runner with Cairo and Git Bash."""

    def matrix(self, job_id):
        return perf_workflow_fixture.load_perf_workflow()["jobs"][job_id]["strategy"]["matrix"]["include"]

    def test_every_scenario_runs_once_per_platform(self):
        # Each platform's shards partition the common scenario sets plus its own S11 variants. macOS runs three
        # shards, one per macOS slot that reaches perf beside CI's two macOS jobs; Windows keeps five.
        for job_id, platform_name, runner, counts in (("compare-macos", "macOS", "macos-15", (3,)),
                                                       ("compare-windows", "Windows", "windows-latest", (5,))):
            with self.subTest(job=job_id):
                entries = self.matrix(job_id)
                self.assertIn(len(entries), counts)
                self.assertEqual({(entry["platform"], entry["runner"]) for entry in entries},
                                 {(platform_name, runner)})
                scenarios = [scenario for entry in entries for scenario in entry["scenarios"].split()]
                self.assertEqual(sorted(scenarios), sorted(perf_workflow_fixture.ALL_SCENARIOS + perf_workflow_fixture.PLATFORM_SCENARIOS[platform_name]))
                self.assertEqual(len({entry["shard"] for entry in entries}), len(entries))
        # Windows keeps its S11 variants in S4-S5-S11, after S4, S5 and S11.
        shard = next(entry for entry in self.matrix("compare-windows") if entry["shard"] == "S4-S5-S11")
        self.assertEqual(shard["scenarios"].split()[:6], ["S4", "S5", "S11"] + perf_workflow_fixture.PLATFORM_SCENARIOS["Windows"])

    def test_the_macos_shards_are_the_reviewed_three_set_partition(self):
        # Three sets of about 19 min from measured per-variant times: S7 leads the first, S12 joins the S9 set and S2
        # the release set. Exact-head CI must measure it including runner queues against the 35-minute budget.
        self.assertEqual({entry["shard"]: entry["scenarios"].split() for entry in self.matrix("compare-macos")}, {
            "S7-S4": ["S7", "S4", "S1/atlas-retry", "S12/status-line", "S3", "S8", "S6/selection-drag"],
            "S9-S10sync": ["S10/sync", "S9", "S11", "S6", "S1", "S5", "S6/rest", "S1/status-line", "S12"],
            "S10-S11release": ["S10", "S11/release", "S2", "S2/flood", "S6/flood"],
        })

    def test_the_cell_layout_reader_names_the_shard_that_holds_s12(self):
        # The cell-layout decision reads S12/default runs only from the shards CELL_LAYOUT_SHARDS names, so on each
        # platform the one matrix entry that runs S12 must be among them.
        for job_id, platform_name in (("compare-macos", "macOS"), ("compare-windows", "Windows")):
            with self.subTest(job=job_id):
                holders = [entry["shard"] for entry in self.matrix(job_id) if "S12" in entry["scenarios"].split()]
                self.assertEqual(len(holders), 1, holders)
                self.assertIn(holders[0], perf_cell_layout.CELL_LAYOUT_SHARDS[platform_name])

    def test_the_atlas_retry_variant_runs_in_the_shard_its_projection_names(self):
        # S1/atlas-retry runs where its shard has room: macOS S7-S4 in the three-set partition, and Windows
        # S1-S3-S6-S8-S12, since Windows S4-S5-S11 was critical once.
        for job_id, shard_name in (("compare-macos", "S7-S4"), ("compare-windows", "S1-S3-S6-S8-S12")):
            with self.subTest(job=job_id):
                holders = [entry["shard"] for entry in self.matrix(job_id)
                           if "S1/atlas-retry" in entry["scenarios"].split()]
                self.assertEqual(holders, [shard_name])

    def test_only_the_s9_s10_shards_run_s9_laps(self):
        # Every matrix entry carries a laps field: S9 on the shard holding S9 (macOS S9-S10sync, Windows S9-S10),
        # empty elsewhere; each comparison step passes the laps flags only when the field is set, and no step sets
        # timeout-minutes.
        for job_id, laps_shard in (("compare-macos", "S9-S10sync"), ("compare-windows", "S9-S10")):
            with self.subTest(job=job_id):
                entries = self.matrix(job_id)
                self.assertEqual({entry["shard"]: entry["laps"] for entry in entries if entry["laps"]},
                                 {laps_shard: "S9"})
                holder = next(entry for entry in entries if entry["shard"] == laps_shard)
                self.assertIn("S9", holder["scenarios"].split())
                self.assertTrue(all("laps" in entry for entry in entries))
                step = next(step for step in perf_workflow_fixture.load_perf_workflow()["jobs"][job_id]["steps"]
                            if step.get("name") == "Compare the base and the head")
                self.assertEqual(step["env"]["LAPS"], "${{ matrix.laps }}")
                self.assertIn('if [ -n "$LAPS" ]; then\n  run_flags+=(--laps-scenario "$LAPS" --laps-runs 2)\nfi',
                              step["run"])
        self.assertNotIn("timeout-minutes", (Path(__file__).resolve().parent.parent / ".github" / "workflows"
                                             / "perf.yml").read_text(encoding="utf-8"))

    def test_windows_legs_get_cairo_bash_and_their_own_names(self):
        # Windows builds need Cairo from vcpkg, the shared step scripts need bash, and the two platforms'
        # shards must not share an artifact name.
        workflow = perf_workflow_fixture.load_perf_workflow()
        windows = workflow["jobs"]["compare-windows"]
        cairo = perf_expression_fixture.job_step(workflow, "compare-windows", "Install Cairo for Windows")
        self.assertEqual((cairo["shell"], cairo["run"]), ("pwsh", ".\\scripts\\setup-windows-cairo.ps1"))
        self.assertEqual(windows["defaults"]["run"]["shell"], "bash")
        for job_id in perf_workflow_fixture.COMPARISON_JOBS:
            job = workflow["jobs"][job_id]
            self.assertEqual(job["runs-on"], "${{ matrix.runner }}")
            self.assertIn("-${{ matrix.platform }}-${{ matrix.shard }}-",
                          perf_expression_fixture.job_step(workflow, job_id, perf_workflow_fixture.EVIDENCE_STEP)["with"]["name"])
            self.assertEqual(job["strategy"]["fail-fast"], "false")
