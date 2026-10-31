#!/usr/bin/env python3
"""Offline contracts for the pinned macOS 14 Homebrew bottle setup."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import time
import unittest
from unittest import mock
import urllib.error

HERE = Path(__file__).resolve().parent


def load_setup():
    """Import `setup-macos-native.py`, whose hyphenated name is not an importable module name."""
    specification = importlib.util.spec_from_file_location("setup_macos_native", HERE / "setup-macos-native.py")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


SETUP = load_setup()


class FakeResponse(io.BytesIO):
    """A response body the download reads in chunks."""


def record_for(payload, formula="lzo", version="2.10", tag="2.10"):
    """A pinned record whose size and SHA-256 match `payload`."""
    return {"formula": formula, "version": version, "tag": tag, "size": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(), "depends_on": []}


def opener_serving(payload, redirect_to=None, calls=None):
    """An opener that hands out a token, optionally redirects the blob once, then serves `payload`."""
    def opener(request, timeout):
        calls.append((request.full_url, dict(request.header_items()))) if calls is not None else None
        if "/token?" in request.full_url:
            return FakeResponse(json.dumps({"token": "secret-token"}).encode())
        if redirect_to and "ghcr.io/v2/" in request.full_url:
            raise urllib.error.HTTPError(request.full_url, 307, "redirect", {"Location": redirect_to}, None)
        return FakeResponse(payload)
    return opener


class ManifestTests(unittest.TestCase):
    """The checked-in manifest pins the Cairo closure, and malformed manifests are refused."""

    def write(self, document):
        directory = tempfile.mkdtemp()
        path = Path(directory) / "manifest.json"
        path.write_text(json.dumps(document), encoding="utf-8")
        return path

    def test_the_checked_in_manifest_pins_cairo_after_its_dependencies(self):
        # Every runtime library of Cairo 1.18.4 is pinned to an arm64_sonoma bottle, dependencies first.
        manifest = SETUP.load_manifest()
        names = [record["formula"] for record in manifest["formulae"]]
        self.assertEqual(names[-1], "cairo")
        self.assertEqual(manifest["max_minimum_macos"], "14.0")
        self.assertEqual(len(names), len(set(names)))
        self.assertTrue({"libpng", "freetype", "fontconfig", "glib", "pixman"} <= set(names))

    def test_malformed_manifests_are_refused(self):
        # Each change below breaks one rule the setup relies on; none may load.
        good = json.loads((HERE / "macos-native-bottles.json").read_text(encoding="utf-8"))
        cases = {
            "extra key": {**good, "extra": 1},
            "schema": {**good, "schema_version": 2},
            "bottle tag": {**good, "bottle_tag": "arm64_sequoia"},
            "registry": {**good, "registry": "https://example.com/v2"},
            "dependency after dependent": {**good, "formulae": list(reversed(good["formulae"]))},
            "bad sha": {**good, "formulae": [*good["formulae"][:-1], {**good["formulae"][-1], "sha256": "ABC"}]},
            "bad tag": {**good, "formulae": [*good["formulae"][:-1], {**good["formulae"][-1], "tag": "9.9"}]},
            "zero size": {**good, "formulae": [*good["formulae"][:-1], {**good["formulae"][-1], "size": 0}]},
            "not cairo last": {**good, "formulae": good["formulae"][:-1]},
        }
        for name, document in cases.items():
            with self.subTest(name), self.assertRaises(SETUP.SetupError):
                SETUP.load_manifest(self.write(document))


class NamingTests(unittest.TestCase):
    """Bottle file names and registry repositories follow Homebrew's spelling."""

    def test_bottle_filename_carries_the_rebuild(self):
        # A rebuild tag `1.2-3` names `.bottle.3.tar.gz`; a plain tag has no rebuild number.
        plain = {"formula": "cairo", "version": "1.18.4", "tag": "1.18.4"}
        rebuilt = {"formula": "cairo", "version": "1.18.4", "tag": "1.18.4-2"}
        self.assertEqual(SETUP.bottle_filename(plain, "arm64_sonoma"), "cairo--1.18.4.arm64_sonoma.bottle.tar.gz")
        self.assertEqual(SETUP.bottle_filename(rebuilt, "arm64_sonoma"), "cairo--1.18.4.arm64_sonoma.bottle.2.tar.gz")

    def test_repository_names_follow_ghcr_spelling(self):
        # `@` becomes a path separator and `+` becomes `x`, as Homebrew publishes them.
        self.assertEqual(SETUP.repository("openssl@3"), "openssl/3")
        self.assertEqual(SETUP.repository("libstdc++"), "libstdcxx")


