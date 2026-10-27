"""Perf prebuilt tests."""

from __future__ import annotations
import contextlib
import hashlib
import io
import json
import os
import re
import shutil
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import perf_arguments
import perf_build_policy
import perf_counter_schema_tests
import perf_driver_tests
import perf_harness_sources
import perf_host_tests
import perf_prebuilt
import perf_settings



# The producer's GitHub context, which the consumer's own binding flags and environment must match.
PRODUCER_ENV = {"GITHUB_RUN_ID": "37118041050", "GITHUB_RUN_ATTEMPT": "1", "ImageOS": "macos14",
                "ImageVersion": "20260928.1", "CARGO_PROFILE_RELEASE_LTO": "off",
                "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16"}


class PrebuiltHarness(perf_driver_tests.CompareHarness):
    """Produces binaries with `--build-only`, then consumes them with `--prebuilt` in a fresh comparison."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.shared = Path(temporary.name)

    def environment(self, **overrides):
        """The producer's environment with `overrides`; any other CARGO_PROFILE_RELEASE_* is cleared."""
        environ = {name: value for name, value in os.environ.items() if not name.startswith("CARGO_PROFILE_RELEASE_")}
        environ.update(PRODUCER_ENV)
        environ.update(overrides)
        return environ

    def produce(self, options=(), **kwargs):
        """Run `--build-only` into a shared directory; return it and the manifest digest the run printed."""
        binaries = self.shared / "perf-binaries"
        environ = kwargs.pop("environ", None) or self.environment()
        with mock.patch.dict(os.environ, environ, clear=True):
            code, *_rest = self.compare(options=("--require-base", "--build-only", str(binaries), *options),
                                        real_binaries=True, **kwargs)
        self.assertEqual(code, perf_settings.EXIT_PASS)
        printed = re.search(r"^manifest_sha256=([0-9a-f]{64})$", self.printed, re.M)
        self.assertIsNotNone(printed, self.printed)
        return binaries, printed.group(1)

    def consume(self, binaries, digest, run_id="37118041050", attempt="1", options=(), environ=None, **kwargs):
        """Run a strict comparison on the produced binaries; return what `compare` returns."""
        binding = ("--prebuilt", str(binaries), "--prebuilt-run-id", run_id, "--prebuilt-attempt", attempt,
                   "--prebuilt-manifest-sha256", digest)
        with mock.patch.dict(os.environ, environ or self.environment(), clear=True):
            return self.compare(options=("--require-base", *binding, *options), **kwargs)

    @staticmethod
    def rewrite(binaries, change):
        """Apply `change` to the manifest, write it back and return its new digest, so rule 2 still passes."""
        manifest = binaries / "manifest.json"
        data = json.loads(manifest.read_text(encoding="utf-8"))
        change(data)
        manifest.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        return hashlib.sha256(manifest.read_bytes()).hexdigest()


