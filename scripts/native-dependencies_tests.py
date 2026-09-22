#!/usr/bin/env python3
"""Regression tests for the offline native-dependency verifier."""

from __future__ import annotations

import contextlib
import errno
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

_HERE = Path(__file__).resolve().parent
_REPO_ROOT = _HERE.parent
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

# Pin reviewed source trees independently so manifest-only digest changes cannot hide drift.
PINNED_VENDOR_TREES = {
    "freetype": "283cb02ba9baaa5e6b8a99e7bfa8673bd2d45dae9c9f4debdfd685c9abd97bb6",
    "harfbuzz": "5c177bc1d1ba83d5f06d9ea1fe6bcdd69bdb853bac2490af34ac148de582c787",
    "libpng": "75a542981ad0461cf449c448a20267256111f1fe61d2a4d537e4c865774303da",
    "zlib": "d021ec147dcd37fb5b33f8413d7409942d4a7607558e03b6bc0c191a6b5156f8",
    "winit": "e9e7ee5adefc2fbc8f81d7983658ca63ed6cf88a0a7c5257280c1739b9e4b456",
}


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
            "upstream_fixes": [],
            "tree_sha256": PINNED_TWO_FILE_TREE,
        }
        library.update(overrides)
        return {"schema_version": tool.SCHEMA_VERSION, "libraries": [library]}

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
        # Protect the manifest from shipping without the reviewed tree digest recorded.
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

    def test_single_library_selection_checks_only_that_library(self):
        # Protect `--library`, which stays part of the supported command surface.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            fixture.write_manifest(fixture.manifest())
            self.assertEqual(fixture.run("check", "--library", "demo"), 0)
            self.assertEqual(fixture.run("check", "--library", "absent"), 1)

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

    def test_unknown_schema_version_is_rejected(self):
        # Protect the tool from misreading a manifest shape it cannot claim to understand,
        # including the retired schema 1 whose `patches` entries named local files.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            for version in (1, tool.SCHEMA_VERSION + 1, "2", None):
                with self.subTest(schema_version=version):
                    document = fixture.manifest()
                    document["schema_version"] = version
                    fixture.write_manifest(document)
                    self.assertEqual(fixture.run("check"), 1)


class UpstreamFixMetadataTests(unittest.TestCase):
    """`upstream_fixes` records provenance only: a revision and where to read it."""

    def test_valid_records_are_accepted_without_any_local_file(self):
        # The central contract: recorded fixes need no file on disk to verify.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            fixture.write_manifest(fixture.manifest(upstream_fixes=[
                {
                    "revision": "8fa928f1617aba65f45b00f0dcb109f077b7741e",
                    "url": "https://example.invalid/commit/8fa928f",
                },
            ]))
            self.assertEqual(fixture.run("check"), 0)

    def test_incomplete_records_are_rejected(self):
        # A fix without both a revision and a URL is not provenance anyone can follow.
        broken = {
            "no revision": {"url": "https://example.invalid/c/1"},
            "no url": {"revision": "a" * 40},
            "empty revision": {"revision": "", "url": "https://example.invalid/c/1"},
            "empty url": {"revision": "a" * 40, "url": ""},
            "not an object": "https://example.invalid/c/1",
        }
        for label, entry in broken.items():
            with self.subTest(record=label):
                with tempfile.TemporaryDirectory() as directory:
                    fixture = Fixture(Path(directory))
                    fixture.populate_tree()
                    fixture.write_manifest(fixture.manifest(upstream_fixes=[entry]))
                    self.assertEqual(fixture.run("check"), 1)

    def test_local_patch_fields_are_rejected_inside_a_record(self):
        # Refuse the old shape outright: a path or file hash here would re-create the
        # duplication this manifest exists to avoid, and nothing would apply it.
        for field, value in (("path", "scripts/native-patches/x.patch"), ("sha256", "0" * 64)):
            with self.subTest(field=field):
                with tempfile.TemporaryDirectory() as directory:
                    fixture = Fixture(Path(directory))
                    fixture.populate_tree()
                    entry = {
                        "revision": "a" * 40,
                        "url": "https://example.invalid/c/1",
                        field: value,
                    }
                    fixture.write_manifest(fixture.manifest(upstream_fixes=[entry]))
                    self.assertEqual(fixture.run("check"), 1)

    def test_legacy_patches_key_is_rejected(self):
        # A library still carrying `patches` would be silently unverified, since nothing
        # reads that key any more; failing loudly is the only honest response.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            document = fixture.manifest()
            document["libraries"][0]["patches"] = [
                {"path": "scripts/native-patches/x.patch", "sha256": "0" * 64}
            ]
            fixture.write_manifest(document)
            self.assertEqual(fixture.run("check"), 1)

    def test_missing_upstream_fixes_key_is_rejected(self):
        # Absent is not the same as empty: an omitted list hides whether fixes were considered.
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.populate_tree()
            document = fixture.manifest()
            del document["libraries"][0]["upstream_fixes"]
            fixture.write_manifest(document)
            self.assertEqual(fixture.run("check"), 1)