class DownloadTests(unittest.TestCase):
    """A download is kept only when its size and SHA-256 match, and the token stays on ghcr.io."""

    def test_a_matching_bottle_is_kept(self):
        payload = b"bottle bytes" * 100
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "lzo.tar.gz"
            SETUP.download(record_for(payload), target, opener_serving(payload))
            self.assertEqual(target.read_bytes(), payload)

    def test_a_mismatched_bottle_is_refused_and_removed(self):
        # Wrong bytes of the right size, and a short body, both fail and leave no file behind.
        payload = b"bottle bytes" * 100
        for served in (b"x" * len(payload), payload[:-1]):
            with self.subTest(len(served)), tempfile.TemporaryDirectory() as directory:
                target = Path(directory) / "lzo.tar.gz"
                with self.assertRaises(SETUP.SetupError):
                    SETUP.download(record_for(payload), target, opener_serving(served))
                self.assertFalse(target.exists())

    def test_the_token_is_not_sent_to_the_storage_host(self):
        # The blob request to ghcr.io carries the token; the redirected storage request does not.
        payload = b"bottle"
        calls = []
        with tempfile.TemporaryDirectory() as directory:
            SETUP.download(record_for(payload), Path(directory) / "lzo.tar.gz",
                           opener_serving(payload, "https://pkg-containers.githubusercontent.com/blob", calls))
        blob_calls = [headers for url, headers in calls if "/token?" not in url]
        self.assertIn("Authorization", blob_calls[0])
        self.assertNotIn("Authorization", blob_calls[1])

    def test_a_redirect_off_https_is_refused(self):
        with tempfile.TemporaryDirectory() as directory, self.assertRaises(SETUP.SetupError):
            SETUP.download(record_for(b"bottle"), Path(directory) / "lzo.tar.gz",
                           opener_serving(b"bottle", "http://example.com/blob"))


