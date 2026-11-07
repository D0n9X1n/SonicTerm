#!/usr/bin/env python3
"""Regression tests for the upstream delta check of the vendored native trees."""

from __future__ import annotations

import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

_HERE = Path(__file__).resolve().parent
_REPO_ROOT = _HERE.parent


def _load(module_name: str, file_name: str):
    spec = importlib.util.spec_from_file_location(module_name, _HERE / file_name)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


delta = _load("native_dependency_delta", "native-dependency-delta.py")
verifier = delta.verifier
fixtures = _load("native_dependencies_tests", "native-dependencies_tests.py")
Fixture = fixtures.Fixture


def archive_bytes(root: str, files: dict[str, bytes]) -> bytes:
    """A gzipped tar holding `files` under the directory `root`, like a release archive."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as bundle:
        for relative, data in files.items():
            member = tarfile.TarInfo("{}/{}".format(root, relative))
            member.size = len(data)
            bundle.addfile(member, io.BytesIO(data))
    return buffer.getvalue()


class DiffTests(unittest.TestCase):
    """`diff` compares a tree with its pinned archive and holds the delta to `local_modifications`."""

    UPSTREAM = {"a.txt": b"alpha\n", "sub/b.txt": b"beta\n", "sub/deep/c.txt": b"gamma\n",
                "excluded.txt": b"not in the include subset\n"}

    def library(self, fixture: Fixture, archive: bytes, modifications: list[str]) -> dict:
        return fixture.manifest(
            archive={"url": "https://example.invalid/demo.tar.gz",
                     "sha256": hashlib.sha256(archive).hexdigest(), "root": "demo-1.0"},
            local_modifications=[{"path": path, "reason": "fixture"} for path in modifications],
        )["libraries"][0]

    def run_diff(self, local: dict[str, bytes], modifications: list[str]) -> list[str]:
        """The disagreements `diff_library` reports for a tree holding `local`."""
        archive = archive_bytes("demo-1.0", self.UPSTREAM)
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            for relative, data in local.items():
                path = fixture.tree / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            library = self.library(fixture, archive, modifications)
            return delta.diff_library(fixture.root, library, fetch=lambda _url: archive)

    def matching_tree(self) -> dict[str, bytes]:
        """The archive's include subset, unchanged: the excluded file is not retained."""
        return {path: data for path, data in self.UPSTREAM.items() if path != "excluded.txt"}

    def test_an_unchanged_tree_with_no_modifications_agrees(self):
        # The include subset decides what is compared; a file outside it is never expected locally.
        self.assertEqual(self.run_diff(self.matching_tree(), []), [])

    def test_an_unlisted_change_is_reported(self):
        tree = self.matching_tree() | {"sub/b.txt": b"beta, patched\n"}
        problems = self.run_diff(tree, [])
        self.assertEqual(problems, ["library demo sub/b.txt differs from upstream but is not in "
                                    "local_modifications"])

    def test_a_listed_change_agrees(self):
        tree = self.matching_tree() | {"sub/b.txt": b"beta, patched\n"}
        self.assertEqual(self.run_diff(tree, ["sub/b.txt"]), [])

    def test_a_listed_file_that_matches_upstream_is_reported(self):
        # A recorded modification that was lost, say by an upgrade, must not pass silently.
        problems = self.run_diff(self.matching_tree(), ["a.txt"])
        self.assertEqual(problems, ["library demo lists a.txt in local_modifications but it "
                                    "matches upstream"])

    def test_an_added_file_is_a_modification(self):
        tree = self.matching_tree() | {"sub/deep/added_tests.rs": b"// local\n"}
        self.assertEqual(len(self.run_diff(tree, [])), 1)
        self.assertEqual(self.run_diff(tree, ["sub/deep/added_tests.rs"]), [])

    def test_an_archive_whose_sha256_differs_is_refused(self):
        archive = archive_bytes("demo-1.0", self.UPSTREAM)
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            library = self.library(fixture, archive, [])
            with self.assertRaisesRegex(delta.DependencyError, "archive sha256"):
                delta.diff_library(fixture.root, library, fetch=lambda _url: archive + b"x")

    def test_include_globs_match_per_segment(self):
        # `*` stays inside one directory; a trailing `**` takes every depth below its directory.
        self.assertTrue(delta._included("src/lib.rs", ["src/*.rs"]))
        self.assertFalse(delta._included("src/platform/mod.rs", ["src/*.rs"]))
        self.assertTrue(delta._included("src/platform_impl/windows/keyboard.rs",
                                       ["src/platform_impl/windows/**"]))
        self.assertFalse(delta._included("src/platform_impl/web/mod.rs",
                                        ["src/platform_impl/windows/**"]))
        self.assertFalse(delta._included("src/platform_impl/windows", ["src/platform_impl/windows/**"]))

    def test_local_modifications_entries_are_validated(self):
        for entry, message in (({"path": "a.txt"}, "exactly"), ({"path": "a.txt", "reason": " "}, "non-empty"),
                               ("a.txt", "exactly")):
            with self.subTest(entry=entry), tempfile.TemporaryDirectory() as directory:
                fixture = Fixture(Path(directory))
                fixture.write_manifest(fixture.manifest(local_modifications=[entry]))
                with self.assertRaisesRegex(delta.DependencyError, message):
                    verifier.load_manifest(fixture.manifest_path)
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            twice = [{"path": "a.txt", "reason": "one"}, {"path": "a.txt", "reason": "two"}]
            fixture.write_manifest(fixture.manifest(local_modifications=twice))
            with self.assertRaisesRegex(delta.DependencyError, "twice"):
                verifier.load_manifest(fixture.manifest_path)

    def test_the_shipped_manifest_records_the_winit_delta(self):
        # The reviewed delta: thirteen winit files, including the Windows keyboard metadata seam.
        libraries = verifier.load_manifest(_REPO_ROOT / "scripts" / "native-dependencies.json")
        winit = verifier.select_library(libraries, "winit")
        paths = {modification["path"] for modification in winit["local_modifications"]}
        self.assertEqual(len(paths), 13)
        self.assertIn("src/platform/windows.rs", paths)
        self.assertIn("src/platform_impl/windows/keyboard_tests.rs", paths)



if __name__ == "__main__":
    unittest.main(verbosity=2)