class NoPatchFileDependencyTests(unittest.TestCase):
    """The tool must not depend on, advertise, or imply local patch reconstruction."""

    def test_patch_and_staging_entry_points_are_gone(self):
        # These are the exact names that made a patch file a build-adjacent input.
        for name in ("apply_patches", "verify_patches", "stage_library",
                     "extract_selected", "glob_to_regex", "file_sha256",
                     "_assert_patch_stays_inside", "_scan_archive"):
            with self.subTest(symbol=name):
                self.assertFalse(hasattr(tool, name))

    def test_archive_and_process_machinery_is_not_imported(self):
        # Without staging there is nothing to untar and nothing to execute; keeping the
        # imports would leave that capability one call away from returning.
        for name in ("tarfile", "subprocess", "shutil", "tempfile"):
            with self.subTest(module=name):
                self.assertFalse(hasattr(tool, name))

    def test_stage_is_neither_advertised_nor_accepted(self):
        # A removed command that still parses would promise a reconstruction it cannot do.
        help_text = io.StringIO()
        with contextlib.redirect_stdout(help_text):
            with self.assertRaises(SystemExit):
                tool._parse_args(["--help"])
        self.assertNotIn("stage", help_text.getvalue())
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                tool._parse_args(["stage", "freetype"])

    def test_patch_directory_is_absent_from_the_repository(self):
        # The duplication this change removes must not creep back in beside the manifest.
        self.assertFalse((_REPO_ROOT / "scripts" / "native-patches").exists())


