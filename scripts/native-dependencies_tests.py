#!/usr/bin/env python3
"""Regression tests for the offline native-dependency maintenance tool."""

from __future__ import annotations

import errno
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import stat
import tarfile
import tempfile
import unittest
from unittest import mock

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "native_dependencies", _HERE / "native-dependencies.py"
)
assert _SPEC is not None and _SPEC.loader is not None
tool = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(tool)

# Precomputed outside the tool from the documented framing: sha256 over
# b"sonicterm-native-tree-v1\n" + count + b"\n" and then, per sorted path,
# len(path) + b"\n" + path + len(bytes) + b"\n" + bytes.
PINNED_TWO_FILE_TREE = "6196283baec24038d2c66d7b8bd519e86c8bd9e18dce96e7c49f6bbcf4317d4d"
PINNED_EMPTY_TREE = "dc1b85cc111ad965f947938798d33b98ffcc90b724dacac0b2153368eb15df93"

HAVE_GIT = shutil.which("git") is not None


def create_test_symlink(target, link: Path, *, directory: bool = False) -> None:
    """Skip only unavailable symlink capability, never unrelated fixture errors."""
    try:
        os.symlink(target, link, target_is_directory=directory)
    except OSError as error:
        if error.errno in (errno.EPERM, errno.EACCES, errno.ENOSYS, errno.ENOTSUP) or getattr(error, "winerror", None) == 1314:
            raise unittest.SkipTest("host does not permit symlink creation") from error
        raise


def reference_tree_hash(root: Path) -> str:
    """Re-derive the digest from the written spec rather than from the tool's own code."""
    records = []
    for current, _directories, names in os.walk(root):
        for name in names:
            absolute = Path(current) / name
            relative = absolute.relative_to(root).as_posix().encode("utf-8")
            records.append((relative, absolute.read_bytes()))
    records.sort(key=lambda item: item[0])
    buffer = io.BytesIO()
    buffer.write(b"sonicterm-native-tree-v1\n")
    buffer.write(str(len(records)).encode("ascii") + b"\n")
    for relative, data in records:
        buffer.write(str(len(relative)).encode("ascii") + b"\n" + relative)
        buffer.write(str(len(data)).encode("ascii") + b"\n" + data)
    return hashlib.sha256(buffer.getvalue()).hexdigest()


def write_file(path: Path, data: bytes | str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data.encode("utf-8") if isinstance(data, str) else data)
    return path


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_tar(path: Path, entries) -> Path:
    """Build a tar from declarative members so hostile shapes stay expressible."""
    with tarfile.open(path, "w") as archive:
        for entry in entries:
            kind = entry.get("kind", "file")
            info = tarfile.TarInfo(entry["name"])
            info.mode = entry.get("mode", 0o644)
            if kind == "file":
                payload = entry["data"]
                if isinstance(payload, str):
                    payload = payload.encode("utf-8")
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
            elif kind == "dir":
                info.type = tarfile.DIRTYPE
                info.mode = entry.get("mode", 0o755)
                archive.addfile(info)
            elif kind == "symlink":
                info.type = tarfile.SYMTYPE
                info.linkname = entry["target"]
                archive.addfile(info)
            elif kind == "hardlink":
                info.type = tarfile.LNKTYPE
                info.linkname = entry["target"]
                archive.addfile(info)
            elif kind == "device":
                info.type = tarfile.CHRTYPE
                info.devmajor, info.devminor = 1, 3
                archive.addfile(info)
            else:  # pragma: no cover - guards fixture typos
                raise AssertionError("unknown fixture member kind {}".format(kind))
    return path


class Fixture:
    """A throwaway repository root holding one manifest-described library tree."""

    def __init__(self, root: Path):
        self.root = root
        self.library_path = "crates/demo/upstream"
        self.tree = root / self.library_path
        self.manifest_path = root / "scripts" / "native-dependencies.json"

    def populate_tree(self) -> None:
        write_file(self.tree / "a.txt", "alpha\n")
        write_file(self.tree / "sub" / "b.txt", "beta\n")

    def manifest(self, **overrides) -> dict:
        library = {
            "name": "demo",
            "path": self.library_path,
            "version": "1.0",
            "archive": {
                "url": "https://example.invalid/demo-1.0.tar",
                "sha256": "0" * 64,
                "root": "demo-1.0",
            },
            "revision": None,
            "tag": None,
            "include": ["a.txt", "sub/**"],
            "patches": [],
            "tree_sha256": PINNED_TWO_FILE_TREE,
        }
        library.update(overrides)
        return {"schema_version": 1, "libraries": [library]}

    def write_manifest(self, document: dict) -> None:
        write_file(self.manifest_path, json.dumps(document, indent=2))

    def run(self, *arguments) -> int:
        return tool.main(
            list(arguments)
            + ["--manifest", str(self.manifest_path), "--repo-root", str(self.root)]
        )


