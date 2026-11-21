"""Perf workflow identity tests."""

from __future__ import annotations
import json
import os
import re
import shutil
import sys
from pathlib import Path
import tempfile
import unittest

import perf_execution
import perf_expression_fixture
import perf_shards
import perf_workflow_fixture
import perf_workflow_tests


# (context, eligible): a tag push; perf or perf-full added, alone or together; a push while either is set; another
# label on a perf or perf-full pull request; a pull request with neither, or with a label that only starts alike.
ELIGIBILITY_CASES = (
    (perf_workflow_fixture.push_github(), True),
    (perf_workflow_fixture.pull_request_github("labeled", ["perf"], "perf"), True),
    (perf_workflow_fixture.pull_request_github("labeled", ["perf-full"], "perf-full"), True),
    (perf_workflow_fixture.pull_request_github("labeled", ["perf", "perf-full"], "perf-full"), True),
    (perf_workflow_fixture.pull_request_github("synchronize", ["perf", "bug"]), True),
    (perf_workflow_fixture.pull_request_github("synchronize", ["perf-full"]), True),
    (perf_workflow_fixture.pull_request_github("labeled", ["perf", "bug"], "bug"), False),
    (perf_workflow_fixture.pull_request_github("labeled", ["perf-full", "bug"], "bug"), False),
    (perf_workflow_fixture.pull_request_github("opened", ["bug"]), False),
    (perf_workflow_fixture.pull_request_github("synchronize", ["perf-fullish"]), False),
    (perf_workflow_fixture.pull_request_github("synchronize", []), False),
)
SKIP_WITHOUT_BASH = (os.name == "nt" or shutil.which("bash") is None, "runs a workflow bash step with fake tools")


class ResultIdentityTests(unittest.TestCase):
    """Only an eligible run publishes the real result name, and only eligible runs share a concurrency group."""

    def test_only_an_eligible_run_publishes_the_result_name(self):
        # An ineligible run's skipped result job reads "(not run)", so it can never hide an eligible run's check.
        job = perf_workflow_fixture.load_perf_workflow()["jobs"]["perf-result"]
        for context, eligible in ELIGIBILITY_CASES:
            with self.subTest(event=context["github"]["event"].get("action", "push"), eligible=eligible):
                self.assertEqual(perf_expression_fixture.substitute(job["name"], context),
                                 perf_workflow_fixture.RESULT_NAME if eligible else perf_workflow_fixture.RESULT_NOT_RUN)
                self.assertEqual(perf_expression_fixture.condition_holds(job["if"], context, False), eligible)

    def concurrency(self, context):
        """The workflow-level group and cancel-in-progress for one event."""
        block = perf_workflow_fixture.load_perf_workflow()["concurrency"]
        return (perf_expression_fixture.substitute(block["group"], context),
                perf_expression_fixture.substitute(block["cancel-in-progress"], context))

    def test_eligible_runs_share_a_group_and_ineligible_runs_cancel_nothing(self):
        # Two eligible runs of one PR share a cancelling group, so adding perf-full replaces a running core
        # comparison; each ineligible run has a group of its own; a release run never cancels.
        first = self.concurrency(perf_workflow_fixture.pull_request_github("labeled", ["perf"], "perf", run_id="70"))
        newer = self.concurrency(perf_workflow_fixture.pull_request_github("synchronize", ["perf"], run_id="80"))
        full = self.concurrency(perf_workflow_fixture.pull_request_github("labeled", ["perf", "perf-full"], "perf-full",
                                                                          run_id="90"))
        self.assertEqual(first, ("perf-comparison-1575", "true"))
        self.assertEqual((newer, full), (first, first))
        ineligible = [self.concurrency(perf_workflow_fixture.pull_request_github("labeled", ["perf", "bug"], "bug",
                                                                                 run_id=str(run_id)))
                      for run_id in (81, 82)]
        self.assertEqual([group for group, _cancel in ineligible], ["perf-ineligible-81", "perf-ineligible-82"])
        self.assertEqual(self.concurrency(perf_workflow_fixture.push_github()), ("perf-comparison-v1.4.0", "false"))

    @unittest.skipIf(*SKIP_WITHOUT_BASH)
    def test_a_skipped_duplicate_run_is_all_skipped_under_its_own_name(self):
        # Another label on a perf PR starts a run whose every job skips; its result check reads "(not run)".
        context = perf_workflow_fixture.pull_request_github("labeled", ["perf", "bug"], "bug")
        with tempfile.TemporaryDirectory() as temp:
            simulator = perf_workflow_tests.PushSimulator(perf_workflow_fixture.load_perf_workflow(), Path(temp),
                                                          "first", github=context["github"]).run()
        self.assertEqual(simulator.results, {job: "skipped" for job in perf_workflow_fixture.PERF_JOBS})
        self.assertEqual(simulator.names["perf-result"], perf_workflow_fixture.RESULT_NOT_RUN)
        self.assertEqual(self.concurrency(context)[0], "perf-ineligible-70")

    @unittest.skipIf(*SKIP_WITHOUT_BASH)
    def test_a_superseded_eligible_run_never_reports_success(self):
        # A newer eligible run cancels this one: its result job still runs (always()), under the real name, and fails.
        job = perf_workflow_fixture.load_perf_workflow()["jobs"]["perf-result"]
        context = perf_workflow_fixture.pull_request_github("labeled", ["perf"], "perf")
        self.assertTrue(perf_expression_fixture.condition_holds(job["if"], context, False))
        self.assertEqual(perf_expression_fixture.substitute(job["name"], context), perf_workflow_fixture.RESULT_NAME)
        for results in (("cancelled", "cancelled", "cancelled"), ("success", "cancelled", "success"),
                        ("success", "skipped", "cancelled"), ("skipped", "skipped", "skipped")):
            with self.subTest(results=results):
                environ = dict(zip(("PLAN", "MACOS", "WINDOWS"), results))
                self.assertEqual(perf_workflow_tests.run_bash_step(job["steps"][0]["run"], environ).code, 1)


