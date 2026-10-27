"""Perf focus tests."""

from __future__ import annotations
import json
from pathlib import Path
import tempfile
import unittest

import perf_focus


def command(argv, stdout="", exit_code=0, stderr="", timed_out=False, unix_s=100.0):
    """Build one raw command record as the bounded runner reports it."""
    return perf_focus.CommandRecord(unix_s=unix_s, argv=tuple(argv), exit_code=exit_code,
                              stdout=stdout, stderr=stderr, timed_out=timed_out)


FRONT_ARGV = ("lsappinfo", "front")


class FrontApplicationTests(unittest.TestCase):
    def classify(self, front, pid_record=None):
        """Classify one sample; record every `info` lookup so tests can see whether it ran."""
        lookups = []

        def lookup(asn):
            lookups.append(asn)
            if pid_record is None:
                raise AssertionError("no lookup expected")
            return pid_record
        return perf_focus.classify_front(front, lookup), lookups

    def test_both_null_forms_mean_no_front_application(self):
        # Only `[ NULL ]` and a null ASN followed by a character that is not a hex digit mean no front application.
        for stdout in ("[ NULL ]", "  [ NULL ] \n", "ASN:0x0-0x0-NULL\n", "ASN:0x0-0x0:\n"):
            with self.subTest(stdout=stdout):
                reading, lookups = self.classify(command(FRONT_ARGV, stdout))
                self.assertEqual(reading.kind, "none")
                self.assertEqual(lookups, [])

    def test_a_cut_off_null_asn_fails_the_sample(self):
        # A bare `ASN:0x0-0x0` is cut-off output, not a null form. It fails the sample, so it never stands
        # for no front application before a harness activation.
        reading, lookups = self.classify(command(FRONT_ARGV, "ASN:0x0-0x0\n"))
        self.assertEqual(reading.kind, "failed")
        self.assertEqual(lookups, [])

    def test_front_application_needs_a_successful_pid_lookup(self):
        # An ASN line followed by a `"pid"=<n>` lookup names the front application.
        asn = "ASN:0x0-0x6ba4b9e:"
        reading, lookups = self.classify(
            command(FRONT_ARGV, asn + "\n"),
            command(("lsappinfo", "info", "-only", "pid", asn), '"pid"=55181\n'))
        self.assertEqual((reading.kind, reading.pid), ("app", 55181))
        self.assertEqual(lookups, [asn])
        self.assertEqual(perf_focus.front_pid_argv(asn), ("lsappinfo", "info", "-only", "pid", asn))

    def test_a_hex_digit_after_the_null_prefix_is_a_real_application(self):
        # `ASN:0x0-0x0a1:` is an application whose ASN starts with zero, not the null ASN.
        reading, lookups = self.classify(command(FRONT_ARGV, "ASN:0x0-0x0a1:\n"),
                                         command(("lsappinfo",), '"pid"=42\n'))
        self.assertEqual((reading.kind, reading.pid), ("app", 42))
        self.assertEqual(lookups, ["ASN:0x0-0x0a1:"])

    def test_a_low_half_without_0x_is_looked_up_with_0x(self):
        # The macos-14 runner prints `ASN:0x0-c00c:` and its lookup of that spelling prints nothing,
        # so the fake answers only the `0x` spelling, as the lookup must ask for it.
        for printed, spelled in (("ASN:0x0-c00c:", "ASN:0x0-0xc00c:"), ("ASN:0x0-24024:", "ASN:0x0-0x24024:")):
            with self.subTest(printed=printed):
                lookups = []

                def lookup(asn, spelled=spelled):
                    lookups.append(asn)
                    return command(perf_focus.front_pid_argv(asn), '"pid"=4242\n' if asn == spelled else "")
                reading = perf_focus.classify_front(command(FRONT_ARGV, printed + "\n"), lookup)
                self.assertEqual((reading.kind, reading.pid), ("app", 4242))
                self.assertEqual(lookups, [spelled])

    def test_failed_samples_never_count_as_no_front_application(self):
        # Empty output, a nonzero exit, a timeout or other text invalidates the sample.
        cases = (command(FRONT_ARGV, ""),
                 command(FRONT_ARGV, "[ NULL ]", exit_code=1),
                 command(FRONT_ARGV, "ASN:0x0-0x0:", exit_code=None, timed_out=True),
                 command(FRONT_ARGV, "[NULL]"),
                 command(FRONT_ARGV, "no application\n"),
                 command(FRONT_ARGV, "ASN:0x0-0x6ba4b9e:\nASN:0x0-0x1:\n"))
        for record in cases:
            with self.subTest(record=record):
                reading, _lookups = self.classify(record)
                self.assertEqual(reading.kind, "failed")
                self.assertTrue(reading.detail)

    def test_a_failed_pid_lookup_fails_the_sample(self):
        # The lookup must exit 0 and print exactly one positive pid.
        asn_record = command(FRONT_ARGV, "ASN:0x0-0x6ba4b9e:\n")
        for lookup in (command(("lsappinfo",), "", exit_code=0),
                       command(("lsappinfo",), '"pid"=55181\n', exit_code=1),
                       command(("lsappinfo",), "", exit_code=None, timed_out=True),
                       command(("lsappinfo",), '"pid"=0\n'),
                       command(("lsappinfo",), '"pid"=[ NULL ]\n')):
            with self.subTest(lookup=lookup):
                reading, _lookups = self.classify(asn_record, lookup)
                self.assertEqual(reading.kind, "failed")
                self.assertEqual(len(reading.records), 2)


