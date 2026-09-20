#!/usr/bin/env python3
"""Portable dependency-closure tests; no macOS tools or Homebrew are executed."""

from __future__ import annotations

import copy
import errno
import importlib.util
import json
import os
import plistlib
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

_SPEC = importlib.util.spec_from_file_location(
    "macos_bundle", Path(__file__).with_name("macos-bundle.py")
)
assert _SPEC is not None and _SPEC.loader is not None
bundle = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bundle)


def symlink(target, path, directory=False):
    try:
        path.symlink_to(target, target_is_directory=directory)
    except OSError as error:
        if error.errno in (errno.EPERM, errno.EACCES, errno.ENOSYS, errno.ENOTSUP) or getattr(error, "winerror", None) == 1314:
            raise unittest.SkipTest("host cannot create fixture symlinks") from error
        raise


class FakeNative:
    """Model native inspection and mutations using otool's actual text format."""

    def __init__(self):
        self.nodes = {}
        self.originals = {}
        self.calls = []

    def add(self, path, loads=(), rpaths=(), install_id=None, arch="arm64"):
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = b"\xcf\xfa\xed\xfe" + str(path).encode()
        path.write_bytes(payload)
        self.nodes[path] = dict(loads=list(map(str, loads)), rpaths=list(map(str, rpaths)),
                                install_id=install_id, arch=arch, minos="13.0", legacy=False)
        self.originals[payload] = copy.deepcopy(self.nodes[path])
        return path

    def __call__(self, *args):
        self.calls.append(tuple(map(str, args)))
        tool, *options, filename = map(str, args)
        path = Path(filename)
        if path not in self.nodes:
            self.nodes[path] = copy.deepcopy(self.originals[path.read_bytes()])
        node = self.nodes[path]
        if tool == "lipo":
            return node["arch"] + "\n"
        if tool == "otool":
            if options == ["-D"]:
                return f"{path}:\n" + (f"{node['install_id']}\n" if node["install_id"] else "")
            if options == ["-L"]:
                names = ([node["install_id"]] if node["install_id"] else []) + node["loads"]
                return f"{path}:\n" + "".join(
                    f"\t{name} (compatibility version 1.0.0, current version 2.3.0)\n"
                    for name in names
                )
            if options == ["-l"]:
                deployment = (f"Load command 0\n cmd LC_VERSION_MIN_MACOSX\n cmdsize 16\n version {node['minos']}\n sdk 15.0\n"
                              if node["legacy"] else
                              f"Load command 0\n cmd LC_BUILD_VERSION\n cmdsize 32\n platform 1\n minos {node['minos']}\n sdk 15.0\n ntools 1\n")
                return f"{path}:\n" + deployment + "".join(
                    f"Load command {i}\n          cmd LC_RPATH\n      cmdsize 48\n"
                    f"         path {rpath} (offset 12)\n"
                    for i, rpath in enumerate(node["rpaths"])
                )
        if tool == "install_name_tool":
            if options[0] == "-change":
                node["loads"] = [options[2] if load == options[1] else load
                                 for load in node["loads"]]
            elif options[0] == "-id":
                node["install_id"] = options[1]
            elif options[0] == "-delete_rpath":
                node["rpaths"].remove(options[1])
            else:
                raise AssertionError(options)
            path.write_bytes(path.read_bytes() + b" rewritten")
            return ""
        if tool == "codesign":
            path.write_bytes(path.read_bytes() + b" signed")
            return ""
        raise AssertionError(args)


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.app = self.root / "Sonic Term.app"
        self.exe = self.app / "Contents/MacOS/sonicterm-mac"
        self.native = FakeNative()
        self.native.add(self.exe, ["/usr/lib/libSystem.B.dylib"])
        self.exe.chmod(0o755)
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps({"LSMinimumSystemVersion": "14.0"}))
        self.addCleanup(patch.stopall)
        patch.object(bundle, "run", side_effect=self.native).start()

    def library(self, formula="cairo", name="libcairo.2.dylib", loads=(), rpaths=(), license=True):
        keg = self.root / "homebrew/Cellar" / formula / "1.2.3_1"
        path = self.native.add(keg / "lib" / name, loads, rpaths, f"@rpath/{name}")
        (keg / "INSTALL_RECEIPT.json").write_text(json.dumps({
            "source": {"versions": {"stable": "1.2.3"}, "tap": "homebrew/core"}
        }))
        (keg / "sbom.spdx.json").write_text(json.dumps({"packages": [{
            "name": formula, "SPDXID": f"SPDXRef-Archive-{formula}-src",
            "downloadLocation": f"https://upstream.example/{formula}.tar.xz"
        }]}))
        if license:
            (keg / "COPYING").write_text("Fixture license; not a legal assessment.\n")
        return path

    def link(self, *paths, rpaths=()):
        self.native.nodes[self.exe]["loads"] += list(map(str, paths))
        self.native.nodes[self.exe]["rpaths"] = list(map(str, rpaths))

    def manifest(self):
        return json.loads((self.app / "Contents/Resources/native-libraries.json").read_text())

    def test_cycle_aliases_self_ids_and_post_sign_hashes(self):
        # A self install-id is metadata, while canonical aliases and cycles copy only once.
        cairo = self.library()
        pixman = self.library("pixman", "libpixman.1.dylib", [cairo])
        self.native.nodes[cairo]["loads"] = [str(pixman)]
        self.native.originals[cairo.read_bytes()] = copy.deepcopy(self.native.nodes[cairo])
        alias = cairo.with_name("libcairo.dylib")
        symlink(cairo.name, alias)
        self.link(cairo, alias)
        source = {p: p.read_bytes() for p in (cairo, pixman)}
        bundle.bundle_app(self.app, "arm64")
        bundle.verify_app(self.app, "arm64")
        manifest = self.manifest()
        self.assertEqual(len(manifest["libraries"]), 2)
        self.assertEqual(manifest["architecture"], "arm64")
        for library in manifest["libraries"]:
            destination = self.app / library["path"]
            self.assertEqual(library["packaged_sha256"], bundle.sha256(destination))
            self.assertNotEqual(library["source_sha256"], library["packaged_sha256"])
            self.assertEqual(library["install_name"]["current_version"], "2.3.0")
            self.assertTrue(library["licenses"])
            self.assertTrue(library["upstream_sources"])
            self.assertEqual(len(library["metadata_files"]), 2)
        for path, content in source.items():
            self.assertEqual(path.read_bytes(), content)
        edits = [call for call in self.native.calls if call[0] in ("install_name_tool", "codesign")]
        self.assertTrue(all(Path(call[-1]).is_relative_to(self.app) for call in edits))

    def test_rpath_stack_and_loader_relative_dependencies_are_rewritten(self):
        # The loader's rpaths precede inherited executable rpaths, including transitive imports.
        pixman = self.library("pixman", "libpixman.1.dylib")
        cairo = self.library(loads=["@rpath/libpixman.1.dylib"], rpaths=[str(pixman.parent)])
        # A host-native absolute path keeps this portable fixture valid on Windows drive roots too.
        unused = self.root / "unused/host/path"
        self.assertTrue(unused.is_absolute())
        self.link("@rpath/libcairo.2.dylib", rpaths=[str(cairo.parent), str(unused)])
        bundle.bundle_app(self.app)
        bundle.verify_app(self.app)
        self.assertEqual(self.native.nodes[self.exe]["rpaths"], [])
        self.assertIn("@executable_path/../Frameworks/libcairo.2.dylib",
                      self.native.nodes[self.exe]["loads"])
        library = self.app / "Contents/Frameworks/libcairo.2.dylib"
        self.assertEqual(self.native.nodes[library]["loads"], ["@loader_path/libpixman.1.dylib"])
        self.assertEqual(self.native.nodes[library]["rpaths"], [])

    def test_inherited_executable_rpath_and_loader_path(self):
        # Transitive @rpath lookup keeps the executable stack; loader-relative aliases resolve canonically.
        cairo = self.library()
        other = self.native.add(cairo.with_name("libother.dylib"), [], [], "@rpath/libother.dylib")
        self.native.nodes[cairo]["loads"] = ["@rpath/libother.dylib", "@loader_path/libother.dylib"]
        self.native.originals[cairo.read_bytes()] = copy.deepcopy(self.native.nodes[cairo])
        self.link(cairo, rpaths=[str(other.parent)])
        bundle.bundle_app(self.app)
        bundle.verify_app(self.app)
        self.assertEqual(len(self.manifest()["libraries"]), 2)

    def test_basename_collision_fails_before_modification(self):
        # Flattening different canonical files to one filename must never overwrite either library.
        a = self.library("first", "libsame.dylib")
        b = self.library("second", "libsame.dylib")
        self.link(a, b)
        original = self.exe.read_bytes()
        with self.assertRaisesRegex(bundle.BundleError, "collision"):
            bundle.bundle_app(self.app)
        self.assertEqual(self.exe.read_bytes(), original)

    def test_architecture_mismatch_and_universal_executable_fail(self):
        # Shipping is single-architecture, and every dylib must contain that architecture.
        lib = self.library()
        self.link(lib)
        self.native.nodes[lib]["arch"] = "x86_64"
        with self.assertRaisesRegex(bundle.BundleError, "architecture"):
            bundle.bundle_app(self.app)
        self.native.nodes[self.exe]["arch"] = "arm64 x86_64"
        with self.assertRaisesRegex(bundle.BundleError, "architecture"):
            bundle.bundle_app(self.app)
        self.native.nodes[self.exe]["arch"] = "arm64"
        with self.assertRaisesRegex(bundle.BundleError, "architecture"):
            bundle.bundle_app(self.app, "x86_64")

    def test_missing_or_unknown_load_fails_closed(self):
        # Unsupported tokens, relative imports and unresolved rpaths cannot consult ambient host search paths.
        for load in ("@rpath/missing.dylib", "@unknown/lib.dylib", "librelative.dylib",
                     str(self.root / "absent.dylib"), "/usr/library/absent.dylib"):
            with self.subTest(load=load):
                self.native.nodes[self.exe]["loads"] = [load]
                with self.assertRaises(bundle.BundleError):
                    bundle.bundle_app(self.app)

    def test_system_prefixes_require_boundaries_and_no_traversal(self):
        # Similar-looking prefixes and dot-dot paths are not system dependencies.
        for name in ("/usr/lib/libSystem.B.dylib", "/System/Library/Frameworks/CoreText.framework/CoreText"):
            self.assertTrue(bundle.is_system(name))
        for name in ("/usr/library/libbad.dylib", "/System/LibraryEvil/x", "/usr/lib/../../tmp/x"):
            self.assertFalse(bundle.is_system(name))

    def test_missing_license_or_non_homebrew_library_is_actionable(self):
        # Packaging must refuse missing attribution rather than infer legal permission from a filename.
        lib = self.library(license=False)
        self.link(lib)
        with self.assertRaisesRegex(bundle.BundleError, "COPYING|LICENSE"):
            bundle.bundle_app(self.app)
        other = self.native.add(self.root / "libunknown.dylib", install_id="@rpath/libunknown.dylib")
        self.native.nodes[self.exe]["loads"] = [str(other)]
        with self.assertRaisesRegex(bundle.BundleError, "Homebrew"):
            bundle.bundle_app(self.app)

    def test_share_doc_licenses_and_notices_are_preserved(self):
        # Preserve nested attribution and metadata instead of flattening colliding document names.
        lib = self.library(license=False)
        doc = lib.parent.parent / "share/doc/cairo"
        doc.mkdir(parents=True)
        (doc / "LICENSE.txt").write_text("license")
        (doc / "NOTICE").write_text("notice")
        self.link(lib)
        bundle.bundle_app(self.app)
        bundle.verify_app(self.app)
        self.assertEqual(len(self.manifest()["libraries"][0]["licenses"]), 2)

    def test_license_symlink_escape_and_destination_symlink_are_rejected(self):
        # Neither source attribution nor output paths may escape their validated roots.
        lib = self.library(license=False)
        outside = self.root / "outside"
        outside.write_text("not keg attribution")
        symlink(outside, lib.parent.parent / "LICENSE")
        self.link(lib)
        with self.assertRaisesRegex(bundle.BundleError, "escape|symlink"):
            bundle.bundle_app(self.app)
        symlink(self.root, self.app / "Contents/Frameworks", directory=True)
        with self.assertRaisesRegex(bundle.BundleError, "symlink"):
            bundle.bundle_app(self.app)

    def test_manifest_paths_cannot_escape_app(self):
        # Even a manifest edit cannot turn verification into an external-file hash check.
        self.link(self.library())
        bundle.bundle_app(self.app)
        manifest = self.manifest()
        manifest["libraries"][0]["licenses"][0]["path"] = "../outside"
        (self.app / "Contents/Resources/native-libraries.json").write_text(json.dumps(manifest))
        with self.assertRaisesRegex(bundle.BundleError, "path|escape"):
            bundle.verify_app(self.app)

    def test_tampered_libraries_licenses_and_metadata_fail_verification(self):
        # Every redistributed byte covered by provenance, including receipts, is integrity-checked.
        self.link(self.library())
        bundle.bundle_app(self.app)
        lib = self.manifest()["libraries"][0]
        for item in [lib] + lib["licenses"] + lib["metadata_files"]:
            path = self.app / item["path"]
            original = path.read_bytes()
            path.write_bytes(original + b"tampered")
            with self.assertRaisesRegex(bundle.BundleError, "hash"):
                bundle.verify_app(self.app)
            path.write_bytes(original)

    def test_verify_rejects_host_loads_rpaths_and_missing_libraries(self):
        # Inspection is independent of the manifest and rejects residual host search/load commands.
        self.link(self.library())
        bundle.bundle_app(self.app)
        self.native.nodes[self.exe]["loads"].append("/opt/homebrew/lib/libbad.dylib")
        with self.assertRaises(bundle.BundleError):
            bundle.verify_app(self.app)
        self.native.nodes[self.exe]["loads"].pop()
        self.native.nodes[self.exe]["rpaths"] = ["/host/path"]
        with self.assertRaisesRegex(bundle.BundleError, "RPATH"):
            bundle.verify_app(self.app)
        self.native.nodes[self.exe]["rpaths"] = []
        (self.app / self.manifest()["libraries"][0]["path"]).unlink()
        with self.assertRaises(bundle.BundleError):
            bundle.verify_app(self.app)

    def test_verify_rejects_extra_macho_and_fake_frameworks_entries(self):
        # Unreachable executable code and unregistered fake dylibs cannot evade closure verification.
        bundle.bundle_app(self.app)
        extra = self.app / "Contents/Resources/hidden-code"
        extra.write_bytes(b"\xcf\xfa\xed\xfeextra")
        with self.assertRaisesRegex(bundle.BundleError, "Mach-O|unregistered"):
            bundle.verify_app(self.app)
        extra.unlink()
        extra = self.app / "Contents/Frameworks/fake.dylib"
        extra.write_text("not Mach-O")
        with self.assertRaises(bundle.BundleError):
            bundle.verify_app(self.app)

    def test_deployment_floor_preserves_requested_minimum(self):
        # Older linked code never lowers the product's declared supported OS floor.
        self.link(self.library())
        bundle.bundle_app(self.app)
        self.assertEqual(self.manifest()["required_macos"], "14.0")
        self.assertEqual(self.manifest()["libraries"][0]["minimum_macos"], "13.0")

    def test_deployment_floor_raises_for_legacy_or_modern_dependencies(self):
        # Both Mach-O deployment encodings contribute to the honest package OS requirement.
        lib = self.library()
        self.native.nodes[lib].update(minos="15.2", legacy=True)
        self.native.originals[lib.read_bytes()] = copy.deepcopy(self.native.nodes[lib])
        self.link(lib)
        with self.assertRaisesRegex(bundle.BundleError, "ceiling"):
            bundle.bundle_app(self.app)
        bundle.bundle_app(self.app, max_minimum="15.2")
        self.assertEqual(self.manifest()["required_macos"], "15.2")
        plist = plistlib.loads((self.app / "Contents/Info.plist").read_bytes())
        self.assertEqual(plist["LSMinimumSystemVersion"], "15.2")
        with self.assertRaisesRegex(bundle.BundleError, "ceiling"):
            bundle.verify_app(self.app)
        bundle.verify_app(self.app, max_minimum="15.2")
        self.native.nodes[self.exe]["minos"] = "26.0"
        with self.assertRaisesRegex(bundle.BundleError, "minimum|deployment"):
            bundle.verify_app(self.app, max_minimum="15.2")

    def test_verifier_rejects_changed_declared_minimum(self):
        # Manifest and Info.plist must agree, even if somebody lowers the floor after bundling.
        bundle.bundle_app(self.app)
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps({"LSMinimumSystemVersion": "11.0"}))
        with self.assertRaisesRegex(bundle.BundleError, "minimum|deployment"):
            bundle.verify_app(self.app)

    def test_executable_symlink_and_hardlink_are_rejected(self):
        # install_name_tool must never modify an external build binary through an alias.
        self.exe.unlink()
        source = self.root / "build-binary"
        source.write_text("source")
        symlink(source, self.exe)
        with self.assertRaisesRegex(bundle.BundleError, "symlink"):
            bundle.bundle_app(self.app)
        self.exe.unlink()
        os.link(source, self.exe)
        with self.assertRaisesRegex(bundle.BundleError, "hardlink"):
            bundle.bundle_app(self.app)


class CommandTests(unittest.TestCase):
    def test_native_command_timeout_is_reported(self):
        # Native tool hangs must fail promptly with the failing tool named in the diagnostic.
        with patch.object(bundle.subprocess, "run", side_effect=subprocess.TimeoutExpired("otool", 60)) as run:
            with self.assertRaisesRegex(bundle.BundleError, "otool.*60|60.*otool"):
                bundle.run("otool", "-L", "/tmp/fixture")
        self.assertEqual(run.call_args.kwargs["timeout"], 60)
        # A file-size rlimit would also cap install_name_tool's executable output, not merely its diagnostics.
        self.assertNotIn("preexec_fn", run.call_args.kwargs)


if __name__ == "__main__":
    unittest.main()
