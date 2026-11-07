#!/usr/bin/env python3
"""Regression tests for the upstream delta check of the vendored native trees."""

from __future__ import annotations

import hashlib
import importlib.util
import contextlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time
import unittest
from unittest import mock

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


def tagged_repository(directory: Path, tags: list[str]) -> str:
    """A local repository holding one commit under each of `tags`: the fake upstream index
    `git ls-remote` reads in place of a hosted repository."""
    def git(*arguments: str) -> None:
        subprocess.run(["git", "-c", "user.name=fixture", "-c", "user.email=fixture@invalid",
                        "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", *arguments],
                       cwd=directory, check=True, capture_output=True)
    git("init", "-q")
    git("commit", "-q", "--allow-empty", "-m", "fixture")
    for tag in tags:
        git("tag", tag)
    return str(directory)


class LatestTests(unittest.TestCase):
    """`--latest` reads stable versions from one bounded tag listing and reports, never fails."""

    PATTERN = r"v(\d+)\.(\d+)(?:\.(\d+))?"

    def library(self, url: str, version: str = "1.9.0", tag: str = "v1.9.0") -> dict:
        return {"name": "demo", "version": version, "tag": tag, "local_modifications": [],
                "latest_source": {"provider": "git-tags", "url": url, "pattern": self.PATTERN}}

    def test_the_newest_stable_tag_wins_numerically_and_pre_releases_are_skipped(self):
        # `v1.10` beats `v1.9.2` numerically, though not as text, and a two-part tag pads with
        # zero; a beta or rc tag never matches the full-match pattern, however new it is.
        with tempfile.TemporaryDirectory() as directory:
            url = tagged_repository(Path(directory),
                                    ["v1.9.0", "v1.9.2", "v1.10", "v1.11.0beta3", "v2.0.0-rc.1"])
            line = delta.latest_line(self.library(url))
        self.assertEqual(line, "update demo pinned 1.9.0 latest 1.10 (tag v1.10), "
                               "0 local modification(s)")

    def test_a_pinned_newest_release_is_current(self):
        with tempfile.TemporaryDirectory() as directory:
            url = tagged_repository(Path(directory), ["v1.8.0", "v1.9.0", "v1.9.1beta1"])
            self.assertTrue(delta.latest_line(self.library(url)).startswith("current demo"))

    def test_a_failed_lookup_is_reported_per_library_and_the_run_exits_zero(self):
        # One unreachable upstream must not hide the others' answers or fail the report.
        with tempfile.TemporaryDirectory() as directory:
            url = tagged_repository(Path(directory), ["v1.9.0"])
            missing = str(Path(directory) / "missing")
            libraries = [self.library(missing) | {"name": "gone"}, self.library(url)]
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                self.assertEqual(delta.report_latest(libraries), 0)
        lines = stdout.getvalue().splitlines()
        self.assertTrue(lines[0].startswith("error gone: git ls-remote"), lines)
        self.assertTrue(lines[1].startswith("current demo"), lines)

    def test_a_listing_over_the_size_cap_is_refused(self):
        # The cap holds while git is still writing: the reader kills it rather than buffering on.
        with tempfile.TemporaryDirectory() as directory:
            url = tagged_repository(Path(directory), ["v1.{}.0".format(minor) for minor in range(40)])
            with mock.patch.object(delta, "LATEST_MAX_BYTES", 256):
                line = delta.latest_line(self.library(url))
        self.assertRegex(line, r"^error demo: .*more than 256 bytes")

    @unittest.skipIf(os.name == "nt", "the stand-in git is a POSIX shell script")
    def test_a_listing_over_the_timeout_is_killed_with_its_helpers(self):
        # A stand-in `git` that starts a helper holding stdout, as a transport helper does, then
        # never answers: the call still ends at the deadline, and the helper is killed with it.
        with tempfile.TemporaryDirectory() as directory:
            fake_git = Path(directory) / "git"
            marker = Path(directory) / "helper-alive"
            fake_git.write_text("#!/bin/sh\n(sleep 3; touch '{}') &\nexec sleep 30\n".format(marker))
            fake_git.chmod(0o755)
            path = "{}{}{}".format(directory, os.pathsep, os.environ.get("PATH", ""))
            started = time.monotonic()
            with mock.patch.object(delta, "LATEST_TIMEOUT_S", 0.3), \
                    mock.patch.dict(os.environ, {"PATH": path}):
                line = delta.latest_line(self.library("https://example.invalid/demo.git"))
            elapsed = time.monotonic() - started
            time.sleep(3.5)
            helper_survived = marker.exists()
        self.assertRegex(line, r"^error demo: .*took over 0.3 s")
        self.assertLess(elapsed, 1.0, "the deadline bounded the call")
        self.assertFalse(helper_survived, "the helper outlived the deadline")

    @unittest.skipIf(os.name == "nt", "the stand-in git is a POSIX shell script")
    def test_output_past_the_cap_ends_a_listing_still_running(self):
        # A stand-in `git` that writes past the cap on stdout or stderr and then keeps running: the
        # call ends on the size while git is still alive, long before its deadline.
        for stream in ("", " 1>&2"):
            with self.subTest(stream=stream or "stdout"), tempfile.TemporaryDirectory() as directory:
                fake_git = Path(directory) / "git"
                fake_git.write_text("#!/bin/sh\nhead -c 4096 /dev/zero{}\nexec sleep 30\n".format(stream))
                fake_git.chmod(0o755)
                path = "{}{}{}".format(directory, os.pathsep, os.environ.get("PATH", ""))
                started = time.monotonic()
                with mock.patch.object(delta, "LATEST_MAX_BYTES", 256), \
                        mock.patch.object(delta, "LATEST_TIMEOUT_S", 20), \
                        mock.patch.dict(os.environ, {"PATH": path}):
                    line = delta.latest_line(self.library("https://example.invalid/demo.git"))
                elapsed = time.monotonic() - started
                self.assertRegex(line, r"^error demo: .*more than 256 bytes")
                self.assertLess(elapsed, 2.0, "the size, not the deadline, ended the call")

    @unittest.skipIf(os.name == "nt", "the stand-in git is a POSIX shell script")
    def test_a_finished_listing_past_the_cap_on_either_stream_is_refused(self):
        # A stand-in `git` that writes past the cap on stdout or stderr, prints one valid tag and
        # exits at once, before any poll could see it running: the finished output is still capped.
        for stream in ("", " 1>&2"):
            with self.subTest(stream=stream or "stdout"), tempfile.TemporaryDirectory() as directory:
                fake_git = Path(directory) / "git"
                fake_git.write_text("#!/bin/sh\nhead -c 4096 /dev/zero | tr '\\0' x{}\n"
                                    "printf '0000 refs/tags/v1.0.0\\n'\n".format(stream))
                fake_git.chmod(0o755)
                path = "{}{}{}".format(directory, os.pathsep, os.environ.get("PATH", ""))
                with mock.patch.object(delta, "LATEST_MAX_BYTES", 256), \
                        mock.patch.object(delta, "LATEST_POLL_S", 5), \
                        mock.patch.dict(os.environ, {"PATH": path}):
                    with self.assertRaisesRegex(delta.DependencyError, "more than 256 bytes"):
                        delta.list_tags("https://example.invalid/demo.git")

    def test_latest_source_is_validated(self):
        # Each malformed source is refused before any lookup, and the pattern must read the
        # pinned tag back as the pinned version, or the comparison would be meaningless.
        cases = (
            ({"provider": "rss", "url": "u", "pattern": self.PATTERN}, "provider"),
            ({"provider": "git-tags", "url": "u"}, "exactly"),
            ({"provider": "git-tags", "url": "u", "pattern": "("}, "invalid"),
            ({"provider": "git-tags", "url": "u", "pattern": "v1.9.0"}, "captures no"),
            ({"provider": "git-tags", "url": "u", "pattern": r"(\d+)\.(\d+)\.(\d+)"}, "does not read"),
        )
        for source, message in cases:
            with self.subTest(source=source):
                library = self.library("u") | {"latest_source": source}
                with self.assertRaisesRegex(delta.DependencyError, message):
                    delta.latest_source(library)
                self.assertTrue(delta.latest_line(library).startswith("error demo:"))

    def test_every_shipped_library_names_a_valid_latest_source(self):
        # Offline: each pinned tag reads back as its pinned version, so `--latest` can compare.
        libraries = verifier.load_manifest(_REPO_ROOT / "scripts" / "native-dependencies.json")
        for library in libraries:
            with self.subTest(library=library["name"]):
                self.assertEqual(delta.latest_source(library)["provider"], "git-tags")


if __name__ == "__main__":
    unittest.main(verbosity=2)