class ShippedManifestTests(unittest.TestCase):
    """Guard the real manifest's reviewed digests and upstream provenance."""

    def setUp(self):
        self.manifest_path = _HERE / "native-dependencies.json"
        self.document = json.loads(self.manifest_path.read_text(encoding="utf-8"))
        self.libraries = tool.load_manifest(self.manifest_path)

    def test_vendored_trees_keep_their_reviewed_digests(self):
        # Each manifest pin must match the independently recorded reviewed source tree.
        recorded = {library["name"]: library["tree_sha256"] for library in self.libraries}
        self.assertEqual(recorded, PINNED_VENDOR_TREES)

    def test_winit_desktop_subset_keeps_required_inputs_without_upstream_extras(self):
        # The local desktop dependency must remain self-contained without restoring the full upstream package.
        library = tool.select_library(self.libraries, "winit")
        self.assertEqual(library["path"], "crates/sonicterm-winit")
        tree = _REPO_ROOT / library["path"]
        self.assertFalse((_REPO_ROOT / "third_party" / "winit").exists())
        required = {
            "Cargo.toml", "Cargo.lock", "build.rs", "LICENSE", "src/lib.rs",
            "src/platform/windows.rs", "src/platform/macos.rs",
            "src/platform/x11.rs", "src/platform/wayland.rs",
            "src/platform_impl/windows/keyboard_tests.rs",
            "src/platform_impl/linux/x11/tests/xsettings.dat",
            "tests/send_objects.rs", "tests/serde_objects.rs", "tests/sync_object.rs",
        }
        for relative in required:
            with self.subTest(required=relative):
                self.assertTrue((tree / relative).is_file(), relative)
        self.assertEqual(
            {path.name for path in tree.iterdir()},
            {"Cargo.toml", "Cargo.lock", "build.rs", "LICENSE", "src", "tests"},
        )
        self.assertEqual(
            {path.name for path in (tree / "src" / "platform").iterdir()},
            {"mod.rs", "windows.rs", "macos.rs", "x11.rs", "wayland.rs",
             "startup_notify.rs", "pump_events.rs", "run_on_demand.rs",
             "modifier_supplement.rs", "scancode.rs"},
        )
        self.assertEqual(
            {path.name for path in (tree / "src" / "platform_impl").iterdir()},
            {"mod.rs", "windows", "macos", "linux"},
        )
        self.assertFalse((tree / "src" / "changelog").exists())
        self.assertNotEqual(library["include"], ["**"])
        self.assertEqual(library["revision"], "e9809ef54b18499bb4f2cac945719ecc2a61061b")
        self.assertEqual(
            library["archive"]["sha256"],
            "a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d",
        )

    def test_winit_manifest_has_no_example_or_non_desktop_dependencies(self):
        # Removed targets must not leave Cargo declarations that require deleted inputs or unused packages.
        library = tool.select_library(self.libraries, "winit")
        manifest = (_REPO_ROOT / library["path"] / "Cargo.toml").read_text(encoding="utf-8")
        self.assertRegex(manifest, r'(?m)^name = "winit"$')
        self.assertRegex(manifest, r'(?m)^version = "0\.30\.13"$')
        for removed in ("[[example]]", "[dev-dependencies.", ".dev-dependencies.",
                        "android-activity", 'target_family = "wasm"',
                        'target_os = "ios"', 'target_os = "redox"', "ndk/rwh_"):
            with self.subTest(removed=removed):
                self.assertFalse(removed in manifest, f"unused Cargo declaration remains: {removed}")
        for name in ("send_objects", "serde_objects", "sync_object"):
            self.assertIn(f'path = "tests/{name}.rs"', manifest)

    def test_winit_module_selectors_match_the_retained_desktop_backends(self):
        # Deleting a backend must also remove its module declaration, including declarations enabled only by Rustdoc.
        tree = _REPO_ROOT / "crates" / "sonicterm-winit"
        for relative in ("src/platform/mod.rs", "src/platform_impl/mod.rs"):
            content = (tree / relative).read_text(encoding="utf-8")
            for backend in ("android", "ios", "web", "orbital"):
                with self.subTest(path=relative, backend=backend):
                    self.assertNotRegex(content, rf"\bmod\s+{backend}\s*;")
        build = (tree / "build.rs").read_text(encoding="utf-8")
        self.assertIn('free_unix: { target_os = "linux" }', build)
        root = (tree / "src" / "lib.rs").read_text(encoding="utf-8")
        self.assertNotRegex(root, r"\bmod\s+changelog\s*;")

    def test_winit_path_is_shared_by_build_gates_and_license_packaging(self):
        # A partial relocation must fail before packaging omits the dependency license or the gate skips its tests.
        current = "crates/sonicterm-winit"
        contracts = {
            "Cargo.toml": (f'exclude = ["{current}"]', f'winit = {{ path = "{current}" }}'),
            ".gitattributes": (f"{current}/**",),
            ".ignore": (f"{current}/",),
            "scripts/check-workspace-crates.sh": (
                f"{current}/Cargo.toml", f"{current}/src/platform_impl/windows/keyboard_tests.rs",
                "--features serde --lib --tests --no-fail-fast", "--features serde --no-deps --lib",
            ),
            "scripts/make-macos-dmg.sh": (f"$ROOT/{current}/LICENSE",),
            "scripts/make-linux-packages.sh": (f"$root/{current}/LICENSE",),
            "crates/sonicterm-windows/wix/main.wxs": (r"..\..\crates\sonicterm-winit\LICENSE",),
        }
        for relative, required in contracts.items():
            with self.subTest(path=relative):
                content = (_REPO_ROOT / relative).read_text(encoding="utf-8")
                for value in required:
                    self.assertTrue(value in content, f"{relative} is missing {value}")
                normalized = content.replace("\\", "/")
                self.assertFalse("third_party/winit" in normalized, relative)

    def test_every_library_records_followable_upstream_fixes(self):
        # Each retained fix must still name a full revision and a place to read it.
        for library in self.libraries:
            for fix in library["upstream_fixes"]:
                with self.subTest(library=library["name"], revision=fix["revision"]):
                    self.assertEqual(set(fix), {"revision", "url"})
                    self.assertRegex(fix["revision"], r"\A[0-9a-f]{40}\Z")
                    self.assertTrue(fix["url"].startswith("https://"))

    def test_base_release_provenance_is_retained(self):
        # Dropping the patch files must not drop the archive identity they were cut against.
        for library in self.libraries:
            with self.subTest(library=library["name"]):
                self.assertRegex(library["revision"], r"\A[0-9a-f]{40}\Z")
                self.assertRegex(library["archive"]["sha256"], r"\A[0-9a-f]{64}\Z")
                self.assertTrue(library["archive"]["url"].startswith("https://"))

    def test_no_local_patch_path_survives_in_the_manifest(self):
        # A stale path would advertise a file the repository no longer ships.
        self.assertNotIn("native-patches", self.manifest_path.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