def bottle_bytes(members):
    """A gzipped bottle holding `members`: `(name, bytes)` files, `(name, None)` directories, and
    `(name, "->target")` symlinks."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        for name, content in members:
            info = tarfile.TarInfo(name)
            if content is None:
                info.type, info.mode = tarfile.DIRTYPE, 0o755
                archive.addfile(info)
            elif isinstance(content, str):
                info.type, info.linkname = tarfile.SYMTYPE, content[2:]
                archive.addfile(info)
            else:
                info.size, info.mode = len(content), 0o644
                archive.addfile(info, io.BytesIO(content))
    return buffer.getvalue()


CAIRO = {"formula": "cairo", "version": "1.18.4"}
MACHO = b"\xcf\xfa\xed\xfe" + b"\0" * 60
BUILD_VERSION = "Load command 9\n      cmd LC_BUILD_VERSION\n  cmdsize 32\n platform 1\n    minos {}\n      sdk 15.0\n"
VERSION_MIN = "Load command 8\n      cmd LC_VERSION_MIN_MACOSX\n  cmdsize 16\n  version {}\n      sdk 14.0\n"


def otool_loads(path, loads):
    """An `otool -L` listing of `path` naming `loads`."""
    return f"{path}:\n" + "".join(f"\t{load} (compatibility version 1.0.0, current version 1.0.0)\n" for load in loads)


class ExtractTests(unittest.TestCase):
    """A bottle unpacks only inside its own keg."""

    def test_a_bottle_unpacks_into_its_keg(self):
        with tempfile.TemporaryDirectory() as directory:
            bottle = Path(directory) / "cairo.tar.gz"
            bottle.write_bytes(bottle_bytes([("cairo/1.18.4/lib/libcairo.2.dylib", MACHO),
                                             ("cairo/1.18.4/COPYING", b"license")]))
            keg = SETUP.extract_bottle(bottle, Path(directory) / "Cellar", CAIRO)
            self.assertEqual(keg, Path(directory) / "Cellar" / "cairo" / "1.18.4")
            self.assertEqual((keg / "lib" / "libcairo.2.dylib").read_bytes(), MACHO)

    def test_a_bottle_listing_its_own_keg_directories_unpacks(self):
        # Real bottles list `<formula>/<version>` and its subdirectories as members before their files.
        with tempfile.TemporaryDirectory() as directory:
            bottle = Path(directory) / "libpng.tar.gz"
            bottle.write_bytes(bottle_bytes([("libpng/1.6.47", None), ("libpng/1.6.47/lib", None),
                                             ("libpng/1.6.47/lib/libpng16.16.dylib", MACHO),
                                             ("libpng/1.6.47/LICENSE", b"license")]))
            record = {"formula": "libpng", "version": "1.6.47"}
            keg = SETUP.extract_bottle(bottle, Path(directory) / "Cellar", record)
            self.assertEqual((keg / "lib" / "libpng16.16.dylib").read_bytes(), MACHO)

    def test_members_outside_the_keg_are_refused_before_anything_is_written(self):
        # A bottle is untrusted input: no member may land, or link, outside its formula/version keg.
        cases = {
            "parent traversal": [("cairo/1.18.4/../../escape", b"x")],
            "another formula": [("pixman/0.44.2/lib/libpixman-1.0.dylib", b"x")],
            "another version": [("cairo/1.18.6/lib/libcairo.2.dylib", b"x")],
            "absolute symlink": [("cairo/1.18.4/lib/libcairo.dylib", "->/etc/passwd")],
            "symlink out of the keg": [("cairo/1.18.4/lib/libcairo.dylib", "->../../../pixman")],
        }
        for name, members in cases.items():
            with self.subTest(name), tempfile.TemporaryDirectory() as directory:
                bottle = Path(directory) / "cairo.tar.gz"
                bottle.write_bytes(bottle_bytes(members))
                with self.assertRaises(SETUP.SetupError):
                    SETUP.extract_bottle(bottle, Path(directory) / "Cellar", CAIRO)
                self.assertFalse((Path(directory) / "Cellar").exists())


    @unittest.skipUnless(os.name == "posix", "the bottle's symlinks need a filesystem that makes them")
    def test_a_symlink_chain_cannot_write_into_another_keg(self):
        # Each link and member passes a string check, but through `alias -> .` the last link
        # resolves into the pixman keg; the file written through it must never land there.
        with tempfile.TemporaryDirectory() as directory:
            cellar = Path(directory) / "Cellar"
            pixman = cellar / "pixman" / "0.44.2" / "lib" / "libpixman-1.0.dylib"
            pixman.parent.mkdir(parents=True)
            pixman.write_bytes(b"pinned pixman")
            bottle = Path(directory) / "cairo.tar.gz"
            bottle.write_bytes(bottle_bytes([
                ("cairo/1.18.4/lib", None),
                ("cairo/1.18.4/lib/alias", "->."),
                ("cairo/1.18.4/lib/alias/alias/escape", "->../../../pixman/0.44.2"),
                ("cairo/1.18.4/lib/escape/lib/libpixman-1.0.dylib", b"replaced"),
            ]))
            with self.assertRaises(SETUP.SetupError):
                SETUP.extract_bottle(bottle, cellar, CAIRO)
            self.assertEqual(pixman.read_bytes(), b"pinned pixman")
            self.assertFalse((cellar / "cairo").exists())
            self.assertEqual(sorted(path.name for path in Path(directory).iterdir()), ["Cellar", "cairo.tar.gz"])


class RelocationTests(unittest.TestCase):
    """A bottle's Homebrew paths move under the private prefix; system and relative paths stay."""

    def test_paths_move_under_the_prefix(self):
        prefix = "/tmp/sonicterm-native"
        cases = {
            "@@HOMEBREW_PREFIX@@/opt/pixman/lib/libpixman-1.0.dylib": f"{prefix}/opt/pixman/lib/libpixman-1.0.dylib",
            "@@HOMEBREW_CELLAR@@/cairo/1.18.4/lib/libcairo.2.dylib": f"{prefix}/Cellar/cairo/1.18.4/lib/libcairo.2.dylib",
            "/opt/homebrew/opt/libpng/lib/libpng16.16.dylib": f"{prefix}/opt/libpng/lib/libpng16.16.dylib",
            "/opt/homebrew/Cellar/glib/2.82.5/lib": f"{prefix}/Cellar/glib/2.82.5/lib",
            "/usr/lib/libSystem.B.dylib": "/usr/lib/libSystem.B.dylib",
            "@loader_path/libcairo.2.dylib": "@loader_path/libcairo.2.dylib",
            # Only a whole path component matches, never a longer name that shares the prefix's text.
            "/opt/homebrewer/lib/libx.dylib": "/opt/homebrewer/lib/libx.dylib",
        }
        for old, new in cases.items():
            with self.subTest(old):
                self.assertEqual(SETUP.relocate_path(old, prefix), new)

    def test_pkg_config_text_moves_under_the_prefix(self):
        text = b"prefix=@@HOMEBREW_CELLAR@@/cairo/1.18.4\nlibdir=/opt/homebrew/opt/pixman/lib\nRequires: pixman-1\n"
        self.assertEqual(SETUP.relocate_text(text, "/p"),
                         b"prefix=/p/Cellar/cairo/1.18.4\nlibdir=/p/opt/pixman/lib\nRequires: pixman-1\n")

    def test_otool_listings_are_parsed(self):
        self.assertEqual(SETUP.parse_install_id("/x/libcairo.2.dylib:\n@@HOMEBREW_PREFIX@@/opt/cairo/lib/libcairo.2.dylib\n"),
                         "@@HOMEBREW_PREFIX@@/opt/cairo/lib/libcairo.2.dylib")
        self.assertIsNone(SETUP.parse_install_id("/x/tool:\n"))
        listing = otool_loads("/x/libcairo.2.dylib", ["/usr/lib/libSystem.B.dylib", "@rpath/libz.1.dylib"])
        self.assertEqual(SETUP.parse_loads(listing), ["/usr/lib/libSystem.B.dylib", "@rpath/libz.1.dylib"])
        rpath = "Load command 20\n          cmd LC_RPATH\n      cmdsize 48\n         path @@HOMEBREW_PREFIX@@/lib (offset 12)\n"
        self.assertEqual(SETUP.parse_rpaths(rpath), ["@@HOMEBREW_PREFIX@@/lib"])

    def test_a_dylib_is_rewritten_and_signed_again(self):
        # Rewriting load commands invalidates the signature, so every rewritten dylib is signed again.
        calls = []

        def run(command):
            calls.append(command)
            path = command[-1]
            if command[:2] == ["otool", "-D"]:
                return f"{path}:\n@@HOMEBREW_PREFIX@@/opt/cairo/lib/libcairo.2.dylib\n"
            if command[:2] == ["otool", "-L"]:
                return otool_loads(path, ["@@HOMEBREW_PREFIX@@/opt/cairo/lib/libcairo.2.dylib",
                                          "@@HOMEBREW_PREFIX@@/opt/pixman/lib/libpixman-1.0.dylib",
                                          "/usr/lib/libSystem.B.dylib"])
            if command[:2] == ["otool", "-l"]:
                return BUILD_VERSION.format("14.0")
            return ""
        with tempfile.TemporaryDirectory() as directory:
            dylib = Path(directory) / "libcairo.2.dylib"
            dylib.write_bytes(MACHO)
            self.assertTrue(SETUP.relocate_macho(dylib, "/p", run))
        rewrite = [command for command in calls if command[0] == "install_name_tool"]
        self.assertEqual(rewrite, [["install_name_tool", "-id", "/p/opt/cairo/lib/libcairo.2.dylib",
                                    "-change", "@@HOMEBREW_PREFIX@@/opt/pixman/lib/libpixman-1.0.dylib",
                                    "/p/opt/pixman/lib/libpixman-1.0.dylib", str(dylib)]])
        self.assertEqual(calls[-1], ["codesign", "--force", "--sign", "-", str(dylib)])

    def test_a_dylib_with_nothing_to_move_is_left_alone(self):
        calls = []

        def run(command):
            calls.append(command)
            if command[:2] == ["otool", "-L"]:
                return otool_loads(command[-1], ["/usr/lib/libSystem.B.dylib"])
            return f"{command[-1]}:\n" if command[:2] == ["otool", "-D"] else ""
        with tempfile.TemporaryDirectory() as directory:
            dylib = Path(directory) / "libz.dylib"
            dylib.write_bytes(MACHO)
            self.assertFalse(SETUP.relocate_macho(dylib, "/p", run))
        self.assertFalse([command for command in calls if command[0] in ("install_name_tool", "codesign")])