@unittest.skipIf(*SKIP_WITHOUT_BASH)
class ProfileStepTests(unittest.TestCase):
    """Both comparison jobs relax the release profile the same way on a pull request, and not on a tag."""

    def test_every_comparison_job_sets_the_same_profile(self):
        # Byte-for-byte the same step, writing the same GITHUB_ENV on a pull request and nothing on a push; the
        # plan builds nothing, so it has no such step.
        workflow = perf_workflow_fixture.load_perf_workflow()
        steps = [perf_expression_fixture.job_step(workflow, job_id, perf_workflow_fixture.PROFILE_STEP)
                 for job_id in perf_workflow_fixture.COMPARISON_JOBS]
        self.assertEqual(steps[1:], steps[:1])
        self.assertFalse([step for step in workflow["jobs"]["plan"]["steps"]
                          if step.get("name") == perf_workflow_fixture.PROFILE_STEP])
        for event, expected in (("pull_request", {"CARGO_PROFILE_RELEASE_LTO": "off",
                                                  "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16"}), ("push", None)):
            with self.subTest(event=event):
                holds = perf_expression_fixture.condition_holds(steps[0]["if"], {"github": {"event_name": event}}, True)
                self.assertEqual(holds, expected is not None)
                if expected:
                    self.assertEqual(perf_workflow_tests.run_bash_step(steps[0]["run"], {}).env, expected)