class TreeHashTests(unittest.TestCase):
    def test_symlink_capability_skip_does_not_hide_unrelated_errors(self):
        # Ordinary Windows accounts may lack symlink permission; other fixture failures must remain visible.
        denied = OSError("Windows symlink privilege is absent")
        denied.winerror = 1314
        for error in (denied, PermissionError(errno.EPERM, "not permitted")):
            with mock.patch("os.symlink", side_effect=error):
                with self.assertRaises(unittest.SkipTest):
                    create_test_symlink("target", Path("link"))
        with mock.patch("os.symlink", side_effect=FileExistsError(errno.EEXIST, "exists")):
            with self.assertRaises(FileExistsError):
                create_test_symlink("target", Path("link"))

    def test_pinned_framing_matches_an_independent_expectation(self):
        # Protect every manifest hash from a silent change of framing or ordering.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            self.assertEqual(tool.tree_sha256(fixture.tree), PINNED_TWO_FILE_TREE)
            self.assertEqual(reference_tree_hash(fixture.tree), PINNED_TWO_FILE_TREE)

    def test_empty_tree_keeps_its_pinned_header(self):
        # Protect the count header from vanishing when a tree holds no files.
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(tool.tree_sha256(Path(directory)), PINNED_EMPTY_TREE)

    def test_length_delimiting_separates_path_from_content(self):
        # Protect against ambiguity: moving bytes between name and data must change the digest.
        with tempfile.TemporaryDirectory() as directory:
            first, second = Path(directory) / "first", Path(directory) / "second"
            write_file(first / "ab", "cd")
            write_file(second / "a", "bcd")
            self.assertNotEqual(tool.tree_sha256(first), tool.tree_sha256(second))
            self.assertEqual(tool.tree_sha256(first), reference_tree_hash(first))

    def test_mode_is_not_hashed_but_links_are_rejected(self):
        # Protect Windows checkouts from mode drift while refusing entries the digest cannot describe.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            (fixture.tree / "a.txt").chmod(0o755)
            self.assertEqual(tool.tree_sha256(fixture.tree), PINNED_TWO_FILE_TREE)
            create_test_symlink("a.txt", fixture.tree / "link.txt")
            with self.assertRaises(tool.DependencyError) as caught:
                tool.tree_sha256(fixture.tree)
            self.assertIn("link.txt", str(caught.exception))

    def test_symlinked_directory_is_rejected_and_never_followed(self):
        # Protect the walk from descending out of the tree through a directory link.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            create_test_symlink(directory, fixture.tree / "loop", directory=True)
            with self.assertRaises(tool.DependencyError):
                tool.tree_sha256(fixture.tree)


