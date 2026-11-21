"""Perf shards tests."""

from __future__ import annotations
import contextlib
import copy
import io
import json
from pathlib import Path
import tempfile
import unittest

import perf_shards


def minimal_document():
    """A valid partition file: one partition a platform in each profile, laps on S9 in full and release."""
    def profile(name, laps):
        partition = {"name": f"{name}-1", "variants": ["S1", "S9"]}
        if laps:
            partition["laps"] = ["S9"]
        entry = {"description": f"the {name} profile", "runs": 5, "counters_runs": 2,
                 "partitions": {"macOS": [dict(partition)], "Windows": [dict(partition)]}}
        if laps:
            entry["laps_runs"] = 2
        return entry
    return {"schema_version": 1, "profiles": {"core": profile("core", False), "full": profile("full", True),
                                              "release": profile("release", True)}}


class ShardLoaderTests(unittest.TestCase):
    """perf_shards.load_profiles reads the partition file and refuses any layout a comparison could not run."""

    def load(self, document):
        """Write `document` (or raw text) to a temporary file and load it."""
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "perf-shards.json"
            path.write_text(document if isinstance(document, str) else json.dumps(document), encoding="utf-8")
            return perf_shards.load_profiles(path)

    def refused(self, change, pattern):
        """Apply `change` to a valid document and expect the loader to refuse it with `pattern`."""
        document = copy.deepcopy(minimal_document())
        change(document)
        with self.assertRaisesRegex(perf_shards.ShardError, pattern):
            self.load(document)

    def test_the_repository_file_loads(self):
        # The file the plan job reads passes every rule, with each profile's partitions on both platforms.
        profiles = perf_shards.load_profiles()
        self.assertEqual(tuple(profiles), perf_shards.PROFILES)
        for profile in profiles.values():
            self.assertEqual(tuple(profile.partitions), perf_shards.PLATFORMS)

    def test_a_valid_document_reads_its_sampling_and_partitions(self):
        profiles = self.load(minimal_document())
        self.assertEqual((profiles["full"].runs, profiles["full"].counters_runs, profiles["full"].laps_runs), (5, 2, 2))
        self.assertIsNone(profiles["core"].laps_runs)
        self.assertEqual(profiles["full"].partitions["macOS"][0], perf_shards.Partition("full-1", ("S1", "S9"), ("S9",)))

    def test_the_document_shape_is_exact(self):
        # Unreadable JSON, another schema, a missing or extra profile and an unknown key are all refused.
        with self.assertRaisesRegex(perf_shards.ShardError, "cannot read"):
            self.load("{broken")
        self.refused(lambda document: document.update(schema_version=2), "schema_version")
        self.refused(lambda document: document["profiles"].pop("full"), "profiles must be exactly")
        self.refused(lambda document: document["profiles"].update(extra=document["profiles"]["core"]), "exactly")
        self.refused(lambda document: document.update(extra=1), "exactly schema_version and profiles")
        self.refused(lambda document: document["profiles"]["core"].update(short=True), "takes only")
        self.refused(lambda document: document["profiles"]["core"]["partitions"]["macOS"][0].update(extra=1),
                     "takes only laps")

    def test_every_profile_names_its_description_and_positive_sampling(self):
        # Zero, a negative, a float, a string or a boolean run count is refused; so is a missing description.
        for key in ("runs", "counters_runs"):
            for value in (0, -1, 2.5, "5", True):
                with self.subTest(key=key, value=value):
                    self.refused(lambda document, key=key, value=value: document["profiles"]["core"].update({key: value}),
                                 "not a positive integer")
        self.refused(lambda document: document["profiles"]["core"].pop("runs"), "has no runs")
        self.refused(lambda document: document["profiles"]["core"].update(description=" "), "no description")

    def test_partition_names_and_variant_spellings_are_checked(self):
        # A name is part of a check and an artifact name; a variant is spelled as --scenario takes it.
        for name in ("Core-1", "core 1", "core/1", "", "-core"):
            with self.subTest(name=name):
                self.refused(lambda document, name=name: document["profiles"]["core"]["partitions"]["macOS"][0]
                             .update(name=name), "lowercase word")
        for variant in ("s1", "S1/", "S1/Flood", "S", "all", 1):
            with self.subTest(variant=variant):
                self.refused(lambda document, variant=variant: document["profiles"]["core"]["partitions"]["macOS"][0]
                             .update(variants=["S1", variant]), "not S<n>")

    def test_a_variant_runs_once_per_platform(self):
        # Within a partition, across partitions, and as a bare ID beside its own default variant.
        def second_partition(document, variants):
            document["profiles"]["core"]["partitions"]["Windows"].append({"name": "core-2", "variants": variants})
        self.refused(lambda document: second_partition(document, ["S1"]), "S1/default in both core-1 and core-2")
        self.refused(lambda document: second_partition(document, ["S1/default"]), "in both")
        self.refused(lambda document: document["profiles"]["core"]["partitions"]["macOS"][0]
                     .update(variants=["S1", "S1/default"]), "twice")
        self.refused(lambda document: document["profiles"]["core"]["partitions"]["macOS"].append(
            {"name": "core-1", "variants": ["S2"]}), "names a partition twice")

    def test_laps_are_a_subset_of_the_partition_and_need_laps_runs(self):
        # Laps for a variant the partition does not run, laps without laps_runs, and laps_runs without laps.
        self.refused(lambda document: document["profiles"]["full"]["partitions"]["macOS"][0].update(laps=["S2"]),
                     "does not run")
        self.refused(lambda document: document["profiles"]["full"].pop("laps_runs"), "laps_runs exactly")
        self.refused(lambda document: document["profiles"]["core"].update(laps_runs=2), "laps_runs exactly")

    def test_both_platforms_need_partitions(self):
        self.refused(lambda document: document["profiles"]["core"]["partitions"].pop("Windows"), "exactly macOS")
        self.refused(lambda document: document["profiles"]["core"]["partitions"].update(Windows=[]), "no partitions")
        self.refused(lambda document: document["profiles"]["core"]["partitions"]["macOS"][0].update(variants=[]),
                     "nonempty")