class PlanJobTests(unittest.TestCase):
    """The plan: the one job that resolves both SHAs and emits each platform's matrix from the chosen profile."""

    def job(self):
        return perf_workflow_fixture.load_perf_workflow()["jobs"]["plan"]

    def test_the_plan_runs_on_ubuntu_and_publishes_the_refs_and_matrices(self):
        # It builds nothing: a full-history checkout, the ref step and the partition step, whose outputs the other
        # jobs read, so a comparison job that starts late never resolves a base of its own.
        job = self.job()
        self.assertEqual((job["name"], job["runs-on"]), (perf_workflow_fixture.PLAN_NAME, "ubuntu-latest"))
        self.assertEqual(job["outputs"], {
            "base": "${{ steps.refs.outputs.base }}", "head": "${{ steps.refs.outputs.head }}",
            "length": "${{ steps.refs.outputs.length }}", "profile": "${{ steps.partitions.outputs.profile }}",
            "macos": "${{ steps.partitions.outputs.macos }}", "windows": "${{ steps.partitions.outputs.windows }}"})
        names = [step.get("name") or step["uses"].split("@")[0] for step in job["steps"]]
        self.assertEqual(names, ["actions/checkout", perf_workflow_fixture.REF_STEP, perf_workflow_fixture.PARTITION_STEP])
        self.assertEqual(job["steps"][0]["with"]["fetch-depth"], "0")
        self.assertEqual([step.get("id") for step in job["steps"][1:]], ["refs", "partitions"])

    def partition(self, event, labels, shards="real"):
        """Run the partition step for an event and the pull request's labels, with the real partition script."""
        step = perf_expression_fixture.job_step(perf_workflow_fixture.load_perf_workflow(), "plan",
                                                perf_workflow_fixture.PARTITION_STEP)
        context = {"github": {"event_name": event,
                              "event": {"pull_request": {"labels": [{"name": name} for name in labels]}}}}
        environ = {name: perf_expression_fixture.substitute(value, context) for name, value in step["env"].items()}
        environ.update(REAL_PYTHON=sys.executable, SCRIPTS_DIR=str(Path(__file__).resolve().parent), FAKE_SHARDS=shards)
        return perf_workflow_tests.run_bash_step(step["run"], environ, fakes={"python3": perf_workflow_tests.FAKE_PYTHON})

    @unittest.skipIf(*SKIP_WITHOUT_BASH)
    def test_the_event_and_labels_choose_the_profile(self):
        # A tag runs release; perf-full runs full, also beside perf; perf alone runs core. Each output is the
        # profile's matrix exactly as perf_shards.py builds it.
        profiles = perf_shards.load_profiles()
        for event, labels, expected in (("push", [], "release"), ("pull_request", ["perf"], "core"),
                                        ("pull_request", ["perf-full"], "full"),
                                        ("pull_request", ["perf", "perf-full"], "full")):
            with self.subTest(event=event, labels=labels):
                result = self.partition(event, labels)
                self.assertEqual(result.code, 0, result.stderr)
                self.assertEqual(result.outputs["profile"], expected)
                for platform_name in perf_shards.PLATFORMS:
                    self.assertEqual(json.loads(result.outputs[perf_shards.OUTPUT_KEYS[platform_name]]),
                                     perf_shards.matrix(profiles[expected], platform_name))
                self.assertIn(f"`{expected}`", result.summary)

    @unittest.skipIf(*SKIP_WITHOUT_BASH)
    def test_a_partition_file_that_breaks_a_rule_fails_the_plan(self):
        # The script's failure fails the step before any matrix is written, so no comparison job starts.
        result = self.partition("pull_request", ["perf"], shards="fail")
        self.assertNotEqual(result.code, 0)
        self.assertNotIn("macos", result.outputs)