class CheckTests(unittest.TestCase):
    def test_pinned_tree_passes(self):
        # Protect the ordinary offline verification path from regressing into a false failure.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            fixture.write_manifest(fixture.manifest())
            self.assertEqual(fixture.run("check"), 0)

    def test_missing_extra_and_modified_files_all_fail(self):
        # Protect vendored trees from deletion, unreviewed additions, and in-place edits alike.
        mutations = (
            ("missing", lambda tree: (tree / "sub" / "b.txt").unlink()),
            ("extra", lambda tree: write_file(tree / "extra.txt", "x")),
            ("modified", lambda tree: write_file(tree / "a.txt", "alpha!\n")),
        )
        for label, mutate in mutations:
            with self.subTest(mutation=label):
                with tempfile.TemporaryDirectory() as directory:
                    fixture = Fixture(Path(directory))
                    fixture.populate_tree()
                    fixture.write_manifest(fixture.manifest())
                    mutate(fixture.tree)
                    self.assertEqual(fixture.run("check"), 1)

    def test_include_globs_do_not_narrow_verification(self):
        # Protect against an unreviewed file hiding in a directory the include list omits.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            fixture.write_manifest(fixture.manifest(include=["a.txt"]))
            write_file(fixture.tree / "docs" / "smuggled.txt", "payload")
            self.assertEqual(fixture.run("check"), 1)

    def test_null_tree_hash_is_rejected_by_check(self):
        # Protect the manifest from staying in its unpinned bootstrap state.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            fixture.write_manifest(fixture.manifest(tree_sha256=None))
            self.assertEqual(fixture.run("check"), 1)

    def test_missing_tree_is_reported_not_treated_as_empty(self):
        # Protect against an absent vendor directory verifying as the empty-tree digest.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.write_manifest(fixture.manifest())
            self.assertEqual(fixture.run("check"), 1)

    def test_patch_hash_is_verified_from_the_repository_root(self):
        # Protect applied patches from being edited after review without failing verification.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            patch = write_file(fixture.root / "scripts" / "patches" / "p.patch", "diff\n")
            entry = {
                "path": "scripts/patches/p.patch",
                "sha256": sha256_file(patch),
                "revision": None,
                "url": None,
            }
            fixture.write_manifest(fixture.manifest(patches=[entry]))
            self.assertEqual(fixture.run("check"), 0)
            write_file(patch, "diff tampered\n")
            self.assertEqual(fixture.run("check"), 1)
            patch.unlink()
            self.assertEqual(fixture.run("check"), 1)

    def test_optional_version_check_reads_one_capture_group(self):
        # Protect the pinned version from drifting out of the header the build compiles.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            write_file(fixture.tree / "a.txt", "alpha\n")
            write_file(
                fixture.tree / "sub" / "b.txt",
                '#define DEMO_VERSION_STRING "1.0"\n',
            )
            tree_hash = tool.tree_sha256(fixture.tree)
            single = {
                "path": "sub/b.txt",
                "pattern": 'DEMO_VERSION_STRING\\s+"([^"]+)"',
                "value": "1.0",
            }
            cases = (
                ("single object", single, 0),
                ("wrong value", dict(single, value="9.9"), 1),
                ("absent pattern", dict(single, pattern='DEMO_ABSENT\\s+"([^"]+)"'), 1),
                ("missing file", dict(single, path="missing.h"), 1),
                ("no capture group", dict(single, pattern="DEMO_VERSION_STRING"), 1),
            )
            for label, check, expected in cases:
                with self.subTest(case=label):
                    fixture.write_manifest(
                        fixture.manifest(tree_sha256=tree_hash, version_check=check)
                    )
                    self.assertEqual(fixture.run("check"), expected)

    def test_version_check_accepts_a_list_for_split_macros(self):
        # Protect split version macros, which one capture cannot express, from drifting apart.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            write_file(fixture.tree / "a.txt", "alpha\n")
            write_file(
                fixture.tree / "sub" / "b.txt",
                "#define DEMO_MAJOR 1\n#define DEMO_MINOR 0\n",
            )
            tree_hash = tool.tree_sha256(fixture.tree)
            checks = [
                {"path": "sub/b.txt", "pattern": "DEMO_MAJOR\\s+(\\d+)", "value": "1"},
                {"path": "sub/b.txt", "pattern": "DEMO_MINOR\\s+(\\d+)", "value": "0"},
            ]
            fixture.write_manifest(
                fixture.manifest(tree_sha256=tree_hash, version_check=checks)
            )
            self.assertEqual(fixture.run("check"), 0)

            checks[1]["value"] = "9"
            fixture.write_manifest(
                fixture.manifest(tree_sha256=tree_hash, version_check=checks)
            )
            self.assertEqual(fixture.run("check"), 1)

    def test_unknown_schema_version_and_unknown_library_fail(self):
        # Protect the tool from misreading a future manifest or accepting a typo'd library name.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            fixture.write_manifest(fixture.manifest())
            self.assertEqual(fixture.run("check", "--library", "absent"), 1)
            fixture.write_manifest({"schema_version": 2, "libraries": []})
            self.assertEqual(fixture.run("check"), 1)