@unittest.skipUnless(os.name == "posix", "the private prefix links opt/<formula> to its keg with a symlink")
class VerifyTests(unittest.TestCase):
    """Verification reads each dylib's declared minimum macOS and refuses one past the ceiling, and
    refuses a dylib that still loads, or names itself by, a path outside the pinned prefix."""

    MANIFEST = {"max_minimum_macos": "14.0", "formulae": [CAIRO]}

    def test_the_minimum_is_read_from_either_load_command(self):
        self.assertEqual(SETUP.parse_minimum_macos(BUILD_VERSION.format("14.0")), "14.0")
        self.assertEqual(SETUP.parse_minimum_macos(VERSION_MIN.format("13.3")), "13.3")
        self.assertIsNone(SETUP.parse_minimum_macos("Load command 1\n      cmd LC_SEGMENT_64\n"))

    def prefix(self, directory, dylib="lib/libcairo.2.dylib"):
        """A private prefix holding one pinned cairo keg with `dylib`, and its opt link."""
        prefix = Path(directory).resolve()
        keg = prefix / "Cellar" / "cairo" / "1.18.4"
        (keg / dylib).parent.mkdir(parents=True)
        (keg / dylib).write_bytes(MACHO)
        (prefix / "opt").mkdir()
        (prefix / "opt" / "cairo").symlink_to(keg)
        return prefix

    def run_for(self, prefix, minimum="14.0", loads=()):
        install_id = f"{prefix}/opt/cairo/lib/libcairo.2.dylib"

        def run(command):
            if command[:2] == ["otool", "-l"]:
                return BUILD_VERSION.format(minimum)
            if command[:2] == ["otool", "-D"]:
                return f"{command[-1]}:\n{install_id}\n"
            return otool_loads(command[-1], [install_id, "/usr/lib/libSystem.B.dylib", *loads])
        return run

    def test_a_relocated_keg_within_the_ceiling_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = self.prefix(directory)
            self.assertEqual(SETUP.verify(self.MANIFEST, prefix, self.run_for(prefix, loads=["@rpath/libz.1.dylib"])), 1)

    def test_a_dylib_past_the_ceiling_is_refused(self):
        # A macOS 15.0 dylib is exactly what this setup exists to keep out of the package.
        with tempfile.TemporaryDirectory() as directory:
            prefix = self.prefix(directory)
            with self.assertRaises(SETUP.SetupError):
                SETUP.verify(self.MANIFEST, prefix, self.run_for(prefix, minimum="15.0"))

    def test_a_nested_dylib_past_the_ceiling_is_refused(self):
        # Cairo keeps libcairo-trace.dylib under lib/cairo/; a scan of lib/ alone would miss it.
        with tempfile.TemporaryDirectory() as directory:
            prefix = self.prefix(directory, dylib="lib/cairo/libcairo-trace.dylib")
            with self.assertRaises(SETUP.SetupError):
                SETUP.verify(self.MANIFEST, prefix, self.run_for(prefix, minimum="15.0"))

    def test_a_load_outside_the_pinned_prefix_is_refused(self):
        # Homebrew's own copy, an unrelocated placeholder, or a prefix path with no file would each let
        # the build or the package pick up a library this setup did not pin.
        for load in ("/opt/homebrew/opt/pixman/lib/libpixman-1.0.dylib",
                     "@@HOMEBREW_PREFIX@@/opt/pixman/lib/libpixman-1.0.dylib", "{prefix}/opt/missing/lib/libx.dylib"):
            with self.subTest(load), tempfile.TemporaryDirectory() as directory:
                prefix = self.prefix(directory)
                with self.assertRaises(SETUP.SetupError):
                    SETUP.verify(self.MANIFEST, prefix, self.run_for(prefix, loads=[load.format(prefix=prefix)]))

    def test_a_missing_opt_link_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = self.prefix(directory)
            (prefix / "opt" / "cairo").unlink()
            with self.assertRaises(SETUP.SetupError):
                SETUP.verify(self.MANIFEST, prefix, self.run_for(prefix))