class ComparisonJobTests(unittest.TestCase):
    """Each comparison job runs the plan's partitions for its platform, building both refs on its own runner."""

    def test_each_comparison_job_takes_its_matrix_and_refs_from_the_plan(self):
        # It needs the plan and nothing else; its matrix and SHAs are the plan's outputs; it resolves no refs itself,
        # and its comparison, summary and evidence steps run only when the plan resolved a base.
        workflow = perf_workflow_fixture.load_perf_workflow()
        for job_id in perf_workflow_fixture.COMPARISON_JOBS:
            with self.subTest(job=job_id):
                job = workflow["jobs"][job_id]
                self.assertEqual((job["needs"], job["runs-on"]), ("plan", perf_workflow_fixture.COMPARISON_RUNNERS[job_id]))
                self.assertEqual(job["name"], "${{ matrix.platform }} before/after comparison (${{ matrix.partition }})")
                self.assertEqual(job["strategy"]["fail-fast"], "false")
                output = perf_workflow_fixture.COMPARISON_OUTPUTS[job_id]
                self.assertEqual(job["strategy"]["matrix"], {"include": f"${{{{ fromJSON(needs.plan.outputs.{output}) }}}}"})
                self.assertFalse([step for step in job["steps"] if step.get("name") == perf_workflow_fixture.REF_STEP])
                step = perf_expression_fixture.job_step(workflow, job_id, perf_workflow_fixture.COMPARE_STEP)
                self.assertEqual(step["if"], "needs.plan.outputs.base != ''")
                self.assertEqual({key: step["env"][key] for key in ("BASE_SHA", "HEAD_SHA", "RUN_LENGTH", "SELECTION",
                                                                    "PERF_SHARD")},
                                 {"BASE_SHA": "${{ needs.plan.outputs.base }}", "HEAD_SHA": "${{ needs.plan.outputs.head }}",
                                  "RUN_LENGTH": "${{ needs.plan.outputs.length }}", "SELECTION": "${{ matrix.selection }}",
                                  "PERF_SHARD": "${{ matrix.partition }}"})
                for name in (perf_workflow_fixture.SUMMARY_STEP, perf_workflow_fixture.EVIDENCE_STEP):
                    self.assertEqual(perf_expression_fixture.job_step(workflow, job_id, name)["if"],
                                     "always() && needs.plan.outputs.base != ''")

    @unittest.skipIf(*SKIP_WITHOUT_BASH)
    def test_the_compare_step_passes_the_selection_and_shortens_only_a_pull_request(self):
        # The partition's words reach perf-compare.py unchanged after --require-base; a short run adds --short.
        workflow = perf_workflow_fixture.load_perf_workflow()
        for job_id, platform_name in perf_workflow_fixture.COMPARISON_PLATFORMS.items():
            step = perf_expression_fixture.job_step(workflow, job_id, perf_workflow_fixture.COMPARE_STEP)
            for run_length, short in (("short", True), ("full", False)):
                with self.subTest(job=job_id, run_length=run_length):
                    context = {"github": {}, "needs": {"plan": {"outputs": {"base": "b" * 40, "head": "c" * 40,
                                                                            "length": run_length}}},
                               "matrix": {"platform": platform_name, "partition": "core-1",
                                          "selection": "--scenario S1 S2/flood --runs 5 --counters --counters-runs 1"}}
                    environ = {name: perf_expression_fixture.substitute(value, context) for name, value in step["env"].items()}
                    result = perf_workflow_tests.run_bash_step(step["run"], environ, fakes={
                        "python3": perf_workflow_tests.RECORD_ARGV, "python": perf_workflow_tests.RECORD_ARGV})
                    self.assertEqual(result.code, 0, result.stderr)
                    self.assertEqual(result.argv, ["scripts/perf-compare.py", "--base", "b" * 40, "--head", "c" * 40,
                                                   "--require-base", "--scenario", "S1", "S2/flood", "--runs", "5",
                                                   "--counters", "--counters-runs", "1", *(["--short"] if short else []),
                                                   "--out", f"{result.root}/perf-comparison"])

    def test_windows_legs_get_cairo_and_bash(self):
        # Windows builds need Cairo from vcpkg, restored from CI's cache and never written, and the shared step
        # scripts need bash; no job or step sets timeout-minutes.
        workflow = perf_workflow_fixture.load_perf_workflow()
        cairo = perf_expression_fixture.job_step(workflow, "compare-windows", "Install Cairo for Windows")
        self.assertEqual((cairo["shell"], cairo["run"]), ("pwsh", ".\\scripts\\setup-windows-cairo.ps1"))
        self.assertEqual(workflow["jobs"]["compare-windows"]["defaults"]["run"]["shell"], "bash")
        restore = perf_expression_fixture.job_step(workflow, "compare-windows", "Restore vcpkg binaries (Cairo)")
        self.assertTrue(restore["uses"].startswith("actions/cache/restore@"))
        self.assertNotIn("timeout-minutes", perf_workflow_fixture.PERF_WORKFLOW.read_text(encoding="utf-8"))


class EvidenceArtifactTests(unittest.TestCase):
    """The CI comparison's artifact is the only copy of its evidence once the runner is gone."""

    EVIDENCE_ROOT = "${{ runner.temp }}/perf-comparison"

    def upload_patterns(self, job_id):
        """Return a comparison job's evidence `path:` lines, each as (excluded, pattern relative to the evidence root)."""
        step = perf_expression_fixture.job_step(perf_workflow_fixture.load_perf_workflow(), job_id,
                                                perf_workflow_fixture.EVIDENCE_STEP)
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

    def test_every_evidence_name_carries_its_platform_partition_and_attempt(self):
        # A rerun's evidence never shares a name with the attempt before it, and the platforms' partitions never
        # share a name with each other.
        workflow = perf_workflow_fixture.load_perf_workflow()
        for job_id in perf_workflow_fixture.COMPARISON_JOBS:
            with self.subTest(job=job_id):
                name = perf_expression_fixture.job_step(workflow, job_id, perf_workflow_fixture.EVIDENCE_STEP)["with"]["name"]
                self.assertTrue(name.endswith("-${{ matrix.platform }}-${{ matrix.partition }}-${{ github.run_attempt }}"))

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