class StageArchiveSafetyTests(unittest.TestCase):
    def stage_with(self, fixture: Fixture, entries, output: Path, **overrides) -> int:
        archive = build_tar(fixture.root / "demo.tar", entries)
        archive_meta = {
            "url": "https://example.invalid/demo-1.0.tar",
            "sha256": overrides.pop("archive_sha256", sha256_file(archive)),
            "root": overrides.pop("archive_root", "demo-1.0"),
        }
        fixture.write_manifest(
            fixture.manifest(archive=archive_meta, tree_sha256=None, **overrides)
        )
        return fixture.run(
            "stage", "demo", "--archive", str(archive), "--output", str(output)
        )

    def test_wrong_archive_hash_stages_nothing(self):
        # Protect the staging root from a substituted or truncated download.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            code = self.stage_with(
                fixture,
                [{"name": "demo-1.0/a.txt", "data": "alpha\n"}],
                output,
                archive_sha256="1" * 64,
            )
            self.assertEqual(code, 1)
            self.assertFalse(output.exists())

    def test_unsafe_member_paths_are_refused_anywhere_in_the_archive(self):
        # Protect the host filesystem from escape and from a conflicting duplicate member.
        hostile = {
            "absolute": [{"name": "/etc/passwd", "data": "x"}],
            "traversal": [{"name": "demo-1.0/../../escape.txt", "data": "x"}],
            # Rejected although the duplicate sits outside the include list.
            "duplicate": [
                {"name": "demo-1.0/docs/d.txt", "data": "one"},
                {"name": "demo-1.0/docs/d.txt", "data": "two"},
            ],
        }
        for label, entries in hostile.items():
            with self.subTest(member=label):
                with tempfile.TemporaryDirectory() as directory:
                    fixture = Fixture(Path(directory))
                    output = fixture.root / "stage"
                    self.assertEqual(self.stage_with(fixture, entries, output), 1)
                    self.assertFalse(output.exists())
                    self.assertFalse((fixture.root / "escape.txt").exists())

    def test_selected_links_and_devices_are_refused(self):
        # Protect the staged tree from members the tree digest cannot describe.
        hostile = {
            "symlink": [{"name": "demo-1.0/sub/link", "kind": "symlink", "target": "/etc/passwd"}],
            "hardlink": [{"name": "demo-1.0/sub/hard", "kind": "hardlink", "target": "demo-1.0/a.txt"}],
            "device": [{"name": "demo-1.0/sub/null", "kind": "device"}],
        }
        for label, extra in hostile.items():
            with self.subTest(member=label):
                with tempfile.TemporaryDirectory() as directory:
                    fixture = Fixture(Path(directory))
                    output = fixture.root / "stage"
                    entries = [
                        {"name": "demo-1.0/a.txt", "data": "alpha\n"},
                        {"name": "demo-1.0/sub/b.txt", "data": "beta\n"},
                    ] + extra
                    self.assertEqual(self.stage_with(fixture, entries, output), 1)
                    self.assertFalse(output.exists())

    def test_excluded_benign_symlink_does_not_fail_a_valid_release(self):
        # Protect real upstream releases, which ship root instruction symlinks we never select.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            entries = [
                {"name": "demo-1.0/a.txt", "data": "alpha\n"},
                {"name": "demo-1.0/sub/b.txt", "data": "beta\n"},
                {"name": "demo-1.0/AGENTS.md", "data": "upstream instructions\n"},
                {"name": "demo-1.0/CLAUDE.md", "kind": "symlink", "target": "AGENTS.md"},
            ]
            self.assertEqual(self.stage_with(fixture, entries, output), 0)
            self.assertFalse((output / "CLAUDE.md").exists())
            self.assertFalse((output / "AGENTS.md").exists())
            self.assertEqual(tool.tree_sha256(output), PINNED_TWO_FILE_TREE)

    def test_entry_count_and_byte_bounds_are_enforced(self):
        # Protect the host from an archive bomb before any member reaches the disk.
        entries = [
            {"name": "demo-1.0/f{}.txt".format(index), "data": "x" * 16}
            for index in range(6)
        ]
        for attribute, value in (("MAX_ARCHIVE_ENTRIES", 3), ("MAX_ARCHIVE_BYTES", 32)):
            with self.subTest(bound=attribute):
                original = getattr(tool, attribute)
                setattr(tool, attribute, value)
                try:
                    with tempfile.TemporaryDirectory() as directory:
                        fixture = Fixture(Path(directory))
                        output = fixture.root / "stage"
                        self.assertEqual(self.stage_with(fixture, entries, output), 1)
                        self.assertFalse(output.exists())
                finally:
                    setattr(tool, attribute, original)

    def test_existing_non_empty_output_is_preserved(self):
        # Protect an existing checkout or vendor tree from being overwritten or deleted.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            keep = write_file(output / "keep.txt", "precious")
            code = self.stage_with(
                fixture, [{"name": "demo-1.0/a.txt", "data": "alpha\n"}], output
            )
            self.assertEqual(code, 1)
            self.assertEqual(keep.read_text(encoding="utf-8"), "precious")
            self.assertFalse((output / "a.txt").exists())

    def test_output_symlink_cannot_redirect_publication(self):
        # A caller's output path must not silently write into a different directory through a link.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            elsewhere = fixture.root / "elsewhere"
            elsewhere.mkdir()
            output = fixture.root / "stage"
            create_test_symlink(elsewhere, output, directory=True)
            self.assertEqual(self.stage_with(
                fixture, [{"name": "demo-1.0/a.txt", "data": "alpha\n"}], output
            ), 1)
            self.assertEqual(list(elsewhere.iterdir()), [])

    def test_staging_checks_the_declared_version(self):
        # An unpinned candidate still must match the version the maintainer intended to import.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            check = {"path": "a.txt", "pattern": r"VERSION (\d+)", "value": "2"}
            self.assertEqual(self.stage_with(
                fixture, [{"name": "demo-1.0/a.txt", "data": "VERSION 1\n"}],
                output, version_check=check
            ), 1)
            self.assertFalse(output.exists())

    def test_missing_archive_root_fails(self):
        # Protect against a renamed upstream top-level directory staging an empty tree.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            code = self.stage_with(
                fixture, [{"name": "other-2.0/a.txt", "data": "alpha\n"}], output
            )
            self.assertEqual(code, 1)
            self.assertFalse(output.exists())

    def test_include_filters_and_root_is_stripped(self):
        # Protect the bootstrap workflow: stage the selected subset under a stripped root.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            entries = [
                {"name": "demo-1.0", "kind": "dir"},
                {"name": "demo-1.0/a.txt", "data": "alpha\n"},
                {"name": "demo-1.0/sub", "kind": "dir"},
                {"name": "demo-1.0/sub/nested", "kind": "dir"},
                {"name": "demo-1.0/sub/b.txt", "data": "beta\n"},
                {"name": "demo-1.0/docs/manual.txt", "data": "excluded"},
            ]
            self.assertEqual(self.stage_with(fixture, entries, output), 0)
            self.assertTrue((output / "a.txt").exists())
            self.assertTrue((output / "sub" / "b.txt").exists())
            self.assertFalse((output / "docs").exists())
            self.assertEqual(tool.tree_sha256(output), PINNED_TWO_FILE_TREE)

    def test_staged_files_are_not_executable(self):
        # Protect maintainers from an upstream build script arriving pre-armed for execution.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            entries = [
                {"name": "demo-1.0/a.txt", "data": "alpha\n"},
                {"name": "demo-1.0/configure", "data": "#!/bin/sh\n", "mode": 0o777},
            ]
            self.assertEqual(self.stage_with(fixture, entries, output, include=["**"]), 0)
            self.assertFalse((output / "configure").stat().st_mode & stat.S_IXUSR)

    def test_upstream_scripts_are_never_executed(self):
        # Protect the host from an archive whose own build scripts would run during staging.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            marker = fixture.root / "executed.marker"
            script = "#!/bin/sh\ntouch {}\n".format(marker)
            entries = [
                {"name": "demo-1.0/a.txt", "data": "alpha\n"},
                {"name": "demo-1.0/configure", "data": script, "mode": 0o777},
                {"name": "demo-1.0/autogen.sh", "data": script, "mode": 0o777},
                {"name": "demo-1.0/Makefile", "data": "all:\n\ttouch {}\n".format(marker)},
            ]
            self.assertEqual(self.stage_with(fixture, entries, output, include=["**"]), 0)
            self.assertFalse(marker.exists())