class BuildOnlyTests(PrebuiltHarness, unittest.TestCase):
    """`--build-only DIR`: the producer builds both refs once and refuses every comparison option."""

    def test_comparison_options_are_rejected(self):
        # A producer measures nothing, so a run option would be silently ignored; each is a usage error.
        for extra in (("--scenario", "S1"), ("--runs", "5"), ("--short",), ("--laps",), ("--counters",),
                      ("--counters-runs", "2"), ("--keep",), ("--out", "/tmp/out"),
                      ("--prebuilt", "/tmp/binaries")):
            with self.subTest(extra=extra), self.assertRaises(SystemExit):
                self.parse("--base", "main", "--head", "HEAD", "--require-base", "--build-only", "/tmp/b", *extra)
        with self.assertRaises(SystemExit):
            # A producer is strict: a manifest always holds both sides.
            self.parse("--base", "main", "--head", "HEAD", "--build-only", "/tmp/b")
        self.assertTrue(self.parse("--base", "main", "--head", "HEAD", "--require-base", "--build-only", "/tmp/b",
                                   "--alloc").alloc)

    def test_prebuilt_needs_every_binding_flag(self):
        # A consumer binds the artifact to the run, the producer's attempt and the published digest.
        binding = ("--prebuilt-run-id", "1", "--prebuilt-attempt", "1", "--prebuilt-manifest-sha256", "a" * 64)
        for index in range(0, len(binding), 2):
            partial = binding[:index] + binding[index + 2:]
            with self.subTest(missing=binding[index]), self.assertRaises(SystemExit):
                self.parse("--base", "main", "--head", "HEAD", "--prebuilt", "/tmp/b", *partial)
        with self.assertRaises(SystemExit):
            self.parse("--base", "main", "--head", "HEAD", *binding)
        with self.assertRaises(SystemExit):
            self.parse("--base", "main", "--head", "HEAD", "--prebuilt", "/tmp/b", *binding[:4],
                       "--prebuilt-manifest-sha256", "not-hex")
        self.assertEqual(self.parse("--base", "main", "--head", "HEAD", "--prebuilt", "/tmp/b", *binding)
                         .prebuilt_attempt, "1")

    def parse(self, *argv):
        with contextlib.redirect_stderr(io.StringIO()):
            return perf_arguments.parse_args(list(argv))

    def test_a_head_build_failure_fails(self):
        # The head builds first and its error ends the producer.
        with self.assertRaisesRegex(ValueError, "the head cannot build"):
            self.produce(head_build="FAIL")

    def test_a_base_build_failure_fails(self):
        # A producer never publishes a manifest without the base.
        with self.assertRaisesRegex(ValueError, "the base cannot build"):
            self.produce(base_build="FAIL")

    def test_a_producer_whose_binary_cannot_list_fails(self):
        # The producer lists both binaries before publishing them.
        with self.assertRaisesRegex(ValueError, "--list"):
            self.produce(list_fail=("head",))

    def test_alloc_adds_the_alloc_example(self):
        # `--alloc` builds and publishes perf_scenarios_alloc on both sides.
        binaries, _digest = self.produce(options=("--alloc",))
        manifest = json.loads((binaries / "manifest.json").read_text(encoding="utf-8"))
        for side in perf_settings.SIDES:
            self.assertEqual(sorted(manifest["binaries"][side]), sorted([perf_settings.ALLOC_EXAMPLE, perf_settings.HARNESS_EXAMPLE]))


class PrebuiltManifestTests(PrebuiltHarness, unittest.TestCase):
    """What the producer's manifest.json records, and the digest it prints for the workflow."""

    def test_every_field_is_recorded(self):
        # The manifest binds the binaries to the run, both SHAs, the harness, toolchain, profile and image.
        binaries, digest = self.produce(head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST)
        manifest_path = binaries / "manifest.json"
        self.assertEqual(hashlib.sha256(manifest_path.read_bytes()).hexdigest(), digest)
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        self.assertEqual(manifest["schema_version"], 1)
        self.assertEqual((manifest["run_id"], manifest["run_attempt"]), ("37118041050", "1"))
        self.assertEqual((manifest["base_sha"], manifest["head_sha"]), (self.SHAS["main"], self.SHAS["HEAD"]))
        self.assertRegex(manifest["harness_hash"], r"^[0-9a-f]{64}$")
        for side in perf_settings.SIDES:
            entry = manifest["binaries"][side][perf_settings.HARNESS_EXAMPLE]
            self.assertEqual(entry["path"], f"{side}/{perf_prebuilt.executable_name(perf_settings.HARNESS_EXAMPLE)}")
            self.assertEqual(entry["sha256"], hashlib.sha256((binaries / entry["path"]).read_bytes()).hexdigest())
            self.assertTrue(os.access(binaries / entry["path"], os.X_OK))
        self.assertEqual(manifest["harness_cfg_scope"], "example-target-v1")
        self.assertEqual(manifest["target"], "aarch64-apple-darwin")
        self.assertEqual(manifest["toolchain"], {"rustc": perf_host_tests.RUSTC_VV.strip(),
                                                 "cargo": "cargo 1.99.0 (abcdef 2026-09-01)"})
        self.assertEqual(manifest["profile"]["overrides"],
                         "CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 CARGO_PROFILE_RELEASE_LTO=off")
        self.assertEqual(manifest["profile"]["lto"], {"base": "off", "head": "off"})
        self.assertEqual(manifest["features"], {"base": [], "head": [perf_harness_sources.COUNTERS_FEATURE]})
        self.assertEqual(manifest["runner_image"], {"os": "macos14", "version": "20260928.1"})

    def test_the_lto_setting_is_read_from_the_tree_without_an_override(self):
        # Without CARGO_PROFILE_RELEASE_LTO, a release build takes the tree's own [profile.release] lto.
        manifest_text = '[workspace]\n\n[profile.release]\nopt-level = 3\nlto = "fat"\n\n[profile.dev]\nlto = false\n'
        with tempfile.TemporaryDirectory() as temp:
            (Path(temp) / "Cargo.toml").write_text(manifest_text, encoding="utf-8")
            self.assertEqual(perf_prebuilt.release_lto(Path(temp), {}), "fat")
            self.assertEqual(perf_prebuilt.release_lto(Path(temp), {"CARGO_PROFILE_RELEASE_LTO": "off"}), "off")
            self.assertEqual(perf_prebuilt.release_lto(Path(temp) / "missing", {}), "unset")

    def test_the_producer_publishes_no_assets(self):
        # Only executables and the manifest move; each ref's assets come from its own tree on the consumer.
        binaries, _digest = self.produce()
        published = sorted(path.relative_to(binaries).as_posix() for path in binaries.rglob("*")
                           if path.is_file() and "build-logs" not in path.parts)
        executable = perf_prebuilt.executable_name(perf_settings.HARNESS_EXAMPLE)
        self.assertEqual(published, [f"base/{executable}", f"head/{executable}", "manifest.json"])