@unittest.skipIf(*SKIP_WITHOUT_BASH)
class ReleaseRefSelectionTests(unittest.TestCase):
    """The plan's ref step: only a real first release may skip the comparison."""

    def step_script(self):
        """Return the `run:` block of the plan's ref-selection step."""
        return perf_expression_fixture.job_step(perf_workflow_fixture.load_perf_workflow(), "plan",
                                                perf_workflow_fixture.REF_STEP)["run"]

    def test_only_the_plan_resolves_refs(self):
        # The SHAs are resolved once, so a late comparison job can never measure a base that moved.
        workflow = perf_workflow_fixture.load_perf_workflow()
        counts = {job_id: sum(step.get("name") == perf_workflow_fixture.REF_STEP for step in job["steps"])
                  for job_id, job in workflow["jobs"].items()}
        self.assertEqual(counts, {"plan": 1, "compare-macos": 0, "compare-windows": 0, "perf-result": 0})

    def run_step(self, mode, environ=None):
        """Run the step with a fake git (a tag push by default); return its exit code, outputs and summary."""
        result = perf_workflow_tests.run_bash_step(self.step_script(), environ or {
            "EVENT": "push", "GITHUB_SHA": "tag-commit", "FAKE_GIT_MODE": mode}, fakes={"git": perf_workflow_fixture.FAKE_GIT})
        outputs = "".join(f"{name}={value}\n" for name, value in result.outputs.items())
        return result.code, outputs, result.summary

    def test_a_pull_request_compares_its_merge_base_with_its_head_in_short_runs(self):
        # The base branch is fetched and the merge base with the pull request's head is the base.
        code, outputs, _summary = self.run_step("previous", {"EVENT": "pull_request", "PR_HEAD_SHA": "c" * 40,
                                                             "PR_BASE_REF": "main", "FAKE_GIT_MODE": "previous"})
        self.assertEqual(code, 0)
        self.assertEqual(outputs.split(), ["base=merge-base-commit", f"head={'c' * 40}", "length=short"])

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


