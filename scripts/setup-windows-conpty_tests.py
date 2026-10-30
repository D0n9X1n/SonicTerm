#!/usr/bin/env python3
"""Offline contracts for the pinned Windows ConPTY staging tool."""

from __future__ import annotations

import ast
import errno
import hashlib
import importlib.util
import io
import json
import os
import re
from pathlib import Path
import stat
import struct
import tempfile
import unittest
from unittest import mock
import urllib.request
import warnings
import zipfile

HERE = Path(__file__).resolve().parent
PACKAGE_URL = "https://www.nuget.org/api/v2/package/Microsoft.Windows.Console.ConPTY/1.25.260930003"
REVISION = "dc8ae365847d41f62d1ebca93815bb00959bc12f"
MEMBERS = {
    "conpty.dll": "runtimes/win-x64/native/conpty.dll",
    "OpenConsole.exe": "build/native/runtimes/x64/OpenConsole.exe",
}


def load_tool():
    specification = importlib.util.spec_from_file_location(
        "setup_windows_conpty", HERE / "setup-windows-conpty.py"
    )
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


def pin(data):
    return {"size": len(data), "sha256": hashlib.sha256(data).hexdigest()}


class Fixture:
    def __init__(self, root):
        self.destination = root / "destination"
        self.destination.mkdir()
        self.downloads = root / "downloads"
        self.downloads.mkdir()
        self.manifest = root / "windows-conpty.json"
        self.license = root / "LICENSE-conpty"
        self.license.write_bytes(b"reviewed license\r\n")
        self.payloads = {"conpty.dll": b"pinned dll", "OpenConsole.exe": b"pinned host"}
        self.archive = root / "package.nupkg"
        with zipfile.ZipFile(self.archive, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, data in self.payloads.items():
                archive.writestr(MEMBERS[name], data)
        self.document = {
            "schema_version": 1,
            "package_id": "Microsoft.Windows.Console.ConPTY",
            "version": "1.25.260930003",
            "architecture": "x64",
            "reviewed_upstream_revision": REVISION,
            "archive": {"url": PACKAGE_URL, **pin(self.archive.read_bytes())},
            "files": {
                name: {"member": MEMBERS[name], **pin(data)}
                for name, data in self.payloads.items()
            },
            "license": {
                "source": "LICENSE-conpty",
                "url": f"https://raw.githubusercontent.com/microsoft/terminal/{REVISION}/LICENSE",
                **pin(self.license.read_bytes()),
            },
        }
        self.save_manifest()

    def save_manifest(self):
        self.manifest.write_text(json.dumps(self.document), encoding="utf-8")

    def stage(self, tool, **options):
        archive = options.pop("archive", self.archive)
        return tool.stage(
            self.destination, archive=archive, manifest_path=self.manifest,
            temp_root=self.downloads, **options,
        )

    def repin_archive(self):
        self.document["archive"].update(pin(self.archive.read_bytes()))
        self.save_manifest()

    def replace_zip(self, entries):
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            with zipfile.ZipFile(self.archive, "w") as archive:
                for member, data in entries:
                    archive.writestr(member, data)
        self.repin_archive()


def create_symlink(target, link, *, directory=False):
    try:
        link.symlink_to(target, target_is_directory=directory)
    except OSError as error:
        if error.errno in (errno.EPERM, errno.EACCES, errno.ENOSYS, errno.ENOTSUP) or getattr(error, "winerror", None) == 1314:
            raise unittest.SkipTest("host does not permit symlink creation") from error
        raise


class Response(io.BytesIO):
    def __init__(self, data, *, content_length=None, url=PACKAGE_URL, read_limit=None, fail_read=False):
        super().__init__(data)
        self.headers = {} if content_length is None else {"Content-Length": content_length}
        self.url = url
        self.read_limit = read_limit
        self.fail_read = fail_read
        self.requested = []
        self.consumed = 0

    def geturl(self):
        return self.url

    def read1(self, size):
        self.requested.append(size)
        if self.fail_read:
            raise TimeoutError("socket timed out")
        data = super().read(size if self.read_limit is None else min(size, self.read_limit))
        self.consumed += len(data)
        return data


class StagingTests(unittest.TestCase):
    def test_offline_stage_publishes_only_pinned_files_and_license(self):
        # Archive paths never become output paths, and an offline install cannot open a URL.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())

            def reject_network(*arguments, **keywords):
                self.fail("offline staging attempted network access")

            target = fixture.stage(tool, opener=reject_network)
            self.assertEqual(target, fixture.destination / "conpty")
            expected = {**fixture.payloads, "LICENSE": fixture.license.read_bytes()}
            self.assertEqual({entry.name: entry.read_bytes() for entry in target.iterdir()}, expected)
            self.assertEqual(list(fixture.destination.iterdir()), [target])
            self.assertEqual(list(fixture.downloads.iterdir()), [])

    def test_archive_size_and_hash_are_checked_before_zip_parsing(self):
        # A matching manifest structure cannot authorize corrupt outer archive bytes.
        tool = load_tool()
        for change in ("size", "sha256"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                fixture.document["archive"][change] = 1 if change == "size" else "0" * 64
                fixture.save_manifest()
                with self.assertRaisesRegex(tool.StageError, change):
                    fixture.stage(tool)
                self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_verified_archive_still_requires_exact_inner_member_size_and_hash(self):
        # Re-pinning the ZIP lets malformed member contracts reach the inner verifier.
        tool = load_tool()
        for change in ("size", "sha256"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                fixture.document["files"]["conpty.dll"][change] = 1 if change == "size" else "0" * 64
                fixture.save_manifest()
                with self.assertRaisesRegex(tool.StageError, change):
                    fixture.stage(tool)
                self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_expected_member_must_have_one_exact_case_sensitive_name(self):
        # ZIP duplicates and lookalike paths must never substitute for the pinned member.
        tool = load_tool()
        for change in ("duplicate", "case", "traversal", "missing"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                entries = [(MEMBERS[name], data) for name, data in fixture.payloads.items()]
                if change == "duplicate":
                    entries.append(entries[0])
                elif change == "case":
                    entries[0] = (entries[0][0].upper(), entries[0][1])
                elif change == "traversal":
                    entries[0] = ("../" + entries[0][0], entries[0][1])
                else:
                    entries.pop(0)
                fixture.replace_zip(entries)
                with self.assertRaisesRegex(tool.StageError, "exactly once"):
                    fixture.stage(tool)
                self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_unselected_traversal_members_are_not_extracted(self):
        # The archive is not tree-extracted, even when unused entries contain traversal syntax.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            with zipfile.ZipFile(fixture.archive, "a") as archive:
                archive.writestr("../escaped.txt", b"unselected")
            fixture.repin_archive()
            target = fixture.stage(tool)
            self.assertEqual({entry.name for entry in target.iterdir()}, {"conpty.dll", "OpenConsole.exe", "LICENSE"})
            self.assertFalse((Path(directory).resolve() / "escaped.txt").exists())

    def test_member_metadata_rejects_links_directories_and_unsupported_compression(self):
        # Explicit special-file metadata is refused before any member is opened.
        tool = load_tool()
        for change in ("symlink", "directory", "fifo", "compression"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                member = zipfile.ZipInfo(MEMBERS["conpty.dll"])
                member.create_system = 3
                member.external_attr = {
                    "symlink": (stat.S_IFLNK | 0o777) << 16,
                    "directory": (stat.S_IFDIR | 0o755) << 16 | 0x10,
                    "fifo": (stat.S_IFIFO | 0o600) << 16,
                    "compression": (stat.S_IFREG | 0o600) << 16,
                }[change]
                member.compress_type = zipfile.ZIP_BZIP2 if change == "compression" else zipfile.ZIP_STORED
                fixture.replace_zip([(member, fixture.payloads["conpty.dll"]),
                                     (MEMBERS["OpenConsole.exe"], fixture.payloads["OpenConsole.exe"])])
                with self.assertRaisesRegex(tool.StageError, "member"):
                    fixture.stage(tool)
                self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_encryption_and_declared_size_corruption_are_rejected(self):
        # Patching both headers preserves outer ZIP validity while changing untrusted metadata.
        tool = load_tool()
        for change in ("encrypted", "size"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                data = bytearray(fixture.archive.read_bytes())
                central = data.index(b"PK\x01\x02")
                if change == "encrypted":
                    struct.pack_into("<H", data, 6, 1)
                    struct.pack_into("<H", data, central + 8, 1)
                else:
                    struct.pack_into("<I", data, 22, 1)
                    struct.pack_into("<I", data, central + 24, 1)
                fixture.archive.write_bytes(data)
                fixture.repin_archive()
                with self.assertRaisesRegex(tool.StageError, "member"):
                    fixture.stage(tool)
                self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_nul_truncated_member_name_cannot_match_the_pinned_name(self):
        # ZipInfo normalizes a NUL suffix away, but that raw member name is not the reviewed name.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            member_name = MEMBERS["conpty.dll"] + "Xsuffix"
            fixture.replace_zip([(member_name, fixture.payloads["conpty.dll"]),
                                 (MEMBERS["OpenConsole.exe"], fixture.payloads["OpenConsole.exe"])])
            data = fixture.archive.read_bytes().replace(member_name.encode(), member_name.replace("Xsuffix", "\0suffix").encode())
            fixture.archive.write_bytes(data)
            fixture.repin_archive()
            with self.assertRaisesRegex(tool.StageError, "member"):
                fixture.stage(tool)
            self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_official_zero_external_attributes_are_regular_member_payloads(self):
        # The official NuGet package has no Unix file-type metadata for its selected files.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            data = bytearray(fixture.archive.read_bytes())
            position = 0
            while True:
                position = data.find(b"PK\x01\x02", position)
                if position < 0:
                    break
                struct.pack_into("<I", data, position + 38, 0)
                position += 46
            fixture.archive.write_bytes(data)
            fixture.repin_archive()
            self.assertTrue(fixture.stage(tool).is_dir())

    def test_invalid_zip_is_reported_and_cleans_owned_staging(self):
        # Correctly hashed non-ZIP data must not leave a partially installed runtime.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            fixture.archive.write_bytes(b"not a zip")
            fixture.repin_archive()
            with self.assertRaises(tool.StageError):
                fixture.stage(tool)
            self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_destination_requires_existing_real_directory(self):
        # Caller mistakes never create parent directories or replace ordinary files.
        tool = load_tool()
        for change in ("missing", "file"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                fixture.destination.rmdir()
                if change == "file":
                    fixture.destination.write_bytes(b"keep")
                with self.assertRaisesRegex(tool.StageError, "directory"):
                    fixture.stage(tool)
                if change == "missing":
                    self.assertFalse(fixture.destination.exists())
                else:
                    self.assertEqual(fixture.destination.read_bytes(), b"keep")

    def test_existing_target_must_match_exact_regular_file_set(self):
        # Existing partial or modified installations are evidence, not disposable staging.
        tool = load_tool()
        for change in ("empty", "missing", "extra", "mismatch", "directory", "file"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                if change == "file":
                    target = fixture.destination / "conpty"
                    target.write_bytes(b"keep")
                else:
                    target = fixture.stage(tool)
                    if change in ("empty", "missing"):
                        (target / "conpty.dll").unlink()
                        if change == "empty":
                            (target / "OpenConsole.exe").unlink()
                            (target / "LICENSE").unlink()
                    elif change == "extra":
                        (target / "unknown").write_bytes(b"keep")
                    elif change == "mismatch":
                        (target / "conpty.dll").write_bytes(b"edited dll")
                    else:
                        (target / "conpty.dll").unlink()
                        (target / "conpty.dll").mkdir()
                with self.assertRaises(tool.StageError):
                    fixture.stage(tool)
                self.assertTrue(target.exists())
                self.assertEqual(list(fixture.destination.iterdir()), [target])
                if change == "mismatch":
                    self.assertEqual((target / "conpty.dll").read_bytes(), b"edited dll")

    def test_matching_target_is_noop_but_license_source_is_still_verified(self):
        # Idempotence preserves files and performs no download; repository license drift still fails.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            target = fixture.stage(tool)
            before = {entry.name: entry.stat().st_mtime_ns for entry in target.iterdir()}

            def reject_network(*arguments, **keywords):
                self.fail("idempotent staging attempted network access")

            self.assertEqual(fixture.stage(tool, archive=None, opener=reject_network), target)
            self.assertEqual({entry.name: entry.stat().st_mtime_ns for entry in target.iterdir()}, before)
            fixture.license.write_bytes(b"changed license")
            with self.assertRaisesRegex(tool.StageError, "LICENSE-conpty"):
                fixture.stage(tool, archive=None, opener=reject_network)

    def test_links_at_destination_target_or_output_are_rejected(self):
        # Neither root selection nor idempotence can follow redirected filesystem entries.
        tool = load_tool()
        for change in ("destination", "target", "output", "ancestor", "dangling"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                root = Path(directory).resolve()
                fixture = Fixture(root)
                other = root / "other"
                other.mkdir()
                if change == "destination":
                    fixture.destination.rmdir()
                    create_symlink(other, fixture.destination, directory=True)
                elif change in ("target", "dangling"):
                    create_symlink(other if change == "target" else root / "absent", fixture.destination / "conpty", directory=True)
                elif change == "ancestor":
                    linked = root / "linked"
                    create_symlink(root, linked, directory=True)
                    fixture.destination = linked / "destination"
                else:
                    target = fixture.stage(tool)
                    (target / "conpty.dll").unlink()
                    replacement = other / "dll"
                    replacement.write_bytes(fixture.payloads["conpty.dll"])
                    create_symlink(replacement, target / "conpty.dll")
                with self.assertRaisesRegex(tool.StageError, "link|reparse"):
                    fixture.stage(tool)
                self.assertEqual({entry.name for entry in other.iterdir()}, {"dll"} if change == "output" else set())

    def test_reparse_directory_metadata_is_rejected(self):
        # Windows junctions and non-symlink reparse points receive the same refusal as symlinks.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            original = Path.lstat

            def reparse(path, *arguments, **keywords):
                result = original(path, *arguments, **keywords)
                if path == fixture.destination:
                    return type("ReparseStat", (), {"st_mode": result.st_mode, "st_file_attributes": 0x400})()
                return result

            with mock.patch.object(Path, "lstat", reparse), self.assertRaisesRegex(tool.StageError, "reparse"):
                fixture.stage(tool)

    def test_failure_never_removes_older_staging_leftovers(self):
        # Cleanup owns only the unique directory created by the current invocation.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            leftover = fixture.destination / ".conpty-staging-older"
            leftover.mkdir()
            (leftover / "evidence").write_bytes(b"keep")
            fixture.document["files"]["OpenConsole.exe"]["sha256"] = "0" * 64
            fixture.save_manifest()
            with self.assertRaises(tool.StageError):
                fixture.stage(tool)
            self.assertEqual(list(fixture.destination.iterdir()), [leftover])
            self.assertEqual((leftover / "evidence").read_bytes(), b"keep")


class ManifestTests(unittest.TestCase):
    def test_fixed_schema_rejects_output_and_source_path_injection(self):
        # Output names, package identity, and member paths are a fixed reviewed schema.
        tool = load_tool()
        mutations = (
            lambda document: document.update(schema_version=True),
            lambda document: document.update(architecture="arm64"),
            lambda document: document.update(package_id="other"),
            lambda document: document.update(repository_commit=REVISION),
            lambda document: document.update(version="latest"),
            lambda document: document["files"].update({"../escape": document["files"].pop("conpty.dll")}),
            lambda document: document["files"]["conpty.dll"].update(member="../conpty.dll"),
            lambda document: document["files"]["conpty.dll"].update(output="../escape"),
            lambda document: document["license"].update(source="../LICENSE-conpty"),
            lambda document: document["archive"].update(size=True),
            lambda document: document["archive"].update(size=-1),
            lambda document: document["archive"].update(sha256="invalid"),
            lambda document: document["archive"].update(url="http://www.nuget.org/package"),
        )
        for index, mutate in enumerate(mutations):
            with self.subTest(mutation=index), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                mutate(fixture.document)
                fixture.save_manifest()
                with self.assertRaises(tool.StageError):
                    fixture.stage(tool)
                self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_malformed_json_and_nonobject_manifest_are_reported(self):
        # Malformed trusted inputs fail explicitly instead of selecting default pins.
        tool = load_tool()
        for text in ("{", "[]", '{"schema_version":1,"schema_version":1}'):
            with self.subTest(text=text), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                fixture.manifest.write_text(text, encoding="utf-8")
                with self.assertRaises(tool.StageError):
                    fixture.stage(tool)

    def test_shipped_manifest_and_license_retain_reviewed_pins(self):
        # These expectations are independent of the manifest and preserve exact license bytes.
        document = json.loads((HERE / "windows-conpty.json").read_text(encoding="utf-8"))
        self.assertEqual(document["archive"], {"url": PACKAGE_URL, "size": 1763391,
                         "sha256": "02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee"})
        self.assertEqual(document["files"]["conpty.dll"], {"member": MEMBERS["conpty.dll"], "size": 119608,
                         "sha256": "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a"})
        self.assertEqual(document["files"]["OpenConsole.exe"], {"member": MEMBERS["OpenConsole.exe"], "size": 1089376,
                         "sha256": "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586"})
        self.assertEqual(document["reviewed_upstream_revision"], REVISION)
        self.assertNotIn("repository_commit", document)
        self.assertEqual(pin((HERE / "LICENSE-conpty").read_bytes()), {
            "size": 1116, "sha256": "5d177f23ecfeb0ea8e050b6a5a16355e1ae9a0b286436ca8f83ed08b3795be6b"})

    def test_supply_chain_gate_runs_staging_and_verification_with_matching_runtime_pins(self):
        # CI must run offline package and wiring contracts, with every admission point using the staged pins.
        source = (HERE / "check-workflow-supply-chain.sh").read_text(encoding="utf-8")
        for stem in ("setup-windows-conpty", "verify-windows-conpty"):
            self.assertIn(f'"$ROOT/scripts/{stem}.py"', source)
            self.assertIn(f'"$ROOT/scripts/{stem}_tests.py"', source)
            self.assertIn(f'"$PY" -I {stem}_tests.py', source)
        self.assertIn('"$ROOT/scripts/windows_conpty_contract_tests.py"', source)
        self.assertIn('"$PY" -I windows_conpty_contract_tests.py', source)
        document = json.loads((HERE / "windows-conpty.json").read_text(encoding="utf-8"))
        verifier = ast.parse((HERE / "verify-windows-conpty.py").read_text(encoding="utf-8"))
        constants = {
            statement.targets[0].id: ast.literal_eval(statement.value)
            for statement in verifier.body
            if isinstance(statement, ast.Assign) and isinstance(statement.targets[0], ast.Name)
            and statement.targets[0].id in ("PINS", "VERSION")
        }
        expected = {name: (record["size"], record["sha256"]) for name, record in document["files"].items()}
        self.assertEqual(constants["VERSION"], document["version"])
        self.assertEqual(constants["PINS"], expected)
        runtime = (HERE.parent / "crates/sonicterm-io/src/conpty_runtime.rs").read_text(encoding="utf-8")
        self.assertRegex(runtime, rf'const VERSION: &str = "{re.escape(document["version"])}";')
        for name, prefix in (("conpty.dll", "DLL"), ("OpenConsole.exe", "HOST")):
            size_match = re.search(rf"const {prefix}_SIZE: u64 = ([0-9_]+);", runtime)
            self.assertIsNotNone(size_match, f"missing {prefix}_SIZE")
            self.assertEqual(int(size_match.group(1).replace("_", "")), expected[name][0])
            self.assertRegex(runtime, rf'const {prefix}_SHA256: &str = "{expected[name][1]}";')


class DownloadTests(unittest.TestCase):
    def test_download_is_bounded_verified_and_removed_after_publication(self):
        # A streamed HTTPS response follows the same archive verifier as offline input.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            response = Response(fixture.archive.read_bytes(), content_length=str(fixture.archive.stat().st_size), read_limit=7)
            calls = []

            def opener(request, *, timeout):
                calls.append((request.full_url, timeout))
                self.assertEqual(len(list(fixture.downloads.iterdir())), 1)
                return response

            fixture.stage(tool, archive=None, opener=opener)
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0][0], PACKAGE_URL)
            self.assertGreater(calls[0][1], 0)
            self.assertLessEqual(calls[0][1], tool.SOCKET_TIMEOUT_S)
            self.assertTrue(response.closed)
            self.assertEqual(list(fixture.downloads.iterdir()), [])

    def test_content_length_mismatch_is_refused_before_reading(self):
        # Header mismatches cannot consume disk or silently authorize a truncated download.
        tool = load_tool()
        for length in ("0", "junk", "-1", "999999999999"):
            with self.subTest(length=length), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                response = Response(fixture.archive.read_bytes(), content_length=length)
                with self.assertRaisesRegex(tool.StageError, "Content-Length"):
                    fixture.stage(tool, archive=None, opener=lambda request, timeout: response)
                self.assertEqual(response.consumed, 0)
                self.assertTrue(response.closed)
                self.assertEqual(list(fixture.downloads.iterdir()), [])

    def test_missing_length_enforces_stream_cap_and_rejects_short_or_corrupt_bytes(self):
        # Reads stop at pinned size plus one byte even when the server advertises no length.
        tool = load_tool()
        for change in ("long", "short", "hash"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                data = fixture.archive.read_bytes()
                payload = data + b"extra" * 100 if change == "long" else data[:-1] if change == "short" else b"!" + data[1:]
                response = Response(payload)
                with self.assertRaisesRegex(tool.StageError, "size|sha256"):
                    fixture.stage(tool, archive=None, opener=lambda request, timeout: response)
                self.assertLessEqual(response.consumed, len(data) + 1)
                self.assertEqual(list(fixture.downloads.iterdir()), [])
                self.assertEqual(list(fixture.destination.iterdir()), [])

    def test_non_https_final_response_and_redirects_are_refused(self):
        # Every hop must stay HTTPS; a final URL check also guards injected opener behavior.
        tool = load_tool()
        for change in ("final", "redirect"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                response = Response(b"", url="http://example.invalid/package" if change == "final" else PACKAGE_URL)
                if change == "redirect":
                    response.status = 302
                    response.headers["Location"] = "http://example.invalid/package"
                calls = []

                def opener(request, *, timeout):
                    calls.append(request.full_url)
                    return response

                with self.assertRaisesRegex(tool.StageError, "HTTPS"):
                    fixture.stage(tool, archive=None, opener=opener)
                self.assertEqual(len(calls), 1)
                self.assertEqual(response.consumed, 0)
                self.assertEqual(list(fixture.downloads.iterdir()), [])

    def test_https_redirect_is_followed_without_reading_its_body(self):
        # Unbounded redirect bodies are irrelevant and never drained by the downloader.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            redirect = Response(b"ignored redirect body")
            redirect.status = 302
            redirect.headers["Location"] = "/verified/package.nupkg"
            response = Response(fixture.archive.read_bytes())
            replies = iter((redirect, response))
            calls = []

            def opener(request, *, timeout):
                calls.append(request.full_url)
                return next(replies)

            fixture.stage(tool, archive=None, opener=opener)
            self.assertEqual(calls, [PACKAGE_URL, "https://www.nuget.org/verified/package.nupkg"])
            self.assertEqual(redirect.consumed, 0)
            self.assertTrue(redirect.closed)

    def test_http_error_closes_its_response_without_reading_the_body(self):
        # urllib exposes non-success responses through HTTPError, which still owns a socket.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            response = Response(b"unbounded error body")
            failure = urllib.error.HTTPError(PACKAGE_URL, 503, "unavailable", {}, response)
            self.addCleanup(failure.close)

            def opener(request, *, timeout):
                raise failure

            with self.assertRaisesRegex(tool.StageError, "503"):
                fixture.stage(tool, archive=None, opener=opener)
            self.assertTrue(response.closed)
            self.assertEqual(response.consumed, 0)
            self.assertEqual(list(fixture.downloads.iterdir()), [])

    def test_each_socket_read_uses_the_remaining_overall_budget(self):
        # Late reads cannot restart a full socket timeout after most of the total budget elapsed.
        tool = load_tool()
        with tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
            fixture = Fixture(Path(directory).resolve())
            response = Response(fixture.archive.read_bytes())
            timeouts = []

            class Socket:
                def settimeout(self, timeout):
                    timeouts.append(timeout)

            response.fp = type("Stream", (), {"raw": type("Raw", (), {"_sock": Socket()})()})()
            ticks = [0.0]

            def clock():
                return ticks[0]

            def opener(request, *, timeout):
                ticks[0] = tool.DOWNLOAD_TIMEOUT_S - 0.25
                return response

            with mock.patch.object(tool.time, "monotonic", clock):
                fixture.stage(tool, archive=None, opener=opener)
            self.assertTrue(timeouts)
            self.assertTrue(all(timeout <= 0.25 for timeout in timeouts))

    def test_socket_timeout_and_overall_deadline_both_cleanup(self):
        # Timeout failures leave no final target and no download/staging scratch directory.
        tool = load_tool()
        for change in ("socket", "deadline"):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix="conpty-tests-") as directory:
                fixture = Fixture(Path(directory).resolve())
                response = Response(fixture.archive.read_bytes(), fail_read=change == "socket")
                ticks = [0.0]

                def clock():
                    return ticks[0]

                def opener(request, *, timeout):
                    if change == "deadline":
                        ticks[0] = tool.DOWNLOAD_TIMEOUT_S + 1
                    return response

                with mock.patch.object(tool.time, "monotonic", clock), self.assertRaisesRegex(tool.StageError, "timed out|deadline"):
                    fixture.stage(tool, archive=None, opener=opener)
                self.assertTrue(response.closed)
                self.assertEqual(list(fixture.downloads.iterdir()), [])
                self.assertEqual(list(fixture.destination.iterdir()), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