class PrebuiltRefusalTests(PrebuiltHarness, unittest.TestCase):
    """A consumer refuses any artifact that disagrees with its own job, one rule at a time."""

    def refused(self, pattern, binaries, digest, **kwargs):
        """Assert the consumer refuses the binaries with a message matching `pattern`."""
        with self.assertRaisesRegex(ValueError, "refusing the prebuilt binaries: .*" + pattern):
            self.consume(binaries, digest, **kwargs)

    def test_rule_1_a_missing_directory_manifest_or_wrong_schema(self):
        # A download that left nothing, a directory without a manifest, or another schema version.
        binaries, digest = self.produce()
        self.refused("no directory", self.shared / "absent", digest)
        digest = self.rewrite(binaries, lambda data: data.update(schema_version=2))
        self.refused("schema", binaries, digest)
        (binaries / "manifest.json").unlink()
        self.refused("manifest", binaries, digest)

    def test_rule_2_a_manifest_whose_digest_differs(self):
        # The manifest must be byte for byte the one the producer job published.
        binaries, _digest = self.produce()
        self.refused("sha256", binaries, "0" * 64)

    def test_rule_3_another_run_or_attempt(self):
        # A stale artifact from another run, or another producer attempt, is not this job's producer.
        binaries, digest = self.produce()
        self.refused("run", binaries, digest, run_id="37000000000")
        self.refused("attempt", binaries, digest, attempt="2")

    def test_rule_4_either_sha_differs(self):
        # The producer built other commits than this shard resolved.
        for side in perf_settings.SIDES:
            with self.subTest(side=side):
                binaries, _digest = self.produce()
                digest = self.rewrite(binaries, lambda data, side=side: data.update({f"{side}_sha": "9" * 40}))
                self.refused(f"{side} SHA", binaries, digest)
                shutil.rmtree(binaries)

    def test_rule_5_the_harness_hash_differs(self):
        # The binaries were built from another overlaid harness.
        binaries, _digest = self.produce()
        digest = self.rewrite(binaries, lambda data: data.update(harness_hash="f" * 64))
        self.refused("harness", binaries, digest)

    def test_rule_6_the_features_differ(self):
        # Each side's cargo features must be the ones this job derives from that side's tree.
        for side in perf_settings.SIDES:
            with self.subTest(side=side):
                binaries, _digest = self.produce()
                digest = self.rewrite(binaries, lambda data, side=side: data["features"].update(
                    {side: [perf_harness_sources.COUNTERS_FEATURE]}))
                self.refused(f"features", binaries, digest)
                shutil.rmtree(binaries)

    def test_a_global_cfg_build_cannot_substitute_for_target_scoped_binaries(self):
        # Compiler flag spelling alone does not state which crates received it; bind placement too.
        binaries, digest = self.produce()
        digest = self.rewrite(binaries, lambda data: data.update(harness_cfg_scope="workspace"))
        self.refused("harness cfg scope", binaries, digest)

    def test_rule_7_target_toolchain_or_image_differs(self):
        # A rolled-over toolchain or runner image fails closed rather than mixing builds and runs.
        binaries, digest = self.produce()
        self.refused("toolchain", binaries, digest, toolchain=perf_host_tests.RUSTC_VV.replace("1.99.0", "1.100.0"))
        self.refused("target", binaries, digest, toolchain=perf_host_tests.RUSTC_VV.replace("aarch64", "x86_64"))
        self.refused("runner image", binaries, digest, environ=self.environment(ImageVersion="20261001.2"))

    def test_rule_8_the_profile_differs(self):
        # A consumer whose profile step set other overrides would describe binaries it did not get.
        binaries, digest = self.produce()
        self.refused("profile", binaries, digest, environ=self.environment(CARGO_PROFILE_RELEASE_LTO="thin"))

    def test_rule_9_a_binary_is_missing_linked_unexecutable_misdigested_or_absent(self):
        # Every published file is checked before it is copied, on each side.
        executable = perf_prebuilt.executable_name(perf_settings.HARNESS_EXAMPLE)
        damages = {
            "missing": lambda path: path.unlink(),
            "symlink": lambda path: (path.rename(path.with_name("real")), path.symlink_to("real")),
            "digest": lambda path: path.write_bytes(b"tampered"),
        }
        if os.name != "nt":
            damages["executable"] = lambda path: path.chmod(0o644)
        for side in perf_settings.SIDES:
            for damage, apply in damages.items():
                with self.subTest(side=side, damage=damage):
                    binaries, digest = self.produce()
                    try:
                        apply(binaries / side / executable)
                    except OSError:
                        # When: a Windows runner without symlink privilege cannot create the link, only it skips.
                        shutil.rmtree(binaries)
                        self.skipTest("this host cannot create a symlink")
                    self.refused(f"{side} {perf_settings.HARNESS_EXAMPLE}.*{damage}", binaries, digest)
                    shutil.rmtree(binaries)
        binaries, digest = self.produce()
        # A selected set that needs an example the producer never built.
        self.refused(f"base {perf_settings.ALLOC_EXAMPLE}", binaries, digest, options=("--alloc",))

    def test_rule_10_a_side_that_cannot_list_or_has_an_asset_problem(self):
        # The copied binary must load and list from that side's tree, which must hold its own assets.
        for side in perf_settings.SIDES:
            other = "head" if side == "base" else "base"
            with self.subTest(side=side, problem="list"):
                binaries, digest = self.produce()
                self.refused("--list", binaries, digest, list_fail=(side,))
                shutil.rmtree(binaries)
            with self.subTest(side=side, problem="assets"):
                binaries, digest = self.produce()
                self.refused(f"{side} cannot run", binaries, digest, assets=(other,))
                shutil.rmtree(binaries)