class ShardProfileTests(unittest.TestCase):
    """scripts/perf-shards.json's invariants, checked on the real file rather than as an exact layout."""

    @classmethod
    def setUpClass(cls):
        cls.profiles = perf_shards.load_profiles()
        cls.catalogue = perf_workflow_fixture.harness_catalogue()

    def labels(self, profile_name, platform_name):
        """Each variant label a profile runs on a platform, in partition order; a repeat would appear twice."""
        return [perf_shards.variant_label(variant) for partition in self.profiles[profile_name].partitions[platform_name]
                for variant in partition.variants]

    def each_layout(self):
        """Every (profile name, profile, platform) the file declares."""
        return [(name, profile, platform_name) for name, profile in self.profiles.items()
                for platform_name in perf_shards.PLATFORMS]

    def test_every_listed_variant_is_in_the_harness_catalogue(self):
        # A variant the harness does not list would fail its comparison's selection after both builds.
        for name, _profile, platform_name in self.each_layout():
            with self.subTest(profile=name, platform=platform_name):
                self.assertEqual(set(self.labels(name, platform_name)) - self.catalogue, set())

    def test_each_variant_runs_once_per_platform_per_profile(self):
        # A second copy would cost a second set of runs and two rows for one variant.
        for name, _profile, platform_name in self.each_layout():
            with self.subTest(profile=name, platform=platform_name):
                labels = self.labels(name, platform_name)
                self.assertEqual(len(labels), len(set(labels)))

    def test_platform_only_variants_stay_on_their_platform(self):
        # Neither platform runs a variant the other's runner alone can measure.
        for name, _profile, platform_name in self.each_layout():
            for label in self.labels(name, platform_name):
                self.assertIn(perf_workflow_fixture.PLATFORM_ONLY.get(label), (None, platform_name), (name, label))

    def test_release_holds_full_and_full_holds_core(self):
        # A pull request never measures a variant the release comparison would not, and perf-full covers perf.
        for platform_name in perf_shards.PLATFORMS:
            with self.subTest(platform=platform_name):
                core, full, release = (set(self.labels(name, platform_name)) for name in perf_shards.PROFILES)
                self.assertLessEqual(core, full)
                self.assertLessEqual(full, release)

    def test_core_is_the_reviewed_set_in_two_partitions_a_platform(self):
        # The pull-request default keeps the reviewed core variants, with S13/sustained-4 for the contention verdict.
        for platform_name in perf_shards.PLATFORMS:
            with self.subTest(platform=platform_name):
                self.assertEqual(sorted(self.labels("core", platform_name)), sorted(perf_workflow_fixture.CORE_VARIANTS))
                self.assertEqual(len(self.profiles["core"].partitions[platform_name]), 2)

    def test_full_runs_every_pull_request_variant_in_three_partitions_a_platform(self):
        # Every catalogue variant the platform measures, except the release-only ones and those no profile compares.
        for platform_name in perf_shards.PLATFORMS:
            with self.subTest(platform=platform_name):
                expected = {label for label in self.catalogue
                            if label not in perf_workflow_fixture.RELEASE_ONLY
                            and label not in perf_workflow_fixture.NOT_COMPARED
                            and perf_workflow_fixture.PLATFORM_ONLY.get(label, platform_name) == platform_name}
                self.assertEqual(set(self.labels("full", platform_name)), expected)
                self.assertEqual(len(self.profiles["full"].partitions[platform_name]), 3)

    def test_release_adds_only_the_release_only_variants(self):
        # Every full variant, plus each release-only one the platform measures (S13's wide layouts on both).
        for platform_name in perf_shards.PLATFORMS:
            with self.subTest(platform=platform_name):
                extra = {label for label in perf_workflow_fixture.RELEASE_ONLY
                         if perf_workflow_fixture.PLATFORM_ONLY.get(label, platform_name) == platform_name}
                self.assertEqual(set(self.labels("release", platform_name)), set(self.labels("full", platform_name)) | extra)

    def test_laps_run_only_where_declared(self):
        # Only S9 runs the laps set, in full and release and never in core; a partition passes laps flags exactly
        # when it declares laps (the loader refuses laps for a variant the partition does not run).
        for name, profile, platform_name in self.each_layout():
            with self.subTest(profile=name, platform=platform_name):
                laps = [variant for partition in profile.partitions[platform_name] for variant in partition.laps]
                self.assertEqual(laps, [] if name == "core" else ["S9"])
                for partition in profile.partitions[platform_name]:
                    arguments = perf_shards.partition_arguments(profile, partition)
                    self.assertEqual("--laps-scenario" in arguments, bool(partition.laps))

    def test_variants_one_report_reads_together_share_a_partition(self):
        # S13's verdict and scaling rule and the S11 burst pair read their variants from one comparison.
        for name, profile, platform_name in self.each_layout():
            holder = {perf_shards.variant_label(variant): partition.name
                      for partition in profile.partitions[platform_name] for variant in partition.variants}
            for group in perf_workflow_fixture.SHARED_PARTITION:
                present = [label for label in group if label in holder]
                with self.subTest(profile=name, platform=platform_name, group=group):
                    self.assertIn(present, ([], list(group)))
                    self.assertLessEqual(len({holder[label] for label in present}), 1)

    def test_a_counters_only_variant_always_runs_the_counters_set(self):
        # S1/atlas-retry measures nothing outside the counters set, so its partition must pass --counters.
        for name, profile, platform_name in self.each_layout():
            for partition in profile.partitions[platform_name]:
                if "S1/atlas-retry" in partition.variants:
                    self.assertIn("--counters", perf_shards.partition_arguments(profile, partition), (name, partition.name))

    def test_partition_names_carry_their_profile(self):
        # A name is part of a check name and an artifact name, so it says which profile measured it.
        for name, profile, platform_name in self.each_layout():
            for partition in profile.partitions[platform_name]:
                self.assertTrue(partition.name.startswith(f"{name}-"), (platform_name, partition.name))

    def test_the_core_sampling_is_five_timed_runs_and_one_counters_run(self):
        # The pull-request default's reviewed sampling: one short S13 counters run still types 300 characters,
        # above the contention verdict's 200-sample floor.
        core = self.profiles["core"]
        self.assertEqual((core.runs, core.counters_runs, core.laps_runs), (5, 1, None))