@unittest.skipUnless(HAVE_GIT, "git is required to apply patches")
class StagePatchTests(unittest.TestCase):
    def stage(self, fixture: Fixture, output: Path, patches, tree_hash=None) -> int:
        archive = build_tar(
            fixture.root / "demo.tar",
            [
                {"name": "demo-1.0/a.txt", "data": "alpha\n"},
                {"name": "demo-1.0/sub/b.txt", "data": "beta\n"},
            ],
        )
        fixture.write_manifest(
            fixture.manifest(
                archive={
                    "url": "https://example.invalid/demo-1.0.tar",
                    "sha256": sha256_file(archive),
                    "root": "demo-1.0",
                },
                patches=patches,
                tree_sha256=tree_hash,
            )
        )
        return fixture.run(
            "stage", "demo", "--archive", str(archive), "--output", str(output)
        )

    def patch_entry(self, fixture: Fixture, name: str, body: str) -> dict:
        path = write_file(fixture.root / "scripts" / "patches" / name, body)
        return {
            "path": "scripts/patches/{}".format(name),
            "sha256": sha256_file(path),
            "revision": None,
            "url": None,
        }

    def test_patch_applies_and_the_final_tree_hash_is_verified(self):
        # Protect the staged result from differing from the reviewed, pinned tree.
        body = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-alpha\n+patched\n"
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            entry = self.patch_entry(fixture, "one.patch", body)
            self.assertEqual(self.stage(fixture, output, [entry]), 0)
            self.assertEqual((output / "a.txt").read_text(encoding="utf-8"), "patched\n")
            staged = tool.tree_sha256(output)

            matching = fixture.root / "stage-pinned"
            self.assertEqual(self.stage(fixture, matching, [entry], tree_hash=staged), 0)

            mismatched = fixture.root / "stage-mismatched"
            self.assertEqual(self.stage(fixture, mismatched, [entry], tree_hash="2" * 64), 1)
            self.assertFalse(mismatched.exists())

    def test_tampered_patch_is_refused_before_application(self):
        # Protect applied changes from an edit made after the patch was reviewed.
        body = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-alpha\n+patched\n"
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            entry = self.patch_entry(fixture, "one.patch", body)
            entry["sha256"] = "3" * 64
            self.assertEqual(self.stage(fixture, output, [entry]), 1)
            self.assertFalse(output.exists())

    def test_patch_that_does_not_apply_leaves_no_stage_behind(self):
        # Protect the host from a half-patched tree when a patch rebase is overdue.
        body = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-nomatch\n+patched\n"
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "nested" / "stage"
            keep = write_file(fixture.root / "nested" / "keep.txt", "precious")
            entry = self.patch_entry(fixture, "bad.patch", body)
            self.assertEqual(self.stage(fixture, output, [entry]), 1)
            self.assertFalse(output.exists())
            self.assertEqual(keep.read_text(encoding="utf-8"), "precious")

    def test_patch_paths_may_not_escape_the_stage_root(self):
        # Protect files outside the staging directory from a traversal in a patch header.
        bodies = {
            "traversal": "--- a/../escape.txt\n+++ b/../escape.txt\n@@ -0,0 +1 @@\n+x\n",
            "absolute": "--- a/tmp/escape.txt\n+++ /tmp/escape.txt\n@@ -0,0 +1 @@\n+x\n",
        }
        for label, body in bodies.items():
            with self.subTest(patch=label):
                with tempfile.TemporaryDirectory() as directory:
                    fixture = Fixture(Path(directory))
                    output = fixture.root / "stage"
                    entry = self.patch_entry(fixture, "escape.patch", body)
                    self.assertEqual(self.stage(fixture, output, [entry]), 1)
                    self.assertFalse(output.exists())
                    self.assertFalse((fixture.root / "escape.txt").exists())

    def test_patches_apply_in_manifest_order(self):
        # Protect a dependent patch series from being reordered into a failure.
        first = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-alpha\n+second\n"
        second = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-second\n+third\n"
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            output = fixture.root / "stage"
            entries = [
                self.patch_entry(fixture, "1.patch", first),
                self.patch_entry(fixture, "2.patch", second),
            ]
            self.assertEqual(self.stage(fixture, output, entries), 0)
            self.assertEqual((output / "a.txt").read_text(encoding="utf-8"), "third\n")

            reversed_output = fixture.root / "stage-reversed"
            self.assertEqual(self.stage(fixture, reversed_output, list(reversed(entries))), 1)


if __name__ == "__main__":
    unittest.main()