class PrebuiltRerunTests(PrebuiltHarness, unittest.TestCase):
    """GitHub reruns: which producer attempt a consumer may accept."""

    def test_a_failed_job_rerun_accepts_the_successful_producer_attempt(self):
        # "Re-run failed jobs" reruns a shard at attempt 2 but reuses the producer of attempt 1.
        binaries, digest = self.produce()
        code, *_rest = self.consume(binaries, digest, attempt="1",
                                    environ=self.environment(GITHUB_RUN_ATTEMPT="2"))
        self.assertEqual(code, perf_settings.EXIT_PASS)

    def test_a_full_rerun_refuses_the_earlier_attempt(self):
        # "Re-run all jobs" gets a new producer at attempt 2; attempt 1's artifact is no longer its producer.
        binaries, digest = self.produce()
        with self.assertRaisesRegex(ValueError, "attempt"):
            self.consume(binaries, digest, attempt="2", environ=self.environment(GITHUB_RUN_ATTEMPT="2"))

    def test_a_stale_artifact_from_another_run_is_refused(self):
        # An artifact left from another workflow run names that run's id.
        binaries, digest = self.produce(environ=self.environment(GITHUB_RUN_ID="37000000000"))
        with self.assertRaisesRegex(ValueError, "run"):
            self.consume(binaries, digest)