def reading(kind, pid=None):
    """Build a classified front sample without raw records."""
    return perf_focus.FrontReading(kind, pid, "" if kind != "failed" else "unparseable", ())


class FocusTheftTests(unittest.TestCase):
    HARNESS = 900

    def test_activation_while_another_application_is_front_is_theft(self):
        # The harness becoming front right after another application was front is theft.
        verdict = perf_focus.judge_focus([reading("app", 10), reading("app", self.HARNESS)], self.HARNESS)
        self.assertTrue(verdict.theft)
        self.assertTrue(verdict.problems)

    def test_activation_with_no_front_application_is_not_theft(self):
        # A host with no front application cannot have focus stolen.
        verdict = perf_focus.judge_focus([reading("none"), reading("app", self.HARNESS),
                                    reading("app", self.HARNESS)], self.HARNESS)
        self.assertFalse(verdict.theft)
        self.assertEqual(verdict.problems, [])

    def test_other_applications_changing_focus_is_not_theft(self):
        # Focus moving between the user's own applications says nothing about the harness.
        verdict = perf_focus.judge_focus([reading("app", 10), reading("app", 11), reading("none")], self.HARNESS)
        self.assertEqual(verdict.problems, [])

    def test_a_failed_sample_invalidates_the_run(self):
        # A failed sample is a problem of its own and never reads as no front application.
        verdict = perf_focus.judge_focus([reading("app", 10), reading("failed"), reading("app", self.HARNESS)],
                                   self.HARNESS)
        self.assertFalse(verdict.theft)
        self.assertEqual(len(verdict.failed), 1)
        self.assertTrue(verdict.problems)

    def test_unknown_harness_pid_cannot_pass(self):
        # Without harness.pid no sample can be judged, so the focus check cannot pass.
        verdict = perf_focus.judge_focus([reading("app", 10)], None)
        self.assertFalse(verdict.judged)
        self.assertFalse(verdict.passed)
        self.assertTrue(perf_focus.judge_focus([reading("app", 10)], self.HARNESS).passed)

    def test_activation_without_a_user_session_is_noted_not_theft(self):
        # On a CI runner nobody's focus can be taken: activation after another application is only noted.
        verdict = perf_focus.judge_focus([reading("app", 10), reading("app", self.HARNESS)], self.HARNESS,
                                   user_session=False)
        self.assertFalse(verdict.theft)
        self.assertEqual(verdict.problems, [])
        self.assertTrue(verdict.passed)
        self.assertEqual(len(verdict.notes), 1)
        self.assertIn(str(self.HARNESS), verdict.notes[0])

    def test_failed_samples_still_fail_without_a_user_session(self):
        # The CI exception covers activation only; a failed sample still invalidates the run.
        verdict = perf_focus.judge_focus([reading("app", 10), reading("failed")], self.HARNESS, user_session=False)
        self.assertFalse(verdict.passed)

    def test_only_a_github_hosted_runner_lacks_a_user_session(self):
        # The smoke and a comparison on a GitHub-hosted runner have no user; GITHUB_ACTIONS=true alone does not
        # prove that, because a self-hosted runner may have one, and a desk run stays strict.
        hosted = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
        self.assertFalse(perf_focus.has_user_session(hosted))
        for environ in ({"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "self-hosted"}, {"GITHUB_ACTIONS": "true"},
                        {"RUNNER_ENVIRONMENT": "github-hosted"},
                        {"GITHUB_ACTIONS": "false", "RUNNER_ENVIRONMENT": "github-hosted"}, {}):
            with self.subTest(environ=environ):
                self.assertTrue(perf_focus.has_user_session(environ))

    def test_the_log_names_the_focus_rule_and_the_runner(self):
        # The first CI log shows that RUNNER_ENVIRONMENT reached the run, and which focus rule judged it.
        hosted = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
        line = perf_focus.focus_rule_line(hosted)
        self.assertIn("GITHUB_ACTIONS=true RUNNER_ENVIRONMENT=github-hosted", line)
        self.assertIn("a GitHub-hosted runner has no user session", line)
        for environ in (dict(hosted, RUNNER_ENVIRONMENT="self-hosted"), {}):
            with self.subTest(environ=environ):
                self.assertIn("strict", perf_focus.focus_rule_line(environ))
        self.assertIn("RUNNER_ENVIRONMENT=unset", perf_focus.focus_rule_line({}))

    def test_every_raw_sample_is_appended_to_the_evidence_log(self):
        # The log keeps time, argv, exit status, stdout and stderr of both commands of a sample.
        with tempfile.TemporaryDirectory() as temporary:
            log_path = Path(temporary) / "front-samples.log"
            records = (command(FRONT_ARGV, "ASN:0x0-0x1:\n", unix_s=5.0),
                       command(("lsappinfo", "info"), "", exit_code=1, stderr="gone\n", unix_s=5.5))
            perf_focus.append_front_samples(log_path, records)
            perf_focus.append_front_samples(log_path, records[:1])
            lines = [json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines()]
        self.assertEqual(len(lines), 3)
        self.assertEqual(lines[1], {"unix_s": 5.5, "argv": ["lsappinfo", "info"], "exit_code": 1,
                                    "timed_out": False, "stdout": "", "stderr": "gone\n"})
        failed = perf_focus.FrontReading("failed", None, "lookup failed", records)
        self.assertIn('"stderr": "gone\\n"', perf_focus.describe_sample(failed))