@unittest.skipUnless(os.name == "posix", "the private prefix links opt/<formula> to its keg with a symlink")
class SetupTests(unittest.TestCase):
    """The whole setup builds a new private prefix and never runs Homebrew."""

    def manifest(self, directory, payload):
        good = json.loads((HERE / "macos-native-bottles.json").read_text(encoding="utf-8"))
        document = {**good, "formulae": [{**record_for(payload, "cairo", "1.18.4", "1.18.4")}]}
        path = Path(directory) / "manifest.json"
        path.write_text(json.dumps(document), encoding="utf-8")
        return path

    def test_the_prefix_holds_relocated_signed_pins_with_a_receipt(self):
        payload = bottle_bytes([("cairo/1.18.4/lib/libcairo.2.dylib", MACHO),
                                ("cairo/1.18.4/lib/libcairo.dylib", "->libcairo.2.dylib"),
                                ("cairo/1.18.4/lib/pkgconfig/cairo.pc", b"prefix=@@HOMEBREW_CELLAR@@/cairo/1.18.4\n"),
                                ("cairo/1.18.4/COPYING", b"license")])
        calls, relocated = [], set()

        def run(command):
            calls.append(command)
            path = command[-1]
            if command[0] == "install_name_tool":
                relocated.add(path)
            install_id = (f"{prefix_holder[0]}/opt/cairo/lib/libcairo.2.dylib" if path in relocated
                          else "@@HOMEBREW_PREFIX@@/opt/cairo/lib/libcairo.2.dylib")
            if command[:2] == ["otool", "-D"]:
                return f"{path}:\n{install_id}\n"
            if command[:2] == ["otool", "-L"]:
                return otool_loads(path, [install_id, "/usr/lib/libSystem.B.dylib"])
            if command[:2] == ["otool", "-l"]:
                return BUILD_VERSION.format("14.0")
            return ""
        with tempfile.TemporaryDirectory() as directory:
            prefix_holder = [Path(directory).resolve() / SETUP.PREFIX_NAME]
            manifest = self.manifest(directory, payload)
            prefix, checked = SETUP.setup(manifest, directory, opener_serving(payload), run)
            self.assertEqual((prefix, checked), (prefix_holder[0], 1))
            keg = prefix / "Cellar" / "cairo" / "1.18.4"
            self.assertEqual((prefix / "opt" / "cairo").resolve(), keg.resolve())
            self.assertEqual((keg / "lib" / "pkgconfig" / "cairo.pc").read_text(), f"prefix={prefix}/Cellar/cairo/1.18.4\n")
            receipt = json.loads((keg / "INSTALL_RECEIPT.json").read_text(encoding="utf-8"))
            self.assertEqual(receipt["bottle"]["sha256"], hashlib.sha256(payload).hexdigest())
            self.assertIn(["codesign", "--force", "--sign", "-", str(keg / "lib" / "libcairo.2.dylib")], calls)
            self.assertFalse([command for command in calls if command[0] == "brew"])
            self.assertEqual(SETUP.pkg_config_path(SETUP.load_manifest(manifest), prefix),
                             str(prefix / "opt" / "cairo" / "lib" / "pkgconfig"))
            # The prefix is built once; a second run never reuses or overwrites it.
            with self.assertRaises(SETUP.SetupError):
                SETUP.setup(manifest, directory, opener_serving(payload), run)