class PrebuiltCompareTests(PrebuiltHarness, unittest.TestCase):
    """A prebuilt comparison measures exactly as a building one does, without Cargo."""

    def test_no_cargo_build_and_each_side_runs_in_its_own_tree(self):
        # The consumer runs no build step; its runs use the copied binaries with each ref's worktree as cwd.
        binaries, digest = self.produce()
        code, gate, calls, plans, work, out = self.consume(binaries, digest)
        self.assertEqual(code, perf_settings.EXIT_PASS)
        self.assertFalse([step.id for step in gate.steps if step.id.startswith("build-")])
        self.assertFalse([call for call in calls if call[:2] == ("cargo", "build")])
        executable = perf_prebuilt.executable_name(perf_settings.HARNESS_EXAMPLE)
        self.assertEqual({(plan.side, plan.binary, plan.source_root) for plan in plans},
                         {(side, work / "prebuilt" / side / executable, work / side) for side in perf_settings.SIDES})
        details = (out / "comparison.md").read_text(encoding="utf-8")
        self.assertIn(f"- Builds: prebuilt by run 37118041050 attempt 1, manifest sha256 `{digest}`", details)

    def test_the_tables_equal_a_building_comparisons(self):
        # Only the builds' origin differs: every table row matches a comparison that built both refs itself.
        binaries, digest = self.produce(head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST)
        options = ("--counters", "--counters-runs", "2")
        _code, _gate, _calls, _plans, _work, prebuilt_out = self.consume(binaries, digest, options=options,
                                                                         head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST)
        with mock.patch.dict(os.environ, self.environment(), clear=True):
            _code, _gate, _calls, _plans, _work, built_out = self.compare(
                options=("--require-base", *options), head_manifest=perf_counter_schema_tests.COUNTERS_MANIFEST)

        def tables(out):
            return (out / "comparison.md").read_text(encoding="utf-8").split("### Host", 1)[0]
        self.assertEqual(tables(prebuilt_out), tables(built_out))

    def test_the_copied_binaries_have_no_assets_sibling(self):
        # An `assets` beside the executable would win over the tree's, so the copy directory never has one,
        # even when the producer's directory gained one.
        binaries, digest = self.produce()
        (binaries / "head" / "assets" / "fonts").mkdir(parents=True)
        code, _gate, _calls, _plans, work, _out = self.consume(binaries, digest)
        self.assertEqual(code, perf_settings.EXIT_PASS)
        for side in perf_settings.SIDES:
            self.assertFalse(os.path.lexists(work / "prebuilt" / side / "assets"))

    def test_timing_marks_are_written_in_order(self):
        # timing.json carries the run, attempt, job, shard and ordered marks the critical-path report reads.
        binaries, digest = self.produce()
        environ = self.environment(GITHUB_RUN_ATTEMPT="2", PERF_JOB_NAME="macOS before/after comparison (S7)",
                                   PERF_SHARD="S7")
        _code, _gate, _calls, _plans, _work, out = self.consume(binaries, digest, environ=environ)
        timing = json.loads((out / "timing.json").read_text(encoding="utf-8"))
        self.assertEqual((timing["run_id"], timing["run_attempt"], timing["job"], timing["shard"]),
                         ("37118041050", "2", "macOS before/after comparison (S7)", "S7"))
        marks = [timing["marks"][name] for name in perf_build_policy.TIMING_MARKS]
        self.assertEqual(marks, sorted(marks))