class PartitionArgumentTests(unittest.TestCase):
    """The perf-compare.py options a partition's matrix entry carries."""

    def test_a_partition_selects_its_variants_with_its_profiles_sampling(self):
        # Variants in partition order, the profile's runs and counters runs, and laps flags only with laps.
        profiles = TestDocument.profiles()
        self.assertEqual(perf_shards.partition_arguments(profiles["core"], profiles["core"].partitions["macOS"][0]),
                         ["--scenario", "S1", "S9", "--runs", "5", "--counters", "--counters-runs", "2"])
        self.assertEqual(perf_shards.partition_arguments(profiles["full"], profiles["full"].partitions["macOS"][0]),
                         ["--scenario", "S1", "S9", "--runs", "5", "--counters", "--counters-runs", "2",
                          "--laps-scenario", "S9", "--laps-runs", "2"])

    def test_a_matrix_entry_names_its_platform_partition_and_selection(self):
        # `selection` is the arguments joined by spaces, which the compare step splits back into words.
        profile = TestDocument.profiles()["full"]
        entries = perf_shards.matrix(profile, "Windows")
        self.assertEqual(entries, [{"platform": "Windows", "partition": "full-1",
                                    "selection": " ".join(perf_shards.partition_arguments(
                                        profile, profile.partitions["Windows"][0]))}])
        self.assertEqual(entries[0]["selection"].split(), perf_shards.partition_arguments(profile, profile.partitions["Windows"][0]))


class TestDocument:
    """The minimal document, loaded through a temporary file."""

    @staticmethod
    def profiles():
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "perf-shards.json"
            path.write_text(json.dumps(minimal_document()), encoding="utf-8")
            return perf_shards.load_profiles(path)


class ShardCommandTests(unittest.TestCase):
    """`python3 scripts/perf_shards.py <profile>`: GITHUB_OUTPUT lines, or exit 1 for a broken file."""

    def run_main(self, argv):
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = perf_shards.main(argv)
        return code, stdout.getvalue(), stderr.getvalue()

    def test_the_command_prints_one_compact_line_a_platform(self):
        # Each line is `key=<json>` on one line, as GITHUB_OUTPUT needs, holding the profile's matrix.
        code, stdout, _stderr = self.run_main(["core"])
        self.assertEqual(code, 0)
        lines = stdout.splitlines()
        self.assertEqual([line.split("=", 1)[0] for line in lines], ["macos", "windows"])
        profile = perf_shards.load_profiles()["core"]
        for line, platform_name in zip(lines, perf_shards.PLATFORMS):
            self.assertEqual(json.loads(line.split("=", 1)[1]), perf_shards.matrix(profile, platform_name))

    def test_a_broken_file_exits_1_with_its_reason(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "perf-shards.json"
            document = minimal_document()
            document["schema_version"] = 9
            path.write_text(json.dumps(document), encoding="utf-8")
            code, stdout, stderr = self.run_main(["core", "--shards", str(path)])
        self.assertEqual((code, stdout), (1, ""))
        self.assertIn("perf-shards: ", stderr)
        self.assertIn("schema_version", stderr)

    def test_an_unknown_profile_is_a_usage_error(self):
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            perf_shards.main(["nightly"])