class MainTests(unittest.TestCase):
    """Later workflow steps build against the prefix through PKG_CONFIG_PATH."""

    def test_main_puts_the_prefix_ahead_of_the_existing_search_path(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "sonicterm-native"
            (prefix / "opt" / "cairo" / "lib" / "pkgconfig").mkdir(parents=True)
            environment_file = Path(directory) / "github-env"
            environment = {"GITHUB_ENV": str(environment_file), "PKG_CONFIG_PATH": "/existing"}
            with mock.patch.object(SETUP.sys, "platform", "darwin"), \
                    mock.patch.object(SETUP.platform, "machine", return_value="arm64"), \
                    mock.patch.object(SETUP, "setup", return_value=(prefix, 3)), \
                    mock.patch.dict(os.environ, environment), \
                    mock.patch("sys.stdout", new_callable=io.StringIO):
                self.assertEqual(SETUP.main(["--work-dir", directory]), 0)
            expected = os.pathsep.join([str(prefix / "opt" / "cairo" / "lib" / "pkgconfig"), "/existing"])
            self.assertEqual(environment_file.read_text(encoding="utf-8"), f"PKG_CONFIG_PATH={expected}\n")


# A command that starts a helper in its own process group, as Homebrew does, holding the inherited
# stdout; with `hang` the command then waits on the helper, otherwise it exits and leaves it behind.
ESCAPING_COMMAND = """
import os, subprocess, sys
helper = subprocess.Popen(["sleep", "60"], preexec_fn=os.setpgrp)
open(sys.argv[1], "w").write(str(helper.pid))
print("done", flush=True)
if sys.argv[2] == "hang":
    helper.wait()
"""


def wait_until_gone(pid, timeout_s=5.0):
    """Whether `pid` stops running within `timeout_s`, read through the process table cleanup uses.

    A killed orphan stays a zombie where nothing reaps it, as under a container's non-init PID 1, and
    a zombie still answers `kill(pid, 0)`; the table reads a zombie as gone, as cleanup does.
    """
    import perf_processes
    table = perf_processes.make_process_table()
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if table.read(pid) is None:
            return True
        time.sleep(0.05)
    return False


@unittest.skipUnless(hasattr(os, "killpg"), "sessions and process groups are POSIX-only")
class RunTests(unittest.TestCase):
    """Every process a command starts is reaped, including helpers that leave its process group."""

    def run_escaping(self, mode, timeout_s):
        """Run ESCAPING_COMMAND, returning `_run`'s result or error, the helper pid and the elapsed time."""
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "helper.pid"
            started = time.monotonic()
            try:
                outcome = SETUP._run([sys.executable, "-c", ESCAPING_COMMAND, str(marker), mode], timeout_s=timeout_s)
            except SETUP.SetupError as error:
                outcome = error
            return outcome, int(marker.read_text().strip()), time.monotonic() - started

    def test_a_hung_command_and_its_escaped_helper_are_killed(self):
        # The helper holds the command's stdout from another group; _run must still return at its deadline.
        outcome, helper, elapsed_s = self.run_escaping("hang", timeout_s=1)
        self.assertIsInstance(outcome, SETUP.SetupError)
        self.assertIn("timed out after 1 seconds", str(outcome))
        self.assertLess(elapsed_s, 30)
        self.assertTrue(wait_until_gone(helper))

    def test_a_helper_left_by_a_finished_command_is_killed(self):
        # A command that succeeds returns its output, and the helper it left behind does not outlive it.
        outcome, helper, elapsed_s = self.run_escaping("exit", timeout_s=30)
        self.assertEqual(outcome, "done\n")
        self.assertLess(elapsed_s, 30)
        self.assertTrue(wait_until_gone(helper))

    def test_a_failing_command_reports_its_exit_status(self):
        with self.assertRaises(SETUP.SetupError) as raised:
            SETUP._run(["sh", "-c", "echo broken >&2; exit 3"])
        self.assertIn("failed (3): broken", str(raised.exception))


class IncompleteTable:
    """A process table whose census never completes: the list is unreadable, or one pid is."""

    def __init__(self, unreadable_pid=None):
        self.unreadable_pid = unreadable_pid

    def pids(self):
        return None if self.unreadable_pid is None else [self.unreadable_pid]

    def session_of(self, pid):
        import perf_sessions
        raise perf_sessions.ProcessUnreadable(f"pid {pid} cannot be read")


class ReapTests(unittest.TestCase):
    """Cleanup succeeds only on a complete census; one that cannot be completed fails the setup."""

    def test_an_incomplete_census_is_never_taken_as_an_empty_session(self):
        # An unreadable process list and an unreadable pid both leave the session unproven, so both fail.
        for name, table in (("unreadable list", IncompleteTable()), ("unreadable pid", IncompleteTable(4242))):
            with self.subTest(name), mock.patch.object(SETUP, "REAP_TIMEOUT_S", 0.2):
                with self.assertRaises(SETUP.SetupError) as raised:
                    SETUP._reap_session(4242, table)
                self.assertIn("incomplete process census", str(raised.exception))


if __name__ == "__main__":
    unittest.main(verbosity=2)
